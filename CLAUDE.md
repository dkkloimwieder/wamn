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

wamn-host builds against a **patched** wash-runtime — see `patches/README.md`
for the carried-patch mechanism and the wasmCloud rev-bump procedure. The rev
is pinned in one place: `workspace.dependencies.wash-runtime.rev` in the root
`Cargo.toml`.

```bash
./scripts/vendor-wasmcloud.sh   # once per clone / rev bump / patch change:
                                # produces vendor/wasmcloud (pinned rev + patches)
cargo build --release -p wamn-host
(cd components && cargo build --release --target wasm32-wasip2)  # guest fixtures

# S1/4p3 gates (instantiation, density, cap kill, epoch kill):
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
  --component $REL/flow_composed.wasm --component $REL/hello.wasm

cargo clippy -p wamn-host --all-targets && cargo fmt -p wamn-host --check

# [5.1] flow-graph schema crate (crates/wamn-flow) — canonical flow JSON: types,
# validation, import/export, version diff. Pure Rust, no host/DB. Tests cover
# fixture round-trip, structural validation, JSON-Schema conformance (boon),
# schema drift-guard, and diff. docs/flow-schema.md + docs/flow-schema.schema.json.
cargo test -p wamn-flow
cargo clippy -p wamn-flow --all-targets && cargo fmt -p wamn-flow --check
# regenerate the published JSON Schema contract after changing the types:
cargo run -p wamn-flow --example print-schema > docs/flow-schema.schema.json

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
# EMITS+CLASSIFIES only (live apply=2.5, backup=2.3/10.3, lifecycle=3.4,
# per-role RLS=3.5). docs/ddl-compiler.md. No JSON-schema to regen.
cargo test -p wamn-ddl
cargo clippy -p wamn-ddl --all-targets && cargo fmt -p wamn-ddl --check
# optional live-apply gate (emitted SQL against a throwaway PG; superuser URL —
# provisions wamn_app + an ephemeral schema; skips cleanly when unset):
docker run -d --rm --name wamn-ddl-pg -p 5451:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DDL_PG_URL=postgres://postgres:postgres@127.0.0.1:5451/wamn cargo test -p wamn-ddl
docker stop wamn-ddl-pg

docker build -t wamn-host:dev .   # runs the vendor script in its builder stage
```

## Architecture Overview

wasmCloud-based managed low-code platform. `docs/` is the design source of
truth (`platform-plan.md`, `p0-exit-criteria.md`, decision table, WIT
contracts); `docs/p0-results.md` records spike measurements. `crates/wamn-host`
is the custom host image (embeds `wash_runtime::washlet::ClusterHostBuilder`,
deployed by the runtime-operator Helm chart with custom image values in
`deploy/`); `components/` holds wasm32-wasip2 guest fixtures; `patches/` +
`scripts/vendor-wasmcloud.sh` carry our wash-runtime modifications.

## Conventions & Patterns

_Add your project-specific conventions here_
