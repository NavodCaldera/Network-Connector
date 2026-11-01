# Builds the server workspace (nc-coordinator + nc-relay) into one slim runtime image.
# Build context: repository root (see docker-compose.yml).
FROM rust:1.99.1-slim AS build
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY rust-toolchain.toml ./
COPY server/ server/
RUN cd server && cargo build --release --locked -p nc-coordinator -p nc-relay

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/nc --create-home nc
COPY --from=build /src/server/target/release/nc-coordinator /usr/local/bin/nc-coordinator
COPY --from=build /src/server/target/release/nc-relay /usr/local/bin/nc-relay
USER nc
WORKDIR /var/lib/nc
