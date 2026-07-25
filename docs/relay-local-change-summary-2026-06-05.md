# Relay Local Change Summary - 2026-06-05

This document records the uncommitted relay worktree changes that were present on top of `main` before rollback. At the time of review the repository was on `main` at `665491c` and had no commits ahead of `main`; everything below was local, uncommitted state.

## Indexed Search / NIP-50 Toggle

Planned code changes added a relay setting for LMDB indexed search support:

- `relay.enable_indexed_search`: default `true`; when `false`, NIP-50 `search` filters are rejected.
- `relay.advertise_indexed_search`: optional; when `false`, search can remain enabled but NIP-50 is omitted from advertised `supported_nips`.
- NIP-11 relay info and `/api/relay-info` were changed to use one runtime `supported_nips` list instead of hard-coded values.
- A new `SearchCapabilityMiddleware` rejected `REQ` and `COUNT` messages containing `search` when indexed search was disabled.
- Defaults/examples were added to `config/settings.yml`, `public-config/settings.yml`, and `public-config/settings.local.yml.example`.
- README supported NIPs were updated to mention configurable NIP-50.

Validation performed before rollback: `cargo check` passed, with only pre-existing warnings about an unexpected `nip59` cfg and deprecated `TimeoutLayer::new`.

This was not deployed or restarted on the test relay.

## Multi-Relay / Branding Guardrails

Planned local ops changes tried to prevent Obelisk and La Crypta relay assets/config from being mixed again:

- `.gitignore` ignored `config/branding/` so server-local mounted favicons would not be committed.
- `Caddyfile` was rewritten for Obelisk-only routes: `relay.obelisk.ar` and `public.obelisk.ar`.
- `compose.instance.yml` was added as a generic per-instance Docker Compose template with env-driven config, DB, branding, host port, and container name.
- `deploy/instances/*.env` inventory files described `relay`, `public`, `lacrypta`, plus an example template.
- `scripts/relay-admin-local.mjs` was expanded to load instance inventory and add `instances`, `branding`, and `doctor` checks.
- `scripts/README.md` documented the local admin helper and multi-relay Docker layout.

Validation performed before rollback:

- `node --check scripts/relay-admin-local.mjs` passed.
- `./scripts/relay-admin-local.mjs doctor all` reported ok.
- `docker compose --env-file deploy/instances/example.env -p example-relay -f compose.instance.yml config` rendered successfully.
- Existing relay containers were healthy when checked.

## Local Runtime / Server-Specific Drift

These local changes were also present but should not be blindly committed:

- `config/settings.local.yml` changed relay secret/config/whitelist/rate-limit contents.
- `config/whitelist_follows.json` had a very large runtime-derived whitelist diff.
- `config/whitelist_runtime.json` had a small runtime whitelist diff.
- Runtime/untracked files existed under `public-config/*.json` and old `config/settings.local.yml.bak.*` backups.
- `compose.yml` had local Docker changes including a mounted favicon and Caddy service.

## Live/Host Notes

The earlier logo confusion was traced to a La Crypta favicon mounted into the Obelisk relay. The host favicon under `config/branding/favicon.ico` was corrected to the Obelisk favicon, and the running container HTML was temporarily cache-busted with `/favicon.ico?v=obelisk-20260605`. That container HTML edit was ephemeral and would disappear on container recreation.

## Rollback Decision

The requested decision after this summary was: keep this document only, then roll back/forget the uncommitted relay worktree changes listed above.
