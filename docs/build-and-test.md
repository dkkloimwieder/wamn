# Build & Test — gate commands per bead

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
cargo build --release -p wamn-host -p wamn-gates   # prod host + gate suite (SR1 split)
(cd components && cargo build --release --target wasm32-wasip2)  # guest fixtures
```

### S1/4p3/bp4.1 gates

```bash
./target/release/wamn-gates --log-level warn bench \
  --hello components/target/wasm32-wasip2/release/hello.wasm \
  --memhog components/target/wasm32-wasip2/release/memhog.wasm \
  --busyloop components/target/wasm32-wasip2/release/busyloop.wasm
```

### S2 gates (qps + p99, saturation, chaos/RLS/injection)

```bash
# Local iteration (throwaway container + the same fixture SQL):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
./target/release/wamn-gates --log-level error pgbench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (p99 is measured in-cluster):
kubectl -n wamn-system create configmap pg-init --from-file=init.sql=deploy/postgres-init.sql
kubectl -n wamn-system apply -f deploy/postgres.yaml -f deploy/pgbench-job.yaml
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
kubectl -n wamn-system apply -f deploy/pgbench-multiproject-job.yaml
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
# The production tool is `wamn-host provision-project --project <id>
# In-cluster gate of record (against the shared CNPG cluster = the D6 substrate,
# NO cpu limit — S2 CFS lesson):
kubectl apply --server-side -f deploy/cnpg-operator.yaml
kubectl -n cnpg-system rollout status deploy/cnpg-controller-manager --timeout=150s
kubectl apply -f deploy/cnpg-cluster.yaml
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/wamn-pg --timeout=300s
# A HOST change => full docker rebuild (both --target stages + kind load BOTH images):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/provisionbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/provisionbench --timeout=180s
kubectl -n wamn-system logs job/provisionbench
```

### S3 gates

```bash
./target/release/wamn-gates --log-level error flowbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster (same co-located / no-cpu-limit Job topology as pgbench):
kubectl -n wamn-system apply -f deploy/flowbench-job.yaml
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
kubectl -n wamn-system apply -f deploy/serve-node.yaml
kubectl -n wamn-system rollout status deploy/serve-node --timeout=120s
kubectl -n wamn-system apply -f deploy/nodebench-job.yaml
kubectl -n wamn-system logs -f job/nodebench
```

### S5 gates

```bash
# Local iteration (throwaway loki + collector on a docker network):
docker network create wamn-s5 2>/dev/null || true
docker run -d --name wamn-s5-loki --network wamn-s5 -p 3100:3100 \
  -v "$PWD/deploy/loki-local.yaml:/etc/loki/loki.yaml:ro" \
  grafana/loki:3.4.2 -config.file=/etc/loki/loki.yaml
docker run -d --name wamn-s5-otelcol --network wamn-s5 -p 4317:4317 -p 8888:8888 \
  -v "$PWD/deploy/otelcol-local.yaml:/etc/otelcol/config.yaml:ro" \
  otel/opentelemetry-collector-contrib:0.115.1 --config=/etc/otelcol/config.yaml
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 RUST_LOG=error \
  LOKI_URL=http://127.0.0.1:3100 COLLECTOR_METRICS_URL=http://127.0.0.1:8888/metrics \
  ./target/release/wamn-gates --log-level info logbench \
  --logspewer components/target/wasm32-wasip2/release/logspewer.wasm --mode all
# In-cluster gate of record (real Loki + collector; no cpu limit — the S2 lesson):
kubectl -n wamn-system apply -f deploy/loki.yaml -f deploy/otel-collector.yaml
kubectl -n wamn-system rollout status deploy/loki deploy/otel-collector --timeout=120s
kubectl -n wamn-system apply -f deploy/logbench-job.yaml
kubectl -n wamn-system logs -f job/logbench
```

### [9.1] OTel trace pipeline

Docs: docs/tracing.md

```bash
cargo clippy -p wamn-host -p wamn-gates --all-targets \
  && cargo fmt -p wamn-host -p wamn-gates --check
# Local iteration (throwaway Postgres + Tempo + collector on a docker network;
# spans are INFO):
docker network create wamn-s5 2>/dev/null || true
docker run -d --rm --name wamn-trace-pg --network wamn-s5 -p 5482:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
docker run -d --name wamn-s5-tempo --network wamn-s5 -p 3200:3200 \
  -v "$PWD/deploy/tempo-local.yaml:/etc/tempo/tempo.yaml:ro" \
  grafana/tempo:2.6.1 -config.file=/etc/tempo/tempo.yaml
docker run -d --name wamn-s5-otelcol --network wamn-s5 -p 4317:4317 -p 8888:8888 \
  -v "$PWD/deploy/otelcol-local.yaml:/etc/otelcol/config.yaml:ro" \
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
kubectl -n wamn-system apply -f deploy/tempo.yaml -f deploy/otel-collector.yaml
kubectl -n wamn-system rollout status deploy/tempo deploy/otel-collector --timeout=120s
kubectl -n wamn-system apply -f deploy/tracebench-job.yaml
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
kubectl -n wamn-system apply -f deploy/serve-echo.yaml
kubectl -n wamn-system rollout status deploy/serve-echo --timeout=120s
kubectl -n wamn-system apply -f deploy/trace-relay-workload.yaml
kubectl -n wamn-system apply -f deploy/traceproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/traceproof --timeout=180s
kubectl -n wamn-system logs job/traceproof
```

### S6 gates

```bash
# Local iteration (throwaway container + the same fixture SQL):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
./target/release/wamn-gates --log-level error testhostbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (co-located with Postgres, no cpu limit — S2 lesson;
# WAMN_PG_ADMIN_URL is the superuser used only to provision the ephemeral schema):
kubectl -n wamn-system apply -f deploy/testhostbench-job.yaml
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

cargo clippy -p wamn-host -p wamn-gates -p wamn-gate-harness --all-targets \
  && cargo fmt -p wamn-host -p wamn-gates -p wamn-gate-harness --check
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
# optional live-apply gate (deploy/run-state.sql on a throwaway PG; superuser URL
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

### [5.14] durable run queue & runner scaling (crates/wamn-run-queue)

Docs: docs/run-queue.md

```bash
cargo test -p wamn-run-queue
cargo clippy -p wamn-run-queue --all-targets && cargo fmt -p wamn-run-queue --check
# optional live-apply gate (deploy/run-state.sql + run-queue.sql on a throwaway PG;
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
kubectl -n wamn-system apply -f deploy/queuebench-job.yaml
kubectl -n wamn-system logs -f job/queuebench
```

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
kubectl -n wamn-system apply -f deploy/failoverbench-job.yaml
kubectl -n wamn-system logs -f job/failoverbench
```

### [5.14] guest-self-claim

Docs: docs/run-queue.md

```bash
cargo test -p wamn-run-store   # incl select_run_dispatch shape (fl3's traceparent seam)
cargo build -p wamn-run-queue --no-default-features   # the guest's pure claim-path core builds alone
cargo clippy -p wamn-host -p wamn-gates -p wamn-run-store -p wamn-run-queue --all-targets \
  && cargo fmt -p wamn-host -p wamn-gates -p wamn-run-store -p wamn-run-queue --check
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
kubectl -n wamn-system apply -f deploy/failoverbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/failoverbench --timeout=240s
kubectl -n wamn-system logs job/failoverbench
```

### [5.14] production runner (run-worker, fqg.8)

Docs: docs/run-queue.md · Manifests: deploy/runner.yaml + deploy/runner-db.example.yaml

```bash
cargo test -p wamn-host run_worker   # owner fallback + drain tally + idle backoff
cargo clippy -p wamn-host -p wamn-gates --all-targets \
  && cargo fmt -p wamn-host -p wamn-gates --check
# Local runnerbench (throwaway postgres:18 + wamn_app; guest UNCHANGED — no wasm rebuild):
docker run -d --name wamn-fqg8-pg -p 5490:5432 -e POSTGRES_PASSWORD=postgres postgres:18
docker exec wamn-fqg8-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
./target/debug/wamn-gates --log-level warn runnerbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5490/postgres \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5490/postgres   # drain + reuse + empty
docker rm -f wamn-fqg8-pg
# In-cluster live smoke = gate of record (HOST changed — the run-worker module +
# flowrunner.wasm baked into the prod image — so FULL rebuild BOTH stages + kind load):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn
# Provision a demo schema (wamn_runner_demo: run-state.sql + run-queue.sql rewritten,
# a flows table + a sink table) via kubectl exec psql, register a fast-cron flow, then:
kubectl -n wamn-system apply -f deploy/dispatcher-projects.example.yaml   # (pointed at the demo)
kubectl -n wamn-system apply -f deploy/dispatcher.yaml
kubectl -n wamn-system apply -f deploy/runner-db.example.yaml
kubectl -n wamn-system apply -f deploy/runner.yaml
kubectl -n wamn-system rollout status deploy/runner --timeout=120s
# Assert a dispatcher-fired cron run was CLAIMED by the runner and driven end-to-end:
#   SELECT status FROM wamn_runner_demo.runs WHERE run_id LIKE 'runner-demo:cron:%'  -> completed
#   + a wamn_runner_demo.sink row + wamn_runner_demo.node_runs rows.
```

### [5.14] shared trigger dispatcher

Docs: docs/run-queue.md

```bash
cargo test -p wamn-run-queue   # incl cron calendar edges + outbox/adaptive decisions
cargo clippy -p wamn-run-queue --all-targets && cargo fmt -p wamn-run-queue --check
# optional live-apply gate (run-state.sql + run-queue.sql now incl the outbox; real
# atomicity + redelivery dedupe, cron last-tick recovery, wake scan; skips when unset):
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
# The production service is `wamn-host dispatch --projects-file <json>` (one entry
# In-cluster gate of record (co-located with postgres,
# HOST change => full docker rebuild (both --target stages + kind load BOTH images):
kubectl -n wamn-system apply -f deploy/dispatchbench-job.yaml
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
kubectl apply -f deploy/wamn-sysdb.yaml
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
cargo test -p wamn-registry   # drift-guard + inv-1 grep + as_str coherence (live-apply skips)
cargo clippy -p wamn-registry --all-targets && cargo fmt -p wamn-registry --check
# optional throwaway-PG live-apply gate (WAMN_REGISTRY_PG_URL, superuser url — the
# 2/3/4 + FK integrity + saga exactly-once; skips when unset):
docker run -d --rm --name wamn-reg-pg -p 5461:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5461/wamn cargo test -p wamn-registry
docker stop wamn-reg-pg
# IN-CLUSTER gate of record — apply system-schema.sql INTO wamn-sysdb's (wamn-q3n.2)
# EMPTY, wamn_system-owned, ready for provisioning:
{ echo "DROP SCHEMA IF EXISTS registry, provisioning CASCADE; SET ROLE wamn_system;"; \
  cat deploy/system-schema.sql; } | kubectl -n wamn-system exec -i wamn-sysdb-1 \
  -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 -f -
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT schemaname||'.'||tablename FROM pg_tables \
        WHERE schemaname IN ('registry','provisioning') ORDER BY 1;"  # 5 control-plane tables
```

### [D6/wamn-q3n.6] provision-org

Docs: docs/provisioning.md, docs/postgres-topology.md

```bash
cargo test -p wamn-registry -p wamn-provision -p wamn-host   # renderer shape + org-row SQL + drift/subcommand units
cargo clippy -p wamn-registry -p wamn-provision -p wamn-host --all-targets \
  && cargo fmt -p wamn-registry -p wamn-provision -p wamn-host --check
# CONFLICT mutant). Render CRs locally (no cluster/DB needed):
./target/debug/wamn-host provision-org --org demo --tier standard \
  --emit-prod /tmp/demo-prod.json --emit-dev /tmp/demo-dev.json
# IN-CLUSTER live standup = the gate of record (the wamn-q3n.2 infra precedent;
# port-forwarded wamn-sysdb, then kubectl-apply the emitted CRs ADDITIVELY:
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5463:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5463/wamn_system?sslmode=disable" \
  ./target/debug/wamn-host provision-org --org demo --tier standard \
  --emit-prod /tmp/demo-prod.json --emit-dev /tmp/demo-dev.json   # renders + writes registry.orgs
kubectl apply -f /tmp/demo-prod.json -f /tmp/demo-dev.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=2 cluster/demo-prod --timeout=300s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/demo-dev  --timeout=300s
# deletes ONLY the new pair + its row:
kubectl -n wamn-system delete cluster demo-prod demo-dev
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='demo';"
```

### [D6/wamn-q3n.7] provision-project-env

Docs: docs/provisioning.md, docs/postgres-topology.md

```bash
cargo test -p wamn-provision -p wamn-registry -p wamn-host   # renderer/naming + project SQL + drift/subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-host --check
# (--cluster given => no DB needed):
./target/debug/wamn-host provision-project-env --org demo --project demo --env dev \
  --cluster wamn-pg --emit-database - --emit-role-sql - --emit-privilege-sql - --emit-secret -
# IN-CLUSTER live standup = the gate of record (T3 pool wamn-pg is ALWAYS up; the
# SQL -> Database CR -> privilege SQL in order:
kubectl -n wamn-system exec -i wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "SET ROLE wamn_system; INSERT INTO registry.orgs (id,tier,prod_cluster,dev_cluster) \
      VALUES ('demo','trials','wamn-pg','wamn-pg') ON CONFLICT (id) DO NOTHING;"
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5470:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5470/wamn_system?sslmode=disable" \
  ./target/debug/wamn-host provision-project-env --org demo --project demo --env dev \
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
# IN-CLUSTER gate of record = a LIVE T2 ORG-PAIR STANDUP (the .6/.7 precedent; the
# registry read/write (the registry-write path is the .6/.7 gate of record):
./target/debug/wamn-host provision-org --org gate8 --tier standard \
  --emit-prod /tmp/gate8-prod.json --emit-dev /tmp/gate8-dev.json
kubectl apply -f /tmp/gate8-prod.json -f /tmp/gate8-dev.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=2 cluster/gate8-prod --timeout=300s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/gate8-dev  --timeout=180s
for E in prod dev; do C=gate8-$E; \
  ./target/debug/wamn-host provision-project-env --org gate8 --project app --env $E \
    --cluster $C --emit-database /tmp/db-$E.json --emit-role-sql /tmp/role-$E.sql \
    --emit-privilege-sql /tmp/priv-$E.sql --emit-secret /tmp/sec-$E.json; \
  kubectl -n wamn-system exec -i $C-1 -c postgres -- psql -U postgres -f - < /tmp/role-$E.sql; \
  kubectl apply -f /tmp/db-$E.json; \
  kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-gate8--app--$E --timeout=90s; \
  kubectl -n wamn-system exec -i $C-1 -c postgres -- psql -U postgres -f - < /tmp/priv-$E.sql; done
# wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown deletes ONLY the new pair:
kubectl -n wamn-system delete database wamn-db-gate8--app--prod wamn-db-gate8--app--dev
kubectl -n wamn-system delete cluster gate8-prod gate8-dev
```

### [D6/wamn-q3n.9] demote the shipped shared cluster to the T3 trials pool

Docs: docs/postgres-topology.md, docs/provisioning.md

```bash
cargo test -p wamn-registry -p wamn-host   # Org::for_pool refs==pool + org_for trials-vs-pair + subcommand units
cargo clippy -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-registry -p wamn-host --check
# Render/plan a trials org locally (no DB needed — omit --system-database-url):
./target/debug/wamn-host provision-org --org trialco --tier trials --pool wamn-pg
# IN-CLUSTER gate of record = a LIVE T3 trials-org standup (the .6/.7 precedent; T3
# port-forward (check `ss -ltn | grep 547` first):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5473:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5473/wamn_system?sslmode=disable" \
  ./target/debug/wamn-host provision-org --org t3gate --tier trials --pool wamn-pg   # records registry.orgs (both refs=wamn-pg), NO CRs
# provision-project-env WITHOUT --cluster reads placement from the registered row -> wamn-pg:
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5473/wamn_system?sslmode=disable" \
  ./target/debug/wamn-host provision-project-env --org t3gate --project demo --env dev \
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
cargo test -p wamn-provision -p wamn-registry -p wamn-host   # renderers/builders + record_dump SQL + drift/subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-host --check
# Render locally (no DB — --tier gives the cadence without a registry read):
./target/debug/wamn-host dump-project-env --org demo --project app --env prod \
  --tier standard --emit-cronjob - --emit-job -
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
awk '/^CREATE TABLE provisioning\.dumps/{f=1} f{print} f&&/^\);/{exit}' deploy/system-schema.sql \
  | { echo "SET ROLE wamn_system;"; cat; } | kubectl -n wamn-system exec -i wamn-sysdb-1 \
  -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 -f -
# it, then dump+restore. PICK CLEAN unused ports (check `ss -ltn | grep 547`):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5474:5432 &
kubectl -n wamn-system port-forward svc/wamn-pg-rw 5475:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
PGPW=$(kubectl -n wamn-system get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
SYS="postgres://postgres:${SYSPW}@127.0.0.1:5474/wamn_system?sslmode=disable"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-org --org t10gate --tier trials --pool wamn-pg
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-project-env \
  --org t10gate --project demo --env dev --connection-limit 10 \
  --emit-database /tmp/t10-db.json --emit-role-sql /tmp/t10-role.sql \
  --emit-privilege-sql /tmp/t10-priv.sql --emit-secret /tmp/t10-secret.json
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t10-role.sql
kubectl apply -f /tmp/t10-db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-t10gate--demo--dev --timeout=90s
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t10-priv.sql
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -d "wamn-db-t10gate--demo--dev" \
  -c "CREATE TABLE parts (id int primary key, sku text, weight_kg numeric(8,3)); INSERT INTO parts VALUES (1,'bolt',0.125),(2,'nut',0.050),(3,'washer',0.008);"
# Dump the REAL project-env DB (reads tier from wamn-sysdb -> trials daily), then restore:
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host dump-project-env --org t10gate --project demo --env dev \
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
cargo test -p wamn-provision -p wamn-registry -p wamn-host   # restore builders + select_latest shape/drift + subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-host --check
# Render/plan locally (no cluster/DB needed — explicit --dump-dir, render only):
./target/debug/wamn-host restore-project-env --org demo --project app --env dev \
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
# Register a trials org + provision a project-env DB on wamn-pg (the .7/.9 path), seed:
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-org --org t11gate --tier trials --pool wamn-pg
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-project-env \
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
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host dump-project-env --org t11gate --project demo --env dev \
  --database-url "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" --run-now --out-dir "$DUMPROOT"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host restore-project-env --org t11gate --project demo --env dev \
  --database-url "$PGADMIN" --dump-root "$DUMPROOT"   # reads the catalog -> scratch DB
# row (mutate live -> restore -> stale gone):
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/wamn-restore-t11gate--demo--dev?sslmode=disable" \
  -tAc "SELECT count(*), sum(weight_kg) FROM parts;"
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" -c "INSERT INTO parts VALUES (99,'STALE',9.999);"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host restore-project-env --org t11gate --project demo --env dev \
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

### [D6/wamn-q3n.13] tier-move / promotion tooling

Docs: docs/provisioning.md, docs/postgres-topology.md

```bash
cargo test -p wamn-provision -p wamn-registry -p wamn-host   # tier_move validate/plan + project-env list SQL + subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-host --check
# IN-CLUSTER gate of record = a LIVE T3->T2 tier move across REAL clusters (the .6/.7/.9/.10/
# .10; NO docker rebuild — real debug subcommand + kubectl). PICK CLEAN ports (ss -ltn | grep 547):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5478:5432 &
kubectl -n wamn-system port-forward svc/wamn-pg-rw 5479:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
PGPW=$(kubectl -n wamn-system get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
SYS="postgres://postgres:${SYSPW}@127.0.0.1:5478/wamn_system?sslmode=disable"; DUMPROOT=$(mktemp -d)
# Register a trials org + provision + seed a project-env DB on wamn-pg (.7/.9), then dump it (.10):
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-org --org t13gate --tier trials --pool wamn-pg
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-project-env --org t13gate --project app --env prod \
  --connection-limit 10 --emit-role-sql /tmp/t13-role.sql --emit-database /tmp/t13-db.json \
  --emit-privilege-sql /tmp/t13-priv.sql --emit-secret /tmp/t13-secret.json
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t13-role.sql
kubectl apply -f /tmp/t13-db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-t13gate--app--prod --timeout=90s
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t13-priv.sql
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -d "wamn-db-t13gate--app--prod" \
  -c "CREATE TABLE parts (id int primary key, sku text, weight_kg numeric(8,3)); INSERT INTO parts VALUES (1,'bolt',0.125),(2,'nut',0.050),(3,'washer',0.008);"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host dump-project-env --org t13gate --project app --env prod \
  --database-url "postgres://postgres:${PGPW}@127.0.0.1:5479/wamn-db-t13gate--app--prod?sslmode=disable" --run-now --out-dir "$DUMPROOT"
# Plan the move, then provision the REAL T2 pair (render-only, NO flip yet):
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host move-org-tier --org t13gate --target-tier standard   # PLAN
env -u WAMN_SYSTEM_ADMIN_URL ./target/debug/wamn-host provision-org --org t13gate --tier standard \
  --emit-prod /tmp/t13-prod.json --emit-dev /tmp/t13-dev.json   # render-only (env -u => no --system-database-url => no early flip)
kubectl apply -f /tmp/t13-prod.json -f /tmp/t13-dev.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=2 cluster/t13gate-prod --timeout=300s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/t13gate-dev  --timeout=180s
# triple-derived CR name is free, then provision the DB on t13gate-prod + restore the dump into it:
NPPW=$(kubectl -n wamn-system get secret t13gate-prod-superuser -o jsonpath='{.data.password}' | base64 -d)
kubectl -n wamn-system port-forward svc/t13gate-prod-rw 5480:5432 &
NEWADMIN="postgres://postgres:${NPPW}@127.0.0.1:5480/postgres?sslmode=disable"
kubectl -n wamn-system delete database wamn-db-t13gate--app--prod --ignore-not-found
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-project-env --org t13gate --project app --env prod \
  --cluster t13gate-prod --emit-role-sql /tmp/n-role.sql --emit-database /tmp/n-db.json \
  --emit-privilege-sql /tmp/n-priv.sql --emit-secret /tmp/n-secret.json
kubectl -n wamn-system exec -i t13gate-prod-1 -c postgres -- psql -U postgres -f - < /tmp/n-role.sql
kubectl apply -f /tmp/n-db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-t13gate--app--prod --timeout=90s
kubectl -n wamn-system exec -i t13gate-prod-1 -c postgres -- psql -U postgres -f - < /tmp/n-priv.sql
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host restore-project-env --org t13gate --project app --env prod \
  --in-place --confirm --database-url "$NEWADMIN" --dump-root "$DUMPROOT"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host move-org-tier --org t13gate --target-tier standard --flip   # CUTOVER
# now points there (resolve prod -> t13gate-prod), tier flipped standard:
kubectl -n wamn-system exec t13gate-prod-1 -c postgres -- psql -U postgres -d "wamn-db-t13gate--app--prod" \
  -tAc "SELECT count(*), sum(weight_kg) FROM parts;"   # 3 | 0.183 (exact decimals survived the cross-cluster move)
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT id,tier,prod_cluster,dev_cluster FROM registry.orgs WHERE id='t13gate';"  # t13gate|standard|t13gate-prod|t13gate-dev
# projects+project_envs+dumps:
kubectl -n wamn-system delete database wamn-db-t13gate--app--prod --ignore-not-found
kubectl -n wamn-system delete cluster t13gate-prod t13gate-dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- \
  psql -U postgres -c 'DROP DATABASE IF EXISTS "wamn-db-t13gate--app--prod" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='t13gate';"
```

### [D6/wamn-q3n.14] T4 dedicated-per-env regulated tier

Docs: docs/postgres-topology.md, docs/provisioning.md

```bash
cargo test -p wamn-registry -p wamn-provision -p wamn-host   # model/DDL drift + renderer + routing + subcommand units
cargo clippy -p wamn-registry -p wamn-provision -p wamn-host -p wamn-gates --all-targets \
  && cargo fmt -p wamn-registry -p wamn-provision -p wamn-host -p wamn-gates --check
# Render a dedicated org's 3 CRs locally (no cluster/DB needed):
./target/debug/wamn-host provision-org --org demo --tier dedicated \
  --emit-prod /tmp/demo-prod.json --emit-canary /tmp/demo-canary.json --emit-dev /tmp/demo-dev.json
# optional live gates (throwaway postgres:18; superuser url): (a) the storage
# upsert; standard/trials orgs carry canary NULL):
docker run -d --rm --name wamn-reg-pg -p 5461:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5461/wamn cargo test -p wamn-registry
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5461/wamn \
  ./target/debug/wamn-gates --log-level error provisionbench --mode all
docker stop wamn-reg-pg
# IN-CLUSTER gate of record = a LIVE dedicated-org standup (the .6/.8/.13 precedent;
# wamn-pg/postgres.yaml). PICK A CLEAN port (ss -ltn | grep 548):
kubectl -n wamn-system exec -i wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 <<'SQL'
SET ROLE wamn_system;
ALTER TABLE registry.orgs ADD COLUMN IF NOT EXISTS canary_cluster text;
ALTER TABLE registry.orgs DROP CONSTRAINT IF EXISTS orgs_canary_dedicated_check;
ALTER TABLE registry.orgs ADD CONSTRAINT orgs_canary_dedicated_check
    CHECK ((tier = 'dedicated') = (canary_cluster IS NOT NULL));
ALTER TABLE registry.orgs DROP CONSTRAINT IF EXISTS orgs_canary_recovery_domain_check;
ALTER TABLE registry.orgs ADD CONSTRAINT orgs_canary_recovery_domain_check
    CHECK (canary_cluster IS NULL OR (canary_cluster <> prod_cluster AND canary_cluster <> dev_cluster));
SQL
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5481:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
SYS="postgres://postgres:${SYSPW}@127.0.0.1:5481/wamn_system?sslmode=disable"
# Register a dedicated org -> records registry.orgs (canary_cluster=d14gate-canary) + emits 3 CRs:
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-org --org d14gate --tier dedicated \
  --emit-prod /tmp/d14-prod.json --emit-canary /tmp/d14-canary.json --emit-dev /tmp/d14-dev.json
kubectl apply -f /tmp/d14-prod.json -f /tmp/d14-canary.json -f /tmp/d14-dev.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 cluster/d14gate-prod   --timeout=360s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=2 cluster/d14gate-canary --timeout=360s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/d14gate-dev    --timeout=360s
# the registry); for each: role SQL -> Database CR -> wait applied -> privilege SQL:
for E in canary prod; do
  WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-project-env --org d14gate --project app --env $E \
    --connection-limit 10 --emit-role-sql /tmp/d14-$E-role.sql --emit-database /tmp/d14-$E-db.json \
    --emit-privilege-sql /tmp/d14-$E-priv.sql --emit-secret /tmp/d14-$E-secret.json
  CL=$(python3 -c "import json;print(json.load(open('/tmp/d14-$E-db.json'))['spec']['cluster']['name'])")
  kubectl -n wamn-system exec -i ${CL}-1 -c postgres -- psql -U postgres -f - < /tmp/d14-$E-role.sql
  kubectl apply -f /tmp/d14-$E-db.json
  kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-d14gate--app--$E --timeout=120s
  kubectl -n wamn-system exec -i ${CL}-1 -c postgres -- psql -U postgres -f - < /tmp/d14-$E-priv.sql
done
# d14gate-canary/d14gate-dev + project_envs canary+prod rows:
for CL in d14gate-canary d14gate-prod; do kubectl -n wamn-system exec ${CL}-1 -c postgres -- \
  psql -U postgres -tAc "SELECT datname FROM pg_database WHERE datname LIKE 'wamn-db-d14gate%';"; done
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT id,tier,prod_cluster,canary_cluster,dev_cluster FROM registry.orgs WHERE id='d14gate';"
# — it is the shipped schema):
kubectl -n wamn-system delete database wamn-db-d14gate--app--canary wamn-db-d14gate--app--prod --ignore-not-found
kubectl -n wamn-system delete cluster d14gate-prod d14gate-canary d14gate-dev
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='d14gate';"
```

### [D6/wamn-e1g] per-org WAL/PITR via the Barman Cloud plugin + the shared object

Docs: docs/postgres-topology.md, docs/provisioning.md

```bash
cargo test -p wamn-provision -p wamn-host   # backup renderer + tier knobs + org/dump wiring + subcommand units
cargo clippy -p wamn-provision -p wamn-host -p wamn-registry -p wamn-gates --all-targets \
  && cargo fmt -p wamn-provision -p wamn-host -p wamn-registry -p wamn-gates --check
# Render a standard org's backup CRs locally (no cluster/DB needed):
./target/debug/wamn-host provision-org --org demo --tier standard \
  --emit-prod /tmp/demo-prod.json --emit-dev /tmp/demo-dev.json \
  --emit-object-store /tmp/demo-os.json --emit-scheduled-backup /tmp/demo-sb.json
# IN-CLUSTER gate of record = a LIVE WAL/PITR standup (the .6/.14 precedent; T3 pool
# precedent — the shared-cluster guardrail forbids re-applying wamn-pg/wamn-sysdb):
kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.21.0/cert-manager.yaml
kubectl -n cert-manager wait --for=condition=Available deploy --all --timeout=180s
kubectl apply -f deploy/barman-cloud-plugin.yaml
kubectl -n cnpg-system rollout status deploy/barman-cloud --timeout=180s
kubectl apply -f deploy/minio.yaml
kubectl -n wamn-system rollout status deploy/minio --timeout=150s
kubectl -n wamn-system wait --for=condition=complete job/minio-init --timeout=120s
# backup CRs, not the registry row), apply ObjectStore -> Clusters -> ScheduledBackup:
env -u WAMN_SYSTEM_ADMIN_URL ./target/debug/wamn-host provision-org --org e1gate --tier standard \
  --emit-prod /tmp/e1-prod.json --emit-dev /tmp/e1-dev.json \
  --emit-object-store /tmp/e1-os.json --emit-scheduled-backup /tmp/e1-sb.json
kubectl apply -f /tmp/e1-os.json                             # ObjectStore BEFORE the cluster
kubectl apply -f /tmp/e1-prod.json -f /tmp/e1-dev.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=2 cluster/e1gate-prod --timeout=300s
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

### [2.5] migration engine (crates/wamn-migrate + wamn-host migrate-catalog)

Docs: docs/migration-engine.md

```bash
cargo test -p wamn-migrate     # unit (guards/gate/dry-run/rollback) + drift-guard + live-apply
cargo test -p wamn-host --lib migrate_catalog   # the subcommand's bare-ident + param-map units
cargo clippy -p wamn-migrate -p wamn-host --all-targets \
  && cargo fmt -p wamn-migrate -p wamn-host --check
# optional live-apply gate (throwaway postgres:18; superuser url — provisions
# unset):
docker run -d --rm --name wamn-migrate-pg -p 5467:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_MIGRATE_PG_URL=postgres://postgres:postgres@127.0.0.1:5467/wamn cargo test -p wamn-migrate
docker stop wamn-migrate-pg
# The production tool is `wamn-host migrate-catalog --admin-database-url <superuser>
```

### [3.1] metadata catalog schema crate (crates/wamn-catalog)

Docs: docs/catalog-model.md

```bash
cargo test -p wamn-catalog
cargo clippy -p wamn-catalog --all-targets && cargo fmt -p wamn-catalog --check
# regenerate the published JSON Schema contract after changing the types:
cargo run -p wamn-catalog --example print-schema > docs/catalog-model.schema.json
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
  < deploy/catalog-schema.sql
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
kubectl -n wamn-system apply -f deploy/apibench-job.yaml
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
kubectl -n wamn-system apply -f deploy/registry.yaml
kubectl -n wamn-system rollout status deploy/registry --timeout=60s
kubectl -n wamn-system port-forward svc/registry 5000:5000 &
wash push localhost:5000/wamn/api-gateway:dev \
  components/target/wasm32-wasip2/release/api_gateway.wasm --insecure
# The host group gains --allow-insecure-registries + WAMN_PG_URL:
helm upgrade --install -n wamn-system wamn \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.5.2 \
  -f deploy/values-wamn.yaml
kubectl -n wamn-system rollout status deploy/hostgroup-default --timeout=150s
# Provision the project schema/floor + seed + publish the snapshot:
kubectl -n wamn-system create configmap proof-catalog \
  --from-file=proof-catalog.json=deploy/proof-catalog.json
kubectl -n wamn-system apply -f deploy/publish-catalog-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/publish-catalog --timeout=120s
# Deploy the gateway workload, then prove it serves over the network:
kubectl -n wamn-system apply -f deploy/api-gateway-workload.yaml
kubectl -n wamn-system apply -f deploy/apiproof-job.yaml
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
  -v "$PWD/deploy/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
REL=components/target/wasm32-wasip2/release
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5450/wamn \
  ./target/release/wamn-gates --log-level error f1bench \
  --webhook-entry $REL/poc_webhook_f1.wasm --api-gateway $REL/api_gateway.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS
# lesson; ephemeral schema => shared-PG safe; bench Jobs run SEQUENTIALLY):
kubectl -n wamn-system apply -f deploy/f1bench-job.yaml
kubectl -n wamn-system logs -f job/f1bench
# (sync + burst + DB audit + REST):
wash push localhost:5000/wamn/poc-webhook-f1:dev \
  components/target/wasm32-wasip2/release/poc_webhook_f1.wasm --insecure
kubectl -n wamn-system create configmap f1-fixtures \
  --from-file=poc-receiving.catalog.json=crates/wamn-catalog/tests/fixtures/poc-receiving.catalog.json \
  --from-file=f1-flow.json=deploy/f1-flow.json \
  --from-file=f1-seed.dataset.json=deploy/f1-seed.dataset.json
kubectl -n wamn-system apply -f deploy/f1-provision-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/f1-provision --timeout=120s
kubectl -n wamn-system apply -f deploy/f1-workloads.yaml
kubectl -n wamn-system apply -f deploy/f1proof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/f1proof --timeout=180s
kubectl -n wamn-system logs job/f1proof

docker build --target host -t wamn-host:dev . \
  && docker build --target gates -t wamn-gates:dev .   # fork git dep fetched in the builder stage
```
