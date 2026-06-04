#!/usr/bin/env bash
set -euo pipefail

RELAY_DIR="${RELAY_DIR:-/home/pepe/obelisk-relay}"
RELAY_HOSTNAME="${RELAY_HOSTNAME:-relay.fabriok.ar}"
SERVICE="${SERVICE:-groups_relay}"

log() {
  printf '\n==> %s\n' "$*"
}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
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

  die "Docker is not reachable. Log out/in after docker group changes, or run this script with sudo from the host shell."
}

wait_for_health() {
  local url="$1"
  local label="$2"

  for _ in $(seq 1 90); do
    if curl -fsS --max-time 5 "$url" >/dev/null 2>&1; then
      printf '%s OK\n' "$label"
      return
    fi
    sleep 2
  done

  die "${label} did not become healthy: ${url}"
}

verify_admin_html() {
  local url="$1"
  local label="$2"
  local html

  html="$(curl -fsS --max-time 15 "$url")" || die "Cannot fetch ${label} admin page: ${url}"

  printf '%s admin assets:\n' "$label"
  printf '%s\n' "$html" | grep -o 'assets/\(index\|preact\|nostr-wot\)-[^"]*\.js' | sort -u

  printf '%s\n' "$html" | grep -q 'assets/preact-' \
    || die "${label} admin page is still missing the isolated Preact runtime chunk"
  printf '%s\n' "$html" | grep -q 'assets/nostr-wot-' \
    || die "${label} admin page is still missing the isolated Nostr WoT chunk"
}

main() {
  [ -d "$RELAY_DIR" ] || die "Relay directory not found: $RELAY_DIR"
  [ -f "$RELAY_DIR/compose.yml" ] || die "Missing compose.yml in $RELAY_DIR"

  cd "$RELAY_DIR"
  pick_docker

  local host_port
  host_port="${RELAY_HOST_PORT:-$(env_value RELAY_HOST_PORT 8082)}"

  log "Building local Docker image for ${SERVICE}"
  export DOCKER_BUILDKIT=1
  "${COMPOSE[@]}" build "$SERVICE"

  log "Restarting ${SERVICE}"
  "${COMPOSE[@]}" up -d --no-deps "$SERVICE"

  log "Verifying local relay"
  wait_for_health "http://127.0.0.1:${host_port}/health" "local health"
  verify_admin_html "http://127.0.0.1:${host_port}/admin" "local"

  log "Verifying public relay through Cloudflare"
  wait_for_health "https://${RELAY_HOSTNAME}/health" "public health"
  verify_admin_html "https://${RELAY_HOSTNAME}/admin" "public"

  log "Container status"
  "${COMPOSE[@]}" ps "$SERVICE"

  printf '\nRedeploy complete for https://%s/admin\n' "$RELAY_HOSTNAME"
}

main "$@"
