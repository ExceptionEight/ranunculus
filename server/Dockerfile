# The server of Ranunculus with its web UI. Built from the repository's root:
#   docker build -f server/Dockerfile -t ranunculus-server .
FROM node:26-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1.99.1-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY server server
COPY station station
RUN cargo build --release --locked -p ranunculus-server

# the client for the stations (aarch64, static), of this server's version: a station of another one is given it.
# On an arm64 machine the compiler at hand is the one for it; elsewhere a cross compiler is added
FROM rust:1.99.1-slim AS station
RUN if [ "$(dpkg --print-architecture)" != arm64 ]; then \
        apt-get update && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
        && rm -rf /var/lib/apt/lists/*; \
    fi && rustup target add aarch64-unknown-linux-musl
ENV CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc \
    AR_aarch64_unknown_linux_musl=aarch64-linux-gnu-ar \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C linker-flavor=ld.lld -C target-feature=+crt-static"
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY server server
COPY station station
RUN cargo build --release --locked -p ranunculus-station --target aarch64-unknown-linux-musl

FROM debian:trixie-slim
# ffmpeg: the sounds people upload for timers and alarms come in any format
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/* && mkdir -p /app/data && chown 1000:1000 /app/data
WORKDIR /app
COPY --from=build /src/target/release/ranunculus-server /usr/local/bin/
COPY --from=web /web/dist web/dist
COPY --from=station /src/target/aarch64-unknown-linux-musl/release/ranunculus-station station/ranunculus-station
COPY server/docker/server.toml server.toml
USER 1000:1000
VOLUME /app/data
# 52700: the stations; 52702: the web UI
EXPOSE 52700 52702
CMD ["ranunculus-server"]
