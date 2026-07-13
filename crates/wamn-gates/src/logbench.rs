//! The `logbench` subcommand: the S5 logging-capture gates
//! (docs/p0-exit-criteria.md S5).
//!
//! S5 asks three things of the `wasi:logging` capture path — a guest logs, the
//! `wamn:logging` plugin enriches (host-trusted tenant/project + guest
//! flow/run/node) and ships OTel log records to an OTel Collector, which
//! forwards them to Loki:
//!
//!   1. **Overhead (<50 µs).** How much does a single `log()` call cost the
//!      guest? Measured in-guest with `std::time::Instant` (the plugin only
//!      enriches + enqueues, so this excludes the OTLP export).
//!   2. **Loss (<0.1% unaccounted at 10k lines/s for 30s).** Emit 300k lines,
//!      then account: `emitted = delivered(Loki) + dropped(plugin) +
//!      unaccounted`, gate `unaccounted/emitted < 0.1%`. Loki's exact count is
//!      the ground truth; the collector's internal metrics cross-check it.
//!   3. **Drops visible + enrichment 100%.** A saturation burst overflows the
//!      plugin's bounded front queue, proving drops are *counted* (a metric),
//!      not silent; and a Loki query proves every delivered record carries
//!      tenant/project/flow/run/node.
//!
//! The plugin owns the OTLP `LoggerProvider`; this harness drives the guest and
//! does the accounting. Loki/collector endpoints come from the environment so
//! the same binary runs locally (docker) and in-cluster (the gate of record).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, InstancePre, Linker, TypedFunc};

use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_logging::{self, WamnLogging, WamnLoggingConfig};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Per-call `log()` cost, guest-observed (<50 µs).
    Overhead,
    /// 10k lines/s × 30s loss accounting (<0.1% unaccounted).
    Throughput,
    /// Saturation burst — drops are counted, not silent.
    Saturation,
    /// 100% of delivered records carry tenant/project/flow/run/node.
    Enrichment,
    /// Every gate in sequence.
    All,
}

#[derive(Debug, Args)]
pub struct LogBenchArgs {
    /// logspewer guest (imports wasi:logging, exports overhead + emit-batch).
    #[arg(long, default_value = "/bench/logspewer.wasm")]
    pub logspewer: PathBuf,

    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Target lines/sec for the loss gate.
    #[arg(long, default_value_t = 10_000)]
    pub rate: u64,
    /// Seconds to sustain `rate` for the loss gate.
    #[arg(long, default_value_t = 30)]
    pub duration_secs: u64,
    /// `log()` calls the guest self-times for the overhead gate.
    #[arg(long, default_value_t = 2_000)]
    pub overhead_iters: u32,
    /// Lines emitted for the (self-contained) enrichment gate.
    #[arg(long, default_value_t = 5_000)]
    pub enrichment_lines: u64,

    /// Saturation: bounded front-queue capacity (small, so the burst overflows).
    #[arg(long, default_value_t = 4_096)]
    pub sat_queue: usize,
    /// Saturation: drain rate (records/s) — below the burst so drops occur.
    #[arg(long, default_value_t = 2_000)]
    pub sat_drain_rate: u64,
    /// Saturation: total lines in the overflow burst.
    #[arg(long, default_value_t = 200_000)]
    pub sat_burst: u64,

    /// Seconds to wait after flushing before querying Loki (collector→Loki
    /// batching + ingestion settle).
    #[arg(long, default_value_t = 12)]
    pub settle_secs: u64,

    /// Loki HTTP base URL (query API).
    #[arg(long, env = "LOKI_URL", default_value = "http://127.0.0.1:3100")]
    pub loki_url: String,
    /// OTel Collector Prometheus metrics URL (cross-check; best-effort).
    #[arg(
        long,
        env = "COLLECTOR_METRICS_URL",
        default_value = "http://127.0.0.1:8888/metrics"
    )]
    pub collector_metrics_url: String,

    /// Host-trusted claim injected for the bench component.
    #[arg(long, default_value = "acme")]
    pub tenant: String,
    #[arg(long, default_value = "receiving")]
    pub project: String,
}

const BENCH_ID: &str = "s5-logbench";
const FLOW: &str = "receipt-flow";
const RUN: &str = "run-0001";
const NODE: &str = "log-node";

// ---------------------------------------------------------------------------
// Harness: compile logspewer once, instantiate against a given plugin
// ---------------------------------------------------------------------------

struct Harness {
    engine: wash_runtime::engine::Engine,
    pre: InstancePre<SharedCtx>,
}

/// One warm logspewer instance with its two typed exports resolved.
struct LogInstance {
    store: Store<SharedCtx>,
    overhead: TypedFunc<(u32,), (Vec<u64>,)>,
    emit_batch: TypedFunc<(u32, u64, String, String, String, String), ()>,
}

impl Harness {
    fn new(engine: wash_runtime::engine::Engine, guest: &[u8]) -> anyhow::Result<Self> {
        let raw = engine.inner();
        let component =
            Component::new(raw, guest).map_err(|e| anyhow::anyhow!("compile logspewer: {e}"))?;
        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wamn_logging::add_to_linker(&mut linker)?;
        let pre = linker.instantiate_pre(&component)?;
        Ok(Self { engine, pre })
    }

    async fn instance(&self, plugin: Arc<WamnLogging>) -> anyhow::Result<LogInstance> {
        let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
        plugins.insert(
            wamn_logging::WAMN_LOGGING_ID,
            plugin as Arc<dyn HostPlugin + Send + Sync>,
        );
        let ctx = Ctx::builder(BENCH_ID.to_string(), BENCH_ID.to_string())
            .with_plugins(plugins)
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = self.pre.instantiate_async(&mut store).await?;
        let overhead = instance.get_typed_func::<(u32,), (Vec<u64>,)>(&mut store, "overhead")?;
        let emit_batch = instance
            .get_typed_func::<(u32, u64, String, String, String, String), ()>(
                &mut store,
                "emit-batch",
            )?;
        Ok(LogInstance {
            store,
            overhead,
            emit_batch,
        })
    }
}

/// Build a plugin with the given config and register the bench claim.
fn make_plugin(args: &LogBenchArgs, cfg: WamnLoggingConfig) -> anyhow::Result<Arc<WamnLogging>> {
    let plugin = Arc::new(WamnLogging::new(cfg)?);
    plugin.set_claim(BENCH_ID, &args.tenant, &args.project);
    Ok(plugin)
}

fn unique_label(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{nanos}")
}

// ---------------------------------------------------------------------------
// percentiles
// ---------------------------------------------------------------------------

fn pct(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx]
}

struct Stats {
    p50: u64,
    p99: u64,
    max: u64,
    mean: u64,
}
fn summarize(mut s: Vec<u64>) -> Stats {
    s.sort_unstable();
    let sum: u128 = s.iter().map(|&x| x as u128).sum();
    let mean = if s.is_empty() {
        0
    } else {
        (sum / s.len() as u128) as u64
    };
    Stats {
        p50: pct(&s, 0.50),
        p99: pct(&s, 0.99),
        max: *s.last().unwrap_or(&0),
        mean,
    }
}

// ---------------------------------------------------------------------------
// entry
// ---------------------------------------------------------------------------

pub async fn run(args: LogBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-host S5 logbench");
    println!(
        "loki = {}, collector metrics = {}, tenant = {}, project = {}",
        args.loki_url, args.collector_metrics_url, args.tenant, args.project
    );
    if !std::env::vars().any(|(k, _)| k.starts_with("OTEL_")) {
        bail!(
            "no OTEL_* env set: logbench needs OTEL_EXPORTER_OTLP_ENDPOINT pointing at the collector (else nothing is exported)"
        );
    }

    let guest = std::fs::read(&args.logspewer)
        .with_context(|| format!("read {}", args.logspewer.display()))?;
    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let harness = Harness::new(engine, &guest)?;

    let run_all = args.mode == Mode::All;
    let mut pass = true;

    if run_all || args.mode == Mode::Overhead {
        pass &= overhead_phase(&harness, &args).await?;
    }
    if run_all || args.mode == Mode::Throughput {
        pass &= throughput_phase(&harness, &args).await?;
    }
    if run_all || args.mode == Mode::Saturation {
        pass &= saturation_phase(&harness, &args).await?;
    }
    if run_all || args.mode == Mode::Enrichment {
        pass &= enrichment_phase(&harness, &args).await?;
    }

    ticker.abort();
    println!("\nlogbench complete — overall PASS: {pass}");
    if !pass {
        bail!("one or more S5 gates failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// overhead gate (<50 µs guest-observed)
// ---------------------------------------------------------------------------

async fn overhead_phase(harness: &Harness, args: &LogBenchArgs) -> anyhow::Result<bool> {
    println!(
        "\n## overhead — {} guest-timed log() calls (non-blocking enrich+enqueue)",
        args.overhead_iters
    );
    // Big queue + unbounded drain: overhead measures the enqueue path with no
    // back-pressure.
    let cfg = WamnLoggingConfig {
        queue_capacity: 1 << 20,
        drain_rate_per_sec: 0,
        ..WamnLoggingConfig::default()
    };
    let plugin = make_plugin(args, cfg)?;
    let mut inst = harness.instance(plugin).await?;

    // Warm the instance + drain path.
    let _ = inst.overhead.call_async(&mut inst.store, (200,)).await?;
    let (samples,) = inst
        .overhead
        .call_async(&mut inst.store, (args.overhead_iters,))
        .await?;

    let s = summarize(samples);
    println!(
        "per-call log(): p50 = {} ns, p99 = {} ns, max = {} ns (mean {} ns)",
        s.p50, s.p99, s.max, s.mean
    );
    let pass = s.p99 < 50_000;
    println!(
        "PASS(log() p99 < 50 µs): {pass} (p99 = {:.2} µs, max = {:.2} µs)",
        s.p99 as f64 / 1000.0,
        s.max as f64 / 1000.0
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// emit driver: pace `total` lines at `rate` lines/s through one instance
// ---------------------------------------------------------------------------

/// Emit `total` lines paced to ~`rate`/s in 10 ms ticks. Returns the number of
/// `log()` calls issued (== accepted + dropped at the plugin).
async fn emit_paced(
    inst: &mut LogInstance,
    rate: u64,
    total: u64,
    run_label: &str,
) -> anyhow::Result<u64> {
    let per_tick = (rate / 100).max(1); // 10 ms ticks
    let mut seq: u64 = 0;
    let mut ticker = tokio::time::interval(Duration::from_millis(10));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while seq < total {
        ticker.tick().await;
        let n = per_tick.min(total - seq) as u32;
        inst.emit_batch
            .call_async(
                &mut inst.store,
                (
                    n,
                    seq,
                    run_label.to_string(),
                    FLOW.to_string(),
                    RUN.to_string(),
                    NODE.to_string(),
                ),
            )
            .await?;
        seq += n as u64;
    }
    Ok(seq)
}

/// Emit `total` lines as fast as the guest can (no pacing) — the saturation
/// burst. One `emit-batch` call so the guest loop outruns the drain.
async fn emit_burst(inst: &mut LogInstance, total: u64, run_label: &str) -> anyhow::Result<u64> {
    // Chunk to keep each host call bounded, but issue back-to-back (no sleeps).
    let mut seq = 0u64;
    while seq < total {
        let n = (total - seq).min(50_000) as u32;
        inst.emit_batch
            .call_async(
                &mut inst.store,
                (
                    n,
                    seq,
                    run_label.to_string(),
                    FLOW.to_string(),
                    RUN.to_string(),
                    NODE.to_string(),
                ),
            )
            .await?;
        seq += n as u64;
    }
    Ok(seq)
}

/// Wait until the plugin's drain has emitted everything it accepted, then flush
/// the OTLP batch processor and let the collector→Loki hop settle.
async fn drain_and_settle(plugin: &WamnLogging, settle_secs: u64) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while plugin.emitted() < plugin.accepted() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    plugin.force_flush()?;
    tokio::time::sleep(Duration::from_secs(settle_secs)).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// throughput gate (<0.1% unaccounted at rate × duration)
// ---------------------------------------------------------------------------

async fn throughput_phase(harness: &Harness, args: &LogBenchArgs) -> anyhow::Result<bool> {
    let total = args.rate * args.duration_secs;
    println!(
        "\n## throughput — {} lines/s for {}s ({} lines); unaccounted-loss gate < 0.1%",
        args.rate, args.duration_secs, total
    );
    let cfg = WamnLoggingConfig {
        queue_capacity: 1 << 17, // absorbs jitter; unbounded drain keeps it near-empty
        drain_rate_per_sec: 0,
        ..WamnLoggingConfig::default()
    };
    let plugin = make_plugin(args, cfg)?;
    let mut inst = harness.instance(plugin.clone()).await?;

    let run_label = unique_label("tp");
    let start = SystemTime::now();
    let emitted = emit_paced(&mut inst, args.rate, total, &run_label).await?;
    drain_and_settle(&plugin, args.settle_secs).await?;
    let window_secs = start.elapsed().map(|d| d.as_secs()).unwrap_or(0) + 5;

    let accepted = plugin.accepted();
    let dropped = plugin.dropped();
    let delivered = loki_count(&args.loki_url, &run_label, window_secs, &[]).await?;
    let sent = collector_sent(&args.collector_metrics_url).await;

    println!(
        "emitted = {emitted}, accepted = {accepted}, dropped = {dropped}, delivered(Loki) = {delivered}"
    );
    match sent {
        // Cumulative across the collector's lifetime (all phases/runs); a
        // liveness/delivery sanity, not a per-run figure. Loki's per-run_label
        // count is the authoritative delivered number.
        Some(s) => println!("cross-check: collector sent_log_records = {s} (cumulative)"),
        None => println!("cross-check: collector metrics unavailable (skipped)"),
    }

    let unaccounted = (emitted as i64) - (dropped as i64) - (delivered as i64);
    let frac = unaccounted.max(0) as f64 / emitted.max(1) as f64;
    println!(
        "unaccounted = emitted - dropped - delivered = {unaccounted} ({:.4}% of emitted)",
        frac * 100.0
    );
    if unaccounted < 0 {
        println!(
            "note: delivered exceeds (emitted - dropped) by {} — collector/Loki retry duplication, not loss",
            -unaccounted
        );
    }
    let pass = frac < 0.001;
    println!("PASS(unaccounted < 0.1%): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// saturation gate (drops counted, not silent)
// ---------------------------------------------------------------------------

async fn saturation_phase(harness: &Harness, args: &LogBenchArgs) -> anyhow::Result<bool> {
    println!(
        "\n## saturation — {} line burst into a {}-slot queue draining at {}/s; drops must be counted",
        args.sat_burst, args.sat_queue, args.sat_drain_rate
    );
    let cfg = WamnLoggingConfig {
        queue_capacity: args.sat_queue,
        drain_rate_per_sec: args.sat_drain_rate,
        ..WamnLoggingConfig::default()
    };
    let plugin = make_plugin(args, cfg)?;
    let mut inst = harness.instance(plugin.clone()).await?;

    let run_label = unique_label("sat");
    let start = SystemTime::now();
    let emitted = emit_burst(&mut inst, args.sat_burst, &run_label).await?;
    // The slow drain needs time to export what it accepted.
    drain_and_settle(&plugin, args.settle_secs).await?;
    let window_secs = start.elapsed().map(|d| d.as_secs()).unwrap_or(0) + 5;

    let accepted = plugin.accepted();
    let dropped = plugin.dropped();
    let delivered = loki_count(&args.loki_url, &run_label, window_secs, &[]).await?;

    println!(
        "emitted = {emitted}, accepted = {accepted}, dropped = {dropped}, delivered(Loki) = {delivered}"
    );
    let unaccounted = (emitted as i64) - (dropped as i64) - (delivered as i64);
    let frac = unaccounted.max(0) as f64 / emitted.max(1) as f64;
    println!(
        "unaccounted = {unaccounted} ({:.4}% of emitted); drop counter = {dropped} (surfaced as metric wamn.logging.dropped)",
        frac * 100.0
    );
    let drops_visible = dropped > 0;
    let accounted = frac < 0.001;
    println!("PASS(drops counted > 0): {drops_visible}");
    println!("PASS(unaccounted < 0.1%): {accounted}");
    if !drops_visible {
        println!(
            "note: no drops occurred — raise --sat-burst / lower --sat-queue or --sat-drain-rate so the queue overflows"
        );
    }
    Ok(drops_visible && accounted)
}

// ---------------------------------------------------------------------------
// enrichment gate (100% of delivered records carry all five fields)
// ---------------------------------------------------------------------------

async fn enrichment_phase(harness: &Harness, args: &LogBenchArgs) -> anyhow::Result<bool> {
    println!(
        "\n## enrichment — {} lines; every delivered record must carry tenant/project/flow/run/node",
        args.enrichment_lines
    );
    let plugin = make_plugin(args, WamnLoggingConfig::default())?;
    let mut inst = harness.instance(plugin.clone()).await?;

    let run_label = unique_label("enr");
    let start = SystemTime::now();
    let emitted = emit_paced(
        &mut inst,
        args.rate.min(5_000),
        args.enrichment_lines,
        &run_label,
    )
    .await?;
    drain_and_settle(&plugin, args.settle_secs).await?;
    let window_secs = start.elapsed().map(|d| d.as_secs()).unwrap_or(0) + 5;

    let total = loki_count(&args.loki_url, &run_label, window_secs, &[]).await?;
    // Same query, but require all five enrichment fields to be present+non-empty.
    let enriched_filter = ["tenant", "project", "flow", "run", "node"];
    let enriched = loki_count(&args.loki_url, &run_label, window_secs, &enriched_filter).await?;

    println!("emitted = {emitted}, delivered(Loki) = {total}, fully-enriched = {enriched}");
    let pass = total > 0 && enriched == total;
    println!("PASS(100% enrichment): {pass} ({enriched}/{total} records carry all five fields)");
    if total == 0 {
        println!("note: 0 delivered — check the collector→Loki pipeline / LogQL selector");
    }
    Ok(pass)
}

// ---------------------------------------------------------------------------
// Loki query (exact count) + collector metrics cross-check
// ---------------------------------------------------------------------------

/// `sum(count_over_time({service_name="wamn-host"} | run_label="RL" [<window>s]))`
/// with an optional set of `| key!=""` structured-metadata filters (enrichment
/// check). Returns the delivered record count.
async fn loki_count(
    base: &str,
    run_label: &str,
    window_secs: u64,
    require_nonempty: &[&str],
) -> anyhow::Result<u64> {
    let mut selector = format!("{{service_name=\"wamn-host\"}} | run_label=\"{run_label}\"");
    for k in require_nonempty {
        selector.push_str(&format!(" | {k}!=\"\""));
    }
    let logql = format!("sum(count_over_time({selector} [{window_secs}s]))");
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = format!(
        "/loki/api/v1/query?query={}&time={}",
        percent_encode(&logql),
        now_ns
    );
    let body = http_get(base, &path)
        .await
        .with_context(|| format!("Loki query {logql}"))?;
    parse_loki_vector(&body)
}

/// Extract the scalar out of a Loki `resultType:"vector"` instant-query body.
fn parse_loki_vector(body: &str) -> anyhow::Result<u64> {
    let v: serde_json::Value =
        serde_json::from_str(body).with_context(|| format!("Loki JSON: {body:.200}"))?;
    let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
    if status != "success" {
        bail!("Loki query status = {status:?}: {body:.300}");
    }
    let result = v
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(|r| r.as_array());
    let Some(result) = result else {
        return Ok(0);
    };
    // vector: [ { "metric": {...}, "value": [ <ts>, "<count>" ] }, ... ]
    let mut total = 0u64;
    for series in result {
        if let Some(val) = series
            .get("value")
            .and_then(|x| x.as_array())
            .and_then(|a| a.get(1))
            .and_then(|s| s.as_str())
            .and_then(|s| s.parse::<f64>().ok())
        {
            total += val as u64;
        }
    }
    Ok(total)
}

/// Best-effort: sum `otelcol_exporter_sent_log_records` across the collector's
/// Prometheus metrics. Returns None if the endpoint is unreachable.
async fn collector_sent(metrics_url: &str) -> Option<u64> {
    let (base, path) = split_url(metrics_url).ok()?;
    let body = http_get(&base, &path).await.ok()?;
    let mut sum = 0u64;
    let mut saw = false;
    for line in body.lines() {
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with("otelcol_exporter_sent_log_records")
            && let Some(val) = line.rsplit(' ').next().and_then(|s| s.parse::<f64>().ok())
        {
            sum += val as u64;
            saw = true;
        }
    }
    saw.then_some(sum)
}

// ---------------------------------------------------------------------------
// minimal HTTP/1.1 GET (Loki + collector are plain http; no client dep)
// ---------------------------------------------------------------------------

/// Split `http://host:port/path?query` into (`http://host:port`, `/path?query`).
fn split_url(url: &str) -> anyhow::Result<(String, String)> {
    let rest = url
        .strip_prefix("http://")
        .context("only http:// URLs supported")?;
    match rest.find('/') {
        Some(i) => Ok((format!("http://{}", &rest[..i]), rest[i..].to_string())),
        None => Ok((url.to_string(), "/".to_string())),
    }
}

async fn http_get(base: &str, path: &str) -> anyhow::Result<String> {
    let host_port = base.strip_prefix("http://").unwrap_or(base);
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(80)),
        None => (host_port.to_string(), 80),
    };
    let mut stream = TcpStream::connect((host.as_str(), port))
        .await
        .with_context(|| format!("connect {host}:{port}"))?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw);
    // Status line sanity.
    let status_ok = text.starts_with("HTTP/1.1 2") || text.starts_with("HTTP/1.0 2");
    if !status_ok {
        let status = text.lines().next().unwrap_or("<none>");
        bail!("GET {path} -> {status}");
    }
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .map(|(h, b)| (h.to_string(), b.to_string()))
        .unwrap_or_default();
    // Go's net/http (Loki, the collector) may reply chunked; de-frame if so.
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        Ok(dechunk(&body))
    } else {
        Ok(body)
    }
}

/// Decode an HTTP/1.1 chunked body: `<hex-size>\r\n<data>\r\n` … `0\r\n\r\n`.
fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        if after.len() < size {
            out.push_str(after);
            break;
        }
        out.push_str(&after[..size]);
        // Skip the chunk data + trailing CRLF.
        rest = after[size..].strip_prefix("\r\n").unwrap_or(&after[size..]);
    }
    out
}

/// Percent-encode a LogQL query for a URL query-string value.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
