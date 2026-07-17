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
# Local (exit-code disciplined since wamn-cjv.1: any failed phase — p99 SLO,
# cap kill at the 256 MiB ceiling, epoch Trap::Interrupt, 64/192 budget
# differentiation — makes bench exit non-zero; job completion IS the verdict):
./target/release/wamn-gates --log-level warn bench \
  --hello components/target/wasm32-wasip2/release/hello.wasm \
  --memhog components/target/wasm32-wasip2/release/memhog.wasm \
  --busyloop components/target/wasm32-wasip2/release/busyloop.wasm
# In-cluster gate of record (no DB/NATS; fixtures ship in the image):
kubectl -n wamn-system apply -f deploy/bench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/bench --timeout=600s
kubectl -n wamn-system logs job/bench
# Mutation harness (4 mutants, each must exit non-zero): scratchpad/mutate_cjv1.py
```

### S2 gates (qps + p99, saturation, chaos/RLS/injection)

```bash
# Local iteration (throwaway container + the same fixture SQL):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
./target/release/wamn-gates --log-level error pgbench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# --mode attack is the wamn-cjv.2 in-band claim-override gate (pgprobe ops 7/8/9);
# guard unit tests: cargo test -p wamn-host guard_
# Mutation harness (3 guard mutants, each must fail --mode attack): scratchpad/mutate_cjv2.py
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

### [5.9] credential vault (plugins/wamn_credentials + credproof)

Docs: docs/credential-vault.md

```bash
# Pure units: the SDK facade + http-request injection/classification + the
# guest per-dispatch scoping + the host vault resolution + the WIT coherence
# drift-guards (the credentials copies) + the credproof fixture pins.
cargo test -p wamn-node-sdk && cargo test -p wamn-nodes
cargo test -p wamn-node-guest --all-features
cargo test -p wamn-host wamn_credentials && cargo test -p wamn-gates credproof

# Local end-to-end (throwaway PG + local serve-echo + a background run-worker
# whose vault carries the demo secret; the run-worker needs the target on its
# --allowed-hosts — EMPTY = deny-all, fail-closed):
docker run -d --name wamn-cred-pg -p 5493:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
cat > /tmp/wamn-credentials.json <<'JSON'
{ "default": { "notify-token": "wamn-cred-proof-7f3a9b2e41d05c68" } }
JSON
./target/debug/wamn-gates --log-level error serve-echo --port 8093 &
WAMN_RUNNER=cred-local ./target/debug/wamn-host --log-level info run-worker \
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
# provision wamn_runner_demo + register deploy/cred/notify.flow.json active
# (the fqg.8/ojm recipe), then:
kubectl -n wamn-system apply -f deploy/serve-echo.yaml
kubectl -n wamn-system apply -f deploy/runner-credentials.example.yaml
kubectl -n wamn-system apply -f deploy/runner-db.example.yaml -f deploy/runner.yaml
kubectl -n wamn-system apply -f deploy/credproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/credproof --timeout=180s
kubectl -n wamn-system logs job/credproof   # overall PASS: true
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

### [EXEC-LADDER.1/2/3] rungs 1-3: single-node, linear chain, conditional branch on the deployed runner (wamn-ojm.1/2/3)

Docs: docs/exec-ladder.md · Fixtures: deploy/ladder/rung{1,2,3}.flow.json · Manifest: deploy/ladderproof-job.yaml

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
WAMN_RUNNER=ojm3-local ./target/debug/wamn-host \
  --log-level info run-worker \
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
# deploy/ladder/rung{1,2,3}.flow.json active as tenant demo-tenant (superuser, RLS bypassed).
kubectl -n wamn-system apply -f deploy/runner-db.example.yaml
kubectl -n wamn-system apply -f deploy/runner.yaml
kubectl -n wamn-system rollout status deploy/runner --timeout=120s
kubectl -n wamn-system apply -f deploy/ladderproof-job.yaml   # --rung 3 (rung-2/1 regressions: edit --rung to 2 / 1)
kubectl -n wamn-system wait --for=condition=complete job/ladderproof --timeout=120s
kubectl -n wamn-system logs job/ladderproof   # -> overall PASS: true
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
cargo test -p wamn-registry   # drift-guard (placement cols + env_policies seed vs the model) + inv-1 grep (live-apply skips)
cargo clippy -p wamn-registry --all-targets && cargo fmt -p wamn-registry --check
# optional throwaway-PG live-apply gate (WAMN_REGISTRY_PG_URL, superuser url —
# invariants 2/3 + the placement biconditional + the composite (org, env) FK ->
# env_policies(org, name) + the template stamp insert-if-absent + FK integrity +
# saga exactly-once; skips when unset):
docker run -d --rm --name wamn-reg-pg -p 5461:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5461/wamn cargo test -p wamn-registry
docker stop wamn-reg-pg
# IN-CLUSTER gate of record — apply system-schema.sql INTO wamn-sysdb's (wamn-q3n.2)
# wamn_system DB (empty of rows — a DROP+re-apply is safe pre-production only):
{ echo "DROP SCHEMA IF EXISTS registry, provisioning CASCADE; SET ROLE wamn_system;"; \
  cat deploy/system-schema.sql; } | kubectl -n wamn-system exec -i wamn-sysdb-1 \
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
cargo test -p wamn-registry -p wamn-provision -p wamn-host   # renderer shape + org-row SQL + drift/subcommand units
cargo clippy -p wamn-registry -p wamn-provision -p wamn-host --all-targets \
  && cargo fmt -p wamn-registry -p wamn-provision -p wamn-host --check
# CONFLICT mutant). Render CRs locally (no cluster/DB needed — template policies):
./target/debug/wamn-host provision-org --org demo --template standard \
  --emit-clusters /tmp/demo-clusters.json --emit-object-store /tmp/demo-os.json \
  --emit-scheduled-backup /tmp/demo-sb.json
# IN-CLUSTER live standup = the gate of record (the wamn-q3n.2 infra precedent;
# port-forwarded wamn-sysdb — reads registry.env_policies for sizing + writes the
# org's placement row — then kubectl-apply the emitted CRs ADDITIVELY (ObjectStore
# BEFORE the clusters, ScheduledBackup after — the wamn-e1g order):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5463:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5463/wamn_system?sslmode=disable" \
  ./target/debug/wamn-host provision-org --org demo --template standard \
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
cargo test -p wamn-provision -p wamn-registry -p wamn-host   # renderer/naming + project SQL + drift/subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-host --check
# (--cluster given => no DB needed):
./target/debug/wamn-host provision-project-env --org demo --project demo --env dev \
  --cluster wamn-pg --emit-database - --emit-role-sql - --emit-privilege-sql - --emit-secret -
# IN-CLUSTER live standup = the gate of record (T3 pool wamn-pg is ALWAYS up; the
# SQL -> Database CR -> privilege SQL in order:
kubectl -n wamn-system exec -i wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "SET ROLE wamn_system; INSERT INTO registry.orgs (id,placement_kind,pool_cluster) \
      VALUES ('demo','pooled','wamn-pg') ON CONFLICT (id) DO NOTHING;"
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
# IN-CLUSTER gate of record = a LIVE DEDICATED-ORG STANDUP (the .6/.7 precedent; the
# registry read/write (the registry-write path is the .6/.7 gate of record):
./target/debug/wamn-host provision-org --org gate8 --template standard \
  --emit-clusters /tmp/gate8-clusters.json --emit-object-store /tmp/gate8-os.json \
  --emit-scheduled-backup /tmp/gate8-sb.json
kubectl apply -f /tmp/gate8-os.json -f /tmp/gate8-clusters.json   # ObjectStore first (prod is backed)
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 cluster/gate8-prod --timeout=300s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/gate8-dev  --timeout=180s
for E in prod dev; do C=gate8-$E; \
  ./target/debug/wamn-host provision-project-env --org gate8 --project app --env $E \
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
cargo test -p wamn-registry -p wamn-host   # Org::pooled placement + pooled-vs-dedicated subcommand units
cargo clippy -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-registry -p wamn-host --check
# Plan a pooled org locally (no DB needed — omit --system-database-url):
./target/debug/wamn-host provision-org --org trialco --template trials --pool wamn-pg
# IN-CLUSTER gate of record = a LIVE T3 trials-org standup (the .6/.7 precedent; T3
# port-forward (check `ss -ltn | grep 547` first):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5473:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5473/wamn_system?sslmode=disable" \
  ./target/debug/wamn-host provision-org --org t3gate --template trials --pool wamn-pg   # records registry.orgs (pooled|wamn-pg), NO CRs
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
# Render locally (no DB — the cadence is --schedule, default daily 03:00):
./target/debug/wamn-host dump-project-env --org demo --project app --env prod \
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
awk '/^CREATE TABLE provisioning\.dumps/{f=1} f{print} f&&/^\);/{exit}' deploy/system-schema.sql \
  | { echo "SET ROLE wamn_system;"; cat; } | kubectl -n wamn-system exec -i wamn-sysdb-1 \
  -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 -f -
# it, then dump+restore. PICK CLEAN unused ports (check `ss -ltn | grep 547`):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5474:5432 &
kubectl -n wamn-system port-forward svc/wamn-pg-rw 5475:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
PGPW=$(kubectl -n wamn-system get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
SYS="postgres://postgres:${SYSPW}@127.0.0.1:5474/wamn_system?sslmode=disable"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-org --org t10gate --template trials --pool wamn-pg
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
# Dump the REAL project-env DB (records the dump in the wamn-sysdb catalog), then restore:
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
# Register a pooled org + provision a project-env DB on wamn-pg (the .7/.9 path), seed:
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host provision-org --org t11gate --template trials --pool wamn-pg
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
cargo test -p wamn-registry -p wamn-host -p wamn-gates   # Template presets + OrgEnvPolicy + org-scoped validate/resolve/SQL + subcommand units
cargo clippy -p wamn-registry -p wamn-host -p wamn-gates --all-targets \
  && cargo fmt -p wamn-registry -p wamn-host -p wamn-gates --check
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
./target/debug/wamn-host provision-org --org smoke1 --template standard  --emit-clusters /tmp/s1.json ...  # 2 clusters (canary -> prod)
./target/debug/wamn-host provision-org --org smoke2 --template dedicated --emit-clusters /tmp/s2.json ...  # 3 clusters (smoke2-canary)
./target/debug/wamn-host provision-project-env --org smoke1 --project app --env canary ...  # cluster smoke1-prod
./target/debug/wamn-host provision-project-env --org smoke2 --project app --env canary ...  # cluster smoke2-canary
docker stop wamn-8df4-pg
# 5 mutants killed (apply/test/restore, debug builds — scratchpad/mutate_8df4.py):
# M1 standard-canary->Own (template unit), M2 stamp DO NOTHING->DO UPDATE (unit +
# live customization-survives), M3 policy read drops org key (unit + live
# cross-org probe), M4 provision-org stamps nothing (scripted project-env
# refusal), M5 validate env check any-org (org-scoping unit).
# IN-CLUSTER gate of record: re-apply system-schema.sql into wamn-sysdb (the
# [D6/wamn-q3n.3] block — org-scoped env_policies, NO seed), rebuild + kind-load
# wamn-gates, run deploy/provisionbench-job.yaml, then a live TEMPLATE-STAMPED
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
cargo test -p wamn-host                # driver units (incl. the shared apply_catalog_target refactor)
cargo clippy -p wamn-provision -p wamn-registry -p wamn-migrate -p wamn-host --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-migrate -p wamn-host --check
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
cargo test -p wamn-provision -p wamn-host   # backup renderer + policy knobs + org/dump wiring + subcommand units
cargo clippy -p wamn-provision -p wamn-host -p wamn-registry -p wamn-gates --all-targets \
  && cargo fmt -p wamn-provision -p wamn-host -p wamn-registry -p wamn-gates --check
# Render a dedicated org's backup CRs locally (no cluster/DB needed; the prod
# policy's backup_cadence/wal_retention drive the CRs):
./target/debug/wamn-host provision-org --org demo --template standard \
  --emit-clusters /tmp/demo-clusters.json \
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
env -u WAMN_SYSTEM_ADMIN_URL ./target/debug/wamn-host provision-org --org e1gate --template standard \
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
