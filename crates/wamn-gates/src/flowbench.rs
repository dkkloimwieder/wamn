//! The `flowbench` subcommand: the three S3 flow-runner gates
//! (docs/p0-exit-criteria.md S3).
//!
//! Like `pgbench`, this instantiates a guest — here `flowrunner`, which embeds
//! the standard node library as native Rust and imports `wamn:postgres/client`
//! — into a hand-built [`SharedCtx`] store with the real plugin linked, then
//! drives its exports. Working at the store level lets the harness time
//! dispatch, set per-store epoch deadlines to kill a runner mid-run, and read
//! back run state directly.
//!
//! Gates:
//!   dispatch  — standard-node dispatch overhead p99 < 50us (same-binary call).
//!               Pure in-component walks; no DB, no host boundary per node.
//!   hotreload — flip the active catalog version; the new version is live in
//!               < 1s (catalog re-read; the production doorbell is NATS,
//!               wamn-m2z [5.14]).
//!   resume    — epoch-kill a runner after its side effect commits but before
//!               its checkpoint; a fresh instance resumes and the run leaves
//!               exactly one side-effect row (idempotency via run_id+step).
//!   all       — every gate in sequence.
//!
//! PoC shortcuts and where the real work is tracked: catalog re-read instead of
//! a NATS doorbell -> wamn-m2z [5.14]; minimal ad-hoc flow JSON -> wamn-34t
//! [5.1]; webhook-in/respond modeled as walk input/return (no HTTP server) ->
//! trigger dispatch in wamn-m2z [5.14] + production runner wamn-uyd [5.2].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{
    Component as WasmtimeComponent, InstancePre, Linker, TypedFunc,
};
use wash_runtime::wasmtime::{Engine as RawEngine, Store, Trap};

use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Standard-node dispatch overhead (no DB).
    Dispatch,
    /// Catalog version flip visible in < 1s.
    Hotreload,
    /// Kill-mid-run resume with idempotent side effects.
    Resume,
    /// Every gate in sequence.
    All,
}

#[derive(Debug, Args)]
pub struct FlowBenchArgs {
    /// Path to the flowrunner guest component
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: PathBuf,

    /// Postgres connection URL (overrides DATABASE_URL / WAMN_PG_URL). Not
    /// needed for `--mode dispatch`, which never touches the database.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Which gate to run
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Standard-only graph walks for the dispatch gate (× 5 nodes = samples)
    #[arg(long, default_value_t = 200_000)]
    pub dispatch_iters: u32,

    /// Version flips measured by the hot-reload gate
    #[arg(long, default_value_t = 5)]
    pub hotreload_iters: usize,

    /// Kill-then-resume cycles for the resume gate
    #[arg(long, default_value_t = 10)]
    pub resume_iters: usize,

    /// Pool max size (passed to the plugin)
    #[arg(long, default_value_t = 8)]
    pub pool_max: usize,
}

/// The single tenant identity the runner executes under; the host maps this
/// component id to the `app.tenant` claim (see [`WamnPostgres::set_tenant`]).
const FLOW_TENANT: &str = "flow-tenant";

/// Epoch deadline for the kill-window store: generous enough that the pre-kill
/// DB work (load graph, open run, two checkpoints, the sink write) always
/// completes so the busy-loop is actually reached, short enough to kill it
/// promptly. ~600 ms at the 10 ms tick, versus ~15 ms of DB round trips.
const KILL_TICKS: u64 = 60;

/// The dispatch bench's return tuple: (dispatch-count, mean-ns, p50-ns,
/// p99-ns, max-ns).
type DispatchStats = (u64, u64, u64, u64, u64);

/// A flowrunner instance with its export table resolved.
struct Worker {
    store: Store<SharedCtx>,
    dispatch_bench: TypedFunc<(u32,), (DispatchStats,)>,
    seed: TypedFunc<(), (Result<u32, String>,)>,
    set_active: TypedFunc<(u32,), (Result<(), String>,)>,
    active_version: TypedFunc<(), (Result<u32, String>,)>,
    run: TypedFunc<(String, String), (Result<u32, String>,)>,
    run_until_kill: TypedFunc<(String, String), (Result<u32, String>,)>,
    sink_count: TypedFunc<(String,), (Result<u64, String>,)>,
    reset: TypedFunc<(String,), (Result<u64, String>,)>,
}

impl Worker {
    async fn dispatch(&mut self, iters: u32) -> anyhow::Result<DispatchStats> {
        let (t,) = self
            .dispatch_bench
            .call_async(&mut self.store, (iters,))
            .await?;
        Ok(t)
    }
    async fn call_seed(&mut self) -> anyhow::Result<u32> {
        let (r,) = self.seed.call_async(&mut self.store, ()).await?;
        r.map_err(|e| anyhow::anyhow!("seed: {e}"))
    }
    async fn call_set_active(&mut self, v: u32) -> anyhow::Result<()> {
        let (r,) = self.set_active.call_async(&mut self.store, (v,)).await?;
        r.map_err(|e| anyhow::anyhow!("set-active: {e}"))
    }
    async fn call_active_version(&mut self) -> anyhow::Result<u32> {
        let (r,) = self.active_version.call_async(&mut self.store, ()).await?;
        r.map_err(|e| anyhow::anyhow!("active-version: {e}"))
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

/// The compiled+linked guest and the shared plugin.
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
        // The runner also imports wasi:http (the S6 http-call node). The S3
        // flows never call it, but the import must be linkable to instantiate.
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

    async fn worker(&self, deadline: Option<u64>) -> anyhow::Result<Worker> {
        let ctx = Ctx::builder(FLOW_TENANT.to_string(), FLOW_TENANT.to_string())
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
        let dispatch_bench = f!("dispatch-bench");
        let seed = f!("seed");
        let set_active = f!("set-active");
        let active_version = f!("active-version");
        let run = f!("run");
        let run_until_kill = f!("run-until-kill");
        let sink_count = f!("sink-count");
        let reset = f!("reset");
        Ok(Worker {
            store,
            dispatch_bench,
            seed,
            set_active,
            active_version,
            run,
            run_until_kill,
            sink_count,
            reset,
        })
    }
}

pub async fn run(args: FlowBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("failed to read {}", args.flowrunner.display()))?;

    let run_all = args.mode == Mode::All;
    let db_needed = run_all || matches!(args.mode, Mode::Hotreload | Mode::Resume);

    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    cfg.pool_max_size = args.pool_max;
    if db_needed && cfg.database_url.is_none() {
        bail!("no database url: pass --database-url or set DATABASE_URL / WAMN_PG_URL");
    }

    println!("# wamn-host S3 flowbench");

    // The plugin outlives every store; register the runner's tenant identity
    // and its schema. The runner uses unqualified table names; the S3 fixture
    // tables live in schema `s3`, so the host injects `search_path = s3`.
    let plugin = Arc::new(WamnPostgres::new(cfg.clone())?);
    plugin.set_tenant(FLOW_TENANT, FLOW_TENANT)?;
    plugin.set_schema(FLOW_TENANT, "s3")?;

    if db_needed {
        preflight(&plugin).await.context("preflight failed")?;
    }

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let harness = Harness::new(engine, &guest, plugin.clone())?;

    let mut pass = true;
    if run_all || args.mode == Mode::Dispatch {
        pass &= dispatch_phase(&harness, &args).await?;
    }
    if run_all || args.mode == Mode::Hotreload {
        pass &= hotreload_phase(&harness, &args).await?;
    }
    if run_all || args.mode == Mode::Resume {
        pass &= resume_phase(&harness, &args).await?;
    }

    ticker.abort();
    println!("\nflowbench complete — overall PASS: {pass}");
    if !pass {
        bail!("one or more S3 gates failed");
    }
    Ok(())
}

async fn preflight(plugin: &Arc<WamnPostgres>) -> anyhow::Result<()> {
    // Connectivity only; the phases seed their own fixture rows.
    let _ = plugin.probe_checkout().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------------

async fn dispatch_phase(harness: &Harness, args: &FlowBenchArgs) -> anyhow::Result<bool> {
    println!(
        "\n## dispatch — {} standard-only graph walks (× 5 nodes), same-binary",
        args.dispatch_iters
    );
    let mut w = harness.worker(None).await?;
    let (count, mean, p50, p99, max) = w.dispatch(args.dispatch_iters).await?;
    println!(
        "dispatches = {count}, mean = {mean} ns (amortized), p50 = {p50} ns, p99 = {p99} ns, max = {max} ns"
    );
    println!("(p50/p99/max each include one monotonic-clock read — conservative upper bounds)");
    let p99_ok = p99 < 50_000; // 50 us
    let pass = p99_ok && count == args.dispatch_iters as u64 * 5;
    println!(
        "PASS(dispatch p99 < 50us): {pass} (p99 = {:.2} us)",
        p99 as f64 / 1000.0
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// hotreload
// ---------------------------------------------------------------------------

async fn hotreload_phase(harness: &Harness, args: &FlowBenchArgs) -> anyhow::Result<bool> {
    println!(
        "\n## hotreload — {} catalog version flips, new version live < 1s",
        args.hotreload_iters
    );
    let mut w = harness.worker(None).await?;
    w.call_seed().await?;
    w.call_set_active(1).await?;

    // Sanity: v1 active, a run executes v1, then flip to v2 and confirm the run
    // executes v2 — proving the flip changes real behavior, not just a pointer.
    let v_now = w.call_active_version().await?;
    w.call_reset("hot-sanity").await?;
    let ran = w.call_run("hot-sanity", "receipt").await?;
    println!("baseline: active = {v_now}, run executed under v{ran}");
    if v_now != 1 || ran != 1 {
        println!("PASS(hotreload): false (baseline not on v1)");
        return Ok(false);
    }

    let mut worst = Duration::ZERO;
    let mut behavior_ok = true;
    for i in 0..args.hotreload_iters {
        let target = if i % 2 == 0 { 2 } else { 1 };
        let flip = Instant::now();
        w.call_set_active(target).await?;
        // Doorbell PoC: re-read the active version until the flip is observed.
        loop {
            if w.call_active_version().await? == target {
                break;
            }
        }
        let observed = flip.elapsed();
        worst = worst.max(observed);

        // A fresh run must now execute the newly-active version's behavior.
        let run_id = format!("hot-{i}");
        w.call_reset(&run_id).await?;
        let ran = w.call_run(&run_id, "receipt").await?;
        if ran != target {
            behavior_ok = false;
            tracing::error!(target, ran, "hot-reload: run executed the wrong version");
        }
        println!("flip -> v{target}: live in {observed:?}, run executed under v{ran}");
    }

    let time_ok = worst < Duration::from_secs(1);
    let pass = time_ok && behavior_ok;
    println!(
        "worst flip->live = {worst:?}; PASS(hotreload < 1s, behavior changed): {pass} (time_ok={time_ok}, behavior_ok={behavior_ok})"
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// resume
// ---------------------------------------------------------------------------

async fn resume_phase(harness: &Harness, args: &FlowBenchArgs) -> anyhow::Result<bool> {
    println!(
        "\n## resume — {} kill-mid-run cycles, exactly-one side effect (idempotent)",
        args.resume_iters
    );
    // Deterministic version for the resume gate.
    let mut setup = harness.worker(None).await?;
    setup.call_seed().await?;
    setup.call_set_active(1).await?;

    let mut clean_kills = 0usize; // epoch trap actually fired
    let mut duplicate_absorbed = 0usize; // side effect committed pre-kill, then re-run absorbed
    let mut all_single = true; // post-resume count == 1 for every cycle
    let mut completed = 0usize;

    for i in 0..args.resume_iters {
        let run_id = format!("resume-{i}");
        setup.call_reset(&run_id).await?;

        // Attempt 1: run into the kill window, then epoch-kill the store.
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

        // Did the side effect commit before the kill? (Proves the resume path
        // will face a genuine duplicate.)
        let pre = setup.call_sink_count(&run_id).await?;
        if pre == 1 {
            duplicate_absorbed += 1;
        }

        // Attempt 2: a fresh instance resumes from the checkpoint.
        let mut resumer = harness.worker(None).await?;
        let ran = resumer.call_run(&run_id, "receipt").await?;
        let post = setup.call_sink_count(&run_id).await?;
        if post == 1 {
            completed += 1;
        } else {
            all_single = false;
            tracing::error!(run_id, post, "resume left != 1 side-effect rows");
        }
        drop(resumer);
        let _ = ran;
    }

    println!(
        "clean kills = {clean_kills}/{n}, side effect committed pre-kill = {duplicate_absorbed}/{n}, resumed-to-single-row = {completed}/{n}",
        n = args.resume_iters
    );
    // Every cycle must: trap cleanly, and end with exactly one side-effect row;
    // and the duplicate-absorb path must be exercised (pre-kill commit) so the
    // gate proves idempotency, not just that we never double-ran.
    let pass = all_single
        && clean_kills == args.resume_iters
        && completed == args.resume_iters
        && duplicate_absorbed == args.resume_iters;
    println!("PASS(resume: killed mid-run, single idempotent side effect): {pass}");
    Ok(pass)
}
