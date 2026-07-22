//! The `provision-dashboards` subcommand ([9.9] wamn-b4e): the per-tenant
//! Grafana dashboards half of the observability presentation layer.
//!
//! 9.9 has two halves. The **SRE** dashboards are STATIC (file-provisioned into
//! one shared folder by `deploy/infra/grafana.yaml`); this verb owns the
//! **per-tenant** half, which static provisioning cannot express (a folder per
//! tenant is not knowable at deploy time). Like the `provision-*` family it is an
//! imperative control-plane tool, run as a Job or from a runbook. It:
//!
//!   1. `--emit-sre <dir>`: pure render — write the SRE dashboard JSON (the
//!      dashboards-as-code source that grafana.yaml's ConfigMap + the local
//!      docker mount carry). No network. The `provision-org --emit-clusters`
//!      precedent; the checked-in `deploy/infra/grafana/dashboards/wamn-sre.json`
//!      is this output, drift-guarded (`sre_json_matches_render`).
//!   2. `--grafana-url … --system-database-url …`: enumerate the orgs from the T1
//!      registry (`registry.orgs`) and drive the Grafana HTTP API — one folder
//!      per org (`POST /api/folders`, idempotent on a stable uid) + a per-tenant
//!      dashboard templated with the org pinned into every query
//!      (`POST /api/dashboards/db`, `overwrite:true`). This is how Grafana does
//!      folder-scoped multitenancy and it keeps per-tenant state with the other
//!      control-plane provisioning verbs.
//!
//! **TENANT ≠ ORG (the load-bearing 9.9 caveat, verified in code — docs/dashboards.md).**
//! The registry models `Triple{org, project, env}` and has NO tenant table; the
//! `wamn.tenant` claim that becomes the `wamn_tenant` metric label / the
//! `span.wamn.tenant` trace attribute / the Loki `tenant` field is a per-workload,
//! host-injected, non-spoofable RLS/observability string set in each workload's
//! `localResources.config["wamn.tenant"]` (or the runner `--tenant`). The
//! materializer example sets `wamn.tenant: t1` under `WAMN_MAT_ORG: morg` and
//! states "ONE TENANT PER WORKLOAD (v1): a multi-tenant project-env runs one
//! materializer workload per tenant" — a tenant is a SUB-ORG scope, not the org.
//! The only per-customer unit the registry enumerates is the ORG (the
//! isolation/billing unit, docs/postgres-topology.md), so this verb enumerates
//! orgs and pins the org id as the tenant filter: EXACT under the recommended
//! single-tenant-per-org convention (`wamn.tenant` == org id), a per-org rollup
//! otherwise. It never invents a tenant list the registry does not hold.
//!
//! **Coverage is PARTIAL (docs/dashboards.md §per-tenant limit).** A per-tenant
//! dashboard covers only what carries the tenant: run throughput/success, drive
//! duration, queue depth (`wamn_tenant`), the trace surface (`span.wamn.tenant`,
//! sliced by the WIT-namespaced span name), and logs (the Loki `tenant` field).
//! Postgres pool/query (project-only), memory (component-only) and generated-API
//! RPS (status_class-only) are inherently SRE/platform metrics — omitted per
//! tenant with an on-dashboard note.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio_postgres::NoTls;

// ---------------------------------------------------------------------------
// Metric / attribute names — the SINGLE source both the SRE render and the
// per-tenant template draw from, drift-guarded against docs/metrics.md
// (`metric_names_match_docs`). Prometheus mangling: dots -> underscores,
// add_metric_suffixes:false, so histograms keep only their structural
// `_bucket`/`_count`/`_sum` (deploy/infra/otel-collector.yaml). Verified against
// docs/metrics.md's metric table.
// ---------------------------------------------------------------------------

/// run-worker executions counter — labels `outcome`, `wamn_tenant`, `wamn_project`.
const M_RUN_EXECUTIONS: &str = "wamn_run_executions";
/// run-drive duration histogram — labels `wamn_tenant`, `wamn_project` (+ `le`).
const M_RUN_DRIVE_BUCKET: &str = "wamn_run_drive_duration_ms_bucket";
/// run-queue depth gauge — labels `wamn_tenant`, `wamn_project`.
const M_RUN_QUEUE_DEPTH: &str = "wamn_run_queue_depth";
/// postgres pool gauges (project-only) — SRE.
const M_POOL_SIZE: &str = "wamn_postgres_pool_size";
const M_POOL_AVAILABLE: &str = "wamn_postgres_pool_available";
const M_POOL_WAITING: &str = "wamn_postgres_pool_waiting";
/// postgres query-latency histogram (label `db_operation`, project-only) — SRE.
const M_QUERY_BUCKET: &str = "wamn_postgres_query_duration_ms_bucket";
/// per-component memory (component-only) — SRE.
const M_MEM_HIGH_WATER: &str = "wamn_memory_high_water_bytes";
const M_MEM_BUDGET: &str = "wamn_memory_budget_bytes";
const M_MEM_DENIED: &str = "wamn_memory_denied";
/// generated-API RPS (status_class-only, fork) — SRE.
const M_API_REQUESTS: &str = "wamn_api_requests";

/// Every metric name a panel references — the drift-guard set (each must appear
/// verbatim in docs/metrics.md). Consumed only by `metric_names_match_docs`.
#[cfg(test)]
const ALL_METRICS: &[&str] = &[
    M_RUN_EXECUTIONS,
    M_RUN_DRIVE_BUCKET,
    M_RUN_QUEUE_DEPTH,
    M_POOL_SIZE,
    M_POOL_AVAILABLE,
    M_POOL_WAITING,
    M_QUERY_BUCKET,
    M_MEM_HIGH_WATER,
    M_MEM_BUDGET,
    M_MEM_DENIED,
    M_API_REQUESTS,
];

// Fixed datasource uids (deploy/infra/grafana.yaml provisions these) — panels
// reference a datasource by uid, so the uid must be stable across re-provision.
// Public so the `dashproof` gate asserts the SAME uids (one source, no drift).
pub const DS_PROM: &str = "wamn-prometheus";
pub const DS_TEMPO: &str = "wamn-tempo";
pub const DS_LOKI: &str = "wamn-loki";

/// The SRE dashboard's stable identity (dashproof asserts these present).
pub const SRE_DASHBOARD_UID: &str = "wamn-sre-overview";
pub const SRE_DASHBOARD_TITLE: &str = "wamn SRE overview";
/// The static SRE folder title — MUST match `folder:` in
/// deploy/infra/grafana/provisioning/dashboards/providers.yaml.
pub const SRE_FOLDER_TITLE: &str = "wamn SRE";

// ---------------------------------------------------------------------------
// Panel + dashboard builders (dashboards-as-code). Kept minimal: the smallest
// valid Grafana model the file provisioner AND the /api/dashboards/db POST both
// accept. gridPos is assigned by `layout` (two columns), so builders need not
// track position.
// ---------------------------------------------------------------------------

fn ds(kind: &str, uid: &str) -> Value {
    json!({ "type": kind, "uid": uid })
}

/// A Prometheus timeseries panel over one-or-more `(legend, expr)` targets.
fn ts_panel(title: &str, targets: &[(&str, String)]) -> Value {
    let t: Vec<Value> = targets
        .iter()
        .enumerate()
        .map(|(i, (legend, expr))| {
            json!({
                "refId": ref_id(i),
                "datasource": ds("prometheus", DS_PROM),
                "expr": expr,
                "legendFormat": legend,
            })
        })
        .collect();
    json!({
        "type": "timeseries",
        "title": title,
        "datasource": ds("prometheus", DS_PROM),
        "targets": t,
        "fieldConfig": { "defaults": {}, "overrides": [] },
        "options": {},
    })
}

/// A Tempo TraceQL panel (table of matching spans) — the WIT-namespaced trace
/// surface, filtered to the tenant and sliced by span name (the interface).
fn traceql_panel(title: &str, query: &str) -> Value {
    json!({
        "type": "table",
        "title": title,
        "datasource": ds("tempo", DS_TEMPO),
        "targets": [{
            "refId": "A",
            "datasource": ds("tempo", DS_TEMPO),
            "queryType": "traceql",
            "query": query,
        }],
        "fieldConfig": { "defaults": {}, "overrides": [] },
        "options": {},
    })
}

/// A Loki logs panel — the single-tenant Loki filtered by the `tenant` structured
/// metadata field (loki.yaml: tenant is metadata, `service_name` the stream label).
fn logs_panel(title: &str, expr: &str) -> Value {
    json!({
        "type": "logs",
        "title": title,
        "datasource": ds("loki", DS_LOKI),
        "targets": [{
            "refId": "A",
            "datasource": ds("loki", DS_LOKI),
            "expr": expr,
        }],
        "options": {},
    })
}

/// A markdown text panel — carries the per-tenant coverage note ON the dashboard.
fn note_panel(title: &str, markdown: &str) -> Value {
    json!({
        "type": "text",
        "title": title,
        "options": { "mode": "markdown", "content": markdown },
    })
}

fn ref_id(i: usize) -> String {
    // A..Z then A1.. — only ever a handful of targets per panel.
    if i < 26 {
        char::from(b'A' + i as u8).to_string()
    } else {
        format!("A{i}")
    }
}

/// Assign a two-column gridPos to each panel in order (w=12, h=8), so a rendered
/// dashboard lays out without hand-placed coordinates. Text notes span full width.
fn layout(mut panels: Vec<Value>) -> Vec<Value> {
    let mut x = 0;
    let mut y = 0;
    for p in &mut panels {
        let full = p.get("type").and_then(Value::as_str) == Some("text");
        let (w, h) = if full { (24, 4) } else { (12, 8) };
        if full && x != 0 {
            x = 0;
            y += 8;
        }
        p["gridPos"] = json!({ "h": h, "w": w, "x": x, "y": y });
        if full {
            y += h;
            x = 0;
        } else if x == 0 {
            x = 12;
        } else {
            x = 0;
            y += h;
        }
    }
    panels
}

fn dashboard(uid: &str, title: &str, tags: &[&str], panels: Vec<Value>) -> Value {
    json!({
        "uid": uid,
        "title": title,
        "tags": tags,
        "schemaVersion": 39,
        "version": 0,
        "editable": true,
        "timezone": "",
        "time": { "from": "now-6h", "to": "now" },
        "refresh": "30s",
        "templating": { "list": [] },
        "annotations": { "list": [] },
        "panels": layout(panels),
    })
}

/// The success-ratio expression — `completed / (completed|failed)`, clamped so an
/// idle window reads 0/1 rather than NaN. `sel` is an optional label selector
/// fragment (`{wamn_tenant="x"}` for the tenant view, `""` for SRE).
fn success_ratio_expr(sel: &str) -> String {
    format!(
        "sum(rate({M_RUN_EXECUTIONS}{{outcome=\"completed\"{s}}}[5m])) / \
         clamp_min(sum(rate({M_RUN_EXECUTIONS}{{outcome=~\"completed|failed\"{s}}}[5m])), 1)",
        s = comma_sel(sel),
    )
}

/// Turn a bare selector like `wamn_tenant=\"x\"` into `,wamn_tenant=\"x\"` so it
/// slots after an existing `{outcome=...` predicate; empty stays empty.
fn comma_sel(sel: &str) -> String {
    if sel.is_empty() {
        String::new()
    } else {
        format!(",{sel}")
    }
}

/// Wrap a bare selector as a full `{sel}` label set (or `""`).
fn braces(sel: &str) -> String {
    if sel.is_empty() {
        String::new()
    } else {
        format!("{{{sel}}}")
    }
}

fn quantile_expr(q: &str, bucket: &str, sel: &str, by: &str) -> String {
    format!(
        "histogram_quantile({q}, sum(rate({bucket}{b}[5m])) by (le{by}))",
        b = braces(sel),
    )
}

/// The SRE (platform) dashboard — every family, no tenant filter. The
/// dashboards-as-code source `--emit-sre` writes and grafana.yaml carries.
pub fn render_sre_dashboard() -> Value {
    let panels = vec![
        ts_panel(
            "Run throughput by outcome",
            &[(
                "{{outcome}}",
                format!("sum(rate({M_RUN_EXECUTIONS}[5m])) by (outcome)"),
            )],
        ),
        ts_panel("Run success ratio", &[("success", success_ratio_expr(""))]),
        ts_panel(
            "Run-drive duration",
            &[
                ("p50", quantile_expr("0.5", M_RUN_DRIVE_BUCKET, "", "")),
                ("p99", quantile_expr("0.99", M_RUN_DRIVE_BUCKET, "", "")),
            ],
        ),
        ts_panel(
            "Run-queue depth by project",
            &[(
                "{{wamn_project}}",
                format!("sum({M_RUN_QUEUE_DEPTH}) by (wamn_project)"),
            )],
        ),
        ts_panel(
            "Postgres pool saturation by project",
            &[
                ("size {{wamn_project}}", M_POOL_SIZE.to_string()),
                ("available {{wamn_project}}", M_POOL_AVAILABLE.to_string()),
                ("waiting {{wamn_project}}", M_POOL_WAITING.to_string()),
            ],
        ),
        ts_panel(
            "Postgres query latency p99 by operation",
            &[(
                "{{db_operation}}",
                quantile_expr("0.99", M_QUERY_BUCKET, "", ",db_operation"),
            )],
        ),
        ts_panel(
            "Component memory: high-water vs budget",
            &[
                ("high-water {{component}}", M_MEM_HIGH_WATER.to_string()),
                ("budget {{component}}", M_MEM_BUDGET.to_string()),
            ],
        ),
        ts_panel(
            "Memory denials by component",
            &[(
                "{{component}}",
                format!("sum(rate({M_MEM_DENIED}[5m])) by (component)"),
            )],
        ),
        ts_panel(
            "Generated-API RPS by status class",
            &[(
                "{{status_class}}",
                format!("sum(rate({M_API_REQUESTS}[5m])) by (status_class)"),
            )],
        ),
    ];
    dashboard(
        SRE_DASHBOARD_UID,
        SRE_DASHBOARD_TITLE,
        &["wamn", "sre"],
        panels,
    )
}

/// The per-tenant dashboard for `org` — the run/queue metrics filtered by
/// `wamn_tenant`, the WIT-namespaced trace surface on `span.wamn.tenant`, and the
/// tenant's logs. Org ids are validated slugs, so pinning them into the query
/// strings is injection-safe.
pub fn render_tenant_dashboard(org: &str) -> Value {
    let sel = format!("wamn_tenant=\"{org}\"");
    let panels = vec![
        ts_panel(
            "Run throughput by outcome",
            &[(
                "{{outcome}}",
                format!("sum(rate({M_RUN_EXECUTIONS}{{{sel}}}[5m])) by (outcome)"),
            )],
        ),
        ts_panel(
            "Run success ratio",
            &[("success", success_ratio_expr(&sel))],
        ),
        ts_panel(
            "Run-drive duration",
            &[
                ("p50", quantile_expr("0.5", M_RUN_DRIVE_BUCKET, &sel, "")),
                ("p99", quantile_expr("0.99", M_RUN_DRIVE_BUCKET, &sel, "")),
            ],
        ),
        ts_panel(
            "Run-queue depth",
            &[("depth", format!("sum({M_RUN_QUEUE_DEPTH}{{{sel}}})"))],
        ),
        traceql_panel(
            "Traces by interface (WIT-namespaced span name)",
            &format!("{{ span.wamn.tenant = \"{org}\" }}"),
        ),
        logs_panel(
            "Logs",
            &format!("{{service_name=~\"wamn-.*\"}} | tenant=\"{org}\""),
        ),
        note_panel(
            "Coverage note",
            "Per-tenant panels cover only what carries the tenant claim: run \
             throughput/success, drive duration, queue depth, traces \
             (`span.wamn.tenant`, sliced by interface) and logs (`tenant`). \
             Postgres pool/query (project-only), component memory and \
             generated-API RPS are platform metrics — see the **wamn SRE** folder.",
        ),
    ];
    dashboard(
        &tenant_dashboard_uid(org),
        &format!("wamn tenant {org} — runs / traces / logs"),
        &["wamn", "tenant", org],
        panels,
    )
}

// ---------------------------------------------------------------------------
// Tenant -> Grafana folder/dashboard identity. Grafana uids are <= 40 chars;
// org ids are <= 40 (registry.orgs charset CHECK), so a `wt-<org>` prefix can
// overflow — fall back to a deterministic fnv1a suffix for a pathologically long
// org (keeps idempotency: same org -> same uid). Normal short org ids stay
// human-readable.
// ---------------------------------------------------------------------------

const UID_MAX: usize = 40;

pub fn tenant_folder_uid(org: &str) -> String {
    bounded_uid("wt-", org)
}

pub fn tenant_dashboard_uid(org: &str) -> String {
    bounded_uid("wtd-", org)
}

fn bounded_uid(prefix: &str, org: &str) -> String {
    let readable = format!("{prefix}{org}");
    if readable.len() <= UID_MAX {
        readable
    } else {
        format!("{prefix}{:016x}", fnv1a_64(org.as_bytes()))
    }
}

pub fn tenant_folder_title(org: &str) -> String {
    format!("wamn tenant {org}")
}

/// FNV-1a 64 (the house inline digest, cf. traceproof) — a stable short id for a
/// long org, never a security boundary.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A defensive slug check mirroring the registry `check_id` charset (the
/// `registry.orgs` rows already satisfy it; this guards a hand-passed value).
fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= UID_MAX
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Debug, Args)]
pub struct ProvisionDashboardsArgs {
    /// Emit the SRE dashboard JSON (dashboards-as-code) into this directory and
    /// exit — pure render, no network (regenerates
    /// deploy/infra/grafana/dashboards/). Mutually exclusive with the API drive.
    #[arg(long)]
    pub emit_sre: Option<PathBuf>,

    /// Grafana base URL (the per-tenant API drive), e.g. http://grafana:3000.
    #[arg(long)]
    pub grafana_url: Option<String>,

    /// Grafana admin user (Basic auth for the folder/dashboard writes).
    #[arg(long, env = "GF_SECURITY_ADMIN_USER", default_value = "admin")]
    pub user: String,

    /// Grafana admin password (Basic auth). Env GF_SECURITY_ADMIN_PASSWORD.
    #[arg(long, env = "GF_SECURITY_ADMIN_PASSWORD")]
    pub password: Option<String>,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`) — the org list
    /// is read from `registry.orgs`. Env WAMN_SYSTEM_ADMIN_URL.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,
}

pub async fn run(args: ProvisionDashboardsArgs) -> anyhow::Result<()> {
    // Mode 1: pure emit of the SRE dashboards-as-code (no network).
    if let Some(dir) = &args.emit_sre {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create emit dir {}", dir.display()))?;
        let path = dir.join("wamn-sre.json");
        let text = serde_json::to_string_pretty(&render_sre_dashboard())?;
        std::fs::write(&path, format!("{text}\n"))
            .with_context(|| format!("write {}", path.display()))?;
        println!("wrote SRE dashboard -> {}", path.display());
        return Ok(());
    }

    // Mode 2: drive the Grafana API for per-tenant folders + dashboards.
    let grafana_url = args
        .grafana_url
        .as_deref()
        .context("provision-dashboards needs --grafana-url (or --emit-sre to render only)")?
        .trim_end_matches('/');
    let password = args
        .password
        .as_deref()
        .context("provision-dashboards needs --password / GF_SECURITY_ADMIN_PASSWORD")?;
    let sys_url = args.system_database_url.as_deref().context(
        "provision-dashboards needs --system-database-url / WAMN_SYSTEM_ADMIN_URL to enumerate \
         registry.orgs",
    )?;
    let auth = basic_auth(&args.user, password);

    let orgs = read_orgs(sys_url).await.context("read registry.orgs")?;
    println!(
        "# provision-dashboards: {} org(s) from registry.orgs -> {grafana_url}",
        orgs.len()
    );
    if orgs.is_empty() {
        println!("(no orgs recorded — nothing to provision; SRE dashboards are file-provisioned)");
        return Ok(());
    }

    for org in &orgs {
        if !is_slug(org) {
            bail!("org id {org:?} is not a slug — refusing to template it into queries");
        }
        let folder_uid = tenant_folder_uid(org);
        let folder_title = tenant_folder_title(org);
        upsert_folder(grafana_url, &auth, &folder_uid, &folder_title).await?;
        upsert_dashboard(
            grafana_url,
            &auth,
            &folder_uid,
            &render_tenant_dashboard(org),
        )
        .await?;
        println!(
            "  org {org:?}: folder {folder_uid:?} + dashboard {:?}",
            tenant_dashboard_uid(org)
        );
    }
    println!(
        "provision-dashboards: {} tenant folder(s) provisioned",
        orgs.len()
    );
    Ok(())
}

/// The org ids recorded in the T1 registry, the per-customer enumeration unit.
async fn read_orgs(system_url: &str) -> anyhow::Result<Vec<String>> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        let rows = client
            .query("SELECT id FROM registry.orgs ORDER BY id", &[])
            .await
            .context("SELECT registry.orgs")?;
        anyhow::Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

// ---------------------------------------------------------------------------
// Grafana HTTP API — POST /api/folders (idempotent on uid) + POST
// /api/dashboards/db (overwrite:true). Hand-rolled HTTP/1.1 with Basic auth (the
// repo's dependency-free gate/verb posture; cf. metricbench/traceproof GET).
// ---------------------------------------------------------------------------

async fn upsert_folder(base: &str, auth: &str, uid: &str, title: &str) -> anyhow::Result<()> {
    let body = json!({ "uid": uid, "title": title });
    let (status, resp) = http_json(base, "POST", "/api/folders", Some(auth), Some(&body)).await?;
    // 200 created; 409/412 = it already exists (idempotent re-run — its title is
    // stable, so nothing to update).
    if status == 200 || status == 409 || status == 412 {
        Ok(())
    } else {
        bail!("POST /api/folders {uid:?} -> {status}: {resp}");
    }
}

async fn upsert_dashboard(
    base: &str,
    auth: &str,
    folder_uid: &str,
    dash: &Value,
) -> anyhow::Result<()> {
    let body = json!({
        "dashboard": dash,
        "folderUid": folder_uid,
        "overwrite": true,
    });
    let (status, resp) =
        http_json(base, "POST", "/api/dashboards/db", Some(auth), Some(&body)).await?;
    if status == 200 {
        Ok(())
    } else {
        bail!("POST /api/dashboards/db -> {status}: {resp}");
    }
}

/// `Basic <base64(user:password)>` — the auth header value. Public so the
/// `dashproof` gate shares one Basic-auth + HTTP implementation.
pub fn basic_auth(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64_encode(format!("{user}:{password}").as_bytes())
    )
}

/// Standard base64 (RFC 4648) — a tiny inline encoder so no dep is pulled for one
/// Authorization header.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Hand-rolled HTTP/1.1 JSON request returning `(status_code, body)`. Plain http
/// only (in-cluster/local Grafana), Connection: close. Public so the `dashproof`
/// gate issues its authenticated GETs through the SAME client.
pub async fn http_json(
    base: &str,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: Option<&Value>,
) -> anyhow::Result<(u16, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let host_port = base.strip_prefix("http://").unwrap_or(base);
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(3000)),
        None => (host_port.to_string(), 3000),
    };
    let payload = match body {
        Some(v) => serde_json::to_string(v)?,
        None => String::new(),
    };
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        payload.len(),
    );
    if let Some(a) = auth {
        req.push_str(&format!("Authorization: {a}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(&payload);

    let mut stream = TcpStream::connect((host.as_str(), port))
        .await
        .with_context(|| format!("connect {host}:{port}"))?;
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .with_context(|| format!("no HTTP status in response: {:?}", text.lines().next()))?;
    let (head, raw_body) = text
        .split_once("\r\n\r\n")
        .map(|(h, b)| (h.to_string(), b.to_string()))
        .unwrap_or_default();
    let out = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(&raw_body)
    } else {
        raw_body
    };
    Ok((status, out))
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        if after.len() < size {
            out.push_str(after);
            break;
        }
        out.push_str(&after[..size]);
        rest = after.get(size + 2..).unwrap_or("");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dashboards-as-code drift guard: every metric name a panel references
    /// must appear verbatim in docs/metrics.md (the 9.8 metric-set source of
    /// truth). A corrupted constant (or a doc rename) trips this.
    #[test]
    fn metric_names_match_docs() {
        // docs/metrics.md spells names in OTel form, mixing `.` (namespace) with
        // `_` (unit suffix: `duration_ms`, `budget_bytes`) and a brace family
        // (`wamn.postgres.pool.{size,available,waiting}`). Normalize both sides'
        // separators to `_`, then accept either a literal family match OR the
        // brace form `<stem>_{…<leaf>…}` — corruption of any segment still trips.
        let doc = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/metrics.md"
        ))
        .expect("read docs/metrics.md");
        let doc_n = doc.replace('.', "_");
        for m in ALL_METRICS {
            // Drop a structural histogram suffix (the scrape base is documented).
            let base = m
                .strip_suffix("_bucket")
                .or_else(|| m.strip_suffix("_count"))
                .or_else(|| m.strip_suffix("_sum"))
                .unwrap_or(m);
            let literal = doc_n.contains(base);
            let brace = base.rsplit_once('_').is_some_and(|(stem, leaf)| {
                doc_n.contains(&format!("{stem}_{{")) && doc_n.contains(leaf)
            });
            assert!(
                literal || brace,
                "metric {m:?} (base {base:?}) not documented in docs/metrics.md"
            );
        }
    }

    /// The checked-in SRE dashboard JSON is exactly what the code renders — the
    /// dashboards-as-code source cannot silently drift from grafana.yaml's mount.
    /// Regenerate with `provision-dashboards --emit-sre deploy/infra/grafana/dashboards`.
    #[test]
    fn sre_json_matches_render() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/infra/grafana/dashboards/wamn-sre.json"
        );
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read wamn-sre.json"))
                .expect("parse wamn-sre.json");
        assert_eq!(
            on_disk,
            render_sre_dashboard(),
            "deploy/infra/grafana/dashboards/wamn-sre.json is stale — rerun --emit-sre"
        );
    }

    /// The per-tenant template pins the tenant into every filterable query and
    /// omits the un-scopable families (pool/query/memory/api). The tenant string
    /// appears in the run/trace/log selectors.
    #[test]
    fn tenant_template_pins_tenant_and_omits_platform_metrics() {
        let d = render_tenant_dashboard("acme");
        let json = serde_json::to_string(&d).unwrap();
        // pinned everywhere it can be
        assert!(json.contains("wamn_tenant=\\\"acme\\\""), "metric filter");
        assert!(
            json.contains("span.wamn.tenant = \\\"acme\\\""),
            "trace filter"
        );
        assert!(json.contains("tenant=\\\"acme\\\""), "log filter");
        // the un-scopable families are NOT on a tenant dashboard
        for m in [
            M_POOL_SIZE,
            M_QUERY_BUCKET,
            M_MEM_HIGH_WATER,
            M_API_REQUESTS,
        ] {
            assert!(!json.contains(m), "tenant dashboard must omit {m}");
        }
        assert_eq!(d["uid"], "wtd-acme");
    }

    /// tenant -> folder/dashboard uid mapping: readable for a normal org,
    /// deterministic + bounded (<= 40) for a pathologically long one; distinct
    /// prefixes so a folder and its dashboard never collide.
    #[test]
    fn tenant_uid_mapping() {
        assert_eq!(tenant_folder_uid("acme"), "wt-acme");
        assert_eq!(tenant_dashboard_uid("acme"), "wtd-acme");
        assert_eq!(tenant_folder_title("acme"), "wamn tenant acme");

        let long = "a".repeat(40);
        let fu = tenant_folder_uid(&long);
        let du = tenant_dashboard_uid(&long);
        assert!(fu.len() <= UID_MAX && du.len() <= UID_MAX, "bounded");
        assert!(fu.starts_with("wt-") && du.starts_with("wtd-"));
        // deterministic (idempotent re-run yields the same uid)
        assert_eq!(fu, tenant_folder_uid(&long));
        assert_ne!(fu, du);
    }

    #[test]
    fn slug_guard_rejects_injection() {
        assert!(is_slug("acme"));
        assert!(is_slug("acme-prod-1"));
        assert!(!is_slug("acme\"} or 1=1"));
        assert!(!is_slug("Acme"));
        assert!(!is_slug("-acme"));
        assert!(!is_slug(""));
    }

    #[test]
    fn base64_is_rfc4648() {
        // Known vectors incl. both pad lengths.
        assert_eq!(base64_encode(b"admin:admin"), "YWRtaW46YWRtaW4=");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(basic_auth("admin", "admin"), "Basic YWRtaW46YWRtaW4=");
    }

    #[test]
    fn dechunk_reassembles() {
        assert_eq!(dechunk("5\r\nhello\r\n0\r\n\r\n"), "hello");
    }
}
