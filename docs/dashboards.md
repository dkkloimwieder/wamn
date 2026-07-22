# Dashboards — per-tenant Grafana + SRE (9.9)

The presentation layer over the three first-class signals — traces (9.1, Tempo),
logs (S5/9.3, Loki), metrics (9.8, Prometheus text on `:8889`) — correlated by
run id, with per-tenant isolation expressed as Grafana folders. This is the
dashboards third of Epic 9 (`docs/platform-plan.md` §9.9). It adds **no new
emission**: everything it presents is already exported.

- **Issue:** wamn-b4e `[9.9]`; **Epic:** wamn-7ok (Observability).
- **Builds on:** 9.1 (traces), S5/9.3 (logs), 9.8 (metrics) — the three sinks.

## The stack it adds

9.9 needs a metrics **server** and a **UI** — neither existed. The 9.8 collector
only *exposes* `wamn_*` as Prometheus text on `:8889`; nothing scraped or stored
it (`metricbench` scrapes it directly and throws it away). Tempo (`:3200`) and
Loki (`:3100`) already have queryable APIs. So 9.9 adds exactly two install-once
`deploy/infra/` pieces:

| File | What | Notes |
|---|---|---|
| `deploy/infra/prometheus.yaml` | single-binary `prom/prometheus:v3.1.0` scraping `otel-collector:8889` (+ `:8888`), Service `prometheus:9090` | emptyDir, 24h retention, **no CPU limit** (S2). `prometheus-local.yaml` = the docker config. |
| `deploy/infra/grafana.yaml` | `grafana/grafana:11.4.0`, Service `grafana:3000`, file-provisioned datasources + SRE folder, admin `Secret` | emptyDir, **no CPU limit**. `grafana-local.yaml` = the docker datasources. |

Both mirror the Loki/Tempo single-binary dev posture; HA / per-plan retention is
9.5, out of scope.

## Datasources (fixed uids)

File-provisioned with **stable explicit uids** — a dashboard panel references a
datasource by uid, so the uid must survive re-provision:

| uid | type | in-cluster url |
|---|---|---|
| `wamn-prometheus` | prometheus | `http://prometheus:9090` |
| `wamn-tempo` | tempo | `http://tempo:3200` |
| `wamn-loki` | loki | `http://loki:3100` |

`grafana.yaml` carries the in-cluster URLs inline; `grafana-local.yaml` mirrors
them with the local docker-network names (Tempo/Loki optional locally).

## Dashboards-as-code

Both dashboard sets render from `crates/wamn-ctl/src/provision_dashboards.rs` over
**metric-name CONSTANTS** that a drift guard (`metric_names_match_docs`) pins to
`docs/metrics.md` — a renamed or corrupted metric fails the build, not the
dashboard silently. Prometheus name mangling is dots→underscores with
`add_metric_suffixes:false` (`wamn.run.executions` → `wamn_run_executions`;
histograms keep only `_bucket`/`_count`/`_sum`).

### SRE dashboard (static, folder "wamn SRE")

`deploy/infra/grafana/dashboards/wamn-sre.json` — the **file-provisioned** SRE
set (regenerate with `wamn-ctl provision-dashboards --emit-sre
deploy/infra/grafana/dashboards`; `sre_json_matches_render` guards it from
drifting). Panels, over the REAL 9.8 names:

| Panel | Query |
|---|---|
| Run throughput by outcome | `sum(rate(wamn_run_executions[5m])) by (outcome)` |
| Run success ratio | `…{outcome="completed"} / …{outcome=~"completed\|failed"}` (clamped) |
| Run-drive duration p50/p99 | `histogram_quantile(q, sum(rate(wamn_run_drive_duration_ms_bucket[5m])) by (le))` |
| Run-queue depth by project | `sum(wamn_run_queue_depth) by (wamn_project)` |
| Postgres pool saturation by project | `wamn_postgres_pool_{size,available,waiting}` |
| Postgres query latency p99 by op | `histogram_quantile(0.99, …wamn_postgres_query_duration_ms_bucket… by (le, db_operation))` |
| Component memory: high-water vs budget | `wamn_memory_high_water_bytes` vs `wamn_memory_budget_bytes` |
| Memory denials by component | `sum(rate(wamn_memory_denied[5m])) by (component)` |
| Generated-API RPS by status class | `sum(rate(wamn_api_requests[5m])) by (status_class)` |

### Per-tenant dashboards (runtime, one folder per org)

Static provisioning cannot express a folder-per-tenant (the set is not known at
deploy time), so `wamn-ctl provision-dashboards --grafana-url … --system-database-url …`
enumerates the registry and drives the **Grafana HTTP API**: `POST /api/folders`
(one per org, idempotent on a stable uid `wt-<org>`) + `POST /api/dashboards/db`
(`overwrite:true`) a per-tenant dashboard with the tenant pinned into every
query. This is Grafana's folder-scoped multitenancy and it belongs with the other
`provision-*` control-plane verbs.

Per-tenant panels — only what carries the tenant:

| Panel | Signal / filter |
|---|---|
| Run throughput / success / drive p50-p99 / queue depth | Prometheus, `{wamn_tenant="<org>"}` |
| Traces by interface | Tempo TraceQL `{ span.wamn.tenant = "<org>" }`, sliced by the WIT-namespaced span name (`wamn.postgres` / `wamn.trigger`) |
| Logs | Loki `{service_name=~"wamn-.*"} \| tenant="<org>"` |

## The honest per-tenant limit — coverage is PARTIAL

**Not every signal carries a tenant, so a per-tenant folder cannot show
everything.** This is the load-bearing 9.9 caveat.

| Prometheus metric | Labels | Per-tenant? |
|---|---|---|
| `wamn_run_executions` | `outcome`, **`wamn_tenant`**, `wamn_project` | **yes** |
| `wamn_run_drive_duration_ms_*` | **`wamn_tenant`**, `wamn_project` | **yes** |
| `wamn_run_queue_depth` | **`wamn_tenant`**, `wamn_project` | **yes** |
| `wamn_postgres_pool_{size,available,waiting}` | `wamn_project` only | **no** (project-only) |
| `wamn_postgres_query_duration_ms_*` | `db_operation`, `wamn_project` only | **no** (project-only) |
| `wamn_memory_*` | `component` only | **no** (SRE-only) |
| `wamn_api_requests` | `status_class` only | **no** (SRE-only) |

So per-tenant folders cover **run/queue metrics + traces + logs ONLY**. Postgres
pool/query (project-scoped), component memory, and generated-API RPS are
inherently **SRE/platform** metrics — omitted from the tenant dashboard with an
on-dashboard note pointing at the **wamn SRE** folder.

## `wamn.tenant` ≠ `registry.orgs.id` (verified in code)

The verb enumerates **orgs**, but a metric/trace/log tenant is NOT an org. The
verification (the brief's Q4):

- The registry (`crates/wamn-registry`, `deploy/sql/system-schema.sql`) models
  `Triple{org, project, env}` and has **NO tenant table**. The only per-customer
  unit it enumerates is the **org** — "the unit of isolation is the customer/org"
  (`docs/postgres-topology.md`).
- The `wamn.tenant` claim that becomes the `wamn_tenant` metric label /
  `span.wamn.tenant` trace attribute / Loki `tenant` field is a **per-workload,
  host-injected, non-spoofable** RLS+observability string, set in each workload's
  `localResources.config["wamn.tenant"]` (or the runner `--tenant`), NOT derived
  from the org. `deploy/platform/materializer.example.yaml` sets `wamn.tenant: t1`
  under `WAMN_MAT_ORG: morg` and states *"ONE TENANT PER WORKLOAD (v1): a
  multi-tenant project-env runs one materializer workload per tenant"* — a tenant
  is a **sub-org RLS scope**, not the org.

**Consequence:** `provision-dashboards` enumerates orgs (the only registry-backed
per-customer unit) and pins the **org id** as the tenant filter. This is **exact**
under the recommended single-tenant-per-org convention (`wamn.tenant` == org id)
and a **per-org rollup** otherwise; where a deployment uses distinct sub-org
tenant claims, that org's dashboard shows only the workloads whose `wamn.tenant`
equals the org id. The verb never invents a tenant list the registry does not
hold. (A registry-backed tenant enumeration — a tenants table, or a
tenant-per-workload registration — is the clean follow-up if sub-org dashboards
are needed.)

## The gate — `dashproof`

`crates/wamn-gates/src/dashproof.rs` — a proof (no emission seam to drive), the
`traceproof`/`apiproof` shape, asserting a deployed Grafana over its HTTP API.
Each check is a NAMED failure:

1. `GET /api/health` → `database: ok`.
2. `GET /api/datasources` → Prometheus + Tempo + Loki present (by the fixed
   uids); each `GET /api/datasources/uid/<uid>/health` OK. **Honest-skip**
   (metricbench phase-6 precedent): Prometheus HARD; Tempo/Loki soft in `--local`
   (their containers may be absent), HARD in-cluster.
3. `GET /api/search?type=dash-db` + `GET /api/folders` → the static SRE dashboard
   + folder present.
4. after `provision-dashboards`: every registry org (from `--system-database-url`,
   plus any `--expect-tenant`) has its per-tenant folder + dashboard.

Auth is admin Basic-auth from the `grafana-admin` Secret
(`GF_SECURITY_ADMIN_USER`/`GF_SECURITY_ADMIN_PASSWORD`) — `/api/datasources`
needs an authenticated admin. dashproof shares the SRE identity + per-tenant
folder/dashboard uid derivation with the verb (`wamn_ctl::provision_dashboards`),
so the assertion and the writer never drift.

## Run it

Local iteration + the in-cluster gate of record: `docs/build-and-test.md` §[9.9].

## Boundaries / deferred

- **Sub-org tenant dashboards** — needs a registry-backed tenant enumeration (see
  the `wamn.tenant` ≠ org section); filed as a follow-up.
- **Per-tenant Postgres pool/query** — the metric carries only `wamn_project`; a
  tenant view could *approximate* via `wamn_project=~"<tenant's projects>"` but
  the metric lacks the tenant label. Not shipped.
- **Alerting** — 9.10, a separate bead; 9.9 is presentation only.
- **Loki multitenancy** — Loki is single-tenant (`auth_enabled:false`); the
  `tenant` filter is a structured-metadata predicate, not an isolated stream
  (per-tenant Loki isolation is 9.5).

## References

- Plan: `docs/platform-plan.md` §9.9.
- The three sinks: `docs/tracing.md` (9.1), `docs/metrics.md` (9.8), S5 logging.
- Registry model: `docs/registry-model.md`, `deploy/sql/system-schema.sql`.
