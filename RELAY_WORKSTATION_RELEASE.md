# Relay Workstation Release Runbook

This replaces the slow GitHub Actions build with a Docker image built on your workstation and pushed directly to GHCR.

## What This Publishes

The relays run this image:

```bash
ghcr.io/obelisk-app/obelisk-relay
```

The server currently runs three relay containers from that image:

- `/root/obelisk-relay`: `groups_relay` on port `8080`
- `/root/obelisk-relay`: `public_relay` on port `8081`
- `/root/lacrypta-relay`: `groups_relay` on port `8083`

## Workstation Prerequisites

Install Docker with Buildx support.

Log in to GHCR from the workstation. Use a GitHub token with package write permission:

```bash
docker login ghcr.io -u YOUR_GITHUB_USERNAME
```

When prompted for a password, paste the GitHub token.

## Build And Push From The Workstation

Clone or update the relay repo on the workstation:

```bash
git clone https://github.com/obelisk-app/obelisk-relay.git
cd obelisk-relay
git pull
```

Choose a tag. A commit tag is usually safest:

```bash
export IMAGE=ghcr.io/obelisk-app/obelisk-relay
export TAG=$(git rev-parse --short HEAD)
```

Build and push for this server's architecture:

```bash
docker buildx build \
  --platform linux/arm64 \
  -t "$IMAGE:$TAG" \
  -t "$IMAGE:latest" \
  --push .
```

If you need the image to also run on x86/AMD servers, publish a multi-arch image instead:

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -t "$IMAGE:$TAG" \
  -t "$IMAGE:latest" \
  --push .
```

The multi-arch build may be slower unless your workstation has native builders for both platforms.

## Deploy On This Server

After the image is pushed, SSH into this server and deploy the exact tag.

Deploy the Obelisk relays:

```bash
cd /root/obelisk-relay
export RELAY_IMAGE_TAG=THE_TAG_YOU_PUSHED
docker compose pull groups_relay public_relay
docker compose up -d groups_relay public_relay
```

Deploy the La Crypta relay:

```bash
cd /root/lacrypta-relay
export RELAY_IMAGE_TAG=THE_TAG_YOU_PUSHED
docker compose pull groups_relay
docker compose up -d groups_relay
```

Example using a commit tag:

```bash
export RELAY_IMAGE_TAG=665491c
```

## Verify

Check the containers:

```bash
docker ps --filter ancestor=ghcr.io/obelisk-app/obelisk-relay:$RELAY_IMAGE_TAG
```

Check the exposed relay ports:

```bash
curl -sf http://127.0.0.1:8080/health
curl -sf http://127.0.0.1:8081/health
curl -sf http://127.0.0.1:8083/health
```

## Notes

- You can still push source code to GitHub, but GitHub no longer needs to build the release image.
- Keep tagging images by commit or version so rollback is easy.
- Avoid deploying untagged `latest` blindly. Push it for convenience, but deploy with the immutable commit or version tag.
- If GitHub Actions is still enabled on `main`, a source push can still trigger the old slow build unless the workflow is disabled or changed.
