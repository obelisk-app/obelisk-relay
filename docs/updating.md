# Updating the relay from the console

The Settings screen can move this relay to another published image: it shows
what is running, what is available on GHCR, and an **Update relay** control
behind a typed confirmation. If the new container does not come up healthy, the
previous version is restored automatically.

It needs a one-time install on the host. Until that is done the console shows
"No update agent is running on this host" and the button stays disabled.

## Why there is a host agent

The relay can already restart itself — `POST /api/admin/restart` exits the
process and `restart: unless-stopped` brings it back — but compose does not
re-resolve the image tag on a restart, so that returns the *same* build. A real
update means `docker compose pull` and recreating the container.

The relay cannot do that, because it has no Docker socket. It deliberately never
gets one: a socket mount is root on the host, and the relay is the process
terminating untrusted WebSocket traffic from the open internet. Any
remote-code-execution bug in it would become a host compromise.

So the relay only ever *asks*. It writes a request file into its config
directory; a root-owned script on the host reads it and does the work:

```
console → POST /api/admin/update
            ↓ writes public-config/update-request.json
     systemd obelisk-relay-update.path fires
            ↓ scripts/relay-updater.sh
            ↓   validate tag → docker pull → re-sync branding
            ↓   rewrite RELAY_IMAGE_TAG in .env → compose up -d
            ↓   health check, roll back if it fails
            ↓ writes public-config/update-result.json
console polls GET /api/admin/update
```

The tag is validated on both sides — against the published tag list by the
relay, against a character allowlist by the agent. The request carries a *tag*,
never an image reference; the repository is a constant in the script. Neither
side trusts the other to have checked, because the file crosses a privilege
boundary: it is written by a network-facing service and read by root.

## Install

One agent per relay — a host running several needs several, each with its own
config directory, compose service and health URL, which is why the units are
generated rather than copied:

```bash
scripts/install-update-agent.sh NAME REPO_DIR CONFIG_DIR SERVICE HEALTH_URL [COMPOSE_FILE]
```

The two relays on this host:

```bash
scripts/install-update-agent.sh public /root/obelisk-relay \
    /root/obelisk-relay/public-config public_relay http://127.0.0.1:8081/health

scripts/install-update-agent.sh lacrypta /root/lacrypta-relay \
    /root/lacrypta-relay/config relay http://127.0.0.1:8083/health compose.vps.yml
```

Each install writes `/etc/obelisk-relay-update/NAME.env` plus three units —
`obelisk-relay-update-NAME.{service,path,timer}` — and enables them. The `.path`
unit fires the moment a request appears; the `.timer` runs the same script every
10 minutes to record a heartbeat, without which the console cannot tell
"installed and idle" from "never installed" and would offer a button whose
request nothing would ever read.

To repoint an agent, edit its `.env` file rather than the units; re-running the
installer with the same NAME rewrites the instance in place. Requires `docker`,
`jq` and `curl` on the host.

Check on one with:

```bash
systemctl status obelisk-relay-update-public.path
journalctl -u obelisk-relay-update-public.service -n 50
```

## Reporting the running version

The console can only report a version if it is told one. Two pieces:

- **The commit** is stamped at build time by `build.rs`. The `.git` directory is
  not in the Docker build context, so the image build must pass it:

  ```bash
  docker compose build --build-arg GIT_SHA=$(git rev-parse --short HEAD) \
                       --build-arg BUILD_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ) public_relay
  ```

  Without those it falls back to git (which works for a local `cargo build`) and
  then to `unknown`.

- **The image tag** cannot be discovered from inside a container at all — Docker
  tells a process nothing about the image it came from. `compose.yml` passes it
  in as `RELAY_IMAGE_TAG`. A deployment that predates that shows "Unknown"
  rather than guessing.

## The branding trap

`compose.yml` bind-mounts the purple theme over a **hash-named** stylesheet
inside the image. A new image has a different hash, so an update would mount the
override onto a path nothing loads and the theme would silently revert to green.
This has already happened twice by hand.

The updater therefore runs `public-config/branding/retint.sh` against the new
image before recreating the container, and **fails the update** if it cannot
determine the hash rather than shipping a broken theme.

## Testing it without risking the relay

Point the script at a scratch directory with a fake `docker` on `PATH`:

```bash
RELAY_REPO_DIR=/tmp/updtest/repo RELAY_CONFIG_DIR=/tmp/updtest/repo/public-config \
RELAY_ENV_FILE=/tmp/updtest/repo/.env scripts/relay-updater.sh run
```

Worth exercising all four outcomes, because only one of them is the happy path:
a valid tag that comes up healthy (`ok`), a tag containing shell metacharacters
(`rejected`, before anything runs), an image that will not pull (`failed`, with
the pin untouched), and an image that pulls but never answers `/health`
(`rolled-back`, with the pin restored).

## Rolling back

Requesting the previous tag from the console is a normal update. If the console
itself is unreachable, the manual path is unchanged:

```bash
sed -i 's/^RELAY_IMAGE_TAG=.*/RELAY_IMAGE_TAG=<previous>/' .env
docker compose up -d public_relay
```

## Note on the running image

At the time of writing this relay runs `local-wot-attribution`, a locally built
tag that does not exist in GHCR. The console only offers published tags, so the
first update from the UI necessarily moves off it. Publish an equivalent tag
first if those local changes matter.
