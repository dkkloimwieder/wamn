//! The `metricbench` subcommand: the [9.8] metric-set gate (wamn-jn6).
//!
//! 9.8 ships the host-side metric set — run executions + success ratio, run-drive
//! duration, run-queue depth, `wamn:postgres` pool saturation + query latency,
//! per-component memory high-water/denials, and generated-API RPS — over the
//! SAME global meter provider the S5/9.1 pipeline installs (the fork's
//! `initialize_observability`, active whenever `OTEL_*` is set). This gate drives
//! the real production emission seams and asserts each family lands in the OTel
//! Collector's Prometheus scrape (`:8889`, the metrics analog of `tracebench`'s
//! Tempo query / `logbench`'s Loki query):
//!
//!   1. drive N runs incl. exactly one forced failure -> `wamn_run_executions`
//!      grows by N and carries an `outcome="failed"` series (success ratio);
//!   2. seed a queue then run a dispatcher tick -> `wamn_run_queue_depth` > 0,
//!      then drain -> back to 0;
//!   3. `wamn_run_drive_duration_ms_count` > 0 (a real per-drive histogram);
//!   4. force a memory-limiter denial -> `wamn_memory_denied` > 0 and
//!      `wamn_memory_high_water_bytes` reads the ALLOWED size, not the budget;
//!   5. the run drives' own DB calls surface `wamn_postgres_pool_size` and
//!      `wamn_postgres_query_duration_ms_count` > 0;
//!   6. M api-gateway calls -> `wamn_api_requests` (the fork's inbound HTTP
//!      counter) — IN-CLUSTER ONLY (ProxyPre benches bypass the host's HTTP
//!      server), honest-skipped locally.
//!
//! Local recipe (docs/metrics.md): the tracebench docker collector +
//! otelcol-local's new metrics pipeline + a throwaway Postgres, with
//! `OTEL_METRIC_EXPORT_INTERVAL=1000` so the periodic reader does not wait a
//! minute. In-cluster gate of record: `deploy/gates/metricbench-job.yaml`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_postgres::{Client, NoTls};
use wamn_run_queue::{enqueue_sql, write_ahead_triggered_run_sql};

use wamn_dispatcher::{Dispatcher, DispatcherConfig, ProjectSpec, register_queue_depth_gauge};
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::memory_metrics::global_memory_meter;
use wamn_host::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wamn_run_worker::RunWorker;
use wash_runtime::engine::ctx::WamnStoreLimiter;
use wash_runtime::wasmtime::ResourceLimiter as _;

/// The metricbench ephemeral schema + identity (distinct from runnerbench's so a
/// concurrent run does not collide).
const SCHEMA: &str = "wamn_metricbench";
const TENANT: &str = "metric-tenant";
const OWNER: &str = "metric-bench";
/// The normal (completing) fixture flow — poc-receipt (webhook, pg-write): its DB
/// write also drives the pool + query-latency families.
const FLOW_ID: &str = "poc-receipt";
/// The forced-failure fixture: a single `postgres-query` head that dies
/// `Terminal("capability-denied")` at the standard-node grant check (D8 raw-SQL
/// off) — a one-step, no-I/O terminal business failure (outcome = failed),
/// deterministic and instant (unlike a runaway-budget spin).
const FAIL_FLOW_ID: &str = "metric-terminal";

/// The component id the phase-4 forced-denial limiter is labelled by.
const MEM_COMPONENT: &str = "metricbench-memhog";

fn fail_flow_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{FAIL_FLOW_ID}","version":1,
            "trigger":{{"type":"manual"}},"entry":"in",
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"q","type":"postgres-query","config":{{}}}}
            ],
            "edges":[{{"from":"in","to":"q"}}]}}"#
    )
}

#[derive(Debug, Args)]
pub struct MetricBenchArgs {
    /// The flowrunner guest the runner instantiates + drives.
    #[arg(long)]
    pub flowrunner: PathBuf,

    /// App (runner) Postgres URL — the NOSUPERUSER wamn_app role. Overrides
    /// WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions/drops the ephemeral schema.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The OTel Collector's Prometheus scrape (the new :8889 app-metrics
    /// pipeline). In-cluster: http://otel-collector:8889/metrics.
    #[arg(
        long,
        env = "METRICS_URL",
        default_value = "http://127.0.0.1:8889/metrics"
    )]
    pub metrics_url: String,

    /// Normal (completing) runs driven in phase 1.
    #[arg(long, default_value_t = 8)]
    pub runs: usize,

    /// Claimable runs seeded for the phase-2 depth check.
    #[arg(long, default_value_t = 6)]
    pub depth_seed: usize,

    /// api-gateway calls the in-cluster phase 6 would drive (SKIPPED locally).
    #[arg(long, default_value_t = 5)]
    pub api_calls: usize,
}

// ---------------------------------------------------------------------------
// Ephemeral schema + seeding (the runnerbench pattern; reuses its drift-guarded
// union DDL so metricbench tracks the run-queue schema of record for free).
// ---------------------------------------------------------------------------

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect for ephemeral schema")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
                 CREATE SCHEMA {SCHEMA} AUTHORIZATION postgres; \
                 GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app;"
            ))
            .await
            .context("create ephemeral schema")?;
        client
            .batch_execute(&crate::runnerbench::runner_ddl(SCHEMA))
            .await
            .context("apply runner DDL")?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn teardown(admin_url: &str) {
    if let Ok((client, conn)) = tokio_postgres::connect(admin_url, NoTls).await {
        let conn_task = tokio::spawn(conn);
        let _ = client
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;"))
            .await;
        drop(client);
        let _ = conn_task.await;
    }
}

/// A wamn_app connection pinned to the ephemeral schema + tenant claim (the RLS
/// floor + search_path the runner's plugin session runs under).
async fn connect_app(app_url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(app_url, NoTls)
        .await
        .context("app (wamn_app) connect")?;
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute(&format!(
            "SET search_path TO {SCHEMA}; SET app.tenant TO '{TENANT}';"
        ))
        .await
        .context("set search_path + tenant claim")?;
    Ok((client, handle))
}

/// Seed a run the way the dispatcher does (write-ahead `dispatched` row +
/// immediately-claimable queue row), for the given flow at version 1.
async fn seed_run(client: &mut Client, run_id: &str, flow_id: &str) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(
        &write_ahead_triggered_run_sql(),
        &[&run_id, &flow_id, &1i32, &"cron", &"\"receipt\""],
    )
    .await?;
    tx.execute(
        &enqueue_sql(),
        &[&run_id, &Option::<&str>::None, &0i32, &0i64],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn count(client: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(client.query_one(sql, &[]).await?.get(0))
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

pub async fn run(args: MetricBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    // The exporter only activates when OTEL_* is set (fork observability init) —
    // without it nothing reaches the collector and every scrape is empty.
    if !std::env::vars().any(|(k, _)| k.starts_with("OTEL_")) {
        bail!(
            "no OTEL_* env set: metricbench needs OTEL_EXPORTER_OTLP_ENDPOINT pointing at the \
             collector (+ OTEL_METRIC_EXPORT_INTERVAL=1000) — else nothing is exported"
        );
    }

    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("failed to read {}", args.flowrunner.display()))?;
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args.admin_database_url.clone().context(
        "metricbench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;

    println!("# wamn-gates [9.8] metricbench (schema {SCHEMA}, tenant {TENANT})");
    println!("metrics = {}", args.metrics_url);
    provision(&admin_url)
        .await
        .context("provision ephemeral schema")?;

    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.clone());
    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    // [9.8-5] pool-saturation gauges over the runner's own project pool.
    plugin.register_pool_metrics();

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    let outcome = async {
        let (mut seed_conn, _h) = connect_app(&app_url).await?;
        wamn_gate_harness::seed_flow_version(
            &seed_conn,
            TENANT,
            FLOW_ID,
            1,
            true,
            &crate::flowbench::flow_json(1),
            true,
        )
        .await?;
        wamn_gate_harness::seed_flow_version(
            &seed_conn,
            TENANT,
            FAIL_FLOW_ID,
            1,
            true,
            &fail_flow_json(),
            true,
        )
        .await?;

        // The production runner (registers wamn.run.* on instantiate).
        let vault = Arc::new(wamn_host::plugins::wamn_credentials::WamnCredentials::empty());
        let logging = Arc::new(wamn_host::plugins::wamn_logging::WamnLogging::from_env()?);
        let mut worker = RunWorker::instantiate(
            &engine,
            &guest,
            plugin.clone(),
            vault,
            logging,
            wamn_run_worker::RunnerIdentity {
                owner: OWNER,
                tenant: TENANT,
                schema: Some(SCHEMA),
                project: "default",
            },
            Arc::from([]),
            30_000,
            None,
        )
        .await?;

        let mut pass = true;

        // === (1) executions counter + success ratio =========================
        let n = args.runs;
        let base_exec = scrape_sum(&args.metrics_url, "wamn_run_executions").await;
        let base_failed = scrape_where(&args.metrics_url, "wamn_run_executions", "outcome=\"failed\"")
            .await;
        for i in 0..n {
            seed_run(&mut seed_conn, &format!("mb-{i}"), FLOW_ID).await?;
        }
        seed_run(&mut seed_conn, "mb-fail", FAIL_FLOW_ID).await?;
        let r1 = worker.drain().await?;
        // Local sanity on the drive itself before waiting on the export.
        let drove_ok = r1.claimed == n + 1 && r1.completed == n && r1.failed == 1;
        // Delta == N+1 executions, with at least one `failed` (the mutant target:
        // an outcome-fold would keep failed at its baseline).
        let want_total = (n + 1) as f64;
        let (exec_ok, (exec_total, failed_delta)) = poll(&args.metrics_url, |text| {
            let total = family_sum(text, "wamn_run_executions") - base_exec;
            let failed = label_sum(text, "wamn_run_executions", "outcome=\"failed\"") - base_failed;
            (total >= want_total && failed >= 1.0, (total, failed))
        })
        .await;
        check(
            &mut pass,
            "(1) executions: delta == N+1 with a failed series",
            drove_ok && exec_ok,
            &format!(
                "drove claimed={}/{} completed={} failed={} ; scrape delta={exec_total} (want {want_total}), failed delta={failed_delta} (want >=1)",
                r1.claimed, n + 1, r1.completed, r1.failed
            ),
        );

        // === (3) run-drive duration histogram (same drives) =================
        let (dur_ok, dur_count) = poll(&args.metrics_url, |text| {
            let c = family_sum(text, "wamn_run_drive_duration_ms_count");
            (c > 0.0, c)
        })
        .await;
        check(
            &mut pass,
            "(3) run-drive duration histogram count > 0",
            dur_ok,
            &format!("wamn_run_drive_duration_ms_count = {dur_count}"),
        );

        // === (5) pool saturation + query latency (from the drives' DB writes) =
        let (pool_ok, pool_size) = poll(&args.metrics_url, |text| {
            let present = present(text, "wamn_postgres_pool_size")
                && family_sum(text, "wamn_postgres_query_duration_ms_count") > 0.0;
            (present, family_sum(text, "wamn_postgres_pool_size"))
        })
        .await;
        check(
            &mut pass,
            "(5) postgres pool gauge present + query-latency count > 0",
            pool_ok,
            &format!(
                "wamn_postgres_pool_size present={} size={pool_size}, query_count={}",
                present_now(&args.metrics_url, "wamn_postgres_pool_size").await,
                scrape_sum(&args.metrics_url, "wamn_postgres_query_duration_ms_count").await
            ),
        );

        // === (2) run-queue depth via the dispatcher tick ====================
        // A real Dispatcher over the same schema; register the gauge over its
        // depth registry. Seed a claimable batch, tick -> depth > 0; drain ->
        // tick -> depth back to 0. The claimable predicate is the mutant target.
        let specs = [ProjectSpec {
            name: "default".to_string(),
            url: app_url.clone(),
            tenant: TENANT.to_string(),
            schema: Some(SCHEMA.to_string()),
        }];
        let mut dispatcher =
            Dispatcher::connect(&specs, None, DispatcherConfig::default()).await?;
        register_queue_depth_gauge(&dispatcher.depth_registry());
        let m = args.depth_seed;
        for i in 0..m {
            seed_run(&mut seed_conn, &format!("mq-{i}"), FLOW_ID).await?;
        }
        dispatcher.tick_project(0, wamn_dispatcher::epoch_ms()).await?;
        let (depth_up_ok, depth_up) = poll(&args.metrics_url, |text| {
            let d = family_sum(text, "wamn_run_queue_depth");
            (d >= m as f64, d)
        })
        .await;
        // Drain the seeded batch, re-tick: the gauge must fall to 0.
        let r2 = worker.drain().await?;
        dispatcher.tick_project(0, wamn_dispatcher::epoch_ms()).await?;
        let (depth_zero_ok, depth_zero) = poll(&args.metrics_url, |text| {
            let d = family_sum(text, "wamn_run_queue_depth");
            (d == 0.0, d)
        })
        .await;
        check(
            &mut pass,
            "(2) run_queue depth > 0 on a seeded queue, drains to 0",
            depth_up_ok && depth_zero_ok && r2.claimed == m,
            &format!(
                "seeded {m}: depth peaked {depth_up} (want >= {m}), after drain (claimed {}) depth {depth_zero} (want 0)",
                r2.claimed
            ),
        );

        // === (4) memory limiter denial + high-water (budget knob) ===========
        // Force one allowed grow (sets high-water) then one over-budget grow
        // (denied) on a budgeted limiter, snapshot it into the process memory
        // meter, and assert the SCRAPE: denied >= 1, high_water reads the ALLOWED
        // 32 MiB (NOT the 64 MiB budget — the budget-vs-high-water swap mutant).
        const MIB: usize = 1 << 20;
        let mut limiter = WamnStoreLimiter::new(64 * MIB, Arc::from(MEM_COMPONENT));
        let allowed = limiter.memory_growing(0, 32 * MIB, None)?;
        let denied = limiter.memory_growing(32 * MIB, 128 * MIB, None)?;
        let mem = global_memory_meter();
        mem.snapshot_from(&limiter);
        let inproc = mem.snapshot_of(MEM_COMPONENT);
        let (mem_ok, (mem_denied, mem_hw)) = poll(&args.metrics_url, |text| {
            let d = family_sum(text, "wamn_memory_denied");
            let hw = label_value(
                text,
                "wamn_memory_high_water_bytes",
                &format!("component=\"{MEM_COMPONENT}\""),
            );
            let budget_present = present(text, "wamn_memory_budget_bytes");
            (
                d >= 1.0 && hw == Some((32 * MIB) as f64) && budget_present,
                (d, hw),
            )
        })
        .await;
        check(
            &mut pass,
            "(4) memory: denied >= 1 and high_water is the allowed size, not the budget",
            allowed && !denied && mem_ok,
            &format!(
                "limiter allowed={allowed} denied={denied}; in-proc snapshot={inproc:?}; \
                 scrape denied={mem_denied} high_water={mem_hw:?} (want {} not the 64 MiB budget)",
                (32 * MIB) as f64
            ),
        );

        // === (6) generated-API RPS (IN-CLUSTER ONLY) ========================
        // The fork's wamn.api.requests counter fires in the host HTTP server's
        // record_response_status; ProxyPre benches bypass that server, so there
        // is no local way to drive it. Honest-skip (traceproof-style) — this
        // phase does NOT touch `pass`.
        println!(
            "## (6) api RPS — SKIP: wamn_api_requests needs the deployed api-gateway \
             ({} calls); ProxyPre bypasses the host HTTP server locally (in-cluster only)",
            args.api_calls
        );

        // Housekeeping counts (informational).
        let queued = count(&seed_conn, &format!("SELECT count(*) FROM {SCHEMA}.run_queue")).await?;
        println!("queue drained fully = {}", queued == 0);

        anyhow::Ok(pass)
    }
    .await;

    ticker.abort();
    teardown(&admin_url).await;
    let pass = outcome?;

    println!("\nmetricbench complete — overall PASS: {pass}");
    if !pass {
        bail!("metricbench gate failed");
    }
    Ok(())
}

fn check(pass: &mut bool, label: &str, ok: bool, detail: &str) {
    if ok {
        println!("## {label} -> PASS");
    } else {
        *pass = false;
        println!("## {label} -> FAIL ({detail})");
    }
}

// ---------------------------------------------------------------------------
// Prometheus scrape + text parsing (the logbench :8888 helper, generalized to
// arbitrary wamn_* families / labels on the :8889 app-metrics pipeline)
// ---------------------------------------------------------------------------

/// Poll the scrape (~30s bounded) until `f` accepts, returning `(accepted, value)`
/// from the last observation. The bound covers the OTel periodic export
/// (`OTEL_METRIC_EXPORT_INTERVAL`) + the collector batch + Prometheus refresh.
async fn poll<T, F>(url: &str, f: F) -> (bool, T)
where
    F: Fn(&str) -> (bool, T),
{
    let mut last = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(750)).await;
        let text = fetch(url).await.unwrap_or_default();
        let (ok, value) = f(&text);
        if ok {
            return (true, value);
        }
        last = Some(value);
    }
    // Report the final observation for the failure detail.
    let text = fetch(url).await.unwrap_or_default();
    let (_, value) = f(&text);
    (false, last.unwrap_or(value))
}

async fn scrape_sum(url: &str, name: &str) -> f64 {
    family_sum(&fetch(url).await.unwrap_or_default(), name)
}

async fn scrape_where(url: &str, name: &str, label: &str) -> f64 {
    label_sum(&fetch(url).await.unwrap_or_default(), name, label)
}

async fn present_now(url: &str, name: &str) -> bool {
    present(&fetch(url).await.unwrap_or_default(), name)
}

/// Whether a scrape line is exactly `name` (followed by `{` or a space), so
/// `wamn_run_executions` never matches `wamn_run_executions_created` or the
/// `_bucket`/`_sum` siblings of a histogram.
fn line_is(line: &str, name: &str) -> bool {
    line.strip_prefix(name)
        .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
}

/// The value (last whitespace token) of a scrape line, if it parses.
fn line_value(line: &str) -> Option<f64> {
    line.rsplit(' ').next().and_then(|v| v.parse::<f64>().ok())
}

/// Sum every series of `name`.
fn family_sum(text: &str, name: &str) -> f64 {
    text.lines()
        .filter(|l| !l.starts_with('#') && line_is(l, name))
        .filter_map(line_value)
        .sum()
}

/// Sum every series of `name` whose label set contains `label` (a raw
/// `key="value"` fragment).
fn label_sum(text: &str, name: &str, label: &str) -> f64 {
    text.lines()
        .filter(|l| !l.starts_with('#') && line_is(l, name) && l.contains(label))
        .filter_map(line_value)
        .sum()
}

/// The value of the first series of `name` carrying `label`, if any.
fn label_value(text: &str, name: &str, label: &str) -> Option<f64> {
    text.lines()
        .find(|l| !l.starts_with('#') && line_is(l, name) && l.contains(label))
        .and_then(line_value)
}

/// Whether any series of `name` is present.
fn present(text: &str, name: &str) -> bool {
    text.lines()
        .any(|l| !l.starts_with('#') && line_is(l, name))
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 GET (the collector's Prometheus endpoint is plain http;
// Go/chunked like Loki/Tempo — same helper shape as logbench/tracebench).
// ---------------------------------------------------------------------------

async fn fetch(url: &str) -> anyhow::Result<String> {
    let host_port = url.strip_prefix("http://").unwrap_or(url);
    let (host_port, path) = match host_port.find('/') {
        Some(i) => (&host_port[..i], &host_port[i..]),
        None => (host_port, "/"),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(80)),
        None => (host_port.to_string(), 80),
    };
    let mut stream = TcpStream::connect((host.as_str(), port))
        .await
        .with_context(|| format!("connect {host}:{port}"))?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: text/plain\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw);
    if !(text.starts_with("HTTP/1.1 2") || text.starts_with("HTTP/1.0 2")) {
        bail!("GET {path} -> {}", text.lines().next().unwrap_or("<none>"));
    }
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .map(|(h, b)| (h.to_string(), b.to_string()))
        .unwrap_or_default();
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        Ok(dechunk(&body))
    } else {
        Ok(body)
    }
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
        rest = after[size..].strip_prefix("\r\n").unwrap_or(&after[size..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The prometheus-text parser: exact family matching (never a `_created` or
    // `_bucket` sibling), label filtering, and value extraction — the pure bit
    // the scrape assertions stand on.
    #[test]
    fn prom_text_parse_family_label_present() {
        let text = "\
# HELP wamn_run_executions runs
# TYPE wamn_run_executions counter
wamn_run_executions{outcome=\"completed\",wamn_project=\"default\"} 8
wamn_run_executions{outcome=\"failed\",wamn_project=\"default\"} 1
wamn_run_executions_created{outcome=\"completed\"} 1.72e9
wamn_run_drive_duration_ms_count{wamn_project=\"default\"} 9
wamn_memory_high_water_bytes{component=\"metricbench-memhog\"} 33554432
";
        // Family sum ignores the `_created` sibling (its huge timestamp would
        // otherwise dominate) and the `_count` of a different family.
        assert_eq!(family_sum(text, "wamn_run_executions"), 9.0);
        // Label filter isolates the failed series.
        assert_eq!(
            label_sum(text, "wamn_run_executions", "outcome=\"failed\""),
            1.0
        );
        // A distinct family matched exactly.
        assert_eq!(family_sum(text, "wamn_run_drive_duration_ms_count"), 9.0);
        // Label value read (high-water = the allowed 32 MiB, not a budget).
        assert_eq!(
            label_value(
                text,
                "wamn_memory_high_water_bytes",
                "component=\"metricbench-memhog\""
            ),
            Some(33554432.0)
        );
        assert!(present(text, "wamn_memory_high_water_bytes"));
        assert!(!present(text, "wamn_api_requests"));
    }

    #[test]
    fn chunked_body_is_reassembled() {
        // "wamn_x 1\n" split into two chunks.
        let framed = "8\r\nwamn_x 1\r\n0\r\n\r\n";
        assert_eq!(dechunk(framed), "wamn_x 1");
    }
}
