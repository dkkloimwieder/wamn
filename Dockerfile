# wamn images (SR1 pattern: one build, one final stage per artifact; SR9 split).
#   docker build --target host       -t wamn-host:dev       .  # washlet ONLY
#   docker build --target ctl        -t wamn-ctl:dev        .  # one-shot verbs
#   docker build --target dispatcher -t wamn-dispatcher:dev .  # trigger dispatcher
#   docker build --target run-worker -t wamn-run-worker:dev .  # production executor (+flowrunner.wasm)
#   docker build --target scenario-worker -t wamn-scenario-worker:dev . # deterministic scenarios
#   docker build --target cdc-reader -t wamn-cdc-reader:dev .  # CDC event reader
#   docker build --target waker      -t wamn-waker:dev      .  # scale-to-zero wake actuator
#   docker build --target gates      -t wamn-gates:dev      .  # gates: FROM host + suite + fixtures
# Later invocations reuse cargo-chef dependency layers and named BuildKit
# target caches. The root and component workspaces cook from separate recipes,
# so their lockfiles remain independent cache keys. The
# washlet artifact ships no provisioning / replication-credential / gate code
# (SR9 strings spot-check); the gates image layers the suite on top of the
# IDENTICAL host stage so Jobs exercise the same host lib code they verify.
FROM rust:1.97-trixie AS chef
# libprotobuf-dev carries the well-known types (google/protobuf/*.proto)
# that protobuf-compiler alone does not ship on Debian.
RUN apt-get update && apt-get install -y --no-install-recommends clang mold protobuf-compiler libprotobuf-dev git && rm -rf /var/lib/apt/lists/*
WORKDIR /build
RUN --mount=type=cache,id=wamn-chef-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-chef-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo install cargo-chef --version 0.1.77 --locked

# The planner may see source changes, but the recipe copied into root-cook
# changes only when the root workspace manifests or Cargo.lock change.
FROM chef AS root-planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY services ./services
COPY test-support ./test-support
COPY tests ./tests
RUN cargo chef prepare --recipe-path root-recipe.json

FROM chef AS root-cook
COPY .cargo/config.toml ./.cargo/config.toml
COPY --from=root-planner /build/root-recipe.json ./root-recipe.json
# Keep cooked dependencies in an ordinary image layer so mode=max registry
# cache export can carry them to a fresh builder. The real build seeds its
# local target cache from this layer when the recipe changes.
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo chef cook --locked --release --recipe-path root-recipe.json \
      -p wamn-host -p wamn-ctl -p wamn-dispatcher \
      -p wamn-executor -p wamn-scenario-worker -p wamn-cdc-reader \
      -p wamn-waker -p wamn-gates \
 && mv target /root-chef-target

FROM root-cook AS builder
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY services ./services
COPY test-support ./test-support
COPY tests ./tests
# The canonical deploy DDL (sql/run-state.sql / sql/flows.sql) is include_str!'d by
# publish-catalog's provisioning helpers — single source of truth, no clones.
COPY deploy ./deploy
# wamn-gates embeds the flowrunner dispatch source guard; copy only that file,
# not the component target.
COPY components/execution/flowrunner/src/lib.rs ./components/execution/flowrunner/src/lib.rs
# wash-runtime resolves as a git dep from the fork pinned in Cargo.toml
# (docs/archive/platform/wash-runtime-fork.md); cargo fetches it during the cook/build.
# rust-toolchain.toml is deliberately absent: the base image already ships the
# pinned Rust line, and copying it would force a rustup download in the image.
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    if ! cmp -s root-recipe.json target/.wamn-chef-recipe.json; then \
      find target -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +; \
      cp -a /root-chef-target/. target/; \
      cp root-recipe.json target/.wamn-chef-recipe.json; \
    fi \
 && cargo build --locked --release \
      -p wamn-host -p wamn-ctl -p wamn-dispatcher \
      -p wamn-executor -p wamn-scenario-worker -p wamn-cdc-reader \
      -p wamn-waker -p wamn-gates \
 && cargo build --locked --release -p wamn-ctl --features ops --bin wamn-ctl-ops \
 && install -d /native-output \
 && for artifact in \
      wamn-host wamn-ctl wamn-ctl-ops wamn-dispatcher wamn-run-worker \
      wamn-scenario-worker wamn-cdc-reader wamn-waker wamn-gates; do \
      install -m 0755 "target/release/${artifact}" "/native-output/${artifact}"; \
    done

# ---- locked component outputs shared by every embedding image --------------
# Keep the guest workspace and lockfile separate from the native recipe. Root
# Cargo.toml plus crates are present because guest path dependencies inherit
# root workspace fields and use those source trees.
FROM chef AS component-planner
COPY Cargo.toml ./
COPY crates ./crates
COPY components ./components
WORKDIR /build/components
RUN cargo chef prepare --recipe-path component-recipe.json

FROM chef AS component-toolchain
RUN rustup target add --toolchain 1.97.0 wasm32-wasip2

FROM component-toolchain AS component-cook
COPY .cargo/config.toml /build/.cargo/config.toml
COPY --from=component-planner /build/Cargo.toml /build/Cargo.toml
COPY --from=component-planner /build/crates /build/crates
WORKDIR /build/components
COPY --from=component-planner /build/components/component-recipe.json ./component-recipe.json
RUN --mount=type=cache,id=wamn-component-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-component-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo +1.97.0 chef cook --locked --release --target wasm32-wasip2 \
      --recipe-path component-recipe.json \
      -p flow-http -p flowrunner -p materializer \
      -p busyloop -p connection-http-standard -p sockprobe \
 && mv target /component-chef-target

FROM component-cook AS component-builder
COPY .cargo/config.toml /build/.cargo/config.toml
COPY Cargo.toml /build/Cargo.toml
COPY crates /build/crates
COPY components /build/components
RUN --mount=type=cache,id=wamn-component-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=wamn-component-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=wamn-component-target,target=/build/components/target,sharing=locked \
    if ! cmp -s component-recipe.json target/.wamn-chef-recipe.json; then \
      find target -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +; \
      cp -a /component-chef-target/. target/; \
      cp component-recipe.json target/.wamn-chef-recipe.json; \
    fi \
 && cargo +1.97.0 build --locked --release --target wasm32-wasip2 \
      -p flow-http -p flowrunner -p materializer \
      -p busyloop -p connection-http-standard -p sockprobe \
 && install -d /component-output \
 && for artifact in \
      flow_http flowrunner materializer \
      busyloop connection_http_standard sockprobe; do \
      install -m 0644 "target/wasm32-wasip2/release/${artifact}.wasm" \
        "/component-output/${artifact}.wasm"; \
    done

# ---- washlet image: host + locked inline flowrunner -------------------------
FROM debian:trixie-slim AS host
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /native-output/wamn-host /usr/local/bin/wamn-host
COPY --from=component-builder /component-output/flowrunner.wasm /components/flowrunner.wasm
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-host"]

# ---- ctl image: the one-shot control-plane verbs (SR9) ----------------------
# NOTE pg_dump/pg_restore are NOT installed (parity with the pre-split image);
# dump/restore-project-env need a pg-client-equipped environment.
FROM debian:trixie-slim AS ctl
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /native-output/wamn-ctl /usr/local/bin/wamn-ctl
COPY --from=builder /native-output/wamn-ctl-ops /usr/local/bin/wamn-ctl-ops
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-ctl"]

# ---- dispatcher image: the shared trigger dispatcher service (SR9) ----------
FROM debian:trixie-slim AS dispatcher
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /native-output/wamn-dispatcher /usr/local/bin/wamn-dispatcher
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-dispatcher"]

# ---- executor image: the production flow runner + its component (SR9) -------
# The deployment/image/binary identity remains wamn-run-worker; the owning
# source package is wamn-executor.
FROM debian:trixie-slim AS run-worker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /native-output/wamn-run-worker /usr/local/bin/wamn-run-worker
# The flowrunner component is a PRODUCTION artifact, not a gate fixture: the
# run-worker (fqg.8) instantiates it to drive claimed runs, so it travels with
# this binary (default --flowrunner /components/flowrunner.wasm).
COPY --from=component-builder /component-output/flowrunner.wasm /components/flowrunner.wasm
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-run-worker"]

# ---- scenario-worker image: deterministic product scenarios ----------------
FROM debian:trixie-slim AS scenario-worker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /native-output/wamn-scenario-worker /usr/local/bin/wamn-scenario-worker
# Deliberately the same compiled guest as the production executor. Capability
# composition differs in the native service artifact, not in flow semantics.
COPY --from=component-builder /component-output/flowrunner.wasm /components/flowrunner.wasm
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-scenario-worker"]

# ---- cdc-reader image: the CDC event reader service (SR9) -------------------
FROM debian:trixie-slim AS cdc-reader
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /native-output/wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-cdc-reader"]

# ---- waker image: the scale-to-zero wake actuator (fqg.12, POC-F3) ----------
# Watches the doorbell and scales a parked runner Deployment 0->1 via the k8s
# API. The ONE component granted k8s scale privilege (deploy/platform/waker.yaml).
FROM debian:trixie-slim AS waker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /native-output/wamn-waker /usr/local/bin/wamn-waker
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-waker"]

# ---- gates image: the host stage + the gate suite + wasm fixtures -----------
FROM host AS gates
COPY --from=builder /native-output/wamn-gates /usr/local/bin/wamn-gates
# Control-plane integration proofs drive the deployable ctl artifact through its
# executable boundary; the proof packages do not link the service crate.
COPY --from=builder /native-output/wamn-ctl /usr/local/bin/wamn-ctl
# Operations-only impact analysis crosses its own executable boundary.
COPY --from=builder /native-output/wamn-ctl-ops /usr/local/bin/wamn-ctl-ops
# Stored-suite compatibility is a process adapter: the gate invokes the product
# worker binary and never links its execution engine into wamn-gates.
COPY --from=builder /native-output/wamn-scenario-worker /usr/local/bin/wamn-scenario-worker
# Reader-inclusive gates exercise the native CDC service through its executable
# boundary; the gates package does not link the service crate.
COPY --from=builder /native-output/wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
# Dispatcher gates drive stepped and lifecycle behavior through the executable
# boundary; the gates package does not link the deployable service crate.
COPY --from=builder /native-output/wamn-dispatcher /usr/local/bin/wamn-dispatcher
# Metricbench drives run telemetry through the production executor boundary;
# the integration proof must not duplicate the executor-owned instruments.
COPY --from=builder /native-output/wamn-run-worker /usr/local/bin/wamn-run-worker
# Proof fixtures baked in so the retained gates run with no volume plumbing.
COPY --from=component-builder /component-output/busyloop.wasm /bench/busyloop.wasm
# E13/E15 runtime raw-socket fixture: attempts raw TCP + UDP egress via
# wasi:sockets so egressbench can assert the fork's socket_addr_check deny.
COPY --from=component-builder /component-output/sockprobe.wasm /bench/sockprobe.wasm
COPY --from=component-builder /component-output/flowrunner.wasm /bench/flowrunner.wasm
# Callable-flow HTTP ingress: bounded routing/auth/mapping adapter over the
# frozen flow-invocation provider contract.
COPY --from=component-builder /component-output/flow_http.wasm /bench/flow-http.wasm
# l5i9.17 materializer Service guest (wasi:cli/run; imports wamn:postgres +
# wamn:jetstream; the matbench gate drives it via CommandPre — the same wasm the
# WorkloadDeployment pulls from the registry in production).
COPY --from=component-builder /component-output/materializer.wasm /bench/materializer.wasm
COPY --from=component-builder /component-output/connection_http_standard.wasm /bench/connection-http-standard.wasm
ENTRYPOINT ["/usr/local/bin/wamn-gates"]

FROM chef AS cranelift-dev
# Opt-in native debug shell only. No shipping stage inherits this toolchain.
RUN rustup toolchain install nightly --profile minimal \
 && rustup component add rustc-codegen-cranelift-preview --toolchain nightly
COPY --chmod=0755 tools/cargo-cranelift /usr/local/bin/cargo-cranelift
WORKDIR /workspace
