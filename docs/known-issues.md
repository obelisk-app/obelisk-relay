# Known issues and traps

Things that have bitten this relay, what the symptom looked like, and what to
check first. Ordered by how badly they can ruin a day.

Written 2026-09-17 while adding Web-of-Trust admission, storage attribution and
targeted pruning. Items marked **fixed** have a guard in place; items marked
**open** do not.

---

## 1. Turning on access control can lock you out of the admin console

**Severity: high. Fixed, but the class of bug is permanent.**

The admin console signs in over NIP-46, and the first relay in its rendezvous
list is *this relay*:

```ts
// frontend/src/constants.ts
export const NIP46_RELAYS = [ownRelayUrl(), 'wss://relay.damus.io', ...]
```

So the login handshake has to publish and subscribe to kind 24133 **on the relay
you are logging into**, before anyone is authenticated. The moment any access
restriction is enabled — a whitelist, or Web-of-Trust admission — the bunker's
ephemeral client key stops being able to do that, the handshake never completes,
and `nostr-tools` reports:

```
this signer is not open anymore, create a new one
```

which says nothing about the real cause. The console is then unreachable, and
the setting that would lift the restriction is behind it.

Measured on the live relay before the fix:

```
subscribe(24133): REFUSED: auth-required: this relay only accepts whitelisted pubkeys
publish(24133):   REFUSED: restricted: your pubkey is not whitelisted on this relay
```

**The fix.** Kind 24133 is exempt from the whitelist and from per-pubkey rate
limiting — see `KIND_NIP46_SIGNER` in `src/groups_event_processor.rs`. It is safe
to exempt: the kind is ephemeral so nothing is stored, and payloads are NIP-44
encrypted between two keys that have already paired. A filter that merely
*includes* 24133 alongside other kinds does not inherit the exemption, so it
cannot be used to smuggle access; `nip46_signer_traffic_bypasses_a_closed_whitelist`
pins both halves.

**The permanent rule.** Any new gate placed in front of `handle_event` or
`verify_filters` must exempt signer traffic, or it re-creates this trap. The
console cannot be the only way to undo a lockout that the console itself causes.

---

## 2. Compose in this directory does not deploy this relay

**Severity: high. Fixed by pinning, but worth understanding.**

The live containers run under compose project `nostr-relay`, while the directory
is `obelisk-relay` — and compose derives the project name from the directory. A
bare `docker compose up -d public_relay` therefore attaches
`obelisk-relay_public-relay-db`, an **empty leftover volume from May**, instead
of `nostr-relay_public-relay-db`, which holds the real 550 MB database.

The relay comes up `healthy`, serves zero groups, and looks fine.

This was caught only because port 8081 was already bound. `.env` now pins
`COMPOSE_PROJECT_NAME=nostr-relay`; do not remove that line.

---

## 3. A local build silently takes over the published release tag

**Severity: medium. Open — inherent to the compose file.**

`compose.yml` declares both `image:` and `build:`, so `docker compose build`
tags the result with the `image:` value. Building locally overwrites
`ghcr.io/obelisk-app/obelisk-relay:v2026.09.15-observability` with an unpublished
image, and the original then has no tag and is unreachable by `docker tag` —
recovering it needs a `docker pull`.

Build local images under a distinct tag (`RELAY_IMAGE_TAG=local-…` in `.env`),
so the published tag keeps meaning what it says and rollback stays a one-line
change.

Related: that tag is **mutable**. The image behind
`v2026.09.15-observability` was re-pushed after the running container started,
so "roll back to the tag" is not the same as "roll back to what was running".

---

## 4. A failing `docker compose build` can report success

**Severity: medium. Open.**

A background build wrapper reported `exit code 0` while the build had failed with
a Docker Hub 502 pulling `node:24-slim`. The wrapper shell's status was what got
reported, not the build's.

It was caught only because the retint step produced an unchanged CSS hash, which
cannot happen if the frontend actually rebuilt. Always echo the build's own exit
code and check it; treat an unchanged asset hash after a source change as a
failed build until proven otherwise.

---

## 5. Deleting events never shrinks the database file

**Severity: medium. Not a bug — LMDB semantics. Documented in the UI.**

LMDB reuses freed pages internally and never returns them to the filesystem, so
`data.mdb` only ever grows. Pruning hard and watching the disk figure stay flat
is the expected outcome, not a broken prune.

Only a compaction shrinks it: an export/import rebuild took this relay from
5.2 GB to 330 MB, which deleting alone could never have done. See
`docs/export-import.md`.

The Storage screen now says this next to the disk chart, and the tile is labelled
**File on disk** rather than implying it tracks live data.

---

## 6. The Web-of-Trust graph is incomplete at its outermost hop

**Severity: medium. Instrumented — read the warning.**

To answer "is X within N hops" the graph needs the contact list of every account
at hops `0..N-1`. Those are fetched from public relays, under a budget
(`DEFAULT_MAX_REMOTE_FETCHES`, currently 25,000). When the budget runs out, the
outermost hop is *sampled rather than known*, and a refusal there means "no path
was fetched", not "no path exists" — so people who should be admitted are not.

This is why `max_hops: 3` initially admitted only 62,432 accounts on a 2,000
budget: hop 3 was roughly 3% covered. At 25,000 the same relay admits 145,244
with no truncation.

`FollowGraph::coverage()` records per-hop coverage and a `truncated` flag; a
truncated rebuild logs

```
Follow graph is complete only to N hop(s): the M contact-list budget ran out …
```

and the Access screen says the same in plain language. **If you see it, lower
`max_hops` to the stated depth or add reference accounts** — widening the root
set costs far less than another hop.

The graph can be incomplete but never wrong: a missing contact list means "no
path", never a false one, and no path means not admitted. Incompleteness fails
safe.

---

## 7. Enabling Web-of-Trust closes an open relay

**Severity: medium by design. Intended — but surprising.**

`Whitelist::is_empty()` returns **false** when the WoT tier is on, even with an
empty manual list, because the caller treats `true` as "admit everyone". So
enabling WoT on a relay with no whitelist converts it from open to restricted:
anonymous reads start returning `auth-required`.

That is the safe reading — an empty list plus WoT must not mean "admit everyone"
— but it changes what the relay *is*. On `public.obelisk.ar` it took the relay
from open to 143,871 admitted accounts. The wizard and the Access screen both
say so now; the rule itself is pinned by
`enabling_wot_is_itself_a_restriction`.

---

## 8. The follow graph rebuilds twice on startup

**Severity: low. Open.**

`tokio::time::interval` fires its first tick immediately, so the rebuild loop in
`src/server.rs` does one redundant pass before settling into its hourly cadence.
At the current budget that is ~60 s of pointless contact-list fetching on every
restart. Use `interval_at` with an offset start.

---

## 9. The startup log names an oracle in local mode

**Severity: low. Open.**

```
WoT admission enabled: 1 root(s), max 3 hop(s), oracle http://wot-oracle:8080
```

is printed even when `relay.wot.local` is true and no oracle is contacted.
Harmless, but misleading if you are reading logs during an incident.

---

## 10. Malformed admin POSTs return 422 before the auth check

**Severity: low. Open — affects every admin route.**

Axum runs the `Json` extractor before the handler body, so an unauthenticated
request with a malformed body gets `422 Unprocessable Entity` rather than `401`.
No work is performed and nothing leaks, but the status is wrong and the auth
check is effectively second. Applies to every admin route that takes a body, not
just the recent ones.

---

## 11. The purple theme override is matched by content hash

**Severity: low. Scripted, was silently breaking.**

`compose.yml` bind-mounts a tinted stylesheet over the baked one, matched by
Vite's content-hashed filename. Any frontend change produces a new hash,
`index.html` points at the new file, and the override becomes a file nothing
loads — the site silently reverts to the default green with no error anywhere.

This happened at least twice (`index-DUXQZBvw.css` and `index-PDsRJO3H.css` were
both dead overrides). Never hand-edit or rename the tinted file. After any
frontend change:

```bash
docker compose build public_relay
scripts/retint-branding.sh ghcr.io/obelisk-app/obelisk-relay:<tag>
# then point both sides of the compose mount at the hash it prints
```

The script reads `index.html` out of the built image to learn which stylesheet is
actually loaded, so the hash it writes is correct by construction.

The real fix would be to stop pinning a hashed filename — serve the accent from
relay config and let the frontend apply it at runtime, which is now possible
because every accent colour goes through `--color-accent` / `--color-accent-rgb`
rather than being hardcoded in inline styles.
