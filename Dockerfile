# wamn images (SR1 pattern: one build, one final stage per artifact; SR9 split).
#   docker build --target host       -t wamn-host:dev       .  # washlet ONLY
#   docker build --target ctl        -t wamn-ctl:dev        .  # one-shot verbs
#   docker build --target dispatcher -t wamn-dispatcher:dev .  # trigger dispatcher
#   docker build --target run-worker -t wamn-run-worker:dev .  # flow runner (+flowrunner.wasm)
#   docker build --target cdc-reader -t wamn-cdc-reader:dev .  # CDC event reader
#   docker build --target waker      -t wamn-waker:dev      .  # scale-to-zero wake actuator
#   docker build --target gates      -t wamn-gates:dev      .  # gates: FROM host + suite + fixtures
#   docker build --target builder-svc -t wamn-builder:dev   .  # 5.5 node build sandbox (cargo+jco)
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
# wamn-gates testgate include_str!'s the disposition-node seed cases (11.5);
# only these two fixtures are needed, not the components workspace.
COPY components/samples/disposition-node/cases.json components/samples/disposition-node/cases-refusal-fixture.json ./components/samples/disposition-node/
# wash-runtime resolves as a git dep from the fork pinned in Cargo.toml
# (docs/wash-runtime-fork.md); cargo fetches it during the build.
# rust-toolchain.toml would force a rustup download inside the container;
# the base image already ships the right version.
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git rm rust-toolchain.toml && cargo build --release -p wamn-host -p wamn-ctl -p wamn-dispatcher -p wamn-run-worker -p wamn-cdc-reader -p wamn-waker -p wamn-gates -p wamn-builder

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

# ---- waker image: the scale-to-zero wake actuator (fqg.12, POC-F3) ----------
# Watches the doorbell and scales a parked runner Deployment 0->1 via the k8s
# API. The ONE component granted k8s scale privilege (deploy/platform/waker.yaml).
FROM debian:trixie-slim AS waker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/wamn-waker /usr/local/bin/wamn-waker
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-waker"]

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
# POC-F2 (wamn-1ab) zero-import disposition-recommendation node: the f2invoke
# gate warm-instantiates it in a ServeNode and calls it per disposition outcome.
COPY components/target/wasm32-wasip2/release/disposition_node.wasm /bench/disposition-node.wasm
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
# 11.4 assertion-library fixture: the checked-in Vec<TestCase> the testkitbench
# gate loads (the cases-as-data path). Static JSON, not a compiled artifact.
COPY deploy/gates/testkit-cases.json /bench/testkit-cases.json
# POC-TESTS (wamn-3rj): the F1/F3/F4 stored suite envelopes the pocsuiteproof
# gate seeds + drives. Static JSON, not compiled artifacts; every wasm this gate
# needs (poc-webhook-f1.wasm, flowrunner.wasm, disposition-node.wasm) is already
# baked above, so this gate adds NO host/guest rebuild — only these three files.
COPY deploy/gates/poc-f1-suite.json /bench/poc-f1-suite.json
COPY deploy/gates/poc-f3-suite.json /bench/poc-f3-suite.json
COPY deploy/gates/poc-f4-suite.json /bench/poc-f4-suite.json
ENTRYPOINT ["/usr/local/bin/wamn-gates"]

# ---- builder-svc image: the 5.5 node build sandbox (cargo + jco) ------------
# FROM the cargo-ful `builder` stage (rust:1.97-trixie, WORKDIR /build, the full
# repo source + the release target dir already cached), so `wamn-builder build`
# can run the toolchains itself at runtime: cargo (wasm32-wasip2 target added
# here) for a Rust cdylib node, jco for a JS/TS ES module. This is the ONLY
# cargo-ful runtime image; kept LAST so a `--target host/ctl/…` build never
# pulls the node toolchain in. Threat model (6.2): the Job runs this with no
# service-account token and an egress-deny NetworkPolicy — see
# deploy/platform/builder-job.yaml + builder-netpol.yaml.
FROM builder AS builder-svc
RUN rustup target add wasm32-wasip2 \
 && apt-get update && apt-get install -y --no-install-recommends nodejs npm ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && npm install -g @bytecodealliance/jco @bytecodealliance/componentize-js
# The v0 sandbox Job builds the baked-in components-workspace fixtures. The
# builder stage above copies only crates/poc/deploy, and its cargo caches are
# BuildKit mounts that do not persist into image layers — so copy the
# components source here (member dirs only, never the 3G components/target)
# and warm the crate cache into the image: the in-pod `cargo metadata
# --offline` and `cargo build` must run without network.
COPY components/Cargo.toml components/Cargo.lock ./components/
COPY components/api-gateway ./components/api-gateway
COPY components/fixtures ./components/fixtures
COPY components/flow-driver ./components/flow-driver
COPY components/flowrunner ./components/flowrunner
COPY components/materializer ./components/materializer
COPY components/poc-webhook-f1 ./components/poc-webhook-f1
COPY components/samples ./components/samples
RUN cd components && cargo fetch
# The compiled verb binary (built in the `builder` stage above) on PATH.
RUN cp /build/target/release/wamn-builder /usr/local/bin/wamn-builder
ENV HOME=/tmp
ENTRYPOINT ["/usr/local/bin/wamn-builder"]
