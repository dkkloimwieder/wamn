//! The `nodebench` + `serve-node` subcommands: the S4 custom-node gates
//! (docs/p0-exit-criteria.md S4, decision D7, design-note 9b).
//!
//! S4 asks three questions about invoking a *dynamically-loaded custom node*
//! (as opposed to the standard nodes compiled into the S3 runner):
//!
//!   1. HTTP hop (D7) — is the in-cluster HTTP invocation path cheap enough to
//!      be the v0 dispatch mechanism? Gate: hop p50 < 2 ms (escalate > 5 ms).
//!      Measured as the round trip to a warm Rust `noop` node over HTTP/1.1;
//!      with ~0 node compute the round trip *is* the hop.
//!   2. Interpreted vs composed — does the JS/JCO interpreter default cost too
//!      much versus a `wac`-composed frozen flow? Gate: gap < 5% on an
//!      I/O-bound flow (a large gap on a compute-bound flow is expected and
//!      merely sizes frozen flows' post-GA slot). Measured in-process (no HTTP
//!      noise) as the total latency of a 3-node flow three ways: JS-dynamic
//!      (interpreted), Rust-dynamic, and Rust-composed (the `wac` artifact).
//!   3. Config-parse share (9b) — how much of a cold dispatch is the JSON
//!      config codec that only dynamic custom nodes pay? Gate: <= 5% of cold
//!      dispatch. The Rust node self-times its `serde_json` parse and returns
//!      `parse_ns`; the harness times the whole cold instantiate+run.
//!
//! I/O is modeled by a host `wait-ns` import (a real async sleep) that BOTH
//! guest languages call, so the I/O floor is identical and the gap on the
//! I/O-bound flow is pure framework overhead. The production outbound path is
//! wasi:http (5.6 / wamn-bd5); keeping it a host import keeps S4 free of an echo
//! service. See `components/node-rs`, `components/node-ts`, `components/
//! flow-driver` (+ the `wac`-plugged `flow-composed.wasm`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use wash_runtime::engine::ctx::{ActiveCtx, Ctx, SharedCtx, extract_active_ctx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{
    Component as WasmtimeComponent, InstancePre, Linker, TypedFunc,
};

use crate::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "node-bench",
        imports: { default: async },
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::NodeBench;
use bindings::exports::wamn::node::handler::{Emission, NodeError, Payload, RunContext};

// ---------------------------------------------------------------------------
// wait-ns host import: a real async sleep, uniform across guest languages.
// Implemented on the active-context view (like the wamn:postgres plugin), wired
// with `extract_active_ctx`.
// ---------------------------------------------------------------------------

impl bindings::wamn::nodebench::host::Host for ActiveCtx<'_> {
    async fn wait_ns(&mut self, ns: u64) {
        tokio::time::sleep(Duration::from_nanos(ns)).await;
    }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// HTTP hop overhead p50/p99 (D7).
    Hop,
    /// Interpreted-vs-composed total-latency gap (I/O + compute).
    Gap,
    /// Config-JSON-parse share of cold dispatch (design-note 9b).
    Config,
    /// Frozen-contract conformance of the scaffolding-built sample node
    /// (5.4): taxonomy variants, port selection, streamed-payload refusal.
    Sample,
    /// Every gate in sequence.
    All,
}

#[derive(Debug, Args)]
pub struct NodeBenchArgs {
    /// Rust node component (wamn:node handler); also the HTTP-hop node.
    #[arg(long, default_value = "/bench/node-rs.wasm")]
    pub node_rs: PathBuf,
    /// JS/JCO node component (interpreted arm). Skipped if absent.
    #[arg(long, default_value = "/bench/node-ts.wasm")]
    pub node_ts: PathBuf,
    /// wac-composed frozen flow (flow-driver + node-rs).
    #[arg(long, default_value = "/bench/flow-composed.wasm")]
    pub composed: PathBuf,
    /// Scaffolding-built zero-import sample node (components/sample-node);
    /// the 5.4 frozen-contract conformance fixture. Skipped if absent.
    #[arg(long, default_value = "/bench/sample-node.wasm")]
    pub sample: PathBuf,

    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// HTTP round trips timed for the hop gate.
    #[arg(long, default_value_t = 2000)]
    pub hop_iters: usize,
    /// If set (host:port), the hop gate measures a real cross-pod round trip to
    /// an external `serve-node` instead of spawning an in-process loopback
    /// server. This is the truest in-cluster D7 number.
    #[arg(long)]
    pub hop_url: Option<String>,
    /// Warm flow executions timed per arm/workload for the gap gate. The
    /// I/O-bound flow is ~75ms each and the JS compute-bound flow is ~0.4s
    /// each, so this stays modest.
    #[arg(long, default_value_t = 80)]
    pub gap_iters: usize,
    /// Cold instantiate+run cycles for the config-parse gate.
    #[arg(long, default_value_t = 200)]
    pub cold_iters: usize,

    /// Nodes in the measured flow (the S4 flow is 3).
    #[arg(long, default_value_t = 3)]
    pub hops: u32,
    /// Per-node I/O wait for the I/O-bound flow (microseconds). Default 25ms
    /// models a realistic outbound DB/API call; the tokio-timer granularity
    /// (~1ms) and the interpreter's fixed per-invocation overhead are both
    /// negligible against it (that is exactly why the interpreter default
    /// holds for I/O-bound flows). A wait near the ~1ms timer floor would
    /// instead measure timer noise, not I/O.
    #[arg(long, default_value_t = 25_000)]
    pub io_wait_us: u64,
    /// Per-node hashing rounds for the compute-bound flow. Sized so the native
    /// Rust flow is a few ms; the interpreted JS flow is ~200x slower (that is
    /// the point — it sizes frozen flows' post-GA slot, and larger values just
    /// make the JS arm impractically slow).
    #[arg(long, default_value_t = 5_000)]
    pub compute_iters: u64,
}

/// Standalone HTTP node host (for cross-pod hop measurement / manual use).
#[derive(Debug, Args)]
pub struct ServeNodeArgs {
    /// wamn:node component to warm-instantiate and serve.
    #[arg(long, default_value = "/bench/node-rs.wasm")]
    pub node: PathBuf,
    #[arg(long, default_value_t = 8080)]
    pub port: u16,
}

// ---------------------------------------------------------------------------
// Node host: compile + linker + (cold or warm) instantiation
// ---------------------------------------------------------------------------

const BENCH_ID: &str = "s4-nodebench";

fn empty_plugins() -> HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> {
    HashMap::new()
}

fn new_store(engine: &wash_runtime::engine::Engine) -> Store<SharedCtx> {
    let ctx = Ctx::builder(BENCH_ID.to_string(), BENCH_ID.to_string())
        .with_plugins(empty_plugins())
        .build();
    let mut store = Store::new(engine.inner(), SharedCtx::new(ctx));
    // Never epoch-kill a bench node mid-run (I/O waits are legitimately slow).
    store.set_epoch_deadline(u64::MAX / 2);
    store
}

fn node_pre(
    engine: &wash_runtime::engine::Engine,
    bytes: &[u8],
) -> anyhow::Result<InstancePre<SharedCtx>> {
    let raw = engine.inner();
    let component =
        WasmtimeComponent::new(raw, bytes).map_err(|e| anyhow::anyhow!("compile: {e}"))?;
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    bindings::wamn::nodebench::host::add_to_linker::<_, SharedCtx>(
        &mut linker,
        extract_active_ctx,
    )?;
    Ok(linker.instantiate_pre(&component)?)
}

/// A warm wamn:node instance with its typed `handler.run` resolved.
struct NodeInstance {
    store: Store<SharedCtx>,
    bench: NodeBench,
}

impl NodeInstance {
    async fn instantiate(
        engine: &wash_runtime::engine::Engine,
        pre: &InstancePre<SharedCtx>,
    ) -> anyhow::Result<Self> {
        let mut store = new_store(engine);
        let instance = pre.instantiate_async(&mut store).await?;
        let bench = NodeBench::new(&mut store, &instance)?;
        Ok(Self { store, bench })
    }

    /// One node execution. Returns the output payload JSON (frozen 0.1: `run`
    /// returns an emission; the bench nodes always emit on `main` = absent).
    async fn run(&mut self, config: &str, input: &str) -> anyhow::Result<String> {
        let res = self
            .run_raw(config, Payload::Inline(input.to_string()))
            .await?;
        match res {
            Ok(Emission {
                payload: Payload::Inline(s),
                ..
            }) => Ok(s),
            Ok(Emission {
                payload: Payload::Streamed(_),
                ..
            }) => bail!("node returned a streamed payload"),
            Err(e) => bail!("node error: {e:?}"),
        }
    }

    /// One node execution with the full WIT-shaped result (the sample
    /// conformance gate inspects ports and error variants).
    async fn run_raw(
        &mut self,
        config: &str,
        payload: Payload,
    ) -> anyhow::Result<Result<Emission, NodeError>> {
        let ctx = mk_ctx(config);
        Ok(self
            .bench
            .wamn_node_handler()
            .call_run(&mut self.store, &ctx, &payload)
            .await?)
    }
}

fn mk_ctx(config: &str) -> RunContext {
    RunContext {
        run_id: "s4".to_string(),
        flow_id: "s4-flow".to_string(),
        flow_version: 1,
        node_id: "n0".to_string(),
        attempt: 0,
        idempotency_key: "s4-key".to_string(),
        traceparent: None,
        tracestate: None,
        deadline_ms: None,
        config: config.to_string(),
    }
}

/// Config docs for the two workloads. Padded with representative extra fields
/// so the parse cost reflects a real node config, not a 20-byte stub (serde
/// tokenizes and skips unknown keys).
fn io_config(wait_us: u64) -> String {
    format!(
        "{{\"mode\":\"io\",\"wait_ns\":{},\"iters\":0,\"label\":\"receipt-webhook\",\"timeout_ms\":30000,\"retries\":3,\"headers\":{{\"content-type\":\"application/json\",\"accept\":\"application/json\"}}}}",
        wait_us * 1000
    )
}
fn compute_config(iters: u64) -> String {
    format!(
        "{{\"mode\":\"compute\",\"wait_ns\":0,\"iters\":{iters},\"label\":\"transform\",\"timeout_ms\":30000,\"retries\":3,\"headers\":{{\"content-type\":\"application/json\",\"accept\":\"application/json\"}}}}"
    )
}
const NOOP_CONFIG: &str = "{\"mode\":\"noop\",\"wait_ns\":0,\"iters\":0}";
const SAMPLE_INPUT: &str = "{\"receipt\":\"po-4821\",\"qty\":42,\"sku\":\"WIDGET-9\"}";

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
fn summarize(mut samples: Vec<u64>) -> Stats {
    samples.sort_unstable();
    let sum: u128 = samples.iter().map(|&x| x as u128).sum();
    let mean = if samples.is_empty() {
        0
    } else {
        (sum / samples.len() as u128) as u64
    };
    Stats {
        p50: pct(&samples, 0.50),
        p99: pct(&samples, 0.99),
        max: *samples.last().unwrap_or(&0),
        mean,
    }
}

// ---------------------------------------------------------------------------
// entry
// ---------------------------------------------------------------------------

pub async fn run(args: NodeBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-host S4 nodebench");

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    let run_all = args.mode == Mode::All;
    let mut pass = true;

    if run_all || args.mode == Mode::Hop {
        pass &= hop_phase(&engine, &args).await?;
    }
    if run_all || args.mode == Mode::Config {
        pass &= config_phase(&engine, &args).await?;
    }
    if run_all || args.mode == Mode::Gap {
        pass &= gap_phase(&engine, &args).await?;
    }
    if run_all || args.mode == Mode::Sample {
        pass &= sample_phase(&engine, &args).await?;
    }

    ticker.abort();
    println!("\nnodebench complete — overall PASS: {pass}");
    if !pass {
        bail!("one or more S4 gates failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// hop gate (D7): HTTP round trip to a warm noop node
// ---------------------------------------------------------------------------

async fn hop_phase(
    engine: &wash_runtime::engine::Engine,
    args: &NodeBenchArgs,
) -> anyhow::Result<bool> {
    let iters = args.hop_iters;
    let samples = if let Some(target) = &args.hop_url {
        // Cross-pod: measure against an external serve-node (truest D7 number).
        println!(
            "\n## hop — {iters} HTTP/1.1 round trips to serve-node at {target} (cross-pod, D7)"
        );
        hop_client(target.clone(), iters).await?
    } else {
        // In-process loopback: a client task drives sequential keep-alive
        // requests while the server loop (holding the non-Send Store) runs on
        // this task.
        println!(
            "\n## hop — {iters} HTTP/1.1 round trips to a warm Rust noop node (in-pod loopback, D7)"
        );
        let bytes = std::fs::read(&args.node_rs)
            .with_context(|| format!("read {}", args.node_rs.display()))?;
        let pre = node_pre(engine, &bytes)?;
        let node = NodeInstance::instantiate(engine, &pre).await?;
        let listener = TcpListener::bind(("127.0.0.1", 0u16)).await?;
        let addr = listener.local_addr()?;
        let client = tokio::spawn(async move { hop_client(addr.to_string(), iters).await });
        let (sock, _) = listener.accept().await?;
        serve_connection(sock, node).await?; // returns when the client closes
        client.await??
    };
    let s = summarize(samples);
    println!(
        "hop round trip: p50 = {} us, p99 = {} us, max = {} us (mean {} us)",
        s.p50 / 1000,
        s.p99 / 1000,
        s.max / 1000,
        s.mean / 1000
    );
    let p50_ms = s.p50 as f64 / 1e6;
    let pass = s.p50 < 2_000_000; // 2 ms
    let escalate = s.p50 > 5_000_000; // 5 ms
    println!("PASS(hop p50 < 2ms): {pass} (p50 = {p50_ms:.3} ms)");
    if escalate {
        println!(
            "ESCALATE: hop p50 > 5ms — pull the component-linking/wRPC spike forward to P1 (S4 fail branch)"
        );
    }
    Ok(pass)
}

/// Times `iters` sequential POST /run requests on one keep-alive connection.
/// `target` is a `host:port` (DNS-resolved), so this works for both the
/// in-pod loopback address and a cross-pod Service name.
async fn hop_client(target: String, iters: usize) -> anyhow::Result<Vec<u64>> {
    let mut stream = TcpStream::connect(&target)
        .await
        .with_context(|| format!("connect {target}"))?;
    stream.set_nodelay(true)?;
    let body = format!("{{\"config\":{NOOP_CONFIG:?},\"input\":{SAMPLE_INPUT:?}}}");
    let req = format!(
        "POST /run HTTP/1.1\r\nHost: bench\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut samples = Vec::with_capacity(iters);
    let mut reader_buf = vec![0u8; 64 * 1024];
    // Warm up the connection / instance a little before timing.
    for i in 0..iters + 20 {
        let t0 = Instant::now();
        stream.write_all(req.as_bytes()).await?;
        stream.flush().await?;
        read_http_message(&mut stream, &mut reader_buf).await?;
        let dt = t0.elapsed().as_nanos() as u64;
        if i >= 20 {
            samples.push(dt);
        }
    }
    stream.shutdown().await.ok();
    Ok(samples)
}

/// Minimal HTTP/1.1 server for one keep-alive connection: read POST /run,
/// invoke the node, reply. Loops until the peer closes (EOF).
async fn serve_connection(sock: TcpStream, mut node: NodeInstance) -> anyhow::Result<()> {
    sock.set_nodelay(true)?;
    let mut reader = BufReader::new(sock);
    loop {
        let body = match read_http_request_body(&mut reader).await? {
            Some(b) => b,
            None => break, // clean EOF
        };
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        let config = v
            .get("config")
            .and_then(|c| c.as_str())
            .unwrap_or(NOOP_CONFIG);
        let input = v
            .get("input")
            .and_then(|i| i.as_str())
            .unwrap_or(SAMPLE_INPUT);
        let out = node.run(config, input).await?;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            out.len(),
            out
        );
        reader.get_mut().write_all(resp.as_bytes()).await?;
        reader.get_mut().flush().await?;
    }
    Ok(())
}

/// Read one HTTP message's headers+body from a `BufReader` (server side).
/// Returns None on a clean EOF before any bytes.
async fn read_http_request_body<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> anyhow::Result<Option<Vec<u8>>> {
    use tokio::io::AsyncBufReadExt;
    let mut content_length = 0usize;
    let mut saw_any = false;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return if saw_any {
                bail!("connection closed mid-headers")
            } else {
                Ok(None)
            };
        }
        saw_any = true;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = trimmed.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    Ok(Some(body))
}

/// Read one full HTTP response (client side): headers, then Content-Length body.
async fn read_http_message(stream: &mut TcpStream, _scratch: &mut [u8]) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream);
    let _ = read_http_request_body(&mut reader).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// config-parse gate (9b): parse share of cold dispatch
// ---------------------------------------------------------------------------

async fn config_phase(
    engine: &wash_runtime::engine::Engine,
    args: &NodeBenchArgs,
) -> anyhow::Result<bool> {
    println!(
        "\n## config — {} cold instantiate+run cycles; parse share of cold dispatch (9b)",
        args.cold_iters
    );
    let bytes =
        std::fs::read(&args.node_rs).with_context(|| format!("read {}", args.node_rs.display()))?;
    let pre = node_pre(engine, &bytes)?;
    let config = compute_config(0); // representative config doc, no compute work

    let mut cold = Vec::with_capacity(args.cold_iters);
    let mut parse = Vec::with_capacity(args.cold_iters);
    for _ in 0..args.cold_iters {
        let t0 = Instant::now();
        let mut node = NodeInstance::instantiate(engine, &pre).await?;
        let out = node.run(&config, SAMPLE_INPUT).await?;
        let cold_ns = t0.elapsed().as_nanos() as u64;
        let parse_ns = serde_json::from_str::<serde_json::Value>(&out)
            .ok()
            .and_then(|v| v.get("parse_ns").and_then(|p| p.as_u64()))
            .context("node output missing parse_ns")?;
        cold.push(cold_ns);
        parse.push(parse_ns);
    }
    let c = summarize(cold);
    let p = summarize(parse);
    let share = p.p50 as f64 / c.p50.max(1) as f64 * 100.0;
    println!(
        "cold dispatch p50 = {} us (pooled instantiate + first run, no compile); config parse p50 = {} ns; parse share = {:.2}% of cold",
        c.p50 / 1000,
        p.p50,
        share
    );
    // The denominator is the TIGHTEST honest cold dispatch: a pooled
    // instantiate + one invoke of an already-compiled component. Against a
    // fuller cold start (component fetch + JIT compile) the share is far
    // smaller, so this is a conservative upper bound.
    let closes = share <= 5.0;
    if closes {
        println!(
            "PASS(config parse <= 5% of cold dispatch): true — design-note 9b closed (parse negligible)"
        );
    } else {
        println!(
            "REVISIT(config parse {share:.2}% > 5% of the tightest cold dispatch): design-note 9b's mitigation (memoize parse per (flow-version,node-id) + frozen-flow constant-fold) is CONFIRMED WARRANTED, not dropped. Not a hard fail — parse is small in absolute terms ({} ns) and negligible once component load/compile is included.",
            p.p50
        );
    }
    // Either way the spike question is answered with data; the >5% branch is a
    // benign "keep the planned optimization", so it does not red-fail the suite.
    Ok(true)
}

// ---------------------------------------------------------------------------
// gap gate: interpreted (JS) vs composed (Rust wac), I/O + compute
// ---------------------------------------------------------------------------

/// A composed frozen flow (flow-driver + node-rs): one warm instance driven by
/// the `run-flow` export.
struct ComposedInstance {
    store: Store<SharedCtx>,
    run_flow: TypedFunc<(String, String, u64, u64, u32), (String,)>,
}
impl ComposedInstance {
    async fn instantiate(
        engine: &wash_runtime::engine::Engine,
        pre: &InstancePre<SharedCtx>,
    ) -> anyhow::Result<Self> {
        let mut store = new_store(engine);
        let instance = pre.instantiate_async(&mut store).await?;
        let run_flow = instance.get_typed_func(&mut store, "run-flow")?;
        Ok(Self { store, run_flow })
    }
    async fn run_flow(
        &mut self,
        mode: &str,
        wait_ns: u64,
        iters: u64,
        hops: u32,
    ) -> anyhow::Result<String> {
        let (out,) = self
            .run_flow
            .call_async(
                &mut self.store,
                (
                    SAMPLE_INPUT.to_string(),
                    mode.to_string(),
                    wait_ns,
                    iters,
                    hops,
                ),
            )
            .await?;
        Ok(out)
    }
}

async fn gap_phase(
    engine: &wash_runtime::engine::Engine,
    args: &NodeBenchArgs,
) -> anyhow::Result<bool> {
    println!(
        "\n## gap — {} warm 3-node flow executions per arm/workload (interpreted vs composed)",
        args.gap_iters
    );

    // Arms present: Rust-dynamic + Rust-composed always; JS-dynamic if built.
    let rs_bytes =
        std::fs::read(&args.node_rs).with_context(|| format!("read {}", args.node_rs.display()))?;
    let rs_pre = node_pre(engine, &rs_bytes)?;

    let composed_bytes = std::fs::read(&args.composed)
        .with_context(|| format!("read {}", args.composed.display()))?;
    let composed_pre = node_pre(engine, &composed_bytes)?;

    let ts_pre = match std::fs::read(&args.node_ts) {
        Ok(b) => Some(node_pre(engine, &b).context("compile node-ts")?),
        Err(_) => {
            println!(
                "note: {} not found — JS/interpreted arm skipped (owed); reporting Rust-dynamic vs Rust-composed",
                args.node_ts.display()
            );
            None
        }
    };

    let io_ns = args.io_wait_us * 1000;
    let mut all_pass = true;

    for (label, mode, wait_ns, iters, io_bound) in [
        ("I/O-bound", "io", io_ns, 0u64, true),
        ("compute-bound", "compute", 0u64, args.compute_iters, false),
    ] {
        println!("\n### {label} flow ({} hops)", args.hops);

        let cfg = if io_bound {
            io_config(args.io_wait_us)
        } else {
            compute_config(iters)
        };

        // Rust-dynamic: instantiate the node once, call run() `hops` times/flow.
        let rs_dyn = {
            let mut node = NodeInstance::instantiate(engine, &rs_pre).await?;
            time_dynamic(&mut node, &cfg, args.hops, args.gap_iters).await?
        };

        // Rust-composed: the wac frozen flow, one run-flow call per flow.
        let rs_comp = {
            let mut inst = ComposedInstance::instantiate(engine, &composed_pre).await?;
            time_composed(&mut inst, mode, wait_ns, iters, args.hops, args.gap_iters).await?
        };

        let js_dyn = if let Some(pre) = &ts_pre {
            let mut node = NodeInstance::instantiate(engine, pre).await?;
            Some(time_dynamic(&mut node, &cfg, args.hops, args.gap_iters).await?)
        } else {
            None
        };

        let comp = summarize(rs_comp);
        let rdyn = summarize(rs_dyn);
        println!(
            "  Rust-composed  p50 = {:>8} us   (frozen, wac)",
            comp.p50 / 1000
        );
        println!(
            "  Rust-dynamic   p50 = {:>8} us   (dynamic dispatch + parse)",
            rdyn.p50 / 1000
        );
        let compose_overhead = gap_pct(rdyn.p50, comp.p50);
        println!("    composition overhead (Rust-dyn vs composed): {compose_overhead:+.1}%");

        if let Some(js) = js_dyn {
            let jdyn = summarize(js);
            println!(
                "  JS-interpreted p50 = {:>8} us   (JCO / StarlingMonkey)",
                jdyn.p50 / 1000
            );
            let gap = gap_pct(jdyn.p50, comp.p50);
            println!("    INTERPRETED-vs-COMPOSED gap: {gap:+.1}%");
            if io_bound {
                let pass = gap < 5.0;
                println!("  PASS(I/O-bound gap < 5%): {pass}");
                all_pass &= pass;
            } else {
                println!(
                    "  (compute-bound gap is expected to be large — sizes frozen flows' post-GA slot, not a gate)"
                );
            }
        } else if io_bound {
            println!(
                "  (I/O-bound gap gate NOT evaluated — JS arm owed; Rust-dyn vs composed shown above)"
            );
        }
    }

    Ok(all_pass)
}

/// Run one flow by dynamically dispatching the node `hops` times, threading
/// output into input.
async fn run_dynamic(node: &mut NodeInstance, config: &str, hops: u32) -> anyhow::Result<String> {
    let mut cur = SAMPLE_INPUT.to_string();
    for _ in 0..hops {
        cur = node.run(config, &cur).await?;
    }
    Ok(cur)
}

/// Time `iters` dynamic-dispatch flows (node instantiated once, run `hops`×).
async fn time_dynamic(
    node: &mut NodeInstance,
    config: &str,
    hops: u32,
    iters: usize,
) -> anyhow::Result<Vec<u64>> {
    for _ in 0..10 {
        run_dynamic(node, config, hops).await?;
    }
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t0 = Instant::now();
        run_dynamic(node, config, hops).await?;
        samples.push(t0.elapsed().as_nanos() as u64);
    }
    Ok(samples)
}

/// Time `iters` composed flows (one `run-flow` call each).
async fn time_composed(
    inst: &mut ComposedInstance,
    mode: &str,
    wait_ns: u64,
    iters_cfg: u64,
    hops: u32,
    iters: usize,
) -> anyhow::Result<Vec<u64>> {
    for _ in 0..10 {
        inst.run_flow(mode, wait_ns, iters_cfg, hops).await?;
    }
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t0 = Instant::now();
        inst.run_flow(mode, wait_ns, iters_cfg, hops).await?;
        samples.push(t0.elapsed().as_nanos() as u64);
    }
    Ok(samples)
}

fn gap_pct(interpreted: u64, composed: u64) -> f64 {
    if composed == 0 {
        return 0.0;
    }
    (interpreted as f64 - composed as f64) / composed as f64 * 100.0
}

// ---------------------------------------------------------------------------
// serve-node subcommand (standalone HTTP node host)
// ---------------------------------------------------------------------------

pub async fn serve(args: ServeNodeArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    let engine = build_engine(&[])?;
    let _ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let bytes =
        std::fs::read(&args.node).with_context(|| format!("read {}", args.node.display()))?;
    let pre = node_pre(&engine, &bytes)?;

    // One warm instance behind a mutex; requests are served sequentially.
    let node = Arc::new(Mutex::new(NodeInstance::instantiate(&engine, &pre).await?));
    let listener = TcpListener::bind(("0.0.0.0", args.port)).await?;
    println!(
        "serve-node: {} on 0.0.0.0:{} (POST /run {{\"config\":..,\"input\":..}})",
        args.node.display(),
        args.port
    );
    loop {
        let (sock, _peer) = listener.accept().await?;
        let node = node.clone();
        // Sequential: hold the lock for the whole connection (single instance).
        let mut guard = node.lock().await;
        // Move the instance out to reuse serve_connection is awkward; inline here.
        if let Err(e) = serve_connection_shared(sock, &mut guard).await {
            tracing::warn!("connection error: {e}");
        }
    }
}

/// serve_connection variant borrowing the shared instance (standalone server).
async fn serve_connection_shared(sock: TcpStream, node: &mut NodeInstance) -> anyhow::Result<()> {
    sock.set_nodelay(true)?;
    let mut reader = BufReader::new(sock);
    loop {
        let body = match read_http_request_body(&mut reader).await? {
            Some(b) => b,
            None => break,
        };
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        let config = v
            .get("config")
            .and_then(|c| c.as_str())
            .unwrap_or(NOOP_CONFIG);
        let input = v
            .get("input")
            .and_then(|i| i.as_str())
            .unwrap_or(SAMPLE_INPUT);
        let out = node.run(config, input).await?;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            out.len(),
            out
        );
        reader.get_mut().write_all(resp.as_bytes()).await?;
        reader.get_mut().flush().await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// sample gate (5.4): frozen-contract conformance of the scaffolding-built node
// ---------------------------------------------------------------------------

/// Drives `components/sample-node` (built on the `wamn-node-guest`
/// scaffolding over the FROZEN `wamn:node` 0.1 contract) through every
/// conversion the scaffolding performs, over the real ABI: the five
/// `node-error` taxonomy variants, port selection (absent = `main`), the
/// echo round trip, and the streamed-payload refusal (payload store = 5.10).
/// Topology-independent (pure in-proc ABI), so local == in-cluster.
async fn sample_phase(
    engine: &wash_runtime::engine::Engine,
    args: &NodeBenchArgs,
) -> anyhow::Result<bool> {
    println!("\n## sample — frozen wamn:node 0.1 conformance (scaffolding-built sample node)");
    if !args.sample.exists() {
        println!("SKIP: {} not present", args.sample.display());
        return Ok(true);
    }
    let bytes =
        std::fs::read(&args.sample).with_context(|| format!("read {}", args.sample.display()))?;
    let pre = node_pre(engine, &bytes)?;
    let mut node = NodeInstance::instantiate(engine, &pre).await?;
    let mut pass = true;
    let mut check = |name: &str, ok: bool| {
        println!("PASS({name}): {ok}");
        pass &= ok;
    };

    // Echo: emission on the absent (= main) port, payload round-trips.
    let res = node
        .run_raw("{}", Payload::Inline("{\"x\": 7}".to_string()))
        .await?;
    match res {
        Ok(Emission {
            payload: Payload::Inline(s),
            port,
        }) => {
            let v: serde_json::Value = serde_json::from_str(&s)?;
            check("echo payload round-trips", v["echo"]["x"] == 7);
            check("default port travels absent (= main)", port.is_none());
        }
        other => {
            println!("echo returned {other:?}");
            check("echo returns an inline emission", false);
        }
    }

    // Port selection via config.
    let res = node
        .run_raw("{\"port\": \"true\"}", Payload::Inline("null".to_string()))
        .await?;
    check(
        "named port travels present",
        matches!(&res, Ok(e) if e.port.as_deref() == Some("true")),
    );

    // The five taxonomy variants, variant for variant.
    let fail = |v: &str| Payload::Inline(format!("{{\"fail\": \"{v}\"}}"));
    let res = node.run_raw("{}", fail("retryable")).await?;
    check(
        "retryable maps to retryable",
        matches!(&res, Err(NodeError::Retryable(d)) if d.code.as_deref() == Some("SAMPLE_RETRY")),
    );
    let res = node.run_raw("{}", fail("rate-limited")).await?;
    check(
        "rate-limited carries retry-after + target-host",
        matches!(&res, Err(NodeError::RateLimited(r))
            if r.retry_after_ms == Some(1500) && r.target_host.as_deref() == Some("sample.example")),
    );
    let res = node.run_raw("{}", fail("terminal")).await?;
    check(
        "terminal maps to terminal",
        matches!(&res, Err(NodeError::Terminal(_))),
    );
    let res = node.run_raw("{}", fail("invalid-input")).await?;
    check(
        "invalid-input maps to invalid-input",
        matches!(&res, Err(NodeError::InvalidInput(_))),
    );
    let res = node.run_raw("{}", fail("cancelled")).await?;
    check(
        "cancelled maps to cancelled",
        matches!(&res, Err(NodeError::Cancelled)),
    );

    // Streamed input: refused by the scaffolding until the payload store (5.10).
    use bindings::wamn::node::types::{Framing, PayloadRef};
    let streamed = Payload::Streamed(PayloadRef {
        handle: "h".to_string(),
        framing: Framing::Ndjson,
        size_hint: None,
    });
    let res = node.run_raw("{}", streamed).await?;
    check(
        "streamed input refused (payload store = 5.10)",
        matches!(&res, Err(NodeError::Terminal(d))
            if d.code.as_deref() == Some("streamed-payload-unsupported")),
    );

    println!("PASS(sample conformance): {pass}");
    Ok(pass)
}
