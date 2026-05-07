# Obelisk Relay

NIP-29 Nostr Groups Relay for the Obelisk ecosystem. Whitelisted, role-based, with a built-in admin UI.

Production: `wss://relay.obelisk.ar`
Forked from [verse-pbc/groups_relay](https://github.com/verse-pbc/groups_relay).

<p>
  <a href="https://github.com/obelisk-app/obelisk-relay/stargazers"><img src="https://img.shields.io/github/stars/obelisk-app/obelisk-relay?style=flat&logo=github&color=b4f953&labelColor=0a0a0a" alt="GitHub stars" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/obelisk-app/obelisk-relay?style=flat&color=b4f953&labelColor=0a0a0a" alt="License" /></a>
</p>

## What it does

A NIP-29 relay manages **group chats at the relay level** — unlike a vanilla Nostr relay that only stores and forwards events, this one:

- 🔐 **Pubkey whitelist** — only approved npubs can connect (NIP-42 authenticated)
- 👥 **Role-based permissions** — admin / mod / member, enforced server-side
- 🔒 **Private groups** — content visible only to members
- 🎫 **Invite codes** — time-limited, usage-capped
- 🌐 **Built-in web UI** — Preact frontend at the relay URL
- ⚡ **Cashu wallet** — NIP-60/61 micropayments

## The Obelisk family

| Repo | What |
|------|------|
| [obelisk-app/obelisk](https://github.com/obelisk-app/obelisk) | The chat app (relay-only) |
| [**obelisk-app/obelisk-relay**](https://github.com/obelisk-app/obelisk-relay) | This repo — the NIP-29 relay |
| [obelisk-app/obelisk-sfu](https://github.com/obelisk-app/obelisk-sfu) | mediasoup SFU for voice |
| [obelisk-app/obelisk-bots](https://github.com/obelisk-app/obelisk-bots) | Nostr bots toolkit |
| [obelisk-app/obelisk-classic](https://github.com/obelisk-app/obelisk-classic) | The original centralized stack |

## Quick start

```bash
git clone https://github.com/obelisk-app/obelisk-relay.git
cd obelisk-relay
./setup.sh
```

The wizard:
1. Verifies Docker is installed
2. Asks for your relay domain
3. Asks for your admin npub
4. Lets you add whitelisted pubkeys
5. Generates config and starts the relay

## Management

```bash
./start.sh status    # is it running?
./start.sh logs      # view relay logs
./start.sh restart   # restart after config changes
./start.sh stop      # stop the relay
```

## Configuration

Edit `config/settings.local.yml`:

```yaml
relay:
  relay_url: "wss://relay.yourdomain.com"
  whitelisted_pubkeys:
    - "hex_pubkey_here"
```

Restart after changes: `./start.sh restart`.

## Supported NIPs

NIP-29 (relay-based groups) · NIP-09 (deletion) · NIP-40 (expiration) · NIP-42 (auth) · NIP-70 (protected events)

## Architecture

```
Internet → Caddy (:443) → relay container (:8080)
                            ├── Axum HTTP server
                            │   ├── WebSocket → Nostr protocol
                            │   ├── /health, /metrics
                            │   └── / (Preact frontend)
                            ├── GroupsRelayProcessor (NIP-29 logic)
                            ├── ValidationMiddleware (event validation)
                            └── nostr-lmdb (LMDB database)
```

See [CLAUDE.md](CLAUDE.md) for the full architecture, event-processing flow, and supported event kinds.

## Stack

Rust (Tokio + Axum) · `relay_builder` + `websocket_builder` (verse-pbc) · `nostr-sdk` · `nostr-lmdb` · Preact + TypeScript frontend · Docker

## Roadmap

See [ROADMAP.md](ROADMAP.md) — the next milestone is a Nostr-authenticated admin panel for content moderation.

## License

[AGPL](LICENSE)
