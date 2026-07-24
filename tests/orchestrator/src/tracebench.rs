//! The `tracebench` subcommand: the [9.1] OTel trace-pipeline gate.
//!
//! 9.1 ships host-native spans → OTel Collector → Tempo, enriched with
//! `tenant`/`project` (and, on the host-minted dispatcher path,
//! `flow`/`run_id`). The exporter, the W3C propagator, and wash-runtime's
//! inbound/outbound HTTP + component-invoke spans are already wired (the fork's
//! `initialize_observability`, active whenever `OTEL_*` is set — the S5 logging
//! pattern). What 9.1 adds host-side is two enriched spans this gate proves
//! end to end, the S5 `logbench`→Loki analog but against Tempo's TraceQL API:
//!
//!   * a **`wamn.trigger`** span — the real
//!     [`wamn_dispatcher::trigger_span`] the dispatcher roots a fired run's
//!     trace with, carrying `wamn.flow`/`wamn.run_id`/`wamn.tenant`;
//!   * a **`wamn.postgres`** DB span — the real span the `wamn:postgres` plugin
//!     wraps each guest DB call in, carrying `db.system`/`wamn.tenant`/
//!     `wamn.project`.
//!
//! The gate drives a real guest DB call (`pgprobe` op 6, `SELECT pg_sleep(0)` —
//! fixture-free) *under* the real `trigger_span`, so the plugin's DB span nests
//! beneath it. It then queries Tempo and asserts **one trace** threads the
//! trigger span → the DB span, both enriched. That single trace is the
//! `trigger → runner → wamn:postgres` thread of the plan's acceptance script,
//! proven through the production span builders (not gate scaffolding).
//!
//! Cross-pod threading (traceparent injection on outbound calls) and
//! guest-minted `run_id`/`node_id` are 9.2 (`docs/tracing.md` § Boundaries).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::Instrument as _;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::TypedFunc;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, InstancePre, Linker};

use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};
use wamn_run_queue::Firing;

type RawEngine = wash_runtime::wasmtime::Engine;

// The gate component's identity: the plugin maps it to the tenant claim, and
// `db_span` reads the same map for the DB span's `wamn.tenant`.
const BENCH_ID: &str = "wamn-tracebench";
const TRACE_TENANT: &str = "trace-tenant";
const FLOW_ID: &str = "trace-flow";

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Drive a guest DB call under the real trigger span; assert the enriched
    /// single trace (trigger → wamn:postgres) in Tempo.
    Flow,
    /// Every gate (currently just `flow`).
    All,
}

#[derive(Debug, Args)]
pub struct TracebenchArgs {
    /// The `pgprobe` guest — op 6 (`SELECT pg_sleep`) is fixture-free.
    #[arg(long, default_value = "/bench/pgprobe.wasm")]
    pgprobe: PathBuf,
    /// `wamn_app` connection URL (or `DATABASE_URL` / `WAMN_PG_URL`).
    #[arg(long)]
    database_url: Option<String>,
    /// Tempo's HTTP API base (TraceQL `/api/search`, `/api/traces/<id>`).
    #[arg(long, env = "TEMPO_URL", default_value = "http://127.0.0.1:3200")]
    tempo_url: String,
    #[arg(long, value_enum, default_value_t = Mode::All)]
    mode: Mode,
}

// ---------------------------------------------------------------------------
// Guest driver (the pgbench store-build pattern, one guest)
// ---------------------------------------------------------------------------

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

    fn plugin_map(&self) -> HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> {
        let mut m = HashMap::new();
        m.insert(
            wamn_postgres::WAMN_POSTGRES_ID,
            self.plugin.clone() as Arc<dyn HostPlugin + Send + Sync>,
        );
        m
    }

    async fn worker(&self) -> anyhow::Result<Worker> {
        let ctx = Ctx::builder(BENCH_ID.to_string(), BENCH_ID.to_string())
            .with_plugins(self.plugin_map())
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = self.pre.instantiate_async(&mut store).await?;
        let func =
            instance.get_typed_func::<(u32, String), (Result<u64, String>,)>(&mut store, "run")?;
        Ok(Worker { store, func })
    }
}

pub async fn run(args: TracebenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    // The exporter only activates when OTEL_* is set (fork observability init).
    if !std::env::vars().any(|(k, _)| k.starts_with("OTEL_")) {
        bail!(
            "no OTEL_* env set: tracebench needs OTEL_EXPORTER_OTLP_ENDPOINT pointing at the collector (else nothing is exported)"
        );
    }

    let guest = std::fs::read(&args.pgprobe)
        .with_context(|| format!("failed to read {}", args.pgprobe.display()))?;

    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    if cfg.database_url.is_none() {
        bail!("no database url: pass --database-url or set DATABASE_URL / WAMN_PG_URL");
    }

    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    // The gate component's tenant claim — the DB span reads this map.
    plugin.set_tenant(BENCH_ID, TRACE_TENANT)?;

    println!("# wamn-gates [9.1] tracebench");
    println!("tempo = {}", args.tempo_url);

    let engine = build_engine(&[])?;
    let _ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let harness = Harness::new(engine, &guest, plugin.clone())?;

    let mut pass = true;
    // `all` currently == `flow`; kept as a mode for future trace gates.
    let _ = args.mode;
    pass &= flow_phase(&harness, &args).await?;

    println!("overall {}", if pass { "PASS" } else { "FAIL" });
    if pass {
        Ok(())
    } else {
        bail!("tracebench FAIL")
    }
}

/// Drive `pgprobe` op 6 (`SELECT pg_sleep(0)`) *under* the real dispatcher
/// `trigger_span`, then assert the enriched single trace in Tempo.
async fn flow_phase(harness: &Harness, args: &TracebenchArgs) -> anyhow::Result<bool> {
    // A unique run id keys the assertion to THIS invocation's trace.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let run_id = format!("tracebench-{nanos}");

    // The REAL production span builder — a firing the dispatcher would mint.
    let firing = Firing {
        run_id: run_id.clone(),
        flow_id: FLOW_ID.to_string(),
        flow_version: 1,
        input_json: "{}".to_string(),
        trigger_source: "cron".to_string(),
    };
    let span = wamn_dispatcher::trigger_span(&firing, TRACE_TENANT);

    let mut worker = harness.worker().await?;
    // Instrumenting the guest call with the trigger span makes the plugin's
    // `wamn.postgres` DB span (created inside) a child — one trace.
    let ret = worker
        .call(6, "0")
        .instrument(span)
        .await
        .context("driving pgprobe op 6 (pg_sleep) failed")?;
    if let Err(e) = ret {
        bail!("guest DB call errored (is the wamn_app DB reachable?): {e}");
    }

    // Poll Tempo until the trace is exported + ingested (bounded — this waits on
    // the OTel batch export + collector + Tempo ingestion, not k8s readiness).
    let trace = match await_trace(&args.tempo_url, &run_id).await? {
        Some(t) => t,
        None => {
            println!("  FAIL: trace for run_id={run_id} never appeared in Tempo");
            return Ok(false);
        }
    };

    let mut pass = true;

    // 1. The trigger span carries flow/run/tenant.
    let trigger = trace
        .iter()
        .find(|s| s.name == "wamn.trigger" && s.attrs.get("wamn.run_id") == Some(&run_id));
    let trigger = match trigger {
        Some(t) => t,
        None => {
            println!("  FAIL: no wamn.trigger span with wamn.run_id={run_id}");
            return Ok(false);
        }
    };
    check(
        &mut pass,
        "trigger span enriched (flow/run/tenant)",
        trigger.attrs.get("wamn.flow").map(String::as_str) == Some(FLOW_ID)
            && trigger.attrs.get("wamn.tenant").map(String::as_str) == Some(TRACE_TENANT),
        &format!("attrs = {:?}", trigger.attrs),
    );

    // 2. The wamn:postgres DB span carries db.system/tenant/project.
    let db = trace.iter().find(|s| s.name == "wamn.postgres");
    let db = match db {
        Some(d) => d,
        None => {
            println!("  FAIL: no wamn.postgres DB span in the trace");
            return Ok(false);
        }
    };
    check(
        &mut pass,
        "DB span enriched (db.system/tenant/project)",
        db.attrs.get("db.system").map(String::as_str) == Some("postgresql")
            && db.attrs.get("wamn.tenant").map(String::as_str) == Some(TRACE_TENANT)
            && db
                .attrs
                .get("wamn.project")
                .map(|p| !p.is_empty())
                .unwrap_or(false),
        &format!("attrs = {:?}", db.attrs),
    );

    // 3. The DB span threads under the trigger span — one trace, one thread.
    check(
        &mut pass,
        "single trace threads trigger → wamn:postgres",
        !db.parent_span_id.is_empty() && db.parent_span_id == trigger.span_id,
        &format!(
            "db.parent={} trigger.span={}",
            db.parent_span_id, trigger.span_id
        ),
    );

    Ok(pass)
}

fn check(pass: &mut bool, label: &str, ok: bool, detail: &str) {
    if ok {
        println!("  PASS: {label}");
    } else {
        *pass = false;
        println!("  FAIL: {label} ({detail})");
    }
}

// ---------------------------------------------------------------------------
// Tempo TraceQL query (the S5 logbench→Loki analog)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SpanRec {
    name: String,
    span_id: String,
    parent_span_id: String,
    attrs: HashMap<String, String>,
}

/// Poll Tempo for the trace whose `wamn.trigger` span carries `run_id`. Bounded
/// (~30s) to cover the OTel batch export delay + collector + Tempo ingestion.
async fn await_trace(tempo: &str, run_id: &str) -> anyhow::Result<Option<Vec<SpanRec>>> {
    for attempt in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let ids = match search_trigger_traces(tempo).await {
            Ok(ids) => ids,
            Err(e) => {
                if attempt == 0 {
                    println!("  (waiting on Tempo: {e})");
                }
                continue;
            }
        };
        for id in ids {
            if let Ok(spans) = get_trace(tempo, &id).await {
                let hit = spans.iter().any(|s| {
                    s.name == "wamn.trigger"
                        && s.attrs.get("wamn.run_id") == Some(&run_id.to_string())
                });
                if hit {
                    return Ok(Some(spans));
                }
            }
        }
    }
    Ok(None)
}

/// TraceQL search by the intrinsic span `name` (robust — no dotted-attribute
/// selector); the run_id filter happens Rust-side over the full traces.
async fn search_trigger_traces(tempo: &str) -> anyhow::Result<Vec<String>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let q = percent_encode("{ name = \"wamn.trigger\" }");
    let path = format!(
        "/api/search?q={q}&limit=50&start={}&end={}",
        now.saturating_sub(600),
        now + 60
    );
    let body = http_get(tempo, &path).await?;
    let v: serde_json::Value =
        serde_json::from_str(&body).with_context(|| format!("Tempo search JSON: {body:.200}"))?;
    let mut ids = Vec::new();
    if let Some(traces) = v.get("traces").and_then(|t| t.as_array()) {
        for t in traces {
            if let Some(id) = t.get("traceID").and_then(|i| i.as_str()) {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}

/// Fetch one trace as OTLP JSON and flatten every span across batches/scopes.
async fn get_trace(tempo: &str, trace_id: &str) -> anyhow::Result<Vec<SpanRec>> {
    let body = http_get(tempo, &format!("/api/traces/{trace_id}")).await?;
    let v: serde_json::Value =
        serde_json::from_str(&body).with_context(|| format!("Tempo trace JSON: {body:.200}"))?;
    let mut out = Vec::new();
    let batches = v
        .get("batches")
        .or_else(|| v.get("resourceSpans"))
        .and_then(|b| b.as_array());
    if let Some(batches) = batches {
        for b in batches {
            let scopes = b
                .get("scopeSpans")
                .or_else(|| b.get("instrumentationLibrarySpans"))
                .and_then(|s| s.as_array());
            if let Some(scopes) = scopes {
                for sc in scopes {
                    if let Some(spans) = sc.get("spans").and_then(|s| s.as_array()) {
                        for sp in spans {
                            out.push(span_rec(sp));
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

fn span_rec(sp: &serde_json::Value) -> SpanRec {
    let str_of = |k: &str| sp.get(k).and_then(|n| n.as_str()).unwrap_or("").to_string();
    let mut attrs = HashMap::new();
    if let Some(a) = sp.get("attributes").and_then(|a| a.as_array()) {
        for kv in a {
            if let (Some(key), Some(val)) = (
                kv.get("key").and_then(|k| k.as_str()),
                kv.get("value").and_then(attr_value_str),
            ) {
                attrs.insert(key.to_string(), val);
            }
        }
    }
    SpanRec {
        name: str_of("name"),
        span_id: str_of("spanId"),
        parent_span_id: str_of("parentSpanId"),
        attrs,
    }
}

/// OTLP attribute values arrive typed; the enrichment we assert on is string
/// (tenant/project/flow/run) or int (flow_version).
fn attr_value_str(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.get("stringValue").and_then(|x| x.as_str()) {
        return Some(s.to_string());
    }
    if let Some(iv) = v.get("intValue") {
        return iv
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| iv.as_i64().map(|n| n.to_string()));
    }
    if let Some(b) = v.get("boolValue").and_then(|x| x.as_bool()) {
        return Some(b.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 GET (Tempo's API is Go/chunked, like Loki in logbench).
// ---------------------------------------------------------------------------

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
    let status_ok = text.starts_with("HTTP/1.1 2") || text.starts_with("HTTP/1.0 2");
    if !status_ok {
        let status = text.lines().next().unwrap_or("<none>");
        bail!("GET {path} -> {status}");
    }
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .map(|(h, b)| (h.to_string(), b.to_string()))
        .unwrap_or_default();
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        Ok(dechunk(&body))
    } else {
        Ok(body)
    }
}

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
        rest = after[size..].strip_prefix("\r\n").unwrap_or(&after[size..]);
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}
