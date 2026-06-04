# Build, Publish & Deploy

Small relay hosts should pull a prebuilt GHCR image instead of compiling the
relay locally.

The current release/deploy procedure lives in:

```text
docs/release-deploy-ghcr.md
```

Current image package:

```text
ghcr.io/obelisk-app/obelisk-relay
```

Deploy a pinned release tag:

```bash
export RELAY_IMAGE_TAG=v2026.06.04-admin-docs
docker compose pull groups_relay
docker compose up -d groups_relay
curl -fsS "http://127.0.0.1:${RELAY_HOST_PORT:-8080}/health"
```

See `docs/release-deploy-ghcr.md` for the GHCR login, multi-arch Buildx
publish, arm64 repair flow, verification, rollback, and private-file rules.
