//! Production flow-runner (5.2), grown from the S3 spike and the S6 test-host
//! spike. The runner is a long-lived component that embeds the standard node
//! library as NATIVE Rust and walks the flow graph with the pure `wamn-runner`
//! engine (5.2): the ported-edge walk, branch/merge, error routing, and
//! retry/backoff live in the crate; this component supplies the effects —
//! dispatching each node, the `wamn:postgres` checkpoints, the reload doorbell.
//!
//! Flows are the canonical `wamn-flow` schema (5.1), read from the catalog; the
//! ad-hoc S3 JSON is gone. Everything durable — the flow definition, run-state
//! checkpoints, the business sink — goes through the host `wamn:postgres`
//! capability under a host-injected tenant claim; there is no other data path and
//! the guest never chooses its own tenant.
//!
//! Table names are UNQUALIFIED and resolve through the host-injected
//! `search_path`: the prod host points the runner at the shared fixture schema,
//! the test host at a fresh per-run ephemeral schema — a host-swapped fixture,
//! exactly like the tenant claim (S6 / design-note 9).
//!
//! The S3 flow is `webhook-in -> transform -> pg-write -> conditional ->
//! respond`; the S6 flow is `webhook-in -> delay -> http-call -> pg-write ->
//! respond`. `delay` reads wall-clock time and parks (durable parked-wake);
//! `http-call` makes a `wasi:http` outbound request. Both touch host capabilities
//! the test host virtualizes/interposes — the SAME compiled binary runs under
//! both hosts.
//!
//! ## Checkpoint / resume
//! The engine re-walks from `entry` on every invocation; the DB `step_seq` (the
//! node's index in the flow) is the checkpoint. An effectful node (`pg-write`,
//! `http-call`) whose index is `<= step_seq` skips its effect on replay; `pg-write`
//! is additionally idempotent by `(run_id, step)`, so a crash in the window
//! between its commit and its checkpoint replays cleanly (exactly-once effect).
//! Branch-aware durable resume (persisting the frontier) is 5.7; the linear
//! fixture flows resume exactly on `step_seq`.

wit_bindgen::generate!({
    world: "flowrunner",
    path: "wit",
    generate_all,
});

use std::time::Instant;

use serde_json::Value;
use wamn_flow::Flow;
use wamn_runner::{Dispatch, NodeOutcome, Plan, RunStatus, Step};

use wamn::postgres::client::{self};
use wamn::postgres::types::{PgError, SqlValue};

use wasi::clocks::wall_clock;
use wasi::http::outgoing_handler;
use wasi::http::types::{Fields, Method, OutgoingRequest, Scheme};

struct Component;
export!(Component);

/// The S3 PoC flow. Two versions differ only in the transform op, so a
/// hot-reloaded version is observable in the run's return value.
const FLOW_ID: &str = "poc-receipt";
/// The S6 delay+http flow.
const FLOW_ID_S6: &str = "poc-s6";
const MAX_VERSION: u32 = 2;

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
// Flow definitions (canonical wamn-flow / 5.1 schema)
// ---------------------------------------------------------------------------

/// The stored S3 flow for a version. v1 upper-cases the payload, v2 reverses it —
/// distinct enough that the hot-reload gate can see which version ran.
fn flow_json(version: u32) -> String {
    let op = if version == 1 { "upper" } else { "reverse" };
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{FLOW_ID}","version":{version},
            "trigger":{{"type":"webhook","sync":true}},"entry":"in",
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"t","type":"transform","config":{{"op":"{op}"}}}},
              {{"id":"w","type":"pg-write"}},
              {{"id":"c","type":"conditional","config":{{"min-len":3}}}},
              {{"id":"out","type":"respond"}}
            ],
            "edges":[{{"from":"in","to":"t"}},{{"from":"t","to":"w"}},
                     {{"from":"w","to":"c"}},{{"from":"c","to":"out"}}]}}"#
    )
}

/// The S6 flow: `webhook-in -> delay(delay-secs) -> http-call(url) -> pg-write ->
/// respond`. JSON is hand-built so the config values embed verbatim.
fn flow_json_s6(delay_secs: u64, http_url: &str) -> String {
    // http_url is a controlled harness value (a loopback URL); escape the two
    // JSON-significant characters defensively anyway.
    let url = http_url.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{FLOW_ID_S6}","version":1,
            "trigger":{{"type":"webhook"}},"entry":"in",
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"d","type":"delay","config":{{"delay-secs":{delay_secs}}}}},
              {{"id":"h","type":"http-call","config":{{"url":"{url}"}}}},
              {{"id":"w","type":"pg-write"}},
              {{"id":"out","type":"respond"}}
            ],
            "edges":[{{"from":"in","to":"d"}},{{"from":"d","to":"h"}},
                     {{"from":"h","to":"w"}},{{"from":"w","to":"out"}}]}}"#
    )
}

/// The node's index in the flow — the durable `step` key for checkpoints and
/// `pg-write` idempotency (stable per flow version).
fn node_index(flow: &Flow, node_id: &str) -> i32 {
    flow.nodes
        .iter()
        .position(|n| n.id == node_id)
        .map(|i| i as i32)
        .unwrap_or(-1)
}

fn percentile_ns(sorted: &[u32], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx] as u64
}

/// The current string value carried by a payload (`webhook-in` puts the trigger
/// payload string here; `transform` rewrites it).
fn value_str(payload: &Value) -> &str {
    payload.as_str().unwrap_or("")
}

// ---------------------------------------------------------------------------
// wamn:postgres helpers (all durable state flows through here). Table names
// are UNQUALIFIED — the host injects the schema via search_path.
// ---------------------------------------------------------------------------

/// Read the active flow version + its definition from the catalog for `flow_id`.
fn load_active_flow(flow_id: &str) -> Result<Flow, String> {
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
    Flow::from_json(&raw).map_err(|e| format!("flow parse: {e}"))
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
    let pollable = fut.subscribe();
    pollable.block();
    match fut.get() {
        Some(Ok(Ok(resp))) => resp.status() as u32,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Executor: drive the wamn-runner engine over the loaded flow
// ---------------------------------------------------------------------------

/// A dispatched node's result: emit an outcome to advance the walk, or park the
/// whole run (the `delay` node before its deadline).
enum NodeAction {
    Emit(NodeOutcome),
    Park,
}

/// The outcome of one `execute` call. `outcome`: 0 = completed, 1 = parked.
struct RunOutcome {
    version: u32,
    outcome: u32,
    http_status: u32,
}

/// Dispatch one node — the native standard-node library. Pure nodes recompute
/// (free, idempotent); effectful nodes (`pg-write`, `http-call`) skip their
/// effect on replay (`step <= done`). `pg-write` in `kill_after_write` mode spins
/// after committing, before the caller checkpoints — the crash window the resume
/// gate exercises.
fn dispatch_node(
    d: &Dispatch,
    step: i32,
    done: i32,
    run_id: &str,
    kill_after_write: bool,
    http_status: &mut u32,
) -> Result<NodeAction, String> {
    match d.node_type.as_str() {
        // The trigger payload already sits in the node's input.
        "webhook-in" => Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone()))),
        "transform" => {
            let op = d
                .config
                .get("op")
                .and_then(|v| v.as_str())
                .unwrap_or("upper");
            let out = match op {
                "reverse" => value_str(&d.payload).chars().rev().collect::<String>(),
                _ => value_str(&d.payload).to_uppercase(),
            };
            Ok(NodeAction::Emit(NodeOutcome::ok(Value::String(out))))
        }
        // Records a branch decision but keeps the fixture's linear main path;
        // true branching is exercised in the wamn-runner engine tests.
        "conditional" | "respond" => Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone()))),
        "pg-write" => {
            if step > done {
                pg_write(run_id, step, value_str(&d.payload))?;
                if kill_after_write {
                    // Side effect committed; progress NOT yet checkpointed. Spin
                    // until the host epoch-kills this store; on resume the write
                    // replays and ON CONFLICT DO NOTHING absorbs the duplicate.
                    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
                    loop {
                        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
                        core::hint::black_box(x);
                    }
                }
            }
            Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone())))
        }
        "delay" => {
            if step <= done {
                return Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone())));
            }
            let delay_secs = d
                .config
                .get("delay-secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let now = wall_now_secs();
            // First reach records the deadline and parks WITHOUT checkpointing, so
            // a resume re-enters this node; later reaches compare against it.
            let wake = match load_wake(run_id)? {
                Some(w) => w,
                None => {
                    let w = now.saturating_add(delay_secs);
                    save_wake(run_id, w)?;
                    w
                }
            };
            if now < wake {
                Ok(NodeAction::Park)
            } else {
                Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone())))
            }
        }
        "http-call" => {
            if step > done {
                let url = d.config.get("url").and_then(|v| v.as_str()).unwrap_or("");
                *http_status = http_get(url);
            }
            Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone())))
        }
        other => Err(format!("unknown node type: {other}")),
    }
}

/// Walk the active flow from the first uncompleted step via the engine.
/// `kill_after_write` makes the runner busy-loop right after `pg-write` commits
/// and before its checkpoint (the pod-death window). Returns the version, the
/// outcome (0 = completed, 1 = parked), and the last observed HTTP status.
fn execute(
    run_id: &str,
    payload: &str,
    kill_after_write: bool,
    flow_id: &str,
) -> Result<RunOutcome, String> {
    let flow = load_active_flow(flow_id)?;
    let plan = Plan::compile(&flow).map_err(|e| e.to_string())?;
    let version = plan.version();
    let done = open_run(run_id, flow_id, version)?;
    let mut st = plan.start(run_id, Value::String(payload.to_string()));
    let mut http_status: u32 = 0;

    loop {
        // now_ms = 0: the fixture flows carry no retry backoff, so the engine
        // never returns Wait; `delay` parks via NodeAction::Park instead.
        match plan.next(&mut st, 0) {
            Step::Done(RunStatus::Completed) => {
                mark_completed(run_id)?;
                return Ok(RunOutcome {
                    version,
                    outcome: 0,
                    http_status,
                });
            }
            Step::Done(status) => return Err(format!("run ended in {status:?}")),
            Step::Wait { node, .. } => return Err(format!("unexpected retry wait at {node}")),
            Step::Dispatch(d) => {
                let step = node_index(&flow, &d.node);
                match dispatch_node(&d, step, done, run_id, kill_after_write, &mut http_status)? {
                    NodeAction::Emit(outcome) => {
                        // Record newly-completed progress after the effect commits.
                        if step > done {
                            checkpoint(run_id, step)?;
                        }
                        plan.apply(&mut st, &d, outcome, 0);
                    }
                    NodeAction::Park => {
                        return Ok(RunOutcome {
                            version,
                            outcome: 1,
                            http_status,
                        });
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch bench: same-binary node dispatch overhead, no DB
// ---------------------------------------------------------------------------

/// Pure node dispatch for the bench — the standard-node compute with no DB
/// (`pg-write` is a stubbed passthrough). This is the same-binary call the
/// dispatch gate times.
fn bench_node(d: &Dispatch) -> NodeOutcome {
    match d.node_type.as_str() {
        "transform" => {
            let op = d
                .config
                .get("op")
                .and_then(|v| v.as_str())
                .unwrap_or("upper");
            let out = match op {
                "reverse" => value_str(&d.payload).chars().rev().collect::<String>(),
                _ => value_str(&d.payload).to_uppercase(),
            };
            NodeOutcome::ok(Value::String(out))
        }
        _ => NodeOutcome::ok(d.payload.clone()),
    }
}

/// Drive one bench walk through the engine with the pure dispatcher, invoking
/// `on_step` for each node dispatch so the caller can time it.
fn bench_walk(
    plan: &Plan,
    mut on_step: impl FnMut(&Dispatch, NodeOutcome, &mut wamn_runner::RunState),
) {
    let mut st = plan.start("bench", Value::String("dispatch-probe-payload".into()));
    while let Step::Dispatch(d) = plan.next(&mut st, 0) {
        let outcome = bench_node(&d);
        on_step(&d, outcome, &mut st);
    }
}

// ---------------------------------------------------------------------------
// Guest exports
// ---------------------------------------------------------------------------

impl Guest for Component {
    fn dispatch_bench(iterations: u32) -> (u64, u64, u64, u64, u64) {
        let flow = Flow::from_json(&flow_json(MAX_VERSION)).expect("bench flow parses");
        let plan = Plan::compile(&flow).expect("bench flow compiles");
        let per_walk = flow.nodes.len();
        let iters = iterations.max(1) as usize;

        // Warm up (page in, settle the branch predictor) before measuring.
        for _ in 0..1000 {
            bench_walk(&plan, |d, o, st| plan.apply(st, d, o, 0));
        }

        // Un-instrumented pass: one clock read for the whole batch, so the mean
        // is the amortized per-dispatch cost with no per-sample clock overhead.
        let t_bare = Instant::now();
        for _ in 0..iters {
            bench_walk(&plan, |d, o, st| plan.apply(st, d, o, 0));
        }
        let bare_ns = t_bare.elapsed().as_nanos() as u64;
        let total = (iters * per_walk) as u64;
        let mean = bare_ns / total.max(1);

        // Instrumented pass: time each per-node dispatch (node compute + the
        // engine's route/advance). Each sample includes one monotonic-clock read,
        // so it OVER-reports the true dispatch cost — the p99 is a conservative
        // upper bound.
        let mut samples: Vec<u32> = Vec::with_capacity(iters * per_walk);
        for _ in 0..iters {
            bench_walk(&plan, |d, o, st| {
                let t0 = Instant::now();
                plan.apply(st, d, o, 0);
                let dt = t0.elapsed().as_nanos();
                samples.push(dt.min(u32::MAX as u128) as u32);
            });
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
            &[text(FLOW_ID), text(flow_json(1))],
        )
        .map_err(|e| err_name(&e))?;
        client::execute(
            "INSERT INTO flows (tenant_id, flow_id, version, active, graph_json) \
             VALUES (current_setting('app.tenant', true), $1, 2, false, $2) \
             ON CONFLICT (tenant_id, flow_id, version) \
             DO UPDATE SET graph_json = excluded.graph_json",
            &[text(FLOW_ID), text(flow_json(2))],
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
        execute(&run_id, &payload, false, FLOW_ID).map(|r| r.version)
    }

    fn run_until_kill(run_id: String, payload: String) -> Result<u32, String> {
        execute(&run_id, &payload, true, FLOW_ID).map(|r| r.version)
    }

    fn sink_count(run_id: String) -> Result<u64, String> {
        let rs = client::query(
            "SELECT count(*) FROM sink WHERE run_id = $1",
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
            &[text(FLOW_ID_S6), text(flow_json_s6(delay_secs, &http_url))],
        )
        .map_err(|e| err_name(&e))?;
        Ok(1)
    }

    fn run_s6(run_id: String, payload: String) -> Result<(u32, u32), String> {
        execute(&run_id, &payload, false, FLOW_ID_S6).map(|r| (r.outcome, r.http_status))
    }
}
