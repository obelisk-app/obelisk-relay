# Remaining work

Updated 2026-09-15. Everything under "Open" is not done; everything under
"Landed" is committed and pushed to `main`.

---

## Known issues

Traps that have already bitten this relay — what the symptom looks like and what
to check first — are in [`docs/known-issues.md`](docs/known-issues.md). The one
worth reading before changing access control:

> **Turning on access control can lock you out of the admin console.** The
> console signs in over NIP-46 through *this* relay, so any new gate in front of
> `handle_event` / `verify_filters` must exempt kind 24133 or the setting that
> would lift the restriction ends up behind it.

Still open there: the double graph rebuild on startup, an oracle URL logged in
local mode, `422`-before-`401` on malformed admin POSTs, and the hash-matched
theme override.

---

## Open

### A. Free disk on the deployment host — **blocks the build**

`/` is at **97%, 2.7 GB free**. A Rust release build needs ~3 GB of cache alone,
so a local build fails partway through with `ENOSPC`, and the multi-arch CI route
was cancelled at 42 minutes. Nothing else here can ship until this is done.

```
rm -rf /root/relay-backups/obelisk-relays/20260914T170030Z   # 4.3 GB
docker image prune -a -f                                      # up to 2.8 GB
```

The backup is superseded by `20260915T014037Z`, which is 8.5 h newer and holds a
complete 4.58 GB `data.mdb` for both relays plus integrity reports — verified.
The image prune removes `v2026.09.14-admin-ux`, `v2026.07.25-main-339d70a` and
`alpine:latest`; `v2026.09.14-retention` is what is running and is left alone.

### B. Build and deploy `v2026.09.15-multiarch`

Tag exists at `ee2cf25` and is pushed; **no image was ever published** under it.
Two CI attempts: the first failed on the `CARGO_BUILD_JOBS` bug (now fixed), the
second was cancelled at 42 min while compiling.

This host is **aarch64**, and on x86 GitHub runners arm64 is the QEMU-emulated
leg — so CI pays emulation cost on precisely the architecture we deploy. Once
disk is free, prefer a **native arm64 build here** and let CI do amd64 natively,
then join them:

```
docker buildx imagetools create -t <tag> <tag>-arm64 <tag>-amd64
```

Then bump the pin at `compose.yml:9` and `:35` and recreate. Rollback is
`v2026.09.14-retention`, still on the host.

> Verify the run's `conclusion`, never the exit code. `gh run watch --exit-status
> | tail` returns *tail's* status — that is how a failed build read as success
> twice tonight.

### C. The 7 d retention window is written but inert

`public-config/settings.local.yml` holds `{1059: "7d"}` (set from the Storage
screen) with `prune_interval: "360m"`. Policies are read **only at startup**, so
it takes effect on the next restart and will delete substantially more than the
58,032 the 30 d pass took. Watch `docker stats` through the first pass — it is
one unbounded LMDB write transaction on a box with no swap headroom.

### D. Operator controls still missing

**Delete by kind and age, in one action.** Preview with `admin_exact_kind_count`
(already returns total *and* older-than-N), then a bounded delete over the same
filter the pruner uses, behind the typed-`DELETE` gate. Must use `list_scopes()`,
not `self.groups` — `admin_delete_user_events` derives scopes from the latter and
silently misses any scope holding no loaded group.

**Space used per kind.** The screen shows counts, not bytes. The sampling pass
already walks 20k events, so accumulate size per kind and report average × exact
count, labelled an estimate. Count tags, not just `content.len()` — a kind-9000
event is nearly all tags and would otherwise read as zero.

**Publish the deployed tag.** `v2026.09.18-compaction-update` was built on this
host and both relays run it, but it is **not in GHCR**. The console offers
published tags only, so until it is pushed the update card cannot offer the
version that is actually running, and an update from the UI would move a relay
*backwards* to the newest published tag. Push it:

```
docker push ghcr.io/obelisk-app/obelisk-relay:v2026.09.18-compaction-update
```

The update agents are installed for both relays (`obelisk-relay-update-public`
and `-lacrypta`); see [`docs/updating.md`](docs/updating.md).

### E. README and Reddit posts

Login screenshots are in `screenshots/`. The authenticated console shots need a
NIP-46 approval and are only worth taking after B and C.

### F. Housekeeping

`obelisk-relay-backup.timer` is `disabled`/`inactive`. Arm it only after A, and
shorten its 210-day retention first or it refills the disk. Each backup is 4.3 GB.

---

## Landed

### Group discovery no longer scans the database — `73391b7`

The channel list hung because its query was the slowest one the storage layer can
run. nostr-lmdb's six indexes are all keyed on an **author or a tag**, so a filter
naming only `kinds` matches none and falls through to `query_by_scraping`. Cost is
scan depth, not match count — the rarer the kind, the slower the query.

Measured on production, 5.2 GB, authenticated over NIP-42:

```
{"kinds":[39000,39001,39002]}                  62 events   29.3s
{"kinds":[39000,39001,39002],"authors":[...]}  62 events    0.45s
{"kinds":[31337]}   (no such events exist)      0 events   49.9s
```

The last line proves it is a scan, not a volume problem. The relay now supplies
the author set, because nothing else can: a discovering client has no `d`/`h` tag
yet, and the 2026-08-11 key rotation left **four** pubkeys holding live group
state — the current key covers only 2 of 20 kind-39000 events, so scoping to it
alone would hide 18 groups. The set is scanned once at startup, detached; until it
lands filters pass through unrewritten, because a slow channel list is recoverable
and a silently incomplete one is not. See `src/group_state_filter.rs`.

Chat was never affected — clients send kind 9 with an `h` tag, already served by
`(kind, tag, created_at)` in 0.45 s.

### Storage settings could write unstartable YAML — `a672f37`

`upsert_relay_value` replaced a key's first line only, stranding the children of a
block-style setting. The result does not parse, and since config is read only at
startup the console reported success while the **next restart** was what broke.
Reachable by following `docs/retention.md` and then saving from the Storage
screen; it happened on public.obelisk.ar on 2026-09-15 and was caught by hand.

### Build and installer

- `ad9b94f` — `ARG CARGO_BUILD_JOBS` with no default expands to `ENV
  CARGO_BUILD_JOBS=`, and cargo aborts parsing `""` at exit 101. **CI could not
  build this image at all since `5cd6b11`**; local releases were unaffected only
  because they always passed `--build-arg CARGO_BUILD_JOBS=1`, which is why it
  looked like a QEMU problem.
- `ee2cf25` — cap cargo at 3 jobs in CI so the emulated arm64 leg has headroom.
- `bc05557` — `setup.sh` measured the wrong filesystem. The alpine probe could
  never work (`/var/lib/docker` does not exist in the container) and the pipeline
  after it returned awk's status, so the function "succeeded" with an empty string
  and never reached its fallback — every macOS user got "Could not determine free
  disk space". `NR==2` also broke on `df`'s device-name wrap, reporting 0 GB on
  healthy hosts.
- `d94f625` — `setup.sh` no longer warns "first build needs ~3GB" on the pull
  path, which only downloads a 259 MB image, and no longer defaults to **n**.

### Retention, armed 2026-09-15

Pruner enabled with `{1059: "30d"}`; first pass deleted **58,032** gift wraps in
~38 s with no OOM. NIP-11 now advertises `retention`. `data.mdb` stays at 5.20 GB
— LMDB never returns freed pages; the win is a smaller working set.

Reclaiming the file is now a button on the Storage screen rather than an
export/import round trip — see "Compaction" below. The export/import path is
still the only thing that clears the stale `deleted-ids` entries both relays
report, since a compaction copies live pages verbatim.

### Compaction from the console

**Reclaim disk space** on the Storage screen stages a request, restarts, and
compacts before the database is opened — the only moment nothing holds it open.
Refuses unless 1.2× the live data is free, keeps the original until the new file
is proven openable, and consumes the request before starting so a failure cannot
boot-loop.

Verified against a copy of production: 605 MB → 238 MB in 3.4 s, all 378,481
events intact, file mode preserved at 0600, and `nostr-lmdb-integrity` reporting
exactly the same single pre-existing `deleted-ids` entry before and after.

The measurement could not use heed's `non_free_pages_size`: it decodes every key
in the unnamed database as a UTF-8 database name, and nostr-lmdb stores the
default scope's events there, so it panics on the first event id. `src/compaction.rs`
walks the free list through LMDB directly instead.

Measured after the prune: kind 9 improved 23.5 s → 16.0 s and kind 39000 went from
a 60 s timeout to returning — but both stayed slow, which is what pointed at the
scan rather than at database size.
