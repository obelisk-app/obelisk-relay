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

This relay does **not** currently ingest kind 1984 reports into a moderation queue; they are stored
as ordinary events. NIP-56 advises against automatic moderation from reports because they are
easily gamed. Email is the working channel.
