//! The `failoverbench` subcommand: checkpoint/resume on replica loss as a
//! first-class failover primitive (wamn-fqg.2 [5.14]).
//!
//! 5.14's walking skeleton shipped the two halves of failover on opposite sides
//! of the host/guest split:
//!   * the run-queue lease + `claim_batch_sql` **reclaim** (a dead runner's lease
//!     expires and another replica re-claims the row, bumping `attempts`) — host
//!     side, proven by `queuebench`'s reclaim gate;
//!   * 5.7 branch-aware **reconstruction** (`wamn_run_store::reconstruct` +
//!     `Plan::resume` rebuild the exact outstanding frontier from `node_runs`, so a
//!     killed-then-resumed run leaves exactly one side effect) — guest side, proven
//!     by `flowbench`'s resume gate.
//!
//! This bench UNIONS them into the real thing: replica A claims a run, is
//! epoch-killed mid-effect (its `run_queue` lease still held on a separate DB
//! connection, never renewed); its lease expires; replica B **reclaims** the run
//! from the queue and drives the SAME flowrunner guest, which **reconstructs** and
//! completes with a single side effect, ending `completed` — never
//! `infrastructure-failure`. The guest is UNCHANGED: it needs only a `run_id`, is
//! queue-agnostic, and reconstructs from `node_runs`; the queue is a host-side path
//! (wiring the guest to claim its own work is the separate fqg.4).
//!
//! The failover work exposes — and this bench hardens + gates — a completion-vs-
//! janitor race: a run a replica reclaimed at its budget boundary
//! (`attempts == max_attempts`) whose fresh lease then lapses past grace could be
//! relabeled `infrastructure-failure` by the janitor in the window between the
//! completion write and the host's dequeue. The fix is host-side + in the pure
//! `wamn_run_queue` builder (the guest stays byte-identical): `janitor_sweep_sql`
//! only relabels a still-in-flight run (`status IN ('dispatched','running')`), and
//! the host dequeues strictly after completion. Both orderings of the race are
//! gated, each mutation-tested: the guard direction (completion, then the janitor)
//! by `failover` + `janitor-guard`, and the reverse (the janitor reaps a still-
//! running run, then the resume completes) by `reverse-race`.
//!
//! Gates:
//!   failover      — kill a claimant mid-effect, reclaim on another replica, resume
//!                   via reconstruction (proven by pg-write's `node_runs.seq` — the
//!                   completed prefix is skipped, not replayed): exactly one side
//!                   effect, run ends `completed`, and a janitor sweep fired in the
//!                   completion→dequeue window leaves it alone (the guard, on the
//!                   real reclaimed run).
//!   janitor-guard — a reclaimed-and-completed run with a stale expired+spent queue
//!                   row is NOT relabeled `infrastructure-failure`; a genuine
//!                   non-terminal orphan still is (the guard, deterministically).
//!   reverse-race  — the *other* ordering: the janitor reaps a still-`running`
//!                   reclaimed run to `infrastructure-failure` FIRST, then the
//!                   resume completes — and the guest's unconditional completion
//!                   write wins, so the run still ends `completed`. Pins the
//!                   completion-wins backstop the guard cannot provide.
//!   all           — all three, in sequence.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::{Client, NoTls};
use wamn_run_queue::{
    claim_batch_sql, dequeue_sql, enqueue_sql, janitor_sweep_sql, mark_running_sql,
    write_ahead_run_sql,
};
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{
    Component as WasmtimeComponent, InstancePre, Linker, TypedFunc,
};
use wash_runtime::wasmtime::{Engine as RawEngine, Store, Trap};

use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};

/// The ephemeral schema that unions the guest's flow tables (flows / runs /
/// node_runs / sink) with the 5.14 `run_queue`, provisioned via superuser.
const SCHEMA: &str = "wamn_failover_bench";
/// The single tenant + component identity the runner and the claimers share, so
/// the guest's plugin session and the host's raw claim connections see each
/// other's rows under one RLS/`search_path` scope.
const TENANT: &str = "failover-tenant";
/// The seeded flow the guest runs (its `run`/`run-until-kill` exports use it).
const FLOW_ID: &str = "poc-receipt";
/// Epoch deadline for the kill-window store (mirrors flowbench): generous enough
/// that the pre-kill DB work always completes so the busy-loop is reached, short
/// enough to kill it promptly (~600 ms at the 10 ms tick).
const KILL_TICKS: u64 = 60;
/// The janitor grace, in ms (1 hour) — the fixtures' leases expired 2 h ago.
const GRACE_MS: i64 = 3_600_000;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Failover,
    JanitorGuard,
    ReverseRace,
    All,
}

#[derive(Debug, Args)]
pub struct FailoverBenchArgs {
    /// The flowrunner guest (`flowrunner.wasm`) driven across the replica boundary.
    #[arg(long)]
    pub flowrunner: PathBuf,

    /// App (runner) Postgres URL — the NOSUPERUSER wamn_app role the guest and the
    /// claimers run under. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions/drops the ephemeral schema (the flow tables +
    /// run_queue). wamn_app is NOSUPERUSER/NOCREATEDB, like production.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Which gate to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Kill-reclaim-resume cycles for the failover gate.
    #[arg(long, default_value_t = 10)]
    pub iters: usize,

    /// Pool max size (passed to the plugin).
    #[arg(long, default_value_t = 8)]
    pub pool_max: usize,
}

// ---------------------------------------------------------------------------
// Ephemeral schema: the flow tables (guest) + run_queue (claimers), unioned.
// ---------------------------------------------------------------------------

/// The union DDL: the flowrunner guest's flow tables (mirrors testhostbench's
/// `template_ddl` / the s3 fixture — flows / flow_runs / sink / runs / node_runs)
/// PLUS the 5.14 `run_queue`, all schema-qualified with the house tenant floor.
/// `runs` carries the full status CHECK so the write-ahead `dispatched`, the guest's
/// `running`/`completed`, and the janitor's `infrastructure-failure` are all
/// validated; `run_queue` FK→`runs` ON DELETE CASCADE (a per-run reset cascades).
fn failover_ddl(schema: &str) -> String {
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
         CREATE TABLE {schema}.flow_runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
            flow_version int NOT NULL, step_seq int NOT NULL DEFAULT -1, \
            status text NOT NULL DEFAULT 'running', state_json jsonb, \
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.flow_runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.flow_runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY flow_runs_tenant ON {schema}.flow_runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.flow_runs TO wamn_app;\
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
            .batch_execute(&failover_ddl(SCHEMA))
            .await
            .context("apply failover DDL")?;
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

/// Clear the run/queue/sink tables (superuser) between phases. TRUNCATE runs
/// CASCADEs to node_runs + run_queue via the FKs; `flows` (no FK) survives so the
/// seeded flow persists.
async fn reset(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let r = client
        .batch_execute(&format!("TRUNCATE {SCHEMA}.runs, {SCHEMA}.sink CASCADE;"))
        .await
        .map_err(|e| anyhow::anyhow!("reset failover tables: {e}"));
    drop(client);
    let _ = conn_task.await;
    r.map(|_| ())
}

/// A wamn_app connection pinned to the ephemeral schema + tenant claim — the same
/// RLS floor + `search_path` the guest's plugin session runs under, so the raw
/// claimer and the guest see each other's rows.
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

/// The D15 write-ahead run row (`dispatched`, flow_id = the seeded flow) + the
/// queue row, co-transacted — exactly the production dispatch path.
async fn enqueue(client: &mut Client, run_id: &str) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(&write_ahead_run_sql(), &[&run_id, &FLOW_ID, &1i32])
        .await?;
    tx.execute(
        &enqueue_sql(),
        &[&run_id, &Option::<&str>::None, &0i32, &0i64],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The guest side (flowrunner Harness/Worker) — mirrors flowbench.
// ---------------------------------------------------------------------------

struct Worker {
    store: Store<SharedCtx>,
    seed: TypedFunc<(), (Result<u32, String>,)>,
    set_active: TypedFunc<(u32,), (Result<(), String>,)>,
    run: TypedFunc<(String, String), (Result<u32, String>,)>,
    run_until_kill: TypedFunc<(String, String), (Result<u32, String>,)>,
    sink_count: TypedFunc<(String,), (Result<u64, String>,)>,
    reset: TypedFunc<(String,), (Result<u64, String>,)>,
}

impl Worker {
    async fn call_seed(&mut self) -> anyhow::Result<u32> {
        let (r,) = self.seed.call_async(&mut self.store, ()).await?;
        r.map_err(|e| anyhow::anyhow!("seed: {e}"))
    }
    async fn call_set_active(&mut self, v: u32) -> anyhow::Result<()> {
        let (r,) = self.set_active.call_async(&mut self.store, (v,)).await?;
        r.map_err(|e| anyhow::anyhow!("set-active: {e}"))
    }
    async fn call_run(&mut self, run_id: &str, payload: &str) -> anyhow::Result<u32> {
        let (r,) = self
            .run
            .call_async(&mut self.store, (run_id.to_string(), payload.to_string()))
            .await?;
        r.map_err(|e| anyhow::anyhow!("run: {e}"))
    }
    /// Returns the raw call result so the caller can distinguish an epoch trap
    /// (expected) from an unexpected return.
    async fn call_run_until_kill(
        &mut self,
        run_id: &str,
        payload: &str,
    ) -> anyhow::Result<Result<u32, String>> {
        let (r,) = self
            .run_until_kill
            .call_async(&mut self.store, (run_id.to_string(), payload.to_string()))
            .await?;
        Ok(r)
    }
    async fn call_sink_count(&mut self, run_id: &str) -> anyhow::Result<u64> {
        let (r,) = self
            .sink_count
            .call_async(&mut self.store, (run_id.to_string(),))
            .await?;
        r.map_err(|e| anyhow::anyhow!("sink-count: {e}"))
    }
    async fn call_reset(&mut self, run_id: &str) -> anyhow::Result<u64> {
        let (r,) = self
            .reset
            .call_async(&mut self.store, (run_id.to_string(),))
            .await?;
        r.map_err(|e| anyhow::anyhow!("reset: {e}"))
    }
}

struct Harness {
    engine: wash_runtime::engine::Engine,
    pre: InstancePre<SharedCtx>,
    plugin: Arc<WamnPostgres>,
}

impl Harness {
    fn new(
        engine: wash_runtime::engine::Engine,
        guest: &[u8],
        plugin: Arc<WamnPostgres>,
    ) -> anyhow::Result<Self> {
        let raw: &RawEngine = engine.inner();
        let component = WasmtimeComponent::new(raw, guest)
            .map_err(|e| anyhow::anyhow!("compile flowrunner: {e}"))?;
        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
        wamn_postgres::add_to_linker(&mut linker)?;
        let pre = linker.instantiate_pre(&component)?;
        Ok(Self {
            engine,
            pre,
            plugin,
        })
    }

    fn plugin_map(
        &self,
    ) -> std::collections::HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> {
        let mut m = std::collections::HashMap::new();
        m.insert(
            wamn_postgres::WAMN_POSTGRES_ID,
            self.plugin.clone() as Arc<dyn HostPlugin + Send + Sync>,
        );
        m
    }

    /// A fresh flowrunner instance (a "replica"): `deadline = Some(KILL_TICKS)` for
    /// the victim that will be epoch-killed mid-effect, `None` for a normal runner.
    async fn worker(&self, deadline: Option<u64>) -> anyhow::Result<Worker> {
        let ctx = Ctx::builder(TENANT.to_string(), TENANT.to_string())
            .with_plugins(self.plugin_map())
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(deadline.unwrap_or(u64::MAX / 2));
        let instance = self.pre.instantiate_async(&mut store).await?;
        macro_rules! f {
            ($name:literal) => {
                instance.get_typed_func(&mut store, $name)?
            };
        }
        let seed = f!("seed");
        let set_active = f!("set-active");
        let run = f!("run");
        let run_until_kill = f!("run-until-kill");
        let sink_count = f!("sink-count");
        let reset = f!("reset");
        Ok(Worker {
            store,
            seed,
            set_active,
            run,
            run_until_kill,
            sink_count,
            reset,
        })
    }
}

pub async fn run(args: FailoverBenchArgs) -> anyhow::Result<()> {
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
        "failoverbench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;

    println!("# wamn-host 5.14 failoverbench (schema {SCHEMA}, tenant {TENANT})");
    provision(&admin_url)
        .await
        .context("provision ephemeral schema")?;

    // The plugin outlives every store; register the runner's tenant + schema (the
    // runner uses unqualified table names, resolved via the host-injected
    // search_path).
    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.clone());
    cfg.pool_max_size = args.pool_max;
    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    plugin.set_tenant(TENANT, TENANT)?;
    plugin.set_schema(TENANT, SCHEMA)?;

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let harness = Harness::new(engine, &guest, plugin.clone())?;

    let run_all = args.mode == Mode::All;
    let mut pass = true;
    let outcome = async {
        if run_all || args.mode == Mode::Failover {
            pass &= failover_phase(&harness, &app_url, args.iters).await?;
        }
        if run_all || args.mode == Mode::JanitorGuard {
            pass &= janitor_guard_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::ReverseRace {
            pass &= reverse_race_phase(&harness, &app_url, args.iters).await?;
        }
        anyhow::Ok(())
    }
    .await;

    ticker.abort();
    let _ = teardown(&admin_url).await;
    outcome?;

    println!("\nfailoverbench complete — overall PASS: {pass}");
    if !pass {
        bail!("one or more failover gates failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// failover: kill mid-effect -> lease expires -> reclaim on another replica ->
// reconstruct-resume -> exactly-once + completed (not infrastructure-failure).
// ---------------------------------------------------------------------------

async fn failover_phase(harness: &Harness, app_url: &str, iters: usize) -> anyhow::Result<bool> {
    println!(
        "\n## failover — {iters} kill-mid-effect cycles, reclaimed + resumed on another replica"
    );

    // A setup runner seeds the flow (v1 active) and provides the reset / sink-count
    // reads; two persistent claimer connections model replica A and replica B.
    let mut setup = harness.worker(None).await?;
    setup.call_seed().await?;
    setup.call_set_active(1).await?;
    let (mut a_conn, _ha) = connect_app(app_url).await?;
    let (b_conn, _hb) = connect_app(app_url).await?;

    let claim = claim_batch_sql(1);
    let short_ttl: i64 = 800; // A's lease: held while A is alive, expires after it dies
    let long_ttl: i64 = 5_000; // B's lease: outlives the (ms-scale) resume
    let status_q = format!("SELECT status FROM {SCHEMA}.runs WHERE run_id = $1");
    // pg-write ('w') is recorded by the RESUMING replica (A was killed before it
    // recorded 'w'). Its seq discriminates a correct branch-aware reconstruct (w is
    // the FIRST re-dispatched node -> seq==2, the 2-node prefix skipped) from a
    // broken replay-from-`entry` (w re-dispatched after re-walking the prefix ->
    // seq==4). Robust to the conditional's branch (unlike MAX over all nodes).
    let w_seq_q = format!(
        "SELECT COALESCE(MAX(seq), -1)::int FROM {SCHEMA}.node_runs WHERE run_id = $1 AND node_id = 'w'"
    );
    // Force a claimed run's queue row reap-eligible: lease lapsed more than grace ago
    // ($1 ms) and the redelivery budget spent — the exact predicate the janitor reaps.
    let force_reapable = format!(
        "UPDATE {SCHEMA}.run_queue \
            SET lease_expires_at = now() - ($1::bigint * interval '1 millisecond'), \
                attempts = max_attempts \
          WHERE run_id = $2"
    );

    let mut a_claimed_ok = 0usize;
    let mut clean_kills = 0usize;
    let mut committed_pre = 0usize;
    let mut b_reclaimed_ok = 0usize;
    let mut exactly_once = 0usize;
    let mut reconstructed_ok = 0usize;
    let mut completed_status = 0usize;
    let mut janitor_safe = 0usize;

    for i in 0..iters {
        let run_id = format!("failover-{i}");
        setup.call_reset(&run_id).await?; // clears runs (cascades node_runs + run_queue) + sink

        // --- replica A: dispatch, claim, mark running, then killed mid-effect ---
        enqueue(&mut a_conn, &run_id).await?; // write-ahead 'dispatched' + queue row
        // attempts counts crash evidence only (wamn-fqg.5): the first claim of a
        // never-leased row is FREE, so A claims with attempts == 0.
        let got_a = a_conn.query(&claim, &[&"replica-A", &short_ttl]).await?;
        if got_a.len() == 1
            && got_a[0].get::<_, String>("run_id") == run_id
            && got_a[0].get::<_, i32>("attempts") == 0
        {
            a_claimed_ok += 1;
        }
        a_conn.execute(&mark_running_sql(), &[&run_id]).await?; // dispatched -> running

        let mut victim = harness.worker(Some(KILL_TICKS)).await?;
        let killed = victim.call_run_until_kill(&run_id, "receipt").await;
        let interrupted = match killed {
            Ok(Ok(_)) | Ok(Err(_)) => false, // must never return
            Err(e) => matches!(e.downcast_ref::<Trap>(), Some(Trap::Interrupt)),
        };
        drop(victim);
        if interrupted {
            clean_kills += 1;
        } else {
            tracing::error!(run_id, "run-until-kill returned instead of trapping");
        }
        // The side effect committed before the kill => the resume faces a real
        // duplicate (proves exactly-once, not just never-double-ran).
        if setup.call_sink_count(&run_id).await? == 1 {
            committed_pre += 1;
        }

        // --- A is dead and never renews; its lease ages out. ---
        tokio::time::sleep(Duration::from_millis(short_ttl as u64 + 300)).await;

        // --- replica B: reclaim (attempts bumped), resume via reconstruction ---
        // B's reclaim of the expired lease is the FIRST counted unit of crash
        // evidence: attempts == 1. (It was 2 under the pre-fqg.5 count-every-claim
        // semantics; the new value is the point of the fix, not a regression —
        // A's death is the run's first and only crash.)
        let reclaimed = b_conn.query(&claim, &[&"replica-B", &long_ttl]).await?;
        if reclaimed.len() == 1
            && reclaimed[0].get::<_, String>("run_id") == run_id
            && reclaimed[0].get::<_, i32>("attempts") == 1
        {
            b_reclaimed_ok += 1;
        }
        b_conn.execute(&mark_running_sql(), &[&run_id]).await?; // no-op (already running)

        let mut resumer = harness.worker(None).await?;
        let _ = resumer.call_run(&run_id, "receipt").await?; // reconstruct -> complete
        drop(resumer);
        if setup.call_sink_count(&run_id).await? == 1 {
            exactly_once += 1;
        }

        // Attribute exactly-once to RECONSTRUCTION, not just the sink's ON CONFLICT.
        // A recorded the 2-node prefix (`in`, `t`) before the kill, so a correct
        // branch-aware reconstruct re-dispatches `w` FIRST -> seq==2. A broken
        // reconstruct that replays from `entry` re-walks the prefix and records `w`
        // at seq==4, which this catches.
        let w_seq: i32 = b_conn.query_one(&w_seq_q, &[&run_id]).await?.get(0);
        if w_seq == 2 {
            reconstructed_ok += 1;
        } else {
            tracing::error!(
                run_id,
                w_seq,
                "reconstruct did not skip the completed prefix (expected pg-write seq==2)"
            );
        }

        let status: String = b_conn.query_one(&status_q, &[&run_id]).await?.get(0);
        if status == "completed" {
            completed_status += 1;
        } else {
            tracing::error!(run_id, status, "reclaimed run did not end 'completed'");
        }

        // Completion-vs-failover race (ordering 1: completion, then the janitor).
        // Reproduce the window between the completion write and the host's dequeue:
        // while the run is 'completed' but STILL queued, force its queue row
        // reap-eligible and fire the janitor. The status guard must leave it
        // 'completed' (the stale queue row is cleaned up). Without the guard this
        // flips to infrastructure-failure — so this makes `--mode failover`
        // mutation-sensitive to the guard on the *real* reclaimed run.
        b_conn
            .execute(&force_reapable, &[&(GRACE_MS + 1_000), &run_id])
            .await?;
        b_conn.execute(&janitor_sweep_sql(), &[&GRACE_MS]).await?;
        let status2: String = b_conn.query_one(&status_q, &[&run_id]).await?.get(0);
        if status2 == "completed" {
            janitor_safe += 1;
        } else {
            tracing::error!(run_id, status2, "janitor relabeled a completed run");
        }
        // The normal completion dequeue (the sweep's DELETE may already have removed
        // the row, in which case this is a no-op).
        b_conn.execute(&dequeue_sql(), &[&run_id]).await?;
    }

    println!(
        "A claimed = {a_claimed_ok}/{iters}, clean kills = {clean_kills}/{iters}, side effect committed pre-kill = {committed_pre}/{iters}"
    );
    println!(
        "B reclaimed (attempts==1, the first counted crash) = {b_reclaimed_ok}/{iters}, resumed-to-single-row = {exactly_once}/{iters}, reconstruct skipped prefix (pg-write seq==2) = {reconstructed_ok}/{iters}"
    );
    println!(
        "ended completed = {completed_status}/{iters}, janitor left completed run alone = {janitor_safe}/{iters}"
    );
    let pass = a_claimed_ok == iters
        && clean_kills == iters
        && committed_pre == iters
        && b_reclaimed_ok == iters
        && exactly_once == iters
        && reconstructed_ok == iters
        && completed_status == iters
        && janitor_safe == iters;
    println!(
        "PASS(failover: killed A, reclaimed + resumed on B, exactly-once via reconstruction, completed): {pass}"
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// reverse-race: the janitor reaps a still-'running' reclaimed run FIRST, then the
// resume completes — the guest's unconditional completion write must win.
// ---------------------------------------------------------------------------

async fn reverse_race_phase(
    harness: &Harness,
    app_url: &str,
    iters: usize,
) -> anyhow::Result<bool> {
    println!(
        "\n## reverse-race — {iters} cycles: janitor reaps a still-running reclaimed run, then the resume completes anyway"
    );

    let mut setup = harness.worker(None).await?;
    setup.call_seed().await?;
    setup.call_set_active(1).await?;
    let (mut a_conn, _ha) = connect_app(app_url).await?;
    let (b_conn, _hb) = connect_app(app_url).await?;

    let claim = claim_batch_sql(1);
    let short_ttl: i64 = 800;
    let status_q = format!("SELECT status FROM {SCHEMA}.runs WHERE run_id = $1");
    let force_reapable = format!(
        "UPDATE {SCHEMA}.run_queue \
            SET lease_expires_at = now() - ($1::bigint * interval '1 millisecond'), \
                attempts = max_attempts \
          WHERE run_id = $2"
    );

    let mut clean_kills = 0usize;
    let mut reaped_ok = 0usize; // janitor reaped the still-'running' run
    let mut resurrected_ok = 0usize; // the resume's completion overrode the reap
    let mut exactly_once = 0usize;

    for i in 0..iters {
        let run_id = format!("reverse-{i}");
        setup.call_reset(&run_id).await?;

        // Replica A claims + is killed mid-effect (a partial checkpoint: `in`/`t`
        // recorded, sink row written, `w` outstanding).
        enqueue(&mut a_conn, &run_id).await?;
        a_conn.query(&claim, &[&"replica-A", &short_ttl]).await?;
        a_conn.execute(&mark_running_sql(), &[&run_id]).await?;
        let mut victim = harness.worker(Some(KILL_TICKS)).await?;
        let killed = victim.call_run_until_kill(&run_id, "receipt").await;
        let interrupted = match killed {
            Ok(Ok(_)) | Ok(Err(_)) => false,
            Err(e) => matches!(e.downcast_ref::<Trap>(), Some(Trap::Interrupt)),
        };
        drop(victim);
        if interrupted {
            clean_kills += 1;
        }

        // A's lease expires; replica B reclaims the still-'running' run — but its
        // resume is *slow*: the lease lapses past grace with the budget spent while
        // the run is still 'running'. Force that state deterministically.
        tokio::time::sleep(Duration::from_millis(short_ttl as u64 + 300)).await;
        b_conn.query(&claim, &[&"replica-B", &short_ttl]).await?;
        b_conn.execute(&mark_running_sql(), &[&run_id]).await?; // no-op (already running)
        b_conn
            .execute(&force_reapable, &[&(GRACE_MS + 1_000), &run_id])
            .await?;

        // The janitor wins the race: a non-terminal ('running') abandoned run IS
        // reaped (the guard protects only TERMINAL runs) — status flips to
        // infrastructure-failure and its queue row is removed.
        b_conn.execute(&janitor_sweep_sql(), &[&GRACE_MS]).await?;
        let reaped: String = b_conn.query_one(&status_q, &[&run_id]).await?.get(0);
        if reaped == "infrastructure-failure" {
            reaped_ok += 1;
        } else {
            tracing::error!(run_id, reaped, "janitor did not reap the still-running run");
        }

        // B's resume completes anyway; the guest's UNCONDITIONAL completion write
        // overrides the premature infrastructure-failure verdict.
        let mut resumer = harness.worker(None).await?;
        let _ = resumer.call_run(&run_id, "receipt").await?;
        drop(resumer);
        let final_status: String = b_conn.query_one(&status_q, &[&run_id]).await?.get(0);
        if final_status == "completed" {
            resurrected_ok += 1;
        } else {
            tracing::error!(
                run_id,
                final_status,
                "resume did not override the janitor verdict (expected 'completed')"
            );
        }
        if setup.call_sink_count(&run_id).await? == 1 {
            exactly_once += 1;
        }
    }

    println!(
        "clean kills = {clean_kills}/{iters}, janitor reaped the running run = {reaped_ok}/{iters}, resume overrode to completed = {resurrected_ok}/{iters}, exactly-once = {exactly_once}/{iters}"
    );
    let pass = clean_kills == iters
        && reaped_ok == iters
        && resurrected_ok == iters
        && exactly_once == iters;
    println!(
        "PASS(reverse-race: janitor reaps a running run, the resume's completion still wins): {pass}"
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// janitor-guard: the completion-vs-failover race guard, deterministically.
// ---------------------------------------------------------------------------

async fn janitor_guard_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!(
        "\n## janitor-guard — a reclaimed+completed run is NOT relabeled infrastructure-failure"
    );
    reset(admin_url).await?;

    // Seed (superuser bypasses RLS): a COMPLETED run whose stale queue row is
    // expired-past-grace + budget-spent (the completion-then-dequeue window), and a
    // genuine non-terminal ORPHAN with the same lease shape. grace = 1h below; both
    // leases expired 2h ago.
    {
        let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
        let conn_task = tokio::spawn(conn);
        let r = client
            .batch_execute(&format!(
                "INSERT INTO {SCHEMA}.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
                   ('{TENANT}','jg-completed','{FLOW_ID}',1,'completed'), \
                   ('{TENANT}','jg-orphan','{FLOW_ID}',1,'running'); \
                 INSERT INTO {SCHEMA}.run_queue \
                   (tenant_id, run_id, available_at, lease_owner, lease_expires_at, attempts, max_attempts) VALUES \
                   ('{TENANT}','jg-completed', now()-interval '3 hour','dead',now()-interval '2 hour',5,5), \
                   ('{TENANT}','jg-orphan',    now()-interval '3 hour','dead',now()-interval '2 hour',5,5);"
            ))
            .await;
        drop(client);
        let _ = conn_task.await;
        r.context("seed janitor-guard fixtures")?;
    }

    let (client, _h) = connect_app(app_url).await?;
    client.execute(&janitor_sweep_sql(), &[&GRACE_MS]).await?;

    let status_q = format!("SELECT status FROM {SCHEMA}.runs WHERE run_id = $1");
    let queued_q = format!("SELECT count(*) FROM {SCHEMA}.run_queue WHERE run_id = $1");
    let status_of = async |run: &str| -> anyhow::Result<String> {
        Ok(client.query_one(&status_q, &[&run]).await?.get(0))
    };
    let queued = async |run: &str| -> anyhow::Result<i64> {
        Ok(client.query_one(&queued_q, &[&run]).await?.get(0))
    };

    // The completed run keeps its status (the guard), and its stale queue row is
    // still cleaned up.
    let completed_status = status_of("jg-completed").await?;
    let completed_queued = queued("jg-completed").await?;
    // The genuine orphan is still reaped — the guard doesn't over-block.
    let orphan_status = status_of("jg-orphan").await?;
    let orphan_queued = queued("jg-orphan").await?;

    println!(
        "completed run: status = {completed_status} (kept), queue row cleaned = {}",
        completed_queued == 0
    );
    println!(
        "orphan run: status = {orphan_status} (reaped), dequeued = {}",
        orphan_queued == 0
    );
    let pass = completed_status == "completed"
        && completed_queued == 0
        && orphan_status == "infrastructure-failure"
        && orphan_queued == 0;
    println!("PASS(janitor-guard: keeps a completed run, still reaps a real orphan): {pass}");
    Ok(pass)
}
