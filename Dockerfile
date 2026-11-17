# =============================================================================
# Stage 1: Build embedded React 19 + Vite WebUI (`web/dist`)
# =============================================================================
FROM --platform=$BUILDPLATFORM node:26-alpine AS web-builder

RUN npm install -g pnpm@10

WORKDIR /build/FeedCourier/web

COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile

COPY web/ ./
RUN pnpm build

# =============================================================================
# Stage 2: Normalize Cargo manifests so version bumps do not bust dep cache
# =============================================================================
FROM --platform=$BUILDPLATFORM node:26-alpine AS cargo-recipe

WORKDIR /recipe

COPY Cargo.toml Cargo.lock ./
RUN awk ' \
      /^\[package\]$/ { in_pkg = 1 } \
      /^\[/ && !/^\[package\]$/ { in_pkg = 0 } \
      in_pkg && /^version = "/ { print "version = \"0.0.0\""; next } \
      { print } \
    ' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml \
    && awk ' \
      /^name = "feedcourier"$/ { is_fc = 1; print; next } \
      is_fc && /^version = "/ { print "version = \"0.0.0\""; is_fc = 0; next } \
      { print } \
    ' Cargo.lock > Cargo.lock.tmp && mv Cargo.lock.tmp Cargo.lock

# =============================================================================
# Stage 3: Build single-binary Rust service (caching external dependencies)
# =============================================================================
FROM rust:1-bookworm AS rust-builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build/FeedCourier

# Pre-compile all external crates; cached across src/, web/, and version changes
COPY --from=cargo-recipe /recipe/Cargo.toml ./Cargo.toml
COPY --from=cargo-recipe /recipe/Cargo.lock ./Cargo.lock
RUN mkdir -p src \
    && printf "" > src/lib.rs \
    && printf "fn main() {}\n" > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src \
       target/release/feedcourier* \
       target/release/deps/feedcourier* \
       target/release/deps/libfeedcourier* \
       target/release/.fingerprint/feedcourier-* \
       target/release/incremental/feedcourier*

COPY Cargo.toml Cargo.lock LICENSE ./
COPY migrations ./migrations
COPY src ./src
COPY --from=web-builder /build/FeedCourier/web/dist ./web/dist

RUN cargo build --release --locked \
    && strip /build/FeedCourier/target/release/feedcourier

# =============================================================================
# Stage 4: Minimal Debian Bookworm runtime (drops privileges to UID/GID 10001)
# =============================================================================
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 feedcourier \
    && useradd --uid 10001 --gid feedcourier --home-dir /data --no-create-home --no-log-init --shell /usr/sbin/nologin feedcourier \
    && mkdir -p /data \
    && chown -R feedcourier:feedcourier /data

COPY --from=rust-builder /build/FeedCourier/target/release/feedcourier /usr/local/bin/feedcourier
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod 0755 /usr/local/bin/docker-entrypoint.sh

ENV DATA_DIR=/data \
    LISTEN_ADDR=0.0.0.0:8080

VOLUME ["/data"]
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["/usr/local/bin/feedcourier"]
