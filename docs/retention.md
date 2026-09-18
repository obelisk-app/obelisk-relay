# Retention

Automatic deletion is **off** unless an operator explicitly arms it. Nothing in
a default deployment ever deletes an event.

Policies are per event kind, set from **Storage → Automatic deletion** in the
admin console, or in `settings.local.yml`:

```yaml
relay:
  enable_event_pruner: true
  prune_interval: "1h"
  prune_retention_by_kind:
    1059: "30d"     # gift wraps (private messages)
    2390: "7d"      # obelisk-game moves
    9: "365d"       # group chat
```

Each kind gets its own window. Kinds sharing a window are deleted in one pass,
so the cost scales with the number of distinct durations, not the number of
kinds. The scan cadence defaults to the **shortest** window divided by 48
(clamped 60s–6h), so a long policy never slows enforcement of a short one.

Arming requires typing `DELETE` in the UI. Changes take effect on restart.

## What the relay refuses to delete

Two guards, both enforced in `src/pruner.rs` regardless of what a config asks
for. A refused kind is dropped with a warning at startup; if nothing prunable
remains, the pruner does not start at all.

**Group state.** Kinds 9000–9011 and 39000–39003 (`NEVER_PRUNE_KINDS`).
These reconstitute group identity, membership, roles and metadata. Deleting any
of them makes groups silently disappear or lose their admins.

**Anything replaceable or addressable.** By NIP-01 a relay stores only the
latest event per `(pubkey, kind)` — replaceable — or per
`(pubkey, kind, d-tag)` — addressable. **These cannot accumulate**: there is
exactly one per user per slot however long the relay runs. A time-based policy
against them frees essentially nothing while destroying live state, and the
user it hits is by definition the inactive one who will not republish. Covers
profiles (kind 0), contact lists (3), relay lists (10002), DM relay lists
(10050), and application data (30000–39999) — including a client's read state.

The same guard applies when an admin wipes a user's events: group management
kinds survive, so removing a person cannot orphan the groups they created.

## The kinds you will actually see here

| Kind | What it is | Consequence of pruning |
|---|---|---|
| **1059** | NIP-59 gift wrap — the envelope of a private message. Signed by a throwaway key, so the author is not the sender and the relay cannot read the content. | **Deletes people's DMs.** The relay holds no other copy and clients fetch DM history from it. Prune only if the relay is not meant to retain private messages. |
| **2390** | `obelisk-game` move events. High volume, short-lived. | Game history is lost; play is unaffected. A short window is reasonable. |
| **9 / 11 / 12** | Group chat message, thread root, thread reply. | Conversation history disappears for everyone who has not cached it. |
| **7** | Reactions. | Reaction counts drop. Cheap to prune. |
| **5** | Deletion requests. | Pruning these can resurrect content for clients that replay history. Prefer keeping. |
| **1984** | NIP-56 reports. | Your moderation record disappears. Keep unless storage forces otherwise. |
| **9735 / 9321** | Zap receipts, nutzaps. | Payment records are lost. These are often the proof a payment happened. |
| **30078** | NIP-78 application data — relay branding, channel layout, client read state. | **Never pruned** (addressable). One per user per `d` tag. |
| **9000–9011, 39000–39003** | NIP-29 group management and state. | **Never pruned.** Groups would vanish or lose admins. |

Kind labels in the admin console come from the same table; anything unlisted
shows as `Kind N`.

## Before arming a policy

The storage screen samples the 20,000 newest events for its breakdown, because
exact per-kind counts are slow — measured at **434 seconds** for kind 1059 on a
4.2 GB database. NIP-59 randomises `created_at`, so a newest-first sample
over-represents gift wraps badly; **do not size a policy from the sampled
share.**

Use **count exactly** on the row first. It reports the true total and how many
events already fall outside the window — the number that decides whether
arming is safe. It runs in the background and can take minutes.

Deletions are not reversible. Take a backup first
(`scripts/backup-relays.sh`), and prefer testing a policy against a copy of the
database rather than the live one.

## Getting the disk back

Pruning does not shrink `data.mdb`. LMDB puts freed pages on a free list and
reuses them for later writes; it never returns them to the filesystem, so the
file sits at its historical high-water mark however much you delete. A flat
event count beside a flat-but-large file is not a broken policy — it is the
policy working, with the space still held.

**Reclaim disk space** on the Storage screen is what hands it back. It reports
three numbers:

| | |
|---|---|
| **File on disk** | What the filesystem sees. |
| **Actually in use** | Pages holding data the relay still serves. A compaction leaves the file at roughly this size. |
| **Reclaimable** | The free list. This is what you get back. |

Measured on `public.obelisk.ar` after the first retention pass: a 605 MB file
holding 238 MB of live data, compacted in 3.4 seconds to 238 MB, with all
378,481 events intact.

### What it does

The relay restarts and compacts before it opens the database. It has to: the
copy is taken from a read-transaction snapshot, so any write landing afterwards
would be lost when the copy replaced the original — and the one moment nothing
holds the database open is startup.

1. The console writes `config/compaction-request.json` and the relay exits.
2. Docker restarts it (`restart: unless-stopped`).
3. Before opening the database, the relay consumes the request, copies the live
   pages into `data.mdb.compacting`, moves the original to
   `data.mdb.pre-compact`, swaps the new file in, and re-opens it to prove it
   works. Only then is the original deleted.
4. The outcome is appended to `config/compaction-log.json`, which the Storage
   screen shows.

If anything fails, the original is restored and the relay starts on it. The
request is deleted *before* the work starts, so a compaction that kills the
process cannot retry itself into a boot loop.

### When it refuses

- **Not enough free disk.** It needs 1.2× the live data, because the copy exists
  alongside the original until it is proven. The check runs again immediately
  before the restart is scheduled.
- **Nothing to reclaim.** A free list of zero.
- **`data.mdb.pre-compact` already exists.** An earlier attempt did not finish.
  Work out by hand which file is current before removing it — this one is not
  safe to guess at.

### Verifying against a copy first

Nothing here needs to be taken on trust:

```bash
cp -a /var/lib/docker/volumes/nostr-relay_public-relay-db/_data /tmp/dbtest
docker run --rm -v /tmp/dbtest:/data <relay-image> /app/lmdb_stat --db /data
OBELISK_COMPACT_TEST_DB=/tmp/dbtest \
  cargo test --lib compacts_a_real_database -- --ignored --nocapture
docker run --rm -v /tmp/dbtest:/data <relay-image> /app/nostr-lmdb-integrity --db-path /data
```

A compaction copies live pages verbatim: it neither introduces nor repairs index
corruption. If `nostr-lmdb-integrity` reported stale `deleted-ids` entries before,
it will report the same ones after. Clearing those still needs the
`scripts/relay-data.sh export` → `import` round trip.
