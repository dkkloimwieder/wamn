# wamn images (SR1 pattern: one build, one final stage per artifact; SR9 split).
#   docker build --target host       -t wamn-host:dev       .  # HTTP + durable queue host
#   docker build --target ctl        -t wamn-ctl:dev        .  # one-shot verbs
#   docker build --target scenario-worker -t wamn-scenario-worker:dev . # authoring management
#   docker build --target cdc-reader -t wamn-cdc-reader:dev .  # CDC event reader
#   docker build --target identity   -t wamn-identity:dev   .  # identity authority and JWKS
#   docker build --target gates      -t wamn-gates:dev      .  # gates: FROM host + suite + fixtures
#   docker build --target edge --output type=local,dest=target/edge .  # aarch64 edge box binary
# Later invocations reuse shared BuildKit registry and Git caches and one
# locked target cache per build stage, so the stages no longer wait on each
# other. Each retained native image builds only its top-level package. The
# host artifact ships no provisioning / replication-credential / gate code
# (SR9 strings spot-check); the gates image layers the suite on top of the
# IDENTICAL host stage so Jobs exercise the same host lib code they verify.
FROM rust:1.98-trixie AS toolchain
# libprotobuf-dev carries the well-known types (google/protobuf/*.proto)
# that protobuf-compiler alone does not ship on Debian.
RUN apt-get update && apt-get install -y --no-install-recommends clang mold protobuf-compiler libprotobuf-dev git && rm -rf /var/lib/apt/lists/*
WORKDIR /build

# One source stage feeds every native build stage.
#
# There were eight cargo-chef cook stages here. A cook writes its dependency
# build into the SAME cache mount its build stage reads, not into a layer, so
# it cached nothing that survived the stage; it only took the one lock every
# other stage waited on, and deleted the workspace rlibs a neighbour had just
# written. One target cache per build stage replaces all of it (wamn-szr0).
FROM toolchain AS root-source
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# Native packages use shared guest libraries under apps/platform.
COPY apps ./apps
COPY services ./services
COPY test-support ./test-support
COPY tests ./tests
# The copied .cargo/config.toml carries the clang/mold linker settings the
# toolchain stage installs above, and NO rustc-wrapper: this stage and
# component-toolchain both inherit this file, and a wrapper naming a binary
# neither of them installs fails the build at `<wrapper> rustc -vV`. Keep
# sccache a per-developer RUSTC_WRAPPER setting -- see .cargo/config.toml.
COPY .cargo/config.toml ./.cargo/config.toml
# The canonical deploy DDL is consumed by the ctl reconcilers and exact package
# runner -- single source of truth, no clones.
COPY deploy ./deploy
# wash-runtime resolves as a git dependency at the zero-delta fork revision
# recorded in Cargo.toml and docs/architecture/native-alignment.md;
# cargo fetches it during the build.
# rust-toolchain.toml is deliberately absent: the base image already ships the
# pinned Rust line, and copying it would force a rustup download in the image.

FROM root-source AS build-host
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=shared \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=shared \
    --mount=type=cache,id=wamn-root-target-host,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-host \
 && install -D -m 0755 target/release/wamn-host /native-output/wamn-host

FROM root-source AS build-scenario-worker
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=shared \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=shared \
    --mount=type=cache,id=wamn-root-target-scenario-worker,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-scenario-worker \
 && install -D -m 0755 target/release/wamn-scenario-worker /native-output/wamn-scenario-worker

FROM root-source AS build-ctl
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=shared \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=shared \
    --mount=type=cache,id=wamn-root-target-ctl,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-ctl \
 && cargo build --locked --release -p wamn-ctl --features ops --bin wamn-ctl-ops \
 && install -D -m 0755 target/release/wamn-ctl /native-output/wamn-ctl \
 && install -D -m 0755 target/release/wamn-ctl-ops /native-output/wamn-ctl-ops

FROM root-source AS build-cdc-reader
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=shared \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=shared \
    --mount=type=cache,id=wamn-root-target-cdc-reader,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-cdc-reader \
 && install -D -m 0755 target/release/wamn-cdc-reader /native-output/wamn-cdc-reader

FROM root-source AS build-identity
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=shared \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=shared \
    --mount=type=cache,id=wamn-root-target-identity,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-identity \
 && install -D -m 0755 target/release/wamn-identity /native-output/wamn-identity

# The test image is outside the retained MVP image set. It remains a
# separate, package-scoped build with its own target cache.
FROM root-source AS build-gates
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=shared \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=shared \
    --mount=type=cache,id=wamn-root-target-gates,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-gates \
 && install -D -m 0755 target/release/wamn-gates /native-output/wamn-gates

# ---- edge box binary: aarch64 release, cross-compiled ----------------------
# docs/plan/edge.md 4.10 option A. The C code in aws-lc-sys and libsqlite3-sys
# needs the Debian cross compiler; the Rust linker is the same compiler.
FROM root-source AS build-edge
RUN apt-get update && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross && rm -rf /var/lib/apt/lists/* \
 && rustup target add aarch64-unknown-linux-gnu
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=shared \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=shared \
    --mount=type=cache,id=wamn-root-target-edge,target=/build/target,sharing=locked \
    cargo build --locked --release -p wamn-edge --target aarch64-unknown-linux-gnu \
 && mkdir -p /native-output \
 && aarch64-linux-gnu-strip -o /native-output/wamn-edge target/aarch64-unknown-linux-gnu/release/wamn-edge

# ---- locked component outputs shared by every embedding image --------------
FROM toolchain AS component-toolchain
RUN rustup target add --toolchain 1.98.1 wasm32-wasip2 \
 && rustup toolchain install 1.98.1 --profile minimal --target wasm32-wasip2
COPY .cargo/config.toml /build/.cargo/config.toml
COPY Cargo.toml /build/Cargo.toml
COPY crates /build/crates
COPY apps /build/apps
COPY tools/guest-rustflags /build/tools/guest-rustflags
WORKDIR /build/apps

# tools/guest-rustflags maps /build to /wamn and the Cargo home to /cargo, so
# each guest here has the digest of the same guest built by tools/build-components.
# The router must match the pin beside its crate, as in tools/build-components.
FROM component-toolchain AS component-builder
RUN --mount=type=cache,id=wamn-component-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-component-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-component-target,target=/build/apps/target,sharing=locked \
    export RUSTFLAGS="$(/build/tools/guest-rustflags)" \
 && cargo +1.98.1 build --locked --release --target wasm32-wasip2 \
      -p http-route \
 && (cd target/wasm32-wasip2/release \
     && sha256sum -c /build/apps/platform/ingress/http-route/http_route.wasm.sha256) \
 && cargo +1.98.1 build --locked --release --target wasm32-wasip2 \
      -p materializer \
 && cargo +1.98.1 build --locked --release --target wasm32-wasip2 \
      -p busyloop \
 && cargo +1.98.1 build --locked --release --target wasm32-wasip2 \
      -p connection-http-standard \
 && cargo +1.98.1 build --locked --release --target wasm32-wasip2 \
      -p sockprobe \
 && install -d /component-output \
 && for artifact in \
      http_route materializer \
      busyloop connection_http_standard sockprobe; do \
      install -m 0644 "target/wasm32-wasip2/release/${artifact}.wasm" \
        "/component-output/${artifact}.wasm"; \
    done

# ---- combined HTTP and durable-queue host image ----------------------------
FROM debian:trixie-slim AS host
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-host /native-output/wamn-host /usr/local/bin/wamn-host
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-host"]

# ---- ctl image: the one-shot control-plane verbs (SR9) ----------------------
# NOTE pg_dump and pg_restore are NOT installed (parity with the pre-split
# image). Run copy-project-env from a pg-client-equipped environment.
FROM debian:trixie-slim AS ctl
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-ctl /native-output/wamn-ctl /usr/local/bin/wamn-ctl
COPY --from=build-ctl /native-output/wamn-ctl-ops /usr/local/bin/wamn-ctl-ops
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-ctl"]

# ---- scenario-worker image: authoring management service -------------------
FROM debian:trixie-slim AS scenario-worker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-scenario-worker /native-output/wamn-scenario-worker /usr/local/bin/wamn-scenario-worker
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-scenario-worker"]

# ---- cdc-reader image: the CDC event reader service (SR9) -------------------
FROM debian:trixie-slim AS cdc-reader
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-cdc-reader /native-output/wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-cdc-reader"]

# ---- identity image: separate signing authority and public JWKS ------------
FROM debian:trixie-slim AS identity
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-identity /native-output/wamn-identity /usr/local/bin/wamn-identity
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-identity"]

# ---- gates image: the host stage + the gate suite + wasm fixtures -----------
FROM host AS gates
COPY --from=build-gates /native-output/wamn-gates /usr/local/bin/wamn-gates
# Control-plane integration tests drive the deployable ctl artifact through its
# executable boundary; the test packages do not link the service crate.
COPY --from=build-ctl /native-output/wamn-ctl /usr/local/bin/wamn-ctl
# Operations-only impact analysis crosses its own executable boundary.
COPY --from=build-ctl /native-output/wamn-ctl-ops /usr/local/bin/wamn-ctl-ops
# Reader-inclusive gates exercise the native CDC service through its executable
# boundary; the gates package does not link the service crate.
COPY --from=build-cdc-reader /native-output/wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
# Test fixtures baked in so the retained gates run with no volume plumbing.
COPY --from=component-builder /component-output/busyloop.wasm /bench/busyloop.wasm
COPY --from=component-builder /component-output/sockprobe.wasm /bench/sockprobe.wasm
# Callable-flow HTTP ingress: bounded routing/auth/mapping adapter over the
# native wamn:router-delivery provider contract.
COPY --from=component-builder /component-output/http_route.wasm /bench/http-route.wasm
# l5i9.17 materializer Service guest (wasi:cli/run; imports wamn:postgres +
# wamn:jetstream; the matbench gate drives it via CommandPre — the same wasm the
# WorkloadDeployment pulls from the registry in production).
COPY --from=component-builder /component-output/materializer.wasm /bench/materializer.wasm
COPY --from=component-builder /component-output/connection_http_standard.wasm /bench/connection-http-standard.wasm
ENTRYPOINT ["/usr/local/bin/wamn-gates"]

# ---- edge box artifact: the stripped aarch64 binary alone ------------------
# The box runs no container; export the file with --output (header above).
FROM scratch AS edge
COPY --from=build-edge /native-output/wamn-edge /wamn-edge

FROM toolchain AS cranelift-dev
# Opt-in native debug shell only. No shipping stage inherits this toolchain.
RUN rustup toolchain install nightly --profile minimal \
 && rustup component add rustc-codegen-cranelift-preview --toolchain nightly
COPY --chmod=0755 tools/cargo-cranelift /usr/local/bin/cargo-cranelift
WORKDIR /workspace
