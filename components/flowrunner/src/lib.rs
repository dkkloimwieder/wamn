//! Production flow-runner (5.2), grown from the S3 spike and the S6 test-host
//! spike. The runner is a long-lived component that embeds the standard node
//! library as NATIVE Rust — since 5.3 the `wamn-nodes` vocabulary, dispatched
//! through the SDK capability facade under the policy table
//! (docs/node-library.md), beside the S3/S6 fixture node shapes — and walks
//! the flow graph with the pure `wamn-runner` engine (5.2): the ported-edge
//! walk, branch/merge, error routing, and retry/backoff live in the crate;
//! this component supplies the effects — dispatching each node, the
//! `wamn:postgres` checkpoints, the reload doorbell.
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
//! ## Checkpoint / resume (5.7)
//! Durable run state is the `runs` / `node_runs` tables (`deploy/run-state.sql`):
//! a `runs` row per execution and a `node_runs` row per completed node. On every
//! invocation the runner **reconstructs** the in-memory `RunState` by replaying
//! the persisted `node_runs` through the pure engine (`wamn-run-store`) — the
//! branch-aware durable resume that supersedes the S3 linear `step_seq`. A node
//! with a persisted record is never re-dispatched (its effect does not repeat);
//! a node with none is outstanding and re-runs, so an effect that committed in
//! the crash window between its DB write and its `node_runs` row replays
//! at-least-once and is absorbed by the node's own idempotency (`pg-write`'s
//! `sink` `ON CONFLICT DO NOTHING`). `delay` parks by recording a wake deadline
//! in `runs.state_json` without writing a `node_runs` row, so it re-enters on the
//! next invocation — the durable parked-wake the S6 24-hour test exercises under
//! virtual time. A resumed run finishes down exactly the branch it took.

wit_bindgen::generate!({
    world: "flowrunner",
    path: "wit",
    generate_all,
});

use std::time::Instant;

use serde_json::{Value, json};
use wamn_flow::Flow;
use wamn_node_sdk as sdk;
use wamn_run_store::{NodeRunRecord, RunRecord, sql as run_sql};
use wamn_runner::{
    Dispatch, ERROR_PORT, NodeError, NodeOutcome, Plan, RetryPolicy, RunStatus, Step,
};

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

// ---------------------------------------------------------------------------
// SqlValue helpers + error naming
// ---------------------------------------------------------------------------

fn text(s: impl Into<String>) -> SqlValue {
    SqlValue::Text(s.into())
}
fn int32(v: i32) -> SqlValue {
    SqlValue::Int32(v)
}
/// Encode a payload `Value` for a `jsonb` column (trigger input / node I/O).
/// Sent as a text param the server parses into jsonb — the same path the S3
/// `state_json` write used — so the engine's `serde_json::Value` round-trips.
fn jsonb(v: &Value) -> SqlValue {
    SqlValue::Text(v.to_string())
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

/// The node's index in the flow — the stable `step` key for `pg-write`'s `sink`
/// idempotency (stable per flow version). Run-state checkpointing is now per-node
/// into `node_runs`; this remains the business-effect idempotency key.
fn node_index(flow: &Flow, node_id: &str) -> i32 {
    flow.nodes
        .iter()
        .position(|n| n.id == node_id)
        .map(|i| i as i32)
        .unwrap_or(-1)
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

/// Open (or re-open) the run row: a fresh run records its trigger input and
/// `running` status; a resumed run is a no-op (ON CONFLICT DO NOTHING) — its
/// node_runs history is the durable progress.
fn open_run(run_id: &str, flow_id: &str, flow_version: u32, input: &Value) -> Result<(), String> {
    client::execute(
        &run_sql::insert_run_sql(),
        &[
            text(run_id),
            text(flow_id),
            int32(flow_version as i32),
            text(wamn_run_store::RunStatus::Running.as_sql()),
            SqlValue::Null, // trigger_source: a direct driver, not a dispatcher
            jsonb(input),
        ],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Load a run's already-completed node executions in dispatch (`seq`) order — the
/// branch-aware reconstruction source. Only `success`/`error` rows are completed
/// steps; a `parked`/`running` row is an outstanding node the walk re-dispatches.
/// An error-routed node was recorded as an emission on the `error` port, so
/// reconstruction needs no error taxonomy here.
fn load_completed(run_id: &str) -> Result<Vec<NodeRunRecord>, String> {
    let rs = client::query(&run_sql::select_completed_node_runs_sql(), &[text(run_id)])
        .map_err(|e| err_name(&e))?;
    let mut out = Vec::with_capacity(rs.rows.len());
    for row in &rs.rows {
        let node_id = match row.first() {
            Some(SqlValue::Text(s)) => s.clone(),
            other => return Err(format!("node_runs.node_id shape: {other:?}")),
        };
        let seq = match row.get(1) {
            Some(SqlValue::Int32(n)) => *n as u32,
            Some(SqlValue::Int64(n)) => *n as u32,
            other => return Err(format!("node_runs.seq shape: {other:?}")),
        };
        let port = match row.get(2) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => "main".to_string(),
        };
        let output = match row.get(3) {
            Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => {
                serde_json::from_str(s).map_err(|e| format!("node_runs.output_json parse: {e}"))?
            }
            _ => Value::Null,
        };
        out.push(NodeRunRecord::success(run_id, node_id, seq, port, output));
    }
    Ok(out)
}

/// The pg-write side effect: exactly-once per (run, step) by the sink idempotency
/// key. `step` is the node's stable index. On an at-least-once replay this is a
/// no-op (ON CONFLICT DO NOTHING).
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

/// Record a completed node execution — the durable per-node checkpoint, written
/// after the node's effect commits. Idempotent by (run_id, node_id, occurrence).
/// v1 writes `occurrence = 0`: exact for the acyclic fixture flows; a flow that
/// revisits a node would compute occurrence from its prior visits (the schema +
/// `wamn-run-store` reconstruction already accommodate that — see docs/run-state.md).
fn record_node_run(
    run_id: &str,
    node_id: &str,
    seq: i32,
    port: &str,
    output: &Value,
    input: &Value,
) -> Result<(), String> {
    client::execute(
        &run_sql::insert_node_run_success_sql(),
        &[
            text(run_id),
            text(node_id),
            int32(seq),
            text(port),
            jsonb(output),
            jsonb(input),
        ],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Mark the run completed and record its result payload.
fn mark_completed(run_id: &str, result: &Value) -> Result<(), String> {
    client::execute(
        &run_sql::update_run_completed_sql(),
        &[text(run_id), jsonb(result)],
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
    let rs = client::query(&run_sql::select_run_state_sql(), &[text(run_id)])
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
        &run_sql::update_run_state_sql(),
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
// Standard node library glue (5.3): the wamn-nodes vocabulary dispatches
// through the SHARED capability facade `wamn_node_guest::caps::CapsCtx`
// (SR2) over this component's real imports — the WIT<->SDK mirrors and the
// full outbound-HTTP path live there, not here. Egress still leaves the flow
// ONLY via wasi:http, so the S6 egress spy interposes unchanged.
// ---------------------------------------------------------------------------

/// Whether the engine will ROUTE this error emission down the node's error
/// edge (vs scheduling a retry, or cancelling the run) — the exact policy
/// computation `Plan::apply` makes, mirrored so a recorded error row always
/// matches the walk the engine actually took. Terminal / invalid-input route
/// immediately; retryable / rate-limited route only once the retry budget is
/// spent; a cancellation never fires error branches.
fn will_error_route(err: &NodeError, d: &Dispatch) -> bool {
    match err {
        NodeError::Terminal(_) | NodeError::InvalidInput(_) => true,
        NodeError::Retryable(_) | NodeError::RateLimited(_) => {
            !RetryPolicy::from_config(&d.config).may_retry(d.attempt)
        }
        NodeError::Cancelled => false,
    }
}

/// Record an error-ROUTED node as an emission on the `error` port carrying
/// the same `{"error": {...}}` payload the engine routes — exactly what 5.7
/// reconstruction replays (poc-webhook-f1's shape verbatim); the taxonomy
/// lands in `error_kind`/`error_detail` for the run history.
fn record_error(
    run_id: &str,
    node_id: &str,
    seq: i32,
    err: &NodeError,
    input: &Value,
) -> Result<(), String> {
    let (kind, detail) = match err {
        NodeError::Retryable(d) => ("retryable", Some(d)),
        NodeError::RateLimited(r) => ("rate-limited", Some(&r.detail)),
        NodeError::Terminal(d) => ("terminal", Some(d)),
        NodeError::InvalidInput(d) => ("invalid-input", Some(d)),
        NodeError::Cancelled => ("cancelled", None),
    };
    let payload = detail
        .map(|d| d.to_error_payload())
        .unwrap_or_else(|| json!({ "error": {} }));
    let detail_json = match detail {
        Some(d) => json!({ "message": d.message, "code": d.code, "data": d.data }),
        None => Value::Null,
    };
    client::execute(
        &run_sql::insert_node_run_error_sql(),
        &[
            text(run_id),
            text(node_id),
            int32(seq),
            jsonb(&payload),
            jsonb(input),
            text(kind),
            jsonb(&detail_json),
        ],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Record the run's failure verdict (audit parity with poc-webhook-f1).
fn mark_failed(run_id: &str, kind: &str, node: &str, reason: &str) -> Result<(), String> {
    client::execute(
        &run_sql::update_run_failed_sql(),
        &[text(run_id), text(kind), text(node), text(reason)],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

fn fail_kind_sql(kind: &wamn_runner::FailKind) -> &'static str {
    match kind {
        wamn_runner::FailKind::Terminal => "terminal",
        wamn_runner::FailKind::RetryExhausted => "retry-exhausted",
        wamn_runner::FailKind::InvalidInput => "invalid-input",
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

/// Dispatch one node — the native standard-node library. A node reached here is
/// OUTSTANDING (reconstruction never re-dispatches a node that already has a
/// `node_runs` row), so effectful nodes run their effect unconditionally,
/// deduped by their own idempotency: `pg-write`'s `sink` `ON CONFLICT DO NOTHING`
/// absorbs the at-least-once replay of a node killed after its write but before
/// its `node_runs` row. `kill_after_write` spins right after `pg-write` commits
/// (before that row is written) — the crash window the resume gate exercises.
fn dispatch_node(
    d: &Dispatch,
    run_id: &str,
    flow: &Flow,
    kill_after_write: bool,
    http_status: &mut u32,
) -> Result<NodeAction, String> {
    match d.node_type.as_str() {
        // The trigger payload already sits in the node's input.
        "webhook-in" => Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone()))),
        // An `expression` config routes to the standard library's JMESPath
        // transform/conditional below; the S3 fixture shapes (`op`/`min-len`)
        // keep their legacy semantics byte-identical.
        "transform" if d.config.get("expression").is_none() => {
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
        // true branching is exercised in the wamn-runner / wamn-run-store tests.
        "conditional" if d.config.get("expression").is_none() => {
            Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone())))
        }
        // Passthrough terminal — identical to the standard library's respond
        // (this driver has no HTTP response to answer; poc-webhook-f1 does).
        "respond" => Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone()))),
        "pg-write" => {
            pg_write(run_id, node_index(flow, &d.node), value_str(&d.payload))?;
            if kill_after_write {
                // Side effect committed; the node_runs row NOT yet written. Spin
                // until the host epoch-kills this store; on resume the node is
                // outstanding, the write replays, and ON CONFLICT absorbs it.
                let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
                loop {
                    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
                    core::hint::black_box(x);
                }
            }
            Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone())))
        }
        "delay" => {
            let delay_secs = d
                .config
                .get("delay-secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let now = wall_now_secs();
            // First reach records the deadline in the run's state_json and parks
            // WITHOUT writing a node_runs row, so a resume re-enters this node;
            // later reaches compare against it.
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
            let url = d.config.get("url").and_then(|v| v.as_str()).unwrap_or("");
            *http_status = http_get(url);
            Ok(NodeAction::Emit(NodeOutcome::ok(d.payload.clone())))
        }
        // The standard node library (5.3): everything the library ships
        // dispatches through the capability policy table over this
        // component's real imports. A NodeError feeds the engine, which
        // decides retry-vs-error-path-vs-fail mechanically from the variant.
        t if wamn_nodes::node(t).is_some() => {
            let run_ctx = sdk::RunContext {
                run_id,
                flow_id: &flow.flow_id,
                flow_version: flow.version,
                node_id: &d.node,
                attempt: d.attempt,
                idempotency_key: &d.idempotency_key,
                deadline_ms: d.deadline_ms,
                // 9.2: this guest is host-invoked (exported `run`), not served
                // over `wasi:http`, so it has no inbound request to read a
                // `traceparent` from. Its OUTBOUND calls are still traced — the
                // host stamps the active trace context onto every outbound
                // `wasi:http`/`wamn:postgres` call (host-enforced inject), and
                // the standard http-request node forwards `run.traceparent`
                // once a source exists. Surfacing a per-run traceparent to a
                // host-invoked guest needs the queue/dispatch path to carry it
                // (follow-up); until then this stays `None`.
                traceparent: None,
                tracestate: None,
                config: &d.config,
            };
            let mut ctx = wamn_node_guest::caps::CapsCtx::default();
            let granted = wamn_nodes::granted_for(sdk::NodeCtx::raw_sql_enabled(&ctx));
            Ok(NodeAction::Emit(
                match wamn_nodes::dispatch(t, granted, &mut ctx, &run_ctx, &d.payload) {
                    Ok(em) => NodeOutcome::ok_on(em.payload, em.port),
                    Err(e) => NodeOutcome::Error(e),
                },
            ))
        }
        other => Err(format!("unknown node type: {other}")),
    }
}

/// Walk the active flow via the engine, resuming branch-aware from the persisted
/// `node_runs` (5.7). `kill_after_write` makes the runner busy-loop right after
/// `pg-write` commits and before its `node_runs` row is written (the pod-death
/// window). Returns the version, the outcome (0 = completed, 1 = parked), and the
/// last observed HTTP status.
fn execute(
    run_id: &str,
    payload: &str,
    kill_after_write: bool,
    flow_id: &str,
) -> Result<RunOutcome, String> {
    // v1 reconstructs against the ACTIVE flow version (safe while a flow's
    // versions stay structurally compatible — `Plan::resume` raises Mismatch if
    // not); pinning a resume to the run's persisted `flow_version` is a follow-up
    // (docs/run-state.md).
    let flow = load_active_flow(flow_id)?;
    let plan = Plan::compile(&flow).map_err(|e| e.to_string())?;
    let version = plan.version();
    let input = Value::String(payload.to_string());
    open_run(run_id, flow_id, version, &input)?;

    // Reconstruct the frontier from what already completed (empty on a fresh run
    // => a plain start); the driver continues from there, re-dispatching only
    // outstanding nodes. `seq` continues past the completed count.
    let completed = load_completed(run_id)?;
    let mut next_seq = completed.len() as i32;
    let run_rec = RunRecord::new(run_id, flow_id, version, input);
    let mut st =
        wamn_run_store::reconstruct(&plan, &run_rec, &completed).map_err(|e| e.to_string())?;
    let mut http_status: u32 = 0;

    loop {
        // now_ms = 0: the fixture flows carry no retry backoff, so the engine
        // never returns Wait; `delay` parks via NodeAction::Park instead.
        match plan.next(&mut st, 0) {
            Step::Done(RunStatus::Completed) => {
                mark_completed(run_id, st.result())?;
                return Ok(RunOutcome {
                    version,
                    outcome: 0,
                    http_status,
                });
            }
            Step::Done(status) => {
                // Audit parity with poc-webhook-f1: the failure verdict lands in
                // runs.fail_* before the driver reports the error.
                if let Some(f) = st.failure() {
                    let _ = mark_failed(run_id, fail_kind_sql(&f.kind), &f.node, &f.detail.message);
                }
                return Err(format!("run ended in {status:?}"));
            }
            // Cross-invocation retry scheduling belongs to the queue layer
            // (run_queue.available_at / park_sql — the fqg.4 guest-claim
            // rewire); this per-invocation driver treats a scheduled retry
            // wait defensively, like poc-webhook-f1's sync path.
            Step::Wait { node, .. } => return Err(format!("unexpected retry wait at {node}")),
            Step::Dispatch(d) => {
                match dispatch_node(&d, run_id, &flow, kill_after_write, &mut http_status)? {
                    NodeAction::Emit(outcome) => {
                        match &outcome {
                            // Record the completed node (after its effect
                            // commits) so a later invocation reconstructs past it.
                            NodeOutcome::Success { payload, port } => {
                                record_node_run(
                                    run_id, &d.node, next_seq, port, payload, &d.payload,
                                )?;
                                next_seq += 1;
                            }
                            // Record an error row ONLY when the engine will
                            // ROUTE the emission (an error edge exists AND no
                            // retry follows): 5.7 reconstruction folds every
                            // recorded row as a routed emission, so a row for
                            // a retried or edge-less failure would resume the
                            // run down a path the live walk never took.
                            NodeOutcome::Error(err)
                                if will_error_route(err, &d)
                                    && !plan.successors(&d.node, ERROR_PORT).is_empty() =>
                            {
                                record_error(run_id, &d.node, next_seq, err, &d.payload)?;
                                next_seq += 1;
                            }
                            NodeOutcome::Error(_) => {}
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
    fn dispatch_bench(iterations: u32, flow_json: String) -> Result<(u64, Vec<u32>), String> {
        let flow = Flow::from_json(&flow_json).map_err(|e| format!("bench flow: {e}"))?;
        let plan = Plan::compile(&flow).map_err(|e| e.to_string())?;
        let iters = iterations.max(1) as usize;

        // Warm up (page in, settle the branch predictor) before measuring.
        for _ in 0..1000 {
            bench_walk(&plan, |d, o, st| plan.apply(st, d, o, 0));
        }

        // Un-instrumented pass: one clock read for the whole batch — the
        // harness derives the amortized per-dispatch mean from the total.
        let t_bare = Instant::now();
        for _ in 0..iters {
            bench_walk(&plan, |d, o, st| plan.apply(st, d, o, 0));
        }
        let bare_ns = t_bare.elapsed().as_nanos() as u64;

        // Instrumented pass: time each per-node dispatch (node compute + the
        // engine's route/advance). Each sample includes one monotonic-clock
        // read, so it OVER-reports the true dispatch cost. Raw samples go
        // back to the harness; percentiles are computed host-side
        // (wamn-gate-harness — the guest carries no stats code, SR2).
        let mut samples: Vec<u32> = Vec::with_capacity(iters * flow.nodes.len());
        for _ in 0..iters {
            bench_walk(&plan, |d, o, st| {
                let t0 = Instant::now();
                plan.apply(st, d, o, 0);
                let dt = t0.elapsed().as_nanos();
                samples.push(dt.min(u32::MAX as u128) as u32);
            });
        }
        Ok((bare_ns, samples))
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
        // Deleting the run cascades its node_runs (FK ON DELETE CASCADE).
        let b = client::execute("DELETE FROM runs WHERE run_id = $1", &[text(&run_id)])
            .map_err(|e| err_name(&e))?;
        Ok(a + b)
    }

    fn run_s6(run_id: String, payload: String) -> Result<(u32, u32), String> {
        execute(&run_id, &payload, false, FLOW_ID_S6).map(|r| (r.outcome, r.http_status))
    }
}
