//! Guest flow-runner for the S3 spike (docs/p0-exit-criteria.md S3), extended
//! for the S6 test-host plugin-swap spike (docs/p0-exit-criteria.md S6).
//!
//! The runner is a long-lived component that embeds the standard node library
//! as NATIVE Rust (`std_node`); dispatching a standard node is therefore an
//! ordinary same-binary function call — the `< 50us` overhead the dispatch
//! bench measures. Everything durable — the flow IR, run-state checkpoints, and
//! the business sink — goes through the host `wamn:postgres` capability; there
//! is no other data path and the guest never chooses its own tenant.
//!
//! Table names are UNQUALIFIED and resolve through the host-injected
//! `search_path`: the runner never hard-codes its schema. The prod host points
//! it at the shared fixture schema; the test host points it at a fresh
//! per-run ephemeral schema. The schema is a host-swapped fixture, exactly like
//! the tenant claim — one more thing the test host substitutes with zero guest
//! changes (S6 / design-note 9).
//!
//! The S3 PoC graph is: webhook-in -> transform -> pg-write -> conditional ->
//! respond. The S6 graph is: webhook-in -> delay -> http-call -> pg-write ->
//! respond. The `delay` node reads wall-clock time and parks (durable
//! parked-wake); the `http-call` node makes a `wasi:http` outbound request.
//! Both touch host capabilities the test host virtualizes/interposes — but the
//! SAME compiled binary runs under both hosts.

wit_bindgen::generate!({
    world: "flowrunner",
    path: "wit",
    generate_all,
});

use std::time::Instant;

use wamn::postgres::client::{self};
use wamn::postgres::types::{PgError, SqlValue};

use wasi::clocks::wall_clock;
use wasi::http::outgoing_handler;
use wasi::http::types::{Fields, Method, OutgoingRequest, Scheme};

struct Component;
export!(Component);

/// The S3 PoC flow. Two versions differ only in the transform op, so a
/// hot-reloaded version is observable in the run's output/return value.
const FLOW_ID: &str = "poc-receipt";
/// The S6 delay+http flow.
const FLOW_ID_S6: &str = "poc-s6";
const MAX_VERSION: u32 = 2;

/// pg-write is the third node (index 2) of the S3 PoC graph. It is the only
/// step with a side effect and the point the kill-window busy-loop guards.
const PG_WRITE_STEP: i32 = 2;

// ---------------------------------------------------------------------------
// SqlValue helpers + error naming
// ---------------------------------------------------------------------------

fn text(s: impl Into<String>) -> SqlValue {
    SqlValue::Text(s.into())
}
fn int32(v: i32) -> SqlValue {
    SqlValue::Int32(v)
}

/// Name a pg-error by its variant (no host detail beyond the taxonomy tag), so
/// the harness can assert on the error kind. Mirrors pgprobe's `err_name`.
fn err_name(e: &PgError) -> String {
    match e {
        PgError::SerializationFailure => "serialization-failure".into(),
        PgError::ConnectionUnavailable => "connection-unavailable".into(),
        PgError::StatementTimeout => "statement-timeout".into(),
        PgError::RowLimitExceeded(n) => format!("row-limit-exceeded:{n}"),
        PgError::UniqueViolation(c) => format!("unique-violation:{c}"),
        PgError::ForeignKeyViolation(c) => format!("foreign-key-violation:{c}"),
        PgError::CheckViolation(c) => format!("check-violation:{c}"),
        PgError::PermissionDenied => "permission-denied".into(),
        PgError::QueryError((code, msg)) => format!("query-error:{code}:{msg}"),
    }
}

// ---------------------------------------------------------------------------
// Flow IR (minimal ad-hoc schema; the canonical schema is wamn-34t / 5.1)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct Graph {
    version: u32,
    nodes: Vec<Node>,
}

#[derive(serde::Deserialize)]
struct Node {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    config: serde_json::Value,
}

/// The stored IR for an S3 version. v1 upper-cases the payload, v2 reverses it
/// — distinct enough that the hot-reload gate can see which version ran.
fn graph_json(version: u32) -> String {
    let op = if version == 1 { "upper" } else { "reverse" };
    format!(
        r#"{{"version":{version},
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"t","type":"transform","config":{{"op":"{op}"}}}},
              {{"id":"w","type":"pg-write"}},
              {{"id":"c","type":"conditional","config":{{"min-len":3}}}},
              {{"id":"out","type":"respond"}}
            ],
            "edges":[["in","t"],["t","w"],["w","c"],["c","out"]]}}"#
    )
}

/// The S6 IR: webhook-in -> delay(delay-secs) -> http-call(url) -> pg-write ->
/// respond. `delay` parks until wall-clock reaches now()+delay-secs; `http-call`
/// fetches `url`. JSON is hand-built so the config values embed verbatim.
fn graph_json_s6(delay_secs: u64, http_url: &str) -> String {
    // http_url is a controlled harness value (a loopback URL); escape the two
    // JSON-significant characters defensively anyway.
    let url = http_url.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"{{"version":1,
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"d","type":"delay","config":{{"delay-secs":{delay_secs}}}}},
              {{"id":"h","type":"http-call","config":{{"url":"{url}"}}}},
              {{"id":"w","type":"pg-write"}},
              {{"id":"out","type":"respond"}}
            ],
            "edges":[["in","d"],["d","h"],["h","w"],["w","out"]]}}"#
    )
}

/// Fixed in-memory graph for the dispatch bench (no catalog round trip — the
/// bench measures pure dispatch, so it must not touch the DB).
fn bench_graph() -> Graph {
    serde_json::from_str(&graph_json(MAX_VERSION)).expect("bench graph parses")
}

// ---------------------------------------------------------------------------
// Standard-node dispatch (native, same-binary)
// ---------------------------------------------------------------------------

/// Per-walk mutable state threaded between node handlers.
struct WalkState {
    value: String,
    branch: bool,
    output: String,
    /// HTTP status the http-call node observed (0 = not reached / egress
    /// refused). S6 only.
    http_status: u32,
}
impl WalkState {
    fn new(input: &str) -> Self {
        WalkState {
            value: input.to_string(),
            branch: false,
            output: String::new(),
            http_status: 0,
        }
    }
}

/// Dispatch one standard, side-effect-free node. This is the same-binary call
/// the S3 dispatch gate times. Side effects (pg-write, delay, http-call) are
/// handled by the executor, not here — every standard node stays a pure
/// in-component transform of `WalkState`.
fn std_node(kind: &str, config: &serde_json::Value, st: &mut WalkState) {
    match kind {
        // Input already sits in st.value (webhook payload as walk input).
        "webhook-in" => {}
        "transform" => {
            let op = config.get("op").and_then(|v| v.as_str()).unwrap_or("upper");
            st.value = match op {
                "reverse" => st.value.chars().rev().collect(),
                _ => st.value.to_uppercase(),
            };
        }
        // Side effect handled by the executor; nothing to compute in-node.
        "pg-write" => {}
        "conditional" => {
            let min = config.get("min-len").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
            st.branch = st.value.chars().count() >= min;
        }
        "respond" => {
            st.output = format!("ok:{}:{}", st.branch, st.value);
        }
        _ => {}
    }
}

fn percentile_ns(sorted: &[u32], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx] as u64
}

// ---------------------------------------------------------------------------
// wamn:postgres helpers (all durable state flows through here). Table names
// are UNQUALIFIED — the host injects the schema via search_path.
// ---------------------------------------------------------------------------

/// Read the active flow version + its IR from the catalog for `flow_id`.
fn load_active_graph(flow_id: &str) -> Result<Graph, String> {
    let rs = client::query(
        "SELECT graph_json::text FROM flows WHERE active AND flow_id = $1",
        &[text(flow_id)],
    )
    .map_err(|e| err_name(&e))?;
    let row = rs.rows.first().ok_or("no active flow version")?;
    let raw = match row.first() {
        Some(SqlValue::Text(s)) => s.clone(),
        Some(SqlValue::Json(s)) => s.clone(),
        other => return Err(format!("unexpected graph_json shape: {other:?}")),
    };
    serde_json::from_str(&raw).map_err(|e| format!("graph parse: {e}"))
}

/// Upsert the run row (fresh runs start at step_seq -1) and return the highest
/// completed step. Idempotent: a resumed run keeps its recorded progress.
fn open_run(run_id: &str, flow_id: &str, flow_version: u32) -> Result<i32, String> {
    client::execute(
        "INSERT INTO flow_runs (tenant_id, run_id, flow_id, flow_version, step_seq, status) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, -1, 'running') \
         ON CONFLICT (tenant_id, run_id) DO NOTHING",
        &[text(run_id), text(flow_id), int32(flow_version as i32)],
    )
    .map_err(|e| err_name(&e))?;
    let rs = client::query(
        "SELECT step_seq FROM flow_runs WHERE run_id = $1",
        &[text(run_id)],
    )
    .map_err(|e| err_name(&e))?;
    match rs.rows.first().and_then(|r| r.first()) {
        Some(SqlValue::Int32(n)) => Ok(*n),
        Some(SqlValue::Int64(n)) => Ok(*n as i32),
        other => Err(format!("unexpected step_seq shape: {other:?}")),
    }
}

/// The pg-write side effect: exactly-once per (run, step) by the idempotency
/// key. On replay after a resume this is a no-op (ON CONFLICT DO NOTHING).
fn pg_write(run_id: &str, step: i32, payload: &str) -> Result<(), String> {
    client::execute(
        "INSERT INTO sink (tenant_id, run_id, step, payload) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3) \
         ON CONFLICT (tenant_id, run_id, step) DO NOTHING",
        &[text(run_id), int32(step), text(payload)],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Checkpoint: record that `step` completed. Written after the step's effect
/// commits, so a crash between the effect and here re-runs the step on resume.
fn checkpoint(run_id: &str, step: i32) -> Result<(), String> {
    client::execute(
        "UPDATE flow_runs SET step_seq = $2 WHERE run_id = $1",
        &[text(run_id), int32(step)],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

fn mark_completed(run_id: &str) -> Result<(), String> {
    client::execute(
        "UPDATE flow_runs SET status = 'completed' WHERE run_id = $1",
        &[text(run_id)],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// S3 executor
// ---------------------------------------------------------------------------

/// Walk the S3 graph from the first uncompleted step. `kill_after_write` makes
/// the runner busy-loop right after the pg-write commits and before its
/// checkpoint — simulating a pod dying in the duplicate-risk window. Returns
/// the version.
fn execute(run_id: &str, payload: &str, kill_after_write: bool) -> Result<u32, String> {
    let graph = load_active_graph(FLOW_ID)?;
    let done = open_run(run_id, FLOW_ID, graph.version)?;
    let mut st = WalkState::new(payload);

    for (i, node) in graph.nodes.iter().enumerate() {
        let step = i as i32;
        // Re-derive in-memory state for skipped steps so the resumed run sees
        // the same value the original run computed (transform/conditional are
        // pure, so replaying their compute is free and side-effect-free).
        std_node(&node.kind, &node.config, &mut st);
        if step <= done {
            continue; // already committed on an earlier attempt
        }
        if node.kind == "pg-write" {
            pg_write(run_id, step, &st.value)?;
            if kill_after_write && step == PG_WRITE_STEP {
                // Side effect is durably committed; progress is NOT yet
                // recorded. Spin until the host epoch-kills this store. On the
                // next attempt, `done` is still < PG_WRITE_STEP, so pg-write
                // replays and ON CONFLICT DO NOTHING absorbs the duplicate.
                let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
                loop {
                    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
                    core::hint::black_box(x);
                }
            }
        }
        checkpoint(run_id, step)?;
    }
    mark_completed(run_id)?;
    Ok(graph.version)
}

// ---------------------------------------------------------------------------
// S6: wall-clock (delay / parked-wake) + wasi:http (http-call / egress)
// ---------------------------------------------------------------------------

/// Current wall-clock time in whole seconds since the epoch. Time enters the
/// flow ONLY here (design-note 9), so the test host virtualizes it.
fn wall_now_secs() -> u64 {
    wall_clock::now().seconds
}

/// Load the parked-wake deadline (epoch seconds) recorded in the run's
/// `state_json`, if any.
fn load_wake(run_id: &str) -> Result<Option<u64>, String> {
    let rs = client::query(
        "SELECT state_json::text FROM flow_runs WHERE run_id = $1",
        &[text(run_id)],
    )
    .map_err(|e| err_name(&e))?;
    let raw = match rs.rows.first().and_then(|r| r.first()) {
        Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => s.clone(),
        _ => return Ok(None), // NULL / absent
    };
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("state_json parse: {e}"))?;
    Ok(v.get("wake").and_then(|w| w.as_u64()))
}

/// Persist the parked-wake deadline for the run.
fn save_wake(run_id: &str, wake_secs: u64) -> Result<(), String> {
    client::execute(
        "UPDATE flow_runs SET state_json = $2 WHERE run_id = $1",
        &[text(run_id), text(format!(r#"{{"wake":{wake_secs}}}"#))],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Split an `http://authority/path?query` URL into (scheme, authority, path).
/// Only plain HTTP is used (the loopback egress target); anything else yields
/// None so the caller reports a 0 status.
fn parse_http_url(url: &str) -> Option<(Scheme, String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), "/".to_string()),
    };
    if authority.is_empty() {
        return None;
    }
    Some((Scheme::Http, authority, path))
}

/// Make one outbound GET via `wasi:http/outgoing-handler` and return the
/// response status (0 if the request could not be built, the host refused the
/// egress, or no response arrived). Egress leaves the flow ONLY here, so the
/// test host's egress spy sees and can stub/deny every call.
fn http_get(url: &str) -> u32 {
    let Some((scheme, authority, path)) = parse_http_url(url) else {
        return 0;
    };
    let req = OutgoingRequest::new(Fields::new());
    if req.set_method(&Method::Get).is_err()
        || req.set_scheme(Some(&scheme)).is_err()
        || req.set_authority(Some(&authority)).is_err()
        || req.set_path_with_query(Some(&path)).is_err()
    {
        return 0;
    }
    let fut = match outgoing_handler::handle(req, None) {
        Ok(f) => f,
        Err(_) => return 0, // host refused before dispatch
    };
    // Block until the response future is ready.
    let pollable = fut.subscribe();
    pollable.block();
    match fut.get() {
        // Some(Ok(Ok(resp))): a response arrived. Some(Ok(Err(_))): the host
        // returned an error-code (e.g. egress denied by the spy). None: not
        // ready (shouldn't happen after block()).
        Some(Ok(Ok(resp))) => resp.status() as u32,
        _ => 0,
    }
}

/// Walk the S6 graph from the first uncompleted step. The delay node parks
/// (returns `(1, _)`) until wall-clock reaches its deadline; every other node
/// runs to completion. Returns (outcome, http-status): outcome 0 = completed,
/// 1 = parked-waiting.
fn execute_s6(run_id: &str, payload: &str) -> Result<(u32, u32), String> {
    let graph = load_active_graph(FLOW_ID_S6)?;
    let done = open_run(run_id, FLOW_ID_S6, graph.version)?;
    let mut st = WalkState::new(payload);

    for (i, node) in graph.nodes.iter().enumerate() {
        let step = i as i32;
        match node.kind.as_str() {
            "delay" => {
                if step <= done {
                    continue; // deadline already satisfied on a prior call
                }
                let delay_secs = node
                    .config
                    .get("delay-secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let now = wall_now_secs();
                let wake = match load_wake(run_id)? {
                    Some(w) => w,
                    None => {
                        // First reach: record the deadline and park. step_seq is
                        // NOT advanced, so a resume re-enters this node.
                        let w = now.saturating_add(delay_secs);
                        save_wake(run_id, w)?;
                        w
                    }
                };
                if now < wake {
                    return Ok((1, st.http_status)); // parked
                }
                // Deadline reached: the delay is complete.
                checkpoint(run_id, step)?;
            }
            "http-call" => {
                if step <= done {
                    continue;
                }
                let url = node.config.get("url").and_then(|v| v.as_str()).unwrap_or("");
                st.http_status = http_get(url);
                checkpoint(run_id, step)?;
            }
            "pg-write" => {
                std_node(&node.kind, &node.config, &mut st);
                if step <= done {
                    continue;
                }
                pg_write(run_id, step, &st.value)?;
                checkpoint(run_id, step)?;
            }
            _ => {
                std_node(&node.kind, &node.config, &mut st);
                if step <= done {
                    continue;
                }
                checkpoint(run_id, step)?;
            }
        }
    }
    mark_completed(run_id)?;
    Ok((0, st.http_status))
}

// ---------------------------------------------------------------------------
// Guest exports
// ---------------------------------------------------------------------------

impl Guest for Component {
    fn dispatch_bench(iterations: u32) -> (u64, u64, u64, u64, u64) {
        let graph = bench_graph();
        let nodes = &graph.nodes;
        let per_walk = nodes.len();
        let iters = iterations.max(1) as usize;

        // Warm up (page in, settle the branch predictor) before measuring.
        for _ in 0..1000 {
            let mut st = WalkState::new("warmup-payload");
            for n in nodes {
                std_node(&n.kind, &n.config, &mut st);
            }
            core::hint::black_box(&st.output);
        }

        // Un-instrumented pass: one clock read for the whole batch, so the mean
        // is the amortized per-dispatch cost with no per-sample clock overhead.
        let t_bare = Instant::now();
        let mut sink: u64 = 0;
        for _ in 0..iters {
            let mut st = WalkState::new("dispatch-probe-payload");
            for n in nodes {
                std_node(&n.kind, &n.config, &mut st);
                if n.kind == "pg-write" {
                    sink += 1; // stubbed side effect
                }
            }
            core::hint::black_box(&st.output);
        }
        let bare_ns = t_bare.elapsed().as_nanos() as u64;
        core::hint::black_box(sink);
        let total = (iters * per_walk) as u64;
        let mean = bare_ns / total.max(1);

        // Instrumented pass: time each per-node dispatch. Each sample includes
        // one Instant::elapsed() monotonic-clock read, so it OVER-reports the
        // true dispatch cost — the p99 is a conservative upper bound.
        let mut samples: Vec<u32> = Vec::with_capacity(iters * per_walk);
        for _ in 0..iters {
            let mut st = WalkState::new("dispatch-probe-payload");
            for n in nodes {
                let t0 = Instant::now();
                std_node(&n.kind, &n.config, &mut st);
                let dt = t0.elapsed().as_nanos();
                samples.push(dt.min(u32::MAX as u128) as u32);
            }
            core::hint::black_box(&st.output);
        }
        samples.sort_unstable();
        let count = samples.len() as u64;
        let p50 = percentile_ns(&samples, 0.50);
        let p99 = percentile_ns(&samples, 0.99);
        let max = samples.last().copied().unwrap_or(0) as u64;
        (count, mean, p50, p99, max)
    }

    fn seed() -> Result<u32, String> {
        // v1 active by default; re-seed refreshes graph_json but not `active`.
        client::execute(
            "INSERT INTO flows (tenant_id, flow_id, version, active, graph_json) \
             VALUES (current_setting('app.tenant', true), $1, 1, true, $2) \
             ON CONFLICT (tenant_id, flow_id, version) \
             DO UPDATE SET graph_json = excluded.graph_json",
            &[text(FLOW_ID), text(graph_json(1))],
        )
        .map_err(|e| err_name(&e))?;
        client::execute(
            "INSERT INTO flows (tenant_id, flow_id, version, active, graph_json) \
             VALUES (current_setting('app.tenant', true), $1, 2, false, $2) \
             ON CONFLICT (tenant_id, flow_id, version) \
             DO UPDATE SET graph_json = excluded.graph_json",
            &[text(FLOW_ID), text(graph_json(2))],
        )
        .map_err(|e| err_name(&e))?;
        Ok(MAX_VERSION)
    }

    fn set_active(version: u32) -> Result<(), String> {
        // Exactly one active version per flow: set active = (version = $1).
        client::execute(
            "UPDATE flows SET active = (version = $1) WHERE flow_id = $2",
            &[int32(version as i32), text(FLOW_ID)],
        )
        .map_err(|e| err_name(&e))?;
        Ok(())
    }

    fn active_version() -> Result<u32, String> {
        let rs = client::query(
            "SELECT version FROM flows WHERE active AND flow_id = $1",
            &[text(FLOW_ID)],
        )
        .map_err(|e| err_name(&e))?;
        match rs.rows.first().and_then(|r| r.first()) {
            Some(SqlValue::Int32(n)) => Ok(*n as u32),
            Some(SqlValue::Int64(n)) => Ok(*n as u32),
            _ => Err("no active flow version".into()),
        }
    }

    fn run(run_id: String, payload: String) -> Result<u32, String> {
        execute(&run_id, &payload, false)
    }

    fn run_until_kill(run_id: String, payload: String) -> Result<u32, String> {
        execute(&run_id, &payload, true)
    }

    fn sink_count(run_id: String) -> Result<u64, String> {
        let rs = client::query("SELECT count(*) FROM sink WHERE run_id = $1", &[text(&run_id)])
            .map_err(|e| err_name(&e))?;
        match rs.rows.first().and_then(|r| r.first()) {
            Some(SqlValue::Int64(n)) => Ok(*n as u64),
            Some(SqlValue::Int32(n)) => Ok(*n as u64),
            _ => Err("unexpected count shape".into()),
        }
    }

    fn reset(run_id: String) -> Result<u64, String> {
        let a = client::execute("DELETE FROM sink WHERE run_id = $1", &[text(&run_id)])
            .map_err(|e| err_name(&e))?;
        let b = client::execute("DELETE FROM flow_runs WHERE run_id = $1", &[text(&run_id)])
            .map_err(|e| err_name(&e))?;
        Ok(a + b)
    }

    fn seed_s6(delay_secs: u64, http_url: String) -> Result<u32, String> {
        client::execute(
            "INSERT INTO flows (tenant_id, flow_id, version, active, graph_json) \
             VALUES (current_setting('app.tenant', true), $1, 1, true, $2) \
             ON CONFLICT (tenant_id, flow_id, version) \
             DO UPDATE SET graph_json = excluded.graph_json, active = true",
            &[text(FLOW_ID_S6), text(graph_json_s6(delay_secs, &http_url))],
        )
        .map_err(|e| err_name(&e))?;
        Ok(1)
    }

    fn run_s6(run_id: String, payload: String) -> Result<(u32, u32), String> {
        execute_s6(&run_id, &payload)
    }
}
