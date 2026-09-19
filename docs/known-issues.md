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

---

## 12. A relay secret key is in this repo's public git history

**Severity: medium. Stale — documented rather than rewritten.**

`config/settings.local.yml` was tracked in early commits (`7a235dd`, `5ce75e8`)
with a real `relay_secret_key` in it. The file is gitignored now, but this
repository is public, so the key is permanently readable by anyone who clones it:

```bash
# Prefix only, deliberately. `-S` is a substring search, so this still finds
# the commits -- and a document that exists to say "never reuse this key"
# should not be the most convenient place in the repo to copy it from.
git log --all -S"3ab7d45a8843e459"
```

**The live relay does not use it.** `public-config/settings.local.yml` holds a
different key, rotated 2026-08-11. So nothing is currently exposed, and the
deliberate decision was to document rather than rewrite history — a rewrite
would invalidate every clone and fork to protect a key that is already public
and already unused.

What matters if this ever comes up again: the relay key is a superuser in this
codebase. `can_edit_members`, `can_delete_event` and `can_see_event` all
short-circuit for `relay_pubkey`, and `is_relay_admin` keys off it. **Never
reuse that key, and never restore an old config that contains it.** Rotation is
the mitigation, not obscurity; the console can do it under Settings → rotate key.

Related: `public-config/settings.yml:6` still ships the upstream *example* key
and is tracked. That one is mitigated in code — `ensure_relay_identity`
(`src/config.rs`) recognises the constant and mints a fresh key at startup — so
a fresh deploy self-heals. The mitigation depends on that constant staying in
step with the file; change one and you must change the other.

---

## 13. NIP-46 pairing secrets sit in the repo root

**Severity: medium if leaked, currently untracked.**

`bunker-uri.txt`, `bunker-qr.png` and `bunker-status.txt` are live NIP-46 pairing
artefacts. A `nostrconnect://` URI carries the secret that authenticates a signer
to a browser session, and the QR renders that secret in a form anyone can scan
off a screenshot — these are credentials, not screenshots.

They are correctly gitignored and `git ls-files` confirms they are untracked. The
risk is everything *other* than git: a `docker build` with a loose context, a
screen share, a backup of the directory, a support screenshot. Delete them once
pairing is done rather than leaving them lying in the working tree.

---

## 14. Admin route auth is per-handler, not a router layer

**Severity: low today, structural.**

Every `/api/admin/*` route authenticates by calling `validate_session` as the
first statement of its own handler — around 45 hand-written copies. All of them
are currently present and correct; the only handlers without one
(`/challenge`, `/auth`, `/setup*`, `/relay-info`) are intentionally public.

The problem is that correctness here is maintained by memory. The 46th route
added is the one that forgets, and nothing in the type system or the tests would
catch it. The fix is `axum::middleware::from_fn` on the `/api/admin` nest with
the public routes split into a sibling router, which also resolves issue 10
(`422` returned before the auth check, because Axum runs the `Json` extractor
first). Deliberately not done in the same pass as the security fixes above:
touching 45 call sites is a large diff with a real chance of dropping a check
while removing them, and it wants to be its own reviewable change.

---

## 15. Invites now expire, are use-limited, and need a long code

**Severity: behaviour change. Deliberate — read this before debugging an invite.**

`Invite` previously had no expiry field, no use counter, and no constraint on the
code. A reusable invite was therefore a permanent, unlimited credential, and the
code was whatever the client sent — a four-character one was accepted and was
guessable at the per-pubkey rate limit, across rotating pubkeys.

Three rules now apply at `create_invite`:

| Rule | Default | Override |
|---|---|---|
| Minimum code length | 16 characters | none — it is a floor |
| Expiry | 30 days from `created_at` | `expiration` tag, unix seconds |
| Max redemptions (reusable only) | 100 | `max_uses` tag |

**What this can break.** The shipping web client generates 24 hex characters, so
it is unaffected. Any *other* client in the ecosystem that generates a shorter
code will start getting `Invite code is too short to be secret`. If that happens,
fix the client rather than lowering `MIN_INVITE_CODE_LEN` — a code short enough
to be convenient is short enough to be guessed.

Invites already stored keep working: the three new fields are `#[serde(default)]`,
so state written before this deserializes with no expiry and no cap, i.e. exactly
the old behaviour. They are grandfathered, not retroactively expired. A test
(`an_invite_without_limits_keeps_the_old_behaviour`) pins that.

---

## 16. `force_public_groups` is editable from the console, behind a phrase

**Severity: informational. The gate is the point.**

The Connection limits card now exposes `force_public_groups`, which was
previously file-only. It is the one setting there that destroys information:
`Groups::load_groups` sweeps every stored group at startup and clears
`private` and `hidden` (`src/groups.rs:283-290`), including groups other people
created. Nothing restores them — turning the flag back off leaves every group
that was coerced still public.

So switching it **on** requires typing `FORCE PUBLIC`, server-side as well as in
the UI; the API rejects the change without it. Switching it **off** needs no
confirmation, because that direction destroys nothing.

Two things worth knowing if you use it:

- The coercion happens **at startup**, not at save time. Between saving and
  restarting, groups are still private, and the card says so.
- It is not a privacy *setting* so much as a migration. If you only want new
  groups to be public, this is the wrong control — it rewrites the existing ones
  too.

---

## 17. Moderation reports (NIP-56 kind 1984)

**Severity: informational. New capability — read before changing its visibility.**

Kind 1984 is now accepted, queued and actionable from the admin console's
Reports tab. Before this it could not be filed at all: a report carries no `h`
tag, so `ValidationMiddleware` refused it outright, and a client that invented
one got its report stored as ordinary group content.

Three things are worth knowing.

**Reports are admin-only to read, which diverges from NIP-56.** The spec treats
reports as public. On a relay where admission is a social graph, public reports
mean the reported person can look up who reported them — a retaliation channel.
`verify_filters` therefore refuses any subscription naming kind 1984 unless the
requester is a relay admin, and refuses it *explicitly* rather than returning an
empty result, so a client learns it may not have them rather than concluding
there are none. Mixing 1984 into a wider filter does not launder it past the
check. If you ever want spec-conformant public reports, that is the one function
to change — and it is a deliberate decision, not an oversight.

**Reports group by target, not by reporter.** Ten people reporting one message is
one case, and the queue counts distinct reporters rather than report volume, so
four reports from one account read as one reporter. Cases are sorted by recency,
not by count: a volume-ordered queue would let a brigade set the agenda.

**Reports have their own rate limit**, separate from and far tighter than the
general publishing budget (20/hour, burst 40, admins exempt). The resource being
protected is the moderator's attention, not the database — a 1984 is a tiny
event. Because cases group by target, flooding the queue requires reporting many
*different* things, which is what this bounds.

Resolutions live in `config/reports_state.json`, alongside the blacklist and for
the same reason: whether a report was acted on is the operator's decision about
someone else's claim, and must not be something the reporter or the reported can
publish, replace or delete. Deleting that file reopens every case; it does not
undo any action already taken.

---

## 18. The blacklist did not block anyone on an open relay

**Severity: high. Fixed — found by end-to-end testing, not by review.**

Blacklisting an account said "done", wrote the entry to `blacklist.json`, showed
it in the console, and did not block them. Two independent faults, both in
`is_allowed` (`src/groups_event_processor.rs`):

1. **The blacklist was consulted after the open-relay short-circuit.** With no
   manual whitelist and no Web-of-Trust tier, `Whitelist::is_empty()` returns
   true and admission returned early — before the blacklist was read.
   `Whitelist::contains` had always honoured the blacklist; it simply never got
   asked.
2. **Admission keyed on the authenticated pubkey.** `context.authed_pubkey` is
   `None` until a client completes NIP-42, and on an open relay nothing forces
   one. A banned account could just not authenticate. The ban applied to a
   session identity the spammer had no reason to establish.

Both are fixed: the blacklist is checked before the short-circuit, and an
event's author is now checked against it directly. The signature is the
identity — knowing who signed an event does not require them to announce
themselves first.

**public.obelisk.ar was not affected**, because Web-of-Trust is enabled there, so
`is_empty()` returned false and the first path was never taken. That was luck,
not design: any deployment running open — which the config file and the NIP-11
description both describe this relay as — had an inert blacklist.

Worth stating plainly for next time: **unit tests passed throughout.** The first
version of the regression test authenticated the spammer, which no real client on
an open relay does, so it exercised a path the bug did not live on. What found it
was running the relay and trying to ban someone. Pinned now by
`a_blacklisted_key_is_refused_even_on_an_open_relay`, which publishes
unauthenticated.

---

## 19. `admin_delete_event` silently deleted nothing

**Severity: medium. Fixed.**

It built its scope list purely from managed groups, so on a relay with no managed
groups the loop had nothing to iterate — and it returned `Ok(())` regardless. The
caller, including the reports queue, was told the event had been deleted while it
was still readable. Events in unmanaged groups and every non-group kind live in
`Scope::Default`, which was never in the list. Fixed by always including it.

---

## 20. Reported content is snapshotted, because evidence must outlive the message

**Severity: design decision. Worth understanding before changing retention.**

The reports queue keeps its own copy of every reported message, captured when
the report arrives, in `config/reports_evidence.json`.

Without it the queue is unworkable, because the evidence does not outlive the
thing being moderated. Three ordinary events destroy it:

- the author deletes their own message — which makes deletion a *defence*, not a
  remedy: report me, I delete, your complaint becomes unreviewable;
- an admin deletes it while working a different case;
- the retention pruner reaches it. This relay has done exactly that: pruning was
  armed on 2026-09-15 to rescue a 5.2 GB database, so reports filed before then
  now point at messages nobody can read. That is the state that prompted this —
  a queue entry saying "spam" with nothing to judge.

The snapshot also records the **verified author**, read off the stored event
rather than off the reporter's `p` tag. That is what keeps a case actionable
after the original is gone: without it, blocking has to be refused, because the
only remaining name is the accuser's claim (see §17).

Properties worth knowing:

- **First capture wins.** A later report about the same message cannot rewrite
  what the relay saw first, or the evidence would be editable by whoever reports
  last.
- **Content is capped at 2000 characters.** It is retained indefinitely, and a
  moderator needs enough to judge, not the whole payload.
- **Capture failures are not fatal.** A report is still accepted if the relay
  cannot find what it refers to — the reported event may live on another relay.
  The queue then says so plainly rather than showing an empty quote.
- **The file grows with reports, never shrinks.** It is small (one short text per
  reported message) but it is not covered by the pruner, deliberately: pruning
  the evidence would reintroduce the problem it exists to solve.
