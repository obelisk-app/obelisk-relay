#!/usr/bin/env bash
# Intentionally NOT using `set -u`: bash 3.2 (macOS default) errors on empty
# array expansions like "${!arr[@]}", which would abort the wizard.
set -eo pipefail

# ╔══════════════════════════════════════════════════════════════╗
# ║          Obelisk Relay — Local Setup Wizard                  ║
# ║                                                              ║
# ║  One job: get the relay running on http://localhost:8080.    ║
# ║  No domains, no TLS, no networking. Bulletproof.             ║
# ║                                                              ║
# ║  Once this is green, run ./expose.sh to publish it to the    ║
# ║  internet via Cloudflare Tunnel.                             ║
# ║                                                              ║
# ║  Works on macOS and Linux. Re-runnable at any time.          ║
# ╚══════════════════════════════════════════════════════════════╝

RELAY_DIR="$(cd "$(dirname "$0")" && pwd)"
CONFIG_DIR="${RELAY_DIR}/config"
CONFIG_FILE="${CONFIG_DIR}/settings.local.yml"

# Mode: by default we pull a prebuilt image (~30s). Pass --build to compile
# from source (~5-10 min) — useful for forks or local code changes.
BUILD_FROM_SOURCE=false
for arg in "$@"; do
  case "$arg" in
    --build) BUILD_FROM_SOURCE=true ;;
    -h|--help)
      echo "Usage: ./setup.sh [--build]"
      echo "  --build   Compile the relay from source instead of pulling the published image"
      exit 0 ;;
  esac
done

cd "$RELAY_DIR"

OS="$(uname -s)"
IS_MAC=false
[ "$OS" = "Darwin" ] && IS_MAC=true

# ── Colors ────────────────────────────────────────────────────

if [ -t 1 ]; then
  BOLD=$'\033[1m'; DIM=$'\033[2m'
  GREEN=$'\033[0;32m'; CYAN=$'\033[0;36m'
  YELLOW=$'\033[1;33m'; RED=$'\033[0;31m'
  MAGENTA=$'\033[0;35m'; NC=$'\033[0m'
else
  BOLD=""; DIM=""; GREEN=""; CYAN=""; YELLOW=""; RED=""; MAGENTA=""; NC=""
fi

banner() {
  local text="$1"
  echo ""
  printf "${CYAN}╭"; printf '─%.0s' $(seq 1 58); printf "╮${NC}\n"
  printf "${CYAN}│${NC}  ${BOLD}%s${NC}\n" "$text"
  printf "${CYAN}╰"; printf '─%.0s' $(seq 1 58); printf "╯${NC}\n"
  echo ""
}

section() {
  echo ""
  printf "${MAGENTA}━━━ ${BOLD}%s${NC} ${MAGENTA}" "$1"
  local n=$(( 52 - ${#1} )); [ $n -lt 1 ] && n=1
  printf '━%.0s' $(seq 1 $n); printf "${NC}\n\n"
}

ok()    { printf "  ${GREEN}✓${NC} %s\n" "$1"; }
warn()  { printf "  ${YELLOW}!${NC} %s\n" "$1"; }
fail()  { printf "  ${RED}✗${NC} %s\n" "$1"; }
info()  { printf "  ${DIM}%s${NC}\n" "$1"; }
ask()   { printf "  ${CYAN}?${NC} ${BOLD}%s${NC} " "$1"; }

prompt_default() {
  local p="$1" d="$2" v="$3" input=""
  ask "${p} ${DIM}[${d}]${NC}:"
  read -r input || true
  eval "${v}=\"${input:-$d}\""
}

prompt_yn() {
  local p="$1" d="${2:-y}" hint input=""
  if [ "$d" = "y" ]; then hint="Y/n"; else hint="y/N"; fi
  ask "${p} ${DIM}[${hint}]${NC}:"
  read -r input || true
  input="${input:-$d}"
  case "$input" in [yY]*) return 0 ;; *) return 1 ;; esac
}

lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

# ── Bech32 (npub → hex) ───────────────────────────────────────

BECH32_CHARSET="qpzry9x8gf2tvdw0s3jn54khce6mua7l"

npub_to_hex() {
  local npub="$1"
  [[ "$npub" =~ ^npub1 ]] || { echo ""; return 1; }
  local data_part="${npub:5}"
  local -a data5=()
  local i j ch idx
  for (( i=0; i<${#data_part}; i++ )); do
    ch="${data_part:$i:1}"; idx=-1
    for (( j=0; j<${#BECH32_CHARSET}; j++ )); do
      if [ "${BECH32_CHARSET:$j:1}" = "$ch" ]; then idx=$j; break; fi
    done
    [ $idx -eq -1 ] && { echo ""; return 1; }
    data5+=( $idx )
  done
  local data_len=$(( ${#data5[@]} - 6 ))
  [ $data_len -le 0 ] && { echo ""; return 1; }
  local acc=0 bits=0 hex="" byte
  for (( i=0; i<data_len; i++ )); do
    acc=$(( (acc << 5) | ${data5[$i]} ))
    bits=$(( bits + 5 ))
    while [ $bits -ge 8 ]; do
      bits=$(( bits - 8 ))
      byte=$(( (acc >> bits) & 0xff ))
      hex+=$(printf '%02x' $byte)
    done
  done
  if [ ${#hex} -eq 64 ]; then echo "$hex"; return 0; fi
  echo ""; return 1
}

validate_pubkey() {
  local input
  input="$(printf '%s' "$1" | tr -d '[:space:]')"
  if [[ "$input" =~ ^npub1 ]]; then
    local hex; hex=$(npub_to_hex "$input") || true
    [ -n "$hex" ] && { echo "$hex"; return 0; }
    echo ""; return 1
  fi
  if [[ "$input" =~ ^[0-9a-fA-F]{64}$ ]]; then
    lower "$input"; return 0
  fi
  echo ""; return 1
}

generate_hex_key() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 32
  elif [ -r /dev/urandom ]; then
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'
  else
    local key="" i
    for i in $(seq 1 32); do key+=$(printf '%02x' $(( RANDOM % 256 ))); done
    echo "$key"
  fi
}

# ── Portable helpers ──────────────────────────────────────────

port_in_use() {
  local p="$1"
  if command -v lsof >/dev/null 2>&1; then
    lsof -nP -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1 && return 0
  fi
  if command -v ss >/dev/null 2>&1; then
    ss -tln 2>/dev/null | awk '{print $4}' | grep -Eq "[:.]${p}\$" && return 0
  fi
  if command -v netstat >/dev/null 2>&1; then
    netstat -an 2>/dev/null | awk '{print $4}' | grep -Eq "[:.]${p}\$" && return 0
  fi
  return 1
}

disk_avail_gb() {
  df -k . 2>/dev/null | awk 'NR==2 { printf "%d", $4/1024/1024 }'
}

# ══════════════════════════════════════════════════════════════
clear 2>/dev/null || true
banner "Obelisk Relay — Local Setup"

echo "  This installs and starts the relay on http://localhost:8080."
echo "  Nothing is exposed to the internet — that's a separate step."
echo ""
printf "  ${DIM}When this finishes, run ./expose.sh to publish via Cloudflare.${NC}\n"
printf "  ${DIM}Re-run setup.sh anytime to update config. Ctrl+C to cancel.${NC}\n"

# ── Step 1: Docker ────────────────────────────────────────────

section "Step 1/4 — Docker"

if ! command -v docker >/dev/null 2>&1; then
  fail "Docker is not installed."
  echo ""
  if $IS_MAC; then
    echo "  Install Docker Desktop: https://www.docker.com/products/docker-desktop"
  else
    echo "  Install:  curl -fsSL https://get.docker.com | sh"
  fi
  echo ""
  exit 1
fi
ok "Docker $(docker --version 2>/dev/null | awk '{print $3}' | tr -d ',')"

if ! docker info >/dev/null 2>&1; then
  warn "Docker daemon is not running."
  # Try to start whatever Docker runtime is installed. Don't fail if none of
  # these work — we'll fall through to a wait loop so the user can start it
  # by hand and we'll detect it when it's up.
  if $IS_MAC; then
    for app in Docker OrbStack "Rancher Desktop"; do
      if [ -d "/Applications/${app}.app" ]; then
        info "Launching ${app}..."
        open -a "$app" >/dev/null 2>&1 || true
        break
      fi
    done
    if command -v colima >/dev/null 2>&1 && ! pgrep -f colima >/dev/null 2>&1; then
      info "Starting colima in background..."
      (colima start >/dev/null 2>&1 &) || true
    fi
  else
    if command -v systemctl >/dev/null 2>&1; then
      info "Trying: sudo systemctl start docker"
      sudo -n systemctl start docker >/dev/null 2>&1 || \
        info "(needs password — run it manually if it doesn't come up)"
    fi
  fi

  printf "  ${DIM}Waiting up to 2 min for Docker to be ready"
  READY=false
  for i in $(seq 1 60); do
    if docker info >/dev/null 2>&1; then READY=true; break; fi
    sleep 2; printf "."
  done
  printf "${NC}\n"

  if $READY; then
    ok "Docker daemon is running"
  else
    fail "Docker daemon is still not responding."
    echo ""
    if $IS_MAC; then
      info "Start your Docker runtime manually (Docker Desktop / OrbStack / colima start),"
      info "wait until its icon shows ready, then re-run ./setup.sh."
    else
      info "Start the docker service (e.g. sudo systemctl start docker) and re-run."
    fi
    exit 1
  fi
else
  ok "Docker daemon is running"
fi

if docker compose version >/dev/null 2>&1; then
  ok "Docker Compose $(docker compose version --short 2>/dev/null || echo)"
else
  fail "Docker Compose plugin not found."
  info "Install: https://docs.docker.com/compose/install/"
  exit 1
fi

# ── Step 2: Resources ─────────────────────────────────────────

section "Step 2/4 — Resources"

if port_in_use 8080; then
  warn "Port 8080 is already in use. The relay may fail to bind."
  command -v lsof >/dev/null 2>&1 && info "Check: lsof -nP -iTCP:8080 -sTCP:LISTEN"
  prompt_yn "Continue anyway?" "n" || exit 1
else
  ok "Port 8080 is available"
fi

DISK_AVAIL="$(disk_avail_gb)"
if [ -n "$DISK_AVAIL" ] && [ "$DISK_AVAIL" -gt 3 ] 2>/dev/null; then
  ok "Disk space: ${DISK_AVAIL}GB available"
else
  warn "Low disk space (${DISK_AVAIL:-?}GB). First build needs ~3GB."
  prompt_yn "Continue anyway?" "n" || exit 1
fi

# ── Step 3: Admin & whitelist ─────────────────────────────────

section "Step 3/4 — Admin & Whitelist"

echo "  The admin pubkey controls the relay. It's the first whitelisted"
echo "  identity and can create/manage groups."
echo ""
printf "  ${DIM}Paste your npub (npub1...) or 64-char hex pubkey.${NC}\n"
echo ""

ADMIN_HEX=""
ADMIN_NPUB=""
while [ -z "$ADMIN_HEX" ]; do
  ask "Admin npub or hex pubkey:"
  read -r admin_input || true
  [ -z "${admin_input:-}" ] && { warn "Required."; continue; }
  ADMIN_HEX=$(validate_pubkey "$admin_input") || true
  if [ -z "$ADMIN_HEX" ]; then
    fail "Invalid pubkey format. Need npub1... or 64 hex chars."
  else
    ok "Admin pubkey: ${ADMIN_HEX:0:16}...${ADMIN_HEX: -8}"
    [[ "$admin_input" =~ ^npub1 ]] && ADMIN_NPUB="$admin_input"
  fi
done

echo ""
EXTRA_PUBKEYS=()
EXTRA_NPUBS=()
if prompt_yn "Add more whitelisted pubkeys?" "n"; then
  while true; do
    echo ""
    ask "npub or hex pubkey (empty to finish):"
    read -r extra_input || true
    [ -z "${extra_input:-}" ] && break
    extra_hex=$(validate_pubkey "$extra_input") || true
    if [ -z "$extra_hex" ]; then
      fail "Invalid pubkey. Skipping."
    elif [ "$extra_hex" = "$ADMIN_HEX" ]; then
      warn "Already added as admin."
    else
      EXTRA_PUBKEYS+=("$extra_hex")
      [[ "$extra_input" =~ ^npub1 ]] && EXTRA_NPUBS+=("$extra_input") || EXTRA_NPUBS+=("")
      ok "Added: ${extra_hex:0:16}...${extra_hex: -8}"
    fi
  done
fi

# ── Step 4: Write config & launch ─────────────────────────────

section "Step 4/4 — Build & Launch"

if [ -f "$CONFIG_FILE" ]; then
  BACKUP="${CONFIG_FILE}.bak.$(date +%Y%m%d%H%M%S)"
  cp "$CONFIG_FILE" "$BACKUP"
  info "Existing config backed up to: ${BACKUP##*/}"
fi

RELAY_SECRET_KEY=$(generate_hex_key)
mkdir -p "$CONFIG_DIR"
{
  cat <<YAML
relay:
  relay_secret_key: "${RELAY_SECRET_KEY}"
  # relay_url is set at runtime by ./expose.sh when you publish a domain.
  # Until then, the relay identifies itself with this placeholder.
  relay_url: "ws://localhost:8080"
  db_path: "/app/db"
  local_addr: "0.0.0.0:8080"

  whitelisted_pubkeys:
YAML
  if [ -n "$ADMIN_NPUB" ]; then
    echo "    # ${ADMIN_NPUB} (admin)"
  else
    echo "    # Admin"
  fi
  echo "    - \"${ADMIN_HEX}\""
  for i in "${!EXTRA_PUBKEYS[@]}"; do
    npub="${EXTRA_NPUBS[$i]}"
    [ -n "$npub" ] && echo "    # ${npub}"
    echo "    - \"${EXTRA_PUBKEYS[$i]}\""
  done
  cat <<YAML

  max_subscriptions: 50
  max_limit: 500

  websocket:
    max_connection_duration: "24h"
    idle_timeout: "30m"
    max_connections: 300
YAML
} > "$CONFIG_FILE"

ok "Config written to config/settings.local.yml"

echo ""
if $BUILD_FROM_SOURCE; then
  printf "  ${BOLD}Building from source...${NC}\n"
  printf "  ${DIM}(first build: 3-10 min — Rust + frontend)${NC}\n"
  echo ""
  if ! docker compose up -d --build groups_relay; then
    echo ""
    fail "docker compose build/up failed."
    echo "  Inspect with: docker compose logs groups_relay"
    exit 1
  fi
else
  printf "  ${BOLD}Pulling prebuilt relay image...${NC}\n"
  printf "  ${DIM}(~30 sec; pass --build to compile from source instead)${NC}\n"
  echo ""
  if ! docker compose pull groups_relay; then
    warn "Image pull failed — falling back to building from source."
    info "(This usually means the image hasn't been published yet for this commit.)"
    if ! docker compose up -d --build groups_relay; then
      fail "docker compose build/up failed."
      echo "  Inspect with: docker compose logs groups_relay"
      exit 1
    fi
  else
    if ! docker compose up -d groups_relay; then
      fail "docker compose up failed."
      echo "  Inspect with: docker compose logs groups_relay"
      exit 1
    fi
  fi
fi

echo ""
printf "  Waiting for relay to be healthy"
HEALTHY=false
for i in $(seq 1 40); do
  if curl -sf --max-time 3 http://localhost:8080/health >/dev/null 2>&1; then
    HEALTHY=true; break
  fi
  sleep 3; printf "."
done
echo ""

if $HEALTHY; then
  banner "Relay is live on localhost"
  printf "  ${BOLD}WebSocket:${NC}    ${CYAN}ws://localhost:8080${NC}\n"
  printf "  ${BOLD}Web UI:${NC}       ${CYAN}http://localhost:8080/${NC}\n"
  printf "  ${BOLD}Health:${NC}       ${CYAN}http://localhost:8080/health${NC}\n"
  echo ""
  printf "  ${BOLD}Next:${NC} publish it to the internet with a domain.\n"
  printf "    ${CYAN}./expose.sh${NC}\n"
  echo ""
  printf "  ${BOLD}Manage:${NC}\n"
  printf "    ${DIM}docker compose ps${NC}            status\n"
  printf "    ${DIM}docker compose logs -f${NC}       logs\n"
  printf "    ${DIM}docker compose restart${NC}       restart\n"
  printf "    ${DIM}docker compose down${NC}          stop\n"
  echo ""
else
  fail "Relay did not become healthy within 2 minutes."
  echo "  Check: docker compose logs groups_relay"
  exit 1
fi
