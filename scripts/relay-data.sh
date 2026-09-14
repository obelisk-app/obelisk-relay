#!/usr/bin/env bash
# Export a relay's data and configuration into a portable bundle, and import
# that bundle into a fresh instance.
#
# Events are moved as JSONL via the export_import tool already shipped in the
# relay image; this script wraps it with the configuration, an integrity check
# and a manifest, so a bundle is enough to stand up an equivalent relay.

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/relay-data.sh export <output-dir> [options]
  scripts/relay-data.sh import <bundle-dir> [options]

Export options:
  --container NAME     Running relay container to export (default: nostr-relay-public_relay-1)
  --db PATH            LMDB directory on the host (default: read from the container)
  --config PATH        Config directory on the host (default: read from the container)
  --no-pause           Do not pause the container while copying (risks a torn read)
  --strict             Abort if the source database reports index corruption

Import options:
  --db PATH            Destination LMDB directory (must be empty or absent)
  --config PATH        Destination config directory
  --new-identity       Generate a fresh relay_secret_key instead of reusing the exported one
  --image REF          Relay image to run the tools from (default: taken from the bundle)
  --yes                Skip the confirmation prompt

A bundle contains:
  events/scope_*.jsonl   every stored event, one JSON object per line, per scope
  config/                settings.local.yml plus the runtime JSON state files
  manifest.txt           source relay, commit, image, per-scope event counts
  SHA256SUMS             checksums for everything above

SECURITY: config/settings.local.yml contains relay_secret_key -- the relay's
private identity. A bundle is a secret. Do not commit or share one. Use
--new-identity on import unless you intend the new instance to BE the old relay.
EOF
}

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"; }

DEFAULT_CONTAINER=nostr-relay-public_relay-1

cmd_export() {
  local out="" container="$DEFAULT_CONTAINER" db="" config="" pause=1 strict=0
  out="${1:-}"; shift || true
  [[ -n "$out" ]] || { usage >&2; exit 2; }

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --container) container="$2"; shift 2 ;;
      --db) db="$2"; shift 2 ;;
      --config) config="$2"; shift 2 ;;
      --no-pause) pause=0; shift ;;
      --strict) strict=1; shift ;;
      *) die "unknown option: $1" ;;
    esac
  done

  need docker; need tar; need sha256sum
  docker inspect "$container" >/dev/null || die "no such container: $container"

  local image
  image="$(docker inspect -f '{{.Config.Image}}' "$container")"

  # Resolve the db/config locations from the container's own mounts unless the
  # caller overrode them, so this works for both volume- and bind-mounted relays.
  if [[ -z "$db" ]]; then
    db="$(docker inspect -f '{{range .Mounts}}{{if eq .Destination "/app/db"}}{{.Source}}{{end}}{{end}}' "$container")"
    [[ -n "$db" ]] || die "could not resolve the LMDB mount; pass --db"
  fi
  if [[ -z "$config" ]]; then
    config="$(docker inspect -f '{{range .Mounts}}{{if eq .Destination "/app/config"}}{{.Source}}{{end}}{{end}}' "$container")"
    [[ -n "$config" ]] || die "could not resolve the config mount; pass --config"
  fi
  [[ -f "$db/data.mdb" ]] || die "no LMDB at $db/data.mdb"

  local stamp stage
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  stage="$out/obelisk-relay-export-$stamp"
  mkdir -p "$stage/events" "$stage/config" "$stage/dbcopy"

  local paused=""
  cleanup() {
    [[ -n "$paused" ]] && docker unpause "$paused" >/dev/null 2>&1 || true
    rm -rf "$stage/dbcopy"
  }
  trap cleanup EXIT

  # Copy under a pause so the snapshot is not torn mid-write, then export from
  # the copy -- the live database stays untouched by the export tool.
  echo "Snapshotting $container"
  if (( pause )); then paused="$container"; docker pause "$container" >/dev/null; fi
  cp -a "$db/data.mdb" "$stage/dbcopy/data.mdb"
  cp -a "$config/." "$stage/config/"
  if (( pause )); then docker unpause "$container" >/dev/null; paused=""; fi

  # Integrity is recorded, not enforced. Stale entries in the deleted-ids index
  # are common on a long-running relay, and an export/import cycle is the
  # documented way to clear them -- refusing to export because the database
  # needs exporting would be backwards. --strict opts into the old behaviour.
  echo "Checking integrity"
  docker run --rm --entrypoint /app/nostr-lmdb-integrity \
    -v "$stage/dbcopy:/db" "$image" --db-path /db > "$stage/integrity.txt" 2>&1 || true
  if ! grep -Fq "No corrupted entries found." "$stage/integrity.txt"; then
    echo
    echo "WARNING: the source database reports index corruption:"
    sed -n '1,6p' "$stage/integrity.txt"
    echo "Recorded in the bundle as integrity.txt. Events themselves export fine;"
    echo "importing this bundle produces a clean database."
    echo
    (( strict )) && die "--strict given; refusing to export a database reporting corruption"
  fi

  echo "Exporting events"
  docker run --rm --entrypoint /app/export_import \
    -v "$stage/dbcopy:/db" -v "$stage/events:/out" \
    "$image" export --db /db --output /out --force

  {
    echo "created_at_utc=$stamp"
    echo "source_container=$container"
    echo "source_image=$image"
    echo "source_db=$db"
    echo "source_config=$config"
    for f in "$stage"/events/*.jsonl; do
      [[ -e "$f" ]] || continue
      echo "events_$(basename "$f" .jsonl)=$(wc -l < "$f")"
    done
  } > "$stage/manifest.txt"

  rm -rf "$stage/dbcopy"
  trap - EXIT

  ( cd "$stage" && find events config manifest.txt integrity.txt -type f -print0 \
      | sort -z | xargs -0 sha256sum > SHA256SUMS )
  chmod -R go-rwx "$stage"

  echo
  echo "Bundle: $stage"
  echo "Events: $(cat "$stage"/events/*.jsonl 2>/dev/null | wc -l) across $(ls "$stage"/events/*.jsonl 2>/dev/null | wc -l) scope(s)"
  echo
  echo "This bundle contains relay_secret_key. Treat it as a secret."
}

cmd_import() {
  local bundle="" db="" config="" image="" new_identity=0 assume_yes=0
  bundle="${1:-}"; shift || true
  [[ -n "$bundle" ]] || { usage >&2; exit 2; }

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --db) db="$2"; shift 2 ;;
      --config) config="$2"; shift 2 ;;
      --image) image="$2"; shift 2 ;;
      --new-identity) new_identity=1; shift ;;
      --yes) assume_yes=1; shift ;;
      *) die "unknown option: $1" ;;
    esac
  done

  need docker; need sha256sum
  [[ -d "$bundle/events" ]] || die "no events/ in $bundle"
  [[ -d "$bundle/config" ]] || die "no config/ in $bundle"
  [[ -n "$db" && -n "$config" ]] || die "--db and --config are required on import"

  echo "Verifying checksums"
  ( cd "$bundle" && sha256sum -c SHA256SUMS --quiet ) || die "bundle checksums do not match"

  [[ -n "$image" ]] || image="$(sed -n 's/^source_image=//p' "$bundle/manifest.txt")"
  [[ -n "$image" ]] || die "could not determine the relay image; pass --image"

  # Refuse to merge into an existing database: silently mixing two relays'
  # events is far harder to unpick than being told to start clean.
  if [[ -f "$db/data.mdb" ]]; then
    die "$db/data.mdb already exists; import targets a fresh instance"
  fi
  mkdir -p "$db" "$config"

  if (( ! assume_yes )); then
    echo
    echo "Import $(cat "$bundle"/events/*.jsonl | wc -l) events into $db"
    echo "and configuration into $config."
    (( new_identity )) && echo "A NEW relay identity will be generated." \
      || echo "The EXPORTED relay identity will be reused -- this instance becomes that relay."
    read -r -p "Continue? [y/N] " reply
    [[ "$reply" == "y" || "$reply" == "Y" ]] || die "aborted"
  fi

  echo "Restoring configuration"
  cp -a "$bundle/config/." "$config/"

  if (( new_identity )); then
    need openssl
    local key
    key="$(openssl rand -hex 32)"
    # Replace only the key line; everything else about the relay carries over.
    if grep -q '^\s*relay_secret_key:' "$config/settings.local.yml" 2>/dev/null; then
      sed -i "s|^\(\s*\)relay_secret_key:.*|\1relay_secret_key: \"$key\"|" "$config/settings.local.yml"
      echo "Generated a new relay identity."
    else
      die "no relay_secret_key in the bundle's settings.local.yml; set one manually"
    fi
  fi

  echo "Importing events"
  docker run --rm --entrypoint /app/export_import \
    -v "$(readlink -f "$db"):/db" -v "$(readlink -f "$bundle/events"):/in:ro" \
    "$image" import --db /db --input /in --yes --skip-errors

  echo "Verifying imported database"
  docker run --rm --entrypoint /app/nostr-lmdb-integrity \
    -v "$(readlink -f "$db"):/db" "$image" --db-path /db

  echo
  echo "Imported into $db (config in $config)."
  echo "Review $config/settings.local.yml -- relay_url and admin_pubkeys usually"
  echo "need updating for the new host -- then start the relay."
}

case "${1:-}" in
  export) shift; cmd_export "$@" ;;
  import) shift; cmd_import "$@" ;;
  -h|--help|"") usage ;;
  *) usage >&2; exit 2 ;;
esac
