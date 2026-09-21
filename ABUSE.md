# Reporting abuse

This repository is the source for a NIP-29 Nostr groups relay. Anyone can run it, and many people
do — **running this software does not make its author responsible for your relay.**

If you are reporting content on a specific relay, you need that relay's operator. Every Nostr relay
publishes a contact in its NIP-11 document:

```sh
curl -H "Accept: application/nostr+json" https://<relay-host>
```

## Relays operated by Obelisk

- `wss://public.obelisk.ar`
- `wss://lacrypta-relay.obelisk.ar`

For content on **these**, contact **abuse@obelisk.ar**. See
[the client repository's abuse policy](https://github.com/obelisk-app/obelisk/blob/main/ABUSE.md)
for what can be acted on, what cannot, and response times.

For any other relay running this software, I am not the operator and cannot act.

## What an operator of this software can do

If you run this relay, you can delete a single event, delete every event by a public key across all
scopes, delete a group, remove a group member, and use a blacklist that overrides the whitelist.
See `src/admin.rs` and `src/blacklist.rs`.

## Reports (NIP-56, kind 1984)

This relay **does** ingest kind 1984 reports into a moderation queue, visible to relay admins under
**Reports** in the admin console. Filing one from a client is now a working channel; email remains a
parallel one, and is still the right route if you cannot reach the relay or the report concerns the
operator.

What happens to a report:

- It is grouped with every other report about the same message or account, so ten people reporting
  one thing is one case rather than ten.
- The reported message is copied when the report arrives, so it can still be reviewed if the author
  deletes it afterwards.
- Distinct reporters are counted, not raw volume. One key reporting a thousand times does not
  outrank ten unrelated keys reporting once.
- **Nothing happens automatically.** NIP-56 advises against automatic moderation because reports are
  trivially gamed — a spammer can mint keys and mass-report a target. Only an operator action
  deletes, removes or blocks anything.

Reports are readable by relay admins only. On a relay where admission depends on a social graph, a
publicly readable report queue would tell the reported party exactly who reported them.
