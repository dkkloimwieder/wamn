# Build & Test — gate commands per bead

> **§1.9a audit (2026-07-19): amendments are additive — base sound.**

Every shipped feature/bead has a build+gate command block below. Prose rationale
lives in the design docs (`docs/*.md`) and the beads memories (`bd memories <keyword>`);
this file is the runnable-command reference. See `README.md` for the quick
dev/test/deploy commands.

## Build environment

wamn-host builds against wash-runtime consumed as a **git dependency from our
fork** (dkkloimwieder/wasmCloud, branch `wamn/2.5.2` = upstream v2.5.2 + the
carried epoch-deadline and memory-limiter commits) — see
`docs/wash-runtime-fork.md` for the carried-commit ledger, sync runbook, and
rev-bump procedure. The rev is pinned in one place:
`workspace.dependencies.wash-runtime.rev` in the root `Cargo.toml`.

## Gates by bead

### Workspace build

```bash
cargo build --release -p wamn-host -p wamn-ctl -p wamn-dispatcher -p wamn-run-worker -p wamn-cdc-reader -p wamn-gates   # all artifacts (SR1/SR9 split)
(cd components && cargo build --release --target wasm32-wasip2)  # guest fixtures
```

### S1/4p3/bp4.1 gates

```bash
# Local (exit-code disciplined since wamn-cjv.1: any failed phase — p99 SLO,
# cap kill at the 256 MiB ceiling, epoch Trap::Interrupt, 64/192 budget
# differentiation — makes bench exit non-zero; job completion IS the verdict):
./target/release/wamn-gates --log-level warn bench \
  --hello components/target/wasm32-wasip2/release/hello.wasm \
  --memhog components/target/wasm32-wasip2/release/memhog.wasm \
  --busyloop components/target/wasm32-wasip2/release/busyloop.wasm
# In-cluster gate of record (no DB/NATS; fixtures ship in the image):
kubectl -n wamn-system apply -f deploy/gates/bench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/bench --timeout=600s
kubectl -n wamn-system logs job/bench
# Mutation harness (4 mutants, each must exit non-zero): scratchpad/mutate_cjv1.py
```

### S2 gates (qps + p99, saturation, chaos/RLS/injection)

```bash
# Local iteration (throwaway container + the same fixture SQL):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/sql/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
./target/release/wamn-gates --log-level error pgbench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# --mode attack is the wamn-cjv.2 in-band claim-override gate (pgprobe ops 7/8/9);
# guard unit tests: cargo test -p wamn-host guard_
# Mutation harness (3 guard mutants, each must fail --mode attack): scratchpad/mutate_cjv2.py
# In-cluster gate of record (p99 is measured in-cluster):
kubectl -n wamn-system create configmap pg-init --from-file=init.sql=deploy/sql/postgres-init.sql
kubectl -n wamn-system apply -f deploy/platform/postgres.yaml -f deploy/gates/pgbench-job.yaml
kubectl -n wamn-system logs -f job/pgbench
```

### [2.2] production wamn:postgres

```bash
# Local iteration (same throwaway container as S2, plus WAMN_PG_ADMIN_URL):
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5450/wamn \
  ./target/release/wamn-gates --log-level error pgbench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (co-located, no cpu limit — S2 CFS lesson;
# WAMN_PG_ADMIN_URL is the superuser used only to provision the project DBs):
kubectl -n wamn-system apply -f deploy/gates/pgbench-multiproject-job.yaml
kubectl -n wamn-system logs -f job/pgbench-multiproject
```

### [2.3] managed Postgres provisioning

Docs: docs/provisioning.md

```bash
cargo test -p wamn-provision   # naming/slug/reserved-prefix + SQL shape + secret + live-apply
cargo clippy -p wamn-provision --all-targets && cargo fmt -p wamn-provision --check
# optional plain-PG live-apply (throwaway postgres:18; SUPERUSER url — CREATE
# skips when unset):
docker run -d --rm --name wamn-prov-pg -p 5460:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_PROVISION_PG_URL=postgres://postgres:postgres@127.0.0.1:5460/wamn cargo test -p wamn-provision
# locally against the SAME throwaway postgres:18 (superuser):
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5460/wamn \
  ./target/debug/wamn-gates --log-level error provisionbench
docker stop wamn-prov-pg
# The production tool is `wamn-ctl provision-project --project <id>
# In-cluster gate of record (against the shared CNPG cluster = the D6 substrate,
# NO cpu limit — S2 CFS lesson):
kubectl apply --server-side -f deploy/infra/cnpg-operator.yaml
kubectl -n cnpg-system rollout status deploy/cnpg-controller-manager --timeout=150s
kubectl apply -f deploy/infra/cnpg-cluster.yaml
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/wamn-pg --timeout=300s
# A HOST change => full docker rebuild (both --target stages + kind load BOTH images):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/gates/provisionbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/provisionbench --timeout=180s
kubectl -n wamn-system logs job/provisionbench
```

### S3 gates

```bash
./target/release/wamn-gates --log-level error flowbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster (same co-located / no-cpu-limit Job topology as pgbench):
kubectl -n wamn-system apply -f deploy/gates/flowbench-job.yaml
kubectl -n wamn-system logs -f job/flowbench
```

### S4 gates

```bash
# Two extra fixtures need external tools (one-time installs):
# composition are extra steps:
jco componentize components/samples/node-ts/node.js --wit components/samples/node-ts/wit \
  --world-name node-bench --disable http --disable fetch-event \
  -o components/samples/node-ts/node-ts.wasm
REL=components/target/wasm32-wasip2/release
wac plug $REL/flow_driver.wasm --plug $REL/node_rs.wasm -o $REL/flow_composed.wasm
./target/release/wamn-gates --log-level error nodebench \
  --node-rs $REL/node_rs.wasm --node-ts components/samples/node-ts/node-ts.wasm \
  --composed $REL/flow_composed.wasm --sample $REL/sample_node.wasm --mode all
# In-cluster gate of record (real cross-pod hop via the serve-node Service; the
# gap/config gates run in-pod; no cpu limit — the S2 CFS lesson):
kubectl -n wamn-system apply -f deploy/gates/serve-node.yaml
kubectl -n wamn-system rollout status deploy/serve-node --timeout=120s
kubectl -n wamn-system apply -f deploy/gates/nodebench-job.yaml
kubectl -n wamn-system logs -f job/nodebench
```

### S5 gates

```bash
# Local iteration (throwaway loki + collector on a docker network):
docker network create wamn-s5 2>/dev/null || true
docker run -d --name wamn-s5-loki --network wamn-s5 -p 3100:3100 \
  -v "$PWD/deploy/infra/loki-local.yaml:/etc/loki/loki.yaml:ro" \
  grafana/loki:3.4.2 -config.file=/etc/loki/loki.yaml
docker run -d --name wamn-s5-otelcol --network wamn-s5 -p 4317:4317 -p 8888:8888 \
  -v "$PWD/deploy/infra/otelcol-local.yaml:/etc/otelcol/config.yaml:ro" \
  otel/opentelemetry-collector-contrib:0.115.1 --config=/etc/otelcol/config.yaml
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 RUST_LOG=error \
  LOKI_URL=http://127.0.0.1:3100 COLLECTOR_METRICS_URL=http://127.0.0.1:8888/metrics \
  ./target/release/wamn-gates --log-level info logbench \
  --logspewer components/target/wasm32-wasip2/release/logspewer.wasm --mode all
# In-cluster gate of record (real Loki + collector; no cpu limit — the S2 lesson):
kubectl -n wamn-system apply -f deploy/infra/loki.yaml -f deploy/infra/otel-collector.yaml
kubectl -n wamn-system rollout status deploy/loki deploy/otel-collector --timeout=120s
kubectl -n wamn-system apply -f deploy/gates/logbench-job.yaml
kubectl -n wamn-system logs -f job/logbench
```

### [9.1] OTel trace pipeline

Docs: docs/tracing.md

```bash
cargo clippy -p wamn-host -p wamn-dispatcher -p wamn-gates --all-targets \
  && cargo fmt -p wamn-host -p wamn-dispatcher -p wamn-gates --check
# Local iteration (throwaway Postgres + Tempo + collector on a docker network;
# spans are INFO):
docker network create wamn-s5 2>/dev/null || true
docker run -d --rm --name wamn-trace-pg --network wamn-s5 -p 5482:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
docker run -d --name wamn-s5-tempo --network wamn-s5 -p 3200:3200 \
  -v "$PWD/deploy/infra/tempo-local.yaml:/etc/tempo/tempo.yaml:ro" \
  grafana/tempo:2.6.1 -config.file=/etc/tempo/tempo.yaml
docker run -d --name wamn-s5-otelcol --network wamn-s5 -p 4317:4317 -p 8888:8888 \
  -v "$PWD/deploy/infra/otelcol-local.yaml:/etc/otelcol/config.yaml:ro" \
  otel/opentelemetry-collector-contrib:0.115.1 --config=/etc/otelcol/config.yaml
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 OTEL_EXPORTER_OTLP_PROTOCOL=grpc \
  OTEL_BSP_SCHEDULE_DELAY=1000 RUST_LOG=error \
  ./target/debug/wamn-gates --log-level info tracebench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://postgres:postgres@127.0.0.1:5482/wamn \
  --tempo-url http://127.0.0.1:3200
docker stop wamn-trace-pg wamn-s5-tempo wamn-s5-otelcol
# In-cluster gate of record (real Tempo + collector + Postgres, no cpu limit —
# --target stages + kind load BOTH images):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/infra/tempo.yaml -f deploy/infra/otel-collector.yaml
kubectl -n wamn-system rollout status deploy/tempo deploy/otel-collector --timeout=120s
kubectl -n wamn-system apply -f deploy/gates/tracebench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/tracebench --timeout=180s
kubectl -n wamn-system logs job/tracebench
```

### [9.2] trace context propagation

Docs: docs/wash-runtime-fork.md, docs/tracing.md

```bash
cargo test -p wamn-node-sdk -p wamn-nodes   # trace_headers/apply + http-node forward + explicit-header-wins
cargo test -p wamn-gates --bin wamn-gates traceproof   # w3c/header-parse units
cargo clippy -p wamn-node-sdk -p wamn-nodes -p wamn-gates --all-targets \
  && cargo fmt -p wamn-node-sdk -p wamn-nodes -p wamn-gates --check
(cd components && cargo build --release --target wasm32-wasip2 -p trace-relay)
cargo clippy --manifest-path components/fixtures/trace-relay/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/fixtures/trace-relay/Cargo.toml --check
# No local run: the fork inject fires ONLY on the real washlet outbound path
# In-cluster gate of record. A FORK rev bump => FULL docker rebuild (both --target
# wash-runtime):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system rollout restart deploy/hostgroup-default
kubectl -n wamn-system rollout status deploy/hostgroup-default --timeout=180s
# via the registry port-forward):
kubectl -n wamn-system port-forward svc/registry 5000:5000 &
wash push localhost:5000/wamn/trace-relay:dev \
  components/target/wasm32-wasip2/release/trace_relay.wasm --insecure
# Deploy pod B (serve-echo) + pod A (trace-relay), then run the proof:
kubectl -n wamn-system apply -f deploy/gates/serve-echo.yaml
kubectl -n wamn-system rollout status deploy/serve-echo --timeout=120s
kubectl -n wamn-system apply -f deploy/platform/trace-relay-workload.yaml
kubectl -n wamn-system apply -f deploy/gates/traceproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/traceproof --timeout=180s
kubectl -n wamn-system logs job/traceproof
```

### S6 gates

```bash
# Local iteration (throwaway container + the same fixture SQL):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/sql/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
./target/release/wamn-gates --log-level error testhostbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (co-located with Postgres, no cpu limit — S2 lesson;
# WAMN_PG_ADMIN_URL is the superuser used only to provision the ephemeral schema):
kubectl -n wamn-system apply -f deploy/gates/testhostbench-job.yaml
kubectl -n wamn-system logs -f job/testhostbench
```

### [2.6] DB-path egress review

Docs: docs/security-db-path.md

```bash
REL=components/target/wasm32-wasip2/release
./target/release/wamn-gates --log-level warn egressbench \
  --flowrunner $REL/flowrunner.wasm \
  --component $REL/pgprobe.wasm --component $REL/node_rs.wasm \
  --component $REL/flow_composed.wasm --component $REL/hello.wasm \
  --component $REL/api_gateway.wasm \
  --component $REL/poc_webhook_f1.wasm \
  --component $REL/sample_node.wasm  # webhook/api: {wamn:postgres,wasi:http}; 5.4 sample node: ZERO egress

cargo clippy -p wamn-host -p wamn-run-worker -p wamn-gates -p wamn-gate-harness --all-targets \
  && cargo fmt -p wamn-host -p wamn-run-worker -p wamn-gates -p wamn-gate-harness --check

# E13/E15 runtime raw-socket deny + E17 rejection (wamn-o3u6), the in-cluster
# gate of record. sockprobe attempts raw TCP/UDP egress through the production
# host store path, so the fork's linked_call socket_addr_check is the policy
# under test (pins 8b76869 / eef76cd): raw egress is DENIED by default and
# PERMITTED only under wamn.allow-raw-sockets. --reject-tenant asserts a
# wamn:postgres importer (pgprobe) is refused by the allowlist v1 (E17). Runs
# locally without a cluster:
./target/release/wamn-gates --log-level warn egressbench \
  --flowrunner $REL/flowrunner.wasm \
  --reject-tenant $REL/pgprobe.wasm \
  --sockprobe $REL/sockprobe.wasm
# and in-cluster (fixtures baked in the wamn-gates image; no DB/NATS):
kubectl -n wamn-system apply -f deploy/gates/egressbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/egressbench --timeout=300s
kubectl -n wamn-system logs job/egressbench
```

### [E13a] publish-time egress-guard refusal (socketguard)

Docs: docs/security-db-path.md · Manifest: deploy/gates/socketguard-job.yaml

```bash
# Hermetic: synthesizes a wasi:sockets importer (must be REFUSED at publish) and
# a standard world (must publish) in-process — no registry, no fixtures, no DB,
# so the local run IS the whole gate. Unlike egressbench (which walks the shipped
# components), this proves the guard REJECTS an adversarial world.
cargo test -p wamn-gates            # +the egressbench runtime/reject-tenant units
cargo test -p wamn-host egress_guard  # the shared classifier units
./target/release/wamn-gates --log-level warn socketguard
# in-cluster sweep (carries the hermetic gate alongside egressbench-job):
kubectl -n wamn-system apply -f deploy/gates/socketguard-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/socketguard --timeout=120s
kubectl -n wamn-system logs job/socketguard
```

### [5.1] flow-graph schema crate (crates/wamn-flow)

Docs: docs/flow-schema.md

```bash
cargo test -p wamn-flow
cargo clippy -p wamn-flow --all-targets && cargo fmt -p wamn-flow --check
# regenerate the published JSON Schema contract after changing the types:
cargo run -p wamn-flow --example print-schema > docs/flow-schema.schema.json
```

### [5.2] production flow-runner engine (crates/wamn-runner)

Docs: docs/flow-runner.md

```bash
cargo test -p wamn-runner
cargo clippy -p wamn-runner --all-targets && cargo fmt -p wamn-runner --check
# locally. Rebuild the guest (part of the guest build above), then re-run those gates:
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)
cargo clippy --manifest-path components/flowrunner/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/flowrunner/Cargo.toml --check
```

### [5.3] standard node library v1 (crates/wamn-node-sdk + crates/wamn-nodes)

Docs: docs/node-library.md

```bash
cargo test -p wamn-nodes             # nodes + policy negatives + purity lint
cargo test -p wamn-node-sdk
cargo test -p wamn-runner            # taxonomy re-export + port drift-guard regression
cargo clippy -p wamn-node-sdk -p wamn-nodes --all-targets \
  && cargo fmt -p wamn-node-sdk -p wamn-nodes --check
```

### [5.4] wamn:node contract 0.1 FROZEN + SDK scaffolding

```bash
cargo test -p wamn-node-sdk      # incl the wit_coherence drift-guards
cargo test -p wamn-node-guest    # conversion glue + NoCapsCtx units
cargo test -p wamn-node-manifest # fixture/negatives/conformance/drift
cargo clippy -p wamn-node-guest -p wamn-node-manifest --all-targets \
  && cargo fmt -p wamn-node-sdk -p wamn-node-guest -p wamn-node-manifest --check
# regenerate the published manifest schema after changing the types:
cargo run -p wamn-node-manifest --example print-schema > docs/wamn-node-manifest.schema.json
```

### [5.7] run-state persistence (crates/wamn-run-store)

Docs: docs/run-state.md

```bash
cargo test -p wamn-run-store
cargo test -p wamn-runner   # the resume/seed_at primitives (regression)
cargo clippy -p wamn-run-store --all-targets && cargo fmt -p wamn-run-store --check
# optional live-apply gate (deploy/sql/run-state.sql on a throwaway PG; superuser URL
# node_runs FK cascade; skips cleanly when unset):
docker run -d --rm --name wamn-runstore-pg -p 5458:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RUN_STORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5458/wamn cargo test -p wamn-run-store
docker stop wamn-runstore-pg
# (in-cluster gate of record + locally). Rebuild the guest, re-run those gates (the
# additively (kubectl exec psql — shared-cluster guardrail, never recreate the pod).
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)
cargo clippy --manifest-path components/flowrunner/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/flowrunner/Cargo.toml --check
```

### [5.9] credential vault (plugins/wamn_credentials + credproof)

Docs: docs/credential-vault.md

```bash
# Pure units: the SDK facade + http-request injection/classification + the
# guest per-dispatch scoping + the host vault resolution + the WIT coherence
# drift-guards (the credentials copies) + the credproof fixture pins.
cargo test -p wamn-node-sdk && cargo test -p wamn-nodes
cargo test -p wamn-node-guest --all-features
cargo test -p wamn-host wamn_credentials && cargo test -p wamn-gates credproof

# cjv.3 host-enforced per-execution grant + fail-closed project. credprobe
# drives the direct-import THREAT fixture (components/fixtures/cred-probe,
# imports wamn:node/credentials directly like a custom node) in-proc against a
# vault with a NARROW host-registered grant — proves an ungranted /
# unregistered-project get() is not-granted over the real WIT boundary (no DB):
(cd components && cargo build --release --target wasm32-wasip2 -p cred-probe)
./target/debug/wamn-gates credprobe \
  --cred-probe components/target/wasm32-wasip2/release/cred_probe.wasm
# Mutation (apply/test/restore, sha256, DEBUG): scratchpad mutate_cjv3.py
#   M1 grant check skipped        -> credprobe (sibling/absent not-granted)
#   M2 project_for fail-open      -> credprobe (no-project not-granted)
#   M3 set_granted no-op          -> credprobe (DELIVERY: granted resolves)
#   M4 guest declares empty grant -> credproof e2e (notify get not-granted, no delivery)

# Local end-to-end (throwaway PG + local serve-echo + a background run-worker
# whose vault carries the demo secret; the run-worker needs the target on its
# --allowed-hosts — EMPTY = deny-all, fail-closed):
docker run -d --name wamn-cred-pg -p 5493:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
cat > /tmp/wamn-credentials.json <<'JSON'
{ "default": { "notify-token": "wamn-cred-proof-7f3a9b2e41d05c68" } }
JSON
./target/debug/wamn-gates --log-level error serve-echo --port 8093 &
WAMN_RUNNER=cred-local ./target/debug/wamn-run-worker --log-level info \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5493/wamn \
  --tenant demo-tenant --schema wamn_cred_local --project default \
  --credentials-file /tmp/wamn-credentials.json \
  --allowed-hosts 127.0.0.1:8093 --max-idle-ms 1500 &
./target/debug/wamn-gates credproof \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5493/wamn \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5493/wamn \
  --schema wamn_cred_local --tenant demo-tenant \
  --echo-url http://127.0.0.1:8093 --setup
# Mutation harness (apply/test/restore, sha256-verified): scratchpad mutate_17o.py
# M1 http.rs injection neutered  -> unit http_request_sends_the_declared_credential
# M2 host resolve wrong constant -> live DELIVERY digest mismatch
# M3 CapsCtx scoping neutered    -> unit credential_without_a_declaration_...
# M4 node leaks its credential   -> live CONTAINMENT (notify.output + status.input)

# In-cluster gate of record (kind 'wamn'; FULL rebuild BOTH stages — host
# changed [vault plugin + run-worker egress] AND the guest re-baked
# [flowrunner imports wamn:node/credentials]):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
# provision wamn_runner_demo + register deploy/cred/notify.flow.json AND
# deploy/cred/deny.flow.json active (the fqg.8/ojm recipe; deny.flow.json is
# the fqg.11 per-flow egress deny half credproof now asserts), then:
kubectl -n wamn-system apply -f deploy/gates/serve-echo.yaml
kubectl -n wamn-system apply -f deploy/platform/runner-credentials.example.yaml
kubectl -n wamn-system apply -f deploy/platform/runner-db.example.yaml -f deploy/platform/runner.yaml
kubectl -n wamn-system apply -f deploy/gates/credproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/credproof --timeout=180s
kubectl -n wamn-system logs job/credproof   # overall PASS: true
```

### [5.14] durable run queue & runner scaling (crates/wamn-run-queue)

Docs: docs/run-queue.md

```bash
cargo test -p wamn-run-queue
cargo clippy -p wamn-run-queue --all-targets && cargo fmt -p wamn-run-queue --check
# optional live-apply gate (deploy/sql/run-state.sql + run-queue.sql on a throwaway PG;
# skips cleanly when unset):
docker run -d --rm --name wamn-rq-pg -p 5459:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RUN_QUEUE_PG_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn cargo test -p wamn-run-queue
# throwaway PG above (the live-apply gate created wamn_app) + a throwaway NATS:
docker run -d --rm --name wamn-rq-nats -p 4232:4222 nats:2.12.8-alpine
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/release/wamn-gates --log-level error queuebench \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn \
  --nats-url nats://127.0.0.1:4232 --mode all
docker stop wamn-rq-pg wamn-rq-nats
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS lesson;
# kind load docker-image wamn-gates:dev --name wamn):
kubectl -n wamn-system apply -f deploy/gates/queuebench-job.yaml
kubectl -n wamn-system logs -f job/queuebench
```

D20 (R6, wamn-1d4) the `partitioned(key)` head-unavailability policy lands here:
`wamn-flow` gains `Flow::partition_policy` (`blocking` default / `leapfrog`),
`run_queue.partition_policy` materializes it, `claim_partition_head_sql` branches on
it, and `janitor_sweep_sql` exempts a blocking-policy row (wedge). Pure coverage:
`partition_policy_decides_whether_a_later_run_overtakes_an_unavailable_head`,
`blocking_wedges_a_key_behind_an_exhausted_head_leapfrog_releases_it`,
`blocking_partition_orphan_wedges_instead_of_being_reaped` (janitor verdict), plus
shape/DDL drift guards. The live-apply gate (Phase A/B) and the queuebench
`partition` phase (`partition_policy_cases`) prove it through real Postgres. The
guest does not read the flow field until fqg.9, so the in-cluster gate is a
gates-image rebuild only (guest unchanged for this slice).

### [EVT-C7 / wamn-z7b.1] queuebench ceiling campaign (measurement, not a gate)

Docs: docs/ceilings.md (the published curves) + docs/event-plane-jetstream.md §10/§11

```bash
# The pure ramp/knee controller (coarse-double → bisect; p99-doubling /
# rate-divergence / drain-timeout saturation) lives in wamn-gate-harness:
cargo test -p wamn-gate-harness
# Local iteration (short knobs; correctness only — debug build, dev-host PG):
docker run -d --rm --name wamn-ceil-pg -p 5443:5432 -e POSTGRES_PASSWORD=postgres postgres:18
docker exec wamn-ceil-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:5443/postgres \
  WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5443/postgres \
  ./target/debug/wamn-gates --log-level error queuebench --mode ceiling \
  --level-secs 5 --soak-secs 30 --burst-secs 10
docker stop wamn-ceil-pg
# Numbers of record (in-cluster, §10 knobs baked into the manifest; ~60–90 min):
kubectl -n wamn-system apply -f deploy/gates/queuebench-ceiling-job.yaml
kubectl -n wamn-system logs -f job/queuebench-ceiling
# Extract the `=== BEGIN CSV <name> ===` blocks from the job log into
# docs/ceilings-data/ and cite them from docs/ceilings.md (§11 provenance).
```

The ceiling mode is deliberately NOT in `--mode all` (the regression gate of
record stays deploy/gates/queuebench-job.yaml). Only the exactly-once + completeness
sanity asserts are pass/fail; the knees/curves are measurements. Phase 2
(fillfactor × autovacuum matrix, 30-min soak, 1M-run bloat soak) = wamn-z7b.6.
Mutation harness for the knee controller: scratchpad `mutate_z7b1.py`
(saturation-arm + bisect-direction mutants each fail a named
wamn-gate-harness unit test).

### [EVT-C2 / wamn-z7b.2] outboxbench trigger-overhead campaign (measurement, not a gate)

Docs: docs/ceilings.md (the published curves) + docs/event-plane-jetstream.md §10/§11
+ docs/ddl-compiler.md § Outbox row-event triggers

```bash
cargo test -p wamn-gates outboxbench   # cadence parse/duration + catalog/plan compile units
# Local iteration (short knobs; correctness only — debug build, dev-host PG):
docker run -d --rm --name wamn-c2-pg -p 5443:5432 -e POSTGRES_PASSWORD=postgres postgres:18
docker exec wamn-c2-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:5443/postgres \
  WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5443/postgres \
  ./target/debug/wamn-gates --log-level error outboxbench --mode all \
  --iters 100 --growth-rate 50 --growth-secs 20 --growth-cadences 0,5,15 --retention-ms 2000
docker stop wamn-c2-pg
# Numbers of record (in-cluster, record knobs baked into the manifest; ~35–40 min;
# a SINGLE run is the record — no knee search to poison, the headline numbers
# are byte counts/medians and a stall shows as a visible p99 outlier):
kubectl -n wamn-system apply -f deploy/gates/outboxbench-job.yaml
kubectl -n wamn-system logs -f job/outboxbench
# Extract the `=== BEGIN CSV <name> ===` blocks (c2-trigger / c2-bulk /
# c2-growth-c{0,60,600}) into docs/ceilings-data/ and cite them from
# docs/ceilings.md (§11 provenance).
```

Paired same-table A/B: the bench toggles the REAL `Migration::outbox_triggers`
/ `drop_outbox_triggers` plans between phases, so the with/without delta is the
trigger itself (a closing baseline re-measure bounds drift). Only the sanity
asserts are pass/fail: the trigger fires exactly once per written row (per
event), and the prune never touches a pending row (sentinel proof). Mutation
harness: scratchpad `mutate_z7b2.py` (trigger-apply neutered / acker acks
sentinels / bulk on-leg loses the trigger — each fails a named sanity assert).

### [EVT-C-WAL-0 / wamn-l5i9.4] walbench pre-CDC WAL baseline (measurement, not a gate)

Docs: docs/ceilings.md § C-WAL-0 (the published numbers) + docs/event-plane-jetstream.md
§7/§8/§10. The *denominator* every later C-CDC WAL-delta claim (wamn-l5i9.14) divides
by: representative-app WAL volume BEFORE any publication/slot exists (bd dep
wamn-l5i9.9 → wamn-l5i9.4 keeps it strictly pre-CDC).

```bash
cargo test -p wamn-gates walbench   # rates parse / wide-blob entropy / poc-catalog floor compile
# Local iteration (short knobs; correctness only — debug build, dev-host PG):
docker run -d --rm --name wamn-cwal0-pg -p 5444:5432 -e POSTGRES_PASSWORD=postgres postgres:18
docker exec wamn-cwal0-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:5444/postgres \
  WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5444/postgres \
  ./target/debug/wamn-gates --log-level error walbench --mode all \
  --iters 100 --mixed-rates 20,50 --mixed-secs 8
docker stop wamn-cwal0-pg
# Numbers of record (in-cluster on the fixture pod, record knobs baked into the
# manifest; ~few min; a SINGLE run is the record — byte counts + medians, no knee
# to poison). Needs a gates-only image (docker build --target gates); no wamn-host
# change so the host stage is cached apart from the crates/ recompile:
docker build --target gates -t wamn-gates:dev . && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/gates/walbench-job.yaml
kubectl -n wamn-system logs -f job/walbench
# Extract the `=== BEGIN CSV <name> ===` blocks (cwal0-perop / cwal0-mixed) into
# docs/ceilings-data/ and cite them from docs/ceilings.md (§ C-WAL-0 provenance).
```

The pre-CDC claim is made checkable, not assumed: `precheck` asserts the measured DB
has no publication and no replication slot and every table carries the DEFAULT replica
identity (`d`) before any measurement runs. `pg_current_wal_insert_lsn` (WAL generated),
not the flushed position — exact even under the fixture pod's `fsync=off`/
`synchronous_commit=off`. Only the sanity asserts gate: pre-CDC, per-op WAL > 24 B (the
instrument self-check), exact op counts, and the wide leg genuinely TOASTed. Mutation
harness: scratchpad `mutate_cwal0.py` (M1 instrument swap `pg_current_wal_insert_lsn` →
`pg_current_wal_lsn` fails every `> 24 B/op` assert on an `fsync=off` PG — the fixture-pod
kill; M2 op-batch runs `n/2` fails the exact-op-count assert).

### [EVT-S-CDC-1 / wamn-l5i9.2] pg_walstream diligence spike (diligence, not a gate)

Docs: docs/event-plane-jetstream.md §7; verdicts live in the wamn-l5i9.2 bead
notes and feed wamn-l5i9.6 [BUILD-VS-BUY]. The harness is `poc/cdc1`
(pg_walstream from the wamn fork, rev-pinned in the root workspace table since
wamn-l5i9.8 — ledger: docs/pg-walstream-fork.md).

```bash
cargo build -p wamn-cdc1 && cargo clippy -p wamn-cdc1 && cargo fmt -p wamn-cdc1 --check
# Throwaway 2-instance CNPG cluster (torn down after the spike; NEVER reuse
# wamn-pg or wamn-sysdb — switchover needs a standby):
kubectl apply -f poc/cdc1/cdc1-cluster.yaml   # cluster cdc1 + NodePort 172.28.0.4:30497
export CDC1_URL="postgresql://postgres:$(kubectl -n wamn-system get secret \
  cdc1-superuser -o jsonpath='{.data.password}' | base64 -d)@172.28.0.4:30497/app"
./target/debug/wamn-cdc1 setup        # tables + publication + failover slot (through the crate)
./target/debug/wamn-cdc1 message      # (e) pg_logical_emit_message → EventType::Message
./target/debug/wamn-cdc1 toast        # (c) unchanged-TOAST absent-vs-Null + FULL old image
./target/debug/wamn-cdc1 stream --rows 1000000   # (d) streamed txn, VmRSS profile
./target/debug/wamn-cdc1 soak --secs 1800        # (a) idle keepalive/feedback + canary
./target/debug/wamn-cdc1 switchover --secs 90    # (b) then delete the primary pod mid-run
./target/debug/wamn-cdc1 teardown && kubectl delete -f poc/cdc1/cdc1-cluster.yaml
```

FINDING F1: crates.io pg_walstream 0.8.0's `slot_options.failover = true`
emits legacy space-separated `CREATE_REPLICATION_SLOT … FAILOVER`, which PG17+
rejects (FAILOVER exists only in the parenthesized option grammar). FIXED in
the wamn fork (wamn-l5i9.8): the harness now sets `failover = true` and creates
the slot through the crate.

### [EVT-VENDOR / wamn-l5i9.8] pg_walstream fork + pin

Docs: docs/pg-walstream-fork.md (carried-commit ledger + sync runbook). The
fork branch `wamn/0.8.0` = upstream v0.8.0 + the F1 failover-syntax commit;
the rev is pinned once in the root `Cargo.toml` workspace table.

```bash
# Fork unit tests (in a clone of dkkloimwieder/pg-walstream, branch wamn/0.8.0):
cargo test --lib          # 1247 tests incl the parenthesized-FAILOVER pins
# Consumer + lock sanity (in wamn):
cargo build -p wamn-cdc1
grep -c '^name = "pg_walstream"$' Cargo.lock   # must be 1 (git-sourced)
# Live A/B (throwaway postgres:18 -c wal_level=logical, e.g. :5444):
#   A: pin poc/cdc1 back to crates.io `=0.8.0` → `wamn-cdc1 setup` fails 42601
#   B: the fork pin → setup prints `slot cdc1_spike created: … failover=true`,
#      then `wamn-cdc1 message` passes as the streaming regression.
```

### [EVT-NATS / wamn-l5i9.7] streambench data-plane JetStream gate

Docs: docs/event-plane-jetstream.md §5/§7 Phase 1. Stands up the DEDICATED
data-plane NATS (deploy/infra/nats-jetstream.yaml — a 3-node JetStream cluster, R3
file storage, Service `evt-nats`), SEPARATE from the operator/control-plane NATS
(Service `nats`, doorbells) which stays untouched. The gate (`streambench`, a
pure NATS client — no wasm, no Postgres) proves the four load-bearing claims:
publish → the `EVT_<org>_<env>` stream (subjects
`evt.<org>.<project>.<env>.<entity>.<op>`), `Nats-Msg-Id = <project_env>:<lsn>`
dedupe, consume in commit order, and R3 survives node loss. Accounts: single
shared (default) account — per-org accounts + replication creds are the
wamn-4xw seam (§11).

```bash
cargo build -p wamn-gates   # streambench compiles into the suite
# Local iteration — a throwaway 3-node cluster is R3 (single node = R1):
docker network create evt-nats-local
R=nats://evt-nats-local-0:6222,nats://evt-nats-local-1:6222,nats://evt-nats-local-2:6222
for i in 0 1 2; do docker run -d --name evt-nats-local-$i --network evt-nats-local \
  -p $((4232+i)):4222 nats:2.10-alpine -js -sd /data --name n$i \
  --cluster nats://0.0.0.0:6222 --cluster_name evt-local --routes "$R"; done
./target/debug/wamn-gates --log-level error streambench --mode all \
  --nats-url nats://localhost:4232 --replicas 3 --messages 200
# Physical node-loss heal (degraded 2/3): publish → destroy a node → heal
./target/debug/wamn-gates --log-level error streambench --mode publish \
  --nats-url nats://localhost:4232 --replicas 3 -n 200
docker rm -f evt-nats-local-2
./target/debug/wamn-gates --log-level error streambench --mode heal \
  --nats-url nats://localhost:4232 --replicas 3 --expect-messages 200
docker rm -f evt-nats-local-0 evt-nats-local-1 evt-nats-local-2; docker network rm evt-nats-local

# Gate of record (in-cluster). Gates-only image (no wamn-host change → host stage
# cached apart from the crates/ recompile):
docker build --target gates -t wamn-gates:dev . && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/infra/nats-jetstream.yaml
kubectl -n wamn-system rollout status statefulset/evt-nats --timeout=180s
kubectl -n wamn-system apply -f deploy/gates/streambench-job.yaml    # --mode all: publish/consume/dedupe/stepdown
kubectl -n wamn-system wait --for=condition=complete job/streambench --timeout=180s
kubectl -n wamn-system logs job/streambench
# Physical R3 heal (the runbook is in deploy/gates/streambench-job.yaml's header):
#   streambench-pub pod → kubectl delete pod evt-nats-2 → streambench-heal pod
```

`--mode all` proves R3 durability without k8s (a RAFT leader step-down +
re-election, all messages survive); the two-step `publish` → `kubectl delete pod`
→ `heal` runbook proves survival of a physical node deletion. The heal drain
uses an R1 in-memory consumer (transient bookkeeping — the durability guarantee
is on the R3 stream), so it succeeds while a node is still down. Mutation
harness: scratchpad `mutate_l5i9_7.py` — M1 drops the Nats-Msg-Id on re-publish
(dedupe assert fails), M2 creates the stream R1 not R3 (`stream is R3` fails),
M3 makes the LSN non-monotonic-but-unique via `i^1` (commit-order assert fails),
M4 drops the id on the focused second publish (`second publish IS a duplicate`
fails). The data-plane NATS is left STANDING as the Phase-1 substrate (the
reader wamn-l5i9.10 + C-JS wamn-l5i9.15 consume it); reclaim with
`kubectl -n wamn-system delete -f deploy/infra/nats-jetstream.yaml`.

### [EVT-PROVISION / wamn-l5i9.9] enable-cdc-project-env — publication + failover slot + reader registration

Docs: docs/event-plane-jetstream.md §4, docs/provisioning.md
§enable-cdc-project-env. The CDC capture overlay on a provisioned project-env:
one shared `wamn_cdc_<org>__<project>__<env>` name for the publication
(`FOR TABLES IN SCHEMA <data schema>` — auto-includes tables catalog-publish
creates later), the failover-enabled slot (SQL-function form,
`pg_create_logical_replication_slot(…, failover => true)`; WAL pinned from
enable), and the REPLICATION role (R8b tier; own Secret
`wamn-cdc-<org>--<project>--<env>`), plus the `registry.event_readers`
registration (FK → `project_envs`, so an unprovisioned env is refused).

```bash
cargo test -p wamn-provision            # name/builder/secret units incl the CDC set
cargo test -p wamn-registry             # event-reader builder shapes + EventReader round-trip
cargo test -p wamn-ctl enable_cdc      # bundle ordering + name validation
cargo clippy -p wamn-provision -p wamn-registry -p wamn-ctl
# Live-apply gates (throwaway PG18 with logical decoding ON):
docker run -d --name wamn-cdc-pg -e POSTGRES_PASSWORD=postgres -p 5447:5432 \
  postgres:18 -c wal_level=logical
WAMN_CDC_PG_URL=postgres://postgres:postgres@127.0.0.1:5447/postgres \
  cargo test -p wamn-provision --test cdc          # publication/slot/role/grants live
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5447/postgres \
  cargo test -p wamn-registry --test storage       # incl event_readers upsert/read/FK/cascade
docker rm -f wamn-cdc-pg
# In-cluster gate of record (no docker rebuild — the real debug subcommand +
# kubectl; scratchpad incluster_l5i9_9.sh is the scripted run): register a
# trials org + project-env on wamn-pg (q3n.7 runbook), then:
./target/debug/wamn-ctl enable-cdc-project-env --org <o> --project <p> --env <e> \
  --schema app --system-database-url "$WAMN_SYSTEM_ADMIN_URL" \
  --emit-role-sql role.sql --emit-cdc-sql cdc.sql --emit-secret secret.json
#   apply order: role.sql → the TARGET cluster (any DB; roles are cluster-global),
#   cdc.sql → the PROJECT-ENV database (publication + slot are database-bound),
#   kubectl apply secret.json. Assert pg_publication (+ auto-include after a
#   CREATE TABLE in the schema), pg_replication_slots.failover=true,
#   pg_roles.rolreplication, and the registry.event_readers read-back; teardown
#   drops the slot FIRST (releases pinned WAL — wamn-pg has no
#   max_slot_wal_keep_size bound), then CR/db/role + the org row (cascade).
# kubectl port-forward dies per-connection on this kind cluster — use the
# temporary NodePort-on-the-primary recipe (8df.5) for the host↔wamn-sysdb TCP.
```

Mutation harness: scratchpad `mutate_l5i9_9.py` — M1 slot `failover` true→false,
M2 role loses `REPLICATION`, M3 publication `FOR ALL TABLES`, M4 event-reader
upsert never refreshes; each killed by a named unit AND the live gate (the gate
drops the role in its preamble so a leftover healthy role can't mask a mutated
builder). Cluster-level preconditions (`wal_level=logical` is the CNPG default;
`synchronizeLogicalDecoding` / `max_slot_wal_keep_size` are provision-org
env-policy knobs) are a SIBLING bead, not this overlay.

### [EVT-READER / wamn-l5i9.10] event-reader — one project-env → the EVT_ stream

Docs: docs/event-plane-jetstream.md §4. The CDC reader MVP: `wamn-cdc-reader --org --project --env` (replicas=1 Deployment,
deploy/platform/event-reader.example.yaml) reads its `registry.event_readers`
registration, opens ONE pg_walstream session (`StreamingMode::Off` — whole
txns, commit order), and publishes `wamn-event-wire` envelopes onto
`evt.<org>.<project>.<env>.<entity>.<op>` with
`Nats-Msg-Id = <project>_<env>:<lsn>`. Confirmed LSN advances ONLY on
JetStream ack, at txn granularity; JetStream down ⇒ the publish retries
forever ⇒ WAL retained (delayed, never lost). The reader NEVER creates the
slot — a missing/invalidated slot is the v3 §11 capture-gap incident and the
crash-loop is the MVP alert. `WAMN_CDC_URL` is the plain Secret url; the
reader appends `sslmode` + `replication=database` itself.

```bash
cargo test -p wamn-event-wire           # the draft wire contract, string-pinned
cargo test -p wamn-cdc-reader --lib   # url compose / error classify / row map
cargo clippy -p wamn-event-wire -p wamn-cdc-reader -p wamn-gates
# Local live gate (throwaway PG18 logical + single-node JetStream; ~90s —
# idle-stream feedback rides the ~30s server-keepalive cycle, hence the waits):
docker run -d --name wamn-reader-pg -e POSTGRES_PASSWORD=postgres -p 5448:5432 \
  postgres:18 -c wal_level=logical -c fsync=off
docker run -d --name wamn-reader-nats -p 4261:4222 nats:2.10-alpine -js -sd /data
WAMN_READER_PG_URL=postgres://postgres:postgres@127.0.0.1:5448/postgres \
WAMN_READER_NATS_URL=nats://127.0.0.1:4261 \
  cargo test -p wamn-cdc-reader --test event_reader_live
# drills: disabled-registration + missing-slot refusals, commit order +
# envelope shape (TOAST-absent vs NULL) + dedupe, LSN-advance-on-ack, crash →
# restart resume, severed-proxy JetStream-down holds the LSN, clean shutdown,
# zero-residue teardown (no slot left behind).
docker rm -f wamn-reader-pg wamn-reader-nats
# In-cluster gate of record (no image rebuild — the real debug binary against
# NodePorts on wamn-pg/wamn-sysdb/evt-nats; scripted: scratchpad
# incluster_l5i9_10.sh): provision + enable-cdc a trials org (l5i9.9 runbook),
# run `wamn-cdc-reader`, psql writes → the R3 EVT_ stream, then the
# stream-side asserts + drills:
./target/debug/wamn-gates readerbench --nats-url nats://<node>:30493 \
  --org t10cdc --project app --env dev --expect-ids 1,2,3,… [--delete-stream]
#   + SIGKILL/restart resume, severed-python-proxy LSN hold (never touches
#   evt-nats itself), SIGTERM clean exit, zero-residue teardown (slot first).
```

Mutation harness: scratchpad `mutate_l5i9_10.py` — M1 wire `msg_id` order
swapped (named unit), M2 an unacked publish counts as acked (the live gate's
"confirmed LSN must HOLD" phase), M3 the `enabled` flag ignored (disabled
probe), M4 a missing slot silently tolerated (the CAPTURE GAP probe); all
apply/test/restore with sha256, DEBUG builds.

### [EVT-OIDMAP / wamn-l5i9.11] relation-OID → catalog-entity keying (R9b)

Docs: docs/event-plane-jetstream.md §4/§5, docs/archive/review-findings.md R9b. The
reader resolves each relation OID to its stable catalog **entity id** via the
`wamn_entities` map (`relation_oid → entity_id, table_name`), maintained by
`publish-catalog`/`migrate-catalog` IN the DDL transaction (OID-keyed, so a
rename only updates `table_name`; pg_class OIDs survive `ALTER TABLE RENAME`).
The envelope carries `entity` (the id — ABSENT ⇒ unmapped, the
delayed-never-lost fallback) and `table` (physical name); the subject's entity
segment is the id, so consumer filters are rename-proof. Same throwaway rig as
[EVT-READER]; the live gate gains **phase F**, the rename drill.

```bash
cargo test -p wamn-event-wire                # +unmapped-marker + entity/table wire pin
cargo test -p wamn-provision entity_map      # the OID-keyed upsert drift guard ($2::text)
cargo test -p wamn-cdc-reader --lib          # +entity_lookup_sql pin, +map-order bundle test
# Local live gate (adds the rename drill: provision entity `sales_orders` as
# table `orders` via the REAL migrate-catalog path, wipe+publish-catalog
# backfill, rename → `orders2`, assert the pg_class OID is constant and every
# envelope/subject carries the stable id across the rename; platform tables
# publish entity-ABSENT):
WAMN_READER_PG_URL=postgres://postgres:postgres@127.0.0.1:5448/postgres \
WAMN_READER_NATS_URL=nats://127.0.0.1:4261 \
  cargo test -p wamn-cdc-reader --test event_reader_live
# In-cluster gate of record: incluster_l5i9_10.sh's shape + a rename-drill step
# driving migrate-catalog, asserted with the new readerbench flags:
./target/debug/wamn-gates readerbench --nats-url nats://<node>:30493 \
  --org t10cdc --project app --env dev --stream EVT_t10cdc_dev \
  --filter-entity sales_orders --expect-entity-id sales_orders \
  --id-field num --expect-ids 80,81,90,91,92
```

Mutation harness: scratchpad `mutate_l5i9_11.py` — M1 map upsert dropped from
migrate-catalog's apply txn, M2 dropped from publish-catalog, M3 the reader's
map lookup bypassed (everything unmapped), M4 the subject keyed by the table
even when mapped, M5 the upsert loses `ON CONFLICT` — each fails a NAMED live
assert; apply/test/restore with sha256, DEBUG builds.

### [EVT-CAUSATION-STITCH] reader stitches wamn.causation (l5i9.12.1)

Docs: docs/event-plane-jetstream.md §4 · Recipe extends [EVT-READER]/[EVT-OIDMAP]

The reader enables protocol Messages (`with_messages(true)`) and switches
`drain()` to **buffer-per-txn**: it collects a transaction's row events and
captures a transactional `wamn.causation` message whenever it lands, then at
`Commit` publishes every row with the `{run,root,depth}` stamp attached — robust
to whether the message frame arrives before or after the rows. The LSN still
advances only after every row is acked. The live gate gains **phase G**. (The
plugin-emit half — how a run-owned txn gets the message — is the split sibling
l5i9.12.2; here the message is emitted by test SQL.)

```bash
cargo test -p wamn-event-wire                        # causation wire pin (run/root/depth)
cargo test -p wamn-cdc-reader --lib parse_causation  # only a transactional wamn.causation frame counts
# Local live gate: phase G drives BOTH frame orderings (message-at-BEGIN and
# message-AFTER-rows), a plain txn (causation ABSENT), and a rolled-back txn
# that emitted one (nothing published — transactional):
WAMN_READER_PG_URL=postgres://postgres:postgres@127.0.0.1:5448/postgres \
WAMN_READER_NATS_URL=nats://127.0.0.1:4261 \
  cargo test -p wamn-cdc-reader --test event_reader_live
# In-cluster gate of record (local reader binary + wamn-pg + evt-nats R3): one
# txn emits the message AFTER 5 inserts; the new readerbench flag asserts every
# envelope carries the run. Script: scratchpad incluster_l5i9_12.sh.
./target/debug/wamn-gates readerbench --nats-url nats://<node>:30493 \
  --org t121cau --project app --env dev --stream EVT_t121cau_dev \
  --entity receipts --expect-ids 1,2,3,4,5 --expect-causation-run gate-run-1
```

Mutation harness: scratchpad `mutate_l5i9_12.py` — M1 messages disabled
(`with_messages(false)`), M2 the causation stamp dropped at `Commit`, M3 the
exact-prefix guard broken — M1/M2 fail live-gate phase G, M3 fails the
`parse_causation` unit test; apply/test/restore with sha256, DEBUG builds.

### [EVT-CAUSATION-EMIT] the plugin emits wamn.causation per run-owned txn (l5i9.12.2)

Docs: docs/event-plane-jetstream.md §4 · The emit half of the split above.

The trusted flow-runner declares the run it drives through a new **additive**
`wamn:runner/causation.set-run-context` channel (linked ONLY into the compiled-in
runner — `wamn:postgres` stays FROZEN 0.1.0, no S2 re-gate); the host feeds a
per-component `current_run` map on the `WamnPostgres` plugin, and
`begin_with_claims` appends a transactional
`pg_logical_emit_message(true,'wamn.causation',{run,root,depth})` to every
run-owned txn. MVP: root runs only → `root = run`, `depth = 0` (no claim-SQL
change, no guest-data change; event-chain root/depth thread from the materializer
l5i9.17). A guest raw-SQL `wamn.*` emit is rejected on the query/execute/cursor
surface (defense-in-depth blocklist, AR1). HOST-changed (plugin ships in
wamn-host) AND GUEST-changed (the runner declares the channel) — the in-cluster
gate rebakes the host image + rebuilds the flowrunner wasm.

```bash
cargo test -p wamn-host --lib plugins::wamn_postgres::tests  # emit bytes pinned + batch wiring (run set/unset) + forgery guard + current_run map
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)  # guest declares the channel
# Local live proof — the REAL plugin emit through the REAL runner (both drive
# paths: run/run_s6/run_until_kill via execute(), run_next via execute_claimed()):
docker run -d --name caus-pg -p 5491:5432 -e POSTGRES_PASSWORD=postgres postgres:18 -c wal_level=logical
docker exec caus-pg psql -U postgres -c "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER;"
docker exec caus-pg psql -U postgres -tAc "SELECT pg_create_logical_replication_slot('caus','test_decoding')"
./target/debug/wamn-gates runnerbench --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5491/postgres \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5491/postgres   # runs drive; NOSUPERUSER app role emits, writes never break
# peek: a transactional wamn.causation {run,run,0} rides EACH run's sink-write txn, content == run_id:
docker exec caus-pg psql -U postgres -tAc "SELECT data FROM pg_logical_slot_peek_changes('caus',NULL,1500)" | grep -E "wamn.causation|sink: INSERT"
docker rm -f caus-pg
# In-cluster gate of record (deployed image drives real runs; the reader stitch of
# the identical bytes is already proven at l5i9.12.1's in-cluster R3 + phase G):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev . && kind load docker-image wamn-host:dev --name wamn
```

Mutation harness: scratchpad `mutate_l5i9_12_2.py` — M1 emit dropped from
`build_claim_batch`, M2 `set_current_run` does not store the run, M3 the forgery
guard always passes — each fails a NAMED `wamn_postgres::tests` unit test;
apply/test/restore with sha256, DEBUG builds.

### [EVT-REG / wamn-l5i9.16] registration surface — catalog + minimal API

Docs: docs/event-plane-jetstream.md §5. The **declaration surface** the
materializer (l5i9.17) consumes: a registration = subscribing flow id, entity id
(the rename-proof catalog **entity id**, EVT-OIDMAP — never a table name), a
non-empty op set, an optional JMESPath condition, and an optional JMESPath
partition-key expr. Model + validation in the pure `wamn-event-reg` crate;
storage `catalog.event_registrations` (deploy/sql/catalog-schema.sql, mirrors
`rls_policies` — jsonb doc + denormalized `flow_id`/`entity_id` columns, live-
catalog-scoped not version-tied, tenant-RLS'd, indexed by entity for 11.8 impact
analysis wamn-wvb); minimal CRUD builders in `wamn-api` (`registration` module —
pinned identifiers, `$n` values, `tenant_id` server-side). NO materializer, NO
reader change, NO UI (parked). The condition/partition-key are stored as JMESPath
strings, validated for SYNTAX at write time (the materializer owns evaluation); a
condition referencing `old` ("changed-to") is expressible but its old image needs
REPLICA IDENTITY FULL (l5i9.31) — this surface never flips replica identity.

```bash
cargo test -p wamn-event-reg              # validation rules (entity-by-id, ops non-empty/dedup, JMESPath syntax, schema-version, round-trip)
cargo test -p wamn-api                     # +registration builder shapes + the storage-schema drift guard
cargo clippy -p wamn-event-reg -p wamn-api --all-targets
# Local live-apply gate (throwaway PG): applies the REAL catalog-schema.sql, then
# drives create/list/get/update/delete through the wamn-api builders AS wamn_app
# under a tenant claim — round-trips the document + proves RLS tenant isolation.
# Hermetic (drops+recreates the catalog schema, teardown leaves nothing):
docker run -d --name evtreg-pg -p 55433:5432 -e POSTGRES_PASSWORD=postgres postgres:18
WAMN_API_PG_URL=postgres://postgres:postgres@127.0.0.1:55433/postgres \
  cargo test -p wamn-api --test registration_live
docker rm -f evtreg-pg
# wamn-api is an api-gateway guest dep; confirm the wasm build (dev-deps excluded):
(cd components && cargo build -p api-gateway --target wasm32-wasip2)
```

### [EVT-REG/D24 / wamn-rmxa] publish/migrate-catalog refuse an orphaning publish

Docs: platform-plan decision table D24. Both `publish-catalog` and
`migrate-catalog` REFUSE a catalog that would remove an entity still referenced
by a row in `catalog.event_registrations` — naming every orphaned registration
(id + tenant + entity) across ALL tenants — and never seed or prune
registrations (the owner deletes them via the wamn-api registration surface
first). The pure decision + the `$n` read builder live in `wamn-migrate`
(`check_registration_orphans`, `sql::select_registrations_for_catalog_sql`); the
two `wamn-ctl` verbs share one read-only guard helper
(`publish_catalog::guard_registration_orphans`) that runs BEFORE any mutation.

```bash
cargo test -p wamn-migrate                 # pure decision + mutation-flavored unit tests
cargo clippy -p wamn-migrate -p wamn-ctl --all-targets
# Live gate (throwaway PG): drives the REAL verbs — seed+publish a catalog, register
# entity A as two tenants, attempt a publish/migrate that removes A → REFUSAL naming
# both tenants' rows + NOTHING mutated; delete the registrations → proceeds; and a
# removal of an UNREFERENCED entity proceeds. Hermetic (drops+recreates its schemas):
docker run -d --name wave3-pg-rmxa -p 55431:5432 -e POSTGRES_PASSWORD=postgres postgres:18
WAMN_CTL_PG_URL=postgres://postgres:postgres@127.0.0.1:55431/postgres \
  cargo test -p wamn-ctl --test orphan_guard_live -- --nocapture
docker rm -f wave3-pg-rmxa
```

### [EVT-MAT / wamn-l5i9.17] materializer — CDC events → flow runs (Service-first)

Docs: docs/event-plane-jetstream.md §5 · decisions D19–D24. The Service-first
materializer: a wasi:cli/run SERVICE workload (`spec.service`, E11/D21 + E12 —
deploy/platform/materializer.example.yaml) and the **first `wamn:jetstream`
importer** (the plugin is now wired in the washlet; the doorbell rides the
host's control-plane NATS client). Per event: registration match (rename-proof
entity-id) → tenant guard (unscopable = alertable refusal, never a cross-tenant
enqueue) → causation budget (depth 16; the chain THREADS: the run input carries
`{run,root,depth}`, the flowrunner declares it, so the next hop's envelopes
carry `depth+1`) → condition eval (root-`old` conditions HELD until l5i9.31 —
old-absent is cannot-evaluate, never condition-false) → deterministic
`run_id = <flow>:evt:<stream_seq>` (zero-padded 20, `mint_evt_run_id`) →
write-ahead + `enqueue_evt[_with_policy]_sql` in ONE transaction (REAL
`stream_seq` on the row — E4; key+policy stamp kq0z-coherently) → post-commit
doorbell → ack. Decisions are the PURE `wamn-materializer` crate; the guest
(`components/materializer`) is the effect shell.

```bash
cargo test -p wamn-materializer -p wamn-run-queue          # decide/condition/causation/mint + E4 model/SQL pins
cargo test -p wamn-host --lib plugins::wamn_jetstream      # doorbell subject/tenant map (+ live round-trip w/ WAMN_EVT_NATS_URL)
cargo test -p wamn-host --test jetstream_wit_coherence     # docs WIT == built WIT (doorbell included)
(cd components && cargo build -p materializer --target wasm32-wasip2 --release)
# Live gate — REAL guest + REAL deploy/sql DDL (include_str! — drift-proof) +
# REAL JetStream; 17 asserts: rows/ids/keys/policy, causation thread, distinct
# refusal counters, doorbell rings, burst drain (C-MAT numbers), and a full
# server-side-consumer-delete redelivery proving ON CONFLICT exactly-once:
docker run -d --name mat-pg -p 55461:5432 -e POSTGRES_PASSWORD=matpass postgres:18
docker run -d --name mat-nats -p 44461:4222 nats:2.10 -js
./target/debug/wamn-gates matbench \
  --component components/target/wasm32-wasip2/release/materializer.wasm \
  --admin-database-url postgres://postgres:matpass@127.0.0.1:55461/postgres \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:55461/postgres \
  --nats-url nats://127.0.0.1:44461
docker rm -f mat-pg mat-nats
# In-cluster: rebake host (plugin wiring) + run-worker (flowrunner causation
# thread) + gates (matbench + /bench/materializer.wasm), kind load, then the
# matbench Job / the CDC-write→reader→stream→materializer→run e2e.
```

Mutation harness: scratchpad `mutate_l5i9_17.py` — M1 depth guard off-by-one,
M2 root-`old` detection loses Subexpr context, M3 `enqueue_evt_sql` drops
`stream_seq`, M4 `plan_claim` loses the numeric tiebreak, M6 doorbell-subject
typo — each fails a NAMED unit test; M5 (guest skips the doorbell ring) fails
matbench's `8 doorbell rings` assert. Apply/test/restore with sha256, DEBUG.

### [E10-E2E / wamn-l5i9.57] samplebench — component-driven wamn:jetstream e2e + the js-sample adopter template

Docs: docs/event-plane-jetstream.md §5 · docs/wamn-jetstream.wit (FROZEN 0.1.0).
`components/samples/js-sample` is the **adopter template** — the smallest
wasi:cli/run guest that drives BOTH sides of the frozen `wamn:jetstream@0.1.0`
package and the **first `producer` importer** (the materializer, l5i9.17, only
consumes). It binds a durable pull consumer, drains it, and per event PUBLISHes
a derived message carrying a deterministic `Nats-Msg-Id` (`<prefix>:<input
stream-seq>` — so a redelivered input re-publishes an identical id and dedupes),
then acks; a persistent `publish-rejected` terminates the input. `samplebench`
drives it via CommandPre + the REAL `WamnJetstream` plugin over a throwaway
JetStream (input + output streams), asserting: N fetched+acked, N derived stored
on the output subject with server acks, ack-floor-advanced (rebind fetches
nothing), full-redelivery dedupe (delete the durable → same ids come back
`duplicate = true`, output count unchanged), and the producer error path
(publish to an uncovered subject → `publish-rejected` surfaces as a `js-error`).

```bash
cargo test -p wamn-host --test jetstream_wit_coherence   # docs WIT == host + both vendored guest copies (materializer + js-sample)
(cd components && cargo build -p js-sample --target wasm32-wasip2 --release)
# Local gate — REAL guest + REAL WamnJetstream plugin + REAL JetStream:
docker run -d --name sample-nats -p 44232:4222 nats:2.10 -js
./target/debug/wamn-gates samplebench \
  --component components/target/wasm32-wasip2/release/js-sample.wasm \
  --nats-url nats://127.0.0.1:44232
docker rm -f sample-nats
# In-cluster: rebake gates (samplebench + /bench/js-sample.wasm), kind load,
# then the samplebench Job against the data-plane evt-nats (no Postgres):
#   kubectl -n wamn-system apply -f deploy/gates/samplebench-job.yaml
#   kubectl -n wamn-system wait --for=condition=complete job/samplebench --timeout=300s
#   kubectl -n wamn-system logs job/samplebench
```

### [EVT-CUTOVER / wamn-l5i9.18] shadow equivalence + the per-flow flip (cutbench)

Docs: docs/event-plane-jetstream.md §7 Phase 2 (THE COMPARISON, defined) + §11
(the flip runbook). A registration gains `state: shadow | live` (absent = live
— additive 0.1.x): shadow = the materializer observes into the
`wamn_run.evt_shadow` ledger (no run/queue/doorbell); live = it fires AND the
dispatcher YIELDS the flow from outbox matching (`cdc_live_flows_sql`, read in
the poll transaction) — mutual exclusion replaces the id-collision safety the
v2→v3 run-id change removed. `cutbench` is the gate of record: ONE write
program (a wamn_app session on floor tables carrying the REAL outbox triggers
AND a REAL logical slot) feeds the real dispatcher engine and the embedded
real `wamn-cdc-reader` → JetStream → `materializer.wasm` concurrently; the
comparator joins on (flow, table, op, payload row id) with
`jsonb_populate_record` canonicalization and declared divergence classes,
then the phase-2 flip drills the §11 runbook (flip-window writes fire exactly
once, on the new path; `disp-del` proves the flip is per-flow).

```bash
cargo test -p wamn-event-reg      # state: default-live, shadow round-trip, bogus rejected
cargo test -p wamn-run-queue      # shadow_observe/cdc_live_flows pins + evt_shadow DDL coherence
cargo test -p wamn-dispatcher     # the pure yield filter (live yields; shadow keeps firing)
cargo test -p wamn-gates cutbench # fixture frozen-type + comparator join-key drift guards
# Local live gate (throwaway PG18 `wal_level=logical` + JetStream; ~60s):
docker run -d --name wamn-cut-pg -e POSTGRES_PASSWORD=postgres -p 5449:5432 \
  postgres:18 -c wal_level=logical -c fsync=off
docker run -d --name wamn-cut-nats -p 4262:4222 nats:2.10-alpine -js -sd /data
(cd components && cargo build -p materializer --target wasm32-wasip2)
cargo build -p wamn-gates
./target/debug/wamn-gates cutbench \
  --component components/target/wasm32-wasip2/debug/materializer.wasm \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5449/postgres \
  --nats-url nats://127.0.0.1:4262
# Regressions on the same rig: matbench (default-live registrations + the new
# evt_shadow DDL) and dispatchbench (the missing-catalog probe path).
docker rm -f wamn-cut-pg wamn-cut-nats
# In-cluster gate of record (two-stage image; the fixture postgres carries
# wal_level=logical — deploy/platform/postgres.yaml — apply + restart once):
(cd components && cargo build --release --target wasm32-wasip2)
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/platform/postgres.yaml
kubectl -n wamn-system delete pod -l app=postgres   # pick up the wal_level knob
kubectl -n wamn-system delete job cutbench --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/cutbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/cutbench --timeout=600s
kubectl -n wamn-system logs job/cutbench | tail -45
```

Mutation harness: scratchpad `mutate_l5i9_18.py` — M1 the dispatcher yield
filter dropped (killed by cutbench "dispatcher yields the live flows"), M2 the
guest ignores shadow state (cutbench "shadow fired NOTHING real"), M3
`shadow_observe_sql` loses ON CONFLICT (named unit
`shadow_observe_is_deduped_tenant_scoped_and_ddl_coherent`), M4 the
registration state defaults to shadow (named unit
`state_defaults_to_live_and_live_is_omitted_on_export`); apply/test/restore
with sha256, DEBUG builds.

### [5.14] checkpoint/resume on replica loss

Docs: docs/run-queue.md

```bash
cargo test -p wamn-run-queue   # incl the janitor completion-race guard (shape + live-apply)
cargo clippy -p wamn-run-queue --all-targets && cargo fmt -p wamn-run-queue --check
# Local iteration (reuse the throwaway PG above [wamn-rq-pg on 5459, wamn_app created by
# so NO wasm rebuild — reuse the built flowrunner.wasm):
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/release/wamn-gates --log-level error failoverbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn --mode all
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS lesson;
# HOST change => full docker rebuild (both --target stages + kind load BOTH images):
kubectl -n wamn-system apply -f deploy/gates/failoverbench-job.yaml
kubectl -n wamn-system logs -f job/failoverbench
```

### [5.14] guest-self-claim

Docs: docs/run-queue.md

```bash
cargo test -p wamn-run-store   # incl select_run_dispatch shape (fl3's traceparent seam)
cargo build -p wamn-run-queue --no-default-features   # the guest's pure claim-path core builds alone
cargo clippy -p wamn-dispatcher -p wamn-run-worker -p wamn-gates -p wamn-run-store -p wamn-run-queue --all-targets \
  && cargo fmt -p wamn-dispatcher -p wamn-run-worker -p wamn-gates -p wamn-run-store -p wamn-run-queue --check
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)   # guest CHANGED
cargo clippy --manifest-path components/flowrunner/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/flowrunner/Cargo.toml --check
# Local iteration (throwaway postgres:18 + wamn_app; failoverbench --mode all now includes
# claim/park/heartbeat — the guest CHANGED so rebuild the wasm above first):
docker run -d --rm --name wamn-fqg4-pg -p 5459:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
docker exec wamn-fqg4-pg psql -U postgres -d wamn -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/debug/wamn-gates --log-level error failoverbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn --mode all
docker stop wamn-fqg4-pg
# In-cluster gate of record (failoverbench-job runs claim/park/heartbeat + the failover/
# stages + kind load BOTH images (+ flowbench/testhostbench regress on the new guest):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/gates/failoverbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/failoverbench --timeout=240s
kubectl -n wamn-system logs job/failoverbench
```

### [5.14 / wamn-fqg.9] guest-side partitioned claim

Docs: docs/run-queue.md §Head-unavailability policy + §Per-partition ownership

The guest `run-next` export now also serves `partitioned(key)` runs: when the
global (unpartitioned) `claim_dispatch_sql` is empty it leases a partition
(`acquire_partitions_sql(1)`), claims the earliest HEAD across the partitions it
owns in stream order (`claim_partition_head_sql(1)` — one in flight per key, D20
policy on the row), drives it via the SHARED `execute_claimed` path (renewing the
partition lease per node alongside the run lease), and STEPS DOWN
(`release_partition_sql`) from a just-acquired partition that yields no head. The
WIT is unchanged (`run-next` signature identical) and `RunWorker.drain` loops it
unchanged. The partition SQL/pure builders already existed (host-gated by
queuebench); fqg.9 is their first GUEST caller — the same shape as fqg.4 for
`claim_batch_sql`. All partition builders live in `sql.rs`/`partition.rs` OUTSIDE
the `dispatcher` feature, so `default-features = false` already exposes them —
nothing moved.

```bash
cargo test -p wamn-run-queue --test queue guest_partition_loop_drives_each_key_in_stream_order  # pure: the guest limit-1 loop drives each key in (enqueued_at, stream_seq, run_id) order
cargo clippy -p wamn-run-queue -p wamn-gates --all-targets \
  && cargo fmt -p wamn-run-queue -p wamn-gates --check
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)   # guest CHANGED
cargo clippy --manifest-path components/flowrunner/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/flowrunner/Cargo.toml --check
# Local live gates (throwaway postgres:18 + wamn_app; guest CHANGED so rebuild wasm first):
docker run -d --name wave3-pg-fqg9 -p 55434:5432 -e POSTGRES_PASSWORD=postgres postgres:18
docker exec wave3-pg-fqg9 psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:55434/postgres \
  ./target/debug/wamn-gates --log-level error failoverbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:55434/postgres --mode partition-order
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:55434/postgres \
  ./target/debug/wamn-gates --log-level error failoverbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:55434/postgres --mode partition-failover
docker rm -f wave3-pg-fqg9
```

`failoverbench --mode all` now also runs `partition-order` + `partition-failover`.
`partition-order`: one runner drains two interleaved keyed streams IN STREAM
ORDER per key — `kseq` (equal enqueued_at, distinct stream_seq) + `kenq` (equal
stream_seq, distinct enqueued_at), each seeded so stream order REVERSES run-id
order, so a head decision that dropped either tiebreak re-orders a key — while 5
unordered NULL-key rows drain via the old global claim (exactly once).
`partition-failover`: owner A drives a key's head then dies (its partition lease
force-expired — the queuebench lease-timestamp idiom); replica B reacquires the
key and resumes IN ORDER from the next head with no skipped/duplicated run.
Terminal-BUSINESS-failure wedging of a `blocking` partition head is NOT
fqg.9's scope (D20 wedging covers crash-exhaustion via `janitor_sweep_sql`, and
head-UNAVAILABILITY via `claim_partition_head_sql`; a partitioned head that
RUNS to a terminal `failed` dequeues like the unpartitioned path — filed as a
follow-up). Mutation harness: scratchpad `mutate_fqg9.py` — M1 pure (drop
stream_seq from `partition::stream_key`) fails the pure test; M2 SQL builder
(drop stream_seq from `claim_partition_head_sql`'s blocking arm) + M3 guest loop
(short-circuit `claim_partition_run`) fail `partition-order` live.

### [5.14] production runner (run-worker, fqg.8)

Docs: docs/run-queue.md · Manifests: deploy/platform/runner.yaml + deploy/platform/runner-db.example.yaml

```bash
cargo test -p wamn-run-worker   # owner fallback + drain tally + idle backoff
cargo clippy -p wamn-run-worker -p wamn-gates --all-targets \
  && cargo fmt -p wamn-run-worker -p wamn-gates --check
# Local runnerbench (throwaway postgres:18 + wamn_app; guest UNCHANGED — no wasm rebuild):
docker run -d --name wamn-fqg8-pg -p 5490:5432 -e POSTGRES_PASSWORD=postgres postgres:18
docker exec wamn-fqg8-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
./target/debug/wamn-gates --log-level warn runnerbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5490/postgres \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5490/postgres
# 6 phases: drain + reuse + empty + RUNAWAY (cjv.4 anti-wedge, LOCAL gate of
# record: a never-terminating cyclic flow drives the engine's default 10k
# dispatch budget, ends failed/runaway-budget + DEQUEUES, and the run queued
# behind it still completes — under the phase's own 180s wall guard so a
# budget-removed mutant FAILS instead of hanging; ~1-2 min wall for the 10k
# dispatches) + STREAM + STREAM-RELOAD (fqg.18 record-stream amortization:
# --stream-records record-runs of one flow on one warm instance, per-record
# correctness [exactly-once, full node_runs trail, sink witness] + the
# ms/record measurement — combined claim/checkpoint/complete statements +
# guest plan cache took the local debug number from ~66 to ~32-37 ms/record —
# then a mid-stream version flip must take effect for the following records =
# the plan-cache invalidation guard). Engine units: cargo test -p wamn-runner
# (budget section) + cargo test -p wamn-run-store (fail_kind literal + DDL
# drift guard). Combined-builder shape + live-apply (PREPARE/EXECUTE the real
# claim_dispatch/record+renew/complete+dequeue against deploy DDL incl
# flows.sql): cargo test -p wamn-run-queue (+ WAMN_RUN_QUEUE_PG_URL).
# Mutation harnesses: scratchpad mutate_cjv4.py (6 killed) + mutate_fqg18.py
# (5 killed — cache-never-invalidates, MATERIALIZED fence, renew tail,
# dequeue arm, mark-running arm); NOTE the engine AND the claim path are
# compiled into the GUEST, so those mutants need a flowrunner wasm rebuild
# to reach the live gate.
docker rm -f wamn-fqg8-pg
# In-cluster live smoke = gate of record (HOST changed — the run-worker module +
# flowrunner.wasm baked into the prod image — so FULL rebuild BOTH stages + kind load):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn
# Provision a demo schema (wamn_runner_demo: run-state.sql + run-queue.sql rewritten,
# a flows table + a sink table) via kubectl exec psql, register a fast-cron flow, then:
kubectl -n wamn-system apply -f deploy/platform/dispatcher-projects.example.yaml   # (pointed at the demo)
kubectl -n wamn-system apply -f deploy/platform/dispatcher.yaml
kubectl -n wamn-system apply -f deploy/platform/runner-db.example.yaml
kubectl -n wamn-system apply -f deploy/platform/runner.yaml
kubectl -n wamn-system rollout status deploy/runner --timeout=120s
# Assert a dispatcher-fired cron run was CLAIMED by the runner and driven end-to-end:
#   SELECT status FROM wamn_runner_demo.runs WHERE run_id LIKE 'runner-demo:cron:%'  -> completed
#   + a wamn_runner_demo.sink row + wamn_runner_demo.node_runs rows.
```

### [EXEC-LADDER.1/2/3] rungs 1-3: single-node, linear chain, conditional branch on the deployed runner (wamn-ojm.1/2/3)

Docs: docs/exec-ladder.md · Fixtures: deploy/gates/ladder/rung{1,2,3}.flow.json · Manifest: deploy/gates/ladderproof-job.yaml

`ladderproof --rung <N>` seeds one manual run per case of that rung's flow and
waits for the deployed runner to drive it. Rung 1 is `webhook-in -> respond`;
rung 2 is `webhook-in -> transform{upper} -> transform{reverse} -> respond`
(SEQUENCING + THREADING); rung 3 is a conditional branch + merge
(`in -> cond{true/false} -> yes|no -> out`), driven TWICE (a true and a false
input) to prove correct ROUTING — the conditional's recorded port matches the
predicate, ONLY the taken branch produces a node_run, and its distinct output
threads to the merged result. `--setup` registers EVERY rung's flow so one schema
serves the whole ladder.

```bash
cargo test -p wamn-gates ladderproof   # rung1/2/3 fixture drift-guards (parse + validate) + chain/port/routing units
cargo clippy -p wamn-gates --all-targets && cargo fmt -p wamn-gates --check
# Local end-to-end (throwaway postgres:18 + a background run-worker; guest + host
# UNCHANGED — ladderproof is gates-only, no wasm/host rebuild). Start the runner
# first; it error-drains until --setup provisions the schema + role, then claims:
docker run -d --name wamn-ojm3-pg -p 5491:5432 -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
WAMN_RUNNER=ojm3-local ./target/debug/wamn-run-worker \
  --log-level info \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5491/wamn \
  --tenant demo-tenant --schema wamn_ladder_local \
  --min-idle-ms 250 --max-idle-ms 1500 &                       # error-drains until setup
./target/debug/wamn-gates ladderproof --rung 3 \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5491/wamn \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5491/wamn \
  --schema wamn_ladder_local --tenant demo-tenant --setup   # provision + register rungs + seed both branches + assert
# Rung-2/1 regressions + the mutation loop re-run the client only (schema + runner stay up):
#   ./target/debug/wamn-gates ladderproof --rung 2 --database-url ... --schema wamn_ladder_local --tenant demo-tenant
#   ./target/debug/wamn-gates ladderproof --rung 1 --database-url ... --schema wamn_ladder_local --tenant demo-tenant
#   python3 scratchpad/mutate_ojm3.py   # 3 mutants: fixture drift-guard / gate port assert / in-place edge (routing) swap
kill %1; docker rm -f wamn-ojm3-pg
# In-cluster gate of record (GATES-ONLY: rebuild the gates image, host stage cached;
# the runner reuses the fqg.8 wamn-host:dev):
docker build --target gates -t wamn-gates:dev . && kind load docker-image wamn-gates:dev --name wamn
# Provision the demo schema (sed 's/\bwamn_run\b/wamn_runner_demo/g' over
# deploy/{run-state,run-queue,flows}.sql | kubectl exec psql) + register
# deploy/gates/ladder/rung{1,2,3}.flow.json active as tenant demo-tenant (superuser, RLS bypassed).
kubectl -n wamn-system apply -f deploy/platform/runner-db.example.yaml
kubectl -n wamn-system apply -f deploy/platform/runner.yaml
kubectl -n wamn-system rollout status deploy/runner --timeout=120s
kubectl -n wamn-system apply -f deploy/gates/ladderproof-job.yaml   # --rung 3 (rung-2/1 regressions: edit --rung to 2 / 1)
kubectl -n wamn-system wait --for=condition=complete job/ladderproof --timeout=120s
kubectl -n wamn-system logs job/ladderproof   # -> overall PASS: true
```

### [5.14] shared trigger dispatcher

Docs: docs/run-queue.md

```bash
cargo test -p wamn-run-queue   # incl cron calendar edges + outbox/adaptive decisions
cargo clippy -p wamn-run-queue --all-targets && cargo fmt -p wamn-run-queue --check
# optional live-apply gate (run-state.sql + run-queue.sql now incl the outbox; real
# atomicity + redelivery dedupe, cron last-tick recovery, wake scan, outbox GC
# retention/batch-bound proof [wamn-d8v, outbox_prune_sql]; skips when unset):
docker run -d --rm --name wamn-rq-pg -p 5459:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RUN_QUEUE_PG_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn cargo test -p wamn-run-queue
# the live-apply gate] + a throwaway NATS for the wake/live doorbell hints):
docker run -d --rm --name wamn-rq-nats -p 4232:4222 nats:2.12.8-alpine
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/release/wamn-gates --log-level error dispatchbench \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn \
  --nats-url nats://127.0.0.1:4232 --mode all
docker stop wamn-rq-pg wamn-rq-nats
# dispatchbench modes: cron/outbox/ordering/race/fairness/prune/wake/live/all — `prune`
# (wamn-d8v) drives the maintenance step's outbox GC: batch-bounded drain every
# sweep while saturated, then the 10-min maintenance cadence; retention via
# `wamn-dispatcher --outbox-retention-hours` (default 168 = 7d).
# Mutation harness: scratchpad/mutate_d8v.py (retention sign, batch bound,
# maintenance neuter, stamp inversion — each fails a named test/gate).
# The production service is `wamn-dispatcher --projects-file <json>` (one entry
# In-cluster gate of record (co-located with postgres,
# HOST change => full docker rebuild (both --target stages + kind load BOTH images):
kubectl -n wamn-system apply -f deploy/gates/dispatchbench-job.yaml
kubectl -n wamn-system logs -f job/dispatchbench
```

### [D6/wamn-q3n.1] control-plane registry model crate

Docs: docs/postgres-topology.md, docs/registry-model.md

```bash
cargo test -p wamn-registry
cargo clippy -p wamn-registry --all-targets && cargo fmt -p wamn-registry --check
```

### [D6/wamn-q3n.2] T1 system cluster

Docs: docs/system-cluster.md

```bash
kubectl apply -f deploy/platform/wamn-sysdb.yaml
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 \
  cluster/wamn-sysdb --timeout=300s
# Verify (gate of record — HA + distinct plane + bootstrap + no cpu limit):
kubectl -n wamn-system get cluster wamn-sysdb -o wide   # 3/3 healthy, primary wamn-sysdb-1
kubectl -n wamn-system get svc,secret,pvc -l cnpg.io/cluster=wamn-sysdb  # own -rw/-ro/-r + wamn-sysdb-* + 3 PVCs
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -tAc "SELECT datname, pg_get_userbyid(datdba) FROM pg_database WHERE datname='wamn_system';"
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -tAc "SELECT application_name, state, sync_state FROM pg_stat_replication;"  # 2 streaming replicas
```

### [D6/wamn-q3n.3] system-DB registry schema + the four invariants

Docs: docs/registry-model.md, docs/system-cluster.md

```bash
cargo test -p wamn-registry   # drift-guard (placement cols + env_policies seed vs the model) + inv-1 grep (live-apply skips)
cargo clippy -p wamn-registry --all-targets && cargo fmt -p wamn-registry --check
# cjv.20: the charset/length CHECK backstop on the stored slug/name columns
# (orgs.id/pool_cluster, projects.id, env_policies.name — mirrors validate()
# check_id/check_env/check_name) is pinned by the drift-guard
# `charset_length_checks_backstop_the_stored_slug_names`, proven live by the gate
# below, and mutation-tested (scratchpad/mutate_cjv20.py: 3 mutants — drop the
# orgs.id CHECK / `~`->`~*` case-insensitive / neuter validate_org_id). Pure-crate
# + hand-written SQL — NO in-cluster required (a45 precedent; the live wamn-sysdb
# picks the CHECK up on the next system-schema re-apply — see wamn-cjv.29).
# optional throwaway-PG live-apply gate (WAMN_REGISTRY_PG_URL, superuser url —
# invariants 2/3 + the placement biconditional + the composite (org, env) FK ->
# env_policies(org, name) + the template stamp insert-if-absent + FK integrity +
# the cjv.20 charset CHECKs + saga exactly-once; skips when unset):
docker run -d --rm --name wamn-reg-pg -p 5461:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5461/wamn cargo test -p wamn-registry
docker stop wamn-reg-pg
# IN-CLUSTER gate of record — apply system-schema.sql INTO wamn-sysdb's (wamn-q3n.2)
# wamn_system DB (empty of rows — a DROP+re-apply is safe pre-production only):
{ echo "DROP SCHEMA IF EXISTS registry, provisioning CASCADE; SET ROLE wamn_system;"; \
  cat deploy/sql/system-schema.sql; } | kubectl -n wamn-system exec -i wamn-sysdb-1 \
  -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 -f -
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT schemaname||'.'||tablename FROM pg_tables \
        WHERE schemaname IN ('registry','provisioning') ORDER BY 1;"  # 7 control-plane tables (incl env_policies + dumps)
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT count(*) FROM registry.env_policies;"  # 0 — NO platform seed (8df.4): policies are stamped per org by provision-org --template
```

### [D6/wamn-q3n.6] provision-org

Docs: docs/provisioning.md, docs/postgres-topology.md

```bash
cargo test -p wamn-registry -p wamn-provision -p wamn-ctl   # renderer shape + org-row SQL + drift/subcommand units
cargo clippy -p wamn-registry -p wamn-provision -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-registry -p wamn-provision -p wamn-ctl --check
# CONFLICT mutant). Render CRs locally (no cluster/DB needed — template policies):
./target/debug/wamn-ctl provision-org --org demo --template standard \
  --emit-clusters /tmp/demo-clusters.json --emit-object-store /tmp/demo-os.json \
  --emit-scheduled-backup /tmp/demo-sb.json
# IN-CLUSTER live standup = the gate of record (the wamn-q3n.2 infra precedent;
# port-forwarded wamn-sysdb — reads registry.env_policies for sizing + writes the
# org's placement row — then kubectl-apply the emitted CRs ADDITIVELY (ObjectStore
# BEFORE the clusters, ScheduledBackup after — the wamn-e1g order):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5463:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5463/wamn_system?sslmode=disable" \
  ./target/debug/wamn-ctl provision-org --org demo --template standard \
  --emit-clusters /tmp/demo-clusters.json --emit-object-store /tmp/demo-os.json \
  --emit-scheduled-backup /tmp/demo-sb.json   # renders per-recovery-domain + writes registry.orgs
kubectl apply -f /tmp/demo-os.json -f /tmp/demo-clusters.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 cluster/demo-prod --timeout=300s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/demo-dev  --timeout=300s
kubectl apply -f /tmp/demo-sb.json
# deletes ONLY the new clusters + backup CRs + the org row:
kubectl -n wamn-system delete scheduledbackup demo-prod-backup
kubectl -n wamn-system delete cluster demo-prod demo-dev
kubectl -n wamn-system delete objectstore demo-prod-store
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='demo';"
```

### [D6/wamn-q3n.7] provision-project-env

Docs: docs/provisioning.md, docs/postgres-topology.md

```bash
cargo test -p wamn-provision -p wamn-registry -p wamn-ctl   # renderer/naming + project SQL + drift/subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-ctl --check
# (--cluster given => no DB needed):
./target/debug/wamn-ctl provision-project-env --org demo --project demo --env dev \
  --cluster wamn-pg --emit-database - --emit-role-sql - --emit-privilege-sql - --emit-secret -
# IN-CLUSTER live standup = the gate of record (T3 pool wamn-pg is ALWAYS up; the
# SQL -> Database CR -> privilege SQL in order:
kubectl -n wamn-system exec -i wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "SET ROLE wamn_system; INSERT INTO registry.orgs (id,placement_kind,pool_cluster) \
      VALUES ('demo','pooled','wamn-pg') ON CONFLICT (id) DO NOTHING;"
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5470:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5470/wamn_system?sslmode=disable" \
  ./target/debug/wamn-ctl provision-project-env --org demo --project demo --env dev \
  --connection-limit 20 --emit-database /tmp/db.json --emit-role-sql /tmp/role.sql \
  --emit-privilege-sql /tmp/priv.sql --emit-secret /tmp/secret.json   # reads placement + writes rows
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/role.sql
kubectl apply -f /tmp/db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-demo--demo--dev --timeout=90s
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/priv.sql
# new Database CR + rows, then DROPs the created db (retain leaves it):
kubectl -n wamn-system delete database wamn-db-demo--demo--dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- \
  psql -U postgres -c 'DROP DATABASE IF EXISTS "wamn-db-demo--demo--dev" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='demo';"
```

### [D6/wamn-q3n.8] provisionbench four-tier extension

Docs: docs/provisioning.md, docs/postgres-topology.md

```bash
cargo test -p wamn-registry -p wamn-provision   # saga/named-db builders + drift-guards
cargo clippy -p wamn-registry -p wamn-provision -p wamn-gates --all-targets \
  && cargo fmt -p wamn-registry -p wamn-provision -p wamn-gates --check
# Local iteration (throwaway postgres:18; superuser url provisions wamn_app +
# wamn_system + the per-project-env DBs + the ephemeral registry schema):
docker run -d --rm --name wamn-prov-pg -p 5460:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5460/wamn \
  ./target/debug/wamn-gates --log-level error provisionbench --mode all
# The saga live proof rides the wamn-registry live-apply gate (the .3 block):
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5460/wamn cargo test -p wamn-registry
docker stop wamn-prov-pg
# IN-CLUSTER gate of record = a LIVE DEDICATED-ORG STANDUP (the .6/.7 precedent; the
# registry read/write (the registry-write path is the .6/.7 gate of record):
./target/debug/wamn-ctl provision-org --org gate8 --template standard \
  --emit-clusters /tmp/gate8-clusters.json --emit-object-store /tmp/gate8-os.json \
  --emit-scheduled-backup /tmp/gate8-sb.json
kubectl apply -f /tmp/gate8-os.json -f /tmp/gate8-clusters.json   # ObjectStore first (prod is backed)
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 cluster/gate8-prod --timeout=300s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/gate8-dev  --timeout=180s
for E in prod dev; do C=gate8-$E; \
  ./target/debug/wamn-ctl provision-project-env --org gate8 --project app --env $E \
    --cluster $C --emit-database /tmp/db-$E.json --emit-role-sql /tmp/role-$E.sql \
    --emit-privilege-sql /tmp/priv-$E.sql --emit-secret /tmp/sec-$E.json; \
  kubectl -n wamn-system exec -i $C-1 -c postgres -- psql -U postgres -f - < /tmp/role-$E.sql; \
  kubectl apply -f /tmp/db-$E.json; \
  kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-gate8--app--$E --timeout=90s; \
  kubectl -n wamn-system exec -i $C-1 -c postgres -- psql -U postgres -f - < /tmp/priv-$E.sql; done
# wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown deletes ONLY the new resources:
kubectl -n wamn-system delete database wamn-db-gate8--app--prod wamn-db-gate8--app--dev
kubectl -n wamn-system delete cluster gate8-prod gate8-dev
kubectl -n wamn-system delete objectstore gate8-prod-store --ignore-not-found
```

### [D6/wamn-q3n.9] demote the shipped shared cluster to the T3 trials pool

Docs: docs/postgres-topology.md, docs/provisioning.md

```bash
cargo test -p wamn-registry -p wamn-ctl   # Org::pooled placement + pooled-vs-dedicated subcommand units
cargo clippy -p wamn-registry -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-registry -p wamn-ctl --check
# Plan a pooled org locally (no DB needed — omit --system-database-url):
./target/debug/wamn-ctl provision-org --org trialco --template trials --pool wamn-pg
# IN-CLUSTER gate of record = a LIVE T3 trials-org standup (the .6/.7 precedent; T3
# port-forward (check `ss -ltn | grep 547` first):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5473:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5473/wamn_system?sslmode=disable" \
  ./target/debug/wamn-ctl provision-org --org t3gate --template trials --pool wamn-pg   # records registry.orgs (pooled|wamn-pg), NO CRs
# provision-project-env WITHOUT --cluster reads placement from the registered row -> wamn-pg:
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5473/wamn_system?sslmode=disable" \
  ./target/debug/wamn-ctl provision-project-env --org t3gate --project demo --env dev \
  --connection-limit 15 --emit-database /tmp/t3-db.json --emit-role-sql /tmp/t3-role.sql \
  --emit-privilege-sql /tmp/t3-priv.sql --emit-secret /tmp/t3-secret.json   # Database CR cluster == wamn-pg
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t3-role.sql
kubectl apply -f /tmp/t3-db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-t3gate--demo--dev --timeout=90s
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t3-priv.sql
# org's Database CR + DB + registry.orgs row (cascades projects + project_envs):
kubectl -n wamn-system delete database wamn-db-t3gate--demo--dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- \
  psql -U postgres -c 'DROP DATABASE IF EXISTS "wamn-db-t3gate--demo--dev" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='t3gate';"
```

### [D6/wamn-q3n.10] scheduled per-project-env logical dumps

Docs: docs/postgres-topology.md, docs/provisioning.md

```bash
cargo test -p wamn-provision -p wamn-registry -p wamn-ctl   # renderers/builders + record_dump SQL + drift/subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-ctl --check
# Render locally (no DB — the cadence is --schedule, default daily 03:00):
./target/debug/wamn-ctl dump-project-env --org demo --project app --env prod \
  --emit-cronjob - --emit-job -
# optional live gates (throwaway postgres:18; superuser url): (a) the ARTIFACT
# idempotent + byte_size-refresh proof rides the wamn-q3n.3 storage gate:
docker run -d --rm --name wamn-dump-pg -p 5462:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DUMP_PG_URL=postgres://postgres:postgres@127.0.0.1:5462/wamn \
  cargo test -p wamn-provision --test dump
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5462/wamn cargo test -p wamn-registry
docker stop wamn-dump-pg
# IN-CLUSTER gate of record (the .6/.7/.9 precedent; T3 pool wamn-pg + T1 wamn-sysdb
# (writing the T1 registry's OWN DB IS .10's job; NEVER touch wamn-pg/postgres.yaml):
awk '/^CREATE TABLE provisioning\.dumps/{f=1} f{print} f&&/^\);/{exit}' deploy/sql/system-schema.sql \
  | { echo "SET ROLE wamn_system;"; cat; } | kubectl -n wamn-system exec -i wamn-sysdb-1 \
  -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 -f -
# it, then dump+restore. PICK CLEAN unused ports (check `ss -ltn | grep 547`):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5474:5432 &
kubectl -n wamn-system port-forward svc/wamn-pg-rw 5475:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
PGPW=$(kubectl -n wamn-system get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
SYS="postgres://postgres:${SYSPW}@127.0.0.1:5474/wamn_system?sslmode=disable"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl provision-org --org t10gate --template trials --pool wamn-pg
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl provision-project-env \
  --org t10gate --project demo --env dev --connection-limit 10 \
  --emit-database /tmp/t10-db.json --emit-role-sql /tmp/t10-role.sql \
  --emit-privilege-sql /tmp/t10-priv.sql --emit-secret /tmp/t10-secret.json
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t10-role.sql
kubectl apply -f /tmp/t10-db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-t10gate--demo--dev --timeout=90s
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t10-priv.sql
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -d "wamn-db-t10gate--demo--dev" \
  -c "CREATE TABLE parts (id int primary key, sku text, weight_kg numeric(8,3)); INSERT INTO parts VALUES (1,'bolt',0.125),(2,'nut',0.050),(3,'washer',0.008);"
# Dump the REAL project-env DB (records the dump in the wamn-sysdb catalog), then restore:
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl dump-project-env --org t10gate --project demo --env dev \
  --database-url "postgres://postgres:${PGPW}@127.0.0.1:5475/wamn-db-t10gate--demo--dev?sslmode=disable" \
  --run-now --out-dir /tmp/t10-dump
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres -c 'CREATE DATABASE wamn_dump_scratch_t10;'
pg_restore --no-owner --no-privileges \
  -d "postgres://postgres:${PGPW}@127.0.0.1:5475/wamn_dump_scratch_t10?sslmode=disable" /tmp/t10-dump/*/
# weights intact) + the provisioning.dumps row in wamn-sysdb (fmt=directory, byte_size):
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres -d wamn_dump_scratch_t10 \
  -tAc "SELECT count(*), sum(weight_kg) FROM parts;"
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT object_key, format, byte_size FROM provisioning.dumps WHERE org='t10gate';"
# projects+project_envs+dumps:
kubectl -n wamn-system delete database wamn-db-t10gate--demo--dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres \
  -c 'DROP DATABASE IF EXISTS "wamn-db-t10gate--demo--dev" WITH (FORCE);' \
  -c 'DROP DATABASE IF EXISTS wamn_dump_scratch_t10 WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "DELETE FROM registry.orgs WHERE id='t10gate';"
```

### [D6/wamn-q3n.11] restore per-project-env logical dumps

Docs: docs/postgres-topology.md, docs/provisioning.md

```bash
cargo test -p wamn-provision -p wamn-registry -p wamn-ctl   # restore builders + select_latest shape/drift + subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-ctl --check
# Render/plan locally (no cluster/DB needed — explicit --dump-dir, render only):
./target/debug/wamn-ctl restore-project-env --org demo --project app --env dev \
  --database-url postgres://postgres:postgres@127.0.0.1:5468/postgres \
  --dump-dir /tmp/some-dump --help >/dev/null   # (see the subcommand flags)
# optional live gates (throwaway postgres:18; superuser url): (a) the restore
# wamn-q3n.3 storage gate:
docker run -d --rm --name wamn-restore-pg -p 5468:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RESTORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5468/wamn \
  cargo test -p wamn-provision --test restore
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5468/wamn cargo test -p wamn-registry
docker stop wamn-restore-pg
# IN-CLUSTER gate of record = a LIVE restore standup on the T3 pool (the .6/.7/.9/.10
# CLEAN unused ports (check `ss -ltn | grep 547`):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5476:5432 &
kubectl -n wamn-system port-forward svc/wamn-pg-rw 5477:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
PGPW=$(kubectl -n wamn-system get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
SYS="postgres://postgres:${SYSPW}@127.0.0.1:5476/wamn_system?sslmode=disable"
PGADMIN="postgres://postgres:${PGPW}@127.0.0.1:5477/postgres?sslmode=disable"
DB="wamn-db-t11gate--demo--dev"; DUMPROOT=$(mktemp -d)
# Register a pooled org + provision a project-env DB on wamn-pg (the .7/.9 path), seed:
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl provision-org --org t11gate --template trials --pool wamn-pg
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl provision-project-env \
  --org t11gate --project demo --env dev --connection-limit 10 \
  --emit-database /tmp/t11-db.json --emit-role-sql /tmp/t11-role.sql \
  --emit-privilege-sql /tmp/t11-priv.sql --emit-secret /tmp/t11-secret.json
psql "$PGADMIN" -q -f /tmp/t11-role.sql
kubectl apply -f /tmp/t11-db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/$DB --timeout=90s
psql "$PGADMIN" -q -f /tmp/t11-priv.sql
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" \
  -c "CREATE TABLE parts (id int primary key, sku text, weight_kg numeric(8,3)); INSERT INTO parts VALUES (1,'bolt',0.125),(2,'nut',0.050),(3,'washer',0.008);"
# Dump it (records the REAL wamn-sysdb catalog), then RESTORE-to-last-dump into scratch:
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl dump-project-env --org t11gate --project demo --env dev \
  --database-url "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" --run-now --out-dir "$DUMPROOT"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl restore-project-env --org t11gate --project demo --env dev \
  --database-url "$PGADMIN" --dump-root "$DUMPROOT"   # reads the catalog -> scratch DB
# row (mutate live -> restore -> stale gone):
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/wamn-restore-t11gate--demo--dev?sslmode=disable" \
  -tAc "SELECT count(*), sum(weight_kg) FROM parts;"
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" -c "INSERT INTO parts VALUES (99,'STALE',9.999);"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl restore-project-env --org t11gate --project demo --env dev \
  --database-url "$PGADMIN" --dump-root "$DUMPROOT" --in-place --confirm
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" -tAc "SELECT count(*) FROM parts;"  # 3 (stale gone)
# projects+project_envs+dumps:
kubectl -n wamn-system delete database $DB
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres \
  -c 'DROP DATABASE IF EXISTS "wamn-db-t11gate--demo--dev" WITH (FORCE);' \
  -c 'DROP DATABASE IF EXISTS "wamn-restore-t11gate--demo--dev" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "DELETE FROM registry.orgs WHERE id='t11gate';"
```

### [D6/wamn-q3n.13] tier-move / promotion tooling — RETIRED (D18, wamn-8df.3)

Docs: docs/provisioning.md, docs/deployment-model.md

`move-org-tier` + `wamn_provision::tier_move` are removed with the `Tier` enum.
A placement change is one case of the unified `copy(src -> dst)` operation
(`wamn-8df.5`, with a mandatory quiesce+verify cutover gate); until it lands, a
cross-cluster move is the manual runbook: `dump-project-env` -> `provision-org`
(the new placement) -> `provision-project-env` -> `restore-project-env` ->
update the org's placement row.

### [D6/wamn-q3n.14] dedicated-per-env (T4) — now an env policy, not a tier (D18)

Docs: docs/postgres-topology.md, docs/deployment-model.md

The wamn-q3n.14 canary special case (`canary_cluster` column + two CHECKs +
`Org::cluster_for_env`) is retired (wamn-8df.3). The T4 shape is a `canary` env
policy with its **own** recovery domain; shared-with `prod` reproduces the old
T2 collapse instead. The dedicated standup itself is the `[D6/wamn-q3n.6]` gate.

```bash
# Since wamn-8df.4 the T4 shape is a TEMPLATE: `provision-org --org <org>
# --template dedicated` stamps canary(own) at provision time — three clusters
# (<org>-dev/-canary/-prod), each sized by the org's policy. To flip an EXISTING
# org's canary to its own recovery domain instead, edit THAT ORG's row (policies
# are org-scoped — no other org is affected):
kubectl -n wamn-system exec -i wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "SET ROLE wamn_system; INSERT INTO registry.env_policies
      (org, name, recovery_domain, promotion_rank, instances,
       storage, cpu, memory, image, backup_cadence, wal_retention, hibernation)
      VALUES ('<org>', 'canary', '\"own\"'::jsonb, 20, 2, '2Gi', '200m', '256Mi',
              'ghcr.io/cloudnative-pg/postgresql:18', '0 0 */6 * * *', '14d', 'off')
      ON CONFLICT (org, name) DO UPDATE SET recovery_domain = '\"own\"'::jsonb;"
# Re-running provision-org (any template) re-renders from the org's own rows;
# provision-project-env --env canary derives <org>-canary via cluster_of.
# Remove the policy when done (the composite (org, env) FK blocks removal while in use):
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "DELETE FROM registry.env_policies WHERE org='<org>' AND name='canary';"
```

### [ARCH/wamn-8df.4] templates + org-scoped env policies (the Tier successor)

Docs: docs/deployment-model.md, docs/registry-model.md, docs/provisioning.md

```bash
cargo test -p wamn-registry -p wamn-ctl -p wamn-gates   # Template presets + OrgEnvPolicy + org-scoped validate/resolve/SQL + subcommand units
cargo clippy -p wamn-registry -p wamn-ctl -p wamn-gates --all-targets \
  && cargo fmt -p wamn-registry -p wamn-ctl -p wamn-gates --check
# Throwaway-PG live gates (superuser url): the storage live-apply (composite
# (org, env) FK + stamp insert-if-absent + cross-org isolation + whole-org
# cascade) + provisionbench --mode all (tier scenarios stamp template policies):
docker run -d --rm --name wamn-8df4-pg -p 5494:5432 -e POSTGRES_PASSWORD=postgres postgres:18
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5494/postgres cargo test -p wamn-registry
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5494/postgres \
  ./target/debug/wamn-gates --log-level error provisionbench --mode all
# Subcommand smoke (apply role + system-schema.sql into the throwaway DB as
# wamn_system first — the .3 recipe): standard + dedicated orgs COEXIST (T2/T4),
# canary derives per-org, a customized row survives a re-stamp:
export WAMN_SYSTEM_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5494/postgres
./target/debug/wamn-ctl provision-org --org smoke1 --template standard  --emit-clusters /tmp/s1.json ...  # 2 clusters (canary -> prod)
./target/debug/wamn-ctl provision-org --org smoke2 --template dedicated --emit-clusters /tmp/s2.json ...  # 3 clusters (smoke2-canary)
./target/debug/wamn-ctl provision-project-env --org smoke1 --project app --env canary ...  # cluster smoke1-prod
./target/debug/wamn-ctl provision-project-env --org smoke2 --project app --env canary ...  # cluster smoke2-canary
docker stop wamn-8df4-pg
# 5 mutants killed (apply/test/restore, debug builds — scratchpad/mutate_8df4.py):
# M1 standard-canary->Own (template unit), M2 stamp DO NOTHING->DO UPDATE (unit +
# live customization-survives), M3 policy read drops org key (unit + live
# cross-org probe), M4 provision-org stamps nothing (scripted project-env
# refusal), M5 validate env check any-org (org-scoping unit).
# IN-CLUSTER gate of record: re-apply system-schema.sql into wamn-sysdb (the
# [D6/wamn-q3n.3] block — org-scoped env_policies, NO seed), rebuild + kind-load
# wamn-gates, run deploy/gates/provisionbench-job.yaml, then a live TEMPLATE-STAMPED
# standup: tpl1 (standard) + tpl2 (dedicated) coexisting — tpl1 canary derives
# tpl1-prod while tpl2 renders/holds tpl2-canary. Teardown deletes ONLY the new
# clusters/CRs/org rows (org DELETE cascades policies + project-envs).
```

### [ARCH/wamn-8df.5] unified copy — copy-project-env (deploy/promote/clone/move)

Docs: docs/deployment-model.md §4, docs/provisioning.md

```bash
cargo test -p wamn-provision copy      # the pure plan (clone vs cutover pipeline, unbuilt axes, quiesce/verify builders)
cargo test -p wamn-registry            # select_saga shape + the 'copy' kind literal drift-guard
cargo test -p wamn-migrate             # select_applied_catalogs shape
cargo test -p wamn-ctl                # driver units (incl. the shared apply_catalog_target refactor)
cargo clippy -p wamn-provision -p wamn-registry -p wamn-migrate -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-migrate -p wamn-ctl --check
# Throwaway-PG e2e gate (scratchpad/e2e_8df5.sh; postgres:18 on :5496): builds a
# src project-env (catalog via migrate-catalog + rows + a flow + RLS policy rows)
# and proves, 20 asserts:
#   R  --cutover without --system-database-url is REFUSED (the gate needs the T1 record)
#   A  cross-org DEFINITION clone ("deploy an app"): catalog applied in the dst env,
#      data tables exist, flow registration + RLS rows copied, the compiled RLS
#      policy LIVE on the dst table (pg_policies), zero rows carried, re-copy idempotent
#   C  DATA copy into a pre-populated dst FAILS verify (row counts differ) and the
#      saga records status=failed
#   B  the MOVE (both + cutover): saga completed with every step recorded (5/5),
#      dst holds rows+flow+policies+grants, snapshot recorded in provisioning.dumps,
#      and the src is quiesced — a post-cutover write from a FRESH session is
#      refused read-only (25006)
#   B2 a re-move with --deprovision-old --confirm: six-step saga completed, the
#      retained src database dropped
# Registry/migrate/provision live-apply regressions on the same throwaway:
export U=postgres://postgres:postgres@127.0.0.1:5496/postgres
WAMN_REGISTRY_PG_URL=$U cargo test -p wamn-registry --test storage   # incl. the copy-kind saga probe
WAMN_MIGRATE_PG_URL=$U cargo test -p wamn-migrate --test migrate
WAMN_DUMP_PG_URL=$U WAMN_RESTORE_PG_URL=$U WAMN_PROVISION_PG_URL=$U cargo test -p wamn-provision
# 6 mutants killed (apply/test/restore, debug builds — scratchpad/mutate_8df5.py):
# M1 plan drops Quiesce (pure unit), M2 quiesce SQL read-only OFF (unit),
# M3 driver verify neutered (e2e scenario C), M4 saga advance no-op — the cutover
# gate REFUSES (e2e scenario B), M5 the sagas kind CHECK loses 'copy' (drift),
# M6 --disable-triggers dropped from the data-only restore (unit).
# IN-CLUSTER gate of record: a live CROSS-CLUSTER move — a pooled src project-env
# on wamn-pg copied --include both --cutover to a dedicated dst cluster with the
# saga recorded in the REAL wamn-sysdb (apply the additive sagas_kind_check ALTER
# first), quiesce proven on the live src, then --deprovision-old. Teardown deletes
# ONLY the new clusters/CRs/org rows; wamn-pg / wamn-sysdb untouched.
```

### [D6/wamn-e1g] per-org WAL/PITR via the Barman Cloud plugin + the shared object

Docs: docs/postgres-topology.md, docs/provisioning.md

```bash
cargo test -p wamn-provision -p wamn-ctl   # backup renderer + policy knobs + org/dump wiring + subcommand units
cargo clippy -p wamn-provision -p wamn-ctl -p wamn-registry -p wamn-gates --all-targets \
  && cargo fmt -p wamn-provision -p wamn-ctl -p wamn-registry -p wamn-gates --check
# Render a dedicated org's backup CRs locally (no cluster/DB needed; the prod
# policy's backup_cadence/wal_retention drive the CRs):
./target/debug/wamn-ctl provision-org --org demo --template standard \
  --emit-clusters /tmp/demo-clusters.json \
  --emit-object-store /tmp/demo-os.json --emit-scheduled-backup /tmp/demo-sb.json
# IN-CLUSTER gate of record = a LIVE WAL/PITR standup (the .6/.14 precedent; T3 pool
# precedent — the shared-cluster guardrail forbids re-applying wamn-pg/wamn-sysdb):
kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.21.0/cert-manager.yaml
kubectl -n cert-manager wait --for=condition=Available deploy --all --timeout=180s
kubectl apply -f deploy/infra/barman-cloud-plugin.yaml
kubectl -n cnpg-system rollout status deploy/barman-cloud --timeout=180s
kubectl apply -f deploy/infra/minio.yaml
kubectl -n wamn-system rollout status deploy/minio --timeout=150s
kubectl -n wamn-system wait --for=condition=complete job/minio-init --timeout=120s
# backup CRs, not the registry row), apply ObjectStore -> Clusters -> ScheduledBackup:
env -u WAMN_SYSTEM_ADMIN_URL ./target/debug/wamn-ctl provision-org --org e1gate --template standard \
  --emit-clusters /tmp/e1-clusters.json \
  --emit-object-store /tmp/e1-os.json --emit-scheduled-backup /tmp/e1-sb.json
kubectl apply -f /tmp/e1-os.json                             # ObjectStore BEFORE the cluster
kubectl apply -f /tmp/e1-clusters.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 cluster/e1gate-prod --timeout=300s
kubectl apply -f /tmp/e1-sb.json                             # ScheduledBackup AFTER (immediate base backup)
# forbids re-applying the running clusters here):
kubectl -n wamn-system delete cluster e1gate-restore e1gate-prod e1gate-dev
kubectl -n wamn-system delete objectstore e1gate-prod-store
kubectl -n wamn-system delete scheduledbackup e1gate-prod-backup
```

### [2.4] per-project system schema v1

Docs: docs/app-schema.md

```bash
cargo test -p wamn-sysschema     # unit (status literals + table manifest) + drift-guard
cargo clippy -p wamn-sysschema --all-targets && cargo fmt -p wamn-sysschema --check
# optional live-apply gate (throwaway postgres:18; superuser url provisions
# when unset):
docker run -d --rm --name wamn-as5-pg -p 5466:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_SYSSCHEMA_PG_URL=postgres://postgres:postgres@127.0.0.1:5466/wamn cargo test -p wamn-sysschema
docker stop wamn-as5-pg
```

### [2.5] migration engine (crates/wamn-migrate + wamn-ctl migrate-catalog)

Docs: docs/migration-engine.md

```bash
cargo test -p wamn-migrate     # unit (guards/gate/dry-run/rollback) + drift-guard + live-apply
cargo test -p wamn-ctl --lib migrate_catalog   # the subcommand's bare-ident + param-map units
cargo clippy -p wamn-migrate -p wamn-host --all-targets \
  && cargo fmt -p wamn-migrate -p wamn-host --check
# optional live-apply gate (throwaway postgres:18; superuser url — provisions
# unset):
docker run -d --rm --name wamn-migrate-pg -p 5467:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_MIGRATE_PG_URL=postgres://postgres:postgres@127.0.0.1:5467/wamn cargo test -p wamn-migrate
docker stop wamn-migrate-pg
# The production tool is `wamn-ctl migrate-catalog --admin-database-url <superuser>
```

### [3.1] metadata catalog schema crate (crates/wamn-catalog)

Docs: docs/catalog-model.md

```bash
cargo test -p wamn-catalog
cargo clippy -p wamn-catalog --all-targets && cargo fmt -p wamn-catalog --check
# regenerate the published JSON Schema contract after changing the types:
cargo run -p wamn-catalog --example print-schema > docs/catalog-model.schema.json
# cjv.5 expression-chaining guard (unsafe_expression_reason): the Check (here) and
# RolePredicate (wamn-rls) validators reject a top-level ';', unbalanced parens, or
# a comment-open. Mutation harness (5 mutants, each fails a named test in
# wamn-catalog/wamn-rls): scratchpad/mutate_cjv5.py.
```

### [3.2] DDL compiler crate (crates/wamn-ddl)

Docs: docs/run-queue.md, docs/ddl-compiler.md

```bash
cargo test -p wamn-ddl
cargo clippy -p wamn-ddl --all-targets && cargo fmt -p wamn-ddl --check
# optional live-apply gates (emitted SQL + outbox-trigger behavior [same-txn
# superuser URL — provisions wamn_app + ephemeral schemas; skips when unset):
docker run -d --rm --name wamn-ddl-pg -p 5451:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DDL_PG_URL=postgres://postgres:postgres@127.0.0.1:5451/wamn cargo test -p wamn-ddl
docker stop wamn-ddl-pg
# The WAMN_DDL_PG_URL run includes the cjv.5 live proof
# chaining_check_expression_never_reaches_postgres: a chaining Check is rejected at
# compile time so its DROP never reaches Postgres (a neutered guard would apply it
# and fail).
```

### [3.4] schema versioning & environments crate (crates/wamn-schema)

Docs: docs/schema-lifecycle.md

```bash
cargo test -p wamn-schema
cargo clippy -p wamn-schema --all-targets && cargo fmt -p wamn-schema --check
# optional storage check (the whole standalone schema re-applies on a throwaway
# PG18; it assumes a pre-existing wamn_app role, as in production):
docker run -d --rm --name wamn-cat-pg -p 5452:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
docker exec -i wamn-cat-pg psql -U postgres -d wamn -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
docker exec -i wamn-cat-pg psql -v ON_ERROR_STOP=1 -U postgres -d wamn \
  < deploy/sql/catalog-schema.sql
docker stop wamn-cat-pg
```

### [3.5] RLS policy builder crate (crates/wamn-rls)

Docs: docs/rls-builder.md

```bash
cargo test -p wamn-rls
cargo clippy -p wamn-rls --all-targets && cargo fmt -p wamn-rls --check
# optional live-apply gate (floor + compiled policy on a throwaway PG; asserts
# no-user-claim denies all; superuser URL provisions wamn_app; skips when unset):
docker run -d --rm --name wamn-rls-pg -p 5453:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RLS_PG_URL=postgres://postgres:postgres@127.0.0.1:5453/wamn cargo test -p wamn-rls
docker stop wamn-rls-pg
```

### [3.6] seed-data & fixtures crate (crates/wamn-seed)

Docs: docs/seed-data.md

```bash
cargo test -p wamn-seed
cargo clippy -p wamn-seed --all-targets && cargo fmt -p wamn-seed --check
# optional live-apply gate (floor + compiled seed on a throwaway PG; loads TWICE
# when unset):
docker run -d --rm --name wamn-seed-pg -p 5454:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_SEED_PG_URL=postgres://postgres:postgres@127.0.0.1:5454/wamn cargo test -p wamn-seed
docker stop wamn-seed-pg
```

### [4.1] REST API gateway (crates/wamn-api + components/api-gateway)

Docs: docs/api-gateway.md

```bash
cargo test -p wamn-api
cargo clippy -p wamn-api --all-targets && cargo fmt -p wamn-api --check
# cjv.6: every list appends the unique `id ASC` tiebreaker so OFFSET pagination is
# stable under any user sort (C5-1). Mutation (revert to the guarded append -> both
# sort_and_paginate_are_capped_and_parametrized and user_sort_still_appends_the_id_tiebreaker
# fail): scratchpad/mutate_cjv6.py.
# wamn_app + seeds two tenants + the catalog snapshot the gateway reads):
docker run -d --rm --name wamn-api-pg -p 5455:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
REL=components/target/wasm32-wasip2/release
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5455/wamn \
  ./target/release/wamn-gates --log-level error apibench \
  --api-gateway $REL/api_gateway.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5455/wamn --mode all
docker stop wamn-api-pg
# In-cluster gate of record (co-located with Postgres, no cpu limit — S2 lesson;
# WAMN_PG_ADMIN_URL is the superuser used only to provision the ephemeral schema):
kubectl -n wamn-system apply -f deploy/gates/apibench-job.yaml
kubectl -n wamn-system logs -f job/apibench
```

### [4.1b] api-gateway SERVING deployment + catalog snapshot

Docs: docs/api-gateway.md

```bash
cargo test -p wamn-host -p wamn-gates   # publish-catalog ident test + apifixture drift-guard
cargo clippy -p wamn-host -p wamn-gates --all-targets && cargo fmt -p wamn-host -p wamn-gates --check
# In-cluster proof of record (needs the kind 'wamn' cluster + operator + postgres):
docker build --target host -t wamn-host:dev . \
  && docker build --target gates -t wamn-gates:dev .   # cached; two tags, one build
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kind load docker-image registry:2 --name wamn
kubectl -n wamn-system apply -f deploy/platform/registry.yaml
kubectl -n wamn-system rollout status deploy/registry --timeout=60s
kubectl -n wamn-system port-forward svc/registry 5000:5000 &
wash push localhost:5000/wamn/api-gateway:dev \
  components/target/wasm32-wasip2/release/api_gateway.wasm --insecure
# The host group gains --allow-insecure-registries + WAMN_PG_URL:
helm upgrade --install -n wamn-system wamn \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.5.2 \
  -f deploy/infra/values-wamn.yaml
kubectl -n wamn-system rollout status deploy/hostgroup-default --timeout=150s
# Provision the project schema/floor + seed + publish the snapshot:
kubectl -n wamn-system create configmap proof-catalog \
  --from-file=proof-catalog.json=deploy/poc/proof-catalog.json
kubectl -n wamn-system apply -f deploy/gates/publish-catalog-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/publish-catalog --timeout=120s
# Deploy the gateway workload, then prove it serves over the network:
kubectl -n wamn-system apply -f deploy/platform/api-gateway-workload.yaml
kubectl -n wamn-system apply -f deploy/gates/apiproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/apiproof --timeout=180s
kubectl -n wamn-system logs job/apiproof
```

### [POC-DM1] data model via the catalog API (wamn-521, P1 build)

Docs: docs/poc-material-receiving.md, docs/poc-dm1.md

```bash
cargo test -p wamn-dm1     # drift-guard + compile checks + live-apply gate (skips w/o WAMN_DM1_PG_URL)
cargo clippy -p wamn-dm1 --all-targets && cargo fmt -p wamn-dm1 --check
# optional throwaway-PG live-apply gate (superuser url — provisions wamn_app,
# skips when unset):
docker run -d --rm --name wamn-dm1-pg -p 5463:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DM1_PG_URL=postgres://postgres:postgres@127.0.0.1:5463/wamn cargo test -p wamn-dm1
docker stop wamn-dm1-pg
# NOTHING in-cluster (a catalog + schema deliverable, the migrate/rls/seed
```

### [POC-F1] receipt-received sync flow end-to-end (P1 exit, wamn-067)

Docs: docs/poc-f1.md

```bash
cargo test -p wamn-f1        # decimal/payload/evaluate/shapes + catalog & flow drift-guards
cargo clippy -p wamn-f1 --all-targets && cargo fmt -p wamn-f1 --check
(cd components && cargo build --release --target wasm32-wasip2 -p poc-webhook-f1)
cargo clippy --manifest-path components/poc-webhook-f1/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/poc-webhook-f1/Cargo.toml --check
cargo test -p wamn-gates    # f1fixture coherence (burst = 20 receipts / 3 out-of-spec /
# cross-check incl expand=line). Local iteration (throwaway PG; superuser
# provisions the ephemeral schema):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/sql/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
REL=components/target/wasm32-wasip2/release
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5450/wamn \
  ./target/release/wamn-gates --log-level error f1bench \
  --webhook-entry $REL/poc_webhook_f1.wasm --api-gateway $REL/api_gateway.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS
# lesson; ephemeral schema => shared-PG safe; bench Jobs run SEQUENTIALLY):
kubectl -n wamn-system apply -f deploy/gates/f1bench-job.yaml
kubectl -n wamn-system logs -f job/f1bench
# (sync + burst + DB audit + REST):
wash push localhost:5000/wamn/poc-webhook-f1:dev \
  components/target/wasm32-wasip2/release/poc_webhook_f1.wasm --insecure
kubectl -n wamn-system create configmap f1-fixtures \
  --from-file=poc-receiving.catalog.json=crates/wamn-catalog/tests/fixtures/poc-receiving.catalog.json \
  --from-file=f1-flow.json=deploy/poc/f1-flow.json \
  --from-file=f1-seed.dataset.json=deploy/poc/f1-seed.dataset.json
kubectl -n wamn-system apply -f deploy/poc/f1-provision-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/f1-provision --timeout=120s
kubectl -n wamn-system apply -f deploy/poc/f1-workloads.yaml
kubectl -n wamn-system apply -f deploy/gates/f1proof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/f1proof --timeout=180s
kubectl -n wamn-system logs job/f1proof

docker build --target host -t wamn-host:dev . \
  && docker build --target gates -t wamn-gates:dev .   # fork git dep fetched in the builder stage
```

### [EVT-REPLICA-IDENT / wamn-l5i9.31] per-entity REPLICA IDENTITY FULL reconciler

Docs: docs/event-plane-jetstream.md §5 ("Old images") + docs/provisioning.md
(`reconcile-replica-identity`). `REPLICA IDENTITY FULL` is a platform-managed
per-entity knob (l5i9.1 decision d): an entity runs FULL only when a registered
row-event needs the OLD image — any registration whose condition reads root
`old` ("changed-to") OR that subscribes to `delete` — and DEFAULT (pkey-only)
everywhere else keeps WAL minimal (the global default is never flipped). The
pure decision + SQL builders live in `wamn-migrate`
(`reconcile_replica_identity`, `alter_replica_identity_sql`,
`select_replica_identity_sql`); the root-`old` detection is the SINGLE
`wamn_event_reg` detector the materializer's per-event old-absent guard also
keys on. The `wamn-ctl reconcile-replica-identity` verb reads the catalog's
registrations across ALL tenants + each table's `pg_class.relreplident`, plans
the idempotent flips, and (unless `--dry-run`) runs `ALTER TABLE … REPLICA
IDENTITY FULL|DEFAULT` as a superuser (ALTER needs table ownership). The flip is
**NON-RETROACTIVE**: it enriches only WAL written after it, and the materializer
refuses an absent old image (`old-image-absent`, alertable) rather than evaluate
`old` as null.

```bash
cargo test -p wamn-event-reg -p wamn-materializer   # one root-old detector + the per-event old-absent guard + delete-under-FULL fires
cargo test -p wamn-migrate                          # reconciler derivation (old-cond/delete-op/cross-tenant union/none-required→DEFAULT) + SQL pins
cargo clippy -p wamn-migrate -p wamn-ctl --all-targets
# Live gate (throwaway wal_level=logical PG18): drives the REAL reconcile path —
# a registration on an entity flips its table 'd'->'f' live (pg_class.relreplident),
# an unrelated entity stays 'd', removing the registrations flips back 'f'->'d',
# and a reconcile at target is a no-op; then a test_decoding slot proves the WAL
# truth NON-RETROACTIVELY: under DEFAULT an UPDATE carries no old image and a
# DELETE's old image is the pkey only (no tenant_id); after the flip an UPDATE
# carries the old image and a DELETE's old image carries tenant_id.
docker run -d --name wamn-ri-pg -p 5462:5432 -e POSTGRES_PASSWORD=postgres \
  postgres:18 -c wal_level=logical -c fsync=off -c synchronous_commit=off
WAMN_CTL_PG_URL=postgres://postgres:postgres@127.0.0.1:5462/postgres \
  cargo test -p wamn-ctl --test replica_identity_live -- --nocapture
docker rm -f wamn-ri-pg
# Dry-run the verb against a provisioned project DB (prints flips + no-ops):
./target/debug/wamn-ctl reconcile-replica-identity \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5462/postgres \
  --catalog path/to/applied-catalog.json --schema app --dry-run
# Materializer end-to-end (rebuild the guest — the served old condition + the
# old-image-absent refusal changed): matbench adds an UPDATE carrying a FULL old
# image that evaluates end to end and fires (f-old:evt:8); cutbench phase 3
# reconciles the delete entity to FULL so disp-del cuts over and its post-flip
# delete fires a scoped :evt: run (the EXPECTED-DELETE-RI divergence retires)
# while the pre-flip DEFAULT refusals stand. Recipes: [EVT-MAT], [EVT-CUTOVER].
(cd components && cargo build -p materializer --target wasm32-wasip2)
```

Mutation harness: scratchpad `mutate_l5i9_31.py` — M1 the reconciler drops the
delete-op rule (killed by
`replica_identity::tests::a_delete_only_registration_requires_full_even_without_a_condition`),
M2 the materializer guard treats an absent old image as condition-false (killed
by `decide::tests::old_value_conditions_are_serviceable_and_guarded_per_event`),
M3 `alter_replica_identity_sql` emits the wrong keyword (killed by
`replica_identity::tests::alter_and_read_sql_are_pinned`); apply/test/restore
with sha256, DEBUG builds.
