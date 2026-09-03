# wamn

A wasmCloud-based managed low-code platform: a data/schema layer, wiring execution,
and a four-tier Postgres control plane, all hosted on a customized wasmCloud
runtime. **`docs/exe-model.md` is the single WIP design authority.**
`docs/PLAN/PLAN.md` is its non-normative ordering and ambiguity map; Beads and
git own status. `docs/operations/build-and-test.md` is the gate of record and
the per-bead build and test commands.

`services/host` is the production washlet host. Production queue execution and
deterministic scenario execution are separate artifacts which share
`crates/execution/host`. Our wash-runtime changes are carried commits on a fork,
pinned in one place: `workspace.dependencies.wash-runtime.rev` in the root
`Cargo.toml`.

## Repository layout

```
services/               native deployable Rust services
  host                  production host: washlet embedding + host plugins
                        (wamn:postgres, logging, jetstream) — washlet only (SR9)
  ctl                   one-shot control-plane verbs (provision-*, apply-package,
                        publish-release, dump/restore/copy-project-env,
                        enable-cdc-project-env) — SR9 split
  dispatcher            shared trigger dispatcher service (SR9 split)
  executor              production router executor service; emits the stable
                        wamn-run-worker binary
  scenario-worker       authoring management service
  cdc-reader            CDC event-reader service (SR9 split)
  waker                 scale-to-zero wake actuator

crates/                 shared Rust workspace packages
  # shared, non-deployable packages grouped by bounded context:
  authoring/
    model               wamn-authoring-model: public commands and projections
  platform/
    component-policy    pure component-import and grant policy
    runtime             shared engine, plugins, WIT, and metrics
    pg-core             wamn-pg-core: guest-safe PostgreSQL primitives
  data/
    api                 wamn-api: HTTP/event-registration adapter
  schema/
    control             wamn-schema-control: package migration and runtime storage
    generator           wamn-schema-generator: package contracts and projections
    introspection       wamn-schema-introspection: migration policy + catalog IR
  execution/
    host                shared native host for execution components
    router              wamn-router: host-side graph walk
    run-state           wamn-run-state: run history, queue, lease, and timer state
    scheduler           wamn-scheduler: pure cron, due-tick, and cadence decisions
  control/
    registry            wamn-control-registry: org/project/environment model
    provision           wamn-control-provision: Postgres provisioning builders
  identity/
    project-state       wamn-project-state: per-project app_system model
  scenarios/
    model               wamn-scenario-model: test-set/assertion vocabulary

components/             wasm32-wasip2 guests and guest libraries
  data/                 capability-shaped SQLx transport/transaction runner;
                        generated Receiving data-access kernel
  ingress/              product ingress components (flow-http)
  events/               guest-consumed event rlibs (wamn-event-wire,
                        wamn-event-reg, wamn-materializer)
  execution/            product execution components (materializer) and the
                        node contract rlib (wamn-execution-contract)
  fixtures/             non-product proof fixtures (busyloop,
                        connection-http-standard, sockprobe, sqlx-command)
  no-std/               SECOND cargo workspace: the no_std palette guests
                        (http-request, transform), isolated from serde_json/std

test-support/
  harness/              shared measurement helpers for gates
  fixtures/             repository-only fixture implementations
  infrastructure/       temporary proof infrastructure helpers

tests/
  conformance/          narrow contract and compatibility proofs
  integration/          real-adapter compositions and failure injection
  system/               deployed public-surface journeys
  orchestrator/         compatibility CLI for the existing wamn-gates commands

deploy/                 deployment, gate, schema, and bootstrap assets
  infra/                install-once cluster infrastructure
    kind-config.yaml    local kind cluster definition
    values-wamn.yaml    runtime-operator Helm values (custom host image)
  platform/             long-lived production/platform manifests
  gates/                gate/bench Jobs and their support assets
    *-job.yaml          in-cluster gate-of-record Jobs
  sql/                  standalone SQL schemas
    *.sql               postgres-init, catalog-schema, run-state, run-queue,
                        authoring-tests, system-schema, app-schema, ops-schema,
                        control-portable-store
  mvp/                  bootstrap scripts; outside SR8 lifecycle tiers
                        (pre-tier provisioning; runs before any tier exists)

docs/                   exe-model.md: single WIP design authority
                        PLAN/PLAN.md: ordering and ambiguity map
                        operations/build-and-test.md: gate of record and the
                        per-bead build + test commands

Cargo.toml              root workspace; pins the wash-runtime fork rev
Dockerfile              shared build plus one final stage per deployable artifact
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
cargo build -p wamn-host -p wamn-ctl -p wamn-dispatcher \
  -p wamn-executor -p wamn-scenario-worker -p wamn-cdc-reader -p wamn-gates

# wasm guests — two workspaces, and they must not share one Cargo invocation
# (feature unification would force std into the no_std guests, wamn-0h0g.11.56)
(cd components && cargo build --release --target wasm32-wasip2)
(cd components/no-std && cargo build --release --target wasm32-wasip2)
```

## Test

```bash
# pure-crate unit/integration tests (no cluster needed)
cargo test                       # a specific crate: cargo test -p wamn-router

# lint + format
# --workspace is required: without it Cargo selects default-members only, which
# is 15 of the 34 workspace crates. --keep-going is required because Cargo stops
# scheduling new units at the first error, hiding every later package's lints.
cargo clippy --workspace --all-targets --keep-going && cargo fmt --all --check
```

Many crates also have optional live-apply tests that run against a throwaway
Postgres and skip when their `WAMN_*_PG_URL` env var is unset.

**Proofs** live in `tests/{conformance,integration,system}`. The `wamn-gates`
package at `tests/orchestrator` is the stable deploy-facing command router. The full per-bead
command set — local iteration and the in-cluster gate of record for each
subsystem — is in **`docs/operations/build-and-test.md`**.
Example (S1, no backend):

```bash
./target/release/wamn-gates --log-level warn socketguard \
  --component components/target/wasm32-wasip2/release/sockprobe.wasm
```

## Deploy (in-cluster)

The in-cluster gate of record runs on a local `kind` cluster named `wamn`,
with the host + gate images built from the two-stage `Dockerfile`:

```bash
# 1. stand up the cluster, its base infrastructure, and the runtime-operator
kind create cluster --name wamn --config deploy/infra/kind-config.yaml

#    cert-manager is install-once base infrastructure applied by hand at
#    standup, before anything that renders `cert-manager.io` CRs; it is
#    vendored at a pinned tag and `deploy/README.md` owns the bump procedure.
kubectl apply -f deploy/infra/cert-manager.yaml
kubectl -n cert-manager wait --for=condition=Available deploy --all --timeout=180s

#    The runtime-operator is installed as TWO Helm releases from one chart, in
#    this order: the cluster-singleton operator (which carries the CRDs and no
#    host groups), then this environment's host tier. The chart version pin is
#    deliberately NOT restated here — each values file's header carries the
#    exact `helm upgrade --install` command for its release, and
#    tests/conformance/tests/chart_seam_governance.rs fails if the two drift:
#      deploy/infra/values-wamn.yaml             operator + CRDs, cluster singleton
#      deploy/platform/values-host-default.yaml  host tier, one per environment

# 2. build the host and gate images and load them into kind
docker build --target host  -t wamn-host:dev  .
docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev  --name wamn
kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system rollout status deploy/hostgroup-default

# 3. apply the manifests / gate Jobs for the subsystem under test
#    (see docs/operations/build-and-test.md for the exact per-bead steps)
kubectl -n wamn-system apply -f deploy/gates/<subsystem>-job.yaml
kubectl -n wamn-system logs -f job/<subsystem>
```

## More

- `docs/exe-model.md` — the single WIP design authority.
- `docs/PLAN/PLAN.md` — the non-normative ordering and ambiguity map.
- `docs/operations/build-and-test.md` — the gate of record, every subsystem's
  build + gate commands, and the traps that produce false green.
- `CLAUDE.md` / `AGENTS.md` — instructions for AI coding agents (identical).
