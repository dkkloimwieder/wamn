//! Guest flow-runner for the S3 spike (docs/p0-exit-criteria.md S3).
//!
//! The runner is a long-lived component that embeds the standard node library
//! as NATIVE Rust (`std_node`); dispatching a standard node is therefore an
//! ordinary same-binary function call — the `< 50us` overhead the dispatch
//! bench measures. Everything durable — the flow IR, run-state checkpoints, and
//! the business sink — goes through the host `wamn:postgres` capability under
//! the tenant claim the host injects; there is no other data path and the guest
//! never chooses its own tenant.
//!
//! The 5-node PoC graph is: webhook-in -> transform -> pg-write -> conditional
//! -> respond. Only pg-write has a side effect (one idempotent INSERT). A
//! checkpoint is written after each step; resume skips completed steps and
//! re-runs from the interrupted one, so a killed run leaves exactly one sink
//! row per step.

wit_bindgen::generate!({
    world: "flowrunner",
    path: "wit",
    generate_all,
});

use std::time::Instant;

use wamn::postgres::client::{self};
use wamn::postgres::types::{PgError, SqlValue};

struct Component;
export!(Component);

/// The single PoC flow. Two versions differ only in the transform op, so a
/// hot-reloaded version is observable in the run's output/return value.
const FLOW_ID: &str = "poc-receipt";
/// Tenant the host maps this component's identity to (see flowbench harness).
/// Rows are written with `tenant_id = current_setting('app.tenant', true)` so
/// the value never appears in guest code.
const MAX_VERSION: u32 = 2;

/// pg-write is the third node (index 2) of the PoC graph. It is the only step
/// with a side effect and the point the kill-window busy-loop guards.
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

/// The stored IR for a version. v1 upper-cases the payload, v2 reverses it —
/// distinct enough that the hot-reload gate can see which version ran.
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
}
impl WalkState {
    fn new(input: &str) -> Self {
        WalkState {
            value: input.to_string(),
            branch: false,
            output: String::new(),
        }
    }
}

/// Dispatch one standard node. This is the same-binary call the S3 dispatch
/// gate times. pg-write's *side effect* is not here (the caller performs the
/// real INSERT in `run`, or stubs it to a counter in the dispatch bench); this
/// keeps every standard node a pure in-component transform of `WalkState`.
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
        // Side effect handled by the caller; nothing to compute in-node.
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
// wamn:postgres helpers (all durable state flows through here)
// ---------------------------------------------------------------------------

/// Read the active flow version + its IR from the catalog. Returns the parsed
/// graph. This is the literal "load flow JSON from a catalog table".
fn load_active_graph() -> Result<Graph, String> {
    let rs = client::query(
        "SELECT graph_json::text FROM s3.flows WHERE active AND flow_id = $1",
        &[text(FLOW_ID)],
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
fn open_run(run_id: &str, flow_version: u32) -> Result<i32, String> {
    client::execute(
        "INSERT INTO s3.flow_runs (tenant_id, run_id, flow_id, flow_version, step_seq, status) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, -1, 'running') \
         ON CONFLICT (tenant_id, run_id) DO NOTHING",
        &[text(run_id), text(FLOW_ID), int32(flow_version as i32)],
    )
    .map_err(|e| err_name(&e))?;
    let rs = client::query(
        "SELECT step_seq FROM s3.flow_runs WHERE run_id = $1",
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
        "INSERT INTO s3.sink (tenant_id, run_id, step, payload) \
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
        "UPDATE s3.flow_runs SET step_seq = $2 WHERE run_id = $1",
        &[text(run_id), int32(step)],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

fn mark_completed(run_id: &str) -> Result<(), String> {
    client::execute(
        "UPDATE s3.flow_runs SET status = 'completed' WHERE run_id = $1",
        &[text(run_id)],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Walk the graph from the first uncompleted step. `kill_after_write` makes the
/// runner busy-loop right after the pg-write commits and before its checkpoint
/// — simulating a pod dying in the duplicate-risk window. Returns the version.
fn execute(run_id: &str, payload: &str, kill_after_write: bool) -> Result<u32, String> {
    let graph = load_active_graph()?;
    let done = open_run(run_id, graph.version)?;
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
            "INSERT INTO s3.flows (tenant_id, flow_id, version, active, graph_json) \
             VALUES (current_setting('app.tenant', true), $1, 1, true, $2) \
             ON CONFLICT (tenant_id, flow_id, version) \
             DO UPDATE SET graph_json = excluded.graph_json",
            &[text(FLOW_ID), text(graph_json(1))],
        )
        .map_err(|e| err_name(&e))?;
        client::execute(
            "INSERT INTO s3.flows (tenant_id, flow_id, version, active, graph_json) \
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
            "UPDATE s3.flows SET active = (version = $1) WHERE flow_id = $2",
            &[int32(version as i32), text(FLOW_ID)],
        )
        .map_err(|e| err_name(&e))?;
        Ok(())
    }

    fn active_version() -> Result<u32, String> {
        let rs = client::query(
            "SELECT version FROM s3.flows WHERE active AND flow_id = $1",
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
        let rs = client::query(
            "SELECT count(*) FROM s3.sink WHERE run_id = $1",
            &[text(&run_id)],
        )
        .map_err(|e| err_name(&e))?;
        match rs.rows.first().and_then(|r| r.first()) {
            Some(SqlValue::Int64(n)) => Ok(*n as u64),
            Some(SqlValue::Int32(n)) => Ok(*n as u64),
            _ => Err("unexpected count shape".into()),
        }
    }

    fn reset(run_id: String) -> Result<u64, String> {
        let a = client::execute("DELETE FROM s3.sink WHERE run_id = $1", &[text(&run_id)])
            .map_err(|e| err_name(&e))?;
        let b = client::execute("DELETE FROM s3.flow_runs WHERE run_id = $1", &[text(&run_id)])
            .map_err(|e| err_name(&e))?;
        Ok(a + b)
    }
}
