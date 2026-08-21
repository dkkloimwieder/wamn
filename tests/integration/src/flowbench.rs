//! The `flowbench` subcommand: the two retained S3 flow-runner gates
//! (docs/archive/p0-exit-criteria.md S3).
//!
//! Like `pgbench`, this instantiates a guest — here `flowrunner`, which embeds
//! the standard node library as native Rust and imports `wamn:postgres/client`
//! — into a hand-built [`SharedCtx`] store with the real plugin linked, then
//! drives its exports. Working at the store level lets the harness time
//! dispatch and read back run state directly.
//!
//! Gates:
//!   dispatch  — standard-node dispatch overhead p99 < 50us (same-binary call).
//!               Pure in-component walks; no DB, no host boundary per node.
//!   hotreload — flip the active catalog version; the new version is live in
//!               < 1s (catalog re-read; the production doorbell is NATS,
//!               wamn-m2z [5.14]).
//!   all       — every gate in sequence.
//!
//! PoC shortcuts and where the real work is tracked: catalog re-read instead of
//! a NATS doorbell -> wamn-m2z [5.14]; minimal ad-hoc flow JSON -> wamn-34t
//! [5.1]; request/respond modeled as walk input/return (no HTTP server) ->
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
use wash_runtime::wasmtime::{Engine as RawEngine, Store};

use crate::flowrunner_linker::add_flowrunner_imports_to_linker;
use tokio_postgres::NoTls;
use wamn_gate_harness::{percentile, scope_session, seed_flow_version, set_active_flow_version};
use wamn_runtime::engine::build_engine;
use wamn_runtime::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Standard-node dispatch overhead (no DB).
    Dispatch,
    /// Catalog version flip visible in < 1s.
    Hotreload,
    /// Every gate in sequence.
    All,
}

#[derive(Debug, Args)]
pub struct FlowBenchArgs {
    /// Path to the flowrunner guest component
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: PathBuf,

    /// Postgres connection URL (overrides WAMN_PG_URL). Not
    /// needed for `--mode dispatch`, which never touches the database.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Which gate to run
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Standard-only graph walks for the dispatch gate
    #[arg(long, default_value_t = 200_000)]
    pub dispatch_iters: u32,

    /// Version flips measured by the hot-reload gate
    #[arg(long, default_value_t = 5)]
    pub hotreload_iters: usize,

    /// Guest pool max size (passed to the plugin)
    #[arg(long, default_value_t = 8)]
    pub pool_max: usize,
}

/// The single tenant identity the runner executes under; the host maps this
/// component id to the `app.tenant` claim (see [`WamnPostgres::set_tenant`]).
const FLOW_TENANT: &str = "flow-tenant";

/// The S3 fixture flow id (the guest's `run`/`active-version` exports drive
/// this flow; the JSON now lives HERE — a production component carries no
/// bench fixtures, SR2).
pub const FIXTURE_FLOW_ID: &str = "poc-receipt";

/// The stored S3 flow for a version. v1 upper-cases the payload, v2 reverses
/// it — distinct enough that the hot-reload gate can see which version ran.
pub fn flow_json(version: u32) -> String {
    let op = if version == 1 { "upper" } else { "reverse" };
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{FIXTURE_FLOW_ID}","version":{version},
            "nodes":[
              {{"id":"in","type":"request","config":{{"input-schema":true}}}},
              {{"id":"t","type":"transform","config":{{"op":"{op}"}}}},
              {{"id":"w","type":"pg-write"}},
              {{"id":"c","type":"conditional","config":{{"min-len":3}}}},
              {{"id":"out","type":"respond","config":{{"status":200}}}}
            ],
            "edges":[{{"from":"in","to":"t"}},{{"from":"t","to":"w"}},
                     {{"from":"w","to":"c"}},{{"from":"c","to":"out"}}]}}"#
    )
}

/// Open a fixture-scoped app connection (tenant claim + s3 search_path) and
/// seed the two S3 flow versions with v1 active — the host-side replacement
/// for the guest's retired `seed`/`set-active` exports.
async fn fixture_client(
    db_url: &str,
) -> anyhow::Result<(tokio_postgres::Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(db_url, NoTls)
        .await
        .context("fixture connect")?;
    let task = tokio::spawn(async move {
        let _ = conn.await;
    });
    scope_session(&client, FLOW_TENANT, "s3").await?;
    Ok((client, task))
}

async fn seed_fixture_flows(client: &tokio_postgres::Client) -> anyhow::Result<()> {
    for v in [1, 2] {
        seed_flow_version(
            client,
            FLOW_TENANT,
            FIXTURE_FLOW_ID,
            v,
            v == 1,
            &flow_json(v as u32),
            false,
        )
        .await?;
    }
    set_active_flow_version(client, FLOW_TENANT, FIXTURE_FLOW_ID, 1).await?;
    Ok(())
}

/// The dispatch bench's raw return: (bare-total-ns, per-dispatch samples ns).
/// Stats are computed HERE via the harness (the guest carries none, SR2).
type DispatchRaw = (u64, Vec<u32>);

/// A flowrunner instance with its export table resolved.
struct Worker {
    store: Store<SharedCtx>,
    dispatch_bench: TypedFunc<(u32, String), (Result<DispatchRaw, String>,)>,
    active_version: TypedFunc<(), (Result<u32, String>,)>,
    run: TypedFunc<(String, String), (Result<u32, String>,)>,
    reset: TypedFunc<(String,), (Result<u64, String>,)>,
}

impl Worker {
    async fn dispatch(&mut self, iters: u32, flow_json: &str) -> anyhow::Result<DispatchRaw> {
        let (r,) = self
            .dispatch_bench
            .call_async(&mut self.store, (iters, flow_json.to_string()))
            .await?;
        r.map_err(|e| anyhow::anyhow!("dispatch-bench: {e}"))
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
        // The whole flowrunner import set, registered once for every bench that
        // rolls its own linker. The S3 flows call none of the effectful ones
        // (wasi:http, the vault, wasi:logging, and the trusted HTTP-connection
        // frame), but every import must be linkable to
        // instantiate; the ones `plugin_map` leaves unbacked trap if called.
        add_flowrunner_imports_to_linker(&mut linker)?;
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
        // fqg.11: the flowrunner declares its per-run egress on every walk, so
        // the policy plugin must back the linked interface. Enforcement here is
        // the harness's own http handler, so the declaration is inert — the
        // plugin exists to keep the trusted channel satisfied.
        m.insert(
            wamn_runtime::plugins::runner_egress::RUNNER_EGRESS_ID,
            Arc::new(wamn_runtime::plugins::runner_egress::RunnerEgressPolicy::default())
                as Arc<dyn HostPlugin + Send + Sync>,
        );
        m
    }

    async fn worker(&self) -> anyhow::Result<Worker> {
        let ctx = Ctx::builder(FLOW_TENANT.to_string(), FLOW_TENANT.to_string())
            .with_plugins(self.plugin_map())
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        let instance = self.pre.instantiate_async(&mut store).await?;
        macro_rules! f {
            ($name:literal) => {
                instance.get_typed_func(&mut store, $name)?
            };
        }
        let dispatch_bench = f!("dispatch-bench");
        let active_version = f!("active-version");
        let run = f!("run");
        let reset = f!("reset");
        Ok(Worker {
            store,
            dispatch_bench,
            active_version,
            run,
            reset,
        })
    }
}

pub async fn run(args: FlowBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("failed to read {}", args.flowrunner.display()))?;

    let run_all = args.mode == Mode::All;
    let db_needed = run_all || args.mode == Mode::Hotreload;

    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    cfg.guest_pool_max_size = args.pool_max;
    if db_needed && cfg.database_url.is_none() {
        bail!("no database url: pass --database-url or set WAMN_PG_URL");
    }
    // The resolved URL (args OR env) — the host-side fixture seeding (SR2) opens
    // its own connection with it, so it must accept the env form the in-cluster
    // Jobs use, not just the --database-url flag.
    let db_url = cfg.database_url.clone();

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
    let harness = Harness::new(engine, &guest, plugin.clone())?;

    let mut pass = true;
    if run_all || args.mode == Mode::Dispatch {
        pass &= dispatch_phase(&harness, &args).await?;
    }
    if run_all || args.mode == Mode::Hotreload {
        let url = db_url.as_deref().expect("db_needed guarantees a url");
        pass &= hotreload_phase(&harness, &args, url).await?;
    }
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

/// The unchanged S3 dispatch latency ceiling.
const DISPATCH_P99_NS: u64 = 50_000;

/// Dispatches one walk of `flow` spends.
///
/// Every node type now executes through standard-node dispatch — the wamn-ayq7 series
/// moved `respond` (e937df7), `request` (58c3ed3), `event` (d6f8084), and
/// `fail` (5d99ed0) off the engine-reserved path — so `Plan::next`
/// hands the driver a `Step::Dispatch` for each of them and a linear graph spends
/// exactly one dispatch per node. Until that series the typed entry and the
/// `respond`/`fail` terminals came back as `Step::Reserved` transitions the
/// engine applied itself, which is what the earlier entry/terminal filter here
/// subtracted.
fn walk_dispatches(flow: &wamn_flow::Flow) -> u64 {
    flow.nodes.len() as u64
}

fn dispatch_passes(flow: &wamn_flow::Flow, iterations: u32, count: u64, p99_ns: u64) -> bool {
    p99_ns < DISPATCH_P99_NS && count == u64::from(iterations) * walk_dispatches(flow)
}

#[cfg(test)]
mod dispatch_cardinality_tests {
    use super::{DISPATCH_P99_NS, dispatch_passes, walk_dispatches};

    #[test]
    fn typed_s3_verdict_requires_a_dispatch_for_every_node_per_walk() {
        let flow = wamn_flow::Flow::from_json(
            r#"{
                "schema-version":"0.1",
                "flow-id":"poc-receipt",
                "version":2,
                "nodes":[
                    {"id":"in","type":"request","config":{"input-schema":true}},
                    {"id":"t","type":"transform","config":{"op":"reverse"}},
                    {"id":"w","type":"pg-write"},
                    {"id":"c","type":"conditional","config":{"min-len":3}},
                    {"id":"out","type":"respond","config":{"status":200}}
                ],
                "edges":[
                    {"from":"in","to":"t"},
                    {"from":"t","to":"w"},
                    {"from":"w","to":"c"},
                    {"from":"c","to":"out"}
                ]
            }"#,
        )
        .expect("typed S3 fixture parses");

        assert_eq!(walk_dispatches(&flow), 5);
        assert!(dispatch_passes(&flow, 2, 10, DISPATCH_P99_NS - 1));
        assert!(!dispatch_passes(&flow, 2, 12, DISPATCH_P99_NS - 1));
        // 3 per walk is the pre-wamn-ayq7 count (entry + respond subtracted); the
        // verdict must reject it now that both dispatch through standard-node execution.
        assert!(!dispatch_passes(&flow, 2, 6, DISPATCH_P99_NS - 1));
        assert!(!dispatch_passes(&flow, 2, 10, DISPATCH_P99_NS));
    }
}

async fn dispatch_phase(harness: &Harness, args: &FlowBenchArgs) -> anyhow::Result<bool> {
    let definition = flow_json(2);
    let flow =
        wamn_flow::Flow::from_json(&definition).context("parse the S3 dispatch fixture flow")?;
    let per_walk = walk_dispatches(&flow);
    println!(
        "\n## dispatch — {} standard-only graph walks (× {per_walk} dispatched nodes), same-binary",
        args.dispatch_iters,
    );
    let mut w = harness.worker().await?;
    // The bench walks the v2 fixture flow; the JSON rides in from HERE (SR2).
    let (bare_ns, mut samples) = w.dispatch(args.dispatch_iters, &definition).await?;
    let count = samples.len() as u64;
    let mean = bare_ns / count.max(1);
    samples.sort_unstable();
    let sorted: Vec<Duration> = samples
        .iter()
        .map(|&ns| Duration::from_nanos(ns as u64))
        .collect();
    let p50 = percentile(&sorted, 0.50).as_nanos() as u64;
    let p99 = percentile(&sorted, 0.99).as_nanos() as u64;
    let max = sorted.last().copied().unwrap_or(Duration::ZERO).as_nanos() as u64;
    println!(
        "dispatches = {count}, mean = {mean} ns (amortized), p50 = {p50} ns, p99 = {p99} ns, max = {max} ns"
    );
    println!("(p50/p99/max each include one monotonic-clock read — conservative upper bounds)");
    let expected = u64::from(args.dispatch_iters) * per_walk;
    let pass = dispatch_passes(&flow, args.dispatch_iters, count, p99);
    println!(
        "PASS(dispatch p99 < 50us, dispatches = {expected}): {pass} (p99 = {:.2} us)",
        p99 as f64 / 1000.0
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// hotreload
// ---------------------------------------------------------------------------

async fn hotreload_phase(
    harness: &Harness,
    args: &FlowBenchArgs,
    db_url: &str,
) -> anyhow::Result<bool> {
    println!(
        "\n## hotreload — {} catalog version flips, new version live < 1s",
        args.hotreload_iters
    );
    let mut w = harness.worker().await?;
    let (fix, _fix_task) = fixture_client(db_url).await?;
    seed_fixture_flows(&fix).await?;

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
        set_active_flow_version(&fix, FLOW_TENANT, FIXTURE_FLOW_ID, target as i32).await?;
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

#[cfg(test)]
mod fixture_tests {
    use super::{FIXTURE_FLOW_ID, flow_json};

    fn assert_no_legacy_entry_fields(definition: &serde_json::Value) {
        assert!(definition.get("trigger").is_none());
        assert!(definition.get("entry").is_none());
    }

    /// The interface map the flowrunner's `run`/`run-s6` fixture path resolves
    /// these graphs against (`s3_fixture_interfaces` in
    /// `components/execution/flowrunner/src/lib.rs`, its own guest-side test
    /// pins the entries). Every fixture node type completes on `main`.
    fn fixture_interfaces() -> wamn_flow::ResolvedInterfaces {
        ["conditional", "http-call", "pg-write", "transform"]
            .into_iter()
            .map(|node_type| (node_type.to_string(), vec!["main".to_string()]))
            .collect()
    }

    fn issue_codes(fixture: &str) -> Vec<&'static str> {
        let flow = wamn_flow::Flow::from_json(fixture).expect("fixture parses");
        wamn_flow::validate(&flow, &fixture_interfaces())
            .into_iter()
            .map(|issue| issue.code)
            .collect()
    }

    #[test]
    fn primary_fixture_parses_with_typed_request_entry() {
        for (version, op) in [(1, "upper"), (2, "reverse")] {
            let fixture = flow_json(version);
            let flow =
                wamn_flow::Flow::from_json(&fixture).expect("primary flowbench fixture parses");
            let definition: serde_json::Value =
                serde_json::from_str(&fixture).expect("primary flowbench fixture is JSON");

            assert_no_legacy_entry_fields(&definition);
            assert_eq!(flow.flow_id, FIXTURE_FLOW_ID);
            assert_eq!(flow.version, version);
            assert_eq!(
                flow.entry_node()
                    .map(|node| (node.id.as_str(), node.node_type.as_str())),
                Some(("in", "request"))
            );
            assert_eq!(
                flow.nodes
                    .iter()
                    .map(|node| node.node_type.as_str())
                    .collect::<Vec<_>>(),
                ["request", "transform", "pg-write", "conditional", "respond"]
            );
            assert_eq!(flow.nodes[1].config["op"], op);
            assert_eq!(flow.nodes[4].config["status"], 200);
            assert_eq!(
                flow.edges
                    .iter()
                    .map(|edge| (edge.from.as_str(), edge.to.as_str()))
                    .collect::<Vec<_>>(),
                [("in", "t"), ("t", "w"), ("w", "c"), ("c", "out")]
            );
        }
    }

    /// All retained fixtures validate against the interface map used by the
    /// guest's direct fixture path.
    #[test]
    fn retained_fixtures_validate_against_the_fixture_interface_map() {
        assert_eq!(issue_codes(&flow_json(1)), Vec::<&str>::new());
        assert_eq!(issue_codes(&flow_json(2)), Vec::<&str>::new());
    }
}
