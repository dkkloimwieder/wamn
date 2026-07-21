# wamn images (SR1 pattern: one build, one final stage per artifact; SR9 split).
#   docker build --target host       -t wamn-host:dev       .  # washlet ONLY
#   docker build --target ctl        -t wamn-ctl:dev        .  # one-shot verbs
#   docker build --target dispatcher -t wamn-dispatcher:dev .  # trigger dispatcher
#   docker build --target run-worker -t wamn-run-worker:dev .  # flow runner (+flowrunner.wasm)
#   docker build --target cdc-reader -t wamn-cdc-reader:dev .  # CDC event reader
#   docker build --target gates      -t wamn-gates:dev      .  # gates: FROM host + suite + fixtures
# Later invocations are fully layer-cached off the one builder stage. The
# washlet artifact ships no provisioning / replication-credential / gate code
# (SR9 strings spot-check); the gates image layers the suite on top of the
# IDENTICAL host stage so Jobs exercise the same host lib code they verify.
FROM rust:1.97-trixie AS builder
# libprotobuf-dev carries the well-known types (google/protobuf/*.proto)
# that protobuf-compiler alone does not ship on Debian.
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev git && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY poc ./poc
# The canonical deploy DDL (sql/run-state.sql / sql/flows.sql) is include_str!'d by
# publish-catalog's provisioning helpers — single source of truth, no clones.
COPY deploy ./deploy
# wash-runtime resolves as a git dep from the fork pinned in Cargo.toml
# (docs/wash-runtime-fork.md); cargo fetches it during the build.
# rust-toolchain.toml would force a rustup download inside the container;
# the base image already ships the right version.
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git rm rust-toolchain.toml && cargo build --release -p wamn-host -p wamn-ctl -p wamn-dispatcher -p wamn-run-worker -p wamn-cdc-reader -p wamn-gates

# ---- washlet image: the host binary only ------------------------------------
FROM debian:trixie-slim AS host
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/wamn-host /usr/local/bin/wamn-host
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-host"]

# ---- ctl image: the one-shot control-plane verbs (SR9) ----------------------
# NOTE pg_dump/pg_restore are NOT installed (parity with the pre-split image);
# dump/restore-project-env need a pg-client-equipped environment.
FROM debian:trixie-slim AS ctl
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/wamn-ctl /usr/local/bin/wamn-ctl
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-ctl"]

# ---- dispatcher image: the shared trigger dispatcher service (SR9) ----------
FROM debian:trixie-slim AS dispatcher
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/wamn-dispatcher /usr/local/bin/wamn-dispatcher
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-dispatcher"]

# ---- run-worker image: the flow runner service + its component (SR9) --------
FROM debian:trixie-slim AS run-worker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/wamn-run-worker /usr/local/bin/wamn-run-worker
# The flowrunner component is a PRODUCTION artifact, not a gate fixture: the
# run-worker (fqg.8) instantiates it to drive claimed runs, so it travels with
# this binary (default --flowrunner /components/flowrunner.wasm).
COPY components/target/wasm32-wasip2/release/flowrunner.wasm /components/flowrunner.wasm
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-run-worker"]

# ---- cdc-reader image: the CDC event reader service (SR9) -------------------
FROM debian:trixie-slim AS cdc-reader
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-cdc-reader"]

# ---- gates image: the host stage + the gate suite + wasm fixtures -----------
FROM host AS gates
COPY --from=builder /build/target/release/wamn-gates /usr/local/bin/wamn-gates
# Bench fixtures baked in so the gate Jobs run with no volume plumbing.
COPY components/target/wasm32-wasip2/release/hello.wasm /bench/hello.wasm
COPY components/target/wasm32-wasip2/release/memhog.wasm /bench/memhog.wasm
COPY components/target/wasm32-wasip2/release/busyloop.wasm /bench/busyloop.wasm
# E13/E15 runtime raw-socket fixture: attempts raw TCP + UDP egress via
# wasi:sockets so egressbench can assert the fork's socket_addr_check deny.
COPY components/target/wasm32-wasip2/release/sockprobe.wasm /bench/sockprobe.wasm
COPY components/target/wasm32-wasip2/release/pgprobe.wasm /bench/pgprobe.wasm
COPY components/target/wasm32-wasip2/release/flowrunner.wasm /bench/flowrunner.wasm
# S4 custom-node fixtures: the Rust node, the wac-composed frozen flow, and the
# JS/JCO node (built by `jco componentize`, so it lives outside target/).
COPY components/target/wasm32-wasip2/release/node_rs.wasm /bench/node-rs.wasm
COPY components/target/wasm32-wasip2/release/flow_composed.wasm /bench/flow-composed.wasm
COPY components/samples/node-ts/node-ts.wasm /bench/node-ts.wasm
# 5.4 frozen-contract conformance fixture: the scaffolding-built zero-import
# sample node (nodebench --mode sample / the default `all`).
COPY components/target/wasm32-wasip2/release/sample_node.wasm /bench/sample-node.wasm
# S5 logging-capture fixture (imports wasi:logging, exports overhead+emit-batch).
COPY components/target/wasm32-wasip2/release/logspewer.wasm /bench/logspewer.wasm
# 4.1 generated REST API gateway (exports wasi:http/incoming-handler, imports
# wamn:postgres; the apibench gate drives it via ProxyPre).
COPY components/target/wasm32-wasip2/release/api_gateway.wasm /bench/api-gateway.wasm
# l5i9.17 materializer Service guest (wasi:cli/run; imports wamn:postgres +
# wamn:jetstream; the matbench gate drives it via CommandPre — the same wasm the
# WorkloadDeployment pulls from the registry in production).
COPY components/target/wasm32-wasip2/release/materializer.wasm /bench/materializer.wasm
# l5i9.57 E10-e2e wamn:jetstream sample guest (wasi:cli/run; imports
# wamn:jetstream consumer + producer — the first producer importer + the adopter
# template; the samplebench gate drives it via CommandPre). Bin crate, so the
# artifact keeps its hyphen (js-sample.wasm), unlike the cdylib underscore names.
COPY components/target/wasm32-wasip2/release/js-sample.wasm /bench/js-sample.wasm
# POC-F1 sync-webhook ingress (exports wasi:http/incoming-handler, imports
# wamn:postgres, embeds the wamn-runner engine; the f1bench gate drives it).
COPY components/target/wasm32-wasip2/release/poc_webhook_f1.wasm /bench/poc-webhook-f1.wasm
ENTRYPOINT ["/usr/local/bin/wamn-gates"]
