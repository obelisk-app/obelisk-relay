# Build & Deploy on Server (temporary)

GitHub Actions `docker.yml` keeps getting cancelled, so we build the image
directly on the server and let `compose.yml` pick it up via the local Docker
image cache.

## Prerequisites on the server

- Docker + `docker compose`
- ~8 GB RAM free during build (Rust release build is memory-hungry)
- Git access to `github.com/obelisk-app/obelisk-relay`

## Procedure

```bash
# 1. Get the latest code
cd /path/to/obelisk-relay   # or: git clone https://github.com/obelisk-app/obelisk-relay.git
git fetch origin
git checkout main
git reset --hard origin/main

# 2. Build the image with the tag compose.yml expects
docker build -t ghcr.io/obelisk-app/obelisk-relay:latest .

# 3. Bring the stack up
docker compose down
docker compose up -d

# 4. Verify
docker compose ps
docker compose logs -f groups_relay
curl -fsS http://localhost:8080/health
```

`compose.yml` references `ghcr.io/obelisk-app/obelisk-relay:${RELAY_IMAGE_TAG:-latest}`.
Because the local build tags the same name, Docker uses the local image and
does **not** try to pull from GHCR.

## Pinning a specific build

To avoid `latest` drift, tag with the commit SHA and pass it through:

```bash
SHA=$(git rev-parse --short HEAD)
docker build -t ghcr.io/obelisk-app/obelisk-relay:$SHA .
RELAY_IMAGE_TAG=$SHA docker compose up -d
```

## Troubleshooting

- **rustc ICE / build killed** → out of memory. Either give the host more RAM,
  or add `ENV CARGO_BUILD_JOBS=1` in the Dockerfile before
  `cargo build --release --bins` (slower, lower peak memory).
- **`input/output error` from Docker** → daemon storage wedged. Restart Docker,
  then `docker system prune -af && docker builder prune -af`.
- **Compose still pulls from GHCR** → it only pulls when the local image is
  missing. Confirm `docker images | grep obelisk-relay` shows the tag you built.

## Removing this doc

Delete once GitHub Actions `docker.yml` is publishing successfully again and
the server pulls from GHCR like normal.