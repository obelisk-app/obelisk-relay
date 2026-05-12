#!/usr/bin/env bash
# See setup.sh: avoid `-u` for bash 3.2 empty-array safety.
set -eo pipefail

# ╔══════════════════════════════════════════════════════════════╗
# ║          Obelisk Relay — Expose via Cloudflare Tunnel        ║
# ║                                                              ║
# ║  Publishes your local relay to wss://your.domain over a      ║
# ║  Cloudflare Tunnel — no port forwarding, no public IP, no    ║
# ║  reverse proxy. The tunnel runs as a Docker sidecar that     ║
# ║  auto-restarts on reboot.                                    ║
# ║                                                              ║
# ║  Prerequisites:                                              ║
# ║    1. ./setup.sh ran successfully (relay live on :8080)      ║
# ║    2. A domain in your Cloudflare account                    ║
# ║    3. A tunnel token from the Cloudflare dashboard           ║
# ╚══════════════════════════════════════════════════════════════╝

RELAY_DIR="$(cd "$(dirname "$0")" && pwd)"
OVERRIDE_FILE="${RELAY_DIR}/compose.cloudflared.yml"
ENV_FILE="${RELAY_DIR}/.cloudflared.env"
CONFIG_FILE="${RELAY_DIR}/config/settings.local.yml"

cd "$RELAY_DIR"

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

prompt_yn() {
  local p="$1" d="${2:-y}" hint input=""
  if [ "$d" = "y" ]; then hint="Y/n"; else hint="y/N"; fi
  ask "${p} ${DIM}[${hint}]${NC}:"
  read -r input || true
  input="${input:-$d}"
  case "$input" in [yY]*) return 0 ;; *) return 1 ;; esac
}

# ── Cross-platform in-place sed ───────────────────────────────
sed_inplace() {
  if [ "$(uname -s)" = "Darwin" ]; then sed -i '' "$@"; else sed -i "$@"; fi
}

clear 2>/dev/null || true
banner "Publish via Cloudflare Tunnel"

# ── Step 1: Verify relay is running locally ───────────────────

section "Step 1/4 — Verify Local Relay"

if ! docker compose ps --status running --services 2>/dev/null | grep -q '^groups_relay$'; then
  fail "groups_relay container is not running."
  info "Run ./setup.sh first to install the relay locally."
  exit 1
fi
ok "groups_relay is running"

if curl -sf --max-time 3 http://localhost:8080/health >/dev/null 2>&1; then
  ok "Relay responds on http://localhost:8080"
else
  warn "Relay container is up but /health didn't respond."
  info "Check: docker compose logs groups_relay"
  prompt_yn "Continue anyway?" "n" || exit 1
fi

# ── Step 2: Cloudflare account setup walkthrough ──────────────

section "Step 2/4 — Cloudflare Setup"

cat <<EOF
  We use a Cloudflare Tunnel because it's free, has no port-forwarding,
  works behind NAT, and Cloudflare handles all TLS automatically.

  ${BOLD}Do this in your browser (5 minutes):${NC}

    ${BOLD}1.${NC} Sign up at https://dash.cloudflare.com (free).
    ${BOLD}2.${NC} Add your domain to Cloudflare. Follow the on-screen
       instructions to change your domain's nameservers at your
       registrar to the ones Cloudflare gives you.
       ${DIM}(Wait until the domain status shows "Active".)${NC}
    ${BOLD}3.${NC} Open Zero Trust: https://one.dash.cloudflare.com
       (the first time, pick the Free plan — no card required).
    ${BOLD}4.${NC} In the left menu: ${BOLD}Networks → Tunnels → Create a tunnel${NC}
        • Connector: ${BOLD}Cloudflared${NC}
        • Name: ${BOLD}obelisk-relay${NC} (or anything)
        • On the next screen, copy the ${BOLD}token${NC} shown
          (the long string after "--token" in the install command).
    ${BOLD}5.${NC} On the "Public hostnames" step, add ONE entry:
        • Subdomain: ${BOLD}relay${NC}   (or whatever you want)
        • Domain:    your domain
        • Path:      (leave empty)
        • Service:   Type ${BOLD}HTTP${NC}, URL ${BOLD}groups_relay:8080${NC}
        • ${DIM}(Important: use the container name "groups_relay", not localhost.)${NC}
       Save it.

  When you have the token ready, paste it below.

EOF

CF_TOKEN=""
while [ -z "$CF_TOKEN" ]; do
  ask "Paste your Cloudflare Tunnel token:"
  read -r CF_TOKEN || true
  CF_TOKEN="$(printf '%s' "${CF_TOKEN:-}" | tr -d '[:space:]')"
  if [ -z "$CF_TOKEN" ]; then warn "Token is required."; continue; fi
  # Cloudflare tokens are long base64-ish strings; sanity check length only.
  if [ ${#CF_TOKEN} -lt 40 ]; then
    fail "That doesn't look like a tunnel token (too short)."
    CF_TOKEN=""
  fi
done
ok "Token captured"

echo ""
ask "What's the full hostname you set in Cloudflare? (e.g. relay.example.com):"
read -r RELAY_HOST || true
RELAY_HOST="$(printf '%s' "${RELAY_HOST:-}" | tr -d '[:space:]')"
if [ -z "$RELAY_HOST" ]; then
  fail "Hostname is required to update the relay's advertised URL."
  exit 1
fi
RELAY_URL="wss://${RELAY_HOST}"
ok "Will advertise: ${RELAY_URL}"

# ── Step 3: Write compose override + env ──────────────────────

section "Step 3/4 — Wire Up the Tunnel"

# Token lives only in .cloudflared.env (gitignored), never in compose.
umask 077
cat > "$ENV_FILE" <<EOF
# Generated by ./expose.sh — keep this file private.
TUNNEL_TOKEN=${CF_TOKEN}
EOF
umask 022
ok "Saved token to .cloudflared.env (private, gitignored)"

# Ensure .gitignore protects secrets
if [ -f .gitignore ]; then
  grep -qxF '.cloudflared.env' .gitignore 2>/dev/null || echo '.cloudflared.env' >> .gitignore
else
  echo '.cloudflared.env' > .gitignore
fi

cat > "$OVERRIDE_FILE" <<'YAML'
# Generated by ./expose.sh — Cloudflare Tunnel sidecar.
# Brought up alongside the main compose file:
#   docker compose -f compose.yml -f compose.cloudflared.yml up -d
services:
  cloudflared:
    image: cloudflare/cloudflared:latest
    restart: unless-stopped
    command: tunnel --no-autoupdate run
    env_file:
      - .cloudflared.env
    depends_on:
      - groups_relay
    # cloudflared makes only outbound connections to Cloudflare's edge,
    # so no ports are published on the host.
YAML
ok "Wrote compose.cloudflared.yml"

# Update the relay's advertised URL so NIP-11 / NIP-42 reflect the public hostname
if [ -f "$CONFIG_FILE" ]; then
  if grep -q '^[[:space:]]*relay_url:' "$CONFIG_FILE"; then
    sed_inplace "s|^\([[:space:]]*\)relay_url:.*|\1relay_url: \"${RELAY_URL}\"|" "$CONFIG_FILE"
    ok "Updated relay_url in config/settings.local.yml"
  else
    warn "No relay_url key found in settings.local.yml — skipping rewrite."
  fi
fi

# ── Step 4: Launch ────────────────────────────────────────────

section "Step 4/4 — Launch"

printf "  ${BOLD}Starting cloudflared sidecar and restarting relay...${NC}\n"
echo ""

if ! docker compose -f compose.yml -f "$OVERRIDE_FILE" up -d --build groups_relay cloudflared; then
  fail "Failed to start tunnel."
  echo "  Inspect with: docker compose -f compose.yml -f $OVERRIDE_FILE logs cloudflared"
  exit 1
fi

echo ""
printf "  Waiting for the tunnel to register"
TUNNEL_OK=false
for i in $(seq 1 30); do
  if docker compose -f compose.yml -f "$OVERRIDE_FILE" logs --tail 50 cloudflared 2>/dev/null \
       | grep -qE 'Registered tunnel connection|Connection [a-f0-9-]+ registered'; then
    TUNNEL_OK=true; break
  fi
  sleep 2; printf "."
done
echo ""

if $TUNNEL_OK; then
  banner "Your relay is on the internet"
  printf "  ${BOLD}Public WebSocket:${NC}  ${CYAN}%s${NC}\n" "$RELAY_URL"
  printf "  ${BOLD}Public Web UI:${NC}     ${CYAN}https://%s/${NC}\n" "$RELAY_HOST"
  printf "  ${BOLD}Local fallback:${NC}    ${CYAN}http://localhost:8080/${NC}\n"
  echo ""
  printf "  ${BOLD}Useful commands${NC} (always pass both compose files):\n"
  printf "    ${DIM}alias dco='docker compose -f compose.yml -f compose.cloudflared.yml'${NC}\n"
  printf "    ${DIM}dco ps${NC}            status\n"
  printf "    ${DIM}dco logs -f cloudflared${NC}   tunnel logs\n"
  printf "    ${DIM}dco restart${NC}       restart everything\n"
  printf "    ${DIM}dco down${NC}          stop relay + tunnel\n"
  echo ""
  printf "  ${BOLD}To stop publishing but keep the relay running locally:${NC}\n"
  printf "    ${DIM}docker compose -f compose.yml -f compose.cloudflared.yml stop cloudflared${NC}\n"
  echo ""
  printf "  ${DIM}On reboot, both containers come back up automatically.${NC}\n"
  echo ""
else
  warn "Tunnel didn't register within 60s — but the container is up."
  echo ""
  echo "  Check the cloudflared logs for the reason:"
  echo "    docker compose -f compose.yml -f compose.cloudflared.yml logs cloudflared"
  echo ""
  echo "  Most common causes:"
  echo "    • Token was copied incorrectly"
  echo "    • Domain isn't fully active in Cloudflare yet"
  echo "    • Public hostname in dashboard points to wrong service URL"
  echo "      (must be: HTTP, groups_relay:8080)"
  exit 1
fi
