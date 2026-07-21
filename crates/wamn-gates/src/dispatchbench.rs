//! The `dispatchbench` subcommand: the 5.14 shared-trigger-dispatcher gates
//! (docs/run-queue.md § Trigger dispatcher).
//!
//! Pure host-side like queuebench (no wasm guest — the dispatcher fires runs
//! into the queue; driving them is the runner's job, regression-covered by
//! flowbench/testhostbench). The gate provisions TWO ephemeral schemas as two
//! projects through the superuser URL and drives the REAL
//! [`wamn_dispatcher::Dispatcher`] engine with **stepped time** — the trigger
//! decisions take an injected `now`, so a nightly cron and a three-day outage
//! are gated in milliseconds with no wall-clock waits (the 11.1
//! fast-forwardable-cron discipline). Only the wake and live modes touch real
//! time (sub-second), because `available_at` is a server-side instant.
//!
//! Row events are NOT gated here since l5i9.19: the D19 v3 event plane (CDC
//! reader → JetStream → materializer) delivers them — matbench/streambench/
//! readerbench own that path. This gate covers what the dispatcher still owns:
//! cron and the parked-run wake.
//!
//! Modes:
//!   cron     — a nightly (F3-shaped) schedule fires exactly once per due tick:
//!              not early, once within a tick's second, no duplicate across a
//!              dispatcher RESTART (the anchor is recovered from the run ids),
//!              misfire collapse after a multi-day outage, first-sight
//!              bootstrap, and the fire's write-ahead + enqueue co-transaction
//!              proven atomic by an enqueue-side trap.
//!   ordering — the flow-level ordering declaration (5.11, wamn-fqg.20) is
//!              stamped onto run_queue.partition_key at fire(): an unordered
//!              flow's runs carry a NULL key (today's global claim), a strict
//!              flow's runs all carry the constant whole-flow key (the flow id),
//!              and a partitioned flow's runs carry the JMESPath result over the
//!              run input (here: a key over the cron envelope) — with a missing
//!              key degrading to the flow-wide stream (never NULL), so a
//!              partitioned flow never escapes to unordered. The flow's D20
//!              partition_policy is materialized coherently (wamn-kq0z).
//!   race     — TWO live dispatchers over one project, ticking concurrently:
//!              every cron tick still fires exactly once, with contention
//!              PROVEN (each tick's losing attempt is counted — an inert
//!              second replica fails).
//!   fairness — two projects, one with a deep due parked-run backlog: per-sweep
//!              wake hints are batch-bounded AND oldest-first (the backlog
//!              cannot monopolize a sweep or starve its own head), the quiet
//!              project's cron fires in its own first sweep, and the adaptive
//!              intervals tighten/decay per project independently (no herd).
//!   wake     — a parked run (future available_at) is doorbell-hinted only once
//!              due; a cron firing's hint carries the WON run id and arrives
//!              only after its transaction committed (needs NATS; skipped under
//!              --mode all when absent).
//!   live     — the real `dispatch` run loop (real clock): an every-second cron
//!              keeps firing BESIDE a permanently failing project (isolation),
//!              survives its DB connections being killed (reconnect), and a
//!              cron tick under a fixed 5s interval still fires within ~1s
//!              (cron-aware sleep).
//!   all      — every mode in sequence.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::{Client, NoTls};
use wamn_run_queue::{enqueue_sql, mint_cron_run_id, write_ahead_run_sql};

use wamn_dispatcher::{Dispatcher, DispatcherConfig, ProjectSpec};

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
    Ordering,
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
/// shape incl. trigger_source/input_json — the write-ahead target), and
/// run_queue — self-contained stand-ins for the production DDL so the gate
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
            partition_policy text NOT NULL DEFAULT 'blocking' \
              CHECK (partition_policy IN ('blocking', 'leapfrog')), \
            priority int NOT NULL DEFAULT 0, available_at timestamptz NOT NULL DEFAULT now(), \
            stream_seq bigint NOT NULL DEFAULT 0, \
            lease_owner text, lease_expires_at timestamptz, \
            attempts int NOT NULL DEFAULT 0, max_attempts int NOT NULL DEFAULT 20, \
            enqueued_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         CREATE INDEX {schema}_claimable ON {schema}.run_queue (tenant_id, available_at, stream_seq, lease_expires_at);\
         ALTER TABLE {schema}.run_queue ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_queue FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_queue_tenant ON {schema}.run_queue \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.run_queue TO wamn_app;"
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

/// Clean slate for one project schema: runs (CASCADEs to run_queue) and the
/// registry.
async fn reset(admin_url: &str, schema: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!(
            "TRUNCATE {schema}.runs CASCADE; \
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

async fn scalar_i64(client: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(client.query_one(sql, &[]).await?.get(0))
}

/// The `run_queue.partition_key` of one run (NULL = unordered), for the ordering
/// gate's per-run key assertions.
async fn partition_key_of(client: &Client, run_id: &str) -> anyhow::Result<Option<String>> {
    Ok(client
        .query_one(
            "SELECT partition_key FROM run_queue WHERE run_id = $1",
            &[&run_id],
        )
        .await?
        .get(0))
}

/// The `run_queue.partition_policy` of one run (NOT NULL, DB default 'blocking'),
/// for the ordering gate's per-run D20-policy assertions (wamn-kq0z).
async fn partition_policy_of(client: &Client, run_id: &str) -> anyhow::Result<String> {
    Ok(client
        .query_one(
            "SELECT partition_policy FROM run_queue WHERE run_id = $1",
            &[&run_id],
        )
        .await?
        .get(0))
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
        if run_all || args.mode == Mode::Ordering {
            pass &= ordering_phase(&app_url, &admin_url).await?;
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
// ordering: the flow-level ordering declaration is stamped onto
// run_queue.partition_key at fire() (wamn-fqg.20)
// ---------------------------------------------------------------------------

async fn ordering_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!(
        "\n## ordering — unordered→NULL key, strict→constant flow key, partitioned→JMESPath key \
         over the cron envelope (missing key falls back to the flow-wide stream, never NULL); \
         the flow's D20 partition_policy is materialized COHERENTLY (keyed rows carry the \
         declared policy, unordered rows keep the column default) — wamn-kq0z"
    );
    reset(admin_url, SCHEMA_A).await?;
    let (seeder, _h) = connect_app(app_url, SCHEMA_A, TENANT_A).await?;

    // Cron flows on the SAME nightly schedule, so ONE stepped tick fires all of
    // them — a per-flow key+policy assertion off a single sweep. The cron input
    // envelope is {"trigger":"cron","schedule":...,"fire-at-ms":...}: a
    // partitioned key of `schedule` evaluates to a scalar (the declared-key
    // case), while `payload.customer` is absent from a cron input (the
    // fallback case). `policy` (D20) is the field ABSENT for the default
    // (blocking); the leapfrog flow declares it so we can prove the declared
    // policy is stamped, not the column default (wamn-kq0z).
    const SCHEDULE: &str = "0 2 * * *";
    let ordered_flow = |flow_id: &str, ordering: serde_json::Value, policy: Option<&str>| {
        let mut graph = serde_json::json!({
            "schema-version": "0.1", "flow-id": flow_id, "version": 1,
            "trigger": {"type": "cron", "schedule": SCHEDULE},
            "entry": "n1", "nodes": [{"id": "n1", "type": "noop"}],
        });
        if !ordering.is_null() {
            graph["ordering"] = ordering;
        }
        if let Some(p) = policy {
            graph["partition-policy"] = serde_json::json!(p);
        }
        graph.to_string()
    };
    // unordered = the field absent (today's default).
    seed_flow(
        &seeder,
        "unordered-flow",
        &ordered_flow("unordered-flow", serde_json::Value::Null, None),
    )
    .await?;
    seed_flow(
        &seeder,
        "strict-flow",
        &ordered_flow("strict-flow", serde_json::json!({"mode": "strict"}), None),
    )
    .await?;
    seed_flow(
        &seeder,
        "partitioned-flow",
        &ordered_flow(
            "partitioned-flow",
            serde_json::json!({"mode": "partitioned", "partition-key": "schedule"}),
            None,
        ),
    )
    .await?;
    // A partitioned flow whose key is ABSENT from the cron envelope: the
    // flow-wide fallback. It ALSO declares leapfrog — its keyed rows must carry
    // 'leapfrog', not the column default (the exact wamn-kq0z regression).
    seed_flow(
        &seeder,
        "leapfrog-flow",
        &ordered_flow(
            "leapfrog-flow",
            serde_json::json!({"mode": "partitioned", "partition-key": "payload.customer"}),
            Some("leapfrog"),
        ),
    )
    .await?;

    let specs = [spec("a", app_url, SCHEMA_A, TENANT_A)];
    let mut d = Dispatcher::connect(&specs, None, DispatcherConfig::default()).await?;
    // Bootstrap sweep (first sight — nothing due), then the nightly tick.
    d.tick_project(0, BASE_MS + HOUR).await?;
    let tick = BASE_MS + 2 * HOUR;
    let report = d.tick_project(0, tick + 300).await?;
    let fired = report.cron_fired.len() == 4;
    let rid = |flow_id: &str| mint_cron_run_id(flow_id, tick);

    // Unordered: NULL key — byte-for-byte today's global-claim behavior.
    let unordered_null = partition_key_of(&seeder, &rid("unordered-flow"))
        .await?
        .is_none();
    // Strict: the constant whole-flow key (the flow id).
    let strict_constant = partition_key_of(&seeder, &rid("strict-flow"))
        .await?
        .as_deref()
        == Some("strict-flow");
    // Partitioned: the evaluated JMESPath result over the cron envelope.
    let partitioned_keyed = partition_key_of(&seeder, &rid("partitioned-flow"))
        .await?
        .as_deref()
        == Some(SCHEDULE);
    // Partitioned with a MISSING key: the flow-wide stream (flow id), never NULL.
    let partitioned_fallback = partition_key_of(&seeder, &rid("leapfrog-flow"))
        .await?
        .as_deref()
        == Some("leapfrog-flow");

    // D20 policy materialization (wamn-kq0z). Keyed rows of a DEFAULT-policy
    // flow carry 'blocking' (stamped via the policy enqueue), the leapfrog
    // flow's keyed rows carry 'leapfrog' (the fix — not the silent column
    // default), and unordered rows keep the column-default 'blocking' (NULL
    // key, today's plain enqueue).
    let strict_policy = partition_policy_of(&seeder, &rid("strict-flow")).await? == "blocking";
    let partitioned_policy =
        partition_policy_of(&seeder, &rid("partitioned-flow")).await? == "blocking";
    let leapfrog_policy = partition_policy_of(&seeder, &rid("leapfrog-flow")).await? == "leapfrog";
    let unordered_policy =
        partition_policy_of(&seeder, &rid("unordered-flow")).await? == "blocking";

    let pass = fired
        && unordered_null
        && strict_constant
        && partitioned_keyed
        && partitioned_fallback
        && strict_policy
        && partitioned_policy
        && leapfrog_policy
        && unordered_policy;
    println!(
        "fired={fired} unordered_null={unordered_null} strict_constant={strict_constant} \
         partitioned_keyed={partitioned_keyed} partitioned_fallback={partitioned_fallback} | \
         policy: strict={strict_policy} partitioned={partitioned_policy} \
         leapfrog={leapfrog_policy} unordered={unordered_policy}"
    );
    println!(
        "PASS(ordering: partition_key + D20 partition_policy stamped from the flow declaration): {pass}"
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// race: two live dispatchers over one project — exactly-once with no leader
// ---------------------------------------------------------------------------

async fn race_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!("\n## race — two dispatchers tick concurrently: every cron tick fires exactly once");
    reset(admin_url, SCHEMA_A).await?;
    let (seeder, _h) = connect_app(app_url, SCHEMA_A, TENANT_A).await?;
    seed_flow(
        &seeder,
        "minutely",
        &cron_flow_json("minutely", "* * * * *"),
    )
    .await?;

    let specs = [spec("a", app_url, SCHEMA_A, TENANT_A)];
    let mut d1 = Dispatcher::connect(&specs, None, DispatcherConfig::default()).await?;
    let mut d2 = Dispatcher::connect(&specs, None, DispatcherConfig::default()).await?;

    // Three stepped minutes, both replicas ticking CONCURRENTLY at the same
    // instant. Round 0 bootstraps the cron anchor (first sight); rounds 1..3
    // each have one due minutely tick.
    let mut won_cron = 0usize;
    let mut lost_cron = 0usize;
    for round in 0..4 {
        let now = BASE_MS + round * 60_000 + 250;
        let (r1, r2) = tokio::join!(d1.tick_project(0, now), d2.tick_project(0, now));
        let (r1, r2) = (r1?, r2?);
        won_cron += r1.cron_fired.len() + r2.cron_fired.len();
        lost_cron += r1.cron_lost + r2.cron_lost;
    }

    let cron_runs = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE flow_id = 'minutely'",
    )
    .await?;
    let queued = scalar_i64(&seeder, "SELECT count(*) FROM run_queue").await?;

    // Exactly-once = the number of firings that WON the insert equals the number
    // of distinct runs. CONTENTION is asserted, not assumed: both replicas
    // attempt rounds 1..3's cron tick, so exactly 3 attempts LOSE (an inert
    // second dispatcher would make lost_cron 0 and the race vacuous).
    let pass = cron_runs == 3 && won_cron == 3 && lost_cron == 3 && queued == 3;
    println!("cron runs={cron_runs} (won {won_cron}/3, lost {lost_cron}/3) queued={queued}");
    println!("PASS(replica race exactly-once, contention proven, no leader): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// fairness: a deep due parked-run backlog is batch-bounded per sweep; the quiet
// project's cron fires in its own first sweep; intervals adapt per project
// ---------------------------------------------------------------------------

async fn fairness_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    const BACKLOG: i64 = 120;
    const BATCH: usize = 50;
    println!(
        "\n## fairness — project A's due parked backlog {BACKLOG} (batch {BATCH}) must not starve project B"
    );
    reset(admin_url, SCHEMA_A).await?;
    reset(admin_url, SCHEMA_B).await?;
    let (mut seed_a, _ha) = connect_app(app_url, SCHEMA_A, TENANT_A).await?;
    let (seed_b, _hb) = connect_app(app_url, SCHEMA_B, TENANT_B).await?;

    // A: a deep DUE backlog (a scale-to-zero runner's unclaimed runs — every
    // sweep hints wake candidates, batch-bounded). One transaction, so every
    // row shares available_at and the oldest-first order is the run_id
    // tiebreak (zero-padded ids make it deterministic).
    {
        let tx = seed_a.transaction().await?;
        let wa = write_ahead_run_sql();
        let enq = enqueue_sql();
        for i in 1..=BACKLOG {
            let run_id = format!("p-{i:03}");
            tx.execute(&wa, &[&run_id, &"f", &1i32]).await?;
            tx.execute(&enq, &[&run_id, &Option::<&str>::None, &0i32, &0i64])
                .await?;
        }
        tx.commit().await?;
    }
    seed_flow(
        &seed_b,
        "b-nightly",
        &cron_flow_json("b-nightly", "0 2 * * *"),
    )
    .await?;
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

    // Simulate a healthy runner claiming + completing the hinted runs between
    // sweeps (dequeue), so each sweep's hint set is the backlog's NEXT slice
    // and the adaptive cadence reflects real progress.
    async fn drain(admin_url: &str, woken: &[String]) -> anyhow::Result<()> {
        if woken.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = woken.iter().map(|w| format!("'{w}'")).collect();
        admin_exec(
            admin_url,
            &format!(
                "DELETE FROM {SCHEMA_A}.run_queue WHERE run_id IN ({});",
                ids.join(",")
            ),
        )
        .await
    }

    // Sweep 1 (B's nightly tick is due): A hints a batch-bounded 50, B fires
    // its cron in its own first sweep — not starved behind A.
    let t1 = BASE_MS + 2 * HOUR + 100;
    let a1 = d.tick_project(0, t1).await?;
    let b1 = d.tick_project(1, t1).await?;
    let a_bounded = a1.woken.len() == BATCH;
    // Oldest-first: the bounded first sweep hinted exactly the LOWEST 50 ids
    // (a newest-first scan would starve the backlog's head).
    let expected: Vec<String> = (1..=BATCH as i64).map(|i| format!("p-{i:03}")).collect();
    let a_oldest_first = a1.woken == expected;
    let b_first_sweep = b1.cron_fired.len() == 1;
    drain(admin_url, &a1.woken).await?;
    // B's fired cron run would otherwise sit due in ITS queue and be
    // wake-hinted every sweep (work, pinning B's cadence tight) — complete it,
    // as a healthy runner would.
    admin_exec(admin_url, &format!("DELETE FROM {SCHEMA_B}.run_queue;")).await?;

    // Sweep 2: A keeps draining at the tight interval; B is idle and decays.
    let t2 = t1 + min;
    let a2 = d.tick_project(0, t2).await?;
    let b2 = d.tick_project(1, t2).await?;
    drain(admin_url, &a2.woken).await?;

    // Sweep 3: A finishes the backlog; B decays further.
    let t3 = t2 + min;
    let a3 = d.tick_project(0, t3).await?;
    let b3 = d.tick_project(1, t3).await?;
    drain(admin_url, &a3.woken).await?;

    let a_total = a1.woken.len() + a2.woken.len() + a3.woken.len();
    let queue_left = scalar_i64(&seed_a, "SELECT count(*) FROM run_queue").await?;
    let b_idle = b2.cron_fired.is_empty()
        && b3.cron_fired.is_empty()
        && b2.woken.is_empty()
        && b3.woken.is_empty();
    // Independent per-project cadence: A stayed tight while working; B decayed
    // exponentially over its two idle sweeps (min -> 2min -> 4min).
    let a_interval = d.projects[0].interval_ms;
    let b_interval = d.projects[1].interval_ms;
    let cadence_ok = a_interval == min && b_interval == 4 * min;

    let pass = a_bounded
        && a_oldest_first
        && b_first_sweep
        && a_total as i64 == BACKLOG
        && queue_left == 0
        && b_idle
        && cadence_ok;
    println!(
        "A: sweep1={} (bounded={a_bounded}, oldest_first={a_oldest_first}) total={a_total} \
         queue_left={queue_left} interval={a_interval} | \
         B: first_sweep={b_first_sweep} idle_after={b_idle} interval={b_interval}",
        a1.woken.len()
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

    // wamn-qeeg: a NATS server started seconds earlier can ACK a SUBSCRIBE yet not
    // yet route the first publish to that interest — the race that flaked wake once
    // in --mode all. `flush` guarantees the server processed our SUBs in order, not
    // that delivery is live, so prove the subscribe->publish->deliver path on THIS
    // connection with a bounded probe before the real hints fire: re-publish each
    // round (a publish that lands before interest propagates is simply dropped) and
    // wait a short slice for it back. The real doorbell SUB was flushed first, so
    // once the probe round-trips the real subscription is effective too.
    {
        let probe = format!("wamn.dispatchbench.probe.{}", std::process::id());
        let mut probe_sub = nats.subscribe(probe.clone()).await?;
        nats.flush().await?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut delivering = false;
        while Instant::now() < deadline {
            nats.publish(probe.clone(), b"1".to_vec().into()).await?;
            nats.flush().await?;
            if let Ok(Some(_)) =
                tokio::time::timeout(Duration::from_millis(100), probe_sub.next()).await
            {
                delivering = true;
                break;
            }
        }
        if !delivering {
            if required {
                bail!(
                    "wake mode: NATS at {} accepted the subscription but never delivered \
                     a probe within 3s (server-side interest not ready)",
                    args.nats_url
                );
            }
            println!(
                "(skipping wake gate: NATS at {} not delivering — probe round-trip timed out)",
                args.nats_url
            );
            return Ok(true);
        }
    }

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
    let early = d.tick_project(0, wamn_dispatcher::epoch_ms()).await?;
    let not_woken_early = early.woken.is_empty();
    let premature = tokio::time::timeout(Duration::from_millis(150), subscription.next()).await;
    let no_premature_hint = premature.is_err();

    // Once due (available_at is a server-side instant — this wait is real time),
    // the sweep hints the run.
    tokio::time::sleep(Duration::from_millis(delay_ms as u64 + 200)).await;
    let due = d.tick_project(0, wamn_dispatcher::epoch_ms()).await?;
    let woken = due.woken == ["parked-1"];
    let hinted = match tokio::time::timeout(Duration::from_secs(5), subscription.next()).await {
        Ok(Some(msg)) => msg.payload.as_ref() == b"parked-1",
        _ => false,
    };

    // A FIRING's doorbell: a cron tick fires and the hint carries the WON
    // run id — with the run row already COMMITTED when the hint arrives
    // (publish strictly after commit: a hint for uncommitted work would wake a
    // runner into an empty claim). The parked run may be re-hinted by the same
    // sweep (duplicates are by design), so scan hints for the fired id.
    seed_flow(&app, "secondly", &cron_flow_json("secondly", "* * * * * *")).await?;
    // Bootstrap sweep (first sight — no retroactive tick), then wait past a
    // second boundary so the next sweep fires it.
    d.tick_project(0, wamn_dispatcher::epoch_ms()).await?;
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let fired_tick = d.tick_project(0, wamn_dispatcher::epoch_ms()).await?;
    let fired_id = fired_tick.cron_fired.first().cloned().unwrap_or_default();
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
    let detail = format!(
        "not_woken_early={not_woken_early} no_premature_hint={no_premature_hint} woken={woken} hinted={hinted} \
         firing(hinted={got_fire_hint}, committed_at_hint={committed_at_hint})"
    );
    // wamn-qeeg: mark the failing sub-check unmissably so a future cold-NATS flake
    // is diagnosable from --mode all's log alone (the detail otherwise scrolls past
    // amid the other phases' output).
    if pass {
        println!("{detail}");
    } else {
        println!("WAKE FAILED — {detail}");
    }
    println!("PASS(parked-wake + firing doorbells, publish-after-commit): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// live: the real run loop keeps cron firing beside failure and across reconnect
// ---------------------------------------------------------------------------

async fn live_phase(
    app_url: &str,
    admin_url: &str,
    args: &DispatchBenchArgs,
) -> anyhow::Result<bool> {
    use wash_runtime::washlet::{NatsConnectionOptions, connect_nats};

    println!(
        "\n## live — the real dispatch loop: cron keeps firing beside a failing project, \
         reconnect after backend kill, cron-aware sleep"
    );
    reset(admin_url, SCHEMA_A).await?;
    let (seeder, _h) = connect_app(app_url, SCHEMA_A, TENANT_A).await?;
    seed_flow(
        &seeder,
        "secondly",
        &cron_flow_json("secondly", "* * * * * *"),
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
    // sweeps fails. The healthy project's firing assertions below therefore
    // ALSO prove failing-project isolation — a loop that propagated or wedged
    // on B's errors could not keep A's every-second cron firing.
    let specs = [
        spec("a", app_url, SCHEMA_A, TENANT_A),
        spec("b-broken", app_url, "wamn_dispatch_missing", TENANT_B),
    ];
    let mut d = Dispatcher::connect(
        &specs,
        nats,
        DispatcherConfig {
            cadence: wamn_run_queue::Cadence::new(50, 1_000).unwrap(),
            batch: 64,
        },
    )
    .await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let loop_task = tokio::spawn(async move { d.run_loop(shutdown_rx).await });

    // The every-second cron must fire within ~2s of loop start (first sight
    // anchors, the next second boundary fires) — beside the failing project.
    let started = Instant::now();
    let mut fired = false;
    while started.elapsed() < Duration::from_secs(5) {
        let n = scalar_i64(
            &seeder,
            "SELECT count(*) FROM runs WHERE flow_id = 'secondly'",
        )
        .await?;
        if n >= 1 {
            fired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let latency = started.elapsed();

    // Reconnect: kill every wamn_app backend except the seeder's (a Postgres
    // restart from the dispatcher's point of view). The loop's next sweep hits
    // a dead client, fails, and the sweep after re-dials — the always-on
    // service must outlive its projects' databases. Observable: the secondly
    // cron RESUMES minting new runs.
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
    let before_kill = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE flow_id = 'secondly'",
    )
    .await?;
    let restarted = Instant::now();
    let mut refired = false;
    while restarted.elapsed() < Duration::from_secs(8) {
        let n = scalar_i64(
            &seeder,
            "SELECT count(*) FROM runs WHERE flow_id = 'secondly'",
        )
        .await?;
        if n > before_kill {
            refired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let reconnect_latency = restarted.elapsed();
    let _ = shutdown_tx.send(true);
    let _ = loop_task.await;

    // Cron-aware sleep: a fresh loop with a FIXED 5s interval and the same
    // every-second (6-field) cron must still fire within ~1s of a tick — the
    // sleep computation wakes for the earliest cron fire, not just the next
    // sweep. A loop without the cron-aware wake first fires at ~5s. (A fresh
    // dispatcher recovers the anchor from the run ids, so the next SECOND
    // boundary is its first due tick.)
    let mut d2 = Dispatcher::connect(
        &[spec("a", app_url, SCHEMA_A, TENANT_A)],
        None,
        DispatcherConfig {
            cadence: wamn_run_queue::Cadence::new(5_000, 5_000).unwrap(),
            batch: 64,
        },
    )
    .await?;
    let before_aware = scalar_i64(
        &seeder,
        "SELECT count(*) FROM runs WHERE flow_id = 'secondly'",
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
        if n > before_aware {
            cron_aware = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let cron_latency = cron_started.elapsed();
    let _ = shutdown2_tx.send(true);
    let _ = loop2.await;

    let pass = fired && refired && cron_aware;
    println!(
        "fired={fired} latency={latency:?} (beside a failing project) | \
         reconnect(refired={refired}, latency={reconnect_latency:?}) | \
         cron_aware={cron_aware} ({cron_latency:?} under a fixed 5s interval)"
    );
    println!("PASS(live loop: cron beside failure + reconnect + cron-aware sleep): {pass}");
    Ok(pass)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_drift::{Need, assert_stand_in};

    /// wamn-9mg8 [GATE-DRIFT]: dispatchbench's `run_queue` stand-in vs the schema
    /// of record, through the uniform guard. The dispatcher enqueues + stamps
    /// `partition_key`/`partition_policy` and checks ordering, but never runs the
    /// per-partition claim path or a guest terminal settle — so `partition_owner`
    /// and `run_dead_letters` are AbsentByDesign, while every `run_queue` column
    /// (the c32ffaf `stream_seq` drift class) stays pinned.
    #[test]
    fn dispatchbench_stand_in_tracks_run_queue_schema_of_record() {
        assert_stand_in(
            "dispatchbench",
            &dispatch_ddl("wamn_run"),
            &[
                ("run_queue", Need::Required),
                ("partition_owner", Need::AbsentByDesign),
                ("run_dead_letters", Need::AbsentByDesign),
            ],
        );
    }
}
