# syntax=docker/dockerfile:1
# Both server binaries share this build. Context is the repository root.
FROM rust:1.99.1-slim AS build
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY rust-toolchain.toml ./

# Lock dependency downloads behind manifests, not application source changes.
# server/Cargo.lock locks the complete graph, including the core path dependency.
COPY server/Cargo.toml server/Cargo.lock server/
COPY server/crates/nc-coordinator/Cargo.toml server/crates/nc-coordinator/
COPY server/crates/nc-relay/Cargo.toml server/crates/nc-relay/
COPY core/Cargo.toml core/
COPY core/crates/nc-protocol/Cargo.toml core/crates/nc-protocol/
# Cargo needs targets to inspect manifests; these empty stubs are never compiled.
RUN mkdir -p server/crates/nc-coordinator/src server/crates/nc-relay/src core/crates/nc-protocol/src \
    && touch server/crates/nc-coordinator/src/lib.rs server/crates/nc-coordinator/src/main.rs \
       server/crates/nc-relay/src/lib.rs server/crates/nc-relay/src/main.rs \
       core/crates/nc-protocol/src/lib.rs
RUN --mount=type=cache,id=nc-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=nc-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo fetch --manifest-path server/Cargo.toml --locked

# Preserve repository-relative paths used by Cargo and nc-protocol/build.rs.
COPY core/crates/nc-protocol/ core/crates/nc-protocol/
COPY proto/ proto/
COPY server/ server/
RUN --mount=type=cache,id=nc-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=nc-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=nc-server-target,target=/src/server/target,sharing=locked \
    cd server && cargo build --release --locked -p nc-coordinator -p nc-relay \
    && install -D target/release/nc-coordinator /out/nc-coordinator \
    && install -D target/release/nc-relay /out/nc-relay

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/nc --create-home nc
# Cache mounts are not image layers; export only the finished binaries from /out.
COPY --from=build /out/nc-coordinator /usr/local/bin/nc-coordinator
COPY --from=build /out/nc-relay /usr/local/bin/nc-relay
RUN mkdir -p /var/lib/nc/blobs && chown nc:nc /var/lib/nc/blobs
USER nc
WORKDIR /var/lib/nc
