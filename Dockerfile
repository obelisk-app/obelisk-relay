ARG RUST_VERSION=1.91.0

FROM rust:${RUST_VERSION}-slim-bookworm AS rust-builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    make \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/app

# Copy the project for building
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
# Stamps the binary with the commit it was built from. Cargo picks this up by
# name, so omitting it does not fail the copy -- it just silently builds an
# unstamped relay.
COPY build.rs ./
COPY src ./src
COPY benches ./benches

# Build all binaries (console feature disabled for stability testing)
# tokio_unstable needed for runtime metrics used by watchdog
# tokio_taskdump enables task dumps when deadlocks are detected (Linux only)
ENV RUSTFLAGS="--cfg tokio_unstable --cfg tokio_taskdump"

# Parallelism cap for constrained build hosts. Limiting the builder's CPU alone
# is not enough: cargo still spawns one rustc per core, which thrashes against
# the cgroup quota and multiplies peak memory. Set CARGO_BUILD_JOBS=1 when
# building on a box that is also serving traffic.
#
# The default is the literal "default" — cargo's own word for "use all cores" —
# and not an empty value. `ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}` with the
# arg unset does not leave the variable unset, it sets it to "", and cargo then
# tries to parse "" as a job count and aborts:
#
#   error: could not parse ``. Number of parallel jobs should be `default` or a number.
#
# Every build that did not pass the arg therefore failed at exit 101, which is
# why CI could not build this image at all.
ARG CARGO_BUILD_JOBS=default
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}

# Stamp the build so the admin console can say what it is running. The .git
# directory is not in the build context, so build.rs cannot work this out for
# itself here -- pass it in:
#   docker compose build --build-arg GIT_SHA=$(git rev-parse --short HEAD) \
#                        --build-arg BUILD_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ)
# Left empty, build.rs falls back to git and then to "unknown".
ARG GIT_SHA=
ARG BUILD_TIME=
ENV GIT_SHA=${GIT_SHA}
ENV BUILD_TIME=${BUILD_TIME}

RUN cargo build --release --bins

# Install binaries from relay_builder
RUN cargo install --git https://github.com/verse-pbc/relay_builder \
    --bin export_import \
    --bin negentropy_sync \
    --bin nostr-lmdb-dump \
    --bin nostr-lmdb-integrity

FROM node:24-slim AS frontend-builder

WORKDIR /usr/src/app/frontend

RUN apt-get update && apt-get install -y \
    python3 \
    make \
    g++ \
    && rm -rf /var/lib/apt/lists/*

COPY frontend/package*.json ./
COPY frontend/pnpm-lock.yaml ./

RUN npm install -g pnpm@9.15.9 && pnpm install --frozen-lockfile

COPY frontend/src ./src
COPY frontend/public ./public
COPY frontend/index.html ./
COPY frontend/vite.config.mts ./
COPY frontend/tsconfig.json ./
COPY frontend/postcss.config.cjs ./
COPY frontend/tailwind.config.js ./

ENV NODE_ENV=production
RUN pnpm run build

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    libssl-dev \
    curl \
    iproute2 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy all pre-built binaries and default config
COPY --from=rust-builder /usr/src/app/target/release/groups_relay ./groups_relay
COPY --from=rust-builder /usr/src/app/target/release/delete_event ./delete_event
COPY --from=rust-builder /usr/src/app/target/release/add_original_relay ./add_original_relay
# The admin console shells out to this to measure free-list slack: LMDB forbids
# opening the same environment twice in one process, so the running relay cannot
# measure the database it is serving from.
COPY --from=rust-builder /usr/src/app/target/release/lmdb_stat ./lmdb_stat
# console_dump requires console-dump feature, skipped for stability testing
# COPY --from=rust-builder /usr/src/app/target/release/console_dump ./console_dump
# Copy cargo-installed binaries
COPY --from=rust-builder /usr/local/cargo/bin/export_import ./export_import
COPY --from=rust-builder /usr/local/cargo/bin/negentropy_sync ./negentropy_sync
COPY --from=rust-builder /usr/local/cargo/bin/nostr-lmdb-dump ./nostr-lmdb-dump
COPY --from=rust-builder /usr/local/cargo/bin/nostr-lmdb-integrity ./nostr-lmdb-integrity
COPY config/settings.yml ./config/
COPY --from=frontend-builder /usr/src/app/frontend/dist ./frontend/dist

EXPOSE 8080
EXPOSE 6669

ENV NODE_ENV=production

CMD ["./groups_relay"]
