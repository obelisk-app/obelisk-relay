# Deploying a relay

The short version: start the container, open the web UI, sign in with your Nostr
key. There is no configuration file to hand-edit before first boot.

```bash
docker run -d --name my-relay \
  -p 127.0.0.1:8080:8080 \
  -v my-relay-db:/app/db \
  -v /srv/my-relay/config:/app/config \
  ghcr.io/obelisk-app/obelisk-relay:latest
```

Then open `http://127.0.0.1:8080/admin` (or your reverse-proxied hostname) and
follow the setup wizard.

## What happens on first start

1. **The relay generates its own identity.** With no `relay_secret_key`
   configured, it mints a fresh keypair and writes it to
   `config/settings.local.yml`. The pubkey is logged once:

   ```
   Generated a new relay identity (no relay_secret_key configured): pubkey <hex>.
   ```

   The key is written to disk, so it is stable across restarts — a relay whose
   pubkey changed on every boot would invalidate its own group state each time.
   Keep `config/` on a persistent volume.

   > Earlier versions shipped a `relay_secret_key` inside the committed
   > `config/settings.yml`. Its private half was public, so any deployment that
   > never overrode it was impersonatable. That key is now recognised and
   > replaced automatically on start. If your relay was running on it, it gets a
   > new identity on the next restart — see
   > [export-import.md](export-import.md) for what changing the relay identity
   > means for existing groups.

2. **The relay starts in setup mode.** With no admin pubkeys configured,
   `GET /api/admin/setup/status` reports `needs_setup: true` and `/admin` shows
   the setup wizard instead of a login screen.

## The setup wizard

Three steps, all in the browser:

1. **Owner** — sign in with the Nostr key that should administer the relay.
   Browser extension (NIP-07), remote signer (NIP-46 / bunker), or a pasted
   key. Whichever key signs here becomes the relay's first admin; you do not
   type an npub, you prove ownership by signing.

2. **Access** — choose whether the relay is open to everyone or restricted to a
   whitelist. Restricted relays start with just the owner allowed; add more from
   the Access screen afterwards.

3. **Launch** — the relay writes the configuration and restarts itself. You land
   in the admin console.

After setup you can add further admins by npub or hex from the Settings screen.

## After setup

Two values usually need attention for a real deployment:

- **`relay_url`** — must match the public address clients reach you on
  (`wss://relay.example.com`). It is used for NIP-42 authentication, so a stale
  value breaks auth. Set it from Settings → Relay identity.
- **Relay name, description and icon** — advertised in the NIP-11 document and
  used to brand the admin console and browser tab. Useful when you run more than
  one instance.

Nothing deletes data by default. Automatic retention pruning is off unless you
explicitly arm it on the Storage screen, which also shows what the relay is
currently storing, by kind.

## Reverse proxy

The relay serves HTTP, WebSocket and the frontend on one port. Bind it to
loopback and terminate TLS in front of it:

```
relay.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

WebSocket upgrades need no special handling in Caddy; in nginx, pass through
`Upgrade` and `Connection` headers.

## Moving or cloning a relay

Use `scripts/relay-data.sh` — see [export-import.md](export-import.md),
especially the section on what the relay identity controls, before importing
into a new instance.
