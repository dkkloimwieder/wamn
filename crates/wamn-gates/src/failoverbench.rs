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
    write_ahead_run_sql, write_ahead_triggered_run_sql,
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
    /// fqg.4: the guest claims its own work — claim + drive + dequeue, single
    /// runner draining the queue AND concurrent replicas draining it
    /// exactly-once (SKIP LOCKED), plus the wrong-flow guard (the claim path
    /// drives the RECORDED flow, not a fixture constant).
    Claim,
    /// fqg.4: a delay run the guest parks (releasing the lease) then a later
    /// run-next re-claims and completes — the queue-driven parked-wake.
    Park,
    /// fqg.4: the per-node lease heartbeat — a live-but-slow runner keeps its
    /// lease across a long walk (no spurious reclaim), even under a short TTL and
    /// a contending replica.
    Heartbeat,
    /// fqg.9: the guest claims PARTITIONED(key) runs in order — a single runner
    /// leases each partition, drives its head in stream order (one in flight per
    /// key), and drains interleaved keyed streams (mixed stream_seq/enqueued_at)
    /// while unordered NULL-key rows still claim via the old path.
    PartitionOrder,
    /// fqg.9: partition failover — owner A drives a key's head then dies (its
    /// partition lease force-expired); replica B acquires the key and resumes IN
    /// ORDER from the next head, no skipped/duplicated run.
    PartitionFailover,
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
        .batch_execute(&format!(
            "TRUNCATE {SCHEMA}.runs, {SCHEMA}.sink, {SCHEMA}.partition_owner CASCADE;"
        ))
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

/// Seed the failover flow (`poc-receipt` v1, active) host-side — the replacement
/// for the guest's retired `seed`/`set-active` exports (SR2). The flow JSON is the
/// shared flowbench fixture; v1 is what the `run`/`run-until-kill` exports drive.
/// `client` is a `connect_app` session, already scoped to `SCHEMA` + `TENANT`, so
/// the unqualified `flows` insert lands in the ephemeral schema under the claim.
async fn seed_failover_flow(client: &Client) -> anyhow::Result<()> {
    wamn_gate_harness::seed_flow_version(
        client,
        TENANT,
        FLOW_ID,
        1,
        true,
        &crate::flowbench::flow_json(1),
        true,
    )
    .await
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

/// The `run-next` export's typed signature — `(lease-ttl-ms) -> (claimed,
/// run-id, outcome)` (factored out; the tuple-of-result is verbose inline).
type RunNextFunc = TypedFunc<(u64,), (Result<(bool, Option<String>, u32), String>,)>;

struct Worker {
    store: Store<SharedCtx>,
    run: TypedFunc<(String, String), (Result<u32, String>,)>,
    run_until_kill: TypedFunc<(String, String), (Result<u32, String>,)>,
    run_next: RunNextFunc,
    sink_count: TypedFunc<(String,), (Result<u64, String>,)>,
    reset: TypedFunc<(String,), (Result<u64, String>,)>,
}

impl Worker {
    async fn call_run(&mut self, run_id: &str, payload: &str) -> anyhow::Result<u32> {
        let (r,) = self
            .run
            .call_async(&mut self.store, (run_id.to_string(), payload.to_string()))
            .await?;
        r.map_err(|e| anyhow::anyhow!("run: {e}"))
    }
    /// One turn of the guest's production dispatch loop (fqg.4): claim + drive +
    /// dequeue/park the next queued run. Returns (claimed, run_id, outcome).
    async fn call_run_next(
        &mut self,
        lease_ttl_ms: u64,
    ) -> anyhow::Result<(bool, Option<String>, u32)> {
        let (r,) = self
            .run_next
            .call_async(&mut self.store, (lease_ttl_ms,))
            .await?;
        r.map_err(|e| anyhow::anyhow!("run-next: {e}"))
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
        // 5.9: the runner imports wamn:node/credentials unconditionally; no
        // failover fixture declares one, so the linked vault stays unbacked.
        wamn_host::plugins::wamn_credentials::add_to_linker(&mut linker)?;
        // cjv.3: the flowrunner declares its per-run grant via this trusted
        // channel; the harness must link it or instantiation fails.
        wamn_host::plugins::wamn_credentials::add_runner_to_linker(&mut linker)?;
        // fqg.11: the flowrunner declares its per-run egress the same way.
        wamn_host::plugins::runner_egress::add_runner_to_linker(&mut linker)?;
        // l5i9.12.2: the trusted per-run causation channel (the flowrunner world
        // now imports it; instantiation traps without it).
        wamn_postgres::add_runner_causation_to_linker(&mut linker)?;
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
        // cjv.3: the flowrunner declares its per-run grant on every walk, so a
        // credentials plugin must back the linked interface. No failover fixture
        // declares a credential, so an empty unbacked vault suffices.
        m.insert(
            wamn_host::plugins::wamn_credentials::WAMN_CREDENTIALS_ID,
            Arc::new(wamn_host::plugins::wamn_credentials::WamnCredentials::empty())
                as Arc<dyn HostPlugin + Send + Sync>,
        );
        // fqg.11: the flowrunner declares its per-run egress on every walk, so
        // the policy plugin must back the linked interface. Enforcement here is
        // the harness's own http handler, so the declaration is inert — the
        // plugin exists to keep the trusted channel satisfied.
        m.insert(
            wamn_host::plugins::runner_egress::RUNNER_EGRESS_ID,
            Arc::new(wamn_host::plugins::runner_egress::RunnerEgressPolicy::default())
                as Arc<dyn HostPlugin + Send + Sync>,
        );
        m
    }

    /// A fresh flowrunner instance (a "replica"): `deadline = Some(KILL_TICKS)` for
    /// the victim that will be epoch-killed mid-effect, `None` for a normal runner.
    /// The component id is the shared `TENANT` (no `app.runner` registered — the
    /// failover/reverse/janitor phases drive `run`/`run-until-kill`, not the claim
    /// path).
    async fn worker(&self, deadline: Option<u64>) -> anyhow::Result<Worker> {
        self.build_worker(TENANT, deadline).await
    }

    /// A CLAIMER replica (fqg.4): a distinct component id so the plugin injects a
    /// distinct `app.runner` lease owner (registered here to the component id
    /// itself), while tenant + schema stay the shared `TENANT`/`SCHEMA` — so
    /// concurrent claimers see one queue but lease under distinct owners.
    async fn worker_claim(&self, owner: &str) -> anyhow::Result<Worker> {
        self.plugin.set_tenant(owner, TENANT)?;
        self.plugin.set_schema(owner, SCHEMA)?;
        self.plugin.set_runner(owner, owner)?;
        self.build_worker(owner, None).await
    }

    async fn build_worker(
        &self,
        component_id: &str,
        deadline: Option<u64>,
    ) -> anyhow::Result<Worker> {
        let ctx = Ctx::builder(component_id.to_string(), component_id.to_string())
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
        let run = f!("run");
        let run_until_kill = f!("run-until-kill");
        let run_next = f!("run-next");
        let sink_count = f!("sink-count");
        let reset = f!("reset");
        Ok(Worker {
            store,
            run,
            run_until_kill,
            run_next,
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
        if run_all || args.mode == Mode::Claim {
            pass &= claim_phase(&harness, &app_url, &admin_url, args.iters).await?;
        }
        if run_all || args.mode == Mode::Park {
            pass &= park_phase(&harness, &app_url).await?;
        }
        if run_all || args.mode == Mode::Heartbeat {
            pass &= heartbeat_phase(&harness, &app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::PartitionOrder {
            pass &= partition_order_phase(&harness, &app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::PartitionFailover {
            pass &= partition_failover_phase(&harness, &app_url, &admin_url).await?;
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

    // A setup runner provides the reset / sink-count reads; two persistent claimer
    // connections model replica A and replica B. The flow (v1 active) is seeded
    // host-side over replica A's connection (SR2: the guest's seed export is gone).
    let mut setup = harness.worker(None).await?;
    let (mut a_conn, _ha) = connect_app(app_url).await?;
    let (b_conn, _hb) = connect_app(app_url).await?;
    seed_failover_flow(&a_conn).await?;

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
    let (mut a_conn, _ha) = connect_app(app_url).await?;
    let (b_conn, _hb) = connect_app(app_url).await?;
    seed_failover_flow(&a_conn).await?;

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

// ===========================================================================
// fqg.4: the guest claims its own work from the queue (the production dispatch
// path, guest-side). The flowrunner `run-next` export claims a run
// (`FOR UPDATE SKIP LOCKED`), reads its flow + input from the dispatcher-
// persisted `runs` row, flips it running, walks it renewing the lease per node,
// and dequeues / parks. These phases drive that export against the SAME
// ephemeral schema (`runs`/`node_runs`/`run_queue`/`flows`/`sink`) the failover
// phases use — so the claim path is a gate-of-record path, not a sandbox.
// ===========================================================================

/// A second flow with a DISTINCT id + transform, so the wrong-flow guard can
/// tell whether the claim path drove the RECORDED flow (reverse -> "tpiecer")
/// or a hard-coded fixture id (upper -> "RECEIPT").
const ALT_FLOW_ID: &str = "alt-flow";
/// A `delay`-ending flow the park gate seeds (webhook-in -> delay -> pg-write ->
/// respond — no http-call, so no egress is needed in this bench).
const DELAY_FLOW_ID: &str = "poc-delay";
/// A long transform chain (heartbeat gate): enough nodes that the walk spans
/// several lease renewals.
const HEARTBEAT_FLOW_ID: &str = "heartbeat";

fn alt_flow_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{ALT_FLOW_ID}","version":1,
            "trigger":{{"type":"webhook","sync":true}},"entry":"in",
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"t","type":"transform","config":{{"op":"reverse"}}}},
              {{"id":"w","type":"pg-write"}},
              {{"id":"c","type":"conditional","config":{{"min-len":3}}}},
              {{"id":"out","type":"respond"}}
            ],
            "edges":[{{"from":"in","to":"t"}},{{"from":"t","to":"w"}},
                     {{"from":"w","to":"c"}},{{"from":"c","to":"out"}}]}}"#
    )
}

fn delay_flow_json(delay_secs: u64) -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{DELAY_FLOW_ID}","version":1,
            "trigger":{{"type":"cron","schedule":"* * * * *"}},"entry":"in",
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"d","type":"delay","config":{{"delay-secs":{delay_secs}}}}},
              {{"id":"w","type":"pg-write"}},
              {{"id":"out","type":"respond"}}
            ],
            "edges":[{{"from":"in","to":"d"}},{{"from":"d","to":"w"}},
                     {{"from":"w","to":"out"}}]}}"#
    )
}

fn heartbeat_flow_json(n: usize) -> String {
    let mut nodes = String::from(r#"{"id":"in","type":"webhook-in"}"#);
    let mut edges = String::new();
    let mut prev = String::from("in");
    for i in 0..n {
        let id = format!("t{i}");
        nodes.push_str(&format!(
            r#",{{"id":"{id}","type":"transform","config":{{"op":"upper"}}}}"#
        ));
        edges.push_str(&format!(r#"{{"from":"{prev}","to":"{id}"}},"#));
        prev = id;
    }
    nodes.push_str(r#",{"id":"out","type":"respond"}"#);
    edges.push_str(&format!(r#"{{"from":"{prev}","to":"out"}}"#));
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{HEARTBEAT_FLOW_ID}","version":1,
            "trigger":{{"type":"cron","schedule":"* * * * *"}},"entry":"in",
            "nodes":[{nodes}],"edges":[{edges}]}}"#
    )
}

/// Seed an active v1 flow host-side (the guest's `seed` export is retired, SR2).
async fn seed_flow(client: &Client, flow_id: &str, json: &str) -> anyhow::Result<()> {
    wamn_gate_harness::seed_flow_version(client, TENANT, flow_id, 1, true, json, true).await
}

/// `count(*)`-style scalar read (a free helper so it never holds a borrow of the
/// seed connection across the `&mut` transactions `seed_claim_run` needs).
async fn count_rows(client: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(client.query_one(sql, &[]).await?.get(0))
}

/// Seed a run the way the DISPATCHER does: the write-ahead `dispatched` row with
/// its `flow_id` + trigger `input_json`, co-transacted with the queue row — the
/// exact producer state the guest claims (`write_ahead_triggered_run_sql` +
/// `enqueue_sql`). `input_json` is JSON text (`"receipt"` = the string payload).
async fn seed_claim_run(
    client: &mut Client,
    run_id: &str,
    flow_id: &str,
    input_json: &str,
) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(
        &write_ahead_triggered_run_sql(),
        &[&run_id, &flow_id, &1i32, &"cron", &input_json],
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

// ---------------------------------------------------------------------------
// claim: single runner drains the queue, concurrent replicas drain it
// exactly-once (SKIP LOCKED), and the claim path drives the RECORDED flow.
// ---------------------------------------------------------------------------

async fn claim_phase(
    harness: &Harness,
    app_url: &str,
    admin_url: &str,
    iters: usize,
) -> anyhow::Result<bool> {
    println!(
        "\n## claim — guest claims from run_queue: drive+dequeue, concurrent exactly-once, wrong-flow"
    );
    reset(admin_url).await?;
    let (mut seed_conn, _h) = connect_app(app_url).await?;
    seed_flow(&seed_conn, FLOW_ID, &crate::flowbench::flow_json(1)).await?;
    seed_flow(&seed_conn, ALT_FLOW_ID, &alt_flow_json()).await?;

    // Generous TTL: the lease never expires here, so claim/exactly-once are not
    // clouded by reclaim noise (the heartbeat gate exercises renewal separately).
    let ttl: u64 = 30_000;
    let n = iters;

    // --- (1) single runner drains N seeded runs, each driven exactly once ---
    for i in 0..n {
        seed_claim_run(
            &mut seed_conn,
            &format!("claim-{i}"),
            FLOW_ID,
            "\"receipt\"",
        )
        .await?;
    }
    let mut drainer = harness.worker_claim("drainer").await?;
    let mut drained = 0usize;
    loop {
        let (claimed, _rid, outcome) = drainer.call_run_next(ttl).await?;
        if !claimed {
            break;
        }
        if outcome == 0 {
            drained += 1;
        }
    }
    let queued = count_rows(
        &seed_conn,
        &format!("SELECT count(*) FROM {SCHEMA}.run_queue WHERE run_id LIKE 'claim-%'"),
    )
    .await?;
    let sinks = count_rows(
        &seed_conn,
        &format!("SELECT count(*) FROM {SCHEMA}.sink WHERE run_id LIKE 'claim-%'"),
    )
    .await?;
    let done = count_rows(
        &seed_conn,
        &format!(
            "SELECT count(*) FROM {SCHEMA}.runs WHERE run_id LIKE 'claim-%' AND status='completed'"
        ),
    )
    .await?;
    let single_drain = drained == n && queued == 0 && sinks as usize == n && done as usize == n;
    println!(
        "single drain: drained {drained}/{n}, queue drained = {} (rows={queued}), sinks={sinks}, completed={done} -> {single_drain}",
        queued == 0
    );

    // --- (2) concurrent exactly-once: M replicas race one queue (SKIP LOCKED) ---
    reset(admin_url).await?;
    seed_flow(&seed_conn, FLOW_ID, &crate::flowbench::flow_json(1)).await?;
    for i in 0..n {
        seed_claim_run(&mut seed_conn, &format!("conc-{i}"), FLOW_ID, "\"receipt\"").await?;
    }
    const M: usize = 4;
    let mut handles = Vec::new();
    for w in 0..M {
        let mut worker = harness.worker_claim(&format!("claimer-{w}")).await?;
        handles.push(tokio::spawn(async move {
            let mut c = 0usize;
            loop {
                match worker.call_run_next(ttl).await {
                    Ok((true, _, _)) => c += 1,
                    Ok((false, _, _)) => break,
                    Err(e) => {
                        tracing::error!("claimer error: {e}");
                        break;
                    }
                }
            }
            c
        }));
    }
    let mut total = 0usize;
    for h in handles {
        total += h.await.unwrap_or(0);
    }
    let conc_queued = count_rows(
        &seed_conn,
        &format!("SELECT count(*) FROM {SCHEMA}.run_queue WHERE run_id LIKE 'conc-%'"),
    )
    .await?;
    // No run drove twice: the max sink rows for any single conc run is 1.
    let conc_dup = count_rows(
        &seed_conn,
        &format!(
            "SELECT COALESCE(MAX(c),0) FROM (SELECT count(*) c FROM {SCHEMA}.sink WHERE run_id LIKE 'conc-%' GROUP BY run_id) s"
        ),
    )
    .await?;
    let conc_done = count_rows(
        &seed_conn,
        &format!(
            "SELECT count(*) FROM {SCHEMA}.runs WHERE run_id LIKE 'conc-%' AND status='completed'"
        ),
    )
    .await?;
    let exactly_once = total == n && conc_queued == 0 && conc_dup <= 1 && conc_done as usize == n;
    println!(
        "concurrent: {M} replicas drove {total}/{n} total, queue drained = {} (rows={conc_queued}), max sink rows/run = {conc_dup} (<=1), completed = {conc_done} -> {exactly_once}",
        conc_queued == 0
    );

    // --- (3) wrong-flow: a run RECORDED as alt-flow must drive alt-flow (reverse),
    // not a hard-coded fixture id (poc-receipt / upper). ---
    reset(admin_url).await?;
    seed_flow(&seed_conn, FLOW_ID, &crate::flowbench::flow_json(1)).await?;
    seed_flow(&seed_conn, ALT_FLOW_ID, &alt_flow_json()).await?;
    seed_claim_run(&mut seed_conn, "wrongflow", ALT_FLOW_ID, "\"receipt\"").await?;
    let mut wf = harness.worker_claim("wf").await?;
    let (wf_claimed, _rid, wf_outcome) = wf.call_run_next(ttl).await?;
    let wf_payload: Option<String> = seed_conn
        .query_opt(
            &format!("SELECT payload FROM {SCHEMA}.sink WHERE run_id = 'wrongflow'"),
            &[],
        )
        .await?
        .map(|r| r.get(0));
    let wrong_flow_ok = wf_claimed && wf_outcome == 0 && wf_payload.as_deref() == Some("tpiecer");
    println!(
        "wrong-flow: claimed={wf_claimed}, outcome={wf_outcome}, sink payload={wf_payload:?} (want 'tpiecer' = reverse of the RECORDED alt-flow, NOT 'RECEIPT') -> {wrong_flow_ok}"
    );

    let pass = single_drain && exactly_once && wrong_flow_ok;
    println!(
        "PASS(claim: guest drains the queue + concurrent exactly-once + drives the recorded flow): {pass}"
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// park: a delay run parks (releasing the lease) then a later run-next re-claims
// and completes — the queue-driven parked-wake, guest-side.
// ---------------------------------------------------------------------------

async fn park_phase(harness: &Harness, app_url: &str) -> anyhow::Result<bool> {
    println!("\n## park — a delay run parks (lease released), a later run-next completes it");
    let (mut seed_conn, _h) = connect_app(app_url).await?;
    seed_flow(&seed_conn, DELAY_FLOW_ID, &delay_flow_json(1)).await?; // 1s real-clock delay
    let run_id = "park-1";
    seed_claim_run(&mut seed_conn, run_id, DELAY_FLOW_ID, "\"x\"").await?;

    let ttl: u64 = 30_000;
    let mut worker = harness.worker_claim("parker").await?;

    // First turn: the delay parks it. run-next returns outcome 1 (parked); the
    // queue row survives with its lease RELEASED (NULL) and available_at pushed
    // to the wake.
    let (c1, _r1, out1) = worker.call_run_next(ttl).await?;
    // query_opt: a mutant that dequeues instead of parking leaves NO row — the
    // gate must print a clean `false`, not error on a missing row.
    let lease_owner: Option<String> = seed_conn
        .query_opt(
            &format!("SELECT lease_owner FROM {SCHEMA}.run_queue WHERE run_id = $1"),
            &[&run_id],
        )
        .await?
        .and_then(|r| r.get(0));
    let still_queued: i64 = seed_conn
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.run_queue WHERE run_id = $1"),
            &[&run_id],
        )
        .await?
        .get(0);
    let parked = c1 && out1 == 1 && still_queued == 1 && lease_owner.is_none();
    println!(
        "park turn: claimed={c1}, outcome={out1} (1=parked), queue row kept={} lease released={} -> {parked}",
        still_queued == 1,
        lease_owner.is_none()
    );

    // Wait past the wake, then a run-next re-claims (a FREE wake — released lease)
    // and completes.
    tokio::time::sleep(Duration::from_millis(1300)).await;
    let (c2, _r2, out2) = worker.call_run_next(ttl).await?;
    let dequeued: i64 = seed_conn
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.run_queue WHERE run_id = $1"),
            &[&run_id],
        )
        .await?
        .get(0);
    let status: String = seed_conn
        .query_one(
            &format!("SELECT status FROM {SCHEMA}.runs WHERE run_id = $1"),
            &[&run_id],
        )
        .await?
        .get(0);
    let sink: i64 = seed_conn
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.sink WHERE run_id = $1"),
            &[&run_id],
        )
        .await?
        .get(0);
    let woke = c2 && out2 == 0 && dequeued == 0 && status == "completed" && sink == 1;
    println!(
        "wake turn: claimed={c2}, outcome={out2} (0=completed), dequeued={} status={status} sink={sink} -> {woke}",
        dequeued == 0
    );

    let pass = parked && woke;
    println!("PASS(park: delay run parks + releases the lease, wakes and completes): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// heartbeat: the per-node lease renewal ADVANCES lease_expires_at across a long
// walk (deterministic — no steal race). A dropped renew leaves it fixed at the
// claim's value.
// ---------------------------------------------------------------------------

async fn heartbeat_phase(
    harness: &Harness,
    app_url: &str,
    admin_url: &str,
) -> anyhow::Result<bool> {
    println!("\n## heartbeat — per-node lease renewal advances the lease during a long walk");
    reset(admin_url).await?;
    let (mut seed_conn, _h) = connect_app(app_url).await?;
    seed_flow(&seed_conn, HEARTBEAT_FLOW_ID, &heartbeat_flow_json(20)).await?;
    let run_id = "heartbeat-1";
    seed_claim_run(&mut seed_conn, run_id, HEARTBEAT_FLOW_ID, "\"x\"").await?;

    // Large TTL so the lease never EXPIRES (no steal); each per-node renew still
    // moves lease_expires_at forward by the node's elapsed time.
    let ttl: u64 = 60_000;
    let mut worker = harness.worker_claim("hb").await?;
    let drive = tokio::spawn(async move { worker.call_run_next(ttl).await });

    // Poll the (committed) lease on a fresh connection while the guest walks.
    let (poll_conn, _hp) = connect_app(app_url).await?;
    let lease_q = format!("SELECT lease_expires_at FROM {SCHEMA}.run_queue WHERE run_id = $1");
    let mut samples: Vec<std::time::SystemTime> = Vec::new();
    for _ in 0..500 {
        if let Some(row) = poll_conn.query_opt(&lease_q, &[&run_id]).await?
            && let Some(ts) = row.get::<_, Option<std::time::SystemTime>>(0)
        {
            samples.push(ts);
        }
        if drive.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let (claimed, _rid, outcome) = drive.await??;

    // The lease advanced across the walk iff renewal fired per node: >= 2 samples
    // and the max is strictly later than the min. A dropped per-node renew leaves
    // every sample equal to the claim-time lease.
    let advanced = samples.len() >= 2 && samples.iter().max() > samples.iter().min();
    let sink: i64 = seed_conn
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.sink WHERE run_id = $1"),
            &[&run_id],
        )
        .await?
        .get(0);
    // heartbeat flow has no pg-write, so completion is the signal it walked.
    let completed = claimed && outcome == 0;
    let _ = sink;
    println!(
        "heartbeat: {} lease samples, advanced = {advanced}, run completed = {completed}",
        samples.len()
    );
    let pass = advanced && completed;
    println!("PASS(heartbeat: per-node renewal advances the lease across the walk): {pass}");
    Ok(pass)
}

// ===========================================================================
// fqg.9: the guest claims PARTITIONED(key) runs in order. The flowrunner
// `run-next` export now, when the global (unpartitioned) claim is empty, leases
// a partition (`acquire_partitions_sql`), claims its HEAD in stream order
// (`claim_partition_head_sql` — one in flight per key, D20 policy on the row),
// drives it via the shared `execute_claimed` path (renewing the partition lease
// per node), and steps down (`release_partition_sql`) when the partition drains.
// These phases drive that export against the SAME ephemeral schema the failover
// phases use (now carrying `partition_owner` + the partition index).
// ===========================================================================

/// Seed a PARTITIONED run the way a keyed producer does: the write-ahead
/// `dispatched` runs row (poc-receipt) co-transacted with a `run_queue` row
/// bound to `key` under the default `blocking` policy, available now, with an
/// explicit `enqueued_at` and `stream_seq` — the two blocking stream-order
/// coordinates the head claim ranks by. `enqueued_at` is anchored to a FIXED
/// past instant + `enq_ms` (NOT `now()`, which drifts per seeding transaction and
/// would let insertion order — not the seeded coordinates — decide the stream
/// order); larger `enq_ms` = later in the stream.
async fn seed_partition_run(
    client: &mut Client,
    run_id: &str,
    key: &str,
    enq_ms: i64,
    stream_seq: i64,
) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(
        &write_ahead_triggered_run_sql(),
        &[&run_id, &FLOW_ID, &1i32, &"cron", &"\"receipt\""],
    )
    .await?;
    tx.execute(
        &format!(
            "INSERT INTO {SCHEMA}.run_queue \
               (tenant_id, run_id, partition_key, partition_policy, priority, available_at, enqueued_at, stream_seq) \
             VALUES (current_setting('app.tenant', true), $1, $2, 'blocking', 0, now(), \
                     TIMESTAMPTZ '2000-01-01 00:00:00+00' + ($3::bigint * interval '1 millisecond'), $4)"
        ),
        &[&run_id, &key, &enq_ms, &stream_seq],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// partition-order: a single runner drains interleaved keyed streams IN STREAM
// ORDER per key (one in flight per key), while unordered NULL-key rows drain via
// the old global claim. Two keys exercise BOTH tiebreak fields: `kseq` (equal
// enqueued_at, distinct stream_seq) and `kenq` (equal stream_seq, distinct
// enqueued_at). Each is seeded so the stream order REVERSES run-id order, so a
// head decision that dropped stream_seq or enqueued_at re-orders a key and fails.
// ---------------------------------------------------------------------------

async fn partition_order_phase(
    harness: &Harness,
    app_url: &str,
    admin_url: &str,
) -> anyhow::Result<bool> {
    println!(
        "\n## partition-order — guest drives partitioned(key) runs in stream order (one in flight/key); NULL-key rows via the old path"
    );
    reset(admin_url).await?;
    let (mut seed, _h) = connect_app(app_url).await?;
    seed_flow(&seed, FLOW_ID, &crate::flowbench::flow_json(1)).await?;

    // kseq: run-ids ks0,ks1,ks2 with EQUAL enqueued_at and stream_seq 2,1,0 ->
    // stream order ks2,ks1,ks0 (stream_seq is the sole discriminator).
    for (i, seq) in [(0i64, 2i64), (1, 1), (2, 0)] {
        seed_partition_run(&mut seed, &format!("ks{i}"), "kseq", 0, seq).await?;
    }
    // kenq: run-ids ke0,ke1,ke2 with EQUAL stream_seq and enqueued-offsets 200,100,0
    // (smaller = earlier) -> stream order ke2,ke1,ke0 (enqueued_at is the sole
    // discriminator).
    for (i, off) in [(0i64, 200i64), (1, 100), (2, 0)] {
        seed_partition_run(&mut seed, &format!("ke{i}"), "kenq", off, 0).await?;
    }
    // Unordered NULL-key rows (the global SKIP-LOCKED claim path).
    for i in 0..5 {
        seed_claim_run(&mut seed, &format!("pu-{i}"), FLOW_ID, "\"receipt\"").await?;
    }

    let ttl: u64 = 30_000;
    let mut worker = harness.worker_claim("po").await?;
    // A single runner drains the whole queue; record the completion order.
    let mut drive_order: Vec<String> = Vec::new();
    loop {
        let (claimed, rid, outcome) = worker.call_run_next(ttl).await?;
        if !claimed {
            break;
        }
        if outcome == 0
            && let Some(id) = rid
        {
            drive_order.push(id);
        }
    }

    let per = |prefix: &str| -> Vec<String> {
        drive_order
            .iter()
            .filter(|id| id.starts_with(prefix))
            .cloned()
            .collect()
    };
    let kseq = per("ks");
    let kenq = per("ke");
    let nulls = per("pu-");
    let kseq_ok = kseq == ["ks2", "ks1", "ks0"];
    let kenq_ok = kenq == ["ke2", "ke1", "ke0"];
    let null_ok = nulls.len() == 5;

    let queued = count_rows(&seed, &format!("SELECT count(*) FROM {SCHEMA}.run_queue")).await?;
    let completed = count_rows(
        &seed,
        &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE status='completed'"),
    )
    .await?;
    // No run drove twice: max sink rows for any keyed run is 1 (exactly-once).
    let max_sink = count_rows(
        &seed,
        &format!(
            "SELECT COALESCE(MAX(c),0) FROM (SELECT count(*) c FROM {SCHEMA}.sink GROUP BY run_id) s"
        ),
    )
    .await?;
    let leases_left = count_rows(
        &seed,
        &format!("SELECT count(*) FROM {SCHEMA}.partition_owner WHERE lease_expires_at > now()"),
    )
    .await?;
    let total = 6 + 5;

    println!("  kseq (stream_seq tiebreak) order {kseq:?} (want ks2,ks1,ks0) -> {kseq_ok}");
    println!("  kenq (enqueued_at tiebreak) order {kenq:?} (want ke2,ke1,ke0) -> {kenq_ok}");
    println!(
        "  NULL-key drained {}/5 -> {null_ok}, queue drained = {} (rows={queued}), completed={completed}/{total}, max sink/run={max_sink} (<=1), live partition leases left={leases_left} (retained; gc'd on expiry)",
        nulls.len(),
        queued == 0
    );
    let pass = kseq_ok
        && kenq_ok
        && null_ok
        && queued == 0
        && completed as usize == total
        && max_sink <= 1;
    println!(
        "PASS(partition-order: per-key stream order + one-in-flight + NULL-key via old path, exactly once): {pass}"
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// partition-failover: owner A drives a key's head then dies (its partition lease
// force-expired — the established gate idiom); replica B acquires the key and
// resumes IN ORDER from the next head, no skipped or duplicated run.
// ---------------------------------------------------------------------------

async fn partition_failover_phase(
    harness: &Harness,
    app_url: &str,
    admin_url: &str,
) -> anyhow::Result<bool> {
    println!(
        "\n## partition-failover — owner A drives the head then dies; replica B reacquires the key and resumes in order"
    );
    reset(admin_url).await?;
    let (mut seed, _h) = connect_app(app_url).await?;
    seed_flow(&seed, FLOW_ID, &crate::flowbench::flow_json(1)).await?;
    // One key, three ordered runs (stream_seq 0,1,2 = stream order pf-0,pf-1,pf-2).
    for seq in 0..3i64 {
        seed_partition_run(&mut seed, &format!("pf-{seq}"), "pf", 0, seq).await?;
    }

    let ttl: u64 = 30_000;
    // Replica A: ONE run-next -> lease pf, drive + complete its head pf-0 (A now
    // owns pf, lease live).
    let mut a = harness.worker_claim("pfa").await?;
    let (a_claimed, a_rid, a_out) = a.call_run_next(ttl).await?;
    let a_head_ok = a_claimed && a_rid.as_deref() == Some("pf-0") && a_out == 0;

    // A "dies": force-expire its partition lease directly (A never renews again),
    // exactly the lease-timestamp idiom queuebench's partition failover uses.
    seed.execute(
        &format!(
            "UPDATE {SCHEMA}.partition_owner SET lease_expires_at = now() - interval '1 hour' WHERE partition_key = 'pf'"
        ),
        &[],
    )
    .await?;

    // Replica B: reacquires pf (steals the expired lease) and resumes IN ORDER.
    let mut b = harness.worker_claim("pfb").await?;
    let mut b_order: Vec<String> = Vec::new();
    loop {
        let (claimed, rid, outcome) = b.call_run_next(ttl).await?;
        if !claimed {
            break;
        }
        if outcome == 0
            && let Some(id) = rid
        {
            b_order.push(id);
        }
    }
    let b_in_order = b_order == ["pf-1", "pf-2"];

    let completed = count_rows(
        &seed,
        &format!(
            "SELECT count(*) FROM {SCHEMA}.runs WHERE run_id LIKE 'pf-%' AND status='completed'"
        ),
    )
    .await?;
    // No skip / dup: exactly one sink row per pf run (three total).
    let sinks = count_rows(
        &seed,
        &format!("SELECT count(*) FROM {SCHEMA}.sink WHERE run_id LIKE 'pf-%'"),
    )
    .await?;
    let queued = count_rows(
        &seed,
        &format!("SELECT count(*) FROM {SCHEMA}.run_queue WHERE run_id LIKE 'pf-%'"),
    )
    .await?;
    let final_owner: Option<String> = seed
        .query_opt(
            &format!("SELECT lease_owner FROM {SCHEMA}.partition_owner WHERE partition_key='pf'"),
            &[],
        )
        .await?
        .and_then(|r| r.get(0));

    println!("  A drove head = {a_rid:?} outcome={a_out} -> {a_head_ok}");
    println!("  B resumed order {b_order:?} (want pf-1,pf-2) -> {b_in_order}");
    println!(
        "  completed={completed}/3, sinks={sinks}/3 (exactly once), queue drained = {} , key owner after B = {final_owner:?}",
        queued == 0
    );
    let pass = a_head_ok && b_in_order && completed == 3 && sinks == 3 && queued == 0;
    println!(
        "PASS(partition-failover: A drove the head, B reacquired + resumed in order, exactly once): {pass}"
    );
    Ok(pass)
}
