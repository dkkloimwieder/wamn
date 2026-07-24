//! # wamn:logging plugin (S5 — logging capture PoC, docs/archive/p0-exit-criteria.md S5)
//!
//! Implements `wasi:logging/logging` for guests, but unlike the vendored
//! `TracingLogger` (which only tags the workload/component identity and routes
//! to `tracing`) this plugin is the **capture path** the S5 gates measure:
//!
//!   * **Enrichment (100%).** Every record carries `tenant`/`project`
//!     (host-injected from a component→claim map — a guest can *not* spoof its
//!     tenant) plus `flow`/`run`/`node` parsed from the guest's `context`
//!     string (the runner legitimately knows these). All five land as
//!     structured OTel log attributes.
//!   * **Non-blocking `log()` (<50 µs guest-observed).** `log()` only enriches
//!     and `try_send`s onto a bounded front queue, then returns. A background
//!     drain task feeds an OTLP `LoggerProvider` this plugin owns. The
//!     guest-observed cost is boundary + enrich + enqueue — never the export.
//!   * **Visible drops (not silent).** The bounded front queue is the *only*
//!     intentional drop point; on queue-full `log()` increments an atomic drop
//!     counter that is also surfaced as an OTel metric (`wamn.logging.dropped`).
//!     Everything downstream (a generously sized batch processor → collector →
//!     Loki) is sized not to drop, so unaccounted loss ≈ 0.
//!
//! # Single owner of the guest-log pipeline (9.3 / wamn-yf3)
//!
//! This plugin is THE guest-log pipeline: it owns its OWN `SdkLoggerProvider`
//! (generous batch queue → OTLP gRPC → collector → Loki) rather than reusing the
//! fork's vendored `observability.rs` logs pipeline. That fork pipeline is
//! **host-internal tracing only** — its batch queue is fixed at 2048 and its OTLP
//! filter is tied to `--log-level`, so it exists to ship the HOST's own `tracing`
//! events, and both limits would bottleneck or misfilter a 10k lines/s stream of
//! GUEST logs. Keeping the two pipelines separate is deliberate: the host tracing
//! path and the guest-log path have different back-pressure, sizing, and
//! filtering needs, and folding guest logs into the fork pipeline would force one
//! set of knobs on both. So "fold into host observability wiring" (the bead's
//! delta) is satisfied by construction, not fusion: the provider is built ONCE
//! from [`WamnLoggingConfig::from_env`] (the `WAMN_LOG_*` knobs, surfaced in the
//! deploy manifests that run this plugin — the runner + logbench Job), and no
//! fork edit is required.
//!
//! ## Trace context (9.2/9.3)
//!
//! The guest `context` string may carry a W3C `traceparent` (+ `tracestate`); the
//! drain task parses it and sets the OTel `LogRecord`'s trace context
//! (trace_id/span_id/flags) so a guest log correlates with its run's trace in the
//! backend. The emitting runner (the flowrunner run path) includes the run's
//! traceparent in that context object.
//!
//! ## Production shape (run path)
//!
//! Beyond the S5 bench, the production flowrunner emits a few structured
//! `wasi:logging` records per run (node completion, node error CLASS, run
//! completion) through THIS plugin, linked into the wamn-run-worker store with
//! the host-injected (unspoofable) tenant/project claim.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::{SpanId, TraceFlags, TraceId};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{BatchConfigBuilder, BatchLogProcessor, SdkLoggerProvider};
use tokio::sync::mpsc;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "logging-plugin",
        imports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wasi::logging::logging::{self, Level};

pub const WAMN_LOGGING_ID: &str = "wamn-logging";

/// Per-workload config keys carrying the host-trusted identity (plumbed from the
/// WorkloadDeployment CRD's `localResources.config`, i.e. set by the platform,
/// not the guest).
pub const TENANT_CONFIG_KEY: &str = "wamn.tenant";
pub const PROJECT_CONFIG_KEY: &str = "wamn.project";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct WamnLoggingConfig {
    /// Bounded front-queue capacity. The only intentional drop point.
    pub queue_capacity: usize,
    /// Max records/sec the drain task feeds downstream (0 = unbounded). A value
    /// below the arrival rate makes the front queue overflow — the S5
    /// saturation demonstration (rate-limit engaging visibly).
    pub drain_rate_per_sec: u64,
    /// Downstream OTLP batch processor queue size (sized ≫ the burst so the
    /// front queue, not the batch processor, is the drop point).
    pub batch_max_queue: usize,
    /// Downstream OTLP batch export chunk.
    pub batch_max_export: usize,
    /// Downstream OTLP batch schedule delay (ms).
    pub batch_schedule_ms: u64,
}

impl Default for WamnLoggingConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 65_536,
            drain_rate_per_sec: 0,
            batch_max_queue: 524_288,
            batch_max_export: 8_192,
            batch_schedule_ms: 200,
        }
    }
}

impl WamnLoggingConfig {
    pub fn from_env() -> Self {
        fn num<T: std::str::FromStr>(key: &str, default: T) -> T {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        }
        let d = Self::default();
        Self {
            queue_capacity: num("WAMN_LOG_QUEUE_CAP", d.queue_capacity),
            drain_rate_per_sec: num("WAMN_LOG_DRAIN_RATE", d.drain_rate_per_sec),
            batch_max_queue: num("WAMN_LOG_BATCH_QUEUE", d.batch_max_queue),
            batch_max_export: num("WAMN_LOG_BATCH_EXPORT", d.batch_max_export),
            batch_schedule_ms: num("WAMN_LOG_BATCH_SCHEDULE_MS", d.batch_schedule_ms),
        }
    }
}

// ---------------------------------------------------------------------------
// Counters (exposed to the bench + surfaced as OTel metrics)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Counters {
    /// Accepted onto the front queue (handed toward the exporter).
    accepted: AtomicU64,
    /// Dropped because the front queue was full (rate-limit engaged; counted).
    dropped: AtomicU64,
    /// Emitted to the OTLP logger by the drain task.
    emitted: AtomicU64,
}

// ---------------------------------------------------------------------------
// Claim + record
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Claim {
    tenant: String,
    project: String,
}

/// One enriched record queued for async OTLP export.
struct Rec {
    severity: Severity,
    tenant: String,
    project: String,
    flow: String,
    run: String,
    node: String,
    seq: Option<i64>,
    run_label: String,
    /// W3C `traceparent` the emitter attached (the run's trace), parsed in the
    /// drain task onto the `LogRecord`'s trace context. `None` when the guest
    /// carries no trace (the S5 bench guest).
    traceparent: Option<String>,
    tracestate: Option<String>,
    message: String,
}

// ---------------------------------------------------------------------------
// Capture seam (gate-facing): an OPT-IN in-memory sink the drain task mirrors
// each emitted record into, so a local gate (logbench `runpath`) can assert
// enrichment + attached trace_id at the Rec layer WITHOUT a collector/Loki. Off
// by default (a `None` handle), so production pays nothing beyond one branch.
// ---------------------------------------------------------------------------

/// One record as it was emitted, for gate inspection. Mirrors the enriched
/// fields plus the `trace_id` actually set on the `LogRecord` (the W3C parse
/// result — `None` when no/garbage traceparent).
#[derive(Clone, Debug)]
pub struct CapturedRec {
    pub severity: Severity,
    pub tenant: String,
    pub project: String,
    pub flow: String,
    pub run: String,
    pub node: String,
    pub seq: Option<i64>,
    pub run_label: String,
    pub trace_id: Option<String>,
    pub message: String,
}

/// A shared, inspectable capture buffer. Cloneable handle shared between the
/// plugin's drain task and the gate that reads it.
#[derive(Default)]
pub struct Capture {
    records: std::sync::Mutex<Vec<CapturedRec>>,
}

impl Capture {
    /// A snapshot of every record emitted so far.
    pub fn snapshot(&self) -> Vec<CapturedRec> {
        self.records.lock().expect("capture lock poisoned").clone()
    }

    fn push(&self, rec: CapturedRec) {
        self.records
            .lock()
            .expect("capture lock poisoned")
            .push(rec);
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct WamnLogging {
    /// component id → host-trusted {tenant, project}.
    claims: std::sync::RwLock<HashMap<String, Claim>>,
    /// Bounded front queue; `try_send` keeps `log()` non-blocking.
    tx: mpsc::Sender<Rec>,
    counters: Arc<Counters>,
    /// Owned OTLP logs pipeline; kept alive + flushed by the bench.
    provider: SdkLoggerProvider,
    _drain: tokio::task::JoinHandle<()>,
}

impl WamnLogging {
    pub fn new(cfg: WamnLoggingConfig) -> anyhow::Result<Self> {
        Self::build(cfg, None)
    }

    /// Build the plugin with a capture sink attached (the gate seam): the drain
    /// task mirrors every emitted record into the returned [`Capture`], so a
    /// local gate can assert enrichment + attached trace_id without a collector.
    pub fn new_with_capture(cfg: WamnLoggingConfig) -> anyhow::Result<(Self, Arc<Capture>)> {
        let capture = Arc::new(Capture::default());
        let plugin = Self::build(cfg, Some(capture.clone()))?;
        Ok((plugin, capture))
    }

    fn build(cfg: WamnLoggingConfig, capture: Option<Arc<Capture>>) -> anyhow::Result<Self> {
        let resource = Resource::builder()
            .with_attribute(opentelemetry::KeyValue::new("service.name", "wamn-host"))
            .build();

        // Build the logs pipeline. When any OTEL_* env is present, export via
        // OTLP gRPC (the operator/bench sets OTEL_EXPORTER_OTLP_ENDPOINT to the
        // collector); otherwise a processor-less provider makes `emit` a no-op
        // so the host path still links wasi:logging safely without a collector.
        let otel_enabled = std::env::vars().any(|(k, _)| k.starts_with("OTEL_"));
        let provider = if otel_enabled {
            let exporter = opentelemetry_otlp::LogExporter::builder()
                .with_tonic()
                .build()?;
            let batch = BatchConfigBuilder::default()
                .with_max_queue_size(cfg.batch_max_queue)
                .with_max_export_batch_size(cfg.batch_max_export)
                .with_scheduled_delay(Duration::from_millis(cfg.batch_schedule_ms))
                .build();
            let processor = BatchLogProcessor::builder(exporter)
                .with_batch_config(batch)
                .build();
            SdkLoggerProvider::builder()
                .with_log_processor(processor)
                .with_resource(resource)
                .build()
        } else {
            SdkLoggerProvider::builder().with_resource(resource).build()
        };

        let counters = Arc::new(Counters::default());
        let (tx, rx) = mpsc::channel::<Rec>(cfg.queue_capacity);

        // Surface the drop + throughput counters as OTel metrics (satisfies
        // "rate-limit drops surfaced as a counter, not silent"). Uses the global
        // meter provider observability.rs installs when OTEL_* is present.
        register_metrics(&counters);

        // Drain task: pace (optional) then emit enriched OTLP records.
        let logger = provider.logger("wamn-logging");
        let drain_counters = counters.clone();
        let drain_rate = cfg.drain_rate_per_sec;
        let drain = tokio::spawn(async move {
            drain_loop(rx, logger, drain_counters, drain_rate, capture).await;
        });

        Ok(Self {
            claims: std::sync::RwLock::new(HashMap::new()),
            tx,
            counters,
            provider,
            _drain: drain,
        })
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Self::new(WamnLoggingConfig::from_env())
    }

    /// Register the host-trusted claim for a component id. The bench calls this
    /// directly; the host path feeds it from workload bind.
    pub fn set_claim(&self, component_id: &str, tenant: &str, project: &str) {
        self.claims.write().expect("claims lock poisoned").insert(
            component_id.to_string(),
            Claim {
                tenant: tenant.to_string(),
                project: project.to_string(),
            },
        );
    }

    /// Read back the registered `(tenant, project)` claim for a component id, or
    /// `None` when nothing was registered (the `claim_for` sentinel path). The
    /// run-worker wiring test asserts the host-injected identity landed here.
    pub fn claim_snapshot(&self, component_id: &str) -> Option<(String, String)> {
        self.claims
            .read()
            .expect("claims lock poisoned")
            .get(component_id)
            .map(|c| (c.tenant.clone(), c.project.clone()))
    }

    fn claim_for(&self, component_id: &str) -> Claim {
        self.claims
            .read()
            .expect("claims lock poisoned")
            .get(component_id)
            .cloned()
            .unwrap_or_else(|| Claim {
                // A guest that imports wasi:logging without a registered claim
                // still logs, but with a visible sentinel so the enrichment gate
                // catches the misconfiguration rather than hiding it.
                tenant: "unregistered".to_string(),
                project: "unregistered".to_string(),
            })
    }

    /// Enrich + enqueue. Non-blocking: `try_send`, counting drops on overflow.
    fn ingest(&self, component_id: &str, level: Level, context: &str, message: String) {
        let claim = self.claim_for(component_id);
        let ctx = ParsedContext::parse(context);
        let rec = Rec {
            severity: map_level(level),
            tenant: claim.tenant,
            project: claim.project,
            flow: ctx.flow,
            run: ctx.run,
            node: ctx.node,
            seq: ctx.seq,
            run_label: ctx.run_label,
            traceparent: ctx.traceparent,
            tracestate: ctx.tracestate,
            message,
        };
        match self.tx.try_send(rec) {
            Ok(()) => {
                self.counters.accepted.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                // Queue full (or closed): the rate limit engaged. Count it —
                // never block the guest, never drop silently.
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    // --- bench-facing accounting + lifecycle ---------------------------------

    pub fn accepted(&self) -> u64 {
        self.counters.accepted.load(Ordering::Relaxed)
    }
    pub fn dropped(&self) -> u64 {
        self.counters.dropped.load(Ordering::Relaxed)
    }
    pub fn emitted(&self) -> u64 {
        self.counters.emitted.load(Ordering::Relaxed)
    }

    /// Flush the downstream batch processor (call after the front queue has
    /// drained, i.e. `emitted == accepted`, so every accepted record exports).
    pub fn force_flush(&self) -> anyhow::Result<()> {
        self.provider
            .force_flush()
            .map_err(|e| anyhow::anyhow!("force_flush: {e}"))
    }
}

/// Link `wasi:logging/logging` into a hand-built store (the `logbench` harness).
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    logging::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

// ---------------------------------------------------------------------------
// Context parsing / level mapping
// ---------------------------------------------------------------------------

struct ParsedContext {
    flow: String,
    run: String,
    node: String,
    seq: Option<i64>,
    run_label: String,
    /// The run's W3C `traceparent` / `tracestate` when present (the run path
    /// attaches them so a guest log correlates with its run's trace). An absent
    /// or non-string field is simply `None`.
    traceparent: Option<String>,
    tracestate: Option<String>,
}

impl ParsedContext {
    /// Guest `context` is
    /// `{"flow":..,"run":..,"node":..,"seq":N,"run_label":..,"traceparent":..}`.
    fn parse(context: &str) -> Self {
        let v: serde_json::Value = serde_json::from_str(context).unwrap_or(serde_json::Value::Null);
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let opt = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        };
        Self {
            flow: s("flow"),
            run: s("run"),
            node: s("node"),
            seq: v.get("seq").and_then(|x| x.as_u64()).map(|n| n as i64),
            run_label: s("run_label"),
            traceparent: opt("traceparent"),
            tracestate: opt("tracestate"),
        }
    }
}

/// Parse a W3C `traceparent` (`<version>-<trace-id>-<span-id>-<flags>`) into the
/// OTel trace-context triple, or `None` for ANYTHING malformed (wrong field
/// count, non-hex, or an all-zero/invalid id). Total and panic-free: a garbage
/// context must never trap the drain task or a guest.
fn parse_w3c_traceparent(tp: &str) -> Option<(TraceId, SpanId, TraceFlags)> {
    let mut parts = tp.split('-');
    let _version = parts.next()?;
    let trace_hex = parts.next()?;
    let span_hex = parts.next()?;
    let flags_hex = parts.next()?;
    // Exactly four fields — a fifth means this is not a bare traceparent.
    if parts.next().is_some() {
        return None;
    }
    let trace_id = TraceId::from_hex(trace_hex).ok()?;
    let span_id = SpanId::from_hex(span_hex).ok()?;
    // A zero id is "invalid" per W3C — reject so we never set a bogus context.
    if trace_id == TraceId::INVALID || span_id == SpanId::INVALID {
        return None;
    }
    let flags = u8::from_str_radix(flags_hex, 16).ok()?;
    Some((trace_id, span_id, TraceFlags::new(flags)))
}

fn map_level(level: Level) -> Severity {
    match level {
        Level::Trace => Severity::Trace,
        Level::Debug => Severity::Debug,
        Level::Info => Severity::Info,
        Level::Warn => Severity::Warn,
        Level::Error => Severity::Error,
        Level::Critical => Severity::Fatal,
    }
}

// ---------------------------------------------------------------------------
// Drain task
// ---------------------------------------------------------------------------

async fn drain_loop(
    mut rx: mpsc::Receiver<Rec>,
    logger: opentelemetry_sdk::logs::SdkLogger,
    counters: Arc<Counters>,
    drain_rate_per_sec: u64,
    capture: Option<Arc<Capture>>,
) {
    // Optional pacing: at a positive rate, sleep the per-record interval after
    // each emit. Only used at the low rates of the saturation demo, where a
    // sleep-per-record is accurate and cheap.
    let interval = (drain_rate_per_sec > 0)
        .then(|| Duration::from_nanos(1_000_000_000 / drain_rate_per_sec.max(1)));

    while let Some(rec) = rx.recv().await {
        let mut lr = logger.create_log_record();
        lr.set_severity_number(rec.severity);
        lr.set_severity_text(rec.severity.name());
        // The run's W3C trace context, when the emitter attached one: a valid
        // traceparent sets trace_id/span_id/flags so the log joins its run's
        // trace; garbage/absent leaves the record untraced (never a trap). The
        // triple is `Copy`, so it costs nothing to keep for the capture below.
        let parsed = rec.traceparent.as_deref().and_then(parse_w3c_traceparent);
        if let Some((trace_id, span_id, flags)) = parsed {
            lr.set_trace_context(trace_id, span_id, Some(flags));
        }
        // `tracestate` is vendor trace state, meaningful only alongside a valid
        // `traceparent` (W3C) — carry it as an attribute when both are present.
        if let Some(ts) = rec.tracestate.filter(|_| parsed.is_some()) {
            lr.add_attribute("tracestate", ts);
        }
        // Mirror into the capture sink (OFF by default) BEFORE the enriched
        // fields move into the log record — so production (no capture) clones
        // nothing and pays only the branch.
        if let Some(cap) = &capture {
            cap.push(CapturedRec {
                severity: rec.severity,
                tenant: rec.tenant.clone(),
                project: rec.project.clone(),
                flow: rec.flow.clone(),
                run: rec.run.clone(),
                node: rec.node.clone(),
                seq: rec.seq,
                run_label: rec.run_label.clone(),
                trace_id: parsed.map(|(t, _, _)| format!("{t:032x}")),
                message: rec.message.clone(),
            });
        }
        lr.set_body(AnyValue::from(rec.message));
        lr.add_attribute("tenant", rec.tenant);
        lr.add_attribute("project", rec.project);
        lr.add_attribute("flow", rec.flow);
        lr.add_attribute("run", rec.run);
        lr.add_attribute("node", rec.node);
        lr.add_attribute("run_label", rec.run_label);
        if let Some(seq) = rec.seq {
            lr.add_attribute("seq", seq);
        }
        logger.emit(lr);
        counters.emitted.fetch_add(1, Ordering::Relaxed);

        if let Some(iv) = interval {
            tokio::time::sleep(iv).await;
        }
    }
}

/// (metric name, description, reader) triple for `register_metrics`.
type MetricSpec = (&'static str, &'static str, fn(&Counters) -> u64);

fn register_metrics(counters: &Arc<Counters>) {
    let meter = opentelemetry::global::meter("wamn-logging");
    let specs: [MetricSpec; 3] = [
        (
            "wamn.logging.dropped",
            "Log records dropped by the bounded front queue (rate-limit engaged)",
            |c| c.dropped.load(Ordering::Relaxed),
        ),
        (
            "wamn.logging.accepted",
            "Log records accepted onto the front queue",
            |c| c.accepted.load(Ordering::Relaxed),
        ),
        (
            "wamn.logging.emitted",
            "Log records emitted to the OTLP exporter",
            |c| c.emitted.load(Ordering::Relaxed),
        ),
    ];
    for (name, desc, read) in specs {
        let c = counters.clone();
        let _ = meter
            .u64_observable_counter(name)
            .with_description(desc)
            .with_callback(move |o| o.observe(read(&c), &[]))
            .build();
    }
}

// ---------------------------------------------------------------------------
// wasi:logging/logging Host impl (enrich + non-blocking enqueue)
// ---------------------------------------------------------------------------

impl logging::Host for ActiveCtx<'_> {
    async fn log(&mut self, level: Level, context: String, message: String) {
        let Ok(plugin) = self.try_get_plugin::<WamnLogging>(WAMN_LOGGING_ID) else {
            return; // logging is best-effort; never trap the guest
        };
        let component_id = self.component_id.to_string();
        plugin.ingest(&component_id, level, &context, message);
    }
}

// ---------------------------------------------------------------------------
// HostPlugin
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl HostPlugin for WamnLogging {
    fn id(&self) -> &'static str {
        WAMN_LOGGING_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wasi:logging/logging")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wasi", "logging", &[]) {
            return Ok(());
        }
        let cfg = &item.local_resources().config;
        let tenant = cfg.get(TENANT_CONFIG_KEY).cloned().unwrap_or_default();
        let project = cfg.get(PROJECT_CONFIG_KEY).cloned().unwrap_or_default();
        if tenant.is_empty() {
            tracing::warn!(
                component = item.id(),
                "component imports wasi:logging but sets no {TENANT_CONFIG_KEY}; logs enrich as 'unregistered'"
            );
        }
        self.set_claim(item.id(), &tenant, &project);
        logging::add_to_linker::<_, SharedCtx>(item.linker(), extract_active_ctx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid W3C traceparent we control (the W3C example); `01` = sampled.
    const VALID_TP: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
    const VALID_TRACE_ID: &str = "0af7651916cd43dd8448eb211c80319c";

    /// Drain until the plugin has emitted everything it accepted (a bounded wait
    /// so a wiring regression fails the test instead of hanging it).
    async fn wait_drained(plugin: &WamnLogging) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while plugin.emitted() < plugin.accepted() {
            assert!(std::time::Instant::now() < deadline, "drain did not settle");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[test]
    fn parse_w3c_traceparent_valid_absent_garbage() {
        // Valid -> the exact triple, panic-free.
        let (trace_id, span_id, flags) = parse_w3c_traceparent(VALID_TP).expect("valid parses");
        assert_eq!(format!("{trace_id}"), VALID_TRACE_ID);
        assert_eq!(format!("{span_id}"), "b7ad6b7169203331");
        assert_eq!(flags, TraceFlags::SAMPLED);
        // Absent / garbage / structurally-wrong -> None, never a panic.
        assert!(parse_w3c_traceparent("").is_none());
        assert!(parse_w3c_traceparent("garbage").is_none());
        assert!(parse_w3c_traceparent("00-nothex-b7ad6b7169203331-01").is_none());
        // A fifth field is not a bare traceparent.
        assert!(parse_w3c_traceparent(&format!("{VALID_TP}-extra")).is_none());
        // An all-zero (INVALID) trace id is rejected.
        assert!(
            parse_w3c_traceparent("00-00000000000000000000000000000000-b7ad6b7169203331-01")
                .is_none()
        );
    }

    /// Enrichment is HOST-injected: the registered claim's tenant/project win
    /// over any guest-supplied `tenant` in the context, while flow/run/node come
    /// from the (guest-legitimate) context. Kills a mutant that reads the tenant
    /// from the guest context (spoofable) or drops the claim enrichment.
    #[tokio::test]
    async fn ingest_enriches_from_host_claim_over_guest_context() {
        let (plugin, capture) =
            WamnLogging::new_with_capture(WamnLoggingConfig::default()).expect("plugin");
        plugin.set_claim("comp-1", "acme", "receiving");
        // The context carries a SPOOFED tenant the guest must not be able to set.
        let ctx = format!(
            r#"{{"flow":"receipt-flow","run":"run-9","node":"log-node","seq":7,"tenant":"evil-tenant","traceparent":"{VALID_TP}"}}"#
        );
        plugin.ingest("comp-1", Level::Info, &ctx, "hello".into());
        wait_drained(&plugin).await;

        let recs = capture.snapshot();
        assert_eq!(recs.len(), 1, "one emitted record");
        let r = &recs[0];
        assert_eq!(
            r.tenant, "acme",
            "tenant is the host claim, NOT the guest's"
        );
        assert_eq!(r.project, "receiving");
        assert_eq!(r.flow, "receipt-flow");
        assert_eq!(r.run, "run-9");
        assert_eq!(r.node, "log-node");
        assert_eq!(r.seq, Some(7));
        assert_eq!(r.trace_id.as_deref(), Some(VALID_TRACE_ID));
    }

    /// A valid traceparent attaches the run's trace_id to the record; an absent
    /// one leaves it untraced. Kills a mutant that breaks the W3C parse (always
    /// None) — the first record would then carry no trace_id.
    #[tokio::test]
    async fn traceparent_sets_trace_id_on_record() {
        let (plugin, capture) =
            WamnLogging::new_with_capture(WamnLoggingConfig::default()).expect("plugin");
        plugin.set_claim("comp-1", "acme", "receiving");
        let with_tp =
            format!(r#"{{"flow":"f","run":"r","node":"n","seq":0,"traceparent":"{VALID_TP}"}}"#);
        plugin.ingest("comp-1", Level::Info, &with_tp, "traced".into());
        // No traceparent key at all.
        let no_tp = r#"{"flow":"f","run":"r","node":"n","seq":1}"#;
        plugin.ingest("comp-1", Level::Info, no_tp, "untraced".into());
        wait_drained(&plugin).await;

        let recs = capture.snapshot();
        assert_eq!(recs.len(), 2);
        assert_eq!(
            recs[0].trace_id.as_deref(),
            Some(VALID_TRACE_ID),
            "a valid traceparent sets trace_id"
        );
        assert_eq!(
            recs[1].trace_id, None,
            "no traceparent leaves the record untraced"
        );
    }
}
