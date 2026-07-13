//! The `dispatchbench` subcommand: the 5.14 shared-trigger-dispatcher gates
//! (docs/run-queue.md § Trigger dispatcher).
//!
//! Pure host-side like queuebench (no wasm guest — the dispatcher fires runs
//! into the queue; driving them is the runner's job, regression-covered by
//! flowbench/testhostbench). The gate provisions TWO ephemeral schemas as two
//! projects through the superuser URL and drives the REAL
//! [`wamn_host::dispatch::Dispatcher`] engine with **stepped time** — the trigger
//! decisions take an injected `now`, so a nightly cron and a three-day outage
//! are gated in milliseconds with no wall-clock waits (the 11.1
//! fast-forwardable-cron discipline). Only the wake and live modes touch real
//! time (sub-second), because `available_at` is a server-side instant.
//!
//! Modes:
//!   cron     — a nightly (F3-shaped) schedule fires exactly once per due tick:
//!              not early, once within a tick's second, no duplicate across a
//!              dispatcher RESTART (the anchor is recovered from the run ids),
//!              misfire collapse after a multi-day outage, first-sight
//!              bootstrap, and the fire's write-ahead + enqueue co-transaction
//!              proven atomic by an enqueue-side trap.
//!   outbox   — pending rows fire one run per (matching flow × row) with the
//!              payload persisted as the run input; unmatched rows are
//!              consumed; a version-SKEWED flow's rows are HELD, not consumed;
//!              bad registry rows (junk/webhook) never wedge the sweep; a
//!              redelivered row dedupes on the deterministic id WITHOUT
//!              resurrecting a completed run's queue row; and the poll/fire/ack
//!              co-transaction is proven atomic by traps on BOTH sides (the
//!              ack-side trap kills the fire-first split-txn mutant, the
//!              fire-side trap kills the ack-first lost-event mutant).
//!   race     — TWO live dispatchers over one project, ticking concurrently:
//!              every cron tick and every outbox row still fires exactly once,
//!              with contention PROVEN (losing attempts are counted and both
//!              replicas must win work — an inert second replica fails).
//!   fairness — two projects, one with a deep outbox backlog: per-sweep work is
//!              batch-bounded AND oldest-first (the backlog cannot monopolize a
//!              sweep or starve its own head), the quiet project's triggers
//!              fire in its own first sweep, and the adaptive intervals
//!              tighten/decay per project independently (no herd).
//!   wake     — a parked run (future available_at) is doorbell-hinted only once
//!              due; a firing's hint carries the WON run id and arrives only
//!              after its transaction committed (needs NATS; skipped under
//!              --mode all when absent).
//!   live     — the real `dispatch` run loop (real clock): an outbox insert
//!              fires sub-500ms BESIDE a permanently failing project
//!              (isolation), survives its DB connections being killed
//!              (reconnect), and a cron tick under a fixed 5s interval still
//!              fires within ~1s (cron-aware sleep).
//!   all      — every mode in sequence.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::{Client, NoTls};
use wamn_run_queue::{enqueue_sql, mint_cron_run_id, outbox_insert_sql, write_ahead_run_sql};

use wamn_host::dispatch::{Dispatcher, DispatcherConfig, ProjectSpec};

const SCHEMA_A: &str = "wamn_dispatch_a";
const TENANT_A: &str = "dispatch-a";
const SCHEMA_B: &str = "wamn_dispatch_b";
const TENANT_B: &str = "dispatch-b";
/// 2026-01-01 00:00:00 UTC — the virtual epoch the stepped ticks start from.
const BASE_MS: i64 = 1_767_225_600_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Cron,
    Outbox,
    Race,
    Fairness,
    Wake,
    Live,
    All,
}

#[derive(Debug, Args)]
pub struct DispatchBenchArgs {
    /// App (dispatcher) Postgres URL — the NOSUPERUSER wamn_app role.
    /// Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions/drops the two ephemeral project schemas.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// NATS URL for the wake/live doorbell hints.
    #[arg(long, default_value = "nats://localhost:4222")]
    pub nats_url: String,

    /// mTLS material for the NATS connection (in-cluster operator NATS).
    #[arg(long)]
    pub nats_tls_ca: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_cert: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_key: Option<PathBuf>,

    /// Which gate to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,
}

/// The ephemeral project schema: flows (the trigger registry), runs (the 5.7
/// shape incl. trigger_source/input_json — the write-ahead target), run_queue,
/// and the outbox — self-contained stand-ins for the production DDL so the gate
/// never touches a shared schema.
fn dispatch_ddl(schema: &str) -> String {
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
         CREATE TABLE {schema}.runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
            flow_version int NOT NULL, \
            status text NOT NULL DEFAULT 'running' \
              CHECK (status IN ('dispatched','running','completed','failed','cancelled','infrastructure-failure')), \
            trigger_source text, input_json jsonb, result_json jsonb, state_json jsonb, \
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY runs_tenant ON {schema}.runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.runs TO wamn_app;\
         CREATE TABLE {schema}.run_queue (\
            tenant_id text NOT NULL, run_id text NOT NULL, partition_key text, \
            priority int NOT NULL DEFAULT 0, available_at timestamptz NOT NULL DEFAULT now(), \
            lease_owner text, lease_expires_at timestamptz, \
            attempts int NOT NULL DEFAULT 0, max_attempts int NOT NULL DEFAULT 20, \
            enqueued_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         CREATE INDEX {schema}_claimable ON {schema}.run_queue (tenant_id, available_at, lease_expires_at);\
         ALTER TABLE {schema}.run_queue ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_queue FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_queue_tenant ON {schema}.run_queue \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.run_queue TO wamn_app;\
         CREATE TABLE {schema}.outbox (\
            tenant_id text NOT NULL, seq bigint GENERATED ALWAYS AS IDENTITY, \
            table_name text NOT NULL, \
            event text NOT NULL CHECK (event IN ('insert', 'update', 'delete')), \
            payload jsonb, created_at timestamptz NOT NULL DEFAULT now(), \
            dispatched_at timestamptz, \
            PRIMARY KEY (tenant_id, seq));\
         CREATE INDEX {schema}_outbox_pending ON {schema}.outbox (tenant_id, seq) \
            WHERE dispatched_at IS NULL;\
         ALTER TABLE {schema}.outbox ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.outbox FORCE ROW LEVEL SECURITY;\
         CREATE POLICY outbox_tenant ON {schema}.outbox \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.outbox TO wamn_app;"
    )
}

async fn admin_exec(admin_url: &str, sql: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let r = client
        .batch_execute(sql)
        .await
        .map_err(|e| anyhow::anyhow!("admin exec: {e}"));
    drop(client);
    let _ = conn_task.await;
    r
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    for schema in [SCHEMA_A, SCHEMA_B] {
        admin_exec(
            admin_url,
            &format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; \
                 CREATE SCHEMA {schema} AUTHORIZATION postgres; \
                 GRANT USAGE ON SCHEMA {schema} TO wamn_app;"
            ),
        )
        .await?;
        admin_exec(admin_url, &dispatch_ddl(schema)).await?;
    }
    Ok(())
}

async fn teardown(admin_url: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!(
            "DROP SCHEMA IF EXISTS {SCHEMA_A} CASCADE; DROP SCHEMA IF EXISTS {SCHEMA_B} CASCADE;"
        ),
    )
    .await
}

/// Clean slate for one project schema: runs (CASCADEs to run_queue), the outbox
/// (identity restarted so per-phase seqs are deterministic), and the registry.
async fn reset(admin_url: &str, schema: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!(
            "TRUNCATE {schema}.runs CASCADE; \
             TRUNCATE {schema}.outbox RESTART IDENTITY; \
             TRUNCATE {schema}.flows;"
        ),
    )
    .await
}

/// A wamn_app session pinned to one project (schema + tenant claim) — the gate's
/// seeding/asserting counterpart of the dispatcher's own project connection.
async fn connect_app(
    app_url: &str,
    schema: &str,
    tenant: &str,
) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(app_url, NoTls)
        .await
        .context("app (wamn_app) connect")?;
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute(&format!(
            "SET search_path TO {schema}; SET app.tenant TO '{tenant}';"
        ))
        .await
        .context("set search_path + tenant claim")?;
    Ok((client, handle))
}

fn spec(name: &str, app_url: &str, schema: &str, tenant: &str) -> ProjectSpec {
    ProjectSpec {
        name: name.to_string(),
        url: app_url.to_string(),
        tenant: tenant.to_string(),
        schema: Some(schema.to_string()),
    }
}

fn cron_flow_json(flow_id: &str, schedule: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1", "flow-id": flow_id, "version": 1,
        "trigger": {"type": "cron", "schedule": schedule},
        "entry": "n1", "nodes": [{"id": "n1", "type": "noop"}],
    })
    .to_string()
}

fn row_event_flow_json(flow_id: &str, table: &str, event: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1", "flow-id": flow_id, "version": 1,
        "trigger": {"type": "row-event", "table": table, "event": event},
        "entry": "n1", "nodes": [{"id": "n1", "type": "noop"}],
    })
    .to_string()
}

async fn seed_flow(client: &Client, flow_id: &str, graph_json: &str) -> anyhow::Result<()> {
    client
        .execute(
            "INSERT INTO flows (tenant_id, flow_id, version, active, graph_json) \
             VALUES (current_setting('app.tenant', true), $1, 1, true, $2::text::jsonb)",
            &[&flow_id, &graph_json],
        )
        .await?;
    Ok(())
}

async fn insert_outbox(
    client: &Client,
    table: &str,
    event: &str,
    payload: Option<&str>,
) -> anyhow::Result<()> {
    client
        .execute(&outbox_insert_sql(), &[&table, &event, &payload])
        .await?;
    Ok(())
}

async fn scalar_i64(client: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(client.query_one(sql, &[]).await?.get(0))
}

pub async fn run(args: DispatchBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args.admin_database_url.clone().context(
        "dispatchbench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;

    println!(
        "# wamn-host 5.14 dispatchbench (projects {SCHEMA_A}/{TENANT_A} + {SCHEMA_B}/{TENANT_B})"
    );
    provision(&admin_url)
        .await
        .context("provision ephemeral project schemas")?;

    let run_all = args.mode == Mode::All;
    let mut pass = true;
    let outcome = async {
        if run_all || args.mode == Mode::Cron {
            pass &= cron_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Outbox {
            pass &= outbox_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Race {
            pass &= race_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Fairness {
            pass &= fairness_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Wake {
            pass &= wake_phase(&app_url, &admin_url, &args, args.mode == Mode::Wake).await?;
        }
        if run_all || args.mode == Mode::Live {
            pass &= live_phase(&app_url, &admin_url, &args).await?;
        }
        anyhow::Ok(())
    }
    .await;

    // Always drop the ephemeral schemas, even on a phase error.
    let _ = teardown(&admin_url).await;
    outcome?;

    println!("\ndispatchbench complete — overall PASS: {pass}");
    if !pass {
        bail!("one or more 5.14 dispatcher gates failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// cron: exactly one run per due tick — not early, restart-safe, misfire collapse
// ---------------------------------------------------------------------------

async fn cron_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!("\n## cron — nightly (F3 shape) fires exactly once per due tick, stepped time");
    reset(admin_url, SCHEMA_A).await?;
    let (seeder, _h) = connect_app(app_url, SCHEMA_A, TENANT_A).await?;
    seed_flow(&seeder, "nightly", &cron_flow_json("nightly", "0 2 * * *")).await?;

    let specs = [spec("a", app_url, SCHEMA_A, TENANT_A)];
    let mut d = Dispatcher::connect(&specs, None, DispatcherConfig::default()).await?;

    // Before the tick: nothing fires.
    let early = d.tick_project(0, BASE_MS + HOUR).await?;
    let not_early = early.cron_fired.is_empty();

    // Within the tick's second: fires once, with the canonical tick identity.
    let tick = BASE_MS + 2 * HOUR;
    let fired = d.tick_project(0, tick + 300).await?;
    let expected_id = mint_cron_run_id("nightly", tick);
    let fired_once = fired.cron_fired == [expected_id.clone()];
    let row = seeder
        .query_one(
            "SELECT status, trigger_source, (input_json->>'fire-at-ms')::bigint AS fire_at, \
                    (SELECT count(*) FROM run_queue WHERE run_id = $1) AS queued \
               FROM runs WHERE run_id = $1",
            &[&expected_id],
        )
        .await?;
    let persisted = row.get::<_, String>("status") == "dispatched"
        && row.get::<_, Option<String>>("trigger_source").as_deref() == Some("cron")
        && row.get::<_, Option<i64>>("fire_at") == Some(tick)
        && row.get::<_, i64>("queued") == 1;

    // A later sweep in the same tick window: no re-fire (cached anchor).
    let again = d.tick_project(0, tick + 800).await?;
    let no_refire = again.cron_fired.is_empty();

    // RESTART: a fresh dispatcher (empty caches) recovers the anchor from the
    // run ids themselves and does not duplicate the tick.
    let mut d2 = Dispatcher::connect(&specs, None, DispatcherConfig::default()).await?;
    let restarted = d2.tick_project(0, tick + 900).await?;
    let restart_no_dup = restarted.cron_fired.is_empty();

    // Misfire collapse: a three-day outage fires exactly ONE more run (the
    // latest missed tick), not a burst.
    let outage_now = BASE_MS + 3 * DAY + 12 * HOUR;
    let latest_tick = BASE_MS + 3 * DAY + 2 * HOUR;
    let collapsed = d2.tick_project(0, outage_now).await?;
    let collapse_once = collapsed.cron_fired == [mint_cron_run_id("nightly", latest_tick)];
    let nightly_total = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE flow_id = 'nightly'",
    )
    .await?;

    // Bootstrap: a NEWLY seeded cron flow starts firing from dispatcher-sight —
    // no retroactive tick at first sight, the next boundary fires once.
    seed_flow(&seeder, "hourly", &cron_flow_json("hourly", "0 * * * *")).await?;
    let sight = outage_now + 30 * 60_000; // 12:30
    let at_sight = d2.tick_project(0, sight).await?;
    let no_retro = !at_sight.cron_fired.iter().any(|id| id.contains("hourly"));
    let next_hour = BASE_MS + 3 * DAY + 13 * HOUR;
    let after_boundary = d2.tick_project(0, next_hour + 100).await?;
    let bootstrap_fires = after_boundary.cron_fired == [mint_cron_run_id("hourly", next_hour)];
    let hourly_total = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE flow_id = 'hourly'",
    )
    .await?;

    // Fire co-transaction atomicity through the REAL fire(): arm a trap that
    // makes the ENQUEUE raise (a BEFORE INSERT trigger on run_queue), make the
    // next hourly tick due — the tick errors and the write-ahead is retracted
    // with the failed enqueue (one transaction). Without that, the anchor
    // recovery would count the tick as fired while no queue row exists: a
    // silently lost tick. The anchor is unchanged on failure, so the SAME tick
    // re-fires exactly once when the trap is gone.
    admin_exec(
        admin_url,
        &format!(
            "CREATE FUNCTION {SCHEMA_A}.enq_trap() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN RAISE EXCEPTION 'enqueue trap'; END $$; \
             CREATE TRIGGER cron_enq_trap BEFORE INSERT ON {SCHEMA_A}.run_queue \
             FOR EACH ROW EXECUTE FUNCTION {SCHEMA_A}.enq_trap();"
        ),
    )
    .await?;
    let trap_tick = BASE_MS + 3 * DAY + 14 * HOUR;
    let trapped = d2.tick_project(0, trap_tick + 100).await;
    let cron_tick_failed = trapped.is_err();
    let retracted = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE flow_id = 'hourly'",
    )
    .await?
        == 1; // the trapped tick's write-ahead rolled back with the enqueue
    admin_exec(
        admin_url,
        &format!(
            "DROP TRIGGER cron_enq_trap ON {SCHEMA_A}.run_queue; DROP FUNCTION {SCHEMA_A}.enq_trap();"
        ),
    )
    .await?;
    let retried = d2.tick_project(0, trap_tick + 300).await?;
    let cron_refired = retried.cron_fired == [mint_cron_run_id("hourly", trap_tick)];

    let pass = not_early
        && fired_once
        && persisted
        && no_refire
        && restart_no_dup
        && collapse_once
        && nightly_total == 2
        && no_retro
        && bootstrap_fires
        && hourly_total == 1
        && cron_tick_failed
        && retracted
        && cron_refired;
    println!(
        "not_early={not_early} fired_once={fired_once} persisted={persisted} no_refire={no_refire} \
         restart_no_dup={restart_no_dup} collapse_once={collapse_once} nightly_total={nightly_total} \
         bootstrap(no_retro={no_retro}, fires={bootstrap_fires}, total={hourly_total}) \
         atomicity(tick_failed={cron_tick_failed}, retracted={retracted}, refired={cron_refired})"
    );
    println!("PASS(cron exactly-once per tick + fire co-txn atomicity): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// outbox: one run per (matching flow x row), payload persisted, redelivery dedupes
// ---------------------------------------------------------------------------

async fn outbox_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    const N: i64 = 5;
    println!(
        "\n## outbox — {N} rows × 2 matching flows fire once each; unmatched consumed; \
         skew + id-mismatch held; redelivery + ghost dedupe; co-txn traps both ways"
    );
    reset(admin_url, SCHEMA_A).await?;
    let (seeder, _h) = connect_app(app_url, SCHEMA_A, TENANT_A).await?;
    seed_flow(
        &seeder,
        "disposition-recorded",
        &row_event_flow_json("disposition-recorded", "dispositions", "insert"),
    )
    .await?;
    seed_flow(
        &seeder,
        "disposition-audit",
        &row_event_flow_json("disposition-audit", "dispositions", "insert"),
    )
    .await?;
    // Poisoned registry rows that must be SKIPPED, never wedge the sweep:
    // junk (unparseable, no extractable trigger — holds nothing), a webhook
    // flow (not the dispatcher's), and a version-SKEWED row-event flow (parses
    // as JSON, rejected by validate) whose (skewed, insert) events must be
    // HELD — pending, not consumed — until the flow/binary is fixed.
    seed_flow(&seeder, "junk-flow", "{\"not\": \"a flow\"}").await?;
    seed_flow(
        &seeder,
        "webhook-flow",
        &serde_json::json!({
            "schema-version": "0.1", "flow-id": "webhook-flow", "version": 1,
            "trigger": {"type": "webhook", "sync": true},
            "entry": "n1", "nodes": [{"id": "n1", "type": "noop"}],
        })
        .to_string(),
    )
    .await?;
    seed_flow(
        &seeder,
        "skew-flow",
        &serde_json::json!({
            "schema-version": "9.9", "flow-id": "skew-flow", "version": 1,
            "trigger": {"type": "row-event", "table": "skewed", "event": "insert"},
            "entry": "n1", "nodes": [{"id": "n1", "type": "noop"}],
        })
        .to_string(),
    )
    .await?;
    // An ID-MISMATCHED row (column flow_id != graph flow-id): the graph
    // validates fine — both ids are legal slugs — but run ids are minted from
    // the COLUMN, which the slug rule never saw, so the dispatcher must treat
    // the mismatch as invalid: skipped, its (mismatched, insert) events HELD.
    seed_flow(
        &seeder,
        "mismatch-col",
        &row_event_flow_json("mismatch-graph", "mismatched", "insert"),
    )
    .await?;

    // Seqs 1..=5 match both flows; 6 (unregistered table) and 7 (unregistered
    // event) match nothing and must still be consumed; 8 belongs to the SKEWED
    // flow and must be held.
    for i in 1..=N {
        let payload = format!("{{\"id\": \"d-{i}\"}}");
        insert_outbox(&seeder, "dispositions", "insert", Some(&payload)).await?;
    }
    insert_outbox(&seeder, "unregistered", "insert", None).await?;
    insert_outbox(&seeder, "dispositions", "update", None).await?;
    insert_outbox(&seeder, "skewed", "insert", Some("{\"id\": \"held-1\"}")).await?;

    let specs = [spec("a", app_url, SCHEMA_A, TENANT_A)];
    let mut d = Dispatcher::connect(&specs, None, DispatcherConfig::default()).await?;
    let report = d.tick_project(0, BASE_MS).await?;

    let fired = report.outbox_fired.len() as i64 == 2 * N;
    let runs_total = scalar_i64(&seeder, "SELECT count(*) FROM runs").await?;
    let payload_kept = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs \
          WHERE trigger_source = 'outbox:3' \
            AND input_json->'payload'->>'id' = 'd-3' \
            AND input_json->>'table' = 'dispositions'",
    )
    .await?;
    let ids_deterministic = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE run_id = 'disposition-recorded:outbox:1' \
             OR run_id = 'disposition-audit:outbox:1'",
    )
    .await?;
    // Everything acked EXCEPT the held skew row, which stays pending with no run.
    let pending_after = scalar_i64(
        &seeder,
        "SELECT count(*) FROM outbox WHERE dispatched_at IS NULL",
    )
    .await?;
    let held_pending = scalar_i64(
        &seeder,
        "SELECT count(*) FROM outbox WHERE seq = 8 AND dispatched_at IS NULL",
    )
    .await?
        == 1
        && scalar_i64(
            &seeder,
            "SELECT count(*) FROM runs WHERE trigger_source = 'outbox:8'",
        )
        .await?
            == 0;

    // Redelivery (a lost ack / split-brain) + GHOST-DISPATCH check: complete +
    // dequeue seqs 1-2's runs (a fast runner finished them), then un-ack their
    // rows. The re-fire is a no-op on the deterministic ids AND must not
    // resurrect the completed runs' queue rows (the losing-enqueue guard —
    // an unconditional enqueue would insert fresh queue rows here).
    admin_exec(
        admin_url,
        &format!(
            "UPDATE {SCHEMA_A}.runs SET status = 'completed' \
              WHERE trigger_source IN ('outbox:1', 'outbox:2'); \
             DELETE FROM {SCHEMA_A}.run_queue \
              WHERE run_id IN (SELECT run_id FROM {SCHEMA_A}.runs \
                                WHERE trigger_source IN ('outbox:1', 'outbox:2')); \
             UPDATE {SCHEMA_A}.outbox SET dispatched_at = NULL WHERE seq <= 2;"
        ),
    )
    .await?;
    let redelivered = d.tick_project(0, BASE_MS + 1_000).await?;
    let no_dup = redelivered.outbox_fired.is_empty();
    let runs_after = scalar_i64(&seeder, "SELECT count(*) FROM runs").await?;
    let reacked = scalar_i64(
        &seeder,
        "SELECT count(*) FROM outbox WHERE seq <= 2 AND dispatched_at IS NULL",
    )
    .await?;
    let no_ghost = scalar_i64(
        &seeder,
        "SELECT count(*) FROM run_queue q JOIN runs r \
            ON r.tenant_id = q.tenant_id AND r.run_id = q.run_id \
          WHERE r.status = 'completed'",
    )
    .await?
        == 0;

    // Co-transaction atomicity, ACK side: arm a trap that makes the ack raise
    // (a BEFORE UPDATE trigger), land a fresh event, tick — the whole tick
    // errors and NOTHING lands: the fire is retracted with the failed ack
    // because poll + fire + ack are ONE transaction. A dispatcher that
    // committed the fire in its own transaction before acking would leave the
    // run behind here (the fire-first split-txn mutant).
    insert_outbox(
        &seeder,
        "dispositions",
        "insert",
        Some("{\"id\": \"d-trap\"}"),
    )
    .await?; // seq 9
    admin_exec(
        admin_url,
        &format!(
            "CREATE FUNCTION {SCHEMA_A}.ack_trap() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN RAISE EXCEPTION 'ack trap'; END $$; \
             CREATE TRIGGER outbox_ack_trap BEFORE UPDATE ON {SCHEMA_A}.outbox \
             FOR EACH ROW EXECUTE FUNCTION {SCHEMA_A}.ack_trap();"
        ),
    )
    .await?;
    let trapped = d.tick_project(0, BASE_MS + 2_000).await;
    let tick_failed = trapped.is_err();
    let trap_runs = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE run_id LIKE '%:outbox:9'",
    )
    .await?;
    let trap_pending = scalar_i64(
        &seeder,
        "SELECT count(*) FROM outbox WHERE seq = 9 AND dispatched_at IS NULL",
    )
    .await?;
    let no_half_state = trap_runs == 0 && trap_pending == 1;
    admin_exec(
        admin_url,
        &format!(
            "DROP TRIGGER outbox_ack_trap ON {SCHEMA_A}.outbox; DROP FUNCTION {SCHEMA_A}.ack_trap();"
        ),
    )
    .await?;
    let recovered = d.tick_project(0, BASE_MS + 3_000).await?;
    let trap_runs_after = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE run_id LIKE '%:outbox:9'",
    )
    .await?;
    let refired = recovered.outbox_fired.len() == 2 && trap_runs_after == 2;

    // Co-transaction atomicity, FIRE side: arm a trap that makes the
    // WRITE-AHEAD raise (a BEFORE INSERT trigger on runs), land a fresh event,
    // tick — the tick errors and the row is STILL PENDING: an ack-first
    // split-transaction mutant (the classic lost-event outbox bug: txn1
    // poll+ack commits, txn2 fire crashes) would have committed the ack and
    // silently lost the event here.
    insert_outbox(
        &seeder,
        "dispositions",
        "insert",
        Some("{\"id\": \"d-firetrap\"}"),
    )
    .await?; // seq 10
    admin_exec(
        admin_url,
        &format!(
            "CREATE FUNCTION {SCHEMA_A}.fire_trap() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN RAISE EXCEPTION 'fire trap'; END $$; \
             CREATE TRIGGER runs_fire_trap BEFORE INSERT ON {SCHEMA_A}.runs \
             FOR EACH ROW EXECUTE FUNCTION {SCHEMA_A}.fire_trap();"
        ),
    )
    .await?;
    let fire_trapped = d.tick_project(0, BASE_MS + 4_000).await;
    let fire_tick_failed = fire_trapped.is_err();
    let fire_trap_pending = scalar_i64(
        &seeder,
        "SELECT count(*) FROM outbox WHERE seq = 10 AND dispatched_at IS NULL",
    )
    .await?
        == 1;
    let fire_trap_runs = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE run_id LIKE '%:outbox:10'",
    )
    .await?
        == 0;
    admin_exec(
        admin_url,
        &format!(
            "DROP TRIGGER runs_fire_trap ON {SCHEMA_A}.runs; DROP FUNCTION {SCHEMA_A}.fire_trap();"
        ),
    )
    .await?;
    let fire_recovered = d.tick_project(0, BASE_MS + 5_000).await?;
    let fire_refired = fire_recovered.outbox_fired.len() == 2;

    // ID-MISMATCH hold: land an event for the mismatched flow's table — the
    // sweep must neither mint a run (under EITHER id) nor consume the row.
    insert_outbox(&seeder, "mismatched", "insert", Some("{\"id\": \"m-1\"}")).await?; // seq 11
    d.tick_project(0, BASE_MS + 6_000).await?;
    let mismatch_held = scalar_i64(
        &seeder,
        "SELECT count(*) FROM outbox WHERE seq = 11 AND dispatched_at IS NULL",
    )
    .await?
        == 1;
    let mismatch_no_run = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE run_id LIKE 'mismatch-%'",
    )
    .await?
        == 0;

    let pass = fired
        && runs_total == 2 * N
        && payload_kept == 2 // both flows carry row 3's payload
        && ids_deterministic == 2
        && pending_after == 1 // only the held skew row
        && held_pending
        && no_dup
        && runs_after == 2 * N
        && reacked == 0
        && no_ghost
        && tick_failed
        && no_half_state
        && refired
        && fire_tick_failed
        && fire_trap_pending
        && fire_trap_runs
        && fire_refired
        && mismatch_held
        && mismatch_no_run;
    println!(
        "fired={} runs={runs_total} payload_kept={payload_kept} deterministic_ids={ids_deterministic} \
         held(pending={held_pending}, total_pending={pending_after}) \
         redelivery(no_dup={no_dup}, runs_after={runs_after}, reacked={}, no_ghost={no_ghost}) \
         ack_trap(failed={tick_failed}, no_half_state={no_half_state}, refired={refired}) \
         fire_trap(failed={fire_tick_failed}, still_pending={fire_trap_pending}, no_run={fire_trap_runs}, refired={fire_refired}) \
         id_mismatch(held={mismatch_held}, no_run={mismatch_no_run})",
        report.outbox_fired.len(),
        reacked == 0
    );
    println!(
        "PASS(outbox fire + consume + skew-hold + id-mismatch-hold + ghost-guard + co-txn traps both ways): {pass}"
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// race: two live dispatchers over one project — exactly-once with no leader
// ---------------------------------------------------------------------------

async fn race_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    const ROWS: i64 = 40;
    println!("\n## race — two dispatchers tick concurrently: every tick + row fires exactly once");
    reset(admin_url, SCHEMA_A).await?;
    let (seeder, _h) = connect_app(app_url, SCHEMA_A, TENANT_A).await?;
    seed_flow(
        &seeder,
        "minutely",
        &cron_flow_json("minutely", "* * * * *"),
    )
    .await?;
    seed_flow(
        &seeder,
        "disposition-recorded",
        &row_event_flow_json("disposition-recorded", "dispositions", "insert"),
    )
    .await?;
    for i in 1..=ROWS {
        let payload = format!("{{\"id\": \"r-{i}\"}}");
        insert_outbox(&seeder, "dispositions", "insert", Some(&payload)).await?;
    }

    let specs = [spec("a", app_url, SCHEMA_A, TENANT_A)];
    let cfg = || DispatcherConfig {
        batch: 25, // < ROWS: neither replica can take the whole backlog alone
        ..DispatcherConfig::default()
    };
    let mut d1 = Dispatcher::connect(&specs, None, cfg()).await?;
    let mut d2 = Dispatcher::connect(&specs, None, cfg()).await?;

    // Three stepped minutes, both replicas ticking CONCURRENTLY at the same
    // instant. Round 0 bootstraps the cron anchor (first sight); rounds 1..3
    // each have one due minutely tick.
    let mut won_cron = 0usize;
    let mut lost_cron = 0usize;
    let (mut d1_outbox, mut d2_outbox) = (0usize, 0usize);
    for round in 0..4 {
        let now = BASE_MS + round * 60_000 + 250;
        let (r1, r2) = tokio::join!(d1.tick_project(0, now), d2.tick_project(0, now));
        let (r1, r2) = (r1?, r2?);
        won_cron += r1.cron_fired.len() + r2.cron_fired.len();
        lost_cron += r1.cron_lost + r2.cron_lost;
        d1_outbox += r1.outbox_fired.len();
        d2_outbox += r2.outbox_fired.len();
    }
    let won_outbox = d1_outbox + d2_outbox;

    let cron_runs = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE flow_id = 'minutely'",
    )
    .await?;
    let outbox_runs = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE trigger_source LIKE 'outbox:%'",
    )
    .await?;
    let distinct_outbox = scalar_i64(
        &seeder,
        "SELECT count(DISTINCT run_id) FROM runs WHERE trigger_source LIKE 'outbox:%'",
    )
    .await?;
    let pending = scalar_i64(
        &seeder,
        "SELECT count(*) FROM outbox WHERE dispatched_at IS NULL",
    )
    .await?;

    // Exactly-once = the number of firings that WON the insert equals the number
    // of distinct runs — a duplicate dispatch would inflate the win count; a
    // lost row would deflate completeness. CONTENTION is asserted, not assumed:
    // both replicas attempt rounds 1..3's cron tick, so exactly 3 attempts LOSE
    // (an inert second dispatcher would make lost_cron 0 and the race vacuous),
    // and with 40 rows against batch-25 polls BOTH replicas must win outbox
    // firings (SKIP LOCKED hands them disjoint batches).
    let pass = cron_runs == 3
        && won_cron == 3
        && lost_cron == 3
        && outbox_runs == ROWS
        && distinct_outbox == ROWS
        && won_outbox as i64 == ROWS
        && d1_outbox > 0
        && d2_outbox > 0
        && pending == 0;
    println!(
        "cron runs={cron_runs} (won {won_cron}/3, lost {lost_cron}/3) | outbox runs={outbox_runs} \
         distinct={distinct_outbox} (d1 {d1_outbox} + d2 {d2_outbox} = {won_outbox}/{ROWS}) | pending={pending}"
    );
    println!("PASS(replica race exactly-once, contention proven, no leader): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// fairness: a deep backlog is batch-bounded per sweep; the quiet project's
// triggers fire in its own first sweep; intervals adapt per project
// ---------------------------------------------------------------------------

async fn fairness_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    const BACKLOG: i64 = 120;
    const BATCH: usize = 50;
    println!(
        "\n## fairness — project A backlog {BACKLOG} (batch {BATCH}) must not starve project B"
    );
    reset(admin_url, SCHEMA_A).await?;
    reset(admin_url, SCHEMA_B).await?;
    let (seed_a, _ha) = connect_app(app_url, SCHEMA_A, TENANT_A).await?;
    let (seed_b, _hb) = connect_app(app_url, SCHEMA_B, TENANT_B).await?;

    seed_flow(
        &seed_a,
        "receipt-flow",
        &row_event_flow_json("receipt-flow", "receipts", "insert"),
    )
    .await?;
    for i in 1..=BACKLOG {
        let payload = format!("{{\"id\": \"a-{i}\"}}");
        insert_outbox(&seed_a, "receipts", "insert", Some(&payload)).await?;
    }
    seed_flow(
        &seed_b,
        "b-nightly",
        &cron_flow_json("b-nightly", "0 2 * * *"),
    )
    .await?;
    seed_flow(
        &seed_b,
        "b-flow",
        &row_event_flow_json("b-flow", "dispositions", "insert"),
    )
    .await?;
    insert_outbox(&seed_b, "dispositions", "insert", Some("{\"id\": \"b-1\"}")).await?;
    // Give B's cron a fired history (the previous nightly tick) so the stepped
    // tick at BASE+2h is due in B's FIRST sweep — and the anchor recovery from
    // seeded history is exercised.
    let prev_tick_id = mint_cron_run_id("b-nightly", BASE_MS - 22 * HOUR);
    admin_exec(
        admin_url,
        &format!(
            "INSERT INTO {SCHEMA_B}.runs (tenant_id, run_id, flow_id, flow_version, status, trigger_source) \
             VALUES ('{TENANT_B}', '{prev_tick_id}', 'b-nightly', 1, 'completed', 'cron');"
        ),
    )
    .await?;

    let specs = [
        spec("a", app_url, SCHEMA_A, TENANT_A),
        spec("b", app_url, SCHEMA_B, TENANT_B),
    ];
    let min = wamn_run_queue::DEFAULT_MIN_INTERVAL_MS;
    let mut d = Dispatcher::connect(
        &specs,
        None,
        DispatcherConfig {
            batch: BATCH,
            ..DispatcherConfig::default()
        },
    )
    .await?;

    // Simulate a healthy runner draining the queue between sweeps, so the
    // adaptive cadence reflects TRIGGER work, not an unclaimed-run backlog.
    let drain = |schema: &'static str| {
        let admin = admin_url.to_string();
        async move { admin_exec(&admin, &format!("DELETE FROM {schema}.run_queue;")).await }
    };

    // Sweep 1 (B's nightly tick is due): A fires a batch-bounded 50, B fires its
    // cron AND its outbox row in its own first sweep — not starved behind A.
    let t1 = BASE_MS + 2 * HOUR + 100;
    let a1 = d.tick_project(0, t1).await?;
    let b1 = d.tick_project(1, t1).await?;
    let a_bounded = a1.outbox_fired.len() == BATCH;
    // Oldest-first: the bounded first sweep took exactly the LOWEST 50 seqs
    // (a newest-first poll would take 71..=120 and starve the backlog's head).
    let a_seqs: Vec<i64> = a1
        .outbox_fired
        .iter()
        .filter_map(|id| id.rsplit(':').next()?.parse().ok())
        .collect();
    let a_oldest_first = a_seqs.iter().min() == Some(&1)
        && a_seqs.iter().max() == Some(&(BATCH as i64))
        && a_seqs.len() == BATCH;
    let b_first_sweep = b1.cron_fired.len() == 1 && b1.outbox_fired.len() == 1;
    drain(SCHEMA_A).await?;
    drain(SCHEMA_B).await?;

    // Sweep 2: A keeps draining at the tight interval; B is idle and decays.
    let t2 = t1 + min;
    let a2 = d.tick_project(0, t2).await?;
    let b2 = d.tick_project(1, t2).await?;
    drain(SCHEMA_A).await?;

    // Sweep 3: A finishes the backlog; B decays further.
    let t3 = t2 + min;
    let a3 = d.tick_project(0, t3).await?;
    let b3 = d.tick_project(1, t3).await?;
    drain(SCHEMA_A).await?;

    let a_total = a1.outbox_fired.len() + a2.outbox_fired.len() + a3.outbox_fired.len();
    let a_runs = scalar_i64(&seed_a, "SELECT count(*) FROM runs").await?;
    let a_distinct = scalar_i64(&seed_a, "SELECT count(DISTINCT run_id) FROM runs").await?;
    let b_idle = b2.outbox_fired.is_empty()
        && b2.cron_fired.is_empty()
        && b3.outbox_fired.is_empty()
        && b3.cron_fired.is_empty();
    // Independent per-project cadence: A stayed tight while working; B decayed
    // exponentially over its two idle sweeps (min -> 2min -> 4min).
    let a_interval = d.projects[0].interval_ms;
    let b_interval = d.projects[1].interval_ms;
    let cadence_ok = a_interval == min && b_interval == 4 * min;

    let pass = a_bounded
        && a_oldest_first
        && b_first_sweep
        && a_total as i64 == BACKLOG
        && a_runs == BACKLOG
        && a_distinct == BACKLOG
        && b_idle
        && cadence_ok;
    println!(
        "A: sweep1={} (bounded={a_bounded}, oldest_first={a_oldest_first}) total={a_total} runs={a_runs} \
         distinct={a_distinct} interval={a_interval} | \
         B: first_sweep={b_first_sweep} idle_after={b_idle} interval={b_interval}",
        a1.outbox_fired.len()
    );
    println!(
        "PASS(fairness: batch-bounded, oldest-first, no starvation, independent cadence): {pass}"
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// wake: a parked run is doorbell-hinted only once due
// ---------------------------------------------------------------------------

async fn wake_phase(
    app_url: &str,
    admin_url: &str,
    args: &DispatchBenchArgs,
    required: bool,
) -> anyhow::Result<bool> {
    use futures_util::StreamExt;
    use wash_runtime::washlet::{NatsConnectionOptions, connect_nats};

    println!("\n## wake — a parked run (future available_at) is hinted only once due");
    reset(admin_url, SCHEMA_A).await?;

    let nats_opts = NatsConnectionOptions {
        request_timeout: None,
        tls_ca: args.nats_tls_ca.clone(),
        tls_first: false,
        tls_cert: args.nats_tls_cert.clone(),
        tls_key: args.nats_tls_key.clone(),
    };
    let nats = match connect_nats(args.nats_url.clone(), nats_opts).await {
        Ok(c) => c,
        Err(e) => {
            if required {
                bail!("wake mode needs NATS at {}: {e}", args.nats_url);
            }
            println!("(skipping wake gate: no NATS at {} — {e})", args.nats_url);
            return Ok(true);
        }
    };

    let subject = format!("wamn.doorbell.{TENANT_A}");
    let mut subscription = nats.subscribe(subject.clone()).await?;
    nats.flush().await?;

    // Park a run 400ms out (a delay-node wake, D15 write-ahead + delayed enqueue).
    let (mut app, _h) = connect_app(app_url, SCHEMA_A, TENANT_A).await?;
    let delay_ms: i64 = 400;
    {
        let tx = app.transaction().await?;
        tx.execute(&write_ahead_run_sql(), &[&"parked-1", &"f", &1i32])
            .await?;
        tx.execute(
            &enqueue_sql(),
            &[&"parked-1", &Option::<&str>::None, &0i32, &delay_ms],
        )
        .await?;
        tx.commit().await?;
    }

    let specs = [spec("a", app_url, SCHEMA_A, TENANT_A)];
    let mut d = Dispatcher::connect(&specs, Some(nats), DispatcherConfig::default()).await?;

    // Still parked: no hint may arrive.
    let early = d.tick_project(0, wamn_host::dispatch::epoch_ms()).await?;
    let not_woken_early = early.woken.is_empty();
    let premature = tokio::time::timeout(Duration::from_millis(150), subscription.next()).await;
    let no_premature_hint = premature.is_err();

    // Once due (available_at is a server-side instant — this wait is real time),
    // the sweep hints the run.
    tokio::time::sleep(Duration::from_millis(delay_ms as u64 + 200)).await;
    let due = d.tick_project(0, wamn_host::dispatch::epoch_ms()).await?;
    let woken = due.woken == ["parked-1"];
    let hinted = match tokio::time::timeout(Duration::from_secs(5), subscription.next()).await {
        Ok(Some(msg)) => msg.payload.as_ref() == b"parked-1",
        _ => false,
    };

    // A FIRING's doorbell: an outbox event fires and the hint carries the WON
    // run id — with the run row already COMMITTED when the hint arrives
    // (publish strictly after commit: a hint for uncommitted work would wake a
    // runner into an empty claim). The parked run may be re-hinted by the same
    // sweep (duplicates are by design), so scan hints for the fired id.
    seed_flow(
        &app,
        "disposition-recorded",
        &row_event_flow_json("disposition-recorded", "dispositions", "insert"),
    )
    .await?;
    insert_outbox(&app, "dispositions", "insert", Some("{\"id\": \"wake-1\"}")).await?;
    let fired_tick = d.tick_project(0, wamn_host::dispatch::epoch_ms()).await?;
    let fired_id = fired_tick.outbox_fired.first().cloned().unwrap_or_default();
    let fire_hinted = !fired_id.is_empty();
    let mut got_fire_hint = false;
    let mut committed_at_hint = false;
    let hunt = Instant::now();
    while hunt.elapsed() < Duration::from_secs(5) {
        match tokio::time::timeout(Duration::from_secs(2), subscription.next()).await {
            Ok(Some(msg)) if msg.payload.as_ref() == fired_id.as_bytes() => {
                got_fire_hint = true;
                committed_at_hint = app
                    .query_one("SELECT count(*) FROM runs WHERE run_id = $1", &[&fired_id])
                    .await?
                    .get::<_, i64>(0)
                    == 1;
                break;
            }
            Ok(Some(_)) => continue, // a re-hinted parked run — skip
            _ => break,
        }
    }

    let pass = not_woken_early
        && no_premature_hint
        && woken
        && hinted
        && fire_hinted
        && got_fire_hint
        && committed_at_hint;
    println!(
        "not_woken_early={not_woken_early} no_premature_hint={no_premature_hint} woken={woken} hinted={hinted} \
         firing(hinted={got_fire_hint}, committed_at_hint={committed_at_hint})"
    );
    println!("PASS(parked-wake + firing doorbells, publish-after-commit): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// live: the real run loop picks up an outbox insert end-to-end
// ---------------------------------------------------------------------------

async fn live_phase(
    app_url: &str,
    admin_url: &str,
    args: &DispatchBenchArgs,
) -> anyhow::Result<bool> {
    use wash_runtime::washlet::{NatsConnectionOptions, connect_nats};

    println!(
        "\n## live — the real dispatch loop: sub-500ms fire beside a failing project, \
         reconnect after backend kill, cron-aware sleep"
    );
    reset(admin_url, SCHEMA_A).await?;
    let (seeder, _h) = connect_app(app_url, SCHEMA_A, TENANT_A).await?;
    seed_flow(
        &seeder,
        "disposition-recorded",
        &row_event_flow_json("disposition-recorded", "dispositions", "insert"),
    )
    .await?;

    let nats_opts = NatsConnectionOptions {
        request_timeout: None,
        tls_ca: args.nats_tls_ca.clone(),
        tls_first: false,
        tls_cert: args.nats_tls_cert.clone(),
        tls_key: args.nats_tls_key.clone(),
    };
    let nats = connect_nats(args.nats_url.clone(), nats_opts).await.ok();

    // Project "b-broken" points at a nonexistent schema: every one of its
    // sweeps fails. The healthy project's latency assertions below therefore
    // ALSO prove failing-project isolation — a loop that propagated or wedged
    // on B's errors could not serve A sub-500ms.
    let specs = [
        spec("a", app_url, SCHEMA_A, TENANT_A),
        spec("b-broken", app_url, "wamn_dispatch_missing", TENANT_B),
    ];
    let mut d = Dispatcher::connect(
        &specs,
        nats,
        DispatcherConfig {
            min_interval_ms: 50,
            max_interval_ms: 1_000,
            batch: 64,
        },
    )
    .await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let loop_task = tokio::spawn(async move { d.run_loop(shutdown_rx).await });

    // Give the loop one idle sweep, then land an event. The 500ms bound is what
    // makes the ADAPTIVE cadence load-bearing: a non-adaptive poller sleeping
    // the gate's max_interval (1s) cannot meet it (floor ~880ms), while the
    // tight-cadence loop measures ~200ms.
    tokio::time::sleep(Duration::from_millis(120)).await;
    insert_outbox(
        &seeder,
        "dispositions",
        "insert",
        Some("{\"id\": \"live-1\"}"),
    )
    .await?;
    let started = Instant::now();
    let mut fired = false;
    while started.elapsed() < Duration::from_secs(5) {
        let n = scalar_i64(
            &seeder,
            "SELECT count(*) FROM runs WHERE run_id = 'disposition-recorded:outbox:1'",
        )
        .await?;
        if n == 1 {
            fired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let latency = started.elapsed();
    let sub_500 = fired && latency < Duration::from_millis(500);

    // Reconnect: kill every wamn_app backend except the seeder's (a Postgres
    // restart from the dispatcher's point of view). The loop's next sweep hits
    // a dead client, fails, and the sweep after re-dials — the always-on
    // service must outlive its projects' databases.
    let seeder_pid: i32 = seeder
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    admin_exec(
        admin_url,
        &format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
              WHERE usename = 'wamn_app' AND pid <> {seeder_pid};"
        ),
    )
    .await?;
    insert_outbox(
        &seeder,
        "dispositions",
        "insert",
        Some("{\"id\": \"live-2\"}"),
    )
    .await?;
    let restarted = Instant::now();
    let mut refired = false;
    while restarted.elapsed() < Duration::from_secs(5) {
        let n = scalar_i64(
            &seeder,
            "SELECT count(*) FROM runs WHERE run_id = 'disposition-recorded:outbox:2'",
        )
        .await?;
        if n == 1 {
            refired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let reconnect_latency = restarted.elapsed();
    let _ = shutdown_tx.send(true);
    let _ = loop_task.await;

    // Cron-aware sleep: a fresh loop with a FIXED 5s interval and an
    // every-second (6-field) cron must still fire within ~1s of a tick — the
    // sleep computation wakes for the earliest cron fire, not just the next
    // sweep. A loop without the cron-aware wake first fires at ~5s.
    seed_flow(
        &seeder,
        "secondly",
        &cron_flow_json("secondly", "* * * * * *"),
    )
    .await?;
    let mut d2 = Dispatcher::connect(
        &[spec("a", app_url, SCHEMA_A, TENANT_A)],
        None,
        DispatcherConfig {
            min_interval_ms: 5_000,
            max_interval_ms: 5_000,
            batch: 64,
        },
    )
    .await?;
    let (shutdown2_tx, shutdown2_rx) = tokio::sync::watch::channel(false);
    let loop2 = tokio::spawn(async move { d2.run_loop(shutdown2_rx).await });
    let cron_started = Instant::now();
    let mut cron_aware = false;
    while cron_started.elapsed() < Duration::from_secs(3) {
        let n = scalar_i64(
            &seeder,
            "SELECT count(*) FROM runs WHERE flow_id = 'secondly'",
        )
        .await?;
        if n >= 1 {
            cron_aware = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let cron_latency = cron_started.elapsed();
    let _ = shutdown2_tx.send(true);
    let _ = loop2.await;

    let pass = fired && sub_500 && refired && cron_aware;
    println!(
        "fired={fired} latency={latency:?} (sub_500={sub_500}, beside a failing project) | \
         reconnect(refired={refired}, latency={reconnect_latency:?}) | \
         cron_aware={cron_aware} ({cron_latency:?} under a fixed 5s interval)"
    );
    println!(
        "PASS(live loop: adaptive sub-500ms + isolation + reconnect + cron-aware sleep): {pass}"
    );
    Ok(pass)
}
