# wamn images (SR1 pattern: one build, one final stage per artifact; SR9 split).
#   docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target host       -t wamn-host:dev       .  # HTTP + durable queue host
#   docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target ctl        -t wamn-ctl:dev        .  # one-shot verbs
#   docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target scenario-worker -t wamn-scenario-worker:dev . # authoring management
#   docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target cdc-reader -t wamn-cdc-reader:dev .  # CDC event reader
#   docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target identity   -t wamn-identity:dev   .  # identity authority and JWKS
#   docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target gates      -t wamn-gates:dev      .  # gates: FROM host + suite + fixtures
#   docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target edge --output type=local,dest=target/edge .  # aarch64 edge box binary
#   docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target roles --output type=local,dest=DIR .  # http and materializer role components
#   docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --build-context apps-target=apps/target/wamn --target runner -t wamn-runner:dev .  # qualification runner: FROM ctl + the case binaries
# `wamn-ctl build-platform` builds host, runner, gates, identity and roles, and
# stamps their digests into the release chart's defaults (R15).
# Later invocations reuse shared BuildKit registry and Git caches with locks
# that prevent concurrent writes, and one shared locked native target cache.
# One build-native stage compiles all native packages in one invocation. The
# host artifact ships no provisioning / replication-credential / gate code
# (SR9 strings spot-check); the gates image layers the suite on top of the
# IDENTICAL host stage so Jobs exercise the same host lib code they verify.
FROM rust:1.99-trixie AS toolchain
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
# written. The native builds now share one stage and target cache.
FROM toolchain AS root-source
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY vendor ./vendor
# Native packages use shared guest libraries under apps/platform.
COPY apps ./apps
COPY services ./services
COPY test-support ./test-support
COPY tests ./tests
# tools/bench is a workspace member, so Cargo needs its manifest to load the
# workspace.
COPY tools/bench ./tools/bench
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

FROM root-source AS build-native
ARG WAMN_CHECKOUT_KEY
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target-native,target=/build/target,sharing=locked \
    test -n "$WAMN_CHECKOUT_KEY" \
 && cargo build --locked --release -p wamn-host -p wamn-runtime -p wamn-ctl -p wamn-scenario-worker -p wamn-cdc-reader -p wamn-identity -p wamn-gates -p wamn-component-composer \
 && cargo build --locked --release -p wamn-ctl --features ops --bin wamn-ctl-ops \
 && install -D -m 0755 target/release/wamn-host /native-output/wamn-host \
 && install -D -m 0755 target/release/docker-credential-wamn /native-output/docker-credential-wamn \
 && install -D -m 0755 target/release/wamn-ctl /native-output/wamn-ctl \
 && install -D -m 0755 target/release/wamn-ctl-ops /native-output/wamn-ctl-ops \
 && install -D -m 0755 target/release/wamn /native-output/wamn \
 && install -D -m 0755 target/release/wamn-scenario-worker /native-output/wamn-scenario-worker \
 && install -D -m 0755 target/release/wamn-cdc-reader /native-output/wamn-cdc-reader \
 && install -D -m 0755 target/release/wamn-identity /native-output/wamn-identity \
 && install -D -m 0755 target/release/wamn-component-composer /native-output/wamn-component-composer \
 && install -D -m 0755 target/release/wamn-gates /native-output/wamn-gates

# ---- edge box binary: aarch64 release, cross-compiled ----------------------
# docs/history/edge.md 4.10 option A. The C code in aws-lc-sys and libsqlite3-sys
# needs the Debian cross compiler; the Rust linker is the same compiler.
FROM root-source AS build-edge
ARG WAMN_CHECKOUT_KEY
RUN apt-get update && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross && rm -rf /var/lib/apt/lists/* \
 && rustup target add aarch64-unknown-linux-gnu
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target-edge-${WAMN_CHECKOUT_KEY},target=/build/target,sharing=locked \
    test -n "$WAMN_CHECKOUT_KEY" \
 && cargo build --locked --release -p wamn-edge --target aarch64-unknown-linux-gnu \
 && mkdir -p /native-output \
 && aarch64-linux-gnu-strip -o /native-output/wamn-edge target/aarch64-unknown-linux-gnu/release/wamn-edge

# ---- cluster web edge: linux/amd64, separate from the edge box target -------
FROM root-source AS build-edge-web
ARG WAMN_CHECKOUT_KEY
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target-native,target=/build/target,sharing=locked \
    test -n "$WAMN_CHECKOUT_KEY" \
 && cargo build --locked --release -p wamn-edge --features web \
 && install -D -m 0755 target/release/wamn-edge /native-output/wamn-edge

FROM debian:trixie-slim AS edge-web
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-edge-web /native-output/wamn-edge /usr/local/bin/wamn-edge
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-edge"]

# ---- locked component outputs shared by every embedding image --------------
FROM toolchain AS component-toolchain
RUN rustup target add --toolchain 1.99.0 wasm32-wasip2 \
 && rustup toolchain install 1.99.0 --profile minimal --target wasm32-wasip2
COPY .cargo/config.toml /build/.cargo/config.toml
COPY Cargo.toml /build/Cargo.toml
COPY crates /build/crates
COPY vendor /build/vendor
COPY apps /build/apps
COPY tools/guest-rustflags /build/tools/guest-rustflags
WORKDIR /build/apps

# tools/guest-rustflags maps /build to /wamn and the Cargo home to /cargo, so
# each guest here has the digest of the same guest built by wamn build.
# The router must match the pin beside its crate, as in wamn build.
FROM component-toolchain AS component-builder
ARG WAMN_CHECKOUT_KEY
RUN --mount=type=cache,id=wamn-component-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-component-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-component-target-${WAMN_CHECKOUT_KEY},target=/build/apps/target,sharing=locked \
    test -n "$WAMN_CHECKOUT_KEY" \
 && export RUSTFLAGS="$(/build/tools/guest-rustflags)" \
 && cargo +1.99.0 build --locked --release --target wasm32-wasip2 \
      -p http-route \
 && (cd target/wasm32-wasip2/release \
     && sha256sum -c /build/apps/platform/ingress/http-route/http_route.wasm.sha256) \
 && cargo +1.99.0 build --locked --release --target wasm32-wasip2 \
      -p materializer \
 && cargo +1.99.0 build --locked --release --target wasm32-wasip2 \
      -p busyloop \
 && cargo +1.99.0 build --locked --release --target wasm32-wasip2 \
      -p connection-http-standard \
 && cargo +1.99.0 build --locked --release --target wasm32-wasip2 \
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
COPY --from=build-native /native-output/wamn-host /usr/local/bin/wamn-host
# The Docker credential helper wash-runtime runs for a registry that the
# mounted config.json names under credHelpers (docs/plan/component-pull.md).
COPY --from=build-native /native-output/docker-credential-wamn /usr/local/bin/docker-credential-wamn
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-host"]

# ---- ctl image: the one-shot control-plane verbs (SR9) ----------------------
FROM debian:trixie-slim AS ctl
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-native /native-output/wamn-ctl /usr/local/bin/wamn-ctl
COPY --from=component-builder /component-output/http_route.wasm /usr/local/share/wamn/http_route.wasm
COPY --from=build-native /native-output/wamn-ctl-ops /usr/local/bin/wamn-ctl-ops
# The ctl image carries the wiring gate program beside the verbs (wamn-zua8.3).
COPY --from=build-native /native-output/wamn-scenario-worker /usr/local/bin/wamn-scenario-worker
# `wamn-ctl env` applies manifests with kubectl, pinned by version and its
# published sha256 (wamn-zua8.3). wamn-iggi moves the pin to the GKE minor.
ADD --chmod=0755 --checksum=sha256:629d3f410e09bf49b64ae7079f7f0bda1191efed311f7d37fdbab0ad5b0ec2b7 https://dl.k8s.io/release/v1.36.1/bin/linux/amd64/kubectl /usr/local/bin/kubectl
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-ctl"]

# ---- scenario-worker image: authoring management service -------------------
FROM debian:trixie-slim AS scenario-worker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-native /native-output/wamn-scenario-worker /usr/local/bin/wamn-scenario-worker
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-scenario-worker"]

# ---- cdc-reader image: the CDC event reader service (SR9) -------------------
FROM debian:trixie-slim AS cdc-reader
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-native /native-output/wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-cdc-reader"]

# ---- identity image: separate signing authority and public JWKS ------------
FROM debian:trixie-slim AS identity
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-native /native-output/wamn-identity /usr/local/bin/wamn-identity
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-identity"]

# ---- gates image: the host stage + the gate suite + wasm fixtures -----------
FROM host AS gates
COPY --from=build-native /native-output/wamn-gates /usr/local/bin/wamn-gates
# Control-plane integration tests drive the deployable ctl artifact through its
# executable boundary; the test packages do not link the service crate.
COPY --from=build-native /native-output/wamn-ctl /usr/local/bin/wamn-ctl
# Operations-only impact analysis crosses its own executable boundary.
COPY --from=build-native /native-output/wamn-ctl-ops /usr/local/bin/wamn-ctl-ops
# Reader-inclusive gates exercise the native CDC service through its executable
# boundary; the gates package does not link the service crate.
COPY --from=build-native /native-output/wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
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

# Compose the control-only platform entry after the route pin is checked.
FROM component-builder AS control-entry
COPY --from=build-native /native-output/wamn-component-composer /usr/local/bin/wamn-component-composer
RUN wamn-component-composer --control-route /component-output/http_route.wasm --output /component-output/control_http.wasm

# ---- role components: the http and materializer guests alone -------------
# `wamn-ctl build-platform` exports them with --output and pushes each as a
# role component of the platform image set (R15).
FROM scratch AS roles
COPY --from=control-entry /component-output/control_http.wasm /http_route.wasm
COPY --from=component-builder /component-output/materializer.wasm /materializer.wasm

# ---- qualification cases: the cluster case executables ----------------------
# The cases compile against the package outputs of `wamn build`, which the
# build takes as the named context apps-target:
#   docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --build-context apps-target=apps/target/wamn --target runner .
# Each executable is installed as /cases/<package>, and keeps reading the
# repository files it names from /build, where the runner carries them.
FROM root-source AS build-cases
ARG WAMN_CHECKOUT_KEY
COPY --from=apps-target . /build/apps/target/wamn
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target-cases-${WAMN_CHECKOUT_KEY},target=/build/target,sharing=locked \
    test -n "$WAMN_CHECKOUT_KEY" \
 && cargo test --locked --no-run --message-format=json --features cluster \
      -p wamn-wms-tests -p wamn-receiving-tests > /tmp/cases.json \
 && install -d /cases \
 && for package in wamn-wms-tests wamn-receiving-tests; do \
      target="$(printf '%s' "$package" | tr - _)"; \
      executable="$(grep '"reason":"compiler-artifact"' /tmp/cases.json \
        | grep "\"name\":\"$target\"" \
        | grep -o '"executable":"[^"]*"' | cut -d'"' -f4)"; \
      test "$(printf '%s\n' "$executable" | grep -c .)" -eq 1 \
        && install -m 0755 "$executable" "/cases/$package" || exit 1; \
    done

# ---- runner image: the qualification runner FROM ctl -----------------------
# ctl is unchanged; the runner adds what `qualify-release` runs the cases with
# (docs/plan/platform-deploy.md §8.2, R15): the case executables, the
# repository files they read, and the cluster tools, each pinned by version
# and its published sha256 (the Docker CLI by the sha256 of its archive).
FROM ctl AS runner
COPY --from=build-native /native-output/wamn /usr/local/bin/wamn
ADD --checksum=sha256:5c0d19f31fece1accd0358bb8cff591fe25d7b6cba19f0fd412cbfdc07f75ff6 https://download.docker.com/linux/static/stable/x86_64/docker-28.5.1.tgz /tmp/docker.tgz
ADD --checksum=sha256:9adafecab4d406853bba163a70e9f104f47dbbf65ce24b7653bae7e36150bcb6 https://get.helm.sh/helm-v4.2.2-linux-amd64.tar.gz /tmp/helm.tgz
ADD --checksum=sha256:1cb3490c4df4c823ae6b76289797791c1077611f677d2b3a3d0bc292dc704f2e https://github.com/kcl-lang/cli/releases/download/v0.13.0/kcl-v0.13.0-linux-amd64.tar.gz /tmp/kcl.tgz
RUN tar -xzf /tmp/docker.tgz -C /usr/local/bin --strip-components=1 docker/docker \
 && tar -xzf /tmp/helm.tgz -C /usr/local/bin --strip-components=1 linux-amd64/helm \
 && tar -xzf /tmp/kcl.tgz -C /usr/local/bin kcl \
 && rm /tmp/docker.tgz /tmp/helm.tgz /tmp/kcl.tgz
ADD --chmod=0755 --checksum=sha256:50030de23cf40a18505f20426f6a8506bedf13c6e509244bd1fa9463721b0f54 https://github.com/kubernetes-sigs/kind/releases/download/v0.32.0/kind-linux-amd64 /usr/local/bin/kind
# The cases run kubectl v1.36.2; ctl's own pin stays as it is.
ADD --chmod=0755 --checksum=sha256:1e9045ec32bea85da43de85f0065358529ea7c7a152eca78154fba5b58c27d82 https://dl.k8s.io/release/v1.36.2/bin/linux/amd64/kubectl /usr/local/bin/kubectl
COPY --from=build-cases /cases /cases
COPY --from=root-source /build/apps /build/apps
COPY --from=apps-target . /build/apps/target/wamn
COPY --from=root-source /build/deploy /build/deploy
COPY tools /build/tools

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
