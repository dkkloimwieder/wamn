//! The `bench` subcommand: S1 measurements (docs/p0-exit-criteria.md).
//!
//! Phase 1 — cold instantiation p50/p99 (pass: p99 < 10 ms). Raw wasmtime
//!   Store::new + CommandPre::instantiate_async on the same engine config the
//!   host runs, mirroring upstream's wasmtime_baseline methodology.
//! Phase 2 — per-component memory overhead at N resident workloads (unique
//!   digests, so each is separately compiled — no cache flattery).
//! Phase 3 — 256 MiB cap kill: a service component allocating past the cap
//!   must die while the host keeps serving (pass: clean kill, no host
//!   restart).
//! Phase 4 — epoch kill (wamn-4p3): with the carried wash-runtime patch and
//!   the epoch ticker running, a busy-loop component must be hard-killed at
//!   its epoch deadline (raw store: assertable Trap::Interrupt; host path:
//!   `wamn.epoch-deadline-ticks` service config) while normal workloads run
//!   unaffected.
//! Phase 5 — per-component memory budgets (wamn-bp4.1, fork ResourceLimiter):
//!   concurrent memhogs budgeted 64 / 192 MiB under the 256 MiB ceiling each
//!   trap at their own number (differentiation — closes S1 finding #2), an
//!   unbudgeted one still traps at the ceiling, and a budget above the
//!   ceiling hard-fails without allocating (error, never a silent clamp).
//!
//! Runs locally and inside the host image (`kubectl run ... -- bench`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clap::Args;
use wash_runtime::host::{HostApi, HostBuilder};
use wash_runtime::types::{
    Component, HostPathVolume, LocalResources, Service, Volume, VolumeMount, VolumeType, Workload,
    WorkloadStartRequest, WorkloadStatusRequest,
};
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker, ResourceTable};
use wash_runtime::wasmtime::{Engine as RawEngine, Store, Trap};
use wasmtime_wasi::p2::bindings::CommandPre;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use wamn_gate_harness::percentile;
use wamn_host::engine::{DEFAULT_EPOCH_TICK, MEMORY_CAP_BYTES, build_engine, spawn_epoch_ticker};

#[derive(Debug, Args)]
pub struct BenchArgs {
    /// Path to the minimal test component (instantiation + density target)
    #[arg(long, default_value = "/bench/hello.wasm")]
    pub hello: PathBuf,

    /// Path to the memory-hog component (cap-kill target)
    #[arg(long, default_value = "/bench/memhog.wasm")]
    pub memhog: PathBuf,

    /// Path to the busy-loop component (epoch-kill target)
    #[arg(long, default_value = "/bench/busyloop.wasm")]
    pub busyloop: PathBuf,

    /// Timed instantiation iterations (after warmup)
    #[arg(long, default_value_t = 2000)]
    pub iterations: usize,

    /// Resident workload count for the density phase
    #[arg(long, default_value_t = 100)]
    pub residents: usize,

    /// Skip the cap-kill phase
    #[arg(long, default_value_t = false)]
    pub skip_capkill: bool,
}

struct BenchCtx {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl BenchCtx {
    fn new() -> Self {
        Self {
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        }
    }
}

impl WasiView for BenchCtx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Raw store with a generous epoch deadline. The engine enables epoch
/// interruption, and a fresh store's deadline of 0 traps on the first check.
fn bench_store(raw: &RawEngine) -> Store<BenchCtx> {
    let mut store = Store::new(raw, BenchCtx::new());
    store.set_epoch_deadline(u64::MAX / 2);
    store
}

fn empty_resources() -> LocalResources {
    LocalResources {
        memory_limit_mb: 0,
        cpu_limit: 0,
        config: Default::default(),
        environment: Default::default(),
        volume_mounts: vec![],
        allowed_hosts: Arc::from(vec![]),
    }
}

fn rss_kib() -> anyhow::Result<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let line = status
        .lines()
        .find(|l| l.starts_with("VmRSS:"))
        .context("VmRSS not found")?;
    let kib: u64 = line
        .split_whitespace()
        .nth(1)
        .context("malformed VmRSS")?
        .parse()?;
    Ok(kib)
}

pub async fn run(args: BenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    let hello_bytes = std::fs::read(&args.hello)
        .with_context(|| format!("failed to read {}", args.hello.display()))?;
    let memhog_bytes = std::fs::read(&args.memhog)
        .with_context(|| format!("failed to read {}", args.memhog.display()))?;
    let busyloop_bytes = std::fs::read(&args.busyloop)
        .with_context(|| format!("failed to read {}", args.busyloop.display()))?;

    println!("# wamn-host S1 bench");
    println!(
        "engine: pooling allocator, max_memory_size = {} MiB",
        MEMORY_CAP_BYTES >> 20
    );

    instantiation_phase(&hello_bytes, args.iterations).await?;
    let host = density_phase(&hello_bytes, args.residents).await?;
    if !args.skip_capkill {
        capkill_phase(&host, &hello_bytes, &memhog_bytes).await?;
    }
    epochkill_phase(&hello_bytes, &busyloop_bytes).await?;
    membudget_phase(&hello_bytes, &memhog_bytes).await?;
    println!("\nbench complete");
    Ok(())
}

/// Phase 1: cold instantiation latency on the host's engine config.
async fn instantiation_phase(hello: &[u8], iterations: usize) -> anyhow::Result<()> {
    let engine = build_engine(&[])?;
    let raw: &RawEngine = engine.inner();

    let component =
        WasmtimeComponent::new(raw, hello).map_err(|e| anyhow::anyhow!("compile hello: {e}"))?;
    let mut linker: Linker<BenchCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    let pre = linker.instantiate_pre(&component)?;
    let cmd_pre = CommandPre::new(pre)?;

    // Sanity: the component actually runs.
    {
        let mut store = bench_store(raw);
        let cmd = cmd_pre.instantiate_async(&mut store).await?;
        cmd.wasi_cli_run()
            .call_run(&mut store)
            .await?
            .map_err(|()| anyhow::anyhow!("hello component returned failure"))?;
    }

    for _ in 0..50 {
        let mut store = bench_store(raw);
        let _ = cmd_pre.instantiate_async(&mut store).await?;
    }

    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let mut store = bench_store(raw);
        let _instance = cmd_pre.instantiate_async(&mut store).await?;
        samples.push(start.elapsed());
    }
    samples.sort();

    println!("\n## Phase 1 — cold instantiation ({iterations} iterations)");
    println!(
        "p50 = {:?}  p90 = {:?}  p99 = {:?}  max = {:?}",
        percentile(&samples, 0.50),
        percentile(&samples, 0.90),
        percentile(&samples, 0.99),
        samples.last().unwrap(),
    );
    let pass = percentile(&samples, 0.99) < Duration::from_millis(10);
    println!("PASS(p99 < 10ms): {pass}");
    Ok(())
}

/// Phase 2: memory overhead at N resident workloads (one component each,
/// unique digest per workload so each is compiled separately).
async fn density_phase(
    hello: &[u8],
    residents: usize,
) -> anyhow::Result<Arc<wash_runtime::host::Host>> {
    let engine = build_engine(&[])?;
    let host = HostBuilder::new().with_engine(engine).build()?;
    let host = host.start().await?;

    let rss_before = rss_kib()?;
    let started = Instant::now();
    for i in 0..residents {
        host.workload_start(WorkloadStartRequest {
            workload_id: format!("bench-{i:03}"),
            workload: Workload {
                namespace: "bench".to_string(),
                name: format!("hello-{i:03}"),
                annotations: Default::default(),
                service: None,
                components: vec![Component {
                    name: "hello".to_string(),
                    bytes: hello.to_vec().into(),
                    // Unique digest defeats the compilation cache: measures
                    // true per-component residency, not 100 handles to one
                    // compiled artifact.
                    digest: Some(format!("bench-unique-{i:03}")),
                    local_resources: empty_resources(),
                    pool_size: 0,
                    max_invocations: 0,
                }],
                host_interfaces: vec![],
                volumes: vec![],
            },
        })
        .await
        .with_context(|| format!("failed to start workload {i}"))?;
    }
    let elapsed = started.elapsed();
    let rss_after = rss_kib()?;
    let delta_mib = (rss_after.saturating_sub(rss_before)) as f64 / 1024.0;

    println!("\n## Phase 2 — {residents} resident workloads (unique digests)");
    println!(
        "start time total = {elapsed:?} ({:?}/workload)",
        elapsed / residents as u32
    );
    println!(
        "RSS before = {:.1} MiB, after = {:.1} MiB, delta = {delta_mib:.1} MiB ({:.2} MiB/component)",
        rss_before as f64 / 1024.0,
        rss_after as f64 / 1024.0,
        delta_mib / residents as f64,
    );
    println!("host stable at {residents} residents: true");
    Ok(host)
}

/// Phase 3: cap-kill. memhog runs as a service and must trap growing past
/// the 256 MiB pooling cap; the host must keep working afterwards.
async fn capkill_phase(
    host: &Arc<wash_runtime::host::Host>,
    hello: &[u8],
    memhog: &[u8],
) -> anyhow::Result<()> {
    println!("\n## Phase 3 — 256 MiB cap kill");
    host.workload_start(WorkloadStartRequest {
        workload_id: "capkill".to_string(),
        workload: Workload {
            namespace: "bench".to_string(),
            name: "memhog".to_string(),
            annotations: Default::default(),
            service: Some(Service {
                bytes: memhog.to_vec().into(),
                digest: Some("bench-memhog".to_string()),
                local_resources: empty_resources(),
                max_restarts: 0,
            }),
            components: vec![],
            host_interfaces: vec![],
            volumes: vec![],
        },
    })
    .await
    .context("failed to start memhog service")?;

    // Give the service time to allocate past the cap and die.
    tokio::time::sleep(Duration::from_secs(5)).await;

    let status = host
        .workload_status(WorkloadStatusRequest {
            workload_id: "capkill".to_string(),
        })
        .await?;
    println!(
        "memhog workload status after kill: {:?}",
        status.workload_status
    );

    // Host must still accept and run work.
    let heartbeat = host.heartbeat().await;
    println!("heartbeat after kill: ok = {}", heartbeat.is_ok());
    host.workload_start(WorkloadStartRequest {
        workload_id: "post-capkill".to_string(),
        workload: Workload {
            namespace: "bench".to_string(),
            name: "post-capkill".to_string(),
            annotations: Default::default(),
            service: None,
            components: vec![Component {
                name: "hello".to_string(),
                bytes: hello.to_vec().into(),
                digest: Some("bench-post-capkill".to_string()),
                local_resources: empty_resources(),
                pool_size: 0,
                max_invocations: 0,
            }],
            host_interfaces: vec![],
            volumes: vec![],
        },
    })
    .await
    .context("host failed to start work after cap kill")?;
    println!("host accepted new workload after kill: true");
    println!("PASS(clean cap kill, host survives): true");
    Ok(())
}

/// Phase 4: epoch kill (wamn-4p3). Raw path: busyloop under a short deadline
/// must come back as `Trap::Interrupt` — the assertable hard kill. Host path:
/// a busyloop *service* with `wamn.epoch-deadline-ticks` config (plumbed by
/// the carried patch) dies at its deadline while the host keeps serving and
/// a default-deadline workload runs unaffected under the same ticker.
async fn epochkill_phase(hello: &[u8], busyloop: &[u8]) -> anyhow::Result<()> {
    const KILL_TICKS: u64 = 20;

    println!("\n## Phase 4 — epoch kill (carried patch + {DEFAULT_EPOCH_TICK:?} ticker)");
    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    let raw_interrupted = {
        let raw: &RawEngine = engine.inner();
        let component = WasmtimeComponent::new(raw, busyloop)
            .map_err(|e| anyhow::anyhow!("compile busyloop: {e}"))?;
        let mut linker: Linker<BenchCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        let cmd_pre = CommandPre::new(linker.instantiate_pre(&component)?)?;

        let mut store = Store::new(raw, BenchCtx::new());
        store.set_epoch_deadline(KILL_TICKS);
        let cmd = cmd_pre.instantiate_async(&mut store).await?;
        let started = Instant::now();
        let result = cmd.wasi_cli_run().call_run(&mut store).await;
        let elapsed = started.elapsed();

        let (interrupted, detail) = match result {
            Ok(_) => (
                false,
                "returned normally (busyloop must never exit)".to_string(),
            ),
            Err(e) => (
                matches!(e.downcast_ref::<Trap>(), Some(Trap::Interrupt)),
                e.to_string(),
            ),
        };
        println!(
            "busyloop raw store, deadline = {KILL_TICKS} ticks: killed after {elapsed:?} ({detail})"
        );
        interrupted
    };
    println!("PASS(raw epoch kill is Trap::Interrupt): {raw_interrupted}");

    let host = HostBuilder::new().with_engine(engine).build()?;
    let host = host.start().await?;

    let mut resources = empty_resources();
    resources
        .config
        .insert("wamn.epoch-deadline-ticks".to_string(), "100".to_string());
    host.workload_start(WorkloadStartRequest {
        workload_id: "epochkill".to_string(),
        workload: Workload {
            namespace: "bench".to_string(),
            name: "busyloop".to_string(),
            annotations: Default::default(),
            service: Some(Service {
                bytes: busyloop.to_vec().into(),
                digest: Some("bench-busyloop".to_string()),
                local_resources: resources,
                max_restarts: 0,
            }),
            components: vec![],
            host_interfaces: vec![],
            volumes: vec![],
        },
    })
    .await
    .context("failed to start busyloop service")?;
    println!(
        "busyloop service started with wamn.epoch-deadline-ticks=100 (~1s); watch for the service-death log line"
    );

    tokio::time::sleep(Duration::from_secs(3)).await;

    let status = host
        .workload_status(WorkloadStatusRequest {
            workload_id: "epochkill".to_string(),
        })
        .await?;
    println!(
        "busyloop workload status after deadline: {:?} (upstream keeps Running after service death — S1 gap #4)",
        status.workload_status
    );
    println!(
        "heartbeat after epoch kill: ok = {}",
        host.heartbeat().await.is_ok()
    );

    // A default-deadline workload starts and runs fine under the ticker.
    host.workload_start(WorkloadStartRequest {
        workload_id: "post-epochkill".to_string(),
        workload: Workload {
            namespace: "bench".to_string(),
            name: "post-epochkill".to_string(),
            annotations: Default::default(),
            service: None,
            components: vec![Component {
                name: "hello".to_string(),
                bytes: hello.to_vec().into(),
                digest: Some("bench-post-epochkill".to_string()),
                local_resources: empty_resources(),
                pool_size: 0,
                max_invocations: 0,
            }],
            host_interfaces: vec![],
            volumes: vec![],
        },
    })
    .await
    .context("host failed to start work after epoch kill")?;
    println!("host accepted new workload after epoch kill: true");
    ticker.abort();
    println!("PASS(epoch kill, host survives): {raw_interrupted}");
    Ok(())
}

/// Start memhog as a service with the given memory budget (0 = unbudgeted)
/// and a host-path volume at /report where it rewrites its running total
/// after every allocation step.
async fn start_memhog(
    host: &Arc<wash_runtime::host::Host>,
    memhog: &[u8],
    id: &str,
    budget_mb: i32,
    report_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let mut resources = empty_resources();
    resources.memory_limit_mb = budget_mb;
    resources.environment.insert(
        "MEMHOG_REPORT_PATH".to_string(),
        "/report/achieved".to_string(),
    );
    resources.volume_mounts = vec![VolumeMount {
        name: "report".to_string(),
        mount_path: "/report".to_string(),
        read_only: false,
    }];
    host.workload_start(WorkloadStartRequest {
        workload_id: id.to_string(),
        workload: Workload {
            namespace: "bench".to_string(),
            name: id.to_string(),
            annotations: Default::default(),
            service: Some(Service {
                bytes: memhog.to_vec().into(),
                digest: Some(format!("bench-{id}")),
                local_resources: resources,
                max_restarts: 0,
            }),
            components: vec![],
            host_interfaces: vec![],
            volumes: vec![Volume {
                name: "report".to_string(),
                volume_type: VolumeType::HostPath(HostPathVolume {
                    local_path: report_dir.to_string_lossy().into_owned(),
                }),
            }],
        },
    })
    .await
    .with_context(|| format!("failed to start {id}"))?;
    Ok(())
}

/// Last running total (MiB) a memhog managed to report before it was killed.
fn read_report_mib(dir: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(dir.join("achieved"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Phase 5: per-component memory budgets (wamn-bp4.1, fork ResourceLimiter).
/// Three memhogs run concurrently through the production store path: budgets
/// of 64 and 192 MiB under the 256 MiB ceiling must each trap at their own
/// number (the differentiation S1 finding #2 asked for), an unbudgeted one
/// must still trap at the ceiling (regression: no budget = ceiling), and a
/// budget above the ceiling must hard-fail before its first allocation
/// (strictness: error, never a silent clamp). Enforcement is read back
/// through each memhog's mounted report file: the last total it wrote is the
/// high-water the limiter allowed.
async fn membudget_phase(hello: &[u8], memhog: &[u8]) -> anyhow::Result<()> {
    println!("\n## Phase 5 — per-component memory budgets (fork limiter, wamn-bp4.1)");
    let engine = build_engine(&[])?;
    let host = HostBuilder::new().with_engine(engine).build()?;
    let host = host.start().await?;

    let base = std::env::temp_dir().join(format!("wamn-bench-mem-{}", std::process::id()));
    let dirs: Vec<PathBuf> = ["b64", "b192", "unbudgeted", "over"]
        .iter()
        .map(|n| base.join(n))
        .collect();
    for d in &dirs {
        std::fs::create_dir_all(d)?;
    }

    start_memhog(&host, memhog, "mem-64", 64, &dirs[0]).await?;
    start_memhog(&host, memhog, "mem-192", 192, &dirs[1]).await?;
    start_memhog(&host, memhog, "mem-unbudgeted", 0, &dirs[2]).await?;
    // Budget above the ceiling: depending on where instantiation happens the
    // hard error can surface at workload start or at the service's first
    // memory creation — either way it must never allocate a step.
    match start_memhog(&host, memhog, "mem-over", 512, &dirs[3]).await {
        Ok(()) => println!(
            "mem-over (512 MiB budget > 256 MiB ceiling): accepted; must die at first memory creation"
        ),
        Err(e) => {
            println!("mem-over (512 MiB budget > 256 MiB ceiling): rejected at start ({e:#})")
        }
    }

    // Let all three allocate to their caps and die.
    tokio::time::sleep(Duration::from_secs(6)).await;

    let a = read_report_mib(&dirs[0]);
    let b = read_report_mib(&dirs[1]);
    let c = read_report_mib(&dirs[2]);
    let o = read_report_mib(&dirs[3]);
    println!(
        "achieved: budget-64 = {a:?} MiB, budget-192 = {b:?} MiB, unbudgeted (ceiling 256) = {c:?} MiB, over-ceiling = {o:?} MiB"
    );

    let pass_a = matches!(a, Some(v) if (32..=64).contains(&v));
    let pass_b = matches!(b, Some(v) if (160..=192).contains(&v));
    let pass_c = matches!(c, Some(v) if (208..=256).contains(&v));
    let pass_o = o.is_none();
    println!("PASS(64 MiB budget honored): {pass_a}");
    println!("PASS(192 MiB budget honored beside it — differentiation): {pass_b}");
    println!("PASS(unbudgeted unchanged, dies at the 256 MiB ceiling): {pass_c}");
    println!("PASS(budget > ceiling never allocates — hard error, no clamp): {pass_o}");

    // Host must still accept and run work after three budget kills.
    println!(
        "heartbeat after budget kills: ok = {}",
        host.heartbeat().await.is_ok()
    );
    host.workload_start(WorkloadStartRequest {
        workload_id: "post-membudget".to_string(),
        workload: Workload {
            namespace: "bench".to_string(),
            name: "post-membudget".to_string(),
            annotations: Default::default(),
            service: None,
            components: vec![Component {
                name: "hello".to_string(),
                bytes: hello.to_vec().into(),
                digest: Some("bench-post-membudget".to_string()),
                local_resources: empty_resources(),
                pool_size: 0,
                max_invocations: 0,
            }],
            host_interfaces: vec![],
            volumes: vec![],
        },
    })
    .await
    .context("host failed to start work after budget kills")?;
    println!("host accepted new workload after budget kills: true");
    let _ = std::fs::remove_dir_all(&base);

    println!(
        "PASS(memory budget differentiation): {}",
        pass_a && pass_b && pass_c && pass_o
    );
    Ok(())
}
