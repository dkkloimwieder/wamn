# wamn-host image (S1). Build args mirror the layout the runtime-operator
# chart expects: ENTRYPOINT receives ["host", ...] / ["bench", ...] verbatim.
FROM rust:1.97-trixie AS builder
# libprotobuf-dev carries the well-known types (google/protobuf/*.proto)
# that protobuf-compiler alone does not ship on Debian.
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
# rust-toolchain.toml would force a rustup download inside the container;
# the base image already ships the right version.
RUN rm rust-toolchain.toml && cargo build --release -p wamn-host

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/wamn-host /usr/local/bin/wamn-host
# Bench fixtures baked in so `kubectl run ... -- bench` works in-cluster.
COPY components/target/wasm32-wasip2/release/hello.wasm /bench/hello.wasm
COPY components/target/wasm32-wasip2/release/memhog.wasm /bench/memhog.wasm
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-host"]
