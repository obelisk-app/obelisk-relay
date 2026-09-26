# Stale claims

Places where this repo, or the relay it deploys, currently says something that
is no longer true. Not runtime traps — those are in `known-issues.md` — but
documentation and metadata that has fallen behind the code, where the cost is a
reader acting on it or a screenshot quoting it.

Found 2026-09-22 while re-cutting the explainer video in `obelisk-media`, which
is why the list is weighted toward things visible on camera. Nothing here has
been fixed; each item says what it should say instead.

---

## 1. The relay advertises itself as open while running allowlist + trust

**Where:** the NIP-11 document served at `/`, field `description`.

```console
$ curl -s -H 'Accept: application/nostr+json' https://public.obelisk.ar/ | jq -r .description
Public NIP-29 groups relay for Obelisk. Open to all pubkeys; per-pubkey rate limited.
```

The admin console's Overview reports the opposite: an `Allowlist + trust` badge
and a four-figure allowed-pubkey count. One of the two is lying to whoever reads
it, and the NIP-11 document is the one that clients and directories consume.

This is the most consequential item on the list, because it is machine-readable.
A client deciding whether to bother authenticating reads this string.

Worth checking whether the description is hard-coded or derived from config; if
it is derived, it is reading the wrong field, which is a bug rather than a typo.

**Should say:** something matching the access mode actually in force — the
console already has the vocabulary: allowlist plus web-of-trust, blocked
overrides.

Still open. Until it is fixed, the README deliberately shows no screenshot of the
landing page, which prints this string; the sign-in shot is cropped to the card
for the same reason.

## 2. ~~The README pins an image tag three releases behind what is deployed~~ — fixed 2026-09-26

**Where:** `README.md:138`.

```
> RELAY_IMAGE_TAG=v2026.09.19-reports-4-arm64 ./setup.sh
```

`compose.yml:39` deploys `v2026.09.22-conncount2-arm64`. Between the two sit the
access-polish, client-redirect and docs-and-live-trends builds — which is to say
anyone following the README gets a relay with no tier UI, the bundled client
still attached, and the connection counter still reporting zero.

The surrounding callout is right that `:latest` is worse (multi-arch, behind the
dated tags). The pin is the correct mechanism; the number is just old.

**Should say:** the current dated tag, or be generated from `compose.yml` so it
cannot drift again.

**Fixed 2026-09-26** (README refresh): the callout now pins
`v2026.09.22-console-rework-arm64`, the tag `public_relay` deploys. It also said
`setup.sh` pulls `:latest`, which was a second stale claim: `setup.sh` pulls
`groups_relay` at the tag `compose.yml` pins for it
(`v2026.09.18-compaction-update-arm64`), and falls back to building from source
when that arm64-only pull fails on amd64. The callout now says so. Drift is
still possible (two hand-maintained tags in `compose.yml`), so generating the
README tag from `compose.yml` remains the real fix.

## 3. ~~The README still sells the bundled chat client~~ — fixed 2026-09-26

**Where:** `README.md:45`.

```
| ⚡ **Cashu wallet** | NIP-60/61 micropayments in the bundled chat UI. |
```

`6a1e8f0` ("Stop serving a second Nostr client, and fix what that exposed")
removed that UI. `frontend/src/main.tsx:36` now redirects `/app` to
`https://obelisk.ar/app?relay=…`. There is no bundled chat UI to hold a wallet.

**Should say:** either drop the row, or point it at the client that now owns the
feature and make clear the relay is not serving it.

**Fixed 2026-09-26:** the row is gone.

## 4. The reference-accounts card cannot be retinted

**Where:** `frontend/src/components/admin/ReferenceAccountsManager.tsx`, still
mounted at `access/AccessScreen.tsx:183`. Six inline literals rather than
`var(--color-accent)`:

| line | what it paints |
|---|---|
| 167 | the "auto-sync complete" status banner (bg, text, border) |
| 173–174 | the "syncing follows" banner and its spinner |
| 180 | the third status banner |
| 234 | the avatar ring on every reference-account row |
| 238 | the fallback avatar for accounts with no picture |

`scripts/retint-branding.sh` rewrites the built stylesheet to swap lime for
purple, but it operates on CSS. Literals that Vite inlines into the JS bundle
are out of its reach.

Confirmed on the live public deployment in a 2026-09-22 capture: the
reference-account avatars carry a visibly olive ring inside an otherwise purple
console. The status banners are worse — they only appear after "Sync follows",
so a full-width lime panel flashes into a purple screen exactly when an operator
is watching.

Cosmetic, but Access → Tier 1 is the tab the screen opens to, which makes these
the most-photographed pixels in the console.

**Should say:** `var(--color-accent)` and `rgba(var(--color-accent-rgb), …)`,
like the rest of the admin components.

---

## Not stale, worth knowing

- `Cargo.toml` is pinned at `0.1.0` and is not the release channel; the image tag
  in `compose.yml` is. There is no CHANGELOG, and `dist/releases/` stopped at
  `v2026.09.14`. Nothing depends on these being current, but anyone looking for
  "what version is this" will find three wrong answers before the right one.
- `/app` is a **client-side** redirect, not a 302. `curl` gets 200 and an SPA
  shell; only a browser follows it. Any health check or scraper that asserts on
  the redirect will assert on the wrong thing.
