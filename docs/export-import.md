# Exporting and importing relay data

`scripts/relay-data.sh` moves a relay's events and configuration into a portable
bundle, and restores that bundle onto a fresh instance.

```bash
# Export a running relay
scripts/relay-data.sh export /root/relay-bundles --container lacrypta-relay-relay-1

# Restore onto a fresh instance
scripts/relay-data.sh import /root/relay-bundles/obelisk-relay-export-<stamp> \
  --db /srv/newrelay/db --config /srv/newrelay/config --new-identity
```

Events are carried as JSONL by the `export_import` binary already shipped inside
the relay image; the script adds the configuration, an integrity report, a
manifest and checksums so the bundle alone is enough to stand up an equivalent
relay.

## Keys: what a bundle contains and what that means

This is the part to get right. **Two different keypairs** are involved, and they
behave differently across an export/import.

### 1. The relay's own identity — `relay_secret_key`

The relay has its own Nostr keypair. It lives in `settings.local.yml` as
`relay_secret_key` (32 bytes, hex) and the matching public key is what the relay
advertises as `pubkey` in its NIP-11 document.

The relay signs with it. Under NIP-29 the relay is the authority for group
state: kinds **39000** (metadata), **39001** (admins), **39002** (members) and
**39003** (roles) are all signed by the relay key, and clients verify group
state against the pubkey the relay advertises.

**A bundle contains this private key.** `config/settings.local.yml` is copied
verbatim. That has two consequences:

- **A bundle is a secret.** Anyone holding it can impersonate the relay: sign
  group-state events as it, and be accepted as that relay by any client that
  trusts the pubkey. Store bundles like private keys — not in git, not in shared
  storage, not in a ticket attachment. The script writes bundles `chmod go-rwx`
  and prints a reminder, but it cannot stop you copying one somewhere careless.

- **You must decide whether the new instance *is* the old relay.**

| | `relay_secret_key` | Result |
|---|---|---|
| **default (reuse)** | carried over unchanged | The new instance **is** the same relay. Same pubkey, so the imported 39000–39003 events still verify and groups keep their identity, admins and members. This is what you want for a host migration. |
| `--new-identity` | regenerated | A **different** relay that happens to hold the same events. Its pubkey no longer matches the signer of the imported group-state events. |

#### What `--new-identity` actually costs

The imported kind 39000–39003 events were signed by the *old* relay key. A new
instance with a new key did not sign them and cannot re-sign them — it has no
access to the old private key.

In practice that means group state may not re-establish cleanly: clients
verifying group metadata, admin and member lists against the new relay's
advertised pubkey will see events signed by a pubkey that is not this relay.
Group content events (kind 9, 11, 12 and so on) are signed by their *authors*,
not by the relay, so those are unaffected and import fine either way.

So:

- **Migrating a relay to new hardware, keeping the same groups** → reuse the
  identity (the default). Do not pass `--new-identity`.
- **Seeding a genuinely new relay from another's data**, or handing a bundle to
  someone else → use `--new-identity`, and expect to re-create groups rather
  than inherit them.
- **The old relay's key may have leaked** → rotate. The admin console has a
  key-rotation action (`/api/admin/relay-identity/rotate-key`) which is the
  supported path, and it carries the same group-state consequence.

Never run two relays on the same `relay_secret_key` at the same time. They will
both sign group state as the same identity, and clients will see two
authorities disagreeing about the same groups.

### 2. Operator and user keys — `admin_pubkeys`, `whitelisted_pubkeys`

These are **public** keys, and only public keys. They say who may administer the
relay and who may connect; the relay never holds the corresponding private keys,
which stay with their owners in their signer apps.

They carry over in the bundle unchanged, and that is usually correct — the same
people should administer the migrated relay. Review them after import anyway:

- `admin_pubkeys` — who can reach the admin console. If ownership is changing
  with the migration, change this.
- `whitelisted_pubkeys` — who may connect at all. Empty means open.
- `relay_url` — almost always needs updating, since the new instance is on a
  different host. It is used for NIP-42 auth validation, so a stale value breaks
  authentication.

The runtime state files (`admin_pubkeys_runtime.json`, whitelist, blacklist,
reference accounts) are copied too, so console-made changes survive the move.

## Consistency and the live relay

Export pauses the container for the duration of a file copy (a second or so),
copies the LMDB, then unpauses and runs the export against the *copy*. The live
database is never opened by the export tool. `--no-pause` skips the pause and
risks a torn read; do not use it on a relay taking writes.

## Integrity

The export records `nostr-lmdb-integrity` output in the bundle as
`integrity.txt` but does **not** abort when it reports problems. Stale entries
in the `deleted-ids` index accumulate on long-running relays, and an
export/import cycle is the documented way to clear them — refusing to export a
database *because* it needs exporting would be backwards.

Both Obelisk production relays currently report such entries (13 on public, 1 on
lacrypta), and a round-trip through this script produced a database that passes
the integrity check cleanly. Pass `--strict` if you would rather abort.

> Note: `scripts/backup-relays.sh` *does* hard-fail on any integrity finding.
> With both relays currently reporting stale `deleted-ids` entries, that script
> exits non-zero for both. Worth fixing before relying on scheduled backups.

## Import safety

- Checksums in `SHA256SUMS` are verified before anything is written.
- Import refuses to write into a directory that already has `data.mdb`. Merging
  two relays' events silently is much harder to unpick than being told to start
  clean.
- Without `--yes` it prints what it is about to do — including whether the
  identity is being reused or replaced — and waits for confirmation.
- The imported database is integrity-checked afterwards.
- The relay must be started (or restarted) after an import for it to load.
