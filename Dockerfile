# wamn-host image (S1). Build args mirror the layout the runtime-operator
# chart expects: ENTRYPOINT receives ["host", ...] / ["bench", ...] verbatim.
FROM rust:1.97-trixie AS builder
# libprotobuf-dev carries the well-known types (google/protobuf/*.proto)
# that protobuf-compiler alone does not ship on Debian.
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev git && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
# The canonical deploy DDL (run-state.sql / flows.sql) is include_str!'d by
# publish-catalog's provisioning helpers — single source of truth, no clones.
COPY deploy ./deploy
# wash-runtime resolves as a git dep from the fork pinned in Cargo.toml
# (docs/wash-runtime-fork.md); cargo fetches it during the build.
# rust-toolchain.toml would force a rustup download inside the container;
# the base image already ships the right version.
RUN rm rust-toolchain.toml && cargo build --release -p wamn-host

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/wamn-host /usr/local/bin/wamn-host
# Bench fixtures baked in so `kubectl run ... -- bench` works in-cluster.
COPY components/target/wasm32-wasip2/release/hello.wasm /bench/hello.wasm
COPY components/target/wasm32-wasip2/release/memhog.wasm /bench/memhog.wasm
COPY components/target/wasm32-wasip2/release/busyloop.wasm /bench/busyloop.wasm
COPY components/target/wasm32-wasip2/release/pgprobe.wasm /bench/pgprobe.wasm
COPY components/target/wasm32-wasip2/release/flowrunner.wasm /bench/flowrunner.wasm
# S4 custom-node fixtures: the Rust node, the wac-composed frozen flow, and the
# JS/JCO node (built by `jco componentize`, so it lives outside target/).
COPY components/target/wasm32-wasip2/release/node_rs.wasm /bench/node-rs.wasm
COPY components/target/wasm32-wasip2/release/flow_composed.wasm /bench/flow-composed.wasm
COPY components/node-ts/node-ts.wasm /bench/node-ts.wasm
# 5.4 frozen-contract conformance fixture: the scaffolding-built zero-import
# sample node (nodebench --mode sample / the default `all`).
COPY components/target/wasm32-wasip2/release/sample_node.wasm /bench/sample-node.wasm
# S5 logging-capture fixture (imports wasi:logging, exports overhead+emit-batch).
COPY components/target/wasm32-wasip2/release/logspewer.wasm /bench/logspewer.wasm
# 4.1 generated REST API gateway (exports wasi:http/incoming-handler, imports
# wamn:postgres; the apibench gate drives it via ProxyPre).
COPY components/target/wasm32-wasip2/release/api_gateway.wasm /bench/api-gateway.wasm
# POC-F1 sync-webhook ingress (exports wasi:http/incoming-handler, imports
# wamn:postgres, embeds the wamn-runner engine; the f1bench gate drives it).
COPY components/target/wasm32-wasip2/release/webhook_entry.wasm /bench/webhook-entry.wasm
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-host"]
