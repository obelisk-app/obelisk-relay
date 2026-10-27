# Relay invitation codes

Admission to the **relay itself** with a code. The relay records who used each code and who handed it out.

Status: shipped in obelisk-relay `v2026.10.27-relay-invites`. Clients:
obelisk-dex and obelisk-sky (in progress). Written so it can be proposed as-is to
[nostr-wot-sdk](https://github.com/nostr-wot) as a shared client capability. See
[§7](#7-proposal-for-nostr-wot-sdk).

---

## 1. Why this exists

`public.obelisk.ar` admits people by web-of-trust distance from its reference
accounts. That works for anyone already in the follow graph, but a newcomer
with a freshly generated key is on no list and in no graph. The relay refuses
every REQ with `auth-required:` and every EVENT with `restricted:`, so the
client shows an empty app with no way forward. `obelisk-dex/docs/operations/flow-review-2026-10-26.md` records this.

NIP-29 group invites (kind 9009 / 9021 with a `code`) don't fix it. They open a
door into one group, but only for someone the relay already admits. The group
logic is never reached.

Relay invitation codes are the missing front door:

- An **admin** creates a code in the console, or **someone the admin granted
  minting rights** creates one from inside Obelisk or Sky.
- The newcomer opens a link. The app makes a key if they have none and
  **redeems the code for that key**. The user never sees the code, the relay or
  the word "whitelist".
- The app **keeps the code**. If the user later generates a new key (new
  account, lost nsec), the app redeems the same code for the new key without
  asking.
- The relay **records every redemption**: which code, which key, when, and
  whose code it was. The console shows the referral tree.

---

## 2. Model

All of this lives in `config/relay_invites.json` (`src/relay_invites.rs`), not
in the event store. Like `reports_state.json`, it is the operator's
bookkeeping, and no client can publish, delete or query it over the websocket.
The file is written atomically (temp file, then rename) and is included in
console config backups.

### Invite code

| Field | Meaning |
|---|---|
| `code` | 20 random base62 characters (~119 bits). Never shorter than `MIN_INVITE_CODE_LEN` (16); see known-issues §15. |
| `created_by` | Who minted it. |
| `created_by_admin` | Whether the creator was a relay admin at the time. Admin codes are bound by no grant. |
| `label` | Free text up to 80 characters, visible to the operator and the minter only. |
| `max_uses` | `null` = unlimited (admins only). Default 100 for admins. |
| `expires_at` | Unix seconds; `null` = never (admins only). Default 30 days for admins. |
| `revoked_at` | Set when revoked. The record is kept. |
| `redemptions[]` | `{pubkey, at, evicted_at?}`, one per distinct pubkey. |

### Usage rule

**Each distinct pubkey that redeems consumes one use.** The same pubkey
redeeming again consumes nothing. A pubkey that already holds a live
redemption of *any* code also consumes nothing: the claim is acknowledged and
no use is spent.

So for key rotation, a user who makes a new key and has it re-admitted by the
stored code **does spend a use**. This was a deliberate product decision: usage
counts are counts of keys let in, which is what the operator is limiting. Old
keys keep their access; nothing is revoked automatically.

### Minter grant

An admin grants a non-admin pubkey the right to mint, with four limits:

| Limit | Default | Effect |
|---|---|---|
| `max_codes` | 5 | Codes that may be *live* (not revoked, expired or exhausted) at once. |
| `max_uses_per_code` | 10 | Ceiling on each code's `max_uses`. A request for more, or for 0/unlimited, is clamped. |
| `max_total_admits` | 25 | People this minter may bring in across all their codes, ever. When it is spent, **every code they minted stops admitting** (`invite inactive`). |
| `max_ttl_secs` | 30 days | Ceiling on each code's lifetime. |

Withdrawing a grant stops all of that minter's codes. People they already
invited keep their access.

### Admission

Redeemed pubkeys form a new admission tier, `AccessTier::Invite`
(`src/whitelist.rs`):

- It is checked after the blacklist, the manual list and follow sync, and
  before web-of-trust. The blacklist still overrides it: a banned key can't
  redeem, and redeeming doesn't unban.
- It publishes at the **Tier 1 budget** (budget key `direct`, 100%). Someone
  with minting rights vouched for the person directly, which is the same
  evidence as a direct follow.
- It does **not** make an open relay closed. `Whitelist::is_empty` ignores the
  invite set, so the first redemption on an open relay doesn't lock everyone
  else out.
- It is seeded from the store at startup, before the first connection.

---

## 3. Redeeming a code (websocket, NIP-43)

Redemption uses [NIP-43](https://github.com/nostr-protocol/nips/blob/master/43.md)'s
join request, kind **28934**, with a `claim` tag:

```json
{
  "kind": 28934,
  "pubkey": "<the key being admitted>",
  "created_at": <now>,
  "tags": [["-"], ["claim", "<code>"]],
  "content": "",
  "sig": "..."
}
```

Rules (`GroupsRelayProcessor::handle_relay_claim`):

1. **Exempt from admission.** The whole point is that the author is not
   admitted yet. This is the second exemption, after NIP-46 signer traffic
   (known-issues §1), and it is kept narrow: nothing is stored or broadcast
   (28934 is ephemeral), and the only effect is on the invite store.
2. **The blacklist is checked first.** A banned key gets `restricted: This
   pubkey is blocked from this relay`, and no use is spent.
3. **The signature proves key ownership.** NIP-42 is not required. If the
   client adds NIP-70's `-` tag, as NIP-43 shows, `Nip70Middleware` asks it to
   AUTH first, and that works too. RelayHub's `auth-when-challenged` handles it.
4. **Freshness.** `created_at` must be within 10 minutes of the relay's clock
   (`CLAIM_FRESHNESS_SECS`).
5. **Persisted before acknowledged.** The relay writes the store to disk before
   it replies `OK true`.

Replies:

| Reply | Meaning | What the client should do |
|---|---|---|
| `["OK", id, true, ""]` | Admitted, or already admitted (no use spent). | Mark this pubkey as redeemed. Re-run the access preflight and re-issue subscriptions. |
| `["OK", id, false, "restricted: invite unknown: …"]` | No such code on this relay. | Forget the code for this relay. |
| `["OK", id, false, "restricted: invite revoked: …"]` | Revoked. | Forget the code. |
| `["OK", id, false, "restricted: invite expired: …"]` | Expired. | Forget the code. |
| `["OK", id, false, "restricted: invite exhausted: …"]` | No uses left. | Forget the code; tell the user to ask for a new one. |
| `["OK", id, false, "restricted: invite inactive: …"]` | The minter's grant was withdrawn or spent. | Same as exhausted. |
| `["OK", id, false, "restricted: invite stale: …"]` | Clock skew over 10 minutes. | Retry with a fresh `created_at`. |
| `["OK", id, false, "restricted: invite missing: …"]` | No `claim` tag. | Client bug. |
| `["OK", id, false, "auth-required: …"]` | Sent with `-` before AUTH. | AUTH, then resend. |

Every invite refusal starts with `restricted: invite `, so a client can tell it
apart from an ordinary whitelist refusal with one prefix check. The word after
`invite` is a stable machine-readable reason.

After `OK true`, the pubkey is admitted immediately. Subscriptions that were
already `CLOSED` with `auth-required:` must be re-sent; the relay doesn't
re-open them.

Not implemented from NIP-43: the relay-signed kind 8000 / 8001 member events and
the kind 13534 membership list. Membership here is the admission ladder, and
publishing invitee lists would make the referral tree public.

---

## 4. Minting and the "Invite people" panel (HTTP, NIP-98)

Every client request is authenticated with a
[NIP-98](https://github.com/nostr-protocol/nips/blob/master/98.md) header:
`Authorization: Nostr <base64(kind 27235 event)>`. The `u` tag is the full URL
(`https://<relay host>/api/invites/me`) and the `method` tag is the HTTP method.
Events older than 60 seconds are refused. The limit is 30 requests per minute
per pubkey. Blocked keys get `403`.

CORS is open (`Any`), so the apps can call the relay directly from the browser.
No server of theirs needs a secret.

### `GET /api/invites/me`

Answers **any** signer, including one that isn't admitted. The apps call this
to decide whether to show "Invite people".

```json
{
  "pubkey": "<hex>",
  "admitted": true,
  "invited_by": { "code": "…", "pubkey": "<hex>", "npub": "npub1…", "at": 1793000000 },
  "can_mint": true,
  "is_admin": false,
  "codes_left": 3,
  "admits_left": 18,
  "admits_used": 7,
  "grant": { "max_codes": 5, "max_uses_per_code": 10, "max_total_admits": 25, "max_ttl_secs": 2592000, "…": "…" },
  "codes": [ /* InviteView, newest first, see below */ ]
}
```

Show the panel when `can_mint` is true. It's false when there's no grant, or
when the grant's code or admit quota is spent. Admins always get
`can_mint: true` and `null` quotas.

### `POST /api/invites`

Body (all optional): `{ "label": "…", "max_uses": 10, "expires_in": 604800 }`.
Minters are clamped to their grant, and `0` doesn't mean unlimited for them.
Returns `201` with an `InviteView`. Returns `403` with a message when no grant
exists or a quota is spent.

### `GET /api/invites`

Lists the caller's own codes (`InviteView[]`).

### `DELETE /api/invites/{code}`

Revokes one of the caller's own codes (admins can revoke any). Returns `204`.
People already admitted keep their access; only an admin can evict.

### `InviteView`

```json
{
  "code": "Xk3…", "label": "meetup", "status": "active",
  "created_by": "<hex>", "created_by_npub": "npub1…", "created_by_admin": false,
  "created_at": 1793000000, "max_uses": 10, "uses": 3, "expires_at": 1793600000,
  "revoked_at": null,
  "redemptions": [ { "pubkey": "<hex>", "npub": "npub1…", "at": 1793000100 } ]
}
```

`status` is one of `active`, `revoked`, `expired`, `exhausted`.

---

## 5. Admin console and API

The **Access → Invites** tab (`frontend/src/components/admin/access/InvitesPanel.tsx`)
has three views:

- **Codes.** Create a code (label, max uses, expiry). Copy it, or copy an Obelisk
  or Sky link. Each code shows its usage. Revoke it, optionally also removing
  everyone who came in on it.
- **Minters.** Grant or update minting rights by npub, hex or NIP-05, set the
  four limits, or withdraw a grant.
- **Referrals.** Who came from whom. One card per inviter shows who invited
  *them* (the next link up the chain), how many people they brought in, and
  every invitee with the code and date. You can remove a single invitee from
  here.

Invitees also appear in **Tier 1**, labelled "redeemed an invite". "Is this
account allowed?" answers "Redeemed invite code X, shared by npub…".

Admin-session routes, under `/api/admin/relay-invites`, behind
`require_admin_session` plus a per-handler check (known-issues §14):

| Method | Path | |
|---|---|---|
| GET | `/` | `{invites, minters, active_codes, total_admitted}` |
| POST | `/` | Mint as admin. `max_uses: 0` = unlimited, `expires_in: 0` = never. |
| DELETE | `/codes/{code}?evict=bool` | Revoke; with `evict=true`, remove its invitees' invite admission. |
| GET | `/minters` | Grants with live usage. |
| POST | `/minters` | `{pubkey, max_codes?, max_uses_per_code?, max_total_admits?, max_ttl_days?}`. Upserts. |
| DELETE | `/minters/{hex}` | Withdraw a grant. |
| GET | `/referrals` | The referral tree. |
| DELETE | `/invitees/{hex}` | Remove one invitee's invite admission. |

Eviction only removes the *invite* tier. If the person is also followed by a
reference account, they stay admitted. To make removal final, block them.

---

## 6. Client integration guide

This is the behaviour obelisk-dex and obelisk-sky implement. Any Nostr client
can do the same.

### Link format

```
https://obelisk.ar/invite/<code>?relay=<relay host>
https://sky.obelisk.ar/invite/<code>?relay=<relay host>
```

`relay` is the relay host (e.g. `public.obelisk.ar`). A code is meaningless on
any other relay.

### Flow

1. **Capture.** On `/invite/<code>`, store
   `{code, relay, redeemed: []}` in localStorage. Use a key outside the session
   record (`obelisk-dex/relay-invite`, `obelisk-sky/invite`) so logging out
   doesn't erase it. Then strip the code from the URL.
2. **Relay trust.** Switch to the relay automatically only if it's one the app
   already knows. For an unknown relay, keep the app's existing confirmation
   prompt. An invite link must not become a way to silently point someone at an
   attacker's relay.
3. **Key.** If there's no session, generate a key silently and log in with it.
   Offer "back up your key" later, in settings.
4. **Redeem on every pubkey change.** Hook the session's "pubkey changed"
   signal. It fires for generate, import, NIP-07 and bunker logins, and for
   restore. If an invite is stored for the active relay and this pubkey isn't
   in `redeemed`, sign and send the kind 28934 claim.
5. **On `OK true`:** add the pubkey to `redeemed`, re-run the access preflight,
   and re-send subscriptions that were closed.
6. **On `restricted: invite <reason>`:** if the reason is
   `unknown`/`revoked`/`expired`/`exhausted`/`inactive`, drop the stored invite
   and show a plain message ("This invite link has expired — ask for a new
   one"). If it's `stale`, retry once.
7. **Invite people.** For the logged-in key, call `GET /api/invites/me`. When
   `can_mint` is true, show an **Invite people** entry in settings: create a
   link, copy or share it, and list your codes with uses and who joined.

---

## 7. Proposal for nostr-wot-sdk

Filed as [nostr-wot/nostr-wot-sdk#26](https://github.com/nostr-wot/nostr-wot-sdk/issues/26).

Steps 4–6 above are the same in every client and depend only on the relay hub
and the signer, which nostr-wot-sdk already owns. Proposed additions:

- **`@nostr-wot/relay`:** a `redeemRelayInvite(relayUrl, code, signer)` helper
  that builds and signs the 28934 claim, sends it through the hub with the
  AUTH-on-challenge policy, and resolves to
  `{ok: true} | {ok: false, reason: 'unknown'|'revoked'|'expired'|'exhausted'|'inactive'|'stale'|'other', message}`
  by parsing the `restricted: invite <reason>` prefix.
- **`@nostr-wot/ui` session:** an optional `pendingRelayInvite` that the session
  provider replays on every login or identity change. This covers the
  rotation case without each client re-implementing the hook.
- **`@nostr-wot/ui` login:** a silent "generate and continue" mode for
  invite-driven onboarding, deferring the backup step to settings.
- **NIP-98 helper:** `nip98Fetch(url, init, signer)` for the `/api/invites/*`
  calls.

The relay side is standard NIP-43 plus NIP-98, so none of this is specific to
Obelisk.

---

## 8. Operational notes

- **Backups and restores.** `relay_invites.json` is in the console's config
  backups. A restore takes effect on restart, like the other runtime files.
- **Brute force.** Codes are ~119 bits, so guessing isn't a practical attack.
  Failed claims don't spend anything and aren't stored.
- **Clock skew.** A relay host with a wrong clock refuses every claim as
  `stale`. Keep NTP running.
- **Theme.** Changing the console frontend changes the bundle hash, so
  deploying this release needs `scripts/retint-branding.sh` (known-issues §11).
