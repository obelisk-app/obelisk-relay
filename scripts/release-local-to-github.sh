#!/usr/bin/env bash
# Build the relay locally, publish the Docker image from this host, and upload
# release assets directly to a GitHub Release.

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/release-local-to-github.sh <tag>

Example:
  GH_TOKEN=ghp_... scripts/release-local-to-github.sh v0.1.0

Required for GitHub release upload:
  GH_TOKEN or GITHUB_TOKEN with contents:write

Required for GHCR push:
  docker login ghcr.io, or GHCR_TOKEN/GH_TOKEN/GITHUB_TOKEN with write:packages

Environment overrides:
  REPO=obelisk-app/obelisk-relay
  IMAGE=ghcr.io/obelisk-app/obelisk-relay
  ARCH_LABEL=linux-x86_64
  DOCKER_PLATFORM=linux/amd64
  PUSH_IMAGE=0              # skip docker push
  BUILD_IMAGE=0             # reuse an existing local IMAGE:tag
  UPLOAD_RELEASE=0          # skip GitHub release creation/upload
  TAG_LATEST=0              # do not also tag/push :latest
  ALLOW_DIRTY=1             # allow a dirty worktree
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

repo_from_origin() {
  local url
  url="$(git remote get-url origin 2>/dev/null || true)"
  case "$url" in
    git@github.com:*)
      url="${url#git@github.com:}"
      url="${url%.git}"
      ;;
    https://github.com/*)
      url="${url#https://github.com/}"
      url="${url%.git}"
      ;;
    http://github.com/*)
      url="${url#http://github.com/}"
      url="${url%.git}"
      ;;
    *)
      return 1
      ;;
  esac
  printf '%s\n' "$url"
}

json_field() {
  local field="$1"
  python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$field"
}

urlencode() {
  python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1]))' "$1"
}

github_token=""

github_request() {
  curl -fsS \
    -H "Authorization: Bearer ${github_token}" \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    "$@"
}

github_request_status() {
  local output="$1"
  shift
  curl -sS \
    -o "$output" \
    -w "%{http_code}" \
    -H "Authorization: Bearer ${github_token}" \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    "$@"
}

upload_asset() {
  local release_json="$1"
  local file="$2"
  local name
  local upload_url
  local existing_id
  local content_type

  name="$(basename "$file")"
  upload_url="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["upload_url"].split("{", 1)[0])' < "$release_json")"
  existing_id="$(
    python3 - "$release_json" "$name" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    release = json.load(f)
name = sys.argv[2]
for asset in release.get("assets", []):
    if asset.get("name") == name:
        print(asset["id"])
        break
PY
  )"

  if [[ -n "$existing_id" ]]; then
    echo "Deleting existing release asset: $name"
    github_request -X DELETE "https://api.github.com/repos/${REPO}/releases/assets/${existing_id}" >/dev/null
  fi

  case "$name" in
    *.tar.gz) content_type="application/gzip" ;;
    *.txt) content_type="text/plain" ;;
    *) content_type="application/octet-stream" ;;
  esac

  echo "Uploading release asset: $name"
  github_request \
    -X POST \
    -H "Content-Type: ${content_type}" \
    --data-binary "@${file}" \
    "${upload_url}?name=$(urlencode "$name")" >/dev/null
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

TAG="${1:-}"
[[ -n "$TAG" ]] || {
  usage >&2
  exit 2
}

need git
need tar
need sha256sum
need python3
need curl
need docker

REPO="${REPO:-$(repo_from_origin || true)}"
[[ -n "$REPO" ]] || die "could not infer GitHub repo from origin; set REPO=owner/name"

IMAGE_DEFAULT="ghcr.io/$(printf '%s' "$REPO" | tr '[:upper:]' '[:lower:]')"
IMAGE="${IMAGE:-$IMAGE_DEFAULT}"
ARCH_LABEL="${ARCH_LABEL:-linux-x86_64}"
BUILD_IMAGE="${BUILD_IMAGE:-1}"
PUSH_IMAGE="${PUSH_IMAGE:-1}"
UPLOAD_RELEASE="${UPLOAD_RELEASE:-1}"
TAG_LATEST="${TAG_LATEST:-1}"
ALLOW_DIRTY="${ALLOW_DIRTY:-0}"
PRERELEASE="${PRERELEASE:-false}"
TARGET_COMMITISH="${TARGET_COMMITISH:-$(git rev-parse HEAD)}"

COMMIT="$(git rev-parse HEAD)"
SHORT_SHA="$(git rev-parse --short=12 HEAD)"
DIST_ROOT="${DIST_ROOT:-dist/releases}"
DIST_DIR="${DIST_ROOT}/${TAG}"
PACKAGE_NAME="obelisk-relay-${TAG}-${ARCH_LABEL}"
PACKAGE_DIR="${DIST_DIR}/package/${PACKAGE_NAME}"
ARCHIVE="${DIST_DIR}/${PACKAGE_NAME}.tar.gz"
CHECKSUMS="${DIST_DIR}/${PACKAGE_NAME}-checksums.txt"
IMAGE_INFO="${DIST_DIR}/${PACKAGE_NAME}-image.txt"
RELEASE_BODY="${DIST_DIR}/release-body.md"

if [[ "$ALLOW_DIRTY" != "1" ]]; then
  git diff --quiet || die "worktree has unstaged changes; commit/stash them or set ALLOW_DIRTY=1"
  git diff --cached --quiet || die "worktree has staged changes; commit/stash them or set ALLOW_DIRTY=1"
  [[ -z "$(git status --porcelain)" ]] || die "worktree has untracked files; commit/stash them or set ALLOW_DIRTY=1"
fi

echo "Preparing local release ${TAG} for ${REPO} at ${COMMIT}"
mkdir -p "$DIST_DIR"
rm -rf "${DIST_DIR}/package"

image_tags=("${IMAGE}:${TAG}" "${IMAGE}:${SHORT_SHA}")
if [[ "$TAG_LATEST" == "1" ]]; then
  image_tags+=("${IMAGE}:latest")
fi

if [[ "$BUILD_IMAGE" == "1" ]]; then
  echo "Building Docker image: ${image_tags[*]}"
  docker_build_args=(build)
  if [[ -n "${DOCKER_PLATFORM:-}" ]]; then
    docker_build_args+=(--platform "$DOCKER_PLATFORM")
  fi
  for image_tag in "${image_tags[@]}"; do
    docker_build_args+=(-t "$image_tag")
  done
  docker_build_args+=(.)
  docker "${docker_build_args[@]}"
else
  echo "Reusing existing local Docker image: ${IMAGE}:${TAG}"
  docker image inspect "${IMAGE}:${TAG}" >/dev/null || die "missing local image: ${IMAGE}:${TAG}"
  for image_tag in "${image_tags[@]}"; do
    if [[ "$image_tag" != "${IMAGE}:${TAG}" ]]; then
      docker tag "${IMAGE}:${TAG}" "$image_tag"
    fi
  done
fi

echo "Packaging release archive from Docker image ${IMAGE}:${TAG}"
mkdir -p "$PACKAGE_DIR"
container_id="$(docker create "${IMAGE}:${TAG}")"
cleanup_container() {
  docker rm -f "$container_id" >/dev/null 2>&1 || true
}
trap cleanup_container EXIT
docker cp "${container_id}:/app/." "$PACKAGE_DIR/"
cleanup_container
trap - EXIT

cp README.md LICENSE compose.yml "${PACKAGE_DIR}/"

cat > "${PACKAGE_DIR}/RELEASE.txt" <<EOF
obelisk-relay ${TAG}
commit: ${COMMIT}
image: ${IMAGE}:${TAG}

Run:
  ./groups_relay --config-dir config
EOF

tar -C "${DIST_DIR}/package" -czf "$ARCHIVE" "$PACKAGE_NAME"
sha256sum "$ARCHIVE" > "$CHECKSUMS"

if [[ "$PUSH_IMAGE" == "1" ]]; then
  ghcr_token="${GHCR_TOKEN:-${GH_TOKEN:-${GITHUB_TOKEN:-}}}"
  if [[ -n "$ghcr_token" ]]; then
    ghcr_user="${GHCR_USER:-${GITHUB_ACTOR:-${REPO%%/*}}}"
    echo "Logging in to ghcr.io as ${ghcr_user}"
    printf '%s' "$ghcr_token" | docker login ghcr.io -u "$ghcr_user" --password-stdin >/dev/null
  else
    echo "No GHCR_TOKEN/GH_TOKEN/GITHUB_TOKEN set; assuming docker is already logged in to ghcr.io"
  fi

  for image_tag in "${image_tags[@]}"; do
    echo "Pushing Docker image: ${image_tag}"
    docker push "$image_tag"
  done
fi

{
  echo "obelisk-relay ${TAG}"
  echo "commit: ${COMMIT}"
  echo
  echo "Docker image tags:"
  for image_tag in "${image_tags[@]}"; do
    echo "- ${image_tag}"
  done
} > "$IMAGE_INFO"

cat > "$RELEASE_BODY" <<EOF
Local release built from \`${COMMIT}\`.

Assets:
- \`$(basename "$ARCHIVE")\`
- \`$(basename "$CHECKSUMS")\`
- \`$(basename "$IMAGE_INFO")\`

Docker image:
EOF

for image_tag in "${image_tags[@]}"; do
  echo "- \`${image_tag}\`" >> "$RELEASE_BODY"
done

if [[ "$UPLOAD_RELEASE" == "1" ]]; then
  github_token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
  [[ -n "$github_token" ]] || die "set GH_TOKEN or GITHUB_TOKEN with contents:write to upload the GitHub Release"

  release_json="${DIST_DIR}/github-release.json"
  release_payload="${DIST_DIR}/github-release-payload.json"

  echo "Creating or updating GitHub Release ${TAG}"
  python3 - "$TAG" "$TARGET_COMMITISH" "$TAG" "$PRERELEASE" "$RELEASE_BODY" > "$release_payload" <<'PY'
import json
import pathlib
import sys

tag, target, name, prerelease, body_path = sys.argv[1:]
print(json.dumps({
    "tag_name": tag,
    "target_commitish": target,
    "name": name,
    "body": pathlib.Path(body_path).read_text(),
    "draft": False,
    "prerelease": prerelease.lower() == "true",
    "make_latest": "true",
}))
PY

  status="$(github_request_status "$release_json" "https://api.github.com/repos/${REPO}/releases/tags/${TAG}")"
  if [[ "$status" == "200" ]]; then
    release_id="$(json_field id < "$release_json")"
    github_request -X PATCH \
      -H "Content-Type: application/json" \
      --data @"$release_payload" \
      "https://api.github.com/repos/${REPO}/releases/${release_id}" > "$release_json"
  elif [[ "$status" == "404" ]]; then
    github_request -X POST \
      -H "Content-Type: application/json" \
      --data @"$release_payload" \
      "https://api.github.com/repos/${REPO}/releases" > "$release_json"
  else
    cat "$release_json" >&2
    die "GitHub release lookup failed with HTTP ${status}"
  fi

  upload_asset "$release_json" "$ARCHIVE"
  upload_asset "$release_json" "$CHECKSUMS"
  upload_asset "$release_json" "$IMAGE_INFO"
fi

echo
echo "Release artifacts:"
echo "  ${ARCHIVE}"
echo "  ${CHECKSUMS}"
echo "  ${IMAGE_INFO}"
if [[ "$UPLOAD_RELEASE" == "1" ]]; then
  echo "GitHub Release:"
  echo "  https://github.com/${REPO}/releases/tag/${TAG}"
fi
