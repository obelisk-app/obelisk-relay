# Build, Publish & Deploy

This repo is set up so heavy Rust/frontend builds happen on the workstation,
then small servers pull a prebuilt GHCR image. Small servers should not compile
the relay unless they are being used for development.

Current image package:

```bash
ghcr.io/obelisk-app/obelisk-relay
```

## Workstation Publish Flow

Run this on the build workstation from the repo checkout:

```bash
cd /home/pepe/obelisk-relay

# Optional but recommended: publish from the commit you expect.
git rev-parse --short=7 HEAD

SHA="$(git rev-parse --short=7 HEAD)"
IMAGE="ghcr.io/obelisk-app/obelisk-relay"

docker build \
  -t "${IMAGE}:sha-${SHA}" \
  -t "${IMAGE}:latest" \
  .

docker push "${IMAGE}:sha-${SHA}"
docker push "${IMAGE}:latest"
```

The last successful workstation publish was:

```bash
ghcr.io/obelisk-app/obelisk-relay:sha-5c154eb
ghcr.io/obelisk-app/obelisk-relay:latest
```

Both tags pointed at:

```text
sha256:2a8f3d8ddc50a420b7b64193dd3674b3e16fdbef6f4e078601631ea21b7f4926
```

## GHCR Login

GitHub account passwords do not work with GHCR. Docker needs a GitHub personal
access token as the password.

Token requirements:

- Classic PAT is fine.
- `write:packages`
- `read:packages`

Preferred one-shot login:

```bash
docker login ghcr.io -u Fabricio333
```

Paste the PAT when Docker asks for the password.

If using a temporary token file, keep it outside the repo, restrict the file
mode, and feed it through stdin:

```bash
mkdir -p /home/pepe/.config/obelisk-relay
chmod 700 /home/pepe/.config/obelisk-relay
chmod 600 /home/pepe/.config/obelisk-relay/ghcr-token
docker login ghcr.io -u Fabricio333 --password-stdin < /home/pepe/.config/obelisk-relay/ghcr-token
```

Remove the token file after pushing:

```bash
shred -u /home/pepe/.config/obelisk-relay/ghcr-token
```

Docker stores GHCR auth in:

```bash
/home/pepe/.docker/config.json
```

## Small Server Deploy

On a small server, pull instead of building:

```bash
cd /path/to/obelisk-relay

docker compose pull groups_relay
docker compose up -d groups_relay

docker compose ps
docker compose logs -f groups_relay
curl -fsS http://localhost:8080/health
```

For the public relay service:

```bash
docker compose pull public_relay
docker compose up -d public_relay
curl -fsS http://localhost:8081/health
```

## Pinning A Build

Use `latest` for fast normal installs:

```bash
docker pull ghcr.io/obelisk-app/obelisk-relay:latest
```

Use a commit tag when you want deterministic deploys:

```bash
RELAY_IMAGE_TAG=sha-5c154eb docker compose pull groups_relay
RELAY_IMAGE_TAG=sha-5c154eb docker compose up -d groups_relay
```

`compose.yml` reads:

```bash
ghcr.io/obelisk-app/obelisk-relay:${RELAY_IMAGE_TAG:-latest}
```

## Local Emergency Deploy

If GHCR is unavailable but the server has enough RAM, build locally:

```bash
docker build -t ghcr.io/obelisk-app/obelisk-relay:latest .
docker compose up -d
```

This is slower and should be the fallback, not the normal deployment path.

## Optional GitHub Release Assets

The helper below can build the Docker image locally, extract `/app` into a
tarball, push the image, and upload release assets to a GitHub Release:

```bash
scripts/release-local-to-github.sh v0.1.0
```

For the current deployment need, GHCR image tags are enough. Use the script only
when you also want a GitHub Release page with downloadable tarballs/checksums.

## Troubleshooting

- **`unauthenticated` during `docker push`**: run `docker login ghcr.io -u Fabricio333` with a PAT, not your GitHub password.
- **`rustc` killed or ICE during Docker build**: the workstation is out of memory. Close other heavy jobs or add `ENV CARGO_BUILD_JOBS=1` before the Rust build in `Dockerfile`.
- **Small server builds instead of pulls**: use `docker compose pull` first, or confirm the `image:` line is present in `compose.yml`.
- **Need to verify what is local**: run `docker images ghcr.io/obelisk-app/obelisk-relay`.
