# wamn

A wasmCloud-based managed low-code platform: a data/schema layer, a flow engine,
and a four-tier Postgres control plane, all hosted on a customized wasmCloud
runtime. **`docs/` is the design source of truth** — start with
`docs/platform-plan.md` and the decision table.

`crates/wamn-host` is the production host (it embeds the `wash-runtime` washlet
and is deployed by the wasmCloud runtime-operator Helm chart); the gate/bench
suite is the separate `crates/wamn-gates` binary over the same lib. One
`Dockerfile` builds both via two `--target` stages (SR1). Our wash-runtime
changes are carried commits on a fork — see `docs/wash-runtime-fork.md`.

## Repository layout

```
crates/                 Rust workspace
  wamn-host             production host: washlet embedding + host plugins
                        (wamn:postgres, logging) + subcommands (host, dispatch,
                        provision-*, migrate-catalog, publish-catalog)
  wamn-gates            gate/bench suite binary (SR1 split)
  wamn-gate-harness     shared measurement helpers for gates

  # pure decision crates (no DB/clock/wasm — pure core / effect shell, SR6):
  wamn-catalog          metadata catalog model + JSON Schema
  wamn-ddl              catalog -> Postgres DDL compiler (tenant RLS floor)
  wamn-schema           schema draft/staged/applied lifecycle + promotion
  wamn-rls              per-role RLS policy builder
  wamn-seed             typed seed datasets -> deterministic INSERTs
  wamn-migrate          live forward-only migration engine
  wamn-flow             flow-graph JSON model + JSON Schema
  wamn-runner           pure flow reducer (walk, branch, retry, resume)
  wamn-run-store        durable runs/node_runs + branch-aware replay
  wamn-run-queue        durable run queue (SKIP LOCKED) + cron/outbox dispatch
  wamn-node-sdk         node authoring contract (Node trait, error taxonomy)
  wamn-node-guest       custom-node componentization scaffolding
  wamn-nodes            standard node library (transform, http, postgres, ...)
  wamn-node-manifest    wamn.node.manifest OCI annotation model
  wamn-api              REST API gateway logic (catalog -> routes/SQL)
  wamn-registry         control-plane registry model (org/project/env)
  wamn-provision        Postgres provisioning builders (clusters, DBs, backups)
  wamn-sysschema        per-project app_system (users/roles/...) model

components/             wasm32-wasip2 guests
  flowrunner            production flow-runner guest (drives wamn-runner)
  api-gateway           REST gateway guest (wasi:http + wamn:postgres)
  poc-webhook-f1        POC-F1 sync-webhook ingress
  flow-driver           node-composition driver
  fixtures/             bench fixtures (hello, memhog, busyloop, pgprobe,
                        logspewer, trace-relay)
  samples/              reference/sample nodes (node-rs, node-ts, sample-node)

poc/                    POC integration crates (f1, dm1)

deploy/                 Kubernetes manifests + standalone SQL schemas
  kind-config.yaml      local kind cluster definition
  values-wamn.yaml      runtime-operator Helm values (custom host image)
  *.sql                 postgres-init, catalog-schema, run-state, run-queue,
                        system-schema, app-schema, flows
  *-job.yaml            in-cluster gate-of-record Jobs

docs/                   design source of truth (platform-plan.md, decision
                        table, WIT contracts, per-subsystem specs)

Cargo.toml              root workspace; pins the wash-runtime fork rev
Dockerfile              two-stage image (--target host, --target gates)
```

## Prerequisites

- **Rust** (pinned by `rust-toolchain.toml`: 1.97.0, edition 2024) with the
  `wasm32-wasip2` target and `clippy`/`rustfmt` — installed automatically by
  `rustup` from the toolchain file.
- **protoc** (+ well-known-type includes) to build `wamn-host`.
- **Docker** for the image build and throwaway Postgres/NATS/etc. used by local
  gates.
- **kind**, **kubectl**, **helm** for the in-cluster gates.

## Develop

```bash
# host + gate suite (debug by default)
cargo build -p wamn-host -p wamn-gates

# wasm guests
(cd components && cargo build --release --target wasm32-wasip2)
```

## Test

```bash
# pure-crate unit/integration tests (no cluster needed)
cargo test                       # a specific crate: cargo test -p wamn-runner

# lint + format
cargo clippy --all-targets && cargo fmt --check
```

Many crates also have optional live-apply tests that run against a throwaway
Postgres and skip when their `WAMN_*_PG_URL` env var is unset.

**Gates** (the bench/fixture/proof triple, SR5) live in `wamn-gates` and assert
against a real backend. The full per-bead command set — local iteration and the
in-cluster gate of record for each subsystem — is in **`docs/build-and-test.md`**.
Example (S1, no backend):

```bash
./target/release/wamn-gates --log-level warn bench \
  --hello    components/target/wasm32-wasip2/release/hello.wasm \
  --memhog   components/target/wasm32-wasip2/release/memhog.wasm \
  --busyloop components/target/wasm32-wasip2/release/busyloop.wasm
```

## Deploy (in-cluster)

The in-cluster gate of record runs on a local `kind` cluster named `wamn`,
with the host + gate images built from the two-stage `Dockerfile`:

```bash
# 1. stand up the cluster + wasmCloud runtime-operator
kind create cluster --name wamn --config deploy/infra/kind-config.yaml
helm upgrade --install -n wamn-system wamn \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.5.2 \
  -f deploy/infra/values-wamn.yaml

# 2. build both images and load them into kind
docker build --target host  -t wamn-host:dev  .
docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev  --name wamn
kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system rollout status deploy/hostgroup-default

# 3. apply the manifests / gate Jobs for the subsystem under test
#    (see docs/build-and-test.md for the exact per-bead steps)
kubectl -n wamn-system apply -f deploy/<subsystem>-job.yaml
kubectl -n wamn-system logs -f job/<subsystem>
```

## More

- `docs/` — design source of truth (per-subsystem specs, WIT contracts).
- `docs/build-and-test.md` — every subsystem's build + gate commands.
- `CLAUDE.md` / `AGENTS.md` — instructions for AI coding agents (identical).
