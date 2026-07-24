# wamn

A wasmCloud-based managed low-code platform: a data/schema layer, a flow engine,
and a four-tier Postgres control plane, all hosted on a customized wasmCloud
runtime. **`docs/` is the design source of truth** — start with
`docs/platform-plan.md` and the decision table.

`services/host` is the production washlet host, while `services/node-host`
serves custom nodes. Both are thin deployable leaves over the reusable
`crates/platform/{runtime,node-runtime,component-policy}` packages. The
gate/bench suite is the separate `tests/orchestrator` binary over those reusable
packages. Our wash-runtime changes are carried commits on a fork — see
`docs/wash-runtime-fork.md`.

## Repository layout

```
services/               native deployable Rust services
  host                  production host: washlet embedding + host plugins
                        (wamn:postgres, logging, jetstream) — washlet only (SR9)
  node-host             custom-node HTTP/auth transport leaf
  ctl                   one-shot control-plane verbs (provision-*, publish/
                        migrate-catalog, dump/restore/copy-project-env,
                        enable-cdc-project-env) — SR9 split
  dispatcher            shared trigger dispatcher service (SR9 split)
  run-worker            production flow-runner service (SR9 split)
  cdc-reader            CDC event-reader service (SR9 split)
  waker                 scale-to-zero wake actuator
  builder               sandboxed custom-node build service

crates/                 shared Rust workspace packages
  # shared, non-deployable packages grouped by bounded context:
  platform/
    component-policy    pure component-import and grant policy
    runtime             shared engine, plugins, WIT, metrics, and test doubles
    node-runtime        transport-free warm custom-node runtime
    sql                 wamn-sql: parameterized SQL composition primitive
  data/
    entity-access       wamn-api: catalog-derived REST/SQL planning logic
  schema/
    model               wamn-catalog: metadata model + JSON Schema
    ddl-compiler        wamn-ddl: catalog -> Postgres DDL compiler
    lifecycle           wamn-schema: draft/staged/applied lifecycle + promotion
    rls-compiler        wamn-rls: per-role RLS policy builder
    seed-compiler       wamn-seed: typed datasets -> deterministic INSERTs
    migration           wamn-migrate: forward-only migration planner
    impact-analysis     wamn-impact: schema-change dependency analysis
  execution/
    flow-model          wamn-flow: flow-graph JSON model + JSON Schema
    flow-engine         wamn-runner: pure flow reducer
    run-state-store     wamn-run-store: durable run/node-run decisions
    run-state-queue     wamn-run-queue: queue, lease, and cron decisions
    standard-nodes      wamn-nodes: standard node library
  events/
    wire                wamn-event-wire: event envelope contract
    registration        wamn-event-reg: event registration model
    materializer        wamn-materializer: CDC materialization decisions
  node/
    sdk                 wamn-node-sdk: node authoring contract
    guest               wamn-node-guest: componentization scaffolding
    invoke              wamn-node-invoke: node invocation wire contract
    manifest            wamn-node-manifest: OCI annotation model
  control/
    registry            wamn-registry: org/project/environment model
    provision           wamn-provision: Postgres provisioning builders
  identity/
    project-state       wamn-sysschema: per-project app_system model
  scenarios/
    model               wamn-testkit: scenario case/assertion vocabulary
    catalog             wamn-flow-tests: persisted suite envelope

components/             wasm32-wasip2 guests
  ingress/              product ingress components (api-gateway)
  execution/            product execution components (flowrunner, materializer)
  fixtures/             non-product proof fixtures (flow-driver, hello, memhog,
                        busyloop, pgprobe, logspewer, trace-relay)
  poc/                  component POCs (webhook-f1)
  samples/              reference/sample nodes (node-rs, node-ts, sample-node)

poc/                    POC integration crates (f1, dm1, cdc1)

test-support/
  harness/              shared measurement helpers for gates

tests/
  orchestrator/         mixed-tier gate/bench suite binary (wamn-gates)

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
cargo build -p wamn-host -p wamn-node-host -p wamn-ctl -p wamn-dispatcher -p wamn-run-worker -p wamn-cdc-reader -p wamn-gates

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

**Gates** (the bench/fixture/proof triple, SR5) live in the `wamn-gates` package
at `tests/orchestrator` and assert against a real backend. The full per-bead
command set — local iteration and the in-cluster gate of record for each
subsystem — is in **`docs/build-and-test.md`**.
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

# 2. build the host, node-host, and gate images and load them into kind
docker build --target host  -t wamn-host:dev  .
docker build --target node-host -t wamn-node-host:dev .
docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev  --name wamn
kind load docker-image wamn-node-host:dev --name wamn
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
