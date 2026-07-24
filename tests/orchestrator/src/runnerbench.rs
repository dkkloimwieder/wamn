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
//!   * PARTITION-ORDER (fqg.9, wamn-7hja): PARTITIONED(key) runs seeded via
//!     `enqueue_with_policy_sql` across two keys with interleaved insertion
//!     dispatch per-key IN STREAM ORDER, one in flight per key, through the
//!     production `RunWorker::drain` — the keyed claim path failoverbench drives
//!     via the gate-local `Worker`, proven here through the long-lived runner.
//!     Dispatch order is read from a gate-local `sink.dispatch_seq` IDENTITY
//!     witness (execution order, not seed order).
//!   * MERGE-RESUME (wamn-03m/cjv.10/wamn-2jkm.42, R24): a diamond whose merge
//!     is a delay node parks between the merge's two visits; every re-claim
//!     reconstructs the partially-recorded merge, and each visit persists its
//!     OWN `node_runs` row (occurrence 0/1) — the per-visit occurrence proof
//!     through the production claim path.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};
use wamn_run_queue::{
    PartitionPolicy, enqueue_sql, enqueue_with_policy_sql, write_ahead_triggered_run_sql,
};

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

/// The wamn-03m/R24 merge-resume fixture: a diamond in -> {ba, bb} -> m -> r
/// whose MERGE is a `delay` node, so the claim path PARKS between the merge's
/// two visits — every re-claim reconstructs a partially-recorded merge from
/// `node_runs` through the production resume seam. Pre-R24 the second visit's
/// row was ON CONFLICT-dropped (occurrence hardcoded 0) and the history
/// collapsed; the phase asserts one row PER VISIT.
const MERGE_FLOW_ID: &str = "merge-resume";

fn merge_flow_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{MERGE_FLOW_ID}","version":1,
            "trigger":{{"type":"manual"}},"entry":"in",
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"ba","type":"transform","config":{{"op":"upper"}}}},
              {{"id":"bb","type":"transform","config":{{"op":"reverse"}}}},
              {{"id":"m","type":"delay","config":{{"delay-secs":1}}}},
              {{"id":"r","type":"respond"}}
            ],
            "edges":[{{"from":"in","to":"ba"}},{{"from":"in","to":"bb"}},
                     {{"from":"ba","to":"m"}},{{"from":"bb","to":"m"}},
                     {{"from":"m","to":"r"}}]}}"#
    )
}

/// A STRUCTURALLY DIFFERENT v2 of the merge-resume flow (wamn-cox): a bare linear
/// `in -> r`, no diamond and no delay-merge. Registered + activated MID-RUN while
/// mr-0 (stamped v1) is parked at its delay-merge. A resume that pins the run's
/// PERSISTED v1 reconstructs the recorded diamond node_runs against v1 and
/// completes; a resume that (wrongly) loaded the ACTIVE v2 would fold the
/// recorded `ba`/`bb`/`m` visits against a graph that has none — `Plan::resume`
/// dies `Mismatch`. So this v2, ignored, is the cox mutant detector.
fn merge_flow_v2_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{MERGE_FLOW_ID}","version":2,
            "trigger":{{"type":"manual"}},"entry":"in",
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"r","type":"respond"}}
            ],
            "edges":[{{"from":"in","to":"r"}}]}}"#
    )
}

/// The wamn-v8cv partition-terminal fixture: the single work node is a
/// `postgres-query`, whose dispatch dies `Terminal("capability-denied")` at the
/// standard-library grant check while the D8 raw-SQL flag is off (as it is in
/// this substrate and production) — a deterministic, one-step, no-I/O
/// GUEST-OBSERVED terminal business failure (no error edge, nothing crashed).
const TERMINAL_FLOW_ID: &str = "terminal-head";

fn terminal_flow_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{TERMINAL_FLOW_ID}","version":1,
            "trigger":{{"type":"manual"}},"entry":"in",
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"q","type":"postgres-query","config":{{}}}}
            ],
            "edges":[{{"from":"in","to":"q"}}]}}"#
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
/// walks + the 5.14 `run_queue` the runner claims from + the `partition_owner`
/// lease table the fqg.9 guest-side partitioned claim path leases against,
/// schema-qualified with the house tenant floor. Kept aligned with
/// `deploy/sql/run-queue.sql` by the drift guard in this module's tests.
// `pub(crate)` so the wamn-t92 testhostbench `runworker` mode drives the SAME
// drift-guarded union schema when it exercises the run-worker `--test-doubles` path.
pub(crate) fn runner_ddl(schema: &str) -> String {
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
            dispatch_seq bigint GENERATED ALWAYS AS IDENTITY, \
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
            preview_head text, payload_size bigint, payload_hash text, capture_mode text, \
            redacted boolean NOT NULL DEFAULT false, \
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
            partition_policy text NOT NULL DEFAULT 'blocking' \
              CHECK (partition_policy IN ('blocking','leapfrog')), \
            priority int NOT NULL DEFAULT 0, available_at timestamptz NOT NULL DEFAULT now(), \
            lease_owner text, lease_expires_at timestamptz, \
            attempts int NOT NULL DEFAULT 0, max_attempts int NOT NULL DEFAULT 20, \
            enqueued_at timestamptz NOT NULL DEFAULT now(), \
            stream_seq bigint NOT NULL DEFAULT 0, \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         CREATE INDEX run_queue_claimable ON {schema}.run_queue (tenant_id, available_at, stream_seq, lease_expires_at);\
         CREATE INDEX run_queue_partition ON {schema}.run_queue (tenant_id, partition_key) WHERE partition_key IS NOT NULL;\
         ALTER TABLE {schema}.run_queue ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_queue FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_queue_tenant ON {schema}.run_queue \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.run_queue TO wamn_app;\
         CREATE TABLE {schema}.partition_owner (\
            tenant_id text NOT NULL, partition_key text NOT NULL, \
            lease_owner text NOT NULL, lease_expires_at timestamptz NOT NULL, \
            acquired_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, partition_key));\
         ALTER TABLE {schema}.partition_owner ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.partition_owner FORCE ROW LEVEL SECURITY;\
         CREATE POLICY partition_owner_tenant ON {schema}.partition_owner \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.partition_owner TO wamn_app;\
         CREATE TABLE {schema}.run_dead_letters (\
            tenant_id text NOT NULL, run_id text NOT NULL, partition_key text NOT NULL, \
            flow_id text NOT NULL, reason text NOT NULL, \
            failed_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         ALTER TABLE {schema}.run_dead_letters ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_dead_letters FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_dead_letters_tenant ON {schema}.run_dead_letters \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT ON {schema}.run_dead_letters TO wamn_app;"
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
/// Defaults to flow_version 1 (the FLOW_ID fixture's only-registered version);
/// [`seed_flow_run`] takes an explicit version for phases that dispatch under a
/// non-default active version (wamn-cox: the run's stamped `flow_version` is the
/// version the guest resume pins to, so it must name a real flows row).
async fn seed_run(client: &mut Client, run_id: &str) -> anyhow::Result<()> {
    seed_flow_run(client, run_id, FLOW_ID, 1).await
}

async fn seed_flow_run(
    client: &mut Client,
    run_id: &str,
    flow_id: &str,
    version: i32,
) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(
        &write_ahead_triggered_run_sql(),
        &[&run_id, &flow_id, &version, &"cron", &"\"receipt\""],
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

/// Seed a PARTITIONED(key) run the way a keyed producer does (fqg.9): the
/// write-ahead `dispatched` runs row co-transacted with the queue row, but via
/// the D20 [`enqueue_with_policy_sql`] — the flow's declared head-unavailability
/// policy materialized onto the row (`blocking`: strict per-key stream order).
/// `enqueue_with_policy_sql` stamps `enqueued_at = now()` per seed txn and
/// `stream_seq = 0`, so the blocking head order `(enqueued_at, stream_seq,
/// run_id)` is seed order; the run ids are named so lexical order AGREES with
/// the intended stream order (the `run_id` tiebreak makes the phase
/// deterministic even if two seeds land in the same `now()` microsecond).
async fn seed_keyed_run(client: &mut Client, run_id: &str, key: &str) -> anyhow::Result<()> {
    seed_keyed_flow_run(client, run_id, FLOW_ID, key).await
}

async fn seed_keyed_flow_run(
    client: &mut Client,
    run_id: &str,
    flow_id: &str,
    key: &str,
) -> anyhow::Result<()> {
    let policy = PartitionPolicy::Blocking.as_sql();
    let tx = client.transaction().await?;
    tx.execute(
        &write_ahead_triggered_run_sql(),
        &[&run_id, &flow_id, &1i32, &"cron", &"\"receipt\""],
    )
    .await?;
    tx.execute(
        &enqueue_with_policy_sql(),
        &[&run_id, &Some(key), &0i32, &0i64, &policy],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn count(client: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(client.query_one(sql, &[]).await?.get(0))
}

/// The dispatch order of the keyed runs whose id starts with `prefix`, read from
/// the gate-local `sink.dispatch_seq` witness (a `GENERATED ALWAYS AS IDENTITY`
/// column the guest's explicit-column sink INSERT auto-populates). The sink row
/// is written DURING run execution (the `pg-write` node) and the production
/// `RunWorker::drain` claims one run at a time, so `dispatch_seq` order IS the
/// true per-key dispatch order — independent of seed order, which is what makes
/// this a real ordering witness rather than a tautology.
async fn dispatch_order(client: &Client, prefix: &str) -> anyhow::Result<Vec<String>> {
    let rows = client
        .query(
            &format!(
                "SELECT run_id FROM {SCHEMA}.sink WHERE run_id LIKE '{prefix}%' ORDER BY dispatch_seq"
            ),
            &[],
        )
        .await?;
    Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
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
            std::sync::Arc::from([]), // no egress fixtures: deny-all
            30_000,
            None, // wamn-t92: production host (no test doubles)
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
        seed_flow_run(&mut seed_conn, "rw-loop", RUNAWAY_FLOW_ID, 1).await?;
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
        // wamn-cox: stamp flow_version 2 on these runs (the version now active) —
        // the guest resume pins the run's PERSISTED version, so a run stamped v1
        // would (correctly) drive v1's `upper` and fail the v2 `reverse` witness.
        // This mirrors the real dispatcher, which stamps the active version at
        // write-ahead time.
        for i in 0..m2 {
            seed_flow_run(&mut seed_conn, &format!("sv-{i}"), FLOW_ID, 2).await?;
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

        // --- (7) PARTITION-ORDER (fqg.9): the production RunWorker drains
        // PARTITIONED(key) runs seeded via enqueue_with_policy_sql. runnerbench
        // otherwise only seeds UNpartitioned runs, so the fqg.9 guest-side keyed
        // claim path never rides its production drain (failoverbench drives it
        // via the gate-local Worker; this is the independent proof through the
        // long-lived RunWorker). Two keys are seeded with INTERLEAVED insertion
        // (ka0,kb0,ka1,kb1,ka2,kb2) so a runner that dropped per-key ordering
        // would interleave the sink witness; the assert is per-key IN-ORDER
        // dispatch (blocking policy = one in flight per key, head-first).
        for i in 0..3 {
            seed_keyed_run(&mut seed_conn, &format!("ka{i}"), "pk-a").await?;
            seed_keyed_run(&mut seed_conn, &format!("kb{i}"), "pk-b").await?;
        }
        // Two UNpartitioned (NULL-key) runs alongside — they drain via the
        // global claim, proving the two paths coexist on one drain.
        for i in 0..2 {
            seed_run(&mut seed_conn, &format!("kn{i}")).await?;
        }
        let r7 = worker.drain().await?;
        let order_a = dispatch_order(&seed_conn, "ka").await?;
        let order_b = dispatch_order(&seed_conn, "kb").await?;
        // The per-key ordering comparator: each key must dispatch in stream
        // order (== its seeded/lexical order). A runner that reordered a key —
        // or a comparator that accepted the wrong order — fails HERE.
        let expected_a = vec!["ka0".to_string(), "ka1".to_string(), "ka2".to_string()];
        let expected_b = vec!["kb0".to_string(), "kb1".to_string(), "kb2".to_string()];
        let a_ok = order_a == expected_a;
        let b_ok = order_b == expected_b;
        // One-in-flight-per-key: no keyed run drove twice (max 1 sink row each).
        let max_sink_keyed = count(
            &seed_conn,
            &format!(
                "SELECT COALESCE(MAX(c),0) FROM \
                 (SELECT count(*) c FROM {SCHEMA}.sink WHERE run_id LIKE 'k%' GROUP BY run_id) s"
            ),
        )
        .await?;
        // The partition path was actually engaged (a per-key ownership lease
        // was taken — proving this rode the fqg.9 keyed claim, not the global one).
        let leases = count(
            &seed_conn,
            &format!("SELECT count(*) FROM {SCHEMA}.partition_owner"),
        )
        .await?;
        let q7 = count(&seed_conn, &queued).await?;
        let partition_ok = r7.claimed == 8
            && r7.completed == 8
            && a_ok
            && b_ok
            && max_sink_keyed <= 1
            && leases >= 2
            && q7 == 0;
        println!(
            "## partition-order — 2 keys x 3 (interleaved) + 2 NULL-key via RunWorker::drain: \
             claimed {}/8 completed {}, key pk-a order {order_a:?} (want ka0,ka1,ka2) -> {a_ok}, \
             key pk-b order {order_b:?} (want kb0,kb1,kb2) -> {b_ok}, one-in-flight max sink/key={max_sink_keyed} (<=1), \
             partition leases taken={leases} (>=2), queue drained={} -> {partition_ok}",
            r7.claimed, r7.completed, q7 == 0
        );

        // --- (8) PARTITION-TERMINAL (wamn-v8cv, the D20 dead-letter + continue
        // decision): the HEAD of a blocking key fails TERMINALLY under the
        // runner's own eyes (a business failure, not a crash) -> the dequeue
        // lands the run_dead_letters marker in the SAME transaction and the key
        // CONTINUES in order — the runs behind it dispatch and complete. The
        // total-ledger-count assert doubles as the polarity proof: phase 4's
        // runaway run ALSO failed terminally, but UNPARTITIONED — it must have
        // written no marker.
        wamn_gate_harness::seed_flow_version(
            &seed_conn,
            TENANT,
            TERMINAL_FLOW_ID,
            1,
            true,
            &terminal_flow_json(),
            true,
        )
        .await?;
        // The failing head FIRST (earliest enqueued_at = the blocking stream
        // head), then two normal runs queued behind it on the same key.
        seed_keyed_flow_run(&mut seed_conn, "kt0", TERMINAL_FLOW_ID, "pk-t").await?;
        seed_keyed_run(&mut seed_conn, "kt1", "pk-t").await?;
        seed_keyed_run(&mut seed_conn, "kt2", "pk-t").await?;
        let r8 = worker.drain().await?;
        let q8 = count(&seed_conn, &queued).await?;
        let head_verdict: (String, Option<String>) = {
            let row = seed_conn
                .query_one(
                    &format!("SELECT status, fail_kind FROM {SCHEMA}.runs WHERE run_id = 'kt0'"),
                    &[],
                )
                .await?;
            (row.get(0), row.get(1))
        };
        // ONE marker in the whole ledger: kt0's — not rw-loop's (unpartitioned
        // terminal failures make no ordering promise).
        let dl_total = count(
            &seed_conn,
            &format!("SELECT count(*) FROM {SCHEMA}.run_dead_letters"),
        )
        .await?;
        let dl_marker: Option<(String, String, String)> = seed_conn
            .query_opt(
                &format!(
                    "SELECT partition_key, flow_id, reason FROM {SCHEMA}.run_dead_letters \
                     WHERE run_id = 'kt0' AND failed_at IS NOT NULL"
                ),
                &[],
            )
            .await?
            .map(|row| (row.get(0), row.get(1), row.get(2)));
        let marker_ok = matches!(
            &dl_marker,
            Some((key, flow, reason))
                if key == "pk-t" && flow == TERMINAL_FLOW_ID && reason.starts_with("terminal:")
        );
        // The key CONTINUED in order: the followers dispatched after the failed
        // head (kt0 has no pg-write node, so the sink witness carries only them).
        let order_t = dispatch_order(&seed_conn, "kt").await?;
        let followers_done = count(
            &seed_conn,
            &format!(
                "SELECT count(*) FROM {SCHEMA}.runs \
                 WHERE run_id IN ('kt1','kt2') AND status = 'completed'"
            ),
        )
        .await?;
        let partition_terminal_ok = r8.claimed == 3
            && r8.failed == 1
            && r8.completed == 2
            && head_verdict.0 == "failed"
            && head_verdict.1.as_deref() == Some("terminal")
            && dl_total == 1
            && marker_ok
            && order_t == vec!["kt1".to_string(), "kt2".to_string()]
            && followers_done == 2
            && q8 == 0;
        println!(
            "## partition-terminal — blocking head kt0 fails terminally via RunWorker::drain: \
             claimed {}/3 failed {} completed {}, head = {}/{} (want failed/terminal), \
             dead-letter rows = {dl_total} (want exactly 1 -> unpartitioned rw-loop wrote none), \
             marker {dl_marker:?} -> {marker_ok}, followers order {order_t:?} (want kt1,kt2), \
             followers completed = {followers_done}/2, queue drained = {} -> {partition_terminal_ok}",
            r8.claimed,
            r8.failed,
            r8.completed,
            head_verdict.0,
            head_verdict.1.as_deref().unwrap_or("<null>"),
            q8 == 0
        );

        // --- (9) MERGE-RESUME (wamn-03m / cjv.10 / wamn-2jkm.42, R24): a
        // diamond whose merge `m` is a delay node. The walk parks at m's FIRST
        // arrival, again between m's two visits, and each re-claim
        // RECONSTRUCTS the run from node_runs through the production resume
        // seam — after m's first visit is recorded, the replay folds a
        // partially-recorded merge (the kill-mid-D shape, via parks). The
        // occurrence fix is what makes this converge: each visit persists its
        // OWN row (m@0/m@1, r@0/r@1 — 7 rows total). Pre-R24 the second-visit
        // inserts were ON CONFLICT-dropped (5 rows), history collapsed, and a
        // later resume of such a run died Mismatch.
        wamn_gate_harness::seed_flow_version(
            &seed_conn,
            TENANT,
            MERGE_FLOW_ID,
            1,
            true,
            &merge_flow_json(),
            true,
        )
        .await?;
        seed_flow_run(&mut seed_conn, "mr-0", MERGE_FLOW_ID, 1).await?;
        let mut mr_claims = 0usize;
        let mr_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mr_status = format!("SELECT status FROM {SCHEMA}.runs WHERE run_id = 'mr-0'");

        // FIRST drain: mr-0 (v1) is claimed and parks at the delay-merge `m`, with
        // in/ba/bb already recorded under v1.
        let r0 = worker.drain().await?;
        mr_claims += r0.claimed;

        // wamn-cox LIVE PROOF (the cox mutant detector): mid-run — while mr-0 is
        // parked at `m` — register AND activate a STRUCTURALLY DIFFERENT v2
        // (linear in -> r). The resumed claims MUST keep driving the run's
        // PERSISTED v1: reconstruction folds the recorded diamond node_runs
        // against v1's graph and converges. Without the pin the resume would load
        // the now-ACTIVE v2 and `Plan::resume` dies Mismatch (recorded ba/bb/m
        // absent from v2), so the asserts below — completed, 7 rows, m/r visits
        // (2,0,1) — are the mutant detector.
        wamn_gate_harness::seed_flow_version(
            &seed_conn,
            TENANT,
            MERGE_FLOW_ID,
            2,
            true,
            &merge_flow_v2_json(),
            true,
        )
        .await?;
        wamn_gate_harness::set_active_flow_version(&seed_conn, TENANT, MERGE_FLOW_ID, 2).await?;

        loop {
            let status: String = seed_conn.query_one(&mr_status, &[]).await?.get(0);
            if status == "completed" {
                break;
            }
            if std::time::Instant::now() > mr_deadline {
                bail!("merge-resume FAIL: run mr-0 still '{status}' after 30s ({mr_claims} claims)");
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let r = worker.drain().await?;
            mr_claims += r.claimed;
        }
        // Restore v1 active so a re-run of the binary starts from the same state
        // (the phase-6 pattern).
        wamn_gate_harness::set_active_flow_version(&seed_conn, TENANT, MERGE_FLOW_ID, 1).await?;
        let mr_rows = count(
            &seed_conn,
            &format!("SELECT count(*) FROM {SCHEMA}.node_runs WHERE run_id = 'mr-0'"),
        )
        .await?;
        let mr_m: (i64, i32, i32) = {
            let row = seed_conn
                .query_one(
                    &format!(
                        "SELECT count(*), min(occurrence)::int, max(occurrence)::int \
                         FROM {SCHEMA}.node_runs WHERE run_id = 'mr-0' AND node_id = 'm'"
                    ),
                    &[],
                )
                .await?;
            (row.get(0), row.get(1), row.get(2))
        };
        let mr_r: (i64, i32, i32) = {
            let row = seed_conn
                .query_one(
                    &format!(
                        "SELECT count(*), min(occurrence)::int, max(occurrence)::int \
                         FROM {SCHEMA}.node_runs WHERE run_id = 'mr-0' AND node_id = 'r'"
                    ),
                    &[],
                )
                .await?;
            (row.get(0), row.get(1), row.get(2))
        };
        let q9 = count(&seed_conn, &queued).await?;
        // >= 3 claims: the fresh claim + at least one park-wake reclaim BEFORE
        // m's first record and one AFTER it — the latter is the replay of a
        // partially-recorded merge this phase exists to prove.
        let merge_resume_ok = mr_rows == 7
            && mr_m == (2, 0, 1)
            && mr_r == (2, 0, 1)
            && mr_claims >= 3
            && q9 == 0;
        println!(
            "## merge-resume — diamond with delay-merge via RunWorker::drain: completed after \
             {mr_claims} claims (>=3 -> parked mid-merge and resumed), node_runs rows = {mr_rows} \
             (want 7 — one PER VISIT), m visits = {mr_m:?} (want (2,0,1)), r visits = {mr_r:?} \
             (want (2,0,1)), queue drained = {} — and a structurally-different v2 (linear in->r) \
             was activated MID-RUN yet the pinned resume kept driving the run's persisted v1 \
             (wamn-cox; an active-version resume would die Mismatch) -> {merge_resume_ok}",
            q9 == 0
        );

        anyhow::Ok(
            drain1
                && reuse
                && empty
                && runaway
                && stream_ok
                && reload_ok
                && partition_ok
                && partition_terminal_ok
                && merge_resume_ok,
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_drift::{Need, assert_stand_in};

    /// wamn-9mg8 [GATE-DRIFT]: runnerbench's `run_queue` stand-in vs the schema of
    /// record, through the uniform guard (folds the wamn-nhjg/wamn-v8cv guard).
    /// run-next falls through to the fqg.9 guest-side partitioned claim path once
    /// the global queue drains, so `partition_owner` + the `run_queue_partition`
    /// index are Required; the guest's terminal settle names `run_dead_letters`
    /// unconditionally (wamn-v8cv), so the ledger is Required too.
    #[test]
    fn runnerbench_stand_in_tracks_run_queue_schema_of_record() {
        assert_stand_in(
            "runnerbench",
            &runner_ddl("wamn_run"),
            &[
                ("run_queue", Need::Required),
                ("partition_owner", Need::Required),
                ("run_dead_letters", Need::Required),
            ],
        );
    }
}
