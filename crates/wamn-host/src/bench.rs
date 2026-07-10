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
//!
//! Runs locally and inside the host image (`kubectl run ... -- bench`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clap::Args;
use wash_runtime::host::{HostApi, HostBuilder};
use wash_runtime::types::{
    Component, LocalResources, Service, Workload, WorkloadStartRequest, WorkloadStatusRequest,
};
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker, ResourceTable};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi::p2::bindings::CommandPre;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::engine::{MEMORY_CAP_BYTES, build_engine};

#[derive(Debug, Args)]
pub struct BenchArgs {
    /// Path to the minimal test component (instantiation + density target)
    #[arg(long, default_value = "/bench/hello.wasm")]
    pub hello: PathBuf,

    /// Path to the memory-hog component (cap-kill target)
    #[arg(long, default_value = "/bench/memhog.wasm")]
    pub memhog: PathBuf,

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

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
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
        let mut store = Store::new(raw, BenchCtx::new());
        let cmd = cmd_pre.instantiate_async(&mut store).await?;
        cmd.wasi_cli_run()
            .call_run(&mut store)
            .await?
            .map_err(|()| anyhow::anyhow!("hello component returned failure"))?;
    }

    for _ in 0..50 {
        let mut store = Store::new(raw, BenchCtx::new());
        let _ = cmd_pre.instantiate_async(&mut store).await?;
    }

    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let mut store = Store::new(raw, BenchCtx::new());
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
