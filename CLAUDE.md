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
cargo build --release -p wamn-host
(cd components && cargo build --release --target wasm32-wasip2)  # guest fixtures

# S1/4p3/bp4.1 gates (instantiation, density, cap kill, epoch kill, memory budgets):
./target/release/wamn-host --log-level warn bench \
  --hello components/target/wasm32-wasip2/release/hello.wasm \
  --memhog components/target/wasm32-wasip2/release/memhog.wasm \
  --busyloop components/target/wasm32-wasip2/release/busyloop.wasm

# S2 gates (qps + p99, saturation, chaos/RLS/injection) — needs a Postgres.
# Local iteration (throwaway container + the same fixture SQL):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
./target/release/wamn-host --log-level error pgbench \
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
  ./target/release/wamn-host --log-level error pgbench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (co-located, no cpu limit — S2 CFS lesson;
# WAMN_PG_ADMIN_URL is the superuser used only to provision the project DBs):
kubectl -n wamn-system apply -f deploy/pgbench-multiproject-job.yaml
kubectl -n wamn-system logs -f job/pgbench-multiproject

# S3 gates (dispatch p99, hot-reload, checkpoint/resume idempotency). The
# dispatch gate is same-binary and needs no DB; hot-reload/resume use the s3.*
# fixture tables (also in deploy/postgres-init.sql).
./target/release/wamn-host --log-level error flowbench \
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
jco componentize components/node-ts/node.js --wit components/node-ts/wit \
  --world-name node-bench --disable http --disable fetch-event \
  -o components/node-ts/node-ts.wasm
REL=components/target/wasm32-wasip2/release
wac plug $REL/flow_driver.wasm --plug $REL/node_rs.wasm -o $REL/flow_composed.wasm
./target/release/wamn-host --log-level error nodebench \
  --node-rs $REL/node_rs.wasm --node-ts components/node-ts/node-ts.wasm \
  --composed $REL/flow_composed.wasm --mode all
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
  ./target/release/wamn-host --log-level info logbench \
  --logspewer components/target/wasm32-wasip2/release/logspewer.wasm --mode all
# In-cluster gate of record (real Loki + collector; no cpu limit — the S2 lesson):
kubectl -n wamn-system apply -f deploy/loki.yaml -f deploy/otel-collector.yaml
kubectl -n wamn-system rollout status deploy/loki deploy/otel-collector --timeout=120s
kubectl -n wamn-system apply -f deploy/logbench-job.yaml
kubectl -n wamn-system logs -f job/logbench

# S6 gates (test-host plugin-swap: sameness / 24h-delay under virtual time /
# egress spy / S3 regression). Needs a Postgres. The test host provisions a
# FRESH ephemeral schema through the SUPERUSER url (the runner's wamn_app role
# is NOSUPERUSER/NOCREATEDB and cannot create schemas). The extended flowrunner
# (delay + http-call nodes, unqualified table names resolved via host-injected
# search_path) builds with the other guests — no extra fixture.
# Local iteration (throwaway container + the same fixture SQL):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
./target/release/wamn-host --log-level error testhostbench \
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
# Job of record. FAIL path is unit-tested (cargo test -p wamn-host egressbench).
# See docs/security-db-path.md.
REL=components/target/wasm32-wasip2/release
./target/release/wamn-host --log-level warn egressbench \
  --flowrunner $REL/flowrunner.wasm \
  --component $REL/pgprobe.wasm --component $REL/node_rs.wasm \
  --component $REL/flow_composed.wasm --component $REL/hello.wasm \
  --component $REL/api_gateway.wasm \
  --component $REL/webhook_entry.wasm  # 4.1/F1 serving workloads: {wamn:postgres,wasi:http}

cargo clippy -p wamn-host --all-targets && cargo fmt -p wamn-host --check

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
  ./target/release/wamn-host --log-level error queuebench \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn \
  --nats-url nats://127.0.0.1:4232 --mode all
docker stop wamn-rq-pg wamn-rq-nats
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS lesson;
# WAMN_PG_ADMIN_URL is the superuser that provisions the ephemeral schema; nats is the
# operator chart's mTLS Service [verify_and_map] — the job mounts the wasmcloud-runtime-
# tls cert so the doorbell connects, no deploy/nats.yaml). A HOST change => full docker
# rebuild (docker build -t wamn-host:dev . && kind load docker-image wamn-host:dev --name wamn):
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
  ./target/release/wamn-host --log-level error failoverbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn --mode all
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS lesson;
# WAMN_PG_ADMIN_URL is the superuser that provisions the ephemeral schema; no NATS). A
# HOST change => full docker rebuild (docker build -t wamn-host:dev . && kind load
# docker-image wamn-host:dev --name wamn):
kubectl -n wamn-system apply -f deploy/failoverbench-job.yaml
kubectl -n wamn-system logs -f job/failoverbench

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
  ./target/release/wamn-host --log-level error dispatchbench \
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
# HOST change => full docker rebuild (docker build -t wamn-host:dev . && kind load
# docker-image wamn-host:dev --name wamn):
kubectl -n wamn-system apply -f deploy/dispatchbench-job.yaml
kubectl -n wamn-system logs -f job/dispatchbench

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
# wamn-catalog (3.1) + wamn-ddl (3.2). Owns the draft->staged->applied->superseded
# LIFECYCLE state machine (pure transition table + Environment enforcing the two
# cross-version guards: single-applied, and the stale-base rebase guard) and
# PROMOTION between first-class dev/prod environments (promote(src_env,tgt_env) /
# promote_catalog(src,tgt_applied?) -> PromotionPlan, reusing Migration +
# Confirmation gate verbatim; the JSON promotion format is already Catalog::to/
# from_json). Version numbers are GLOBALLY UNIQUE per catalog (promotion mints a
# fresh version in the target env), so environment is an attribute, not identity.
# Model + policy only — live apply=2.5, backup=2.3/10.3, designer UI=3.3, per-role
# RLS=3.5. docs/schema-lifecycle.md. No JSON-schema to regen. Storage additions
# (state/environment/base_version + single-applied partial-unique) are ADDITIVE to
# the STANDALONE deploy/catalog-schema.sql (not postgres-init.sql).
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
  ./target/release/wamn-host --log-level error apibench \
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
# optional --provision (3.2 floor) + --seed (demo rows) — additive only (CREATE
# SCHEMA IF NOT EXISTS, dedicated api_proof schema, never drops). Claims
# wamn.tenant/project/schema via components[].localResources.config (host-injected,
# non-spoofable). apiproof drives the DEPLOYED gateway over real HTTP (apibench's
# assertions, over the Service). apifixture is the shared demo catalog/ids/seed
# (= proof-catalog.json, drift-guarded). docs/api-gateway.md § serving.
cargo test -p wamn-host   # apifixture drift-guard + publish-catalog ident test
cargo clippy -p wamn-host --all-targets && cargo fmt -p wamn-host --check
# In-cluster proof of record (needs the kind 'wamn' cluster + operator + postgres):
docker build -t wamn-host:dev . && kind load docker-image wamn-host:dev --name wamn
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

# [POC-F1] receipt-received sync flow end-to-end (P1 exit, wamn-067) — the D15
# sync path LIVE: NEW components/webhook-entry (exports wasi:http/incoming-
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
# holds; flows read is ORDER BY flow_id — deterministic on path collisions). PURE logic
# in NEW crates/wamn-f1 (decimal/payload/evaluate/sql/shapes; does NOT decide
# D8 — no raw-SQL node ships; 5.3 stays wamn-r13-blocked). STORAGE: NEW
# deploy/flows.sql gives the flow registry its production home (ADDITIVE to
# run-state.sql; the a52 stand-in shape, now canonical); publish-catalog is the
# one project-provisioning tool: --runstate (applies the CANONICAL
# deploy/run-state.sql + flows.sql — include_str!'d, dot-anchored
# 'wamn_run'->schema rewrite; .dockerignore now ships deploy/ into the image
# build) + --seed-dataset (wamn-seed compile) + --flow (validate + register +
# ACTIVATE, deactivating prior versions; flows.flow_id minted from the graph =>
# the wi4 column==graph guard holds by construction). f1bench provisions its
# ephemeral schema through the SAME helpers, so the flags are gated too.
# V1 caveats (docs/poc-f1.md): ERP retries mint new runs (duplicate holds /
# FK-blocked line replace under holds); orphaned sync runs stay 'running' (the
# 5.14 janitor only sees QUEUED runs); auth = tenant claim (4.2 pending).
cargo test -p wamn-f1        # decimal/payload/evaluate/shapes + catalog & flow drift-guards
cargo clippy -p wamn-f1 --all-targets && cargo fmt -p wamn-f1 --check
(cd components && cargo build --release --target wasm32-wasip2 -p webhook-entry)
cargo clippy --manifest-path components/webhook-entry/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/webhook-entry/Cargo.toml --check
cargo test -p wamn-host      # f1fixture coherence (burst = 20 receipts / 3 out-of-spec /
                             # 4 holds) + the publish-catalog schema-rewrite drift-guard
# f1bench GATE (in-proc ProxyPre: webhook-entry + the 4.1 api-gateway over ONE
# ephemeral schema wamn_f1_bench, provisioned via the publish-catalog helpers;
# modes happy/holds/invalid/burst/rest — sync 200s, write-ahead audit, node_runs
# traces incl the error port, quality_holds rows, RLS isolation, generated-REST
# cross-check incl expand=line). Local iteration (throwaway PG; superuser
# provisions the ephemeral schema):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
REL=components/target/wasm32-wasip2/release
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5450/wamn \
  ./target/release/wamn-host --log-level error f1bench \
  --webhook-entry $REL/webhook_entry.wasm --api-gateway $REL/api_gateway.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS
# lesson; ephemeral schema => shared-PG safe; bench Jobs run SEQUENTIALLY):
kubectl -n wamn-system apply -f deploy/f1bench-job.yaml
kubectl -n wamn-system logs -f job/f1bench
# DEPLOYED proof over real networking: push the component (via the 4.1b
# registry port-forward), provision poc_f1, deploy the two workloads
# (webhook-entry routed f1.localhost.direct + an api-gateway instance routed
# api-f1.localhost.direct, both claiming wamn.schema=poc_f1), then f1proof
# (sync + burst + DB audit + REST):
wash push localhost:5000/wamn/webhook-entry:dev \
  components/target/wasm32-wasip2/release/webhook_entry.wasm --insecure
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

docker build -t wamn-host:dev .   # fetches the fork git dep in its builder stage
```

## Architecture Overview

wasmCloud-based managed low-code platform. `docs/` is the design source of
truth (`platform-plan.md`, `p0-exit-criteria.md`, decision table, WIT
contracts); `docs/p0-results.md` records spike measurements. `crates/wamn-host`
is the custom host image (embeds `wash_runtime::washlet::ClusterHostBuilder`,
deployed by the runtime-operator Helm chart with custom image values in
`deploy/`); `components/` holds wasm32-wasip2 guest fixtures; our wash-runtime
modifications are carried commits on the fork (`docs/wash-runtime-fork.md`).

## Conventions & Patterns

_Add your project-specific conventions here_
