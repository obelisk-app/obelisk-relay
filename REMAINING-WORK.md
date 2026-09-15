# Remaining work

Snapshot of what is **not** done, as of 2026-09-15. Everything listed under
"Shipped" below is deployed and verified; everything under "Open" is not.

Full plan with reasoning: the session plan file this was extracted from.

---

## Open

### A. Arm retention and recover `public.obelisk.ar` — **blocks everything else**

The relay is degraded right now. Measured against production, authenticated over
NIP-42:

```
kind 39000 (group metadata, limit 100) → TIMEOUT 60s, 0 events
kind 9     (chat, limit 20)            → 20 events, first at 18.7s
```

The metadata query is what the channel list depends on, so the client sits at
*Loading channels…* forever. `data.mdb` is **4.84 GB** on a box with ~2 GB of free
page cache; LMDB is memory-mapped, so once the working set exceeds RAM every read is
a disk seek. The relay sits at ~2.4 GB RSS.

It is not chat traffic. The 20,000 newest events contain **3** kind-9 messages. The
bulk is **358,257 kind-1059 gift wraps** (real NIP-17 DMs, plus sender self-copies —
two events per message sent) and their index overhead.

Steps:

1. **Ship two committed-but-unreleased fixes first** — `41f3d99` and `9d6a5ca`. The
   typed-`DELETE` gate rendered at **1.17:1 contrast** (near-white on white) and
   matched case-sensitively, so typing `delete` silently left Save disabled. That is
   almost certainly why arming retention has not worked. Needs a new image.
2. Storage → **count exactly** on 1059 → set **30 days** → type `DELETE` → save →
   **restart**. Policies are read at startup only.
3. Expect ~63,566 events removed on the first pass. Re-measure both queries above.

If queries are still slow afterwards, **the cause is not database size** — profile the
addressable-event query path instead of deleting more.

> LMDB does not return freed pages to the filesystem. `data.mdb` stays 4.84 GB even
> after a large prune; the win is a smaller working set. To reclaim the file itself,
> round-trip `scripts/relay-data.sh export` → `import`, which rebuilds it compactly
> and also clears the stale `deleted-ids` entries both relays report.

### B. `setup.sh` turns people away — `setup.sh:269-273`

Defaults to **pulling** a 259 MB image, but the disk check is unconditional and prints
*"First build needs ~3GB"* on the pull path too, defaulting to **n**. A real installer
hit this and stopped. Three bugs in five lines:

1. The warning never consults `BUILD_FROM_SOURCE` (parsed at line 27). Gate it on the
   mode; ~1 GB for pull, ~3 GB for build.
2. `disk_avail_gb` truncates (`printf "%d", $4/1024/1024`), so 3.9 GB reports as `3`
   and fails `-gt 3`. Compare in MB; format for display separately.
3. `df -k .` measures the directory the script is in. On macOS with colima or Docker
   Desktop, images live inside a Linux VM with its own disk — the wrong filesystem
   entirely. Prefer `docker system df`, fall back to `df` only if Docker can't be
   queried.

Also default the pull-path prompt to **y**.

### C. The published image is arm64 only

Intel Macs and ordinary x86 servers cannot run it, so "anyone can install this" is
currently false. Decided: attempt the amd64 build locally on the 1-core builder.

Worth knowing before starting: both successful builds so far were **native arm64**.
This host is aarch64, so amd64 needs QEMU user-mode emulation, which is what produced
`x86_64-binfmt-P: QEMU internal SIGSEGV` partway through `cargo build` on the first
attempt. Fewer parallel jobs may help by lowering memory pressure; it does not make
emulation more stable.

```
docker buildx build --builder obelisk-1core --load \
  --build-arg CARGO_BUILD_JOBS=1 --platform linux/amd64 \
  -t ghcr.io/obelisk-app/obelisk-relay:<tag>-amd64 .
```

Run detached and grep the log for `SIGSEGV` — do **not** trust the exit code. A build
piped into `tail` returns `tail`'s status, which is how the first failure initially
looked like success.

Fallback: `.github/workflows/docker.yml` already builds `linux/amd64,linux/arm64` on
x86 runners where amd64 is native. One `workflow_dispatch`, no new code.

Either way publish a **multi-arch manifest** so `docker pull` resolves per platform.

### D. Operator controls still missing

**Delete by kind and age, in one action.** Today you can delete selected events, or all
of one user's, or wait for the pruner. There is no "delete kind 1059 older than 30
days, now". Add it per policy row on the Storage screen: preview with
`admin_exact_kind_count` (already returns total *and* older-than-N), then a bounded
delete over the same filter the pruner uses, behind the typed-`DELETE` gate.

**Space used per kind.** The screen shows counts, not bytes. Exact sizes mean reading
every event, which is not affordable — but the sampling pass already walks 20k events,
so accumulate `content.len()` per kind and report **average bytes × exact count**,
labelled as an estimate.

### E. README and Reddit posts

Login-flow screenshots are captured in `screenshots/` (see below). The authenticated
console shots still need a NIP-46 approval, and are only worth taking **after A** —
right now an honest screenshot shows a console that cannot load its own channel list.

---

## Shipped and deployed

Both relays run `v2026.09.14-retention`; both repos pushed.

- Per-kind retention policies (`prune_retention_by_kind`), replacing the single window
- Pruner refuses **protected** kinds (9000–9011, 39000–39003) *and* anything
  replaceable or addressable — one event per user, so deleting frees nothing and
  destroys live state
- `admin_delete_user_events` no longer deletes group-management events; wiping a
  group's creator used to orphan it
- Bulk moderation by author **and** by recipient (`p` tag) — gift wraps have throwaway
  authors, so recipient is the only per-user handle
- Storage policy table, on-demand exact counts (background + poll; 1059 measured at
  **434 s** on the 4.84 GB database), top-recipient panel
- NIP-11 `retention` advertisement, derived from the live pruner config so it cannot
  drift from what is enforced
- `docs/retention.md`, retention section in `docs/deploy-a-relay.md`
- obelisk-dex: groups read-state moved to a replaceable kind-30078 (one event per
  user, was unbounded); DM gift wraps now publish through `publishSignedEvent` —
  `publishEvent` re-signs, which silently broke read-state sync *and* stamped the
  user's identity onto wraps built to hide it

## Captured

`screenshots/` — login states from live production:

| File | What |
|---|---|
| `01-admin-login.png` | Method list |
| `02-login-extension.png` | NIP-07 with no extension present (inline error, list stays) |
| `03-login-bunker-qr.png` | NIP-46 pairing QR |
| `04-login-paste-key.png` | nsec / hex entry |

## Housekeeping

- `bunker-qr*.png` and `bunker-uri.txt` in the repo root are a **live NIP-46 pairing
  credential**. Delete them once the authenticated screenshots are taken; do not
  commit them.
- `obelisk-relay-backup.timer` is still inactive. The script works now (it used to
  abort on any integrity finding, and both relays report stale `deleted-ids` entries).
- Disk was at **96%** (3.5 GB free) at the time of writing. Each backup is 4.3 GB and
  the script only prunes at 210 days. A Rust build needs headroom.
