#!/usr/bin/env bash
set -euo pipefail

RELAY_DIR="${RELAY_DIR:-/home/pepe/obelisk-relay}"
SERVICE="${SERVICE:-groups_relay}"
RELAY_HOSTNAME="${RELAY_HOSTNAME:-relay.fabriok.ar}"
YES=0
RESTART=1
OWNER_PUBKEY="${RELAY_OWNER_PUBKEY:-}"

log() {
  printf '\n==> %s\n' "$*"
}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'USAGE'
Usage:
  scripts/reset-relay-config-without-data.sh --yes-reset-config [options]

Options:
  --owner-pubkey HEX   Relay owner pubkey to keep as admin/whitelisted/reference.
                       Defaults to the first existing admin, whitelist, or reference pubkey.
  --no-restart         Rewrite config files but do not restart Docker.
  -h, --help           Show this help.

This resets relay configuration files to an owner-only baseline and keeps event
data intact. It does not remove Docker volumes, /app/db, or relay event data.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --yes-reset-config)
      YES=1
      ;;
    --owner-pubkey)
      shift
      [ "$#" -gt 0 ] || die "Missing value for --owner-pubkey"
      OWNER_PUBKEY="$1"
      ;;
    --no-restart)
      RESTART=0
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "Unknown argument: $1"
      ;;
  esac
  shift
done

[ "$YES" -eq 1 ] || {
  usage
  die "Refusing to reset config without --yes-reset-config"
}

env_value() {
  local key="$1"
  local default_value="$2"
  local env_file="${RELAY_DIR}/.env"

  if [ -f "$env_file" ]; then
    sed -n "s/^${key}=//p" "$env_file" | tail -n 1
    return
  fi

  printf '%s\n' "$default_value"
}

pick_docker() {
  if docker info >/dev/null 2>&1; then
    COMPOSE=(docker compose)
    return
  fi

  if grep -q '^NoNewPrivs:[[:space:]]*1$' /proc/$$/status 2>/dev/null; then
    die "Docker is not reachable here and this shell has no_new_privileges enabled. Run from a real host SSH/TTY shell."
  fi

  if command -v sudo >/dev/null 2>&1 && sudo -n docker info >/dev/null 2>&1; then
    COMPOSE=(sudo docker compose)
    return
  fi

  die "Docker is not reachable. Run from the host shell with docker access."
}

yaml_scalar() {
  local key="$1"
  local default_value="$2"
  local file="$3"
  local value

  value="$(sed -n "s/^[[:space:]]*${key}:[[:space:]]*\"\{0,1\}\([^\"]*\)\"\{0,1\}[[:space:]]*$/\1/p" "$file" | head -n 1)"
  if [ -n "$value" ]; then
    printf '%s\n' "$value"
  else
    printf '%s\n' "$default_value"
  fi
}

first_yaml_pubkey() {
  local section="$1"
  local file="$2"

  sed -n "/^[[:space:]]*${section}:[[:space:]]*$/,/^[[:space:]]*[A-Za-z_][A-Za-z0-9_]*:[[:space:]]*/p" "$file" \
    | sed -n 's/^[[:space:]]*-[[:space:]]*"\{0,1\}\([0-9a-fA-F]\{64\}\)"\{0,1\}[[:space:]]*$/\1/p' \
    | head -n 1
}

first_json_pubkey() {
  local file="$1"
  grep -Eo '[0-9a-fA-F]{64}' "$file" 2>/dev/null | head -n 1 || true
}

validate_pubkey() {
  local value="$1"
  case "$value" in
    *[!0-9a-fA-F]*|'')
      return 1
      ;;
  esac
  [ "${#value}" -eq 64 ]
}

write_file() {
  local path="$1"
  local tmp
  tmp="$(mktemp "${path}.tmp.XXXXXX")"
  cat > "$tmp"
  mv "$tmp" "$path"
}

wait_for_health() {
  local url="$1"
  for _ in $(seq 1 60); do
    if curl -fsS --max-time 5 "$url" >/dev/null 2>&1; then
      printf 'health OK: %s\n' "$url"
      return
    fi
    sleep 2
  done
  die "Relay did not become healthy: $url"
}

main() {
  [ -d "$RELAY_DIR" ] || die "Relay directory not found: $RELAY_DIR"
  [ -f "$RELAY_DIR/compose.yml" ] || die "Missing compose.yml in $RELAY_DIR"

  local config_dir settings_file relay_secret_key relay_url db_path local_addr owner
  config_dir="${RELAY_DIR}/config"
  settings_file="${config_dir}/settings.local.yml"

  [ -d "$config_dir" ] || die "Config directory not found: $config_dir"
  [ -f "$settings_file" ] || die "Missing settings.local.yml: $settings_file"

  owner="$OWNER_PUBKEY"
  if [ -z "$owner" ]; then
    owner="$(first_yaml_pubkey admin_pubkeys "$settings_file")"
  fi
  if [ -z "$owner" ]; then
    owner="$(first_yaml_pubkey whitelisted_pubkeys "$settings_file")"
  fi
  if [ -z "$owner" ]; then
    owner="$(first_json_pubkey "${config_dir}/reference_accounts.json")"
  fi

  validate_pubkey "$owner" || die "Could not infer a valid owner pubkey. Pass --owner-pubkey HEX."
  owner="$(printf '%s' "$owner" | tr 'A-F' 'a-f')"

  relay_secret_key="$(yaml_scalar relay_secret_key "" "$settings_file")"
  validate_pubkey "$relay_secret_key" || die "Existing relay_secret_key is missing or invalid; refusing to generate a new relay identity."
  relay_secret_key="$(printf '%s' "$relay_secret_key" | tr 'A-F' 'a-f')"

  relay_url="$(yaml_scalar relay_url "wss://${RELAY_HOSTNAME}" "$settings_file")"
  db_path="$(yaml_scalar db_path "/app/db" "$settings_file")"
  local_addr="$(yaml_scalar local_addr "0.0.0.0:8080" "$settings_file")"

  local timestamp backup_dir
  timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
  backup_dir="${RELAY_DIR}/backups/config-reset-${timestamp}"
  mkdir -p "$backup_dir"
  chmod 700 "$backup_dir"

  log "Backing up current config to ${backup_dir}"
  for file in \
    settings.local.yml \
    whitelist_runtime.json \
    whitelist_follows.json \
    reference_accounts.json \
    admin_pubkeys_runtime.json
  do
    if [ -e "${config_dir}/${file}" ]; then
      cp -a "${config_dir}/${file}" "${backup_dir}/${file}"
    fi
  done

  log "Writing owner-only relay configuration"
  write_file "$settings_file" <<EOF
relay:
  relay_secret_key: "${relay_secret_key}"
  relay_url: "${relay_url}"
  db_path: "${db_path}"
  local_addr: "${local_addr}"

  whitelisted_pubkeys:
    - "${owner}"

  admin_pubkeys:
    - "${owner}"

  max_subscriptions: 50
  max_limit: 500

  pubkey_rate_limit_per_minute: 6000
  connection_rate_limit_per_minute: 12000
  global_rate_limit_per_minute: 600000

  obelisk_index:
    enabled: true
    recent_per_group: 50
    max_bootstrap_groups: 500
    max_page_limit: 100
    bootstrap_requests_per_minute: 30
    message_requests_per_minute: 120
    reconcile_interval: "5m"

  websocket:
    max_connection_duration: "24h"
    idle_timeout: "30m"
    max_connections: 300
EOF

  write_file "${config_dir}/reference_accounts.json" <<EOF
[
  "${owner}"
]
EOF
  write_file "${config_dir}/whitelist_runtime.json" <<'EOF'
[]
EOF
  write_file "${config_dir}/whitelist_follows.json" <<'EOF'
[]
EOF
  write_file "${config_dir}/admin_pubkeys_runtime.json" <<'EOF'
[]
EOF

  printf 'owner pubkey kept: %s\n' "$owner"
  printf 'relay_secret_key preserved: yes\n'
  printf 'event data untouched: yes\n'

  if [ "$RESTART" -eq 0 ]; then
    printf '\nConfig reset complete. Restart skipped because --no-restart was passed.\n'
    return
  fi

  cd "$RELAY_DIR"
  pick_docker

  log "Restarting ${SERVICE}"
  "${COMPOSE[@]}" up -d --no-deps "$SERVICE"

  local host_port
  host_port="${RELAY_HOST_PORT:-$(env_value RELAY_HOST_PORT 8082)}"
  wait_for_health "http://127.0.0.1:${host_port}/health"

  printf '\nConfig reset complete. Data volume was not removed.\n'
}

main "$@"
