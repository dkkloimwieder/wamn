# wamn images (SR1 pattern: one build, one final stage per artifact; SR9 split).
#   cargo xtask build-platform --registry <registry> --out <chart directory>
# For the edge box artifact, use --target edge with a checkout cache key.
# `cargo xtask build-platform` builds host, ctl, identity and edge-web, and
# stamps their digests into the release chart's defaults (R15).
# Later invocations reuse shared BuildKit registry and Git caches with locks
# that prevent concurrent writes, and one locked native target cache per checkout.
# One build-native stage compiles all native packages in one invocation. The
# host executable links no provisioning or replication credential code.
FROM rust:1.100-trixie AS toolchain
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
COPY mise.toml ./
COPY crates ./crates
COPY wit ./wit
# Both workspaces consume guest libraries and binding sources.
COPY apps ./apps
COPY guests ./guests
COPY services ./services
COPY test-support ./test-support
COPY tests ./tests
# xtask is a workspace member, so Cargo needs
# its manifest to load the workspace.
COPY xtask ./xtask
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
 && cargo build --locked --release -p wamn-host -p wamn-control -p wamn-oci -p wamn-cli -p wamn-cdc-reader -p wamn-identity-service \
 && install -D -m 0755 target/release/wamn-host /native-output/wamn-host \
 && install -D -m 0755 target/release/wamn-control /native-output/wamn-control \
 && install -D -m 0755 target/release/docker-credential-wamn /native-output/docker-credential-wamn \
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
 && cargo build --locked --release -p wamn-web \
 && install -D -m 0755 target/release/wamn-web /native-output/wamn-web

FROM debian:trixie-slim AS edge-web
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build-edge-web /native-output/wamn-web /usr/local/bin/wamn-web
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-web"]

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

# Workers use the prepared charts included in the selected image.
FROM root-source AS ctl-deploy

# ---- ctl image: the one-shot control-plane verbs (SR9) ----------------------
FROM debian:trixie-slim AS ctl
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=components /ingress.wasm /usr/local/share/wamn/ingress.wasm
COPY --from=components /palette/ /usr/local/share/wamn/
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

# ---- native org control service and its operation worker -----------------
FROM ctl AS control
COPY --from=build-native /native-output/wamn-control /usr/local/bin/wamn-control
ENTRYPOINT ["/usr/local/bin/wamn-control"]

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
