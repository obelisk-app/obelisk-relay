#!/usr/bin/env bash
#
# Regenerate the purple stylesheet that public.obelisk.ar is served with.
#
# WHY THIS EXISTS
#
# compose.yml bind-mounts a tinted copy of the admin CSS over the one baked into
# the image, matching it by Vite's content-hashed filename:
#
#   ./public-config/branding/assets/index-<HASH>.css -> /app/frontend/dist/assets/index-<HASH>.css
#
# That only works while <HASH> matches. Any frontend change produces a new hash,
# index.html points at the new file, and the override becomes a file nothing
# loads -- the site silently reverts to the default green with no error
# anywhere. That has now happened twice (index-DUXQZBvw.css and
# index-PDsRJO3H.css are both dead overrides from previous bundles).
#
# So: never hand-edit the tinted file. Build the image, then run this against
# it. It reads the CSS out of the image itself, so the hash it writes is by
# construction the hash the image will ask for.
#
# USAGE
#
#   docker compose build public_relay
#   scripts/retint-branding.sh ghcr.io/obelisk-app/obelisk-relay:<tag>
#   # follow the printed instruction to update compose.yml, then:
#   docker compose up -d public_relay
#
# A UI-driven update runs this for you: scripts/relay-updater.sh calls it
# against the incoming image and re-points the compose mount before recreating
# the container.
#
set -euo pipefail

IMAGE="${1:-nostr-relay-public_relay}"
# Output goes into the relay's config directory; this script does not.
#
# It used to live at public-config/branding/retint.sh and derive OUT_DIR from
# its own location. That directory is bind-mounted into the relay read-write,
# so the relay could rewrite this script -- and the update agent executes it as
# root on the host. A remote-code-execution bug in a process terminating
# untrusted WebSocket traffic would then have been root, which is the exact
# outcome not giving the relay a Docker socket was meant to prevent.
#
# Code lives here, under scripts/, which is not mounted into any container.
# Only the generated stylesheet lands in the config directory.
OUT_DIR="${RETINT_OUT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/public-config/branding/assets}"

# The accent pair, and the same colour in the rgba() form the bundle also uses.
# The earlier hand-made overrides replaced only the hex, leaving green glows and
# focus rings scattered through a purple theme; doing both here fixes that.
GREEN_HEX='b4f953'      ; PURPLE_HEX='a855f7'
GREEN_HOVER='c5ff6e'    ; PURPLE_HOVER='c084fc'
GREEN_RGB='180,249,83'  ; PURPLE_RGB='168,85,247'
GREEN_RGB_SP='180, 249, 83' ; PURPLE_RGB_SP='168, 85, 247'

cid="$(docker create "$IMAGE")"
trap 'docker rm -f "$cid" >/dev/null 2>&1 || true' EXIT

# Ask index.html which stylesheet it actually loads, rather than guessing from
# a directory listing -- the image still carries the stale overrides.
html="$(docker cp "$cid:/app/frontend/dist/index.html" - | tar -xO)"
css_name="$(grep -o 'assets/index-[A-Za-z0-9_-]*\.css' <<<"$html" | head -1 | xargs basename)"

if [[ -z "$css_name" ]]; then
  echo "could not find the stylesheet reference in index.html" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
docker cp "$cid:/app/frontend/dist/assets/$css_name" - | tar -xO \
  | sed -e "s/#$GREEN_HEX/#$PURPLE_HEX/g" \
        -e "s/#$GREEN_HOVER/#$PURPLE_HOVER/g" \
        -e "s/$GREEN_RGB_SP/$PURPLE_RGB_SP/g" \
        -e "s/$GREEN_RGB/$PURPLE_RGB/g" \
  > "$OUT_DIR/$css_name"

# `|| true` because grep exits 1 when it finds nothing, which here is the
# success case -- without it the script fails exactly when the tint is perfect.
remaining="$(grep -o "$GREEN_HEX\|$GREEN_RGB" "$OUT_DIR/$css_name" | wc -l | tr -d ' ' || true)"

echo "wrote public-config/branding/assets/$css_name"
echo "green literals remaining: $remaining (expect 0)"
echo
echo "Now point compose.yml's public_relay mount at it:"
echo "  ./public-config/branding/assets/$css_name:/app/frontend/dist/assets/$css_name:ro"
