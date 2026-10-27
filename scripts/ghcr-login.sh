#!/usr/bin/env bash
# Authorize this machine to push relay images to ghcr.io/obelisk-app/obelisk-relay.
#
# Prints a one-time code and a link. Open the link on any device, enter the code,
# approve. The script then adds the package scopes to the GitHub CLI token and
# logs Docker into GHCR with it, so `docker buildx build --push` works.
#
# Usage: scripts/ghcr-login.sh
set -euo pipefail
export PATH="$HOME/.local/bin:$PATH"

command -v gh >/dev/null 2>&1 || { echo "GitHub CLI (gh) is missing" >&2; exit 1; }
command -v docker >/dev/null 2>&1 || { echo "docker is missing" >&2; exit 1; }

SCOPES="write:packages,read:packages"

has_scopes() {
  gh auth status --hostname github.com 2>&1 | grep -q "write:packages"
}

printf 'Authorize pushing images to ghcr.io from this machine.\n'
printf 'Open https://github.com/login/device on any device and enter the code below.\n\n'

if ! gh auth status --hostname github.com >/dev/null 2>&1; then
  # Not logged in at all: log in with the package scopes in one go.
  GH_PROMPT_DISABLED=1 BROWSER=true gh auth login --hostname github.com \
    --git-protocol https --web --scopes "$SCOPES" </dev/null
elif ! has_scopes; then
  # Logged in, but the token cannot write packages: add the scopes.
  #
  # `gh auth refresh` can sit on a prompt after the approval has already gone
  # through, so it runs in the background and is stopped as soon as the token
  # shows the new scope (or after 15 minutes).
  GH_PROMPT_DISABLED=1 BROWSER=true gh auth refresh --hostname github.com \
    --scopes "$SCOPES" </dev/null &
  refresh_pid=$!
  for _ in $(seq 1 450); do
    has_scopes && break
    kill -0 "$refresh_pid" 2>/dev/null || break
    sleep 2
  done
  kill "$refresh_pid" 2>/dev/null || true
  wait "$refresh_pid" 2>/dev/null || true
  has_scopes || { echo "Authorization did not complete; run this again." >&2; exit 1; }
else
  printf 'GitHub CLI already has write:packages.\n'
fi

user="$(gh api user --jq .login)"
gh auth token | docker login ghcr.io -u "$user" --password-stdin

printf '\nReady: docker is logged into ghcr.io as %s.\n' "$user"
printf 'Push with: docker buildx build --platform linux/amd64,linux/arm64 ... --push .\n'
