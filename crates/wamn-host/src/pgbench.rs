//! The `pgbench` subcommand: S2 measurements and the three mandatory
//! `wamn:postgres` security gates (docs/p0-exit-criteria.md S2).
//!
//! Unlike the S1 `bench` command (raw wasi:cli components), this instantiates
//! the `pgprobe` guest — which imports `wamn:postgres/client` — into a
//! hand-built [`SharedCtx`] store with the real plugin linked, then drives its
//! `run(op, arg)` export. Working at the store level (rather than through the
//! Host workload API) lets the harness time individual calls, set per-store
//! epoch deadlines for the chaos gate, and read the plugin's connection
//! accounting directly.
//!
//! Modes:
//!   qps        — sustained throughput + p50/p99 from concurrent guests.
//!   saturation — pool exhaustion returns `connection-unavailable`, not hangs.
//!   chaos      — epoch-kill a guest mid-transaction 100×; every later
//!                checkout is claim-free and transaction-free; killed
//!                connections are destroyed, never reused.
//!   rls        — two tenants, one table; 10k randomized cross-tenant reads
//!                leak zero rows; cross-tenant writes are permission-denied.
//!   injection  — SQL fragments in params round-trip byte-identically.
//!   all        — every mode in sequence.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::NoTls;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{
    Component as WasmtimeComponent, InstancePre, Linker, TypedFunc,
};
use wash_runtime::wasmtime::{Engine as RawEngine, Store, Trap};

use crate::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use crate::plugins::wamn_postgres::{
    self, CredentialProvider, ProjectConfig, StaticCredentialProvider, WamnPostgres,
    WamnPostgresConfig,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Qps,
    Saturation,
    Chaos,
    Rls,
    Injection,
    /// [2.2] per-project pooling + credential resolution + per-project policy.
    Multiproject,
    All,
}

#[derive(Debug, Args)]
pub struct PgBenchArgs {
    /// Path to the pgprobe guest component
    #[arg(long, default_value = "/bench/pgprobe.wasm")]
    pub pgprobe: PathBuf,

    /// Postgres connection URL (overrides DATABASE_URL / WAMN_PG_URL)
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL for the [2.2] multiproject gate: provisions the per-project
    /// databases (wamn_app is NOSUPERUSER/NOCREATEDB, like production). Only the
    /// `multiproject` mode uses it.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Which measurement/gate to run
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Concurrent guest workers for the qps phase
    #[arg(long, default_value_t = 24)]
    pub concurrency: usize,

    /// qps phase duration (seconds)
    #[arg(long, default_value_t = 8)]
    pub duration_secs: u64,

    /// Pool max size (also passed to the plugin)
    #[arg(long, default_value_t = 16)]
    pub pool_max: usize,

    /// Chaos-gate iterations
    #[arg(long, default_value_t = 100)]
    pub chaos_iters: usize,

    /// RLS-gate randomized attempts
    #[arg(long, default_value_t = 10_000)]
    pub rls_iters: usize,

    /// Injection-gate randomized attempts
    #[arg(long, default_value_t = 10_000)]
    pub injection_iters: usize,
}

/// Deterministic xorshift64* — reproducible randomness without a rand crate
/// (and the workflow-sandbox ban on Math.random doesn't apply to the host,
/// but determinism makes gate failures reproducible).
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";

/// A checkout is claim-free when no *active tenant* is set. Postgres reverts a
/// custom GUC (`app.tenant`) to the empty string — not NULL — once it has been
/// `SET LOCAL` in a session, so a connection that previously served a real
/// tenant reads back `Some("")` when idle. An empty claim grants nothing: RLS
/// policies compare `tenant_id = current_setting('app.tenant', true)`, and no
/// row's tenant is the empty string. A *non-empty* residual claim would be a
/// leak.
fn claim_is_clean(claim: &Option<String>) -> bool {
    claim.as_deref().unwrap_or("").is_empty()
}

/// A guest instance bound to a tenant identity, ready to run `run(op, arg)`.
struct Worker {
    store: Store<SharedCtx>,
    func: TypedFunc<(u32, String), (Result<u64, String>,)>,
}

impl Worker {
    async fn call(&mut self, op: u32, arg: &str) -> anyhow::Result<Result<u64, String>> {
        let (ret,) = self
            .func
            .call_async(&mut self.store, (op, arg.to_string()))
            .await?;
        Ok(ret)
    }
}

/// Everything needed to spin up `Worker`s: the compiled+linked guest and the
/// shared plugin.
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
            .map_err(|e| anyhow::anyhow!("compile pgprobe: {e}"))?;
        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
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

    /// Build a worker whose guest carries the given component identity (the
    /// plugin maps that id to a tenant claim). `deadline` sets a per-store
    /// epoch deadline (chaos gate); None leaves the generous default.
    async fn worker(&self, component_id: &str, deadline: Option<u64>) -> anyhow::Result<Worker> {
        let ctx = Ctx::builder(component_id.to_string(), component_id.to_string())
            .with_plugins(self.plugin_map())
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(deadline.unwrap_or(u64::MAX / 2));
        let instance = self.pre.instantiate_async(&mut store).await?;
        let func =
            instance.get_typed_func::<(u32, String), (Result<u64, String>,)>(&mut store, "run")?;
        Ok(Worker { store, func })
    }
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

pub async fn run(args: PgBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let guest = std::fs::read(&args.pgprobe)
        .with_context(|| format!("failed to read {}", args.pgprobe.display()))?;

    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    cfg.pool_max_size = args.pool_max;
    if cfg.database_url.is_none() {
        bail!("no database url: pass --database-url or set DATABASE_URL / WAMN_PG_URL");
    }

    println!("# wamn-host S2 pgbench");
    println!(
        "pool_max = {}, statement_timeout = {} ms, row_limit = {}",
        cfg.pool_max_size, cfg.statement_timeout_ms, cfg.row_limit
    );

    // The plugin outlives every store; register both PoC tenant identities.
    let plugin = Arc::new(WamnPostgres::new(cfg.clone())?);
    plugin.set_tenant(TENANT_A, TENANT_A)?;
    plugin.set_tenant(TENANT_B, TENANT_B)?;

    // Preflight: fail fast with a clear message if the DB/fixture is missing.
    preflight(&plugin).await.context("preflight failed")?;

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let harness = Harness::new(engine, &guest, plugin.clone())?;

    let run_all = args.mode == Mode::All;
    let mut pass = true;
    if run_all || args.mode == Mode::Qps {
        pass &= qps_phase(&harness, &args).await?;
    }
    if run_all || args.mode == Mode::Saturation {
        pass &= saturation_phase(&harness, &args).await?;
    }
    if run_all || args.mode == Mode::Chaos {
        pass &= chaos_phase(&harness, &plugin, &args).await?;
    }
    if run_all || args.mode == Mode::Rls {
        pass &= rls_phase(&harness, &args).await?;
    }
    if run_all || args.mode == Mode::Injection {
        pass &= injection_phase(&harness, &args).await?;
    }
    if run_all || args.mode == Mode::Multiproject {
        if args.admin_database_url.is_some() {
            pass &= multiproject_phase(&guest, &cfg, &args).await?;
        } else if args.mode == Mode::Multiproject {
            bail!("multiproject mode needs --admin-database-url / WAMN_PG_ADMIN_URL");
        } else {
            println!(
                "\n(skipping [2.2] multiproject gate: no --admin-database-url / WAMN_PG_ADMIN_URL)"
            );
        }
    }

    ticker.abort();
    println!("\npgbench complete — overall PASS: {pass}");
    if !pass {
        bail!("one or more S2 gates failed");
    }
    Ok(())
}

/// Cheap connectivity + fixture check before spinning up the engine.
async fn preflight(plugin: &Arc<WamnPostgres>) -> anyhow::Result<()> {
    let probe = plugin.probe_checkout().await?;
    anyhow::ensure!(
        claim_is_clean(&probe.tenant_claim),
        "fresh connection already carries a tenant claim: {:?}",
        probe.tenant_claim
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// qps
// ---------------------------------------------------------------------------

async fn qps_phase(harness: &Harness, args: &PgBenchArgs) -> anyhow::Result<bool> {
    println!(
        "\n## qps — {} workers, {}s, 8-param single-statement, ≤10-row",
        args.concurrency, args.duration_secs
    );
    let deadline = Instant::now() + Duration::from_secs(args.duration_secs);

    let mut tasks = tokio::task::JoinSet::new();
    for w in 0..args.concurrency {
        let mut worker = harness.worker(TENANT_A, None).await?;
        tasks.spawn(async move {
            let mut samples: Vec<Duration> = Vec::new();
            let mut ops: u64 = 0;
            let mut errors: u64 = 0;
            let mut g = (w as u64 * 7) % 1000;
            while Instant::now() < deadline {
                let start = Instant::now();
                match worker.call(0, &g.to_string()).await {
                    Ok(Ok(_)) => {
                        samples.push(start.elapsed());
                        ops += 1;
                    }
                    Ok(Err(_)) | Err(_) => errors += 1,
                }
                g = (g + 1) % 1000;
            }
            (ops, errors, samples)
        });
    }

    let started = Instant::now();
    let mut total_ops = 0u64;
    let mut total_err = 0u64;
    let mut all: Vec<Duration> = Vec::new();
    while let Some(res) = tasks.join_next().await {
        let (ops, errors, samples) = res?;
        total_ops += ops;
        total_err += errors;
        all.extend(samples);
    }
    let elapsed = started.elapsed();
    all.sort();

    let qps = total_ops as f64 / elapsed.as_secs_f64();
    println!("ops = {total_ops}, errors = {total_err}, elapsed = {elapsed:?}, qps = {qps:.0}");
    println!(
        "latency p50 = {:?}  p90 = {:?}  p99 = {:?}  max = {:?}",
        percentile(&all, 0.50),
        percentile(&all, 0.90),
        percentile(&all, 0.99),
        all.last().copied().unwrap_or_default(),
    );
    let qps_ok = qps >= 2000.0;
    let p99_ok = percentile(&all, 0.99) < Duration::from_millis(10);
    let pass = qps_ok && p99_ok && total_err == 0;
    println!("PASS(qps ≥ 2000, p99 < 10ms, no errors): {pass} (qps_ok={qps_ok}, p99_ok={p99_ok})");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// saturation
// ---------------------------------------------------------------------------

async fn saturation_phase(harness: &Harness, args: &PgBenchArgs) -> anyhow::Result<bool> {
    // Far more concurrent slow queries than the pool can serve, each holding
    // its connection for `hold` seconds. With demand ≈ overcommit/pool × hold
    // well over the pool's wait timeout, the backlog cannot drain in time, so
    // excess checkouts must return connection-unavailable — never hang. Bound
    // the whole phase so a hang would surface as a blown deadline.
    let overcommit = args.pool_max * 6;
    let hold = "1.0";
    println!(
        "\n## saturation — {overcommit} concurrent {hold}s queries over a {}-connection pool",
        args.pool_max
    );

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..overcommit {
        let mut worker = harness.worker(TENANT_A, None).await?;
        tasks.spawn(async move {
            let start = Instant::now();
            let r = worker.call(6, hold).await;
            (start.elapsed(), r)
        });
    }

    let phase_deadline = Duration::from_secs(30);
    let mut ok = 0u64;
    let mut unavailable = 0u64;
    let mut other = 0u64;
    let mut worst = Duration::ZERO;
    let overall = Instant::now();
    while let Some(res) = tasks.join_next().await {
        let (elapsed, r) = res?;
        worst = worst.max(elapsed);
        match r {
            Ok(Ok(_)) => ok += 1,
            Ok(Err(e)) if e == "connection-unavailable" => unavailable += 1,
            Ok(Err(_)) | Err(_) => other += 1,
        }
    }
    let no_hang = overall.elapsed() < phase_deadline;
    if let Some((size, available, waiting)) = harness.plugin.pool_status() {
        println!(
            "pool after saturation: size = {size}, available = {available}, waiting = {waiting}"
        );
    }
    println!(
        "served = {ok}, connection-unavailable = {unavailable}, other-errors = {other}, worst call = {worst:?}"
    );
    // Graceful = pool served up to capacity, the rest were told
    // connection-unavailable, and nothing hung past the bound.
    let pass = no_hang && unavailable > 0 && other == 0 && ok > 0;
    println!(
        "PASS(saturation graceful, no hang): {pass} (no_hang={no_hang}, some_unavailable={})",
        unavailable > 0
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// chaos
// ---------------------------------------------------------------------------

async fn chaos_phase(
    harness: &Harness,
    plugin: &Arc<WamnPostgres>,
    args: &PgBenchArgs,
) -> anyhow::Result<bool> {
    // Epoch deadline generous enough that begin()+SET LOCAL (a DB round trip)
    // always completes, but short enough to kill the busy-loop quickly.
    const KILL_TICKS: u64 = 20; // ~200 ms at the 10 ms tick.
    println!(
        "\n## chaos — epoch-kill mid-transaction {}× (deadline {KILL_TICKS} ticks)",
        args.chaos_iters
    );

    let destroyed_before = plugin.destroyed_connections();
    // Distinct backend pids seen in post-kill probes: a growing set is the
    // observable pool churn (new connections replacing destroyed ones).
    let mut probe_pids: std::collections::HashSet<i32> = std::collections::HashSet::new();
    let mut all_interrupted = true;
    let mut all_clean = true;

    for i in 0..args.chaos_iters {
        // Fresh store with a short deadline; guest begins a txn then busyloops.
        let mut worker = harness.worker(TENANT_A, Some(KILL_TICKS)).await?;
        let result = worker.call(1, &i.to_string()).await;
        let interrupted = match result {
            Ok(_) => false, // op 1 must never return
            Err(e) => matches!(e.downcast_ref::<Trap>(), Some(Trap::Interrupt)),
        };
        all_interrupted &= interrupted;
        // Drop the store: teardown runs the transaction resource's Drop, which
        // takes the connection out of the pool (never repooled) and closes it.
        drop(worker);

        // Every subsequent checkout must be clean: no leaked claim from the
        // killed transaction's SET LOCAL, and no open (idle-in-transaction)
        // state carried over.
        let probe = plugin.probe_checkout().await?;
        probe_pids.insert(probe.backend_pid);
        if !claim_is_clean(&probe.tenant_claim) || probe.xact_id.is_some() {
            all_clean = false;
            tracing::warn!(?probe, "post-kill checkout was not clean");
        }
    }

    let destroyed = plugin.destroyed_connections() - destroyed_before;
    let final_probe = plugin.probe_checkout().await?;
    let final_clean = claim_is_clean(&final_probe.tenant_claim) && final_probe.xact_id.is_none();

    println!(
        "interrupted = {all_interrupted}, post-kill checkouts clean = {all_clean}, connections destroyed = {destroyed}/{}",
        args.chaos_iters
    );
    println!(
        "distinct fresh backend pids after kills = {} (pool churn observable), final checkout clean = {final_clean}",
        probe_pids.len()
    );
    // Reuse is impossible by construction: Drop calls deadpool Object::take,
    // which removes the object from pool accounting before closing it, so it
    // can never be handed out again. The destroyed count proves take() ran.
    let pass = all_interrupted && all_clean && final_clean && destroyed >= args.chaos_iters as u64;
    println!("PASS(chaos: killed mid-txn, clean checkouts, connections destroyed): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// rls
// ---------------------------------------------------------------------------

async fn rls_phase(harness: &Harness, args: &PgBenchArgs) -> anyhow::Result<bool> {
    println!(
        "\n## rls — {} randomized cross-tenant attempts (2 identities, 1 table)",
        args.rls_iters
    );
    let mut a = harness.worker(TENANT_A, None).await?;
    let mut b = harness.worker(TENANT_B, None).await?;
    let mut rng = Rng::new(0x5253_4c53_5f47_3432);

    // Sanity: each identity sees its OWN rows (proves RLS isn't just empty).
    let a_own = a
        .call(2, "secret-tenant-a-%")
        .await?
        .map_err(|e| anyhow::anyhow!("tenant-a self-read: {e}"))?;
    let b_own = b
        .call(2, "secret-tenant-b-%")
        .await?
        .map_err(|e| anyhow::anyhow!("tenant-b self-read: {e}"))?;
    println!("sanity: A sees {a_own} of its own, B sees {b_own} of its own");
    if a_own == 0 || b_own == 0 {
        println!("PASS(rls): false (own-row sanity failed — fixture/claims broken)");
        return Ok(false);
    }

    let mut leaks: u64 = 0;
    let mut denied_writes: u64 = 0;
    let mut write_attempts: u64 = 0;
    let mut unexpected: u64 = 0;

    for _ in 0..args.rls_iters {
        let a_side = rng.next_u64() & 1 == 0;
        // Randomize the foreign pattern to defeat any pattern-specific luck.
        let salt = rng.below(100000);
        let (worker, foreign) = if a_side {
            (&mut a, TENANT_B)
        } else {
            (&mut b, TENANT_A)
        };
        // Attempt to read the OTHER tenant's secrets by pattern.
        let pattern = format!("secret-{foreign}-{salt}%");
        match worker.call(2, &pattern).await? {
            Ok(0) => {}
            Ok(n) => {
                leaks += n;
                tracing::error!(foreign, n, "RLS LEAK: cross-tenant rows visible");
            }
            Err(e) => {
                unexpected += 1;
                tracing::warn!(error = e, "unexpected rls read error");
            }
        }
        // Occasionally attempt a cross-tenant WRITE (must be permission-denied).
        if salt.is_multiple_of(10) {
            write_attempts += 1;
            match worker.call(3, foreign).await? {
                Err(e) if e == "permission-denied" => denied_writes += 1,
                Ok(_) => {
                    leaks += 1; // a successful cross-tenant write is a breach
                    tracing::error!(foreign, "RLS BREACH: cross-tenant write succeeded");
                }
                Err(e) => {
                    unexpected += 1;
                    tracing::warn!(error = e, "cross-tenant write gave unexpected error");
                }
            }
        }
    }

    println!(
        "cross-tenant rows leaked = {leaks}, cross-tenant writes denied = {denied_writes}/{write_attempts}, unexpected errors = {unexpected}"
    );
    let pass =
        leaks == 0 && unexpected == 0 && denied_writes == write_attempts && write_attempts > 0;
    println!("PASS(rls: zero leakage, writes denied, no detail): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// injection
// ---------------------------------------------------------------------------

async fn injection_phase(harness: &Harness, args: &PgBenchArgs) -> anyhow::Result<bool> {
    println!(
        "\n## injection — {} param fragments, byte-identical round-trip",
        args.injection_iters
    );
    let mut worker = harness.worker(TENANT_A, None).await?;

    // A base corpus of classic injection payloads; the loop also generates
    // randomized variants so coverage isn't just these literals.
    let corpus = [
        "'; DROP TABLE s2.scratch; --",
        "' OR '1'='1",
        "'); DELETE FROM s2.rls_secrets; --",
        "\\'; SELECT pg_sleep(10); --",
        "$$; SELECT 1; $$",
        "%s %d {0} ${x}",
        "line1\nline2\ttab",
        "unicode ☃ 💥 \u{202e}",
        "quote\" and 'apostrophe' and `backtick`",
        "",
        "0x00 not a null but text",
        "'; SET ROLE postgres; --",
    ];
    let mut rng = Rng::new(0x494e_4a45_4354_4e00);
    let mut mismatches: u64 = 0;
    let mut errors: u64 = 0;

    for i in 0..args.injection_iters {
        let base = corpus[i % corpus.len()];
        // Half the attempts use a randomized fragment to broaden coverage.
        let payload = if rng.next_u64() & 1 == 0 {
            base.to_string()
        } else {
            let salt = rng.below(1_000_000);
            format!("{base}::{salt}'; --\u{1f4a5}")
        };
        match worker.call(4, &payload).await? {
            Ok(1) => {}
            Ok(_) => {
                mismatches += 1;
                tracing::error!(payload, "INJECTION: round-trip not byte-identical");
            }
            Err(e) => {
                errors += 1;
                tracing::error!(payload, error = e, "injection attempt errored");
            }
        }
    }

    // The scratch table must still exist and be well-formed (a successful
    // DROP/DELETE injection would have blown it away).
    let intact = worker.call(4, "post-injection-canary").await?;
    let table_ok = matches!(intact, Ok(1));

    println!("mismatches = {mismatches}, errors = {errors}, table intact = {table_ok}");
    let pass = mismatches == 0 && errors == 0 && table_ok;
    println!("PASS(injection: params are data, never SQL): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// [2.2] multiproject gate: per-project pooling + credential resolution + policy
// ---------------------------------------------------------------------------

const PROJ_A: &str = "proj-a";
const PROJ_B: &str = "proj-b";
const COMP_A: &str = "comp-a";
const COMP_B: &str = "comp-b";
const DB_A: &str = "wamn_p_a";
const DB_B: &str = "wamn_p_b";
const MARKER_A: i32 = 111;
const MARKER_B: i32 = 222;

/// Replace the database name (URL path) while preserving any `?params`.
fn swap_db(url: &str, db: &str) -> anyhow::Result<String> {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let slash = base.rfind('/').context("connection url has no path")?;
    let mut out = format!("{}/{db}", &base[..slash]);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Ok(out)
}

/// Provision one project database as superuser (idempotent): (re)create it,
/// grant `wamn_app` CONNECT, then create the `marker` (routing witness) and
/// `items` (FORCE-RLS) tables and seed them. Mirrors production, where the app
/// role cannot create databases — only the operator can.
async fn provision_project(
    admin_url: &str,
    db: &str,
    marker: i32,
    seeds: &[(&str, i32)],
) -> anyhow::Result<()> {
    // 1. (Re)create the database. CREATE/DROP DATABASE must each be their own
    //    autocommit statement (they cannot run inside a transaction block).
    {
        let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
            .await
            .context("connect admin url")?;
        let handle = tokio::spawn(conn);
        client
            .batch_execute(&format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)"))
            .await?;
        client
            .batch_execute(&format!("CREATE DATABASE {db}"))
            .await?;
        client
            .batch_execute(&format!("GRANT CONNECT ON DATABASE {db} TO wamn_app"))
            .await?;
        drop(client);
        let _ = handle.await;
    }
    // 2. Tables + policies + seed, inside the new database (as superuser).
    let db_admin_url = swap_db(admin_url, db)?;
    let (client, conn) = tokio_postgres::connect(&db_admin_url, NoTls)
        .await
        .context("connect new project db")?;
    let handle = tokio::spawn(conn);
    let mut ddl = format!(
        "CREATE TABLE marker (n int not null); \
         INSERT INTO marker VALUES ({marker}); \
         GRANT SELECT ON marker TO wamn_app; \
         CREATE TABLE items (id bigserial primary key, tenant_id text not null, body text); \
         ALTER TABLE items ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE items FORCE ROW LEVEL SECURITY; \
         CREATE POLICY items_tenant ON items \
           USING (tenant_id = current_setting('app.tenant', true)) \
           WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
         GRANT SELECT, INSERT ON items TO wamn_app; \
         GRANT USAGE ON SEQUENCE items_id_seq TO wamn_app;"
    );
    for (tenant, count) in seeds {
        ddl.push_str(&format!(
            " INSERT INTO items (tenant_id, body) SELECT '{tenant}', 'x' FROM generate_series(1,{count});"
        ));
    }
    client.batch_execute(&ddl).await?;
    drop(client);
    let _ = handle.await;
    Ok(())
}

/// The [2.2] gate: two projects on separate databases, resolved through a
/// [`StaticCredentialProvider`], each with its own pool and its own row-limit
/// policy. Proves (a) routing — each component reaches only its own database;
/// (b) per-project policy — the two row limits are enforced independently;
/// (c) RLS still confines within a project; (d) pool isolation — two distinct
/// pools exist.
async fn multiproject_phase(
    guest: &[u8],
    base_cfg: &WamnPostgresConfig,
    args: &PgBenchArgs,
) -> anyhow::Result<bool> {
    println!("\n== [2.2] multiproject: per-project pooling + credentials + policy ==");
    let admin_url = args
        .admin_database_url
        .as_ref()
        .context("multiproject needs an admin url")?;
    let base_url = base_cfg
        .database_url
        .as_ref()
        .context("multiproject needs a base app url (WAMN_PG_URL)")?;

    // Provision two separate project databases (10 rows for proj-a's tenant;
    // proj-b holds 7 of its own tenant's rows plus 5 of a foreign tenant's, so
    // RLS confinement is observable as 7-of-12).
    provision_project(admin_url, DB_A, MARKER_A, &[("t-a", 10)]).await?;
    provision_project(admin_url, DB_B, MARKER_B, &[("t-b", 7), ("t-x", 5)]).await?;

    // Static provider: proj-a gets a SMALL row limit (4), proj-b a large one
    // (1000). Same query, different per-project caps ⇒ the caps are per-project.
    let mk = |url: String, row_limit: u64| ProjectConfig {
        database_url: url,
        pool_max_size: 8,
        wait_timeout_ms: base_cfg.wait_timeout_ms,
        statement_timeout_ms: base_cfg.statement_timeout_ms,
        row_limit,
    };
    let mut projects = HashMap::new();
    projects.insert(PROJ_A.to_string(), mk(swap_db(base_url, DB_A)?, 4));
    projects.insert(PROJ_B.to_string(), mk(swap_db(base_url, DB_B)?, 1000));
    let provider: Arc<dyn CredentialProvider> =
        Arc::new(StaticCredentialProvider::new(projects, None));
    let plugin = Arc::new(WamnPostgres::with_provider(provider));
    plugin.set_tenant(COMP_A, "t-a")?;
    plugin.set_project(COMP_A, PROJ_A)?;
    plugin.set_tenant(COMP_B, "t-b")?;
    plugin.set_project(COMP_B, PROJ_B)?;

    let engine = build_engine(&[])?;
    let harness = Harness::new(engine, guest, plugin.clone())?;
    let mut wa = harness.worker(COMP_A, None).await?;
    let mut wb = harness.worker(COMP_B, None).await?;

    // Routing witness: each component reads its own database's marker.
    let marker_a = wa.call(11, "").await?;
    let marker_b = wb.call(11, "").await?;
    // Per-project policy + RLS: proj-a's 10 rows trip its row-limit of 4;
    // proj-b's tenant sees 7 of 12 rows (RLS) under its large limit.
    let items_a = wa.call(10, "").await?;
    let items_b = wb.call(10, "").await?;

    let route_ok = marker_a == Ok(MARKER_A as u64) && marker_b == Ok(MARKER_B as u64);
    let policy_ok = matches!(&items_a, Err(e) if e == "row-limit-exceeded:4");
    let rls_ok = items_b == Ok(7);
    let pools = plugin.project_pool_count();
    let isolation_ok = pools == 2
        && plugin.pool_status_of(PROJ_A).is_some()
        && plugin.pool_status_of(PROJ_B).is_some();

    println!(
        "routing:   comp-a marker={marker_a:?} (want Ok({MARKER_A})), comp-b marker={marker_b:?} (want Ok({MARKER_B})) -> {route_ok}"
    );
    println!(
        "policy:    comp-a count-items={items_a:?} (want row-limit-exceeded:4) -> {policy_ok}"
    );
    println!("rls:       comp-b count-items={items_b:?} (want Ok(7) of 12 rows) -> {rls_ok}");
    println!("isolation: live pools={pools} (want 2, one per project) -> {isolation_ok}");

    let pass = route_ok && policy_ok && rls_ok && isolation_ok;
    println!("PASS([2.2] per-project pooling + credentials + policy): {pass}");
    Ok(pass)
}
