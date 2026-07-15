# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->


## Build & Test

wamn-host builds against wash-runtime consumed as a **git dependency from our
fork** (dkkloimwieder/wasmCloud, branch `wamn/2.5.2` = upstream v2.5.2 + the
carried epoch-deadline and memory-limiter commits) — see
`docs/wash-runtime-fork.md` for the carried-commit ledger, sync runbook, and
rev-bump procedure. The rev is pinned in one place:
`workspace.dependencies.wash-runtime.rev` in the root `Cargo.toml`.

```bash
cargo build --release -p wamn-host -p wamn-gates   # prod host + gate suite (SR1 split)
(cd components && cargo build --release --target wasm32-wasip2)  # guest fixtures

# S1/4p3/bp4.1 gates (instantiation, density, cap kill, epoch kill, memory budgets):
./target/release/wamn-gates --log-level warn bench \
  --hello components/target/wasm32-wasip2/release/hello.wasm \
  --memhog components/target/wasm32-wasip2/release/memhog.wasm \
  --busyloop components/target/wasm32-wasip2/release/busyloop.wasm

# S2 gates (qps + p99, saturation, chaos/RLS/injection) — needs a Postgres.
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

# [2.2] production wamn:postgres — per-project pooling + credential resolution +
# per-project policy (multiproject gate), with the S2 gates as regression. Needs
# a Postgres AND a SUPERUSER url: the gate provisions two per-project databases
# (wamn_app is NOSUPERUSER/NOCREATEDB, as in production). `--mode all` runs the
# S2 gates then the multiproject gate; `--mode multiproject` runs only the new one.
# Local iteration (same throwaway container as S2, plus WAMN_PG_ADMIN_URL):
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5450/wamn \
  ./target/release/wamn-gates --log-level error pgbench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (co-located, no cpu limit — S2 CFS lesson;
# WAMN_PG_ADMIN_URL is the superuser used only to provision the project DBs):
kubectl -n wamn-system apply -f deploy/pgbench-multiproject-job.yaml
kubectl -n wamn-system logs -f job/pgbench-multiproject

# [2.3] managed Postgres provisioning — per-project database + credential on the
# shared CloudNativePG cluster (D6). NEW crates/wamn-provision = PURE builders
# (project-id slug validation + wamn-66x reserved-prefix reject; CREATE DATABASE /
# ensure wamn_app role [NOSUPERUSER NOCREATEDB] / REVOKE CONNECT FROM PUBLIC + GRANT
# to wamn_app; connection-URL composer; credential Secret + WAMN_PG_PROJECTS_FILE
# renderers). NEW `wamn-host provision-project` subcommand (imperative CLI, the
# publish-catalog precedent) drives them as superuser (idempotent + additive). NEW
# `wamn-gates provisionbench` gate proves routing/resolution (via the plugin's
# StaticCredentialProvider fed the emitted projects-file JSON) + database-level
# isolation + least privilege + Secret layout. Isolation = per-project DATABASE +
# per-DB CONNECT (PUBLIC revoked) + RLS within, under ONE shared cluster-global
# wamn_app role. docs/provisioning.md.
cargo test -p wamn-provision   # naming/slug/reserved-prefix + SQL shape + secret + live-apply
cargo clippy -p wamn-provision --all-targets && cargo fmt -p wamn-provision --check
# optional plain-PG live-apply (throwaway postgres:18; SUPERUSER url — CREATE
# DATABASE/ROLE; asserts wamn_app CONNECT granted + PUBLIC revoked + least priv;
# skips when unset):
docker run -d --rm --name wamn-prov-pg -p 5460:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_PROVISION_PG_URL=postgres://postgres:postgres@127.0.0.1:5460/wamn cargo test -p wamn-provision
# provisionbench GATE (pure host-side tokio_postgres, NO wasm guest; provisions two
# project databases via the REAL provision-project path). Substrate-agnostic — runs
# locally against the SAME throwaway postgres:18 (superuser):
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5460/wamn \
  ./target/debug/wamn-gates --log-level error provisionbench
docker stop wamn-prov-pg
# The production tool is `wamn-host provision-project --project <id>
# --admin-database-url <superuser> [--emit-secret -|<path>] [--emit-projects-file -]`.
# In-cluster gate of record (against the shared CNPG cluster = the D6 substrate,
# stood up ALONGSIDE the guardrailed deploy/postgres.yaml pod). CNPG operator +
# cluster (pinned CNPG 1.29.2; superuser enabled for provisioning; non-TLS pg_hba;
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

# S3 gates (dispatch p99, hot-reload, checkpoint/resume idempotency). The
# dispatch gate is same-binary and needs no DB; hot-reload/resume use the s3.*
# fixture tables (also in deploy/postgres-init.sql).
./target/release/wamn-gates --log-level error flowbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster (same co-located / no-cpu-limit Job topology as pgbench):
kubectl -n wamn-system apply -f deploy/flowbench-job.yaml
kubectl -n wamn-system logs -f job/flowbench

# S4 gates (HTTP hop / interpreted-vs-composed gap / config parse). No DB.
# Two extra fixtures need external tools (one-time installs):
#   jco: npm i -g @bytecodealliance/jco    (JS/JCO interpreted node)
#   wac: cargo install wac-cli             (composed frozen flow)
# node-rs + flow-driver build with the other guests; the JS node and the wac
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

# S5 gates (log() overhead / 10k-lines/s loss / drops-counted / enrichment).
# Needs an OTel Collector + Loki (the collector bridges the host's OTLP-gRPC
# logs to Loki's HTTP OTLP ingest). logspewer builds with the other guests.
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

# [9.1] OTel trace pipeline (crates/wamn-gates tracebench + host spans +
# deploy/tempo.yaml + a traces pipeline in deploy/otel-collector.yaml) —
# host-native spans -> OTel Collector -> Tempo (D13), enriched with execution
# context. The exporter + W3C propagator + wash-runtime's inbound/outbound HTTP
# + component-invoke spans are ALREADY wired (the fork's initialize_observability,
# active on OTEL_* — the S5 switch); 9.1 adds host-side, WAMN-SIDE (no fork
# patch): a wamn.postgres DB span (plugins/wamn_postgres.rs db_span:
# db.system/db.operation + tenant/project/component from the same non-spoofable
# claim maps that inject app.tenant; wraps one-shot + txn query/execute) + a
# wamn.trigger span (dispatch.rs trigger_span: flow/run_id/flow_version/
# trigger_source/tenant — the HOST-KNOWN path where the dispatcher mints
# flow/run_id; both cron + outbox fire sites) + deploy/tempo.yaml (single-binary
# Tempo, OTLP-gRPC :4317 / TraceQL :3200) + the collector traces pipeline (otlp
# -> otlp/tempo) (+ local variants tempo-local.yaml / otelcol-local.yaml).
# tracebench drives pgprobe op 6 (SELECT pg_sleep(0), FIXTURE-FREE) UNDER the
# real trigger_span so the plugin DB span nests, then queries Tempo's TraceQL
# API asserting ONE trace threads trigger -> wamn:postgres, both enriched (the
# S5 logbench->Loki analog, through the PRODUCTION span builders not gate
# scaffolding). Cross-pod traceparent INJECTION + guest-minted run_id/node_id =
# 9.2 (deferred; the wamn:node traceparent field is frozen [5.4] but unwired).
# 4 mutants killed (DB-span tenant / trigger flow field / DB-span name / DB-span
# parent-inheritance [threading]) each fail a NAMED tracebench assertion.
# docs/tracing.md.
cargo clippy -p wamn-host -p wamn-gates --all-targets \
  && cargo fmt -p wamn-host -p wamn-gates --check
# Local iteration (throwaway Postgres + Tempo + collector on a docker network;
# --log-level info is LOAD-BEARING — the OTLP trace filter is level-tied and the
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
# S2 lesson; pg_sleep is schema-free so it is ADDITIVE on the shared Postgres).
# A HOST change (the plugin + dispatch spans) => FULL docker rebuild (both
# --target stages + kind load BOTH images):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/tempo.yaml -f deploy/otel-collector.yaml
kubectl -n wamn-system rollout status deploy/tempo deploy/otel-collector --timeout=120s
kubectl -n wamn-system apply -f deploy/tracebench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/tracebench --timeout=180s
kubectl -n wamn-system logs job/tracebench

# [9.2] trace context propagation (host-stamped, wamn-rvd): the follow-on 9.1
# deferred — a trace threads ACROSS a process boundary, HOST-ENFORCED. A CARRIED
# FORK COMMIT (d3d83f3, 3rd on wamn/2.5.2; docs/wash-runtime-fork.md ledger)
# injects the current W3C trace context in wash-runtime
# DefaultOutgoingHandler::send_request — the SINGLE production outbound wasi:http
# send path (guest -> CtxHttpHooks -> HttpServer::outgoing_request -> the default
# handler), symmetric to the inbound extract the fork already did. Because EVERY
# outbound wasi:http flows through this handler regardless of SDK use, an
# SDK-bypassing custom node cannot break trace continuity (structural). A no-op
# when observability is off. WAMN-SIDE: wamn-node-sdk RunContext::trace_headers/
# apply_trace_context (the SDK propagation helper) + the wamn-nodes http-request
# node forwards run.traceparent onto its outbound request (a config header of the
# same name wins). wamn:postgres needs no change (no traceparent wire concept; the
# 9.1 DB span nests by parentage). flowrunner is host-invoked (exported run(), no
# inbound HTTP) so its RunContext.traceparent stays None (host inject still traces
# its outbound); a per-run traceparent for host-invoked guests = follow-up
# wamn-fl3. GATE = traceproof, a DEPLOYED cross-pod proof (the 9.1 in-proc
# tracebench BYPASSES wash's HTTP server so cannot exercise the outbound send
# path): NEW components/fixtures/trace-relay (wash-served; makes ONE BARE outbound
# GET, no trace header) -> NEW serve-echo (wamn-gates subcommand; a plain server
# reflecting the received traceparent as JSON) -> the relay returns serve-echo's
# body -> traceproof asserts the reflected trace id == the one it sent (threaded
# the boundary) + the span id differs (host minted a child client span, not a
# blind copy). A traceparent reaching serve-echo can ONLY be host-injected (the
# relay never sets one). 1 fork mutant killed (inject no-op -> serve-echo sees no
# traceparent -> traceproof "downstream received traceparent" FAILS).
# docs/tracing.md § Cross-pod propagation.
cargo test -p wamn-node-sdk -p wamn-nodes   # trace_headers/apply + http-node forward + explicit-header-wins
cargo test -p wamn-gates --bin wamn-gates traceproof   # w3c/header-parse units
cargo clippy -p wamn-node-sdk -p wamn-nodes -p wamn-gates --all-targets \
  && cargo fmt -p wamn-node-sdk -p wamn-nodes -p wamn-gates --check
(cd components && cargo build --release --target wasm32-wasip2 -p trace-relay)
cargo clippy --manifest-path components/fixtures/trace-relay/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/fixtures/trace-relay/Cargo.toml --check
# No local run: the fork inject fires ONLY on the real washlet outbound path
# (in-proc gates bypass wash's HTTP server), so the deployed gate IS the proof.
# In-cluster gate of record. A FORK rev bump => FULL docker rebuild (both --target
# stages) + kind load BOTH images + roll the hostgroup (picks up the new
# wash-runtime):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system rollout restart deploy/hostgroup-default
kubectl -n wamn-system rollout status deploy/hostgroup-default --timeout=180s
# Push the relay component to the in-cluster registry (the 4.1b/POC-F1 pattern,
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

# S6 gates (test-host plugin-swap: sameness / 24h-delay under virtual time /
# egress spy / S3 regression). Needs a Postgres. The test host provisions a
# FRESH ephemeral schema through the SUPERUSER url (the runner's wamn_app role
# is NOSUPERUSER/NOCREATEDB and cannot create schemas). The extended flowrunner
# (delay + http-call nodes, unqualified table names resolved via host-injected
# search_path) builds with the other guests — no extra fixture.
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

# [2.6] DB-path egress review — STATIC gate: no shipped workload imports
# wasi:sockets, so the wamn:postgres plugin (+ the allowed_hosts-gated, S6
# egress-spied wasi:http) is the only egress. Pure wasm-import introspection —
# no DB, no network — so it is identical in-cluster and locally: NO in-cluster
# Job of record. FAIL path is unit-tested (cargo test -p wamn-gates egressbench).
# See docs/security-db-path.md.
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

# [5.1] flow-graph schema crate (crates/wamn-flow) — canonical flow JSON: types,
# validation, import/export, version diff. Pure Rust, no host/DB. Tests cover
# fixture round-trip, structural validation, JSON-Schema conformance (boon),
# schema drift-guard, and diff. docs/flow-schema.md + docs/flow-schema.schema.json.
cargo test -p wamn-flow
cargo clippy -p wamn-flow --all-targets && cargo fmt -p wamn-flow --check
# regenerate the published JSON Schema contract after changing the types:
cargo run -p wamn-flow --example print-schema > docs/flow-schema.schema.json

# [5.2] production flow-runner engine (crates/wamn-runner) — the PURE, synchronous
# reducer over a wamn-flow (5.1) graph: ported-edge walk from `entry`, branch/merge,
# error-path routing, and retry/backoff keyed MECHANICALLY off the wamn:node error
# taxonomy (retryable/rate-limited/terminal/invalid-input/cancelled), plus the shared
# per-(node-type,credential,host) throttle + per-flow concurrency accounting. No
# host/DB/wasm/clock — the whole engine is unit-tested with no cluster (the wamn-api
# split). docs/flow-runner.md. No JSON-schema (an engine, not a contract).
cargo test -p wamn-runner
cargo clippy -p wamn-runner --all-targets && cargo fmt -p wamn-runner --check
# The components/flowrunner GUEST now DRIVES the engine (adopts the wamn-flow schema,
# replacing the S3 ad-hoc IR); the S3 flowbench + S6 testhostbench gates (below) are
# its regression, unchanged — both PASS on the engine-driven runner in-cluster and
# locally. Rebuild the guest (part of the guest build above), then re-run those gates:
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)
cargo clippy --manifest-path components/flowrunner/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/flowrunner/Cargo.toml --check

# [5.3] standard node library v1 (crates/wamn-node-sdk + crates/wamn-nodes) — the
# production node vocabulary + the dispatch-time capability policy table.
# wamn-node-sdk = the node authoring CONTRACT (Node trait, RunContext, the NodeCtx
# capability facade, and the wamn:node error taxonomy — now DEFINED here and
# re-exported by wamn-runner; the 5.4 freeze layers the WIT on top). wamn-nodes =
# the library: transform + conditional (JMESPath — off-the-shelf frozen SPEC, no
# language of our own; no arithmetic => the no-float rule holds through a
# transform by construction; numbers ride serde_json::Number exactly, test-pinned),
# http-request (mechanical status->taxonomy: 429->rate-limited w/ Retry-After +
# target-host throttle key, 408/5xx->retryable, 4xx->terminal, egress-denial->
# terminal), postgres entity ops (catalog-derived via the AUDITED 4.1 wamn-api
# Router — allowlisted identifiers, $n params, server-side tenant; reads the
# wamn_catalog snapshot), postgres-query (D8 raw SQL: $n-bound params, behind the
# RawSql capability, DEFAULT OFF — dispatch dies capability-denied naming the
# flag; enablement gated on wamn-1nd), respond (passthrough + pure status_for).
# PURITY RULE (5.13) enforced MECHANICALLY: node crates depend on the SDK ONLY,
# never the runner — tests/purity.rs walks cargo metadata's normal-edge closure +
# pins the exact direct-dep allowlist. Policy enforced TWICE (grant check before
# the node runs + a gated ctx that NotGrants undeclared calls). Loops are
# STRUCTURAL v1 (cycles + conditional; split/merge nodes land with 5.11);
# email/notify deferred (no email egress capability). components/flowrunner
# ADOPTS the library (an `expression` config routes transform/conditional to it;
# fixture shapes stay byte-identical; error rows recorded ONLY when the engine
# will ROUTE the emission — will_error_route mirrors the exact RetryPolicy
# computation; retry Wait stays defensive, queue-layer scheduling = fqg.4), so
# flowbench/testhostbench/f1bench are its regression — all three re-ran PASS
# in-cluster on the adopted guest (host unchanged => cheap overlay image).
# Mutants killed: neutered grant check / allow-all gated ctx / pg taxonomy swap /
# http taxonomy swap / runner-dep purity violation. docs/node-library.md.
# No JSON-schema (config schemas land with the 5.4 contract freeze).
cargo test -p wamn-nodes             # nodes + policy negatives + purity lint
cargo test -p wamn-node-sdk
cargo test -p wamn-runner            # taxonomy re-export + port drift-guard regression
cargo clippy -p wamn-node-sdk -p wamn-nodes --all-targets \
  && cargo fmt -p wamn-node-sdk -p wamn-nodes --check
# guest adoption regression = the S3/S6/F1 gates above ([5.2]/[POC-F1] blocks)

# [5.4] wamn:node contract 0.1 FROZEN + SDK scaffolding — docs/wamn-node.wit now
# carries the wamn-postgres-style STATUS header (FROZEN 0.1.0, 0.1.x additive-
# only, deferred-to-0.2 = the WASI-0.3 async revision [5.16]; additive candidate
# = emit [5.15]). THREE 5.3-surfaced deltas folded in PRE-freeze so WIT == SDK
# from day one: run() returns an emission record {payload, port: option<string>}
# (absent = main — the engine's ported edges; branch nodes emit true/false),
# traceparent is option<string> (9.2 tracing not wired; a required field every
# runner must fabricate would freeze a lie), rate-limit-detail gained
# target-host (the shared-throttle key only the erroring node can observe). The
# OPTIONAL imports (payloads/credentials/control) are frozen but have NO host
# impls yet (5.10/5.9/5.12) — linking one fails instantiation; the payloads
# wasi:io version pin is provisional until 5.10 activates it. NEW
# crates/wamn-node-guest = the custom-node componentization scaffolding: impl
# the SAME wamn_node_sdk::Node trait the standard library uses, then
# wamn_node_guest::export_node!(MyNode) is the ENTIRE componentization
# (wit-bindgen wrapped via pub_export_macro; NoCapsCtx — the contract's minimal
# `world node` imports NOTHING, so the node is physically incapable of I/O);
# SEPARATE crate because the purity lint pins the SDK's deps == {serde_json}.
# NEW components/samples/sample-node = the reference custom node AND the frozen-contract
# conformance fixture (the POC-F2 seed). NEW crates/wamn-node-manifest = the
# wamn.node.manifest OCI annotation model (design-note 8: display name,
# config/input/output JSON Schemas, ordering-policy support, output ports
# ["error" reserved-rejected]; capability grants deliberately NOT here — note 7:
# derived from actual WIT imports) + the published contract
# docs/wamn-node-manifest.schema.json (schemars + boon + drift-guard, the
# 5.1/3.1 pattern). Determinism-lint rules (note 9) SPECIFIED in the design
# notes (import allowlist; wasi:sockets forbidden — 2.6); the MECHANICAL lint
# lands with the 5.5 builder (egressbench is the precedent + backstop).
# DRIFT-GUARDS (crates/wamn-node-sdk/tests/wit_coherence.rs): every vendored
# WIT copy (3 S4 guests + wamn-node-guest + the host bindgen copy) must be an
# in-order code-line subsequence of docs/wamn-node.wit, the 4 trimmed guest
# copies byte-identical to each other, and the exact WIT lines the SDK mirrors
# are pinned. GATES: nodebench gained a `sample` mode (frozen-contract
# conformance through REAL wasm — all 5 taxonomy variants, port selection
# [absent = main], echo round-trip, streamed-payload refusal; topology-
# independent, runs in --mode all, skips if the fixture is absent) and the S4
# hop/gap/config gates regress on the amended ABI (node-rs + jco node-ts + wac
# flow_composed rebuilt on the emission signature); egressbench gains
# sample_node (zero-import: egress=[]). TS defineNode SDK deferred to 5.5/F2
# (the S4 node-ts fixture already proves the jco path). Mutants killed: frozen-
# WIT line drift (both coherence guards), scaffolding taxonomy swap (unit test
# + the sample gate through real wasm), manifest schema drift (schema_drift).
cargo test -p wamn-node-sdk      # incl the wit_coherence drift-guards
cargo test -p wamn-node-guest    # conversion glue + NoCapsCtx units
cargo test -p wamn-node-manifest # fixture/negatives/conformance/drift
cargo clippy -p wamn-node-guest -p wamn-node-manifest --all-targets \
  && cargo fmt -p wamn-node-sdk -p wamn-node-guest -p wamn-node-manifest --check
# regenerate the published manifest schema after changing the types:
cargo run -p wamn-node-manifest --example print-schema > docs/wamn-node-manifest.schema.json
# the sample node builds with the other guests (see the S4 block above for the
# nodebench command incl --sample; the [2.6] egressbench command includes it)

# [5.7] run-state persistence (crates/wamn-run-store) — durable runs/node_runs +
# BRANCH-AWARE replay reconstruction + partial re-run. The PURE crate (model +
# reconstruct + rerun planners; no DB/wasm/clock — the wamn-api/wamn-runner split)
# drives two ADDITIVE wamn-runner primitives: Plan::resume(run_id,input,completed)
# rebuilds the exact frontier by folding recorded emissions (an error-routed node is
# recorded as an emission on the `error` port, so reconstruction needs no error
# taxonomy), and Plan::seed_at(run_id,node,payload) for partial re-run. A replay /
# partial-re-run is a NEW run linked via replay_of/root_run_id (immutable audit
# lineage). docs/run-state.md. No JSON-schema (a store model, not a contract).
cargo test -p wamn-run-store
cargo test -p wamn-runner   # the resume/seed_at primitives (regression)
cargo clippy -p wamn-run-store --all-targets && cargo fmt -p wamn-run-store --check
# optional live-apply gate (deploy/run-state.sql on a throwaway PG; superuser URL
# provisions wamn_app; asserts tenant RLS isolation + the idempotency index + the
# node_runs FK cascade; skips cleanly when unset):
docker run -d --rm --name wamn-runstore-pg -p 5458:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RUN_STORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5458/wamn cargo test -p wamn-run-store
docker stop wamn-runstore-pg
# The components/flowrunner GUEST now persists a node_runs row per node and RESUMES
# branch-aware by reconstruction (retiring the S3 step_seq as the resume source;
# `delay` parks via runs.state_json). The runs/node_runs tables are ADDITIVE to the
# STANDALONE deploy/run-state.sql (production) AND to the s3 gate fixtures
# (deploy/postgres-init.sql + the testhostbench ephemeral template) so the S3
# flowbench + S6 testhostbench gates exercise the rewired runner — both PASS
# (in-cluster gate of record + locally). Rebuild the guest, re-run those gates (the
# S3/S6 commands above); the in-cluster postgres gains s3.runs/s3.node_runs
# additively (kubectl exec psql — shared-cluster guardrail, never recreate the pod).
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)
cargo clippy --manifest-path components/flowrunner/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/flowrunner/Cargo.toml --check

# [5.14] durable run queue & runner scaling (crates/wamn-run-queue) — the D3 HYBRID
# walking skeleton: a Postgres FOR UPDATE SKIP LOCKED run_queue that co-transacts with
# the 5.7 runs row (ONE durability domain) + D15 write-ahead (runs.status='dispatched')
# + janitor (orphan -> 'infrastructure-failure') + single-owner leases + reclaim +
# reconciliation-due timing + a minimal NATS-core doorbell (publish hint / subscribe-
# claim) + PER-PARTITION OWNERSHIP (deploy/run-queue.sql partition_owner lease table +
# acquire_partitions_sql / claim_partition_head_sql: partitioned(key) runs dispatch
# in-order per key across replicas — a replica leases a partition [INSERT..ON CONFLICT
# arbitration, only an expired lease is stolen] then claims that key's runs head-first,
# one in flight at a time; global claim_batch_sql gains `partition_key IS NULL` so
# partitioned runs go ONLY through the ownership path; in-order failover via partition-
# lease expiry+reacquire; ordering key (available_at, run_id); wedge-on-terminal-failure
# is a 5.11 policy seam). PURE crate (claim/lease/janitor/reconcile/partition decisions
# + parameterized $n SQL
# builders; NO DB/NATS/clock — now is a passed-in millis; the wamn-run-store split);
# reuses wamn-run-store RunStatus (Dispatched/InfrastructureFailure — the seam 5.7
# reserved) rather than redefining the run lifecycle. run_queue is a SEPARATE table
# (deploy/run-queue.sql, STANDALONE + ADDITIVE to run-state.sql, FK->runs ON DELETE
# CASCADE); the 5.7 runs.status CHECK already lists dispatched/infrastructure-failure
# so there is NO 5.7 schema change. The flowrunner GUEST is UNCHANGED (host-side queue)
# so flowbench (S3) + testhostbench (S6) stay green as regression. DEFERRED to follow-
# up beads: checkpoint/resume-on-replica-loss, the shared cron+outbox dispatcher, and
# the guest-claims-from-queue rewire. docs/run-queue.md.
# No JSON-schema (a store model, not a contract).
cargo test -p wamn-run-queue
cargo clippy -p wamn-run-queue --all-targets && cargo fmt -p wamn-run-queue --check
# optional live-apply gate (deploy/run-state.sql + run-queue.sql on a throwaway PG;
# superuser URL provisions wamn_app; asserts the SKIP LOCKED claim predicate [Ready
# claimed, Parked/Leased skipped, expired-lease reclaimed] + janitor sweep + tenant
# RLS isolation + FK cascade + partition acquire/head-claim via the real builders;
# skips cleanly when unset):
docker run -d --rm --name wamn-rq-pg -p 5459:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RUN_QUEUE_PG_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn cargo test -p wamn-run-queue
# queuebench GATE (crates/wamn-host, PURE host-side tokio_postgres claimers — NO wasm
# guest): D15 dispatch SLOs (write-ahead p99<15ms / fast-path p99<10ms), SKIP LOCKED
# throughput (exactly-once + completeness, ~1-5k/s), lease-expiry reclaim, park
# (park/wake budget-neutrality: attempts counts CRASH EVIDENCE only — a claim bumps
# it iff it reclaims an expired lease, so a first claim + every park->wake re-claim
# are free and a flow parking 10x with max_attempts=3 completes on BOTH claim paths;
# wamn-fqg.5), janitor,
# NATS-core doorbell (async warm p50<25ms/p99<100ms), partition (partitioned(key)
# in-order per key across concurrent replicas + exactly-once + in-order failover).
# Provisions an EPHEMERAL schema (runs + run_queue + partition_owner) via the SUPERUSER
# url (wamn_app is NOSUPERUSER). Reuse the
# throwaway PG above (the live-apply gate created wamn_app) + a throwaway NATS:
docker run -d --rm --name wamn-rq-nats -p 4232:4222 nats:2.12.8-alpine
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/release/wamn-gates --log-level error queuebench \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn \
  --nats-url nats://127.0.0.1:4232 --mode all
docker stop wamn-rq-pg wamn-rq-nats
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS lesson;
# WAMN_PG_ADMIN_URL is the superuser that provisions the ephemeral schema; nats is the
# operator chart's mTLS Service [verify_and_map] — the job mounts the wasmcloud-runtime-
# tls cert so the doorbell connects, no deploy/nats.yaml). A HOST change => full docker
# rebuild (docker build --target host -t wamn-host:dev . && docker build --target gates
# -t wamn-gates:dev . && kind load docker-image wamn-host:dev --name wamn &&
# kind load docker-image wamn-gates:dev --name wamn):
kubectl -n wamn-system apply -f deploy/queuebench-job.yaml
kubectl -n wamn-system logs -f job/queuebench

# [5.14] checkpoint/resume on replica loss — first-class FAILOVER (wamn-fqg.2): the
# run-queue lease RECLAIM (5.14) + 5.7 branch-aware RECONSTRUCTION composed into one
# path. A runner dies mid-effect, its run lease ages out, a SECOND replica reclaims the
# run (claim_batch_sql; the expired-lease reclaim is what bumps attempts — crash
# evidence, wamn-fqg.5) and drives the SAME unchanged flowrunner GUEST,
# which reconstructs from node_runs + completes with EXACTLY ONE side effect, ending
# `completed` (never infrastructure-failure). Guest byte-UNCHANGED + host-orchestrated
# (guest-self-claim is fqg.4); the hardening is host-side + in the PURE crate:
# janitor_sweep_sql gained `AND r.status IN ('dispatched','running')` so the janitor
# never relabels a reclaimed-and-completed run (completion-vs-failover race guard; the
# host also dequeues AFTER completion); the REVERSE ordering (janitor reaps a
# still-running slow resume first) is covered by the runner's deliberately
# UNCONDITIONAL completion write overriding the verdict. NEW
# crates/wamn-host/src/failoverbench.rs (failover / janitor-guard / reverse-race
# modes) provisions an EPHEMERAL schema unioning the flow tables
# (flows/flow_runs/sink/runs/node_runs) with run_queue via the SUPERUSER url. The
# failover mode also proves the exactly-once came from RECONSTRUCTION, not just the
# sink constraint (pg-write node_runs.seq==2 = prefix skipped, not replayed) and runs
# the janitor INSIDE the completion->dequeue window (queue row forced reap-eligible).
# All three race/reconstruction assertions are MUTATION-TESTED (broken reconstruct,
# guarded mark_completed, unguarded janitor each FAIL the gate). The guard is also
# unit-shape-tested + live-apply-behavioral-tested (a 'completed' run with a stale
# expired+spent queue row is NOT relabeled, a real orphan is) and queuebench's janitor
# mode is regression; the guest is unchanged so flowbench/testhostbench regress by
# non-change. docs/run-queue.md § Checkpoint/resume on replica loss.
cargo test -p wamn-run-queue   # incl the janitor completion-race guard (shape + live-apply)
cargo clippy -p wamn-run-queue --all-targets && cargo fmt -p wamn-run-queue --check
# Local iteration (reuse the throwaway PG above [wamn-rq-pg on 5459, wamn_app created by
# the run-queue live-apply gate]; failoverbench needs NO NATS, and the guest is UNCHANGED
# so NO wasm rebuild — reuse the built flowrunner.wasm):
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/release/wamn-gates --log-level error failoverbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn --mode all
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS lesson;
# WAMN_PG_ADMIN_URL is the superuser that provisions the ephemeral schema; no NATS). A
# HOST change => full docker rebuild (both --target stages + kind load BOTH images):
kubectl -n wamn-system apply -f deploy/failoverbench-job.yaml
kubectl -n wamn-system logs -f job/failoverbench

# [5.14] guest-self-claim — the flowrunner GUEST claims its own work from the queue
# (wamn-fqg.4): the production dispatch path, guest-side (the 5.14 walking skeleton's
# last deferred item). NEW guest export run-next(lease-ttl-ms)->tuple<claimed,run-id,
# outcome> (components/flowrunner/wit/world.wit + src/lib.rs execute_claimed): ONE turn
# of the dispatch loop — claim_batch_sql(1) [FOR UPDATE SKIP LOCKED, UNPARTITIONED] ->
# read runs.flow_id + input_json (NEW wamn_run_store::sql::select_run_dispatch_sql — the
# SR2 home, where fl3 adds `traceparent` next; drives the RECORDED flow, not a FLOW_ID
# const) -> mark_running_sql (dispatched->running) -> drive the 5.2 engine reconstructing
# from node_runs, RENEWING the lease PER NODE (renew_lease_sql — a live-but-slow runner is
# never reclaimed) -> dequeue_sql on completion / park_sql on a delay (push available_at +
# RELEASE the lease = free wake, fqg.5/.7). outcome 0=completed/1=parked/2=failed;
# claimed=false = queue empty. OWNER is HOST-INJECTED: the wamn:postgres plugin
# (crates/wamn-host/src/plugins/wamn_postgres.rs) sets a NEW app.runner GUC from the
# wamn.runner config (per replica, mirroring app.tenant/search_path) that the guest reads
# (current_setting('app.runner',true)) as its non-spoofable lease owner. The guest deps
# wamn-run-queue default-features=false so ONLY the pure claim-path builders (sql.rs) enter
# the wasm — cron/outbox/dispatch (croner/chrono) is gated behind the NEW default
# `dispatcher` feature. BOUNDED-LEASE OVER-CLAIM FIX (the root cause the guest self-claim
# exposed): running claim_batch_sql(1) through the plugin's cached prepared-statement path
# intermittently leased the WHOLE batch on a LIMIT-1 claim — the classic Postgres
# `FOR UPDATE SKIP LOCKED LIMIT n`-in-a-re-scannable-subquery over-claim (the planner puts
# the LockRows subplan on the inner side of a nested-loop join and RESCANS it per outer row,
# SKIP LOCKED advancing to fresh rows each rescan; plan-dependent, so it surfaced only
# through the plugin path, not raw tokio_postgres). A first rewrite from `WHERE (pk) IN
# (subquery)` to a plain `FROM (subquery)` derived table did NOT fix it (a derived table is
# not an evaluation fence) — which is what pinned the cause to plan-driven subquery
# re-execution, NOT the SQL shape. The fix fences the locking SELECT in a CTE `AS
# MATERIALIZED` (single evaluation into a tuplestore regardless of plan), applied to BOTH
# claim_batch_sql AND claim_partition_head_sql (the per-partition head claim = wamn-fqg.10,
# which had the IN-subquery form). Verified: local failoverbench --mode all reproduced the
# over-lock in ~50% of runs before the fix and 0 of 40 after (instrumented run_query showed
# a single claim execution returning 10 rows); a direct psql proof + the wamn-run-queue
# live-apply gate confirm both CTE builders lease exactly n on PG18. docs/run-queue.md §
# Claim-builder shape. failoverbench gains claim/park/heartbeat modes driving run-next
# against the SAME ephemeral schema: claim (single-drain + N-replica exactly-once via SKIP
# LOCKED + wrong-flow: an alt-flow run drives reverse->"tpiecer" not "RECEIPT"), park (a
# delay run parks+releases the lease, then wakes+completes), heartbeat (per-node renewal
# ADVANCES lease_expires_at across a long walk — a DETERMINISTIC lease-value poll, no steal
# race). The guest's DIRECT exports (run/run-s6) are byte-UNCHANGED so flowbench(S3)/
# testhostbench(S6) regress by non-change. 4 guest MUTANTS killed (drop dequeue / read
# FLOW_ID const / park->dequeue / drop per-node renew each FAIL a NAMED failoverbench mode)
# + 3 fence MUTANTS (drop `AS MATERIALIZED` on claim_batch_sql / claim_partition_head_sql,
# and inject a `FROM (` derived table, each FAIL a NAMED drift-guard).
# SEED-vs-LIVE CAVEAT: the gates SEED run_queue directly (the write-ahead a dispatcher
# does) — the LIVE dispatcher->queue->runner chain closes only with a runner Deployment
# (a52-analog, FILED follow-up). docs/run-queue.md § Guest-side queue claim.
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
# janitor/reverse regression). A GUEST + HOST change => FULL docker rebuild BOTH --target
# stages + kind load BOTH images (+ flowbench/testhostbench regress on the new guest):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/failoverbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/failoverbench --timeout=240s
kubectl -n wamn-system logs job/failoverbench

# [5.14] shared trigger dispatcher — cron + outbox + parked-wake for ALL projects
# (wamn-fqg.3): the always-on control-plane loop D3/D4 locked (LISTEN/NOTIFY removed
# entirely — the outbox is POLLED with adaptive per-project intervals; NATS-core
# doorbell hints on wamn.doorbell.{tenant}). PURE decisions in crates/wamn-run-queue:
# cron.rs next_fire/due_tick over an INJECTED now (croner dep, UTC, misfire collapse =
# fire only the latest missed tick; tick identity truncated to the second so racing
# replicas agree), outbox.rs match_outbox + envelopes, dispatch.rs adaptive
# next_interval + Firing. Run ids are DETERMINISTIC — {flow}:cron:{tick:013} /
# {flow}:outbox:{seq} — so restart, redelivery, and two live replicas racing all
# collapse on the write-ahead ON CONFLICT: exactly-once with NO leader election.
# wamn_run.outbox is ADDITIVE to deploy/run-queue.sql (the PRODUCER inserts in ITS
# OWN txn — D4 "outbox insert shares the user txn"; the dispatcher polls SKIP LOCKED,
# RE-READS the registry INSIDE the txn after the poll [closes the flow-activation
# race — an activated flow's events are never consumed as unmatched], fires
# write_ahead_triggered_run_sql [persists input_json + trigger_source — what a 5.7
# replay re-runs; payload spliced VERBATIM, no float-lossy round trip] + enqueue
# ONLY when the write-ahead WON [a losing re-fire must not resurrect a completed
# run's queue row — the ghost-dispatch guard], and acks everything not HELD — ALL IN
# ONE txn: crash = redeliver AND retract atomically. A skipped-unparseable active
# row-event flow's (table,event) is HELD: pending, not consumed — version skew
# degrades to delayed delivery, never silent loss; likewise a flows.flow_id
# column that differs from the graph's validated flow-id [run ids are minted
# from the COLUMN, so the 5.1 slug rule is extended to it by equality]). The
# runs table IS the cron
# state: cron_last_run_sql recovers the last tick from the FLOW-EXCLUSIVE
# max(run_id) (flow_id + trigger_source='cron' — never a lexical id range: flow
# ids are user text, text order is collation-dependent, a range leaks foreign
# ids into the anchor); unsatisfiable schedules ERROR + are quarantined per
# project. Host driver crates/wamn-host/src/dispatch.rs: long-lived
# `dispatch` subcommand (per-project connections — D3 "reconciliation follows
# connection ownership", no cross-DB sweep; registry = active flows' graph_json
# parsed via wamn-flow; cron-aware adaptive sleep; always-on hardening: re-dial on
# dropped connection, per-sweep deadline, stale-cron-hint clear on failure — a
# failing project never wedges the loop) + the SAME tick engine driven by
# dispatchbench with STEPPED time (the 11.1 fast-forwardable-cron discipline: a
# nightly cron + a 3-day outage gate in ms). dispatchbench modes: cron (exactly-once
# per tick, restart-no-dup via DB-recovered anchor, misfire collapse, bootstrap-
# from-sight, enqueue-trap co-txn atomicity) / outbox (fire per flow×row, payload
# verbatim, unmatched consumed, skew HELD, junk/webhook rows never wedge, redelivery
# dedupes WITHOUT ghost resurrection, ack-trap + fire-trap co-txn atomicity BOTH
# ways) / race (TWO live dispatchers ticking concurrently: won-inserts == distinct
# runs AND contention proven — losing attempts counted, both replicas must win) /
# fairness (2 projects: a 120-row backlog is batch-bounded OLDEST-FIRST + doesn't
# starve the quiet project's first sweep; intervals adapt independently) / wake
# (parked run hinted only once due; a firing's hint carries the WON run id, only
# after commit) / live (the real run loop: sub-500ms fire BESIDE a permanently
# failing project, reconnect after backend kill, cron-aware sleep under a fixed 5s
# interval). All four gate-killing mutants verified: observed-now tick id, ack-first
# split txn, unconditional enqueue, cron-blind sleep each FAIL the gate. Guest
# UNCHANGED (flowbench/testhostbench regress by non-change). docs/run-queue.md §
# Trigger dispatcher.
cargo test -p wamn-run-queue   # incl cron calendar edges + outbox/adaptive decisions
cargo clippy -p wamn-run-queue --all-targets && cargo fmt -p wamn-run-queue --check
# optional live-apply gate (run-state.sql + run-queue.sql now incl the outbox; real
# builders via PREPARE/EXECUTE: RLS-scoped poll, co-txn fire+ack, CRASH-ROLLBACK
# atomicity + redelivery dedupe, cron last-tick recovery, wake scan; skips when unset):
docker run -d --rm --name wamn-rq-pg -p 5459:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RUN_QUEUE_PG_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn cargo test -p wamn-run-queue
# dispatchbench GATE (pure host-side, NO wasm guest; provisions TWO ephemeral project
# schemas via the SUPERUSER url — reuse the throwaway PG above [wamn_app created by
# the live-apply gate] + a throwaway NATS for the wake/live doorbell hints):
docker run -d --rm --name wamn-rq-nats -p 4232:4222 nats:2.12.8-alpine
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/release/wamn-gates --log-level error dispatchbench \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn \
  --nats-url nats://127.0.0.1:4232 --mode all
docker stop wamn-rq-pg wamn-rq-nats
# The production service is `wamn-host dispatch --projects-file <json>` (one entry
# per project: {"name": {"url", "tenant", "schema"}}) or --database-url/--tenant/
# --schema for one project. Production manifest = deploy/dispatcher.yaml (2-replica
# Deployment + PDB, no leader — replicas collapse on the write-ahead ON CONFLICT;
# SIGTERM handled explicitly [PID 1] so pods terminate in ms; rollout guarded by
# maxUnavailable:0 + minReadySeconds since there is no readiness endpoint; projects
# file from the wamn-dispatch-projects Secret — example values in the SEPARATE
# deploy/dispatcher-projects.example.yaml [re-apply must not clobber real config]
# pointing at the additive wamn_dispatch_demo schema; mTLS NATS via
# wasmcloud-runtime-tls [publish-only identity = tracked follow-up]; real
# per-project entries land with hosting/2.3 provisioning). Cron anchor recovery is
# served by the ADDITIVE partial index runs_cron_anchor in deploy/run-state.sql
# (drift-guarded by wamn-run-store; live-applied by the wamn-run-queue gate).
# In-cluster gate of record (co-located with postgres,
# NO cpu limit — S2 CFS lesson; nats via the operator chart's mTLS cert mount). A
# HOST change => full docker rebuild (both --target stages + kind load BOTH images):
kubectl -n wamn-system apply -f deploy/dispatchbench-job.yaml
kubectl -n wamn-system logs -f job/dispatchbench

# [D6/wamn-q3n.1] control-plane registry model crate (crates/wamn-registry) —
# the canonical (org, project, env) identity TRIPLE + the system-DB registry
# DATA MODEL for the four-tier topology (docs/postgres-topology.md, epic
# wamn-q3n). PURE model (SR6 rule 1: no DB/clock/wasm; deps serde+serde_json):
# Registry{orgs,projects,project_envs} + Org(tier + prod/dev ClusterRef) +
# Project + ProjectEnv(Triple + db-secret SecretRef [a REFERENCE, never a
# credential — R8b]); Triple{org,project,env} is the first-class control-plane
# identity (host_label() derives <project>--<env>.<org> routing so tooling never
# parses names); Env is a closed enum {dev,canary,prod} whose side() maps
# canary/prod -> the prod cluster and dev -> the dev cluster (the T2 recovery-
# domain split); Tier {trials,standard,dedicated}. validate()->Vec<Issue>
# (lowercase-slug + reserved wamn prefix [66x] on org/project ids, uniqueness,
# referential integrity, schema-version compat) + Registry::resolve(&Triple)->
# Resolution{tier,cluster,secret}. validate()-only + serde from_json/to_json (a
# store model, not a published contract — the wamn-run-store precedent; NO
# JSON-schema). Load-bearing validation + routing mutation-tested (reserved
# prefix / Env::side / referential integrity). SCOPE: .1 is the MODEL; live
# system-DB tables + the four testable invariants = wamn-q3n.3; the 3.4
# wamn-schema Environment amendment (triple + canary) = wamn-q3n.5; the T1
# cluster infra = wamn-q3n.2. docs/registry-model.md.
cargo test -p wamn-registry
cargo clippy -p wamn-registry --all-targets && cargo fmt -p wamn-registry --check

# [D6/wamn-q3n.2] T1 system cluster — the control-plane CloudNativePG Cluster
# (deploy/wamn-sysdb.yaml). HA day-one (3 instances), a DISTINCT plane from the
# T3 trials pool (deploy/cnpg-cluster.yaml wamn-pg) + the legacy S2–S6 gate pod
# (deploy/postgres.yaml) — two clusters ALWAYS, ADDITIVE (shared-cluster
# guardrail: NEVER touch wamn-pg or postgres.yaml; teardown deletes ONLY
# wamn-sysdb). Exactly one T1 per platform env (this manifest = the kind/dev
# instance); Helm/IaC-provisioned in E1 (it cannot be provisioned by the
# provisioner it backs). HOLDS the registry (wamn-q3n.1 model -> .3 tables),
# saga state, platform RBAC, quota/billing, platform audit; NO tenant data, NO
# credentials (R8b — Secret REFERENCES only). Bootstraps an EMPTY wamn_system DB
# + owner role (NOSUPERUSER LOGIN); .3 applies the registry DDL into it. INFRA
# bead — NO cargo gate; verification is the live standup below (the gate of
# record). enableSuperuserAccess (platform admin path, not a tenant credential);
# NO cpu limit (S2 CFS lesson); non-TLS pg_hba (repo connects NoTls); preferred
# pod anti-affinity (kind control-plane node is tainted, so 3 instances pack the
# 2 schedulable workers — a prod T1 with >=3 schedulable nodes spreads one per
# node); async streaming replication. docs/system-cluster.md.
# Stand it up (needs the kind 'wamn' cluster + CNPG operator 1.29.2 =
# deploy/cnpg-operator.yaml; ALONGSIDE wamn-pg, no host docker rebuild):
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
# wamn-pg + postgres.yaml stay 1/1 healthy throughout (guardrail). Teardown:
# kubectl -n wamn-system delete -f deploy/wamn-sysdb.yaml   # deletes ONLY wamn-sysdb

# [D6/wamn-q3n.3] system-DB registry schema + the four invariants
# (deploy/system-schema.sql) — the wamn-q3n.1 wamn-registry MODEL as TABLES in the
# T1 wamn_system DB (the way deploy/catalog-schema.sql followed wamn-catalog).
# STANDALONE (NOT in postgres-init.sql). PLATFORM-GLOBAL, NOT tenant-scoped: NO
# app.tenant claim, NO RLS floor, NO NULLIF/CHECK(tenant_id<>'') — the top key is
# org_id; APPLIED AS + owned by + used by the wamn_system owner (a superuser
# driving the apply SET ROLEs to it; the 8.1 RBAC role is a GRANT seam). Two
# schemas: registry (meta singleton schema_version + orgs[id,tier,prod_cluster,
# dev_cluster] + projects[org,id] + project_envs[org,project,env,secret_name,
# secret_namespace]) mirroring the model incl the tier/env CHECK literals
# (Tier/Env::as_str), and provisioning (sagas: MINIMAL exactly-once/resumable —
# saga_id PK = exactly-once create, step = durable resume checkpoint, target
# decoupled text; the compensation ledger + RBAC/quota/billing/audit are separate
# subsystems, their own beads = the Q1 scope call). THE FOUR INVARIANTS encoded +
# tested (crates/wamn-registry/tests/storage.rs): (1) request-path-free = a static
# grep asserting NO data-plane manifest references wamn-sysdb/wamn_system (only
# wamn-sysdb.yaml may — an allowlist); (2) no-credentials/R8b = project_envs holds
# a Secret REFERENCE, no credential column (drift-guard + live column-set); (3)
# no-tenant-data = the live table set is exactly registry+provisioning; (4)
# dev!=prod = orgs CHECK (tier='trials' OR prod_cluster<>dev_cluster), a rejected
# bad-standard-org proves it (mirrors the .1 Env::side/resolve). NO new crate (.1
# is the model, .3 is its storage); wamn-registry gained Tier::ALL/as_str (mirrors
# Env) for the drift-guard. Load-bearing asserts MUTATION-TESTED (drop dev!=prod
# CHECK / add credential column / add tenant table / break a drift-guard column —
# all killed). docs/registry-model.md §Storage schema + docs/system-cluster.md.
cargo test -p wamn-registry   # drift-guard + inv-1 grep + as_str coherence (live-apply skips)
cargo clippy -p wamn-registry --all-targets && cargo fmt -p wamn-registry --check
# optional throwaway-PG live-apply gate (WAMN_REGISTRY_PG_URL, superuser url — the
# harness provisions the wamn_system owner + SET ROLEs to it; asserts invariants
# 2/3/4 + FK integrity + saga exactly-once; skips when unset):
docker run -d --rm --name wamn-reg-pg -p 5461:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5461/wamn cargo test -p wamn-registry
docker stop wamn-reg-pg
# IN-CLUSTER gate of record — apply system-schema.sql INTO wamn-sysdb's (wamn-q3n.2)
# empty wamn_system DB AS the wamn_system owner (writing the NEW T1 cluster's own DB
# IS .3's job; NEVER touch wamn-pg or postgres.yaml). Leaves the registry applied +
# EMPTY, wamn_system-owned, ready for provisioning:
{ echo "DROP SCHEMA IF EXISTS registry, provisioning CASCADE; SET ROLE wamn_system;"; \
  cat deploy/system-schema.sql; } | kubectl -n wamn-system exec -i wamn-sysdb-1 \
  -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 -f -
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT schemaname||'.'||tablename FROM pg_tables \
        WHERE schemaname IN ('registry','provisioning') ORDER BY 1;"  # 5 control-plane tables

# [D6/wamn-q3n.6] provision-org — render the T2 org Cluster PAIR (prod/dev) +
# register the org (crates/wamn-provision org.rs renderer + crates/wamn-registry
# cluster_name/sql.rs + crates/wamn-host provision-org subcommand). The four-tier
# split of provision-project: a PAYING org (T2 standard / T4 dedicated) is placed
# on a CNPG Cluster PAIR — <org>-prod (HA per tier: standard 2 / dedicated 3
# instances, pod anti-affinity spread, holds prod+canary) + <org>-dev (ONE
# hibernation-managed instance: the cnpg.io/hibernation annotation set 'off' so it
# comes up ready, the off-hours scheduler flips it 'on'; holds dev/preview, its
# own recovery domain). NEW crates/wamn-provision/src/org.rs = the PURE renderer
# render_org_cluster_pair(&Org)->(prod CR, dev CR) as serde_json::Value (the
# render_secret_manifest precedent; both carry enableSuperuserAccess + non-TLS
# pg_hba + NO cpu limit [S2 CFS lesson] + NO backup stanza [deferred wamn-e1g] +
# a neutral app/app initdb; a trials tier has no pair -> ProvisionError::
# TierHasNoDedicatedPair) + prod_instances(tier). Cluster NAMES come from the SR2
# single-source wamn_registry::cluster_name(org,side)=<org>-prod/<org>-dev +
# Org::for_pair, so the rendered clusters and the registry row name the SAME
# clusters (what resolve() relies on). NEW crates/wamn-registry/src/sql.rs
# upsert_org_sql() = registry.orgs INSERT ... ON CONFLICT (id) DO UPDATE ($n
# params, applied AS wamn_system; SR2 registry-SQL-with-the-model), drift-guarded
# vs system-schema.sql columns + a live idempotent-upsert proof spliced into the
# .3 storage gate. NEW crates/wamn-host provision-org subcommand (imperative
# shell, the provision-project precedent): validate the one-org registry, render,
# emit the two CRs (--emit-prod/--emit-dev, - = stdout), and idempotently record
# the org row via --system-database-url/WAMN_SYSTEM_ADMIN_URL (SET ROLE
# wamn_system). RENDERER + DB WRITER only — the runbook/Job kubectl-applies the
# CRs (no K8s client, the Secret precedent). SCOPE: cluster shape + registry row
# ONLY — per-project-env DB/role (the CNPG Database CRD + managed.roles, RECORDED
# as the .7 mechanism) = wamn-q3n.7; backup = wamn-e1g; provisionbench org
# extension = wamn-q3n.8; T3 = wamn-q3n.9. Mutants killed (apply/test/restore,
# debug builds): tier->instances swap / dev-missing-hibernation / prod-missing-HA
# affinity / org-row ON CONFLICT dropped (live gate) / cluster_name side swap —
# each fails a NAMED test. docs/provisioning.md §provision-org +
# docs/postgres-topology.md §Provisioning rework.
cargo test -p wamn-registry -p wamn-provision -p wamn-host   # renderer shape + org-row SQL + drift/subcommand units
cargo clippy -p wamn-registry -p wamn-provision -p wamn-host --all-targets \
  && cargo fmt -p wamn-registry -p wamn-provision -p wamn-host --check
# The wamn-registry live-apply gate (WAMN_REGISTRY_PG_URL, the .3 block above) now
# also runs upsert_org_sql twice = the idempotent-upsert proof (kills the ON
# CONFLICT mutant). Render CRs locally (no cluster/DB needed):
./target/debug/wamn-host provision-org --org demo --tier standard \
  --emit-prod /tmp/demo-prod.json --emit-dev /tmp/demo-dev.json
# IN-CLUSTER live standup = the gate of record (the wamn-q3n.2 infra precedent;
# needs kind 'wamn' + CNPG 1.29.2 + wamn-sysdb from .2 with the .3 registry
# applied). NO docker rebuild — run the real subcommand locally against a
# port-forwarded wamn-sysdb, then kubectl-apply the emitted CRs ADDITIVELY:
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5463:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5463/wamn_system?sslmode=disable" \
  ./target/debug/wamn-host provision-org --org demo --tier standard \
  --emit-prod /tmp/demo-prod.json --emit-dev /tmp/demo-dev.json   # renders + writes registry.orgs
kubectl apply -f /tmp/demo-prod.json -f /tmp/demo-dev.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=2 cluster/demo-prod --timeout=300s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/demo-dev  --timeout=300s
# Verify (gate of record): HA (demo-prod-2 streaming replica + anti-affinity spread
# across nodes), dev hibernation annotation present / prod absent, distinct plane
# (own -rw/-ro/-r Services + Secrets + PVCs), registry.orgs cluster names == live
# cluster names. Guardrail: wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown
# deletes ONLY the new pair + its row:
kubectl -n wamn-system delete cluster demo-prod demo-dev
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='demo';"

# [D6/wamn-q3n.7] provision-project-env — per-project-env database + role +
# privilege step (crates/wamn-provision database.rs renderer + name.rs/sql.rs +
# crates/wamn-registry sql.rs + crates/wamn-host provision-project-env subcommand).
# The four-tier counterpart of provision-project: identity is the (org,project,env)
# Triple; the database lives on the cluster the org's placement selects by the env's
# recovery-domain SIDE — <org>-prod (prod,canary) / <org>-dev (dev) for a paying
# org, or the shared T3 trials pool for a trials org (both refs point at the pool,
# so ONE registry.org(org).cluster(env.side()) path serves T2 AND T3 by
# construction — NOT resolve(), which needs the project-env to already exist). NEW
# crates/wamn-provision/src/database.rs = the PURE renderer render_project_env_
# database(triple,cluster,connlimit?)->serde_json Database CR (spec.name/owner=
# wamn_app [no tenant db superuser-owned]/cluster.name/ensure present/
# databaseReclaimPolicy RETAIN [CR delete never drops tenant data — guardrail]/
# optional connectionLimit = per-project-env noisy-neighbour cap). NEW per-project-
# env naming (name.rs) project_env_database_name/secret_name = wamn-db-<org>--
# <project>--<env>: the ORG is encoded (unlike 2.3 wamn-db-<project>) — the shared
# pool hosts many orgs (identically-named projects would collide on one cluster) +
# every cluster's Database resources share the one K8s namespace; validate_project_
# env length-checks the assembled name <=63 (PG identifier + DNS-1123 label; NEW
# ProvisionError::NameTooLong). sql.rs grant_connect_on_database_sql(db) targets an
# arbitrary db name (grant_connect_sql(project) delegates) = the thin imperative
# CONNECT step the Database CRD does NOT cover. secret.rs render_project_env_secret_
# manifest (triple-labeled Secret, name = the per-project-env db name). ROLE
# MECHANISM (Q2a): IMPERATIVE ensure_app_role_sql (NOSUPERUSER NOCREATEDB
# NOBYPASSRLS) — uniform across T2 org-clusters + the T3 pool, NO cluster-CR change.
# RLS FLOOR (Q3a): at provision time there are NO tables, so .7 gives the RLS-
# ENFORCEABLE SUBSTRATE only (wamn_app NOBYPASSRLS + per-DB CONNECT confinement); the
# per-TABLE FORCE RLS floor is applied at catalog-publish (2.4/2.5). NEW crates/wamn-
# registry/src/sql.rs upsert_project_sql (ON CONFLICT (org,id) DO NOTHING) + upsert_
# project_env_sql (ON CONFLICT (org,project,env) DO UPDATE — refreshes the Secret
# ref) + select_org_clusters_sql (read placement by env.side, not resolve) — SR2
# registry-SQL-with-the-model, drift-guarded vs system-schema.sql + a LIVE idempotent
# project/project_env proof spliced into the .3 storage gate. NEW crates/wamn-host
# provision-project-env subcommand: read org from registry -> pick cluster by
# env.side() -> render Database CR + emit role SQL + privilege SQL + Secret -> record
# registry.projects + project_envs (SET ROLE wamn_system). RENDERER + DB WRITER only —
# the runbook applies the artifacts IN ORDER (role SQL BEFORE the CR [its owner must
# exist]; privilege SQL AFTER the DB is ready). SCOPE: per-project-env DB + role +
# privilege + registry rows + Secret ONLY — provisionbench org/T3 extension =
# wamn-q3n.8; register the pool AS the trials tier = wamn-q3n.9 (.7 ROUTES to it);
# logical dumps = wamn-q3n.10; WAL/PITR = wamn-e1g. Mutants killed (apply/test/
# restore, sha256 byte-verified, debug builds): db-name env-separator dropped / grant
# omits REVOKE PUBLIC / renderer owner wrong / name length-check dropped / reclaim
# retain->delete / project_env upsert DO UPDATE->DO NOTHING (shape guard + LIVE
# secret-refresh proof) — each fails a NAMED test. docs/provisioning.md §provision-
# project-env + docs/postgres-topology.md §Provisioning rework.
cargo test -p wamn-provision -p wamn-registry -p wamn-host   # renderer/naming + project SQL + drift/subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-host --check
# The wamn-registry live-apply gate (WAMN_REGISTRY_PG_URL, the .3 block above) now
# also runs upsert_project_sql/upsert_project_env_sql = the idempotent + secret-
# refresh proof (kills the project_env ON CONFLICT mutant). Render artifacts locally
# (--cluster given => no DB needed):
./target/debug/wamn-host provision-project-env --org demo --project demo --env dev \
  --cluster wamn-pg --emit-database - --emit-role-sql - --emit-privilege-sql - --emit-secret -
# IN-CLUSTER live standup = the gate of record (T3 pool wamn-pg is ALWAYS up; the
# wamn-q3n.2/.6 infra precedent; NO docker rebuild — real subcommand locally + kubectl
# apply the emitted CR ADDITIVELY). Seed a trials org (org registration is .6/.9; .7
# reads it), run the subcommand against a port-forwarded wamn-sysdb, then apply role
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
# Verify (gate of record): db exists owned by wamn_app + connlimit=20; CONNECT
# confined (PUBLIC revoked, wamn_app granted); wamn_app NOBYPASSRLS substrate;
# registry.projects + project_envs rows (secret ref, ns NULL); env->side selects
# <org>-dev for dev / <org>-prod for canary+prod (render-only vs a T2-shaped org).
# Guardrail: wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown deletes ONLY the
# new Database CR + rows, then DROPs the created db (retain leaves it):
kubectl -n wamn-system delete database wamn-db-demo--demo--dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- \
  psql -U postgres -c 'DROP DATABASE IF EXISTS "wamn-db-demo--demo--dev" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='demo';"

# [D6/wamn-q3n.8] provisionbench four-tier extension — org pair (T2) + trials
# pool (T3) + the saga mechanism (crates/wamn-gates provisionbench --mode +
# crates/wamn-registry saga SQL builders + crates/wamn-provision
# create_database_named_sql). The GATE/COVERAGE counterpart of the .6/.7
# production paths. provisionbench gains --mode (legacy [the 2.3 2-project
# regression] / orgpair / t3 / saga / all — the pgbench/queuebench precedent).
# orgpair = a T2-shaped org (Tier::Standard, so <org>-prod != <org>-dev) with TWO
# project-env databases (prod+dev, wamn-db-<org>--<project>--<env>); off-cluster
# the CNPG Database CRD is unavailable so the DBs are created with plain SQL
# through the REAL .7 builders (ensure_app_role_sql + NEW create_database_named_sql
# [a sibling of grant_connect_on_database_sql taking an already-derived db name;
# the 2.3 create_database_sql/drop_database_sql wrappers now DELEGATE to
# create/drop_database_named_sql] + grant_connect_on_database_sql) = honest
# superuser scaffolding, the shape the CRD reconciles to. Asserts per-DATABASE
# routing (distinct markers) + isolation (a sibling's private table invisible =
# 42P01) + least-priv + per-project-env Secret layout, RECORDS
# registry.orgs/projects/project_envs, and lands a provisioning SAGA
# (create->step-per-env->complete). t3 = a Tier::Trials org (both cluster refs
# collapse onto the shared pool) with 1 env, same assertions. saga = a focused
# proof of the saga builders (exactly-once create / durable step / complete /
# fail). all = legacy then (over ONE ephemeral registry schema —
# deploy/system-schema.sql applied via include_str! into a wamn_system-shaped
# registry/provisioning pair on the same PG, dropped at teardown) saga, orgpair,
# t3. NEW crates/wamn-registry/src/sql.rs saga builders (SR2 with the model):
# create_saga_sql (exactly-once via saga_id PK ON CONFLICT DO NOTHING) /
# advance_saga_step_sql (step=step+1, status='running' — the durable resume
# checkpoint) / complete_saga_sql / fail_saga_sql; the orchestrator that drives
# them through the REAL subcommands stays 10.1 (Q2a — .8 proves the saga MECHANISM
# lands, does NOT change .6/.7). status literals drift-guarded vs the
# provisioning.sagas CHECK + a live exactly-once/step/complete/fail proof spliced
# into the .3 storage gate. Mutants killed (apply/test/restore, sha256, DEBUG
# builds): create ON CONFLICT dropped / step-advance neutered / complete literal
# 'completed'->'running' — each fails a NAMED wamn-registry unit test + the live
# proof (the step mutant also fails --mode saga). docs/provisioning.md
# §provisionbench four-tier extension + docs/postgres-topology.md §Provisioning
# rework item 3.
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
# physical cross-CLUSTER isolation of a real pair needs the operator, so a single
# --mode Job cannot show it). NO docker rebuild — the real debug binary locally +
# kubectl apply the emitted CRs ADDITIVELY (needs kind 'wamn' + CNPG 1.29.2 +
# wamn-pg + wamn-sysdb). --cluster given so the subcommands render WITHOUT a
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
# Verify (gate of record): gate8-prod holds ONLY wamn-db-gate8--app--prod, gate8-dev
# ONLY wamn-db-gate8--app--dev (physical cross-CLUSTER isolation) — each owned by
# wamn_app, CONNECT confined (PUBLIC revoked), NOSUPERUSER/NOCREATEDB/NOBYPASSRLS.
# The same Database-CRD path also serves the T3 pool (--cluster wamn-pg). Guardrail:
# wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown deletes ONLY the new pair:
kubectl -n wamn-system delete database wamn-db-gate8--app--prod wamn-db-gate8--app--dev
kubectl -n wamn-system delete cluster gate8-prod gate8-dev

# [D6/wamn-q3n.9] demote the shipped shared cluster to the T3 trials pool —
# register wamn-pg as the `trials` tier + reframe deploy/cnpg-cluster.yaml
# (crates/wamn-registry Org::for_pool + crates/wamn-host provision-org --tier
# trials). The FOURTH P2 provisioning child. The four-tier provisioning surface
# was ASYMMETRIC: .6 provision-org renders T2/T4 cluster PAIRS and REJECTS trials
# (render_org_cluster_pair -> TierHasNoDedicatedPair); .7 provision-project-env
# ROUTES an EXISTING trials org to the pool (registry.org(org).cluster(env.side())
# — a trials org's both refs = the pool) — but NOTHING created the trials org row
# (.7's live standup INSERTED it by hand via psql). .9 makes the shared pool a
# first-class PLACEMENT TARGET. NEW crates/wamn-registry Org::for_pool(id, pool) =
# the for_pair counterpart: a Tier::Trials org with BOTH cluster refs = the pool
# (env.side() collapses onto it; the recovery-domain invariant tier='trials' OR
# prod<>dev admits the prod==dev collapse for trials only). provision-org gains
# `--tier trials` + `--pool <cluster>` (default wamn-pg): the trials branch builds
# the org via for_pool, validates it, renders NO cluster CRs (the pool exists),
# and records ONLY the registry.orgs placement row (the same idempotent
# upsert_org_sql path, wamn_system owner). NO system-schema.sql change — the org
# row IS the registration (the model's stance: ClusterRef = placement, not
# infrastructure; NO registry.pools table). One org_for(tier,id,pool) decision
# separates the T3 record-only path from the T2/T4 render path (unchanged).
# provision-project-env then routes a REGISTERED trials org's DBs onto the pool
# via env.side() WITHOUT a manual --cluster. deploy/cnpg-cluster.yaml reframed as
# the T3 trials pool (header: pre-contract / RLS floor load-bearing / conversion=
# promotion cross-ref .13) + wamn.tier=trials / component=trials-pool LABELS in
# the FILE (doc-of-intent; the live wamn-pg Cluster is NEVER re-applied). Mutants
# killed (apply/test/restore, sha256-verified, DEBUG builds): for_pool wrong refs
# (dev_cluster != pool) / org_for uses for_pair for trials — each fails a NAMED
# test. docs/postgres-topology.md §T3 "Shipped (wamn-q3n.9)" + docs/provisioning.md
# §provision-org T3 trials orgs. SCOPE: register the pool as a placement target
# ONLY — the tier-move (promotion T3->T2) is wamn-q3n.13 (unblocked by .9); logical
# dumps = .10; retiring the legacy postgres.yaml gate pod = wamn-689 (cross-ref).
cargo test -p wamn-registry -p wamn-host   # Org::for_pool refs==pool + org_for trials-vs-pair + subcommand units
cargo clippy -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-registry -p wamn-host --check
# Render/plan a trials org locally (no DB needed — omit --system-database-url):
./target/debug/wamn-host provision-org --org trialco --tier trials --pool wamn-pg
# IN-CLUSTER gate of record = a LIVE T3 trials-org standup (the .6/.7 precedent; T3
# pool wamn-pg + T1 wamn-sysdb are ALWAYS up). NO docker rebuild — the real debug
# subcommand locally against a port-forwarded wamn-sysdb, then kubectl-apply the
# emitted Database CR ADDITIVELY to wamn-pg. PICK A CLEAN unused port for the
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
# Verify (gate of record): registry.orgs t3gate = trials/wamn-pg/wamn-pg; the Database
# CR routed to wamn-pg FROM THE REGISTERED ROW (no --cluster); the db on wamn-pg owned
# by wamn_app + connlimit 15, CONNECT confined (PUBLIC revoked), wamn_app NOSUPERUSER/
# NOCREATEDB/NOBYPASSRLS; registry.projects + project_envs rows. Guardrail:
# wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown deletes ONLY the new trials
# org's Database CR + DB + registry.orgs row (cascades projects + project_envs):
kubectl -n wamn-system delete database wamn-db-t3gate--demo--dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- \
  psql -U postgres -c 'DROP DATABASE IF EXISTS "wamn-db-t3gate--demo--dev" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='t3gate';"

# [D6/wamn-q3n.10] scheduled per-project-env logical dumps (= the 10.3 export
# artifact) — the SECOND backup mechanism (docs/postgres-topology.md §Backup
# architecture): pg_dump -Fd of one project-env database -> object storage; ONE
# artifact serves tenant-scoped restore-to-last-dump AND the 10.3 project export;
# RPO = dump interval, frequency a TIER KNOB. .10 is the DUMP PRODUCER — the
# operator-facing RESTORE runbook + audit-rewind caveat + backupbench = wamn-q3n.11;
# the tier-move cutover that consumes a dump = wamn-q3n.13; whole-cluster WAL/PITR
# (the OTHER mechanism) = wamn-e1g (its shared object store interacts with Q2).
# NEW crates/wamn-provision/src/dump.rs = PURE renderers + builders (SR3 rule 1, no
# clock/DB/K8s client, the render_project_env_database precedent): render_project_env_
# dump_cronjob (batch/v1 CronJob — postgres:18 pg_dump -Fd of wamn-db-<org>--<project>
# --<env> with DATABASE_URL from the project-env credential Secret's `url` key,
# concurrencyPolicy Forbid, the tier schedule) + render_project_env_dump_job (one-shot
# generateName Job = the 10.3 export / .13 pre-move snapshot) + pure builders
# pg_dump_argv (-Fd DIRECTORY FORMAT is LOAD-BEARING: parallel+selective restore, the
# one artifact 10.3 reuses; --no-password) / dump_object_key (dumps/<org>/<project>/
# <env>/<ts> — DERIVABLE so restore [.11] needs no registry read) / dump_schedule(tier)
# (trials daily / standard 6h / dedicated hourly = "frequency is a tier knob") /
# upload_argv / dump_resource_name (bounded <=52 for the CronJob-name limit). OBJECT
# STORE (Q2, GENUINELY OPEN — NO store in the repo, e1g/Barman also needs one): the
# CronJob RENDERS the upload (aws s3 cp --recursive under the object key) but GUARDS it
# on the CLI being present, so no store yet is NOT a runtime failure; the LIVE S3 upload
# is DEFERRED to the shared MinIO (e1g). NEW `wamn-host dump-project-env` subcommand
# (the provision-project-env precedent, +mod lib.rs +Command variant/arm main.rs):
# --emit-cronjob/--emit-job render; --run-now runs pg_dump -Fd against --database-url +
# RECORDS the dump; tier via --tier OR read from the registry (select_org_tier_sql).
# Q3 (user chose RECORD dump metadata): NEW provisioning.dumps table (deploy/system-
# schema.sql, ADDITIVE — (org,project,env,object_key) PK, format CHECK ('directory'),
# byte_size, taken_at, FK->registry.project_envs ON DELETE CASCADE) = control-plane
# METADATA (invariant 3: NO dump BYTES [object storage], invariant 2: NO credentials)
# + wamn_registry::sql::record_dump_sql (SR2 builder — ON CONFLICT DO UPDATE refreshes
# byte_size, drift-guarded vs the DDL + a LIVE idempotent+refresh proof spliced into
# the .3 storage gate) + select_org_tier_sql; the invariant-3 table set + the cascade
# check are updated for dumps. VERIFY substrate-agnostic (Q2): a WAMN_DUMP_PG_URL
# round-trip gate (crates/wamn-provision/tests/dump.rs) proves the artifact RESTORABLE
# (seed -> pg_dump -Fd -> pg_restore into a scratch DB -> seed incl an exact-decimal
# column round-trips). Mutants killed (apply/test/restore, sha256, DEBUG builds):
# pg_dump drops -Fd / object-key wrong shape / tier->schedule swap / CronJob command
# without -Fd / record_dump DO UPDATE->DO NOTHING — each fails a NAMED test.
# docs/provisioning.md §dump-project-env + docs/postgres-topology.md §Backup
# architecture 'Shipped (wamn-q3n.10)'.
cargo test -p wamn-provision -p wamn-registry -p wamn-host   # renderers/builders + record_dump SQL + drift/subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-host --check
# Render locally (no DB — --tier gives the cadence without a registry read):
./target/debug/wamn-host dump-project-env --org demo --project app --env prod \
  --tier standard --emit-cronjob - --emit-job -
# optional live gates (throwaway postgres:18; superuser url): (a) the ARTIFACT
# round-trip (WAMN_DUMP_PG_URL — seeds a db, pg_dump -Fd, pg_restore into a scratch
# db, asserts the seed round-trips; skips if unset / no pg_dump); (b) the record_dump
# idempotent + byte_size-refresh proof rides the wamn-q3n.3 storage gate:
docker run -d --rm --name wamn-dump-pg -p 5462:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DUMP_PG_URL=postgres://postgres:postgres@127.0.0.1:5462/wamn \
  cargo test -p wamn-provision --test dump
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5462/wamn cargo test -p wamn-registry
docker stop wamn-dump-pg
# IN-CLUSTER gate of record (the .6/.7/.9 precedent; T3 pool wamn-pg + T1 wamn-sysdb
# always up; NO docker rebuild — real debug subcommand + kubectl). First apply the
# ADDITIVE provisioning.dumps table into wamn-sysdb's wamn_system DB AS wamn_system
# (writing the T1 registry's OWN DB IS .10's job; NEVER touch wamn-pg/postgres.yaml):
awk '/^CREATE TABLE provisioning\.dumps/{f=1} f{print} f&&/^\);/{exit}' deploy/system-schema.sql \
  | { echo "SET ROLE wamn_system;"; cat; } | kubectl -n wamn-system exec -i wamn-sysdb-1 \
  -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 -f -
# Register a trials org + provision a project-env DB on wamn-pg (the .7/.9 path), seed
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
# Verify (gate of record): the seed round-trips in the scratch DB (3 parts, exact-decimal
# weights intact) + the provisioning.dumps row in wamn-sysdb (fmt=directory, byte_size):
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres -d wamn_dump_scratch_t10 \
  -tAc "SELECT count(*), sum(weight_kg) FROM parts;"
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT object_key, format, byte_size FROM provisioning.dumps WHERE org='t10gate';"
# Guardrail: wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown deletes ONLY the new
# resources (kill the port-forwards by EXACT pid — not pkill); the org delete cascades
# projects+project_envs+dumps:
kubectl -n wamn-system delete database wamn-db-t10gate--demo--dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres \
  -c 'DROP DATABASE IF EXISTS "wamn-db-t10gate--demo--dev" WITH (FORCE);' \
  -c 'DROP DATABASE IF EXISTS wamn_dump_scratch_t10 WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "DELETE FROM registry.orgs WHERE id='t10gate';"

# [D6/wamn-q3n.11] restore per-project-env logical dumps — the RESTORE counterpart
# of the .10 dump producer (docs/postgres-topology.md §Backup architecture, restore
# runbook). NEW crates/wamn-provision/src/restore.rs = PURE builders (the dump.rs
# precedent, no clock/DB/pg_restore invocation): pg_restore_argv(conninfo,dump_dir,
# clean) [--no-owner --no-privileges = restore DATA not the source roles/ACLs; +
# --clean --if-exists IN PLACE ONLY = drop each object before recreating so a
# restore over the live populated db REPLACES not appends] + restore_scratch_db_name
# (wamn-restore-<org>--<project>--<env>, distinct from the live wamn-db-… so a
# scratch restore never shadows it) + validate_restore_scratch_name (the longer
# scratch prefix can overflow 63 where the live name fits). NEW crates/wamn-host/src/
# restore_project_env.rs = the restore-project-env subcommand (the dump-project-env
# precedent; +mod lib.rs +Command variant/arm main.rs): TWO targets, the safe one
# DEFAULT — SCRATCH (non-destructive, into a fresh wamn-restore-… db = the
# sub-cluster carve-out target, left standing for inspection) vs IN PLACE (--in-place
# --confirm, DESTRUCTIVE, pg_restore --clean over the live db = restore-to-last-dump;
# --confirm REQUIRED via the pure in_place_confirmed gate). WHICH dump: explicit
# --dump-dir wins, else the dump CATALOG is read (select_latest_dump_sql, or
# --object-key) so restore-to-last-dump needs NO manual key; the dir is
# --dump-root/<ts> (ts = the object key's last segment = the --run-now --out-dir
# layout). Dump BYTES staged locally until the shared store lands (Q2, e1g); the
# catalog decides WHICH dump. NEW wamn_registry::sql::select_latest_dump_sql (ORDER
# BY taken_at DESC, object_key DESC LIMIT 1) + select_dumps_sql (the window) — the
# dump catalog .10 DEFERRED to .11 (SR2 builders, drift-guarded vs system-schema.sql
# + a LIVE newest-of-three proof spliced into the .3 storage gate). WHOLE-CLUSTER
# PITR (rewind an org cluster to an instant, carve one DB out) needs WAL/PITR = e1g
# (runbook cross-ref, NOT this); audit-rewind caveat = 8.6 (docs). Mutants killed
# (apply/test/restore, sha256, DEBUG builds): pg_restore_argv drops --clean / drops
# --no-owner / select_latest taken_at DESC->ASC / in_place_confirmed->true — each
# fails a NAMED test. docs/provisioning.md §restore-project-env + docs/postgres-
# topology.md §Backup architecture 'Shipped (wamn-q3n.11)' + operator restore runbook.
cargo test -p wamn-provision -p wamn-registry -p wamn-host   # restore builders + select_latest shape/drift + subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-host --check
# Render/plan locally (no cluster/DB needed — explicit --dump-dir, render only):
./target/debug/wamn-host restore-project-env --org demo --project app --env dev \
  --database-url postgres://postgres:postgres@127.0.0.1:5468/postgres \
  --dump-dir /tmp/some-dump --help >/dev/null   # (see the subcommand flags)
# optional live gates (throwaway postgres:18; superuser url): (a) the restore
# ROUND-TRIP (WAMN_RESTORE_PG_URL — seed -> pg_dump -Fd -> pg_restore into a scratch
# db asserts the seed round-trips + in-place --clean REPLACES a stale row; skips if
# unset / no pg_restore); (b) the select_latest newest-of-three proof rides the
# wamn-q3n.3 storage gate:
docker run -d --rm --name wamn-restore-pg -p 5468:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RESTORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5468/wamn \
  cargo test -p wamn-provision --test restore
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5468/wamn cargo test -p wamn-registry
docker stop wamn-restore-pg
# IN-CLUSTER gate of record = a LIVE restore standup on the T3 pool (the .6/.7/.9/.10
# precedent; T3 pool wamn-pg + T1 wamn-sysdb always up; provisioning.dumps applied in
# wamn-sysdb from .10; NO docker rebuild — real debug subcommand + kubectl). PICK
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
# Verify (gate of record): the scratch round-trips (3 parts, 0.183 kg); the catalog
# read selected the right dump; then in-place --confirm over the live DB drops a stale
# row (mutate live -> restore -> stale gone):
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/wamn-restore-t11gate--demo--dev?sslmode=disable" \
  -tAc "SELECT count(*), sum(weight_kg) FROM parts;"
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" -c "INSERT INTO parts VALUES (99,'STALE',9.999);"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-host restore-project-env --org t11gate --project demo --env dev \
  --database-url "$PGADMIN" --dump-root "$DUMPROOT" --in-place --confirm
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" -tAc "SELECT count(*) FROM parts;"  # 3 (stale gone)
# Guardrail: wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown deletes ONLY the new
# resources (kill the port-forwards by EXACT pid — not pkill); the org delete cascades
# projects+project_envs+dumps:
kubectl -n wamn-system delete database $DB
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres \
  -c 'DROP DATABASE IF EXISTS "wamn-db-t11gate--demo--dev" WITH (FORCE);' \
  -c 'DROP DATABASE IF EXISTS "wamn-restore-t11gate--demo--dev" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "DELETE FROM registry.orgs WHERE id='t11gate';"

# [D6/wamn-q3n.13] tier-move / promotion tooling — T3->T2 + T2->T4 (crates/wamn-provision
# tier_move.rs + crates/wamn-registry select_org_project_envs_sql + crates/wamn-host
# move-org-tier subcommand). Promote an org to a higher-isolation tier by RE-POINTING it
# onto the new tier's clusters via the 2.2 CredentialProvider seam (docs/postgres-
# topology.md §Reversibility): per project-env, dump the current DB, provision it on the
# new cluster, restore the dump, flip the registry row. A SCHEDULED operation (dump/restore
# window; a logical-replication cutover = the near-zero-downtime follow-up). COMPOSES the
# built pieces — .6 provision-org, .7 provision-project-env, .10 dump-project-env, .11
# restore-project-env + the existing registry flip SQL (upsert_org_sql). NEW crates/wamn-
# provision/src/tier_move.rs = the PURE core (SR6 rule 1, no DB/clock/K8s): validate_tier_
# upgrade (the lattice trials<standard<dedicated via tier_rank; same-tier=TierMoveNoop,
# downgrade=TierDowngrade — data never moves DOWN to a shared/lower tier) + plan_tier_move
# -> ordered Vec<TierMoveStep> (ProvisionClusters -> per-env {Dump, ProvisionEnv, Restore}
# -> FlipRegistry LAST; each env's cluster picked by env.side() off Org::for_pair(target),
# the single-source cluster_name the CR renderer + flipped row also use). NEW crates/wamn-
# registry/src/sql.rs select_org_project_envs_sql (SR2, enumerate an org's (project,env)
# rows ORDER BY project,env; drift-guarded vs system-schema.sql). NEW ProvisionError::
# TierMoveNoop / TierDowngrade. NEW crates/wamn-host/src/move_org_tier.rs = the orchestrating
# shell (+mod lib.rs +Command variant/arm main.rs): reads the org's current tier+placement+
# project-envs from the T1 registry; PLAN mode (default) prints the ordered runbook (the
# exact provision-org / dump / provision-project-env / restore invocations + kubectl applies
# in dependency order — the registry row STAYS on the OLD tier through the data move,
# provision-project-env targets the new cluster by explicit --cluster, and the Database CR
# name is triple-derived so the OLD CR is deleted [RETAIN keeps its data] before the new one;
# --flip is the LAST atomic cutover); --flip executes the idempotent registry.orgs cutover
# (upsert_org_sql to the new tier+pair; a re-flip = no-op [crash-retry safe], a downgrade
# flip is rejected). RENDERER/DB-writer only — no K8s client, no pg_dump/pg_restore itself
# (those are the reused subcommands' jobs, the provision-org render-not-apply precedent); the
# full resumable/compensating SAGA that would drive the plan is 10.1. One mechanism, BOTH
# directions (T3->T2 proven by a live cross-CLUSTER standup; T2->T4 the SAME code path, its
# dedicated-per-env cluster shape = wamn-q3n.14, which BLOCKS on .13). CNPG initdb.import =
# the documented CNPG-native restore alternative. Mutants killed (apply/test/restore, sha256,
# DEBUG builds): validate strict '>'->'>=' / tier_rank standard<->dedicated swap / plan flip
# pushed FIRST / per-env cluster ignores env.side / select_org_project_envs ORDER BY drop —
# each fails a NAMED test. docs/provisioning.md §move-org-tier + docs/postgres-topology.md
# §Reversibility 'Shipped (wamn-q3n.13)' + tier-move runbook.
cargo test -p wamn-provision -p wamn-registry -p wamn-host   # tier_move validate/plan + project-env list SQL + subcommand units
cargo clippy -p wamn-provision -p wamn-registry -p wamn-host --all-targets \
  && cargo fmt -p wamn-provision -p wamn-registry -p wamn-host --check
# Plan/flip locally (no cluster — a throwaway PG with the .3 registry applied + a seeded org):
#   WAMN_SYSTEM_ADMIN_URL=<sysdb superuser> ./target/debug/wamn-host move-org-tier \
#     --org <id> --target-tier standard          # PLAN mode: prints the ordered runbook
#   WAMN_SYSTEM_ADMIN_URL=<sysdb superuser> ./target/debug/wamn-host move-org-tier \
#     --org <id> --target-tier standard --flip    # CUTOVER: flip registry.orgs (idempotent)
# IN-CLUSTER gate of record = a LIVE T3->T2 tier move across REAL clusters (the .6/.7/.9/.10/
# .11 precedent; T3 pool wamn-pg + T1 wamn-sysdb always up; provisioning.dumps applied from
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
# Move prod onto the new cluster: delete the OLD Database CR (RETAIN keeps wamn-pg's copy) so the
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
# Verify (gate of record): the moved data lives on the NEW cluster t13gate-prod; the registry
# now points there (resolve prod -> t13gate-prod), tier flipped standard:
kubectl -n wamn-system exec t13gate-prod-1 -c postgres -- psql -U postgres -d "wamn-db-t13gate--app--prod" \
  -tAc "SELECT count(*), sum(weight_kg) FROM parts;"   # 3 | 0.183 (exact decimals survived the cross-cluster move)
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT id,tier,prod_cluster,dev_cluster FROM registry.orgs WHERE id='t13gate';"  # t13gate|standard|t13gate-prod|t13gate-dev
# Guardrail: wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown deletes ONLY the new pair +
# DBs + the org row (kill the port-forwards by EXACT pid — not pkill); the org delete cascades
# projects+project_envs+dumps:
kubectl -n wamn-system delete database wamn-db-t13gate--app--prod --ignore-not-found
kubectl -n wamn-system delete cluster t13gate-prod t13gate-dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- \
  psql -U postgres -c 'DROP DATABASE IF EXISTS "wamn-db-t13gate--app--prod" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='t13gate';"

# [D6/wamn-q3n.14] T4 dedicated-per-env regulated tier — canary gets its OWN
# cluster (crates/wamn-registry canary_cluster_name/Org.canary_cluster/cluster_for_env
# + deploy/system-schema.sql canary_cluster column + crates/wamn-provision org.rs
# render_org_cluster_set + crates/wamn-host provision-org --emit-canary). The §T4
# maximal-separation property: a dedicated org places `canary` on <org>-canary (a
# THIRD recovery domain with independent PITR) — the T2 Env::side collapse
# (canary->prod) cannot express it, so the model gains a STORED
# registry.orgs.canary_cluster (set IFF tier=dedicated) and per-env resolution moves
# from Env::side to Org::cluster_for_env. NEW wamn_registry::canary_cluster_name(org)
# = <org>-canary (sibling of cluster_name) + Org.canary_cluster:Option<ClusterRef>
# (serde-omitted when None so T2/T3 rows round-trip) set by for_pair for Dedicated;
# NEW Org::cluster_for_env(env) (dev->dev, prod->prod, canary->canary_cluster.
# unwrap_or(prod)) REPLACES the removed binary cluster(side) footgun; resolve() +
# tier_move plan_tier_move + provision-project-env do_resolve_cluster all route via it
# (Env::side stays the T2-pair concept, still tested). upsert_org_sql/
# select_org_clusters_sql gain canary_cluster ($4 / read). deploy/system-schema.sql
# registry.orgs += canary_cluster text + TWO CHECKs: biconditional
# orgs_canary_dedicated_check ((tier='dedicated')=(canary_cluster IS NOT NULL)) +
# distinctness orgs_canary_recovery_domain_check (canary NULL OR canary<>prod AND
# canary<>dev), both drift-guarded (expression-pinned in tests/storage.rs) +
# live-apply-proven (a dedicated-NULL-canary / standard-with-canary / canary=prod org
# each REJECTED, a distinct-canary dedicated org ACCEPTED). NEW
# wamn_provision::org::render_org_cluster_set -> OrgClusters{prod, canary:Option, dev}
# (render_cluster keys HA off instances>=2 not Side==Prod + takes a role label;
# canary HA-2) REPLACES render_org_cluster_pair; provision-org emits the 3rd CR via
# --emit-canary (dedicated only); tier_move TierMoveStep::{ProvisionClusters,
# FlipRegistry} gain canary_cluster + per-env route via cluster_for_env (a T2->T4
# canary env routes to <org>-canary, NOT prod). Mutants killed (apply/test/restore,
# sha256, DEBUG builds): cluster_for_env Canary->prod / for_pair canary->None /
# render-set canary->None / drop the distinctness CHECK / plan per-env cluster->prod —
# each fails a NAMED test. docs/postgres-topology.md §T4 'Shipped (wamn-q3n.14)' +
# docs/provisioning.md §provision-org T4 dedicated orgs.
cargo test -p wamn-registry -p wamn-provision -p wamn-host   # model/DDL drift + renderer + routing + subcommand units
cargo clippy -p wamn-registry -p wamn-provision -p wamn-host -p wamn-gates --all-targets \
  && cargo fmt -p wamn-registry -p wamn-provision -p wamn-host -p wamn-gates --check
# Render a dedicated org's 3 CRs locally (no cluster/DB needed):
./target/debug/wamn-host provision-org --org demo --tier dedicated \
  --emit-prod /tmp/demo-prod.json --emit-canary /tmp/demo-canary.json --emit-dev /tmp/demo-dev.json
# optional live gates (throwaway postgres:18; superuser url): (a) the storage
# live-apply gate proves the canary CHECKs reject bad orgs + the upsert refreshes
# canary on conflict; (b) provisionbench --mode all is the .8 regression (the 5-param
# upsert; standard/trials orgs carry canary NULL):
docker run -d --rm --name wamn-reg-pg -p 5461:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5461/wamn cargo test -p wamn-registry
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5461/wamn \
  ./target/debug/wamn-gates --log-level error provisionbench --mode all
docker stop wamn-reg-pg
# IN-CLUSTER gate of record = a LIVE dedicated-org standup (the .6/.8/.13 precedent;
# T3 pool wamn-pg + T1 wamn-sysdb always up; NO docker rebuild — real debug subcommand
# + kubectl; ~6 pods: prod HA-3 + canary HA-2 + dev-1, generous waits). FIRST apply the
# ADDITIVE canary_cluster column + CHECKs into wamn-sysdb's registry AS wamn_system
# (extending the T1 registry's OWN DB IS .14's job — the .3/.10 precedent; NEVER touch
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
# Provision canary + prod project-env DBs WITHOUT --cluster (routing derives per-env from
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
# Verify (gate of record): canary routed to its OWN cluster (Database CR cluster
# d14gate-canary != d14gate-prod); each cluster holds ONLY its env's DB (physical
# cross-CLUSTER isolation), owned by wamn_app, CONNECT confined (PUBLIC revoked), wamn_app
# NOSUPERUSER/NOCREATEDB/NOBYPASSRLS; registry.orgs d14gate = dedicated/d14gate-prod/
# d14gate-canary/d14gate-dev + project_envs canary+prod rows:
for CL in d14gate-canary d14gate-prod; do kubectl -n wamn-system exec ${CL}-1 -c postgres -- \
  psql -U postgres -tAc "SELECT datname FROM pg_database WHERE datname LIKE 'wamn-db-d14gate%';"; done
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT id,tier,prod_cluster,canary_cluster,dev_cluster FROM registry.orgs WHERE id='d14gate';"
# Guardrail: wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown deletes ONLY the new
# clusters + Database CRs + the org row (kill the port-forward by EXACT pid — not pkill);
# the org delete cascades projects+project_envs (the additive canary_cluster column STAYS
# — it is the shipped schema):
kubectl -n wamn-system delete database wamn-db-d14gate--app--canary wamn-db-d14gate--app--prod --ignore-not-found
kubectl -n wamn-system delete cluster d14gate-prod d14gate-canary d14gate-dev
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='d14gate';"

# [D6/wamn-e1g] per-org WAL/PITR via the Barman Cloud plugin + the shared object
# store (crates/wamn-provision/src/backup.rs renderer + org.rs wiring +
# crates/wamn-host provision-org --emit-object-store/--emit-scheduled-backup +
# deploy/minio.yaml + deploy/barman-cloud-plugin.yaml + the .10 dump upload made
# LIVE). The FIRST backup mechanism (docs/postgres-topology.md §Backup
# architecture): continuous WAL archiving + base backups to object storage for
# WHOLE-CLUSTER point-in-time recovery. Build on the CloudNativePG Barman Cloud
# PLUGIN (barman-cloud.cloudnative-pg.io; the in-tree .spec.backup.barmanObjectStore
# provider is deprecated CNPG 1.26, removal slated 1.31) — a SEPARATE install
# (deploy/barman-cloud-plugin.yaml, VENDORED+pinned v0.13.0, into cnpg-system) that
# REQUIRES cert-manager (plugin<->operator mTLS); the shared object store =
# deploy/minio.yaml (MinIO; buckets wamn-backups [WAL] + wamn-dumps [logical dumps]
# + Secret wamn-object-store). NEW crates/wamn-provision/src/backup.rs = PURE
# renderers + tier knobs (the org.rs/dump.rs precedent, no K8s client):
# render_object_store (ObjectStore CR barmancloud.cnpg.io/v1: per-cluster WAL prefix
# s3://wamn-backups/wal/<cluster> [each recovery domain isolated], endpointURL=MinIO,
# s3Credentials->wamn-object-store, spec.retentionPolicy = wal_retention(tier) = the
# PITR-SLA KNOB trials 7d / standard 14d / dedicated 30d), cluster_backup_plugin
# (.spec.plugins WAL-archiver ref isWALArchiver:true barmanObjectName), and
# render_scheduled_backup (base backup via method:plugin at base_backup_schedule(tier)
# daily/12h/6h, immediate:true), gated by backup_enabled_for_role (prod|canary true,
# dev off = "T2-dev optional", its restore path the logical dump). org.rs
# render_org_cluster_set routes each role through the predicate; OrgClusters gains
# object_stores + scheduled_backups; render_cluster attaches the plugin ref (uses
# .spec.plugins NOT the deprecated in-tree .spec.backup). provision-org emits
# --emit-object-store (a List, apply BEFORE the cluster — the plugin references it) /
# --emit-scheduled-backup (AFTER the cluster exists). dump.rs completes the .10 upload
# LIVE: the dump pod is initContainer(postgres:18 pg_dump -Fd into a shared volume) +
# container(minio/mc `mc mirror` the dump dir CONTENTS to s3://wamn-dumps/<derivable
# key> so .11 restore finds toc.dat), guarded on the S3 endpoint env. Mutants killed
# (apply/test/restore, sha256, DEBUG builds): wal_retention swap / backup_enabled
# dev->true / destinationPath drops the per-cluster prefix / scheduled_backup method
# plugin->barmanObjectStore / base_backup_schedule swap / dump upload mc mirror->ls —
# each fails a NAMED test. docs/postgres-topology.md §Backup architecture 'Shipped
# (wamn-e1g)' + docs/provisioning.md §provision-org WAL/PITR.
cargo test -p wamn-provision -p wamn-host   # backup renderer + tier knobs + org/dump wiring + subcommand units
cargo clippy -p wamn-provision -p wamn-host -p wamn-registry -p wamn-gates --all-targets \
  && cargo fmt -p wamn-provision -p wamn-host -p wamn-registry -p wamn-gates --check
# Render a standard org's backup CRs locally (no cluster/DB needed):
./target/debug/wamn-host provision-org --org demo --tier standard \
  --emit-prod /tmp/demo-prod.json --emit-dev /tmp/demo-dev.json \
  --emit-object-store /tmp/demo-os.json --emit-scheduled-backup /tmp/demo-sb.json
# IN-CLUSTER gate of record = a LIVE WAL/PITR standup (the .6/.14 precedent; T3 pool
# wamn-pg + T1 wamn-sysdb always up; NO docker rebuild — real debug subcommand +
# kubectl). FIRST install the backup infra ADDITIVELY (cert-manager is a HARD
# prerequisite for the plugin; all three STAY as platform substrate — the operator
# precedent — the shared-cluster guardrail forbids re-applying wamn-pg/wamn-sysdb):
kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.21.0/cert-manager.yaml
kubectl -n cert-manager wait --for=condition=Available deploy --all --timeout=180s
kubectl apply -f deploy/barman-cloud-plugin.yaml
kubectl -n cnpg-system rollout status deploy/barman-cloud --timeout=180s
kubectl apply -f deploy/minio.yaml
kubectl -n wamn-system rollout status deploy/minio --timeout=150s
kubectl -n wamn-system wait --for=condition=complete job/minio-init --timeout=120s
# Provision a standard org WITH backup (render-only — the PITR proof is the cluster +
# backup CRs, not the registry row), apply ObjectStore -> Clusters -> ScheduledBackup:
env -u WAMN_SYSTEM_ADMIN_URL ./target/debug/wamn-host provision-org --org e1gate --tier standard \
  --emit-prod /tmp/e1-prod.json --emit-dev /tmp/e1-dev.json \
  --emit-object-store /tmp/e1-os.json --emit-scheduled-backup /tmp/e1-sb.json
kubectl apply -f /tmp/e1-os.json                             # ObjectStore BEFORE the cluster
kubectl apply -f /tmp/e1-prod.json -f /tmp/e1-dev.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=2 cluster/e1gate-prod --timeout=300s
kubectl apply -f /tmp/e1-sb.json                             # ScheduledBackup AFTER (immediate base backup)
# Verify (gate of record): cluster condition ContinuousArchiving=True + a plugin base
# backup completes to MinIO (kubectl get backup -l cnpg.io/cluster=e1gate-prod ->
# phase completed). Then PROVE PITR to a precise instant: write row1; capture
# T1=SELECT now() AFTER row1 commits AND its WAL is archived (poll
# pg_stat_archiver.last_archived_wal); write row2; force WAL past T1 (pg_switch_wal
# until pg_stat_archiver.last_archived_time > T1 — else recovery FATALs "recovery
# ended before target reached"); then bootstrap a recovery Cluster with
# bootstrap.recovery.recoveryTarget.targetTime=T1 + externalClusters[].plugin
# {barmanObjectName:e1gate-prod-store, serverName:e1gate-prod} and assert it recovered
# EXACTLY the pre-target state (row1 present, row2 excluded). The .10 dump upload is
# proven live by a one-shot dump Job (dump-project-env --org e1gate --project app --env
# prod --tier standard --emit-job) whose init pg_dump + mc-mirror lands toc.dat under
# the derivable key in s3://wamn-dumps/dumps/e1gate/app/prod/<ts> (a project-env table
# must be wamn_app-owned so the app credential can pg_dump it). Teardown deletes ONLY
# the test org's clusters + ObjectStore + ScheduledBackup + dump Jobs + MinIO objects
# (mc rm wal/e1gate-prod + dumps/e1gate); the backup infra (cert-manager / barman
# plugin / MinIO) STAYS; wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. wamn-sysdb (T1) +
# wamn-pg (T3) get WAL/PITR by the same renderer at next (re)provision (the guardrail
# forbids re-applying the running clusters here):
kubectl -n wamn-system delete cluster e1gate-restore e1gate-prod e1gate-dev
kubectl -n wamn-system delete objectstore e1gate-prod-store
kubectl -n wamn-system delete scheduledbackup e1gate-prod-backup

# [2.4] per-project system schema v1 (crates/wamn-sysschema + deploy/app-schema.sql)
# — the auth/RBAC half of item 2.4: a per-project TENANT-SCOPED schema (schema
# `app_system`) under the 3.2 RLS floor + a45 hardening (tenant_id NOT NULL CHECK
# <> '' + FORCE RLS + NULLIF(current_setting('app.tenant',true),'') policies +
# wamn_app grants — the catalog-schema.sql shape). SEVEN tables: users (id uuid =
# the 3.5 app.user_id ownership target; status IN active/disabled/invited; NO
# credential material), roles (name = the app.role gate target), user_roles
# (user<->role linkage), permissions (role->permission string, 4.3 reads),
# configurations (config_key->jsonb config_value), audit_log (actor_id BARE uuid
# NOT FK'd — immutable history survives user deletion; indexed (tenant_id,
# occurred_at)), api_keys (key_hash = one-way digest, raw key NEVER stored; FK
# users ON DELETE CASCADE). The METADATA half (entities/fields/relations/flows) is
# ALREADY shipped — catalog.* (deploy/catalog-schema.sql, 3.1) + wamn_run.flows
# (deploy/flows.sql, POC-F1) — REFERENCED not redefined; a `deployments` table is
# DEFERRED (a live WorkloadDeployment is a K8s CR). DISTINCT from the T1 registry
# (deploy/system-schema.sql — PLATFORM-GLOBAL, wamn_system-owned, no RLS floor);
# hence a different file, NOT named system-schema.sql. NEW crates/wamn-sysschema =
# the PURE model (SCHEMA_NAME=app_system + TABLES manifest + UserStatus CHECK
# literals + claim GUC names; ZERO deps) drift-guarded vs deploy/app-schema.sql.
# NO password hashing / JWT / session mgmt (that is 4.2/8.1) — the SUBSTRATE only;
# claims are injected by the plugin from a resolved session. STANDALONE DDL (NOT
# in postgres-init.sql). Mutants killed (python apply/test/restore, sha256,
# DEBUG): status literal / model literal / an FK cascade / the tenant policy
# predicate / an added audit FK — each fails a NAMED test. docs/app-schema.md.
cargo test -p wamn-sysschema     # unit (status literals + table manifest) + drift-guard
cargo clippy -p wamn-sysschema --all-targets && cargo fmt -p wamn-sysschema --check
# optional live-apply gate (throwaway postgres:18; superuser url provisions
# wamn_app; applies app-schema.sql, asserts tenant RLS isolation across two
# tenants + empty-claim fail-closed + FK cascade + audit-log immutability +
# status/''-tenant CHECKs + users.id is uuid + a REAL compiled 3.5 policy filters
# a data table by app.user_id [= a users.id] / app.role [= a roles.name]; skips
# when unset):
docker run -d --rm --name wamn-as5-pg -p 5466:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_SYSSCHEMA_PG_URL=postgres://postgres:postgres@127.0.0.1:5466/wamn cargo test -p wamn-sysschema
docker stop wamn-as5-pg

# [2.5] migration engine (crates/wamn-migrate + wamn-host migrate-catalog) — the
# LIVE, versioned, forward-only executor that wraps the shipped machinery: it
# reads the current applied catalog, computes the plan with 3.2 wamn-ddl
# (Migration::create/migrate + the Confirmation gate REUSED VERBATIM — a
# destructive plan is refused without --confirm-with-backup, the emitted DDL then
# carries the "-- BACKUP CHECKPOINT REQUIRED" marker), validates the transition
# against 3.4 wamn-schema (Environment::apply as the single-applied + stale-base
# ORACLE), and produces a ONE-TRANSACTION ApplyPlan [DDL + demote-prior-applied +
# promote-target-with-document + immutable schema_migrations history] + a --dry-run
# report + a generated inverse RollbackPlan (migrate(target->current) + a
# restore-to-last-dump [wamn-q3n.11] pointer). PURE crate (guards + $n SQL builders
# + plan composition; NO DB/clock — the wamn-ddl/wamn-schema SR6 pure/effect
# precedent); the thin `wamn-host migrate-catalog` subcommand is the effect shell
# (superuser connect, read current [FOR UPDATE], execute the plan in one txn).
# STORAGE (ADDITIVE to the STANDALONE deploy/catalog-schema.sql, NOT
# postgres-init.sql): catalog.catalogs gains a `document jsonb` column (the applied
# Catalog JSON = the diff source; 2.5 is its FIRST live writer) + a NEW
# catalog.schema_migrations table (the immutable forward-only apply journal:
# (from->to) version step, destructive flag, confirmation, op count, DDL checksum;
# 3.2 floor + a45 hardening; wamn_app SELECT+INSERT only = append-only; PK
# (tenant,catalog,env,to_version) forbids re-recording a version). The R9c one-txn
# invariant keeps the wamn-ddl name-freeing preamble's zero-residue guarantee
# (CREATE INDEX CONCURRENTLY = the deferred breaker: v1 emits no non-txn step, so
# the residue-janitor + apply-journal are a follow-up). SCOPE: the TENANT catalog
# engine (unblocks POC-DM1); the platform-release system-schema runner +
# catalog-content shredding (3.3/11.8) = follow-up beads. Mutants killed
# (scratchpad/mutate_d8u.py apply/test/restore, sha256, DEBUG): forward-only guard
# / stale-base validation / the backup gate / the demote state literal / the
# history statement — each fails a NAMED test. docs/migration-engine.md. No
# JSON-schema (an engine, not a contract).
cargo test -p wamn-migrate     # unit (guards/gate/dry-run/rollback) + drift-guard + live-apply
cargo test -p wamn-host --lib migrate_catalog   # the subcommand's bare-ident + param-map units
cargo clippy -p wamn-migrate -p wamn-host --all-targets \
  && cargo fmt -p wamn-migrate -p wamn-host --check
# optional live-apply gate (throwaway postgres:18; superuser url — provisions
# wamn_app, applies catalog-schema.sql, then a REAL engine plan applies a first
# materialization + a forward additive migration [document round-trip,
# single-applied advance, history] + a gated destructive migration; skips when
# unset):
docker run -d --rm --name wamn-migrate-pg -p 5467:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_MIGRATE_PG_URL=postgres://postgres:postgres@127.0.0.1:5467/wamn cargo test -p wamn-migrate
docker stop wamn-migrate-pg
# The production tool is `wamn-host migrate-catalog --admin-database-url <superuser>
# --tenant <t> --environment dev|canary|prod --schema <data schema> --target
# <catalog.json> [--base <n>] [--dry-run] [--confirm-with-backup]` (reads current
# applied, plans, applies in one txn; --dry-run touches nothing).

# [3.1] metadata catalog schema crate (crates/wamn-catalog) — canonical model
# JSON: entity/field/relation/index/constraint types + is_system, validation,
# import/export, version diff. Field type system incl. exact-decimal
# numeric(precision,scale)+unit (NO float), enum, reference; system entities are
# structure-locked but extensible. Pure Rust, no host/DB. Tests: POC-model +
# genealogy fixtures round-trip/validate/JSON-Schema-conform (boon)/drift-guard/
# diff. docs/catalog-model.md + docs/catalog-model.schema.json; catalog table
# DDL deploy/catalog-schema.sql (standalone; not wired into postgres-init.sql).
cargo test -p wamn-catalog
cargo clippy -p wamn-catalog --all-targets && cargo fmt -p wamn-catalog --check
# regenerate the published JSON Schema contract after changing the types:
cargo run -p wamn-catalog --example print-schema > docs/catalog-model.schema.json

# [3.2] DDL compiler crate (crates/wamn-ddl) — consumes wamn-catalog: whole
# Catalog -> CREATE, or catalog diff() -> ordered MigrationPlan of ALTERs. Emits
# the tenant floor (id uuid PK + tenant_id + FORCE RLS + app.tenant policy;
# tenant-scoped uniqueness/indexes). Each op classified additive/destructive;
# plan.sql(Confirmation) refuses destructive DDL unless ConfirmedWithBackup.
# migrate() is NAME-REUSE-SAFE: a name-freeing preamble precedes the additive-
# first tail — (1) dropped-reclaimed tables renamed aside wamn_mig_drop_* WITH
# their indexes [index names don't follow a table rename; aside targets
# collision-checked across the full relation namespace], (2) reclaimed
# constraint/index drops PRE-rename on old table names [+ force-hoist of an
# entity's drops when its column drop hoists: DROP COLUMN implicitly drops
# dependent objects], (3) ALL table renames dependency-ordered, each pkey
# following its rename [prevents silent pkey suffix-drift + a later aside-
# rename grabbing a live table's index], (4) per-entity column-namespace
# freeing [hoisted column drops + column renames dependency-ordered]. So
# rename/drop-and-re-add name reuse, same-named constraint/index/column
# redefinition, and rename chains all apply under the 2.5 one-txn apply; the
# DROP TABLE of an aside table stays LAST (FK unwind intact); table/column
# rename swap cycles + aside-name collisions rejected (CompileError::
# TableRenameCycle / ColumnRenameCycle / TempNameCollision). All ordering
# rules are mutation-tested (13 mutants killed).
# PLUS the 5.14/D4 row-event PRODUCERS: Migration::outbox_triggers(catalog,
# &OutboxOptions{schema:"wamn_run"}) — a SEPARATE opt-in all-additive plan (one
# shared plpgsql fn + a CONSTANT-named AFTER INSERT/UPDATE/DELETE trigger per
# entity table; CREATE OR REPLACE = idempotent + rename-safe) inserting the
# outbox row (event=lower(TG_OP), tenant from the ROW, payload=to_jsonb(NEW/OLD)
# — jsonb numerics exact, no-float rule holds) INSIDE the user's txn; NOT folded
# into create()/migrate() (their consumers' schemas have no outbox); gated
# destructive drop_outbox_triggers counterpart; emit-outbox example prints a
# provisioning script. EMITS+CLASSIFIES only (live apply=2.5, backup=2.3/10.3,
# lifecycle=3.4, per-role RLS=3.5; dispatcher consumer=5.14 docs/run-queue.md).
# docs/ddl-compiler.md. No JSON-schema to regen.
cargo test -p wamn-ddl
cargo clippy -p wamn-ddl --all-targets && cargo fmt -p wamn-ddl --check
# optional live-apply gates (emitted SQL + outbox-trigger behavior [same-txn
# event, exact-decimal payload, RLS isolation, conflict no-op fires nothing,
# re-apply stacks no duplicate, confirmed drop silences] against a throwaway PG;
# superuser URL — provisions wamn_app + ephemeral schemas; skips when unset):
docker run -d --rm --name wamn-ddl-pg -p 5451:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DDL_PG_URL=postgres://postgres:postgres@127.0.0.1:5451/wamn cargo test -p wamn-ddl
docker stop wamn-ddl-pg

# [3.4] schema versioning & environments crate (crates/wamn-schema) — composes
# wamn-catalog (3.1) + wamn-ddl (3.2) + wamn-registry (wamn-q3n.1, for the Env +
# Triple vocabulary). Owns the draft->staged->applied->superseded LIFECYCLE state
# machine (pure transition table + Environment enforcing the two cross-version
# guards: single-applied, and the stale-base rebase guard) and PROMOTION between
# first-class environments (promote(src_env,tgt_env) / promote_catalog(src,
# tgt_applied?) -> PromotionPlan, reusing Migration + Confirmation gate verbatim;
# the JSON promotion format is already Catalog::to/from_json). [wamn-q3n.5] an
# Environment carries the (org, project, env) Triple keyed on the closed
# wamn_registry::Env {dev, canary, prod} (canary = prod-shaped validation,
# prod-side failure domain); promote() refuses a cross-application move
# (PromoteError::DifferentApplication, same (org,project) required) and warns on a
# non-forward env order (dev->canary->prod). Version numbers are GLOBALLY UNIQUE
# per catalog (promotion mints a fresh version in the target env), so environment
# is an attribute, not identity. Model + policy only — live apply=2.5,
# backup=2.3/10.3, designer UI=3.3, per-role RLS=3.5. docs/schema-lifecycle.md. No
# JSON-schema to regen. Storage additions (state/environment/base_version +
# single-applied partial-unique + an environment CHECK IN (dev|canary|prod) whose
# literals = Env::as_str, DEFAULT 'dev', both drift-guarded) are ADDITIVE to the
# STANDALONE deploy/catalog-schema.sql (not postgres-init.sql).
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

# [3.5] RLS policy builder crate (crates/wamn-rls) — consumes wamn-catalog (3.1)
# + wamn-ddl (3.2). Compiles per-entity access rules tied to ROLES to Postgres
# RLS: row-ownership (owner col = app.user_id, exempt roles), role command gates
# (which roles may INSERT/UPDATE/DELETE; reads open within tenant), custom
# per-role predicate (escape hatch, emitted verbatim). Every policy is AS
# RESTRICTIVE so it ANDs within — never widens — the 3.2 tenant floor (permissive
# = OR would break isolation). Keys on app.role (COALESCE'd) + app.user_id
# (NULLIF(...)::uuid) claims injected by the plugin (2.2/4.2); absent claim ->
# safe deny. Reuses wamn-ddl MigrationPlan/Operation/Confirmation + its shared
# sql::{quote_ident,quote_literal} (newly public). compile()->MigrationPlan, all
# additive (a note flags restriction-can-deny-until-claims). EMITS+CLASSIFIES
# only (live apply=2.5, claim inject=2.2/4.2, authN=8.1, field masks=4.3; tenant
# floor stays 3.2). docs/rls-builder.md. Storage: catalog.rls_policies (rule
# jsonb) ADDITIVE to the STANDALONE deploy/catalog-schema.sql. No JSON-schema.
cargo test -p wamn-rls
cargo clippy -p wamn-rls --all-targets && cargo fmt -p wamn-rls --check
# optional live-apply gate (floor + compiled policy on a throwaway PG; asserts
# the restrictive policy actually FILTERS rows — owner sees own, exempt sees all,
# no-user-claim denies all; superuser URL provisions wamn_app; skips when unset):
docker run -d --rm --name wamn-rls-pg -p 5453:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RLS_PG_URL=postgres://postgres:postgres@127.0.0.1:5453/wamn cargo test -p wamn-rls
docker stop wamn-rls-pg

# [3.6] seed-data & fixtures crate (crates/wamn-seed) — consumes wamn-catalog
# (3.1) + wamn-ddl (3.2). A typed Dataset (rows per entity, each a symbolic KEY;
# reference fields carry the TARGET KEY not a uuid) validated against a Catalog
# (types incl exact-decimal/no-float, enum variants, uuid parse, referential
# integrity vs seeded keys, required fields, composite-unique) and compiled to
# tenant-scoped INSERTs. IDs are DETERMINISTIC uuidv5("tenant:entity:key") so
# references resolve at compile time + re-seeding is stable; emits one INSERT/row
# in FK-safe (topological) order with ON CONFLICT (id) DO NOTHING (idempotent —
# test-host schema clone / re-seed = no-op). compile(dataset,catalog,tenant)->
# MigrationPlan (reused from 3.2), all additive. deps + a small pure uuid(v5).
# EMITS+CLASSIFIES only (live load=2.5/hosting/test-host 11.1; run fixtures=11.3;
# masking=11.9 — carries the sensitive flag). docs/seed-data.md. Storage:
# catalog.seed_datasets (dataset jsonb) ADDITIVE to the STANDALONE
# deploy/catalog-schema.sql. No JSON-schema.
cargo test -p wamn-seed
cargo clippy -p wamn-seed --all-targets && cargo fmt -p wamn-seed --check
# optional live-apply gate (floor + compiled seed on a throwaway PG; loads TWICE
# and asserts the FK resolves + the re-apply is a no-op; superuser URL; skips
# when unset):
docker run -d --rm --name wamn-seed-pg -p 5454:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_SEED_PG_URL=postgres://postgres:postgres@127.0.0.1:5454/wamn cargo test -p wamn-seed
docker stop wamn-seed-pg

# [4.1] REST API gateway (crates/wamn-api + components/api-gateway) — consumes
# wamn-catalog (3.1) + wamn-ddl (3.2). crates/wamn-api is the PURE gateway logic:
# Catalog -> route table + request -> (injection-safe parameterized SQL, params)
# for CRUD + one-level relation expansion + filter/sort/paginate, and row-set ->
# JSON (numeric = exact-decimal STRING, no float). Values are ALWAYS $n params;
# identifiers are ALWAYS catalog-allowlisted + quote_ident'd (wamn_ddl::sql);
# tenant_id on INSERT = current_setting('app.tenant', true) (server-side; RLS
# floor does isolation). components/api-gateway is the thin
# wasi:http/incoming-handler <-> wamn:postgres shell (loads the catalog snapshot
# from the DB, memoized; no wasi:sockets, no outbound http). Pure-crate tests
# cover CRUD/filter/sort/paginate/expand + injection/allowlist/exact-decimal
# negatives. docs/api-gateway.md. No JSON-schema (routes are derived).
cargo test -p wamn-api
cargo clippy -p wamn-api --all-targets && cargo fmt -p wamn-api --check
# the api-gateway component builds with the other guests (wasm32-wasip2); the
# apibench gate drives it via wasi:http (ProxyPre) against a real PG. Local
# iteration (throwaway container; the superuser provisions the ephemeral schema +
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

# [4.1b] api-gateway SERVING deployment + catalog snapshot (crates/wamn-host:
# publish-catalog + apiproof subcommands + apifixture shared demo fixture;
# deploy/registry.yaml + api-gateway-workload.yaml + publish-catalog-job.yaml +
# apiproof-job.yaml + proof-catalog.json). 4.1 built the component + the
# in-process apibench gate; 4.1b runs it as a real wasi:http WorkloadDeployment.
# wash-runtime ships the inbound HTTP server (--http-addr, port 80); DynamicRouter
# routes by Host header to the component's incoming-handler — NO serve-api
# subcommand; the gateway deploys like deploy/hello-workload.yaml. wamn:postgres
# is a host PLUGIN, so it MUST be declared in the workload hostInterfaces
# (the plugin allowlist; wasi:http is a built-in and bypasses it) or the
# component fails to instantiate. Component images are OCI-pulled by wash-runtime's
# own client (NOT kind-loaded): a local plain-HTTP registry (deploy/registry.yaml)
# holds api_gateway.wasm; the host runs with --allow-insecure-registries +
# WAMN_PG_URL (deploy/values-wamn.yaml hostGroups[].extraArgs/env). publish-catalog
# writes the wamn_catalog snapshot (superuser, RLS-scoped, $n::text::jsonb) +
# optional --provision (3.2 floor); the demo-row --seed flag is GATES-side
# (wamn-gates publish-catalog wraps the prod tool, SR1) — additive only (CREATE
# SCHEMA IF NOT EXISTS, dedicated api_proof schema, never drops). Claims
# wamn.tenant/project/schema via components[].localResources.config (host-injected,
# non-spoofable). apiproof drives the DEPLOYED gateway over real HTTP (apibench's
# assertions, over the Service). apifixture is the shared demo catalog/ids/seed
# (= proof-catalog.json, drift-guarded). docs/api-gateway.md § serving.
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

# [POC-DM1] data model via the catalog API (wamn-521, P1 build) — the API-first
# build of the Material Receiving data model (docs/poc-material-receiving.md §Data
# model) + the end-to-end acceptance test of the 2.5 migration engine. NO new
# engine code: the NEW poc/dm1 (wamn-dm1) crate COMPOSES the shipped tools over
# three PROMOTED deploy/ artifacts — migrate-catalog (2.5) applies
# deploy/poc-material-receiving.catalog.json (a promotion of the wamn-catalog
# fixture, drift-guarded == it) LIVE (DDL + lifecycle advance + history, one txn);
# wamn-rls (3.5) compiles deploy/poc-material-receiving.rls.json (inspector hold
# site-scoping = a RolePredicate on quality_holds.site_id keyed on a NEW app.site
# claim + the ERP receipts-insert RoleCommands gate); wamn-seed (3.6) compiles
# deploy/poc-material-receiving.seed.dataset.json (sites/suppliers/materials +
# inspector users carrying the cert_level extension); app_system (2.4,
# deploy/app-schema.sql) seats the personas' roles + the ERP api-key.
# wamn_dm1::provisioning_sql(tenant) composes migrate->RLS->seed. TWO CAVEATS: the
# is-system users entity migrates to a DATA-SCHEMA users table carrying cert_level
# (wamn-ddl emits CREATE not ALTER — the app_system.users unification is follow-up
# wamn-5x0.3), and the role/site RLS claims are INERT until 4.2 injects them (the
# plugin injects only app.tenant today — the 3.5 deploy-order hazard; the gate
# proves them by SETting the claims by hand). The pricing field mask is 4.3 (the
# sensitive flag is migrated). Mutants killed: site-scoping predicate / ERP gate
# roles / an exact-decimal spec / promoted-catalog drift — each fails a NAMED test.
# docs/poc-dm1.md. No JSON-schema (an integration deliverable, not a contract).
cargo test -p wamn-dm1     # drift-guard + compile checks + live-apply gate (skips w/o WAMN_DM1_PG_URL)
cargo clippy -p wamn-dm1 --all-targets && cargo fmt -p wamn-dm1 --check
# optional throwaway-PG live-apply gate (superuser url — provisions wamn_app,
# applies catalog-schema.sql + app-schema.sql, migrates the POC catalog + attaches
# the RLS + seeds + seats the app_system personas, then asserts site-scoped RLS
# reads/writes + the ERP receipts gate + composite unique + exact-decimal specs;
# skips when unset):
docker run -d --rm --name wamn-dm1-pg -p 5463:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DM1_PG_URL=postgres://postgres:postgres@127.0.0.1:5463/wamn cargo test -p wamn-dm1
docker stop wamn-dm1-pg
# NOTHING in-cluster (a catalog + schema deliverable, the migrate/rls/seed
# precedent; applying it in-cluster would mutate a shared DB — the guardrail).

# [POC-F1] receipt-received sync flow end-to-end (P1 exit, wamn-067) — the D15
# sync path LIVE: NEW components/poc-webhook-f1 (exports wasi:http/incoming-
# handler, imports wamn:postgres ONLY — 2.6-clean) matches POST /receipts
# against the ACTIVE sync-webhook flow (flows registry, re-read per request),
# WRITE-AHEADS a runs row (server-minted run id, status 'dispatched',
# trigger_source 'webhook', input_json VERBATIM — a non-JSON body still gets
# its run and its 400) BEFORE any effect, drives the 5.2 engine over
# deploy/f1-flow.json (fixture topology, F1-shaped node types: validate-receipt
# [shape + no-float + business-key resolution + spec prefetch; every client
# fault => invalid-input => error edge => 400 with the issue list],
# upsert-receipt [ONE wamn:postgres tx: composite-natural-key upsert + replace
# lines], evaluate-specs [pure exact-decimal, boundary equality IN-spec,
# branches port 'out-of-spec'], create-holds [quality_holds status 'open',
# RETURNING ids], respond [status from config; 503 override when the error
# payload's code isn't the configured one]), records node_runs per node in the
# 5.7 shape (an errored node = an 'error'-port emission carrying the engine's
# {"error":...} payload, recorded ONLY when the node HAS an error edge — an
# error row for an edge-less node would reconstruct a failed run as completed;
# taxonomy in error_kind/error_detail), and answers {receipt_id, holds:[...]}
# in-request; infra-failure bodies are GENERIC (pg detail stays in the run
# history, never echoed to the caller; create-holds is ONE tx — no partial
# holds; flows read is ORDER BY flow_id). PURE logic
# in NEW poc/f1 (decimal/payload/evaluate/sql/shapes; does NOT decide
# D8 — no raw-SQL node ships; 5.3 stays wamn-r13-blocked). STORAGE: NEW
# deploy/flows.sql gives the flow registry its production home (ADDITIVE to
# run-state.sql; the a52 stand-in shape, now canonical); publish-catalog is the
# one project-provisioning tool: --runstate (applies the CANONICAL
# deploy/run-state.sql + flows.sql — include_str!'d, dot-anchored
# 'wamn_run'->schema rewrite; .dockerignore now ships deploy/ into the image
# build) + --seed-dataset (wamn-seed compile) + --flow (validate + register +
# ACTIVATE in ONE txn, deactivating prior versions; flows.flow_id minted from
# the graph => the wi4 column==graph guard holds by construction; a webhook
# path another ACTIVE flow serves is REJECTED before any write [wamn-i7i] —
# the flows_active_webhook_path partial-unique expression index in
# deploy/flows.sql is the race-proof backstop, and a failed insert rolls the
# deactivate back). f1bench provisions its ephemeral schema through the SAME
# helpers, so the flags are gated too — incl the collision rejection (named
# pre-check error + raw-insert index violation + different-path acceptance).
# V1 caveats (docs/poc-f1.md): ERP retries mint new runs (duplicate holds /
# FK-blocked line replace under holds); orphaned sync runs stay 'running' (the
# 5.14 janitor only sees QUEUED runs); auth = tenant claim (4.2 pending).
cargo test -p wamn-f1        # decimal/payload/evaluate/shapes + catalog & flow drift-guards
cargo clippy -p wamn-f1 --all-targets && cargo fmt -p wamn-f1 --check
(cd components && cargo build --release --target wasm32-wasip2 -p poc-webhook-f1)
cargo clippy --manifest-path components/poc-webhook-f1/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/poc-webhook-f1/Cargo.toml --check
cargo test -p wamn-gates    # f1fixture coherence (burst = 20 receipts / 3 out-of-spec /
                             # 4 holds) + the publish-catalog schema-rewrite drift-guard
# f1bench GATE (in-proc ProxyPre: poc-webhook-f1 + the 4.1 api-gateway over ONE
# ephemeral schema wamn_f1_bench, provisioned via the publish-catalog helpers;
# modes happy/holds/invalid/burst/rest — sync 200s, write-ahead audit, node_runs
# traces incl the error port, quality_holds rows, RLS isolation, generated-REST
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
# DEPLOYED proof over real networking: push the component (via the 4.1b
# registry port-forward), provision poc_f1, deploy the two workloads
# (poc-webhook-f1 routed f1.localhost.direct + an api-gateway instance routed
# api-f1.localhost.direct, both claiming wamn.schema=poc_f1), then f1proof
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

## Architecture Overview

wasmCloud-based managed low-code platform. `docs/` is the design source of
truth (`platform-plan.md`, `p0-exit-criteria.md`, decision table, WIT
contracts); `docs/p0-results.md` records spike measurements. `crates/wamn-host`
is the production host library + thin binary/image (embeds
`wash_runtime::washlet::ClusterHostBuilder`, deployed by the runtime-operator
Helm chart with custom image values in `deploy/`); the gate suite is the
separate `crates/wamn-gates` binary/image over the same lib, with shared
measurement helpers in `crates/wamn-gate-harness` (SR1 — one Dockerfile, two
`--target` stages); `components/` holds wasm32-wasip2 guests (production at
the root, `fixtures/`+`samples/` beneath, `poc-` prefix for POC components);
POC crates live under `poc/`; our wash-runtime modifications are carried
commits on the fork (`docs/wash-runtime-fork.md`).

## Code Conventions

House rules (docs/structure-review.md SR6), each with its load-bearing reason:

1. **Pure core / effect shell.** Decision logic is clock-free, connection-free,
   unit-testable; effects (DB, wasm, network, time) live in drivers. `now` is a
   passed-in value, never read inside a decision crate. (wamn-runner,
   wamn-run-store, wamn-run-queue, wamn-f1, wamn-api all comply; new subsystems
   follow.)
2. **Errors are enums mirroring WIT variants,** folded mechanically by engines
   and gates — a deliberate deviation from struct-error guidance, justified
   because the WIT boundary dictates variant shape and the runner's retry
   semantics consume it. Keep it a decision, not a habit: new error types get
   variants per failure mode, never `Error(String)`.
3. **SQL is pure text builders + `$n` params in crates;** whoever holds the
   connection executes. Values are ALWAYS `$n` binds; identifiers are ALWAYS
   pinned or allowlist-quoted. No inline SQL in components or drivers (SR2
   completes the regime for run-state).
4. **Components are shells** — ≤ a few hundred lines of dispatch glue binding
   WIT imports/exports to crate logic; logic lives in crates (the wamn-api /
   wamn-runner precedent).
5. **The bench/fixture/proof triple is the gate pattern:** a host-side gate
   subcommand + wasm fixtures + (when deployed) a proof Job, asserted against a
   real backend and mutation-tested where load-bearing. Gates live in the gates
   binary, shared measurement code in the gate harness (SR1).
6. **Naming:** the `wamn_` SQL-identifier prefix is platform-reserved
   (R9a/wamn-66x); env vars are `WAMN_*`; POC components carry a `poc-` prefix
   and POC crates live under `poc/` (SR3) — the tree states the tier.
7. **Drift guards over duplication bans:** where two representations must
   coexist (WIT ↔ SDK mirror, docs WIT ↔ vendored copies, schema literals ↔
   DDL files, fixture consts ↔ committed JSON), a coherence/drift test pins
   them — copy the pattern (`wit_coherence.rs`, `schema_drift`, storage
   drift-guards) rather than banning the second copy.

## Conventions & Patterns

_Add your project-specific conventions here_
