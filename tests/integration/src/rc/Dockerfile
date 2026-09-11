FROM rust:1.98-trixie AS native
RUN apt-get update && apt-get install -y --no-install-recommends clang mold protobuf-compiler libprotobuf-dev git \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY Dockerfile ./Dockerfile
COPY .cargo ./.cargo
COPY apps ./apps
COPY crates ./crates
COPY services ./services
COPY test-support ./test-support
COPY tests ./tests
COPY deploy ./deploy
RUN --mount=type=cache,id=wamn-rc-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-rc-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-rc-debug-target,target=/build/target,sharing=locked \
    RUSTC_WRAPPER= cargo build --locked -p wamn-gates -p wamn-host \
 && install -D -m 0755 target/debug/wamn-host /native-output/wamn-host \
 && install -D -m 0755 target/debug/wamn-gates /native-output/wamn-gates
FROM debian:trixie-slim AS base
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
ARG SOURCE_HEAD
LABEL wamn.dev/source-head="$SOURCE_HEAD" wamn.dev/build-profile="debug"
FROM base AS host
COPY --from=native /native-output/wamn-host /usr/local/bin/wamn-host
ENTRYPOINT ["/usr/local/bin/wamn-host"]
FROM host AS gates
COPY --from=native /native-output/wamn-gates /usr/local/bin/wamn-gates
ENTRYPOINT ["/usr/local/bin/wamn-gates"]
