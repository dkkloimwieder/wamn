# wamn images (SR1 pattern: one build, one final stage per artifact; SR9 split).
#   cargo xtask build-platform --registry <registry> --out <chart directory>
# For the edge box artifact, use --target edge with a checkout cache key.
# `cargo xtask build-platform` builds host, ctl, identity and edge-web, and
# stamps their digests into the release chart's defaults (R15).
# Later invocations reuse shared BuildKit registry and Git caches with locks
# that prevent concurrent writes, and one locked native target cache per checkout.
# One build-native stage compiles all native packages in one invocation. The
# host executable links no provisioning or replication credential code.
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
COPY wit ./wit
# Native packages use shared guest libraries under apps/platform.
COPY apps ./apps
COPY services ./services
COPY test-support ./test-support
COPY tests ./tests
# xtask and tools/submodule-pins are workspace members, so Cargo needs
# their manifests to load the workspace.
COPY xtask ./xtask
COPY tools/submodule-pins ./tools/submodule-pins
# The copied .cargo/config.toml selects clang/mold, limits build jobs, and
# enables offline SQLx. It sets no rustc-wrapper, so this stage and
# component-toolchain need no wrapper binary. Local Cargo uses kache through
# the personal ~/.cargo/config.toml.
COPY .cargo/config.toml ./.cargo/config.toml
# The canonical deploy DDL is consumed by the ctl reconcilers.
COPY deploy ./deploy
# wash-runtime resolves as a git dependency at the zero-delta fork revision
# recorded in Cargo.toml and docs/architecture/native-alignment.md;
# cargo fetches it during the build.
# rust-toolchain.toml is deliberately absent: the base image already ships the
# pinned Rust line, and copying it would force a rustup download in the image.

# The target cache is keyed by checkout, so a bump from one checkout reuses
# its compiled dependencies and no other checkout overwrites them.
FROM root-source AS build-native
ARG WAMN_CHECKOUT_KEY
RUN --mount=type=cache,id=wamn-root-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=wamn-root-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=wamn-root-target-native-${WAMN_CHECKOUT_KEY},target=/build/target,sharing=locked \
    test -n "$WAMN_CHECKOUT_KEY" \
 && cargo build --locked --release -p wamn-host -p wamn-runtime -p wamn-ctl -p wamn-cdc-reader -p wamn-identity \
 && cargo build --locked --release -p wamn-ctl --features ops --bin wamn-ctl-ops \
 && install -D -m 0755 target/release/wamn-host /native-output/wamn-host \
 && install -D -m 0755 target/release/docker-credential-wamn /native-output/docker-credential-wamn \
 && install -D -m 0755 target/release/wamn-ctl /native-output/wamn-ctl \
 && install -D -m 0755 target/release/wamn-ctl-ops /native-output/wamn-ctl-ops \
 && install -D -m 0755 target/release/wamn /native-output/wamn \
 && install -D -m 0755 target/release/wamn-cdc-reader /native-output/wamn-cdc-reader \
 && install -D -m 0755 target/release/wamn-identity /native-output/wamn-identity

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
    --mount=type=cache,id=wamn-root-target-native-${WAMN_CHECKOUT_KEY},target=/build/target,sharing=locked \
    test -n "$WAMN_CHECKOUT_KEY" \
 && cargo build --locked --release -p wamn-web-front \
 && install -D -m 0755 target/release/wamn-web-front /native-output/wamn-web-front

FROM debian:trixie-slim AS edge-web
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-edge-web /native-output/wamn-web-front /usr/local/bin/wamn-web-front
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-web-front"]

# ---- combined HTTP and durable-queue host image ----------------------------
FROM debian:trixie-slim AS host
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-native /native-output/wamn-host /usr/local/bin/wamn-host
COPY --from=build-native /native-output/wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
# The Docker credential helper wash-runtime runs for a registry that the
# mounted config.json names under credHelpers (docs/plan/component-pull.md).
COPY --from=build-native /native-output/docker-credential-wamn /usr/local/bin/docker-credential-wamn
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-host"]

# The worker uses server-selected metadata, so a version bump changes no image bytes.
FROM root-source AS ctl-deploy
RUN rm /build/deploy/platform/release/Chart.yaml

# ---- ctl image: the one-shot control-plane verbs (SR9) ----------------------
FROM debian:trixie-slim AS ctl
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-native /native-output/wamn-ctl /usr/local/bin/wamn-ctl
COPY --from=components /http.wasm /usr/local/share/wamn/http_route.wasm
COPY --from=build-native /native-output/wamn-ctl-ops /usr/local/bin/wamn-ctl-ops
# `wamn-ctl env` applies manifests with kubectl, pinned by version and its
# published sha256 (wamn-zua8.3). wamn-iggi moves the pin to the GKE minor.
ADD --chmod=0755 --checksum=sha256:629d3f410e09bf49b64ae7079f7f0bda1191efed311f7d37fdbab0ad5b0ec2b7 https://dl.k8s.io/release/v1.36.1/bin/linux/amd64/kubectl /usr/local/bin/kubectl
ARG HELM_URL
ARG HELM_CHECKSUM
ADD --checksum=${HELM_CHECKSUM} ${HELM_URL} /tmp/helm.tgz
ARG KCL_URL
ARG KCL_CHECKSUM
ADD --checksum=${KCL_CHECKSUM} ${KCL_URL} /tmp/kcl.tgz
RUN tar -xzf /tmp/helm.tgz -C /usr/local/bin --strip-components=1 linux-amd64/helm \
 && tar -xzf /tmp/kcl.tgz -C /usr/local/bin kcl \
 && rm /tmp/helm.tgz /tmp/kcl.tgz
COPY --from=build-native /native-output/docker-credential-wamn /usr/local/bin/docker-credential-wamn
ENV WAMN_KCL=/usr/local/bin/kcl
COPY --from=ctl-deploy /build/deploy /usr/local/share/wamn/deploy
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-ctl"]

# ---- identity image: separate signing authority and public JWKS ------------
FROM debian:trixie-slim AS identity
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-native /native-output/wamn-identity /usr/local/bin/wamn-identity
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-identity"]

# ---- edge box artifact: the stripped aarch64 binary alone ------------------
# The box runs no container; export the file with --output (header above).
FROM scratch AS edge
COPY --from=build-edge /native-output/wamn-edge /wamn-edge
