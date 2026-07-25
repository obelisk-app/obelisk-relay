#!/usr/bin/env bash
set -euo pipefail

BACKUP_ROOT=/root/relay-backups/obelisk-relays
PUBLIC_CONTAINER=nostr-relay-public_relay-1
PUBLIC_DB=/var/lib/docker/volumes/nostr-relay_public-relay-db/_data
PUBLIC_CONFIG=/root/obelisk-relay/public-config
LACRYPTA_CONTAINER=lacrypta-relay-relay-1
LACRYPTA_DB=/root/lacrypta-relay/db
LACRYPTA_CONFIG=/root/lacrypta-relay/config

install -d -m 700 "$BACKUP_ROOT"
exec 9>/run/lock/obelisk-relay-backup.lock
flock -n 9 || { echo "A relay backup is already running." >&2; exit 1; }

timestamp=$(date -u +%Y%m%dT%H%M%SZ)
stage=$(mktemp -d "$BACKUP_ROOT/.staging.XXXXXX")
final="$BACKUP_ROOT/$timestamp"
paused=

cleanup() {
  status=$?
  if [[ -n "$paused" ]]; then
    docker unpause "$paused" >/dev/null 2>&1 || true
  fi
  if (( status != 0 )); then
    echo "Backup failed; incomplete staging directory retained at $stage" >&2
  fi
}
trap cleanup EXIT

snapshot() {
  local name=$1 container=$2 db=$3 config=$4
  docker inspect "$container" >/dev/null
  [[ -f "$db/data.mdb" ]] || { echo "Missing LMDB at $db/data.mdb" >&2; exit 1; }
  install -d -m 700 "$stage/$name/db" "$stage/$name/config"

  paused=$container
  docker pause "$container" >/dev/null
  cp -a "$db/data.mdb" "$stage/$name/db/data.mdb"
  cp -a "$config/." "$stage/$name/config/"
  docker unpause "$container" >/dev/null
  paused=
}

snapshot public "$PUBLIC_CONTAINER" "$PUBLIC_DB" "$PUBLIC_CONFIG"
snapshot lacrypta "$LACRYPTA_CONTAINER" "$LACRYPTA_DB" "$LACRYPTA_CONFIG"

image=$(docker inspect -f '{{.Config.Image}}' "$PUBLIC_CONTAINER")
for relay in public lacrypta; do
  docker run --rm \
    --entrypoint /app/nostr-lmdb-integrity \
    -v "$stage/$relay/db:/backup-db" \
    "$image" --db-path /backup-db \
    >"$stage/$relay/integrity.txt"
  grep -Fq "No corrupted entries found." "$stage/$relay/integrity.txt" || {
    cat "$stage/$relay/integrity.txt" >&2
    echo "Integrity check failed for $relay backup." >&2
    exit 1
  }
done

{
  echo "created_at_utc=$timestamp"
  echo "public_container=$PUBLIC_CONTAINER"
  echo "lacrypta_container=$LACRYPTA_CONTAINER"
  echo "validator_image=$image"
  echo "schedule=Monday and Thursday at 03:30 server time"
} >"$stage/manifest.txt"

(
  cd "$stage"
  find public lacrypta -type f -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS
)

mv "$stage" "$final"
stage=$final
find "$BACKUP_ROOT" -mindepth 1 -maxdepth 1 -type d -name '20??????T??????Z' -mtime +210 -exec rm -rf -- {} +
echo "$final"
