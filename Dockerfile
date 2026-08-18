# Server image: Rust API + the built dashboard it serves.
#
# The dashboard is built here rather than copied from the host, so the image
# never depends on someone having run `npm run build` first.

# --- dashboard ------------------------------------------------------------
FROM node:22-slim AS dashboard
WORKDIR /app
# Lockfile first: this layer is cached until dependencies actually change.
COPY dashboard/package.json dashboard/package-lock.json ./dashboard/
RUN cd dashboard && npm ci
COPY dashboard/ ./dashboard/
# vite.config.ts writes to ../server/dashboard/dist, so the output lands at
# /app/server/dashboard/dist. `npm run build` typechecks first.
RUN cd dashboard && npm run build

# --- server ---------------------------------------------------------------
FROM rust:1.97-slim-bookworm AS server
WORKDIR /app
# libsqlite3-sys builds a bundled SQLite, so a C toolchain is required.
# TLS is rustls, so there is no OpenSSL dependency to satisfy here.
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential \
    && rm -rf /var/lib/apt/lists/*

# Manifests first, then a throwaway build, so the dependency compile is cached
# independently of the source. The dummy files are replaced below.
COPY Cargo.toml Cargo.lock ./
COPY shared/Cargo.toml ./shared/
COPY server/Cargo.toml ./server/
COPY pi-agent/Cargo.toml ./pi-agent/
RUN mkdir -p shared/src server/src pi-agent/src \
    && echo "" > shared/src/lib.rs \
    && echo "fn main() {}" > server/src/main.rs \
    && echo "fn main() {}" > pi-agent/src/main.rs \
    && cargo build --release -p server \
    && rm -rf shared/src server/src pi-agent/src

COPY shared/ ./shared/
COPY server/ ./server/
# `sqlx::migrate!("./migrations")` embeds the SQL at compile time, so
# server/migrations must be present here — but is not needed at runtime.
RUN touch shared/src/lib.rs server/src/main.rs \
    && cargo build --release -p server

# --- runtime --------------------------------------------------------------
FROM debian:bookworm-slim
WORKDIR /app

# ffprobe (in ffmpeg) is optional — without it videos are indexed with a null
# duration. It is small enough to be worth including.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates \
      ffmpeg \
    && rm -rf /var/lib/apt/lists/*

# The server writes the SQLite database and reads the video directory; both are
# bind-mounted in compose. It needs no root privileges for either.
RUN useradd --system --create-home --uid 10001 tvserver \
    && mkdir -p /app/videos /app/data \
    && chown -R tvserver:tvserver /app

COPY --from=server  --chown=tvserver:tvserver /app/target/release/server /usr/local/bin/server
COPY --from=dashboard --chown=tvserver:tvserver /app/server/dashboard/dist /app/dashboard/dist

USER tvserver

ENV DASHBOARD_DIR=/app/dashboard/dist \
    VIDEOS_DIR=/app/videos \
    DATABASE_URL=sqlite:/app/data/tv-controller.db \
    PORT=8000

EXPOSE 8000

# SERVER_BASE_URL is deliberately not defaulted — the server refuses to start
# without it, and a wrong value fails later and less obviously, on the Pi.
CMD ["server"]
