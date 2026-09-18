#!/usr/bin/env bash
# Carry out an update the relay's admin console asked for.
#
# The relay container has no Docker socket, and deliberately so: a socket mount
# is root on the host, and the relay terminates untrusted WebSocket traffic from
# the open internet. So the relay only writes a request file, and this script --
# running on the host, as root, driven by a systemd path unit -- is what actually
# pulls the image and recreates the container.
#
# That makes the request file untrusted input written by a network-facing
# service. This script therefore:
#
#   * accepts a TAG, never an image reference; the repository is a constant here
#   * rejects any tag outside a strict character allowlist
#   * never passes the tag through a shell word split or an eval
#   * verifies the tag exists in the registry before touching the running relay
#   * health-checks the new container and rolls back if it does not come up
#
# Usage:
#   scripts/relay-updater.sh run        # process a pending request, if any
#   scripts/relay-updater.sh heartbeat  # record that the agent is alive
#
# `run` also writes a heartbeat, so the timer can simply call `run`.

set -euo pipefail

REPO_DIR="${RELAY_REPO_DIR:-/root/obelisk-relay}"
CONFIG_DIR="${RELAY_CONFIG_DIR:-$REPO_DIR/public-config}"
SERVICE="${RELAY_COMPOSE_SERVICE:-public_relay}"
HEALTH_URL="${RELAY_HEALTH_URL:-http://127.0.0.1:8081/health}"
IMAGE_REPOSITORY="${RELAY_IMAGE_REPOSITORY:-ghcr.io/obelisk-app/obelisk-relay}"
ENV_FILE="${RELAY_ENV_FILE:-$REPO_DIR/.env}"
# Relative to REPO_DIR. Empty means compose's own default (compose.yml).
COMPOSE_FILE="${RELAY_COMPOSE_FILE:-}"
HEALTH_TIMEOUT="${RELAY_HEALTH_TIMEOUT:-120}"
AGENT_VERSION=1

REQUEST_FILE="$CONFIG_DIR/update-request.json"
RESULT_FILE="$CONFIG_DIR/update-result.json"
AGENT_FILE="$CONFIG_DIR/update-agent.json"

LOG=()
log() {
  echo "[relay-updater] $*" >&2
  LOG+=("$*")
}

now() { date +%s; }

write_json() {
  # Atomic: the relay reads these files at arbitrary times and must never see a
  # half-written one.
  local target="$1" content="$2"
  printf '%s' "$content" > "$target.tmp"
  mv "$target.tmp" "$target"
}

heartbeat() {
  write_json "$AGENT_FILE" "$(jq -n \
    --argjson at "$(now)" \
    --arg version "$AGENT_VERSION" \
    '{at: $at, version: $version}')"
}

# Record the outcome for the console to read, then stop.
finish() {
  local status="$1" detail="$2" requested="${3:-}" previous="${4:-}" nonce="${5:-}"
  local log_text
  # `${LOG[@]+…}` so an empty array does not trip `set -u`.
  log_text="$(printf '%s\n' ${LOG[@]+"${LOG[@]}"})"
  write_json "$RESULT_FILE" "$(jq -n \
    --arg status "$status" \
    --arg detail "$detail" \
    --arg requested "$requested" \
    --arg previous "$previous" \
    --arg nonce "$nonce" \
    --arg log "$log_text" \
    --argjson finished_at "$(now)" \
    '{
       status: $status,
       detail: (if $detail == "" then null else $detail end),
       requested_tag: (if $requested == "" then null else $requested end),
       previous_tag: (if $previous == "" then null else $previous end),
       nonce: (if $nonce == "" then null else $nonce end),
       finished_at: $finished_at,
       log: $log
     }')"
  heartbeat
  [[ "$status" == "ok" ]] && exit 0 || exit 1
}

# Only characters Docker accepts in a tag, and nothing that could be read as a
# flag, a path, a registry or a shell metacharacter. Mirrors
# `tag_is_well_formed` in src/update.rs -- both sides validate, because neither
# trusts the other to have done it.
tag_is_valid() {
  [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]]
}

current_tag() {
  # The pin lives in the host .env, outside anything the container can write.
  if [[ -f "$ENV_FILE" ]] && grep -q '^RELAY_IMAGE_TAG=' "$ENV_FILE"; then
    grep '^RELAY_IMAGE_TAG=' "$ENV_FILE" | head -1 | cut -d= -f2-
  fi
}

set_tag() {
  local tag="$1"
  touch "$ENV_FILE"
  if grep -q '^RELAY_IMAGE_TAG=' "$ENV_FILE"; then
    # Rewrite in place without a shell-expanded sed replacement: the tag has
    # already been validated, but building the command from it is a habit worth
    # not having.
    local tmp
    tmp="$(mktemp)"
    RELAY_NEW_TAG="$tag" awk '
      /^RELAY_IMAGE_TAG=/ { print "RELAY_IMAGE_TAG=" ENVIRON["RELAY_NEW_TAG"]; next }
      { print }
    ' "$ENV_FILE" > "$tmp"
    mv "$tmp" "$ENV_FILE"
  else
    printf 'RELAY_IMAGE_TAG=%s\n' "$tag" >> "$ENV_FILE"
  fi
}

# The purple theme is bind-mounted over a hash-named stylesheet inside the image
# (see the comment at compose.yml:44). A new image has a different hash, so the
# mount lands on a path nothing loads and the branding silently reverts -- which
# has already happened twice by hand. Re-point it, or refuse the update.
resync_branding() {
  local image="$1"
  local retint="$CONFIG_DIR/branding/retint.sh"
  [[ -x "$retint" ]] || { log "no branding script at $retint; nothing to re-sync"; return 0; }

  local out
  if ! out="$("$retint" "$image" 2>&1)"; then
    log "branding re-sync failed: $(tail -2 <<<"$out")"
    return 1
  fi

  # retint.sh writes the tinted stylesheet under the hash the new image actually
  # asks for. Regenerating it is only half the job: compose still mounts the old
  # filename, which after an image change is a path nothing loads. Re-point it.
  local name
  name="$(grep -o 'index-[A-Za-z0-9_-]*\.css' <<<"$out" | head -1)"
  if [[ -z "$name" ]]; then
    log "could not determine the new stylesheet hash from retint.sh"
    return 1
  fi

  local compose_path="$REPO_DIR/${COMPOSE_FILE:-compose.yml}"
  if ! grep -q "branding/assets/index-[A-Za-z0-9_-]*\.css:" "$compose_path" 2>/dev/null; then
    log "branding re-synced to $name (no mount line in $compose_path to update)"
    return 0
  fi

  local tmp
  tmp="$(mktemp)"
  # `name` is constrained to [A-Za-z0-9_-]+.css by the grep above, so it carries
  # no sed metacharacters.
  sed -E "s#(branding/assets/)index-[A-Za-z0-9_-]+\.css(:/app/frontend/dist/assets/)index-[A-Za-z0-9_-]+\.css#\1${name}\2${name}#" \
    "$compose_path" > "$tmp"

  if ! grep -q "branding/assets/$name:" "$tmp"; then
    rm -f "$tmp"
    log "could not re-point the branding mount in $compose_path"
    return 1
  fi
  mv "$tmp" "$compose_path"
  log "branding re-synced to $name and the mount in $compose_path re-pointed"
  return 0
}

wait_for_health() {
  local deadline=$(( $(now) + HEALTH_TIMEOUT ))
  while (( $(now) < deadline )); do
    if curl -sf --max-time 5 "$HEALTH_URL" > /dev/null 2>&1; then
      return 0
    fi
    sleep 3
  done
  return 1
}

recreate() {
  # Not every relay's stack is in the default compose.yml -- lacrypta's lives in
  # compose.vps.yml -- so the file is part of the instance's wiring.
  local -a compose=(docker compose)
  [[ -n "$COMPOSE_FILE" ]] && compose+=(-f "$COMPOSE_FILE")
  ( cd "$REPO_DIR" && "${compose[@]}" up -d "$SERVICE" ) >&2
}

run() {
  heartbeat

  [[ -f "$REQUEST_FILE" ]] || { log "no pending request"; exit 0; }

  local request
  request="$(cat "$REQUEST_FILE")"
  # Consume the request up front. A crash partway through must not leave a
  # request that fires again on the next path-unit trigger, in a loop.
  rm -f "$REQUEST_FILE"

  local tag nonce requested_by
  tag="$(jq -r '.requested_tag // empty' <<<"$request" || true)"
  nonce="$(jq -r '.nonce // empty' <<<"$request" || true)"
  requested_by="$(jq -r '.requested_by // empty' <<<"$request" || true)"

  if ! tag_is_valid "$tag"; then
    log "rejected tag: $(printf '%q' "$tag")"
    finish "rejected" "The requested tag is not a valid image tag." "" "" "$nonce"
  fi

  local image="$IMAGE_REPOSITORY:$tag"
  local previous
  previous="$(current_tag)"
  log "request from ${requested_by:-unknown}: $previous -> $tag"

  if [[ "$tag" == "$previous" ]]; then
    log "already on $tag; pulling and recreating anyway to pick up a moved tag"
  fi

  if ! docker pull "$image" >&2; then
    log "could not pull $image"
    finish "failed" "The image $image could not be pulled." "$tag" "$previous" "$nonce"
  fi

  if ! resync_branding "$image"; then
    finish "failed" \
      "The branding stylesheet hash could not be determined for the new image; the update was not applied." \
      "$tag" "$previous" "$nonce"
  fi

  set_tag "$tag"
  if ! recreate; then
    log "recreate failed; rolling back"
    [[ -n "$previous" ]] && { set_tag "$previous"; resync_branding "$IMAGE_REPOSITORY:$previous" || true; recreate || true; }
    finish "rolled-back" "The new container could not be started; the previous version was restored." \
      "$tag" "$previous" "$nonce"
  fi

  if wait_for_health; then
    log "healthy on $tag"
    finish "ok" "" "$tag" "$previous" "$nonce"
  fi

  log "no health after ${HEALTH_TIMEOUT}s; rolling back to ${previous:-unknown}"
  if [[ -z "$previous" ]]; then
    finish "failed" \
      "The relay did not become healthy on $tag, and there was no previous tag recorded to roll back to." \
      "$tag" "" "$nonce"
  fi

  set_tag "$previous"
  resync_branding "$IMAGE_REPOSITORY:$previous" || true
  recreate || true
  if wait_for_health; then
    finish "rolled-back" "The relay did not become healthy on $tag; $previous was restored." \
      "$tag" "$previous" "$nonce"
  fi
  finish "failed" \
    "The relay did not become healthy on $tag, and did not come back on $previous either. Check the container logs." \
    "$tag" "$previous" "$nonce"
}

command -v docker >/dev/null || { echo "docker is required" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }
command -v curl >/dev/null || { echo "curl is required" >&2; exit 2; }

case "${1:-run}" in
  run) run ;;
  heartbeat) heartbeat ;;
  *) echo "usage: $0 [run|heartbeat]" >&2; exit 2 ;;
esac
