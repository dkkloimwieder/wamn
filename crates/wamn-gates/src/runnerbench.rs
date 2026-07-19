//! The `runnerbench` subcommand: the production flow runner's claim LOOP as a
//! gate (wamn-fqg.8 [5.14]).
//!
//! fqg.4's `failoverbench` drives the guest `run-next` export DIRECTLY (a
//! gate-local `Worker`), proving the claim/park/heartbeat path. fqg.8 adds the
//! long-lived SERVICE around it — [`wamn_run_worker::RunWorker`]: one
//! flowrunner instance, a `drain` that pulls every currently-claimable run, and
//! the doorbell + backoff serve loop. This gate drives THAT production struct
//! (SR1: the gate exercises the identical host code the binary runs) against an
//! ephemeral schema seeded the way the dispatcher seeds it (write-ahead
//! `dispatched` row + queue row), asserting the runner drains the queue to
//! completion — the local, repeatable, mutation-testable counterpart of the
//! in-cluster dispatcher→queue→runner live smoke.
//!
//! Assertions:
//!   * drain claims all N seeded runs, drives each to `completed`, empties the
//!     queue, and writes one `sink` row per run;
//!   * a second seed + drain on the SAME instance drains again (the serve loop
//!     reuses one instance across many wakes);
//!   * a drain of an empty queue claims nothing (the idle/backoff path's input);
//!   * ANTI-WEDGE (cjv.4): a never-terminating cyclic flow ends `failed` with
//!     `fail_kind = 'runaway-budget'` and DEQUEUES, and a run queued behind it
//!     still drains — the runner is provably not wedged. The phase runs under
//!     its own wall-clock timeout so a budget-removed mutant FAILS the gate
//!     instead of hanging it.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};
use wamn_run_queue::{enqueue_sql, write_ahead_triggered_run_sql};

use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wamn_run_worker::RunWorker;

/// The ephemeral schema unioning the flowrunner's flow tables with the 5.14
/// `run_queue`, provisioned via superuser (mirrors failoverbench).
const SCHEMA: &str = "wamn_runner_bench";
/// The single tenant + lease owner the seeded runs + the runner share.
const TENANT: &str = "runner-tenant";
const OWNER: &str = "runner-bench";
/// The seeded flow the claim path drives (read from the recorded `runs` row).
const FLOW_ID: &str = "poc-receipt";
/// The cjv.4 anti-wedge fixture: a permitted 2-node cycle with no exit
/// (`in → a → b → a → …`, pure transform nodes — no DB, no egress), so the
/// only thing that can end it is the engine's dispatch budget.
const RUNAWAY_FLOW_ID: &str = "runaway-loop";

fn runaway_flow_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{RUNAWAY_FLOW_ID}","version":1,
            "trigger":{{"type":"manual"}},"entry":"in",
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"a","type":"transform","config":{{"op":"upper"}}}},
              {{"id":"b","type":"transform","config":{{"op":"reverse"}}}}
            ],
            "edges":[{{"from":"in","to":"a"}},{{"from":"a","to":"b"}},
                     {{"from":"b","to":"a"}}]}}"#
    )
}

#[derive(Debug, Args)]
pub struct RunnerBenchArgs {
    /// The flowrunner guest (`flowrunner.wasm`) the runner instantiates + drives.
    #[arg(long)]
    pub flowrunner: PathBuf,

    /// App (runner) Postgres URL — the NOSUPERUSER wamn_app role. Overrides
    /// WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions/drops the ephemeral schema. wamn_app is
    /// NOSUPERUSER/NOCREATEDB, like production.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Runs seeded per drain.
    #[arg(long, default_value_t = 12)]
    pub iters: usize,

    /// Records seeded for the stream phase (fqg.18): one flow, many record-runs
    /// on one warm instance — the per-record dispatch cost the amortization work
    /// is judged by.
    #[arg(long, default_value_t = 200)]
    pub stream_records: usize,
}

/// The union DDL (identical shape to failoverbench): the flow tables the guest
/// walks + the 5.14 `run_queue` the runner claims from, schema-qualified with
/// the house tenant floor.
fn runner_ddl(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}.flows (\
            tenant_id text NOT NULL, flow_id text NOT NULL, version int NOT NULL, \
            active boolean NOT NULL DEFAULT false, graph_json jsonb NOT NULL, \
            PRIMARY KEY (tenant_id, flow_id, version));\
         ALTER TABLE {schema}.flows ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.flows FORCE ROW LEVEL SECURITY;\
         CREATE POLICY flows_tenant ON {schema}.flows \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.flows TO wamn_app;\
         CREATE TABLE {schema}.sink (\
            tenant_id text NOT NULL, run_id text NOT NULL, step int NOT NULL, \
            payload text NOT NULL, \
            CONSTRAINT sink_idem UNIQUE (tenant_id, run_id, step));\
         ALTER TABLE {schema}.sink ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.sink FORCE ROW LEVEL SECURITY;\
         CREATE POLICY sink_tenant ON {schema}.sink \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.sink TO wamn_app;\
         CREATE TABLE {schema}.runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
            flow_version int NOT NULL, \
            status text NOT NULL DEFAULT 'running' \
              CHECK (status IN ('dispatched','running','completed','failed','cancelled','infrastructure-failure')), \
            trigger_source text, input_json jsonb, result_json jsonb, state_json jsonb, \
            updated_at timestamptz NOT NULL DEFAULT now(), \
            idempotency_key text, replay_of text, root_run_id text, \
            fail_kind text, fail_node text, fail_reason text, \
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY runs_tenant ON {schema}.runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.runs TO wamn_app;\
         CREATE TABLE {schema}.node_runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, node_id text NOT NULL, \
            occurrence int NOT NULL DEFAULT 0, seq int NOT NULL, attempt int NOT NULL DEFAULT 0, \
            status text NOT NULL, output_port text, output_json jsonb, input_json jsonb, \
            error_kind text, error_detail jsonb, resume_at timestamptz, \
            PRIMARY KEY (tenant_id, run_id, node_id, occurrence), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         ALTER TABLE {schema}.node_runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.node_runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY node_runs_tenant ON {schema}.node_runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.node_runs TO wamn_app;\
         CREATE TABLE {schema}.run_queue (\
            tenant_id text NOT NULL, run_id text NOT NULL, partition_key text, \
            priority int NOT NULL DEFAULT 0, available_at timestamptz NOT NULL DEFAULT now(), \
            lease_owner text, lease_expires_at timestamptz, \
            attempts int NOT NULL DEFAULT 0, max_attempts int NOT NULL DEFAULT 20, \
            enqueued_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         CREATE INDEX run_queue_claimable ON {schema}.run_queue (tenant_id, available_at, lease_expires_at);\
         ALTER TABLE {schema}.run_queue ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_queue FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_queue_tenant ON {schema}.run_queue \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.run_queue TO wamn_app;"
    )
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect for ephemeral schema")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; CREATE SCHEMA {SCHEMA} AUTHORIZATION postgres; GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app;"
            ))
            .await
            .context("create ephemeral schema")?;
        client
            .batch_execute(&runner_ddl(SCHEMA))
            .await
            .context("apply runner DDL")?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn teardown(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let r = client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;"))
        .await
        .map_err(|e| anyhow::anyhow!("drop ephemeral schema: {e}"));
    drop(client);
    let _ = conn_task.await;
    r.map(|_| ())
}

/// A wamn_app connection pinned to the ephemeral schema + tenant claim — the same
/// RLS floor + search_path the runner's plugin session runs under, so the seeder
/// and the runner see each other's rows.
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

/// Seed a run the way the DISPATCHER does: the write-ahead `dispatched` row +
/// the queue row, co-transacted — the exact producer state the runner claims.
async fn seed_run(client: &mut Client, run_id: &str) -> anyhow::Result<()> {
    seed_flow_run(client, run_id, FLOW_ID).await
}

async fn seed_flow_run(client: &mut Client, run_id: &str, flow_id: &str) -> anyhow::Result<()> {
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

pub async fn run(args: RunnerBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("failed to read {}", args.flowrunner.display()))?;
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args.admin_database_url.clone().context(
        "runnerbench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;
    let n = args.iters;

    println!("# wamn-gates fqg.8 runnerbench (schema {SCHEMA}, tenant {TENANT}, owner {OWNER})");
    provision(&admin_url)
        .await
        .context("provision ephemeral schema")?;

    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.clone());
    let plugin = Arc::new(WamnPostgres::new(cfg)?);

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

        // Build the PRODUCTION runner struct (not a gate-local worker): this is the
        // exact instantiate + claim loop the `run-worker` binary runs. The vault
        // is EMPTY (no fixture here declares a credential; credproof gates the
        // vault path) but must be present — the guest imports it unconditionally.
        let vault = Arc::new(wamn_host::plugins::wamn_credentials::WamnCredentials::empty());
        let mut worker = RunWorker::instantiate(
            &engine,
            &guest,
            plugin.clone(),
            vault,
            wamn_run_worker::RunnerIdentity {
                owner: OWNER,
                tenant: TENANT,
                schema: Some(SCHEMA),
                project: "default",
            },
            std::sync::Arc::from([]), // no egress fixtures: deny-all
            30_000,
        )
        .await?;

        let queued = format!("SELECT count(*) FROM {SCHEMA}.run_queue");
        let completed =
            format!("SELECT count(*) FROM {SCHEMA}.runs WHERE status = 'completed'");
        let sinks = format!("SELECT count(*) FROM {SCHEMA}.sink");

        // --- (1) drain N seeded runs, each driven exactly once to completion ---
        for i in 0..n {
            seed_run(&mut seed_conn, &format!("rb-{i}")).await?;
        }
        let r1 = worker.drain().await?;
        let q1 = count(&seed_conn, &queued).await?;
        let done1 = count(&seed_conn, &completed).await?;
        let sink1 = count(&seed_conn, &sinks).await?;
        let drain1 = r1.claimed == n
            && r1.completed == n
            && q1 == 0
            && done1 as usize == n
            && sink1 as usize == n;
        println!(
            "\n## drain — claimed {}/{n}, completed {}, queue drained = {} (rows={q1}), runs completed = {done1}, sinks = {sink1} -> {drain1}",
            r1.claimed, r1.completed, q1 == 0
        );

        // --- (2) re-seed + drain on the SAME instance (the serve loop reuses it) ---
        for i in n..(2 * n) {
            seed_run(&mut seed_conn, &format!("rb-{i}")).await?;
        }
        let r2 = worker.drain().await?;
        let q2 = count(&seed_conn, &queued).await?;
        let done2 = count(&seed_conn, &completed).await?;
        let reuse = r2.claimed == n && r2.completed == n && q2 == 0 && done2 as usize == 2 * n;
        println!(
            "## reuse — second drain on one instance claimed {}/{n}, completed {}, queue drained = {} (rows={q2}), total completed = {done2} -> {reuse}",
            r2.claimed, r2.completed, q2 == 0
        );

        // --- (3) drain an empty queue: claims nothing (the idle/backoff input) ---
        let r3 = worker.drain().await?;
        let empty = r3.claimed == 0 && !r3.found_work();
        println!(
            "## empty — drain of an empty queue claimed {} (found_work = {}) -> {empty}",
            r3.claimed,
            r3.found_work()
        );

        // --- (4) ANTI-WEDGE (cjv.4): a runaway cyclic run fails + dequeues and
        // the run queued behind it still drains — the runner is not wedged. ---
        wamn_gate_harness::seed_flow_version(
            &seed_conn,
            TENANT,
            RUNAWAY_FLOW_ID,
            1,
            true,
            &runaway_flow_json(),
            true,
        )
        .await?;
        // The runaway run first (earlier available_at → claimed first), then a
        // normal run stuck behind it.
        seed_flow_run(&mut seed_conn, "rw-loop", RUNAWAY_FLOW_ID).await?;
        seed_run(&mut seed_conn, "rw-after").await?;
        // The gate's own wall guard: with the engine budget in force the drain
        // ends in seconds (10k dispatches, DB round trips dominating); a
        // budget-removed mutant spins forever and FAILS here instead of
        // hanging the harness.
        let r4 = tokio::time::timeout(std::time::Duration::from_secs(180), worker.drain())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "anti-wedge FAIL: drain did not terminate within 180s — the runaway run wedged the runner"
                )
            })??;
        let q4 = count(&seed_conn, &queued).await?;
        let verdict: (String, Option<String>) = {
            let row = seed_conn
                .query_one(
                    &format!(
                        "SELECT status, fail_kind FROM {SCHEMA}.runs WHERE run_id = 'rw-loop'"
                    ),
                    &[],
                )
                .await?;
            (row.get(0), row.get(1))
        };
        let after_done = count(
            &seed_conn,
            &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE run_id = 'rw-after' AND status = 'completed'"),
        )
        .await?;
        let runaway = r4.claimed == 2
            && r4.failed == 1
            && r4.completed == 1
            && q4 == 0
            && verdict.0 == "failed"
            && verdict.1.as_deref() == Some("runaway-budget")
            && after_done == 1;
        println!(
            "## runaway — claimed {}/2, runaway run = {}/{} (want failed/runaway-budget), \
             queue drained = {} (rows={q4}), run behind it completed = {} -> {runaway}",
            r4.claimed,
            verdict.0,
            verdict.1.as_deref().unwrap_or("<null>"),
            q4 == 0,
            after_done == 1
        );

        // --- (5) RECORD STREAM (fqg.18): many records = many runs of ONE flow
        // on one warm instance. Correctness: every record completes exactly
        // once with a full per-record node_runs trail and the v1 sink witness.
        // Measurement: wall clock per record on this substrate — the relative
        // number the amortization mechanisms are judged by.
        let m = args.stream_records;
        for i in 0..m {
            seed_run(&mut seed_conn, &format!("st-{i}")).await?;
        }
        let t0 = std::time::Instant::now();
        let r5 = worker.drain().await?;
        let stream_elapsed = t0.elapsed();
        let q5 = count(&seed_conn, &queued).await?;
        let st_done = count(
            &seed_conn,
            &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE run_id LIKE 'st-%' AND status = 'completed'"),
        )
        .await?;
        // Per-record node_runs trail: every record carries the same, complete
        // trail (uniformity pinned against the first record's count).
        let per_record: i64 = count(
            &seed_conn,
            &format!("SELECT count(*) FROM {SCHEMA}.node_runs WHERE run_id = 'st-0'"),
        )
        .await?;
        let st_nodes = count(
            &seed_conn,
            &format!("SELECT count(*) FROM {SCHEMA}.node_runs WHERE run_id LIKE 'st-%'"),
        )
        .await?;
        // Sink witness: v1 is the `upper` transform — every record's sink row
        // must carry it (also pins exactly-once: one sink row per record).
        let st_sinks_v1 = count(
            &seed_conn,
            &format!("SELECT count(*) FROM {SCHEMA}.sink WHERE run_id LIKE 'st-%' AND payload = 'RECEIPT'"),
        )
        .await?;
        let per_ms = stream_elapsed.as_secs_f64() * 1000.0 / m.max(1) as f64;
        let stream_ok = r5.claimed == m
            && r5.completed == m
            && q5 == 0
            && st_done as usize == m
            && per_record >= 3
            && st_nodes == per_record * m as i64
            && st_sinks_v1 as usize == m;
        println!(
            "## stream — {m} records in {:.2}s ({per_ms:.2} ms/record), completed {st_done}/{m}, \
             node_runs {st_nodes} ({per_record}/record), v1 sinks {st_sinks_v1}/{m}, \
             queue drained = {} -> {stream_ok}",
            stream_elapsed.as_secs_f64(),
            q5 == 0
        );

        // --- (6) HOT-RELOAD MID-STREAM (fqg.18): activate v2 (the `reverse`
        // transform), stream more records — they must run v2. This is the
        // load-bearing guard on the plan cache: keyed by the ACTIVE version,
        // a version flip must invalidate it.
        wamn_gate_harness::seed_flow_version(
            &seed_conn,
            TENANT,
            FLOW_ID,
            2,
            true,
            &crate::flowbench::flow_json(2),
            true,
        )
        .await?;
        wamn_gate_harness::set_active_flow_version(&seed_conn, TENANT, FLOW_ID, 2).await?;
        let m2 = (m / 4).max(8);
        for i in 0..m2 {
            seed_run(&mut seed_conn, &format!("sv-{i}")).await?;
        }
        let r6 = worker.drain().await?;
        let sv_sinks_v2 = count(
            &seed_conn,
            &format!("SELECT count(*) FROM {SCHEMA}.sink WHERE run_id LIKE 'sv-%' AND payload = 'tpiecer'"),
        )
        .await?;
        let reload_ok = r6.claimed == m2 && r6.completed == m2 && sv_sinks_v2 as usize == m2;
        // Restore v1 active so a re-run of the binary starts from the same state.
        wamn_gate_harness::set_active_flow_version(&seed_conn, TENANT, FLOW_ID, 1).await?;
        println!(
            "## stream-reload — v2 activated mid-stream: {}/{m2} records ran v2 (reverse sinks {sv_sinks_v2}) -> {reload_ok}",
            r6.completed
        );

        anyhow::Ok(drain1 && reuse && empty && runaway && stream_ok && reload_ok)
    }
    .await;

    ticker.abort();
    let _ = teardown(&admin_url).await;
    let pass = outcome?;

    println!("\nrunnerbench complete — overall PASS: {pass}");
    if !pass {
        bail!("runnerbench gate failed");
    }
    Ok(())
}
