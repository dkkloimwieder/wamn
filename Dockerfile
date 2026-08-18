# wamn images (SR1 pattern: one build, one final stage per artifact; SR9 split).
#   docker build --target host       -t wamn-host:dev       .  # washlet ONLY
#   docker build --target ctl        -t wamn-ctl:dev        .  # one-shot verbs
#   docker build --target dispatcher -t wamn-dispatcher:dev .  # trigger dispatcher
#   docker build --target run-worker -t wamn-run-worker:dev .  # production executor (+flowrunner.wasm)
#   docker build --target scenario-worker -t wamn-scenario-worker:dev . # authoring management
#   docker build --target cdc-reader -t wamn-cdc-reader:dev .  # CDC event reader
#   docker build --target waker      -t wamn-waker:dev      .  # scale-to-zero wake actuator
#   docker build --target gates      -t wamn-gates:dev      .  # gates: FROM host + suite + fixtures
# Later invocations reuse one cargo-chef recipe and shared, locked BuildKit
# registry, Git, and target caches. Each retained native image cooks and builds
# only its top-level package closure. The
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

# The planner may see source changes, but the recipe copied into each cook stage
# changes only when the root workspace manifests or Cargo.lock change.
FROM chef AS root-planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY services ./services
COPY test-support ./test-support
COPY tests ./tests
RUN cargo chef prepare --recipe-path root-recipe.json

FROM chef AS root-recipe
COPY .cargo/config.toml ./.cargo/config.toml
COPY --from=root-planner /build/root-recipe.json ./root-recipe.json

FROM root-recipe AS cook-host
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path root-recipe.json -p wamn-host

FROM root-recipe AS cook-executor
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path root-recipe.json -p wamn-executor

FROM root-recipe AS cook-scenario-worker
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path root-recipe.json -p wamn-scenario-worker

FROM root-recipe AS cook-ctl
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path root-recipe.json -p wamn-ctl

FROM root-recipe AS cook-dispatcher
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path root-recipe.json -p wamn-dispatcher

FROM root-recipe AS cook-waker
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path root-recipe.json -p wamn-waker

FROM root-recipe AS cook-cdc-reader
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo chef cook --locked --release --recipe-path root-recipe.json -p wamn-cdc-reader

FROM root-planner AS root-source
COPY .cargo/config.toml ./.cargo/config.toml
# The canonical deploy DDL (sql/run-state.sql) is include_str!'d by
# publish-catalog's provisioning helpers — single source of truth, no clones.
COPY deploy ./deploy
# wamn-gates embeds the flowrunner dispatch source guard; copy only that file,
# not the component target.
COPY components/execution/flowrunner/src/lib.rs ./components/execution/flowrunner/src/lib.rs
# wash-runtime resolves as a git dep from the fork pinned in Cargo.toml
# (docs/archive/platform/wash-runtime-fork.md); cargo fetches it during the cook/build.
# rust-toolchain.toml is deliberately absent: the base image already ships the
# pinned Rust line, and copying it would force a rustup download in the image.

FROM cook-host AS build-host
COPY --from=root-source /build /build
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-host \
 && install -D -m 0755 target/release/wamn-host /native-output/wamn-host

FROM cook-executor AS build-executor
COPY --from=root-source /build /build
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-executor \
 && install -D -m 0755 target/release/wamn-run-worker /native-output/wamn-run-worker

FROM cook-scenario-worker AS build-scenario-worker
COPY --from=root-source /build /build
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-scenario-worker \
 && install -D -m 0755 target/release/wamn-scenario-worker /native-output/wamn-scenario-worker

FROM cook-ctl AS build-ctl
COPY --from=root-source /build /build
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-ctl \
 && cargo build --locked --release -p wamn-ctl --features ops --bin wamn-ctl-ops \
 && install -D -m 0755 target/release/wamn-ctl /native-output/wamn-ctl \
 && install -D -m 0755 target/release/wamn-ctl-ops /native-output/wamn-ctl-ops

FROM cook-dispatcher AS build-dispatcher
COPY --from=root-source /build /build
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-dispatcher \
 && install -D -m 0755 target/release/wamn-dispatcher /native-output/wamn-dispatcher

FROM cook-waker AS build-waker
COPY --from=root-source /build /build
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-waker \
 && install -D -m 0755 target/release/wamn-waker /native-output/wamn-waker

FROM cook-cdc-reader AS build-cdc-reader
COPY --from=root-source /build /build
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-cdc-reader \
 && install -D -m 0755 target/release/wamn-cdc-reader /native-output/wamn-cdc-reader

# The proof image is outside the retained MVP image set. It remains a separate,
# package-scoped build and reuses the same locked caches without adding an
# eighth production cook stage.
FROM root-source AS build-gates
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-gates \
 && install -D -m 0755 target/release/wamn-gates /native-output/wamn-gates

# ---- locked component outputs shared by every embedding image --------------
FROM chef AS component-toolchain
RUN rustup target add --toolchain 1.97.0 wasm32-wasip2
COPY .cargo/config.toml /build/.cargo/config.toml
COPY Cargo.toml /build/Cargo.toml
COPY crates /build/crates
COPY components /build/components
WORKDIR /build/components

FROM component-toolchain AS component-builder
RUN --mount=type=cache,id=wamn-component-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-component-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-component-target,target=/build/components/target,sharing=locked \
    cargo +1.97.0 build --locked --release --target wasm32-wasip2 \
      -p flow-http -p flowrunner -p materializer \
      -p busyloop -p connection-http-standard -p sockprobe \
 && install -d /component-output \
 && for artifact in \
      flow_http flowrunner materializer \
      busyloop connection_http_standard sockprobe; do \
      install -m 0644 "target/wasm32-wasip2/release/${artifact}.wasm" \
        "/component-output/${artifact}.wasm"; \
    done

# ---- washlet image: host only ----------------------------------------------
FROM debian:trixie-slim AS host
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-host /native-output/wamn-host /usr/local/bin/wamn-host
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-host"]

# ---- ctl image: the one-shot control-plane verbs (SR9) ----------------------
# NOTE pg_dump/pg_restore are NOT installed (parity with the pre-split image);
# dump/restore-project-env need a pg-client-equipped environment.
FROM debian:trixie-slim AS ctl
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-ctl /native-output/wamn-ctl /usr/local/bin/wamn-ctl
COPY --from=build-ctl /native-output/wamn-ctl-ops /usr/local/bin/wamn-ctl-ops
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-ctl"]

# ---- dispatcher image: the shared trigger dispatcher service (SR9) ----------
FROM debian:trixie-slim AS dispatcher
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-dispatcher /native-output/wamn-dispatcher /usr/local/bin/wamn-dispatcher
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-dispatcher"]

# ---- executor image: the production flow runner + its component (SR9) -------
# The deployment/image/binary identity remains wamn-run-worker; the owning
# source package is wamn-executor.
FROM debian:trixie-slim AS run-worker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-executor /native-output/wamn-run-worker /usr/local/bin/wamn-run-worker
# The flowrunner component is a PRODUCTION artifact, not a gate fixture: the
# run-worker (fqg.8) instantiates it to drive claimed runs, so it travels with
# this binary (default --flowrunner /components/flowrunner.wasm).
COPY --from=component-builder /component-output/flowrunner.wasm /components/flowrunner.wasm
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-run-worker"]

# ---- scenario-worker image: authoring management service -------------------
FROM debian:trixie-slim AS scenario-worker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-scenario-worker /native-output/wamn-scenario-worker /usr/local/bin/wamn-scenario-worker
# The MINTING pod's flowrunner source (wamn-0h0g.15.50). Draft validation pins a
# trusted runtime revision derived from the exact flowrunner bytes the host
# loaded, so the pod that mints that pin has to carry them locally — it cannot be
# handed them by a transport without pinning a digest that names no real
# executable. Per the wamn-0h0g.15.4 verdict flowrunner stays IN-IMAGE, so this is
# the same locked-builder artifact and the same stable path the run-worker above
# loads; the two images therefore agree on the digest by construction rather than
# by a skew detector.
COPY --from=component-builder /component-output/flowrunner.wasm /components/flowrunner.wasm
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-scenario-worker"]

# ---- cdc-reader image: the CDC event reader service (SR9) -------------------
FROM debian:trixie-slim AS cdc-reader
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-cdc-reader /native-output/wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-cdc-reader"]

# ---- waker image: the scale-to-zero wake actuator (fqg.12, POC-F3) ----------
# Watches the doorbell and scales a parked runner Deployment 0->1 via the k8s
# API. The ONE component granted k8s scale privilege (deploy/platform/waker.yaml).
FROM debian:trixie-slim AS waker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-waker /native-output/wamn-waker /usr/local/bin/wamn-waker
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-waker"]

# ---- gates image: the host stage + the gate suite + wasm fixtures -----------
FROM host AS gates
COPY --from=build-gates /native-output/wamn-gates /usr/local/bin/wamn-gates
# Control-plane integration proofs drive the deployable ctl artifact through its
# executable boundary; the proof packages do not link the service crate.
COPY --from=build-ctl /native-output/wamn-ctl /usr/local/bin/wamn-ctl
# Operations-only impact analysis crosses its own executable boundary.
COPY --from=build-ctl /native-output/wamn-ctl-ops /usr/local/bin/wamn-ctl-ops
# Reader-inclusive gates exercise the native CDC service through its executable
# boundary; the gates package does not link the service crate.
COPY --from=build-cdc-reader /native-output/wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
# Dispatcher gates drive stepped and lifecycle behavior through the executable
# boundary; the gates package does not link the deployable service crate.
COPY --from=build-dispatcher /native-output/wamn-dispatcher /usr/local/bin/wamn-dispatcher
# Metricbench drives run telemetry through the production executor boundary;
# the integration proof must not duplicate the executor-owned instruments.
COPY --from=build-executor /native-output/wamn-run-worker /usr/local/bin/wamn-run-worker
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
