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
//! Durable run state is the `runs` / `node_runs` tables (`deploy/sql/run-state.sql`):
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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use serde_json::{Value, json};
use wamn_flow::Flow;
use wamn_node_sdk as sdk;
use wamn_run_store::{NodeRunRecord, RunRecord, sql as run_sql};
// The durable-queue claim-path builders (5.14). The guest deps wamn-run-queue
// with default-features off, so only these pure `sql.rs` builders link — the
// cron/dispatch pair (croner/chrono) never enters the wasm (fqg.4).
// The combined claim/checkpoint/complete statements are the fqg.18 record-stream
// amortization: one statement where the split path spent two or three.
use wamn_run_queue::{
    acquire_partitions_sql, claim_dispatch_sql, claim_partition_head_sql, complete_dequeue_sql,
    dequeue_sql, mark_running_sql, park_sql, record_error_and_renew_sql,
    record_success_and_renew_sql, release_partition_sql, renew_partition_sql,
};
use wamn_runner::{
    Dispatch, ERROR_PORT, ErrorDetail, NodeError, NodeOutcome, Plan, RateLimitDetail, RetryPolicy,
    RunStatus, Step,
};

use wamn_node_invoke::{
    NodeInvokeRequest, NodeInvokeResponse, SIGNATURE_HEADER, SIGNING_KEY_CREDENTIAL,
    TIMESTAMP_HEADER, WireErrorDetail, WireNodeError, WirePayload, WireRunContext,
    granted_credentials, sign_envelope_with_timestamp,
};

use wamn::postgres::client::{self};
use wamn::postgres::types::{PgError, SqlValue};

use wasi::clocks::wall_clock;
use wasi::http::outgoing_handler;
use wasi::http::types::{
    ErrorCode, Fields, IncomingResponse, Method, OutgoingBody, OutgoingRequest, Scheme,
};
use wasi::io::streams::StreamError;

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
fn int64(v: i64) -> SqlValue {
    SqlValue::Int64(v)
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

/// Persist the in-flight retry cursor — the retrying node + the attempt the next
/// dispatch runs as — in the run's `state_json`, so the retry budget survives
/// park→reclaim→reconstruct (R32): the outstanding node re-enters carrying its
/// attempt instead of resetting to 0 (reconstruction replays only COMPLETED
/// node_runs, so a mid-retry node otherwise loses its count). Home-shares
/// `state_json` with the delay node's `wake`; the engine has one `current` node,
/// so at most one park's state is live at a time, and both readers re-validate
/// against the reconstructed frontier (`restore_retry` no-ops off the front;
/// `load_wake` only fires while its node is outstanding).
fn save_retry(run_id: &str, node: &str, attempt: u32) -> Result<(), String> {
    client::execute(
        &run_sql::update_run_state_sql(),
        &[
            text(run_id),
            text(json!({ "retry": { "node": node, "attempt": attempt } }).to_string()),
        ],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Load a persisted in-flight retry cursor `(node, attempt)` from `state_json`,
/// if any — the reconstruction seam feeds it to [`Plan::restore_retry`].
fn load_retry(run_id: &str) -> Result<Option<(String, u32)>, String> {
    let rs = client::query(&run_sql::select_run_state_sql(), &[text(run_id)])
        .map_err(|e| err_name(&e))?;
    let raw = match rs.rows.first().and_then(|r| r.first()) {
        Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => s.clone(),
        _ => return Ok(None),
    };
    let v: Value = serde_json::from_str(&raw).map_err(|e| format!("state_json parse: {e}"))?;
    let Some(retry) = v.get("retry") else {
        return Ok(None);
    };
    match (
        retry.get("node").and_then(|n| n.as_str()),
        retry.get("attempt").and_then(|a| a.as_u64()),
    ) {
        (Some(node), Some(attempt)) => Ok(Some((node.to_string(), attempt as u32))),
        _ => Ok(None),
    }
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
// 5.6 / wamn-bd5: custom-node invocation (the in-cluster HTTP hop)
// ---------------------------------------------------------------------------

/// Dispatch a `custom` node: the v0 runner->node HTTP hop. Reads the node's
/// Service endpoint from the node step's config (registry-recorded, no
/// EndpointSlice controller in v0), derives this step's credential grant, POSTs
/// the invocation envelope, and folds the reply into a [`NodeOutcome`]. A
/// transport / envelope failure becomes a classified [`NodeError`] so the engine
/// decides retry-vs-error-vs-fail mechanically, exactly as for a standard node.
fn custom_node_dispatch(d: &Dispatch, run_id: &str, flow: &Flow) -> Result<NodeAction, String> {
    // Endpoint discovery (v0): the node step's config carries the in-cluster
    // Service URL. A missing endpoint is a flow authoring error — terminal,
    // routed like any other node failure (never a runner panic).
    let Some(endpoint) = d.config.get("endpoint").and_then(Value::as_str) else {
        return Ok(NodeAction::Emit(NodeOutcome::Error(NodeError::Terminal(
            ErrorDetail::coded(
                "custom-node-misconfigured",
                "custom node step is missing config.endpoint (the serve-node Service URL)",
            ),
        ))));
    };
    let url = if endpoint.ends_with("/run") {
        endpoint.to_string()
    } else {
        format!("{}/run", endpoint.trim_end_matches('/'))
    };

    let req = NodeInvokeRequest {
        ctx: WireRunContext {
            run_id: run_id.to_string(),
            flow_id: flow.flow_id.clone(),
            flow_version: flow.version,
            node_id: d.node.clone(),
            attempt: d.attempt,
            idempotency_key: d.idempotency_key.clone(),
            deadline_ms: d.deadline_ms,
            // 9.2: a host-invoked runner has no inbound request to read a
            // traceparent from; outbound calls are host-stamped. Stays None
            // until the queue/dispatch path carries a per-run trace context.
            traceparent: None,
            tracestate: None,
            config: d.config.to_string(),
        },
        input: WirePayload::Inline(d.payload.to_string()),
        // cjv.3: EXACTLY this node step's declared credential(s) — the shared
        // pure helper, so the grant cannot silently widen to the whole project.
        grant: granted_credentials(d.credential.as_deref()),
    };

    // wamn-fqg.22: sign the exact request body with the per-project-env HMAC key
    // BEFORE the hop, so the serve-node verifies before installing the grant. The
    // key reaches this GUEST the SAME way every per-project credential does — the
    // vault via `wamn:node/credentials.get` (no new WIT) — under the reserved
    // name the runner grants itself (`declare_run_grant`). A deployment with NO
    // key configured resolves nothing here and sends unsigned (legacy
    // network-trust, accepted only by a keyless serve-node). The key is NEVER
    // logged, echoed, or placed in the envelope grant.
    let body = req.to_json();
    // wamn-fqg.32: when we hold a key, stamp a freshness timestamp (unix seconds)
    // that is COVERED BY the signature and rides the `x-wamn-timestamp` header.
    // The serve-node enforces max-age only when configured (OFF by default), but
    // always sending it means a freshness-enabled env needs no flowrunner change;
    // a keyless deployment signs (and stamps) nothing. The timestamp folds into
    // the signed bytes additively (version-safe in wamn-node-invoke).
    let signed = read_signing_key().map(|key| {
        let ts = wall_now_secs().to_string();
        let sig = sign_envelope_with_timestamp(key.as_bytes(), body.as_bytes(), Some(&ts));
        (ts, sig)
    });
    let body = match http_post_run(
        &url,
        &body,
        signed.as_ref().map(|(_, sig)| sig.as_str()),
        signed.as_ref().map(|(ts, _)| ts.as_str()),
    ) {
        Ok(b) => b,
        Err(e) => return Ok(NodeAction::Emit(NodeOutcome::Error(e))),
    };
    let resp = NodeInvokeResponse::from_json(&body)
        .map_err(|e| format!("custom node returned an undecodable response: {e}"))?;
    let outcome = match resp {
        NodeInvokeResponse::Ok(em) => {
            let payload = match em.payload.inline() {
                Some(s) => serde_json::from_str(s)
                    .map_err(|e| format!("custom node output payload is not JSON: {e}"))?,
                None => Value::Null,
            };
            match em.port {
                Some(p) => NodeOutcome::ok_on(payload, p),
                None => NodeOutcome::ok(payload),
            }
        }
        NodeInvokeResponse::Err(we) => NodeOutcome::Error(wire_error_to_runner(we)),
    };
    Ok(NodeAction::Emit(outcome))
}

/// The frozen `node-error` taxonomy off the wire -> the engine's error type,
/// variant for variant (the engine routes/ retries/fails off the variant).
fn wire_error_to_runner(e: WireNodeError) -> NodeError {
    match e {
        WireNodeError::Retryable(d) => NodeError::Retryable(wire_detail(d)),
        WireNodeError::RateLimited(r) => NodeError::RateLimited(RateLimitDetail {
            detail: wire_detail(r.detail),
            retry_after_ms: r.retry_after_ms,
            target_host: r.target_host,
        }),
        WireNodeError::Terminal(d) => NodeError::Terminal(wire_detail(d)),
        WireNodeError::InvalidInput(d) => NodeError::InvalidInput(wire_detail(d)),
        WireNodeError::Cancelled => NodeError::Cancelled,
    }
}

fn wire_detail(d: WireErrorDetail) -> ErrorDetail {
    ErrorDetail {
        message: d.message,
        code: d.code,
        data: d.data.and_then(|s| serde_json::from_str(&s).ok()),
    }
}

/// A retryable transport failure of the runner->node hop.
fn node_transport(msg: impl Into<String>) -> NodeError {
    NodeError::Retryable(ErrorDetail::coded("NODE_TRANSPORT", msg))
}

/// A `wasi:http` refusal on the runner->node hop -> a classified node error: a
/// host egress DENIAL is terminal (the node Service host is not allowlisted — a
/// misconfiguration, not a transient), anything else is a retryable transport
/// failure.
fn hop_egress_error(code: ErrorCode) -> NodeError {
    match code {
        ErrorCode::HttpRequestDenied => NodeError::Terminal(ErrorDetail::coded(
            "egress-denied",
            "runner->node hop denied by the egress allowlist (node Service host not allowed)",
        )),
        other => node_transport(format!("runner->node hop failed: {other:?}")),
    }
}

/// Read the per-project-env HMAC signing key from the vault (wamn-fqg.22),
/// scoped to the runner's host-injected project — the SAME channel every
/// per-project credential reaches this guest through
/// (`wamn:node/credentials.get`), so no new WIT. `None` when no key is
/// configured (a keyless deployment signs nothing — legacy network-trust) or the
/// reserved name is not granted/resolvable. The secret is handed back for
/// signing ONLY and is never logged or echoed.
fn read_signing_key() -> Option<String> {
    wamn::node::credentials::get(SIGNING_KEY_CREDENTIAL).ok()
}

/// POST `body` to the custom node's `serve-node` endpoint and return the
/// response body. Egress leaves the flow ONLY here (wasi:http), so the host
/// egress guard + the flow's `allowed-hosts` govern the hop. `signature`, when
/// present, is the hex HMAC over `body` (+ `timestamp`, wamn-fqg.32) carried in
/// the `x-wamn-signature` header (wamn-fqg.22) so the serve-node verifies before
/// installing the grant; `timestamp` (unix seconds) rides `x-wamn-timestamp` and
/// is bound by that signature.
fn http_post_run(
    url: &str,
    body: &str,
    signature: Option<&str>,
    timestamp: Option<&str>,
) -> Result<String, NodeError> {
    let Some((scheme, authority, path)) = parse_http_url(url) else {
        return Err(NodeError::Terminal(ErrorDetail::coded(
            "bad-endpoint",
            format!("unparseable custom-node endpoint {url:?}"),
        )));
    };
    let headers = Fields::new();
    if let Some(sig) = signature
        && headers.append(SIGNATURE_HEADER, sig.as_bytes()).is_err()
    {
        return Err(node_transport("runner->node signature header rejected"));
    }
    // wamn-fqg.32: the freshness timestamp header (bound by the signature above).
    if let Some(ts) = timestamp
        && headers.append(TIMESTAMP_HEADER, ts.as_bytes()).is_err()
    {
        return Err(node_transport("runner->node timestamp header rejected"));
    }
    let req = OutgoingRequest::new(headers);
    if req.set_method(&Method::Post).is_err()
        || req.set_scheme(Some(&scheme)).is_err()
        || req.set_authority(Some(&authority)).is_err()
        || req.set_path_with_query(Some(&path)).is_err()
    {
        return Err(node_transport("runner->node request fields rejected"));
    }
    let out_body = req
        .body()
        .map_err(|_| node_transport("runner->node request body unavailable"))?;
    {
        let stream = out_body
            .write()
            .map_err(|_| node_transport("runner->node body stream unavailable"))?;
        // blocking_write_and_flush accepts at most 4096 bytes per call.
        for chunk in body.as_bytes().chunks(4096) {
            if stream.blocking_write_and_flush(chunk).is_err() {
                return Err(node_transport("runner->node body write failed"));
            }
        }
        // `stream` (a child resource of `out_body`) is dropped here, before finish.
    }
    if OutgoingBody::finish(out_body, None).is_err() {
        return Err(node_transport("runner->node body finish failed"));
    }
    let fut = match outgoing_handler::handle(req, None) {
        Ok(f) => f,
        Err(code) => return Err(hop_egress_error(code)), // host refused before dispatch
    };
    let pollable = fut.subscribe();
    pollable.block();
    let resp = match fut.get() {
        Some(Ok(Ok(resp))) => resp,
        Some(Ok(Err(code))) => return Err(hop_egress_error(code)),
        _ => return Err(node_transport("no response from custom node")),
    };
    let status = resp.status();
    // wamn-fqg.29: a 401 is the serve-node's signature REFUSAL
    // (`invocation-unauthorized`) — a persistent authn mismatch (a wrong/rotated
    // signing key, a fail-closed keyless host) that is identical on every
    // attempt, so retrying it only burns the node's whole retry budget before the
    // run fails anyway. Map it to a TERMINAL node failure so the engine routes it
    // immediately (the flow's error path, else a `terminal` run failure), exactly
    // as `hop_egress_error` treats an egress DENIAL. `node_transport` below stays
    // for genuinely transient transport faults (a 5xx, a dropped connection). The
    // refusal body carries only the MAC-free reason class (no oracle); it rides
    // the detail message for operators.
    if status == 401 {
        let reason = read_response_body(resp).unwrap_or_default();
        return Err(NodeError::Terminal(ErrorDetail::coded(
            "invocation-unauthorized",
            format!("runner->node signature refused by the serve-node (HTTP 401): {reason}"),
        )));
    }
    if status != 200 {
        return Err(node_transport(format!(
            "custom node host returned HTTP {status}"
        )));
    }
    read_response_body(resp)
}

/// Drain an incoming response body to a `String`.
fn read_response_body(resp: IncomingResponse) -> Result<String, NodeError> {
    let body = resp
        .consume()
        .map_err(|_| node_transport("custom node response body already consumed"))?;
    let stream = body
        .stream()
        .map_err(|_| node_transport("custom node response body stream unavailable"))?;
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) => out.extend_from_slice(&chunk),
            Err(StreamError::Closed) => break,
            Err(_) => return Err(node_transport("custom node response body read failed")),
        }
    }
    String::from_utf8(out).map_err(|_| node_transport("custom node response body is not UTF-8"))
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
    let (kind, payload, detail_json) = error_row_values(err);
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

/// The error row's column values — the taxonomy tag, the routed `{"error":..}`
/// payload, and the history detail — shared by [`record_error`] (direct path)
/// and [`record_error_and_renew`] (claim path).
fn error_row_values(err: &NodeError) -> (&'static str, Value, Value) {
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
    (kind, payload, detail_json)
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
        wamn_runner::FailKind::RunawayBudget => "runaway-budget",
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
        // 5.6 / wamn-bd5: a CUSTOM node — a separately-deployed, untrusted node
        // component served by a `serve-node` host. v0 dispatch is a boring
        // in-cluster HTTP hop: POST the invocation envelope (ctx + input + this
        // step's declared credential GRANT) to the node's Service endpoint over
        // wasi:http (governed by the host egress guard + the flow's
        // allowed-hosts), then fold the node's emission / node-error back into
        // the walk. The grant is EXACTLY `node.credential` (never the project's
        // whole set); the serve-node host installs it before invoking, get-only.
        "custom" => custom_node_dispatch(d, run_id, flow),
        // The standard node library (5.3): everything the library ships
        // dispatches through the capability policy table over this
        // component's real imports. A NodeError feeds the engine, which
        // decides retry-vs-error-path-vs-fail mechanically from the variant.
        t if wamn_nodes::is_standard(t) => {
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
            // 5.9: the ctx is FRESH per dispatch and carries ONLY this node's
            // declared credential name — the vault resolves it lazily via the
            // wamn:node credentials import, so the secret is scoped to the
            // executing node's context structurally (siblings never see it).
            let mut ctx = wamn_node_guest::caps::CapsCtx {
                credential: d.credential.clone(),
                ..Default::default()
            };
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
/// cjv.3: declare this run's credential grant to the host BEFORE dispatching
/// any node, so the host can enforce the frozen `wamn:node/credentials`
/// `not-granted` grant on this single, long-lived component (whose per-node
/// boundary the host never sees). The grant is the flow's DECLARED credentials
/// (`flow.credentials`); a `get` for anything else — the direct-import bypass a
/// custom node could attempt — is refused. Per-NODE scoping still rides
/// `CapsCtx` (a node reads only its OWN declared name), so `get` is bounded by
/// both. Called on every walk (including a resume) since the grant lives on the
/// long-lived instance and each run overwrites the prior declaration.
fn declare_run_grant(flow: &Flow) {
    let mut names: Vec<String> = flow.credentials.iter().map(|c| c.name.clone()).collect();
    // wamn-fqg.22: the runner also grants ITSELF the reserved per-project-env
    // signing key so `read_signing_key` (the custom-node hop) can resolve it
    // through the same `wamn:node/credentials.get` channel every credential uses
    // — infrastructure, not flow-declared. It never enters the invocation
    // envelope grant (`granted_credentials` reads only `node.credential`), and
    // the serve-node strips it from any grant defensively; a keyless deployment
    // simply resolves nothing.
    names.push(SIGNING_KEY_CREDENTIAL.to_string());
    wamn::runner::credentials::set_granted(&names);
}

/// fqg.11: declare this run's egress allowlist (the flow's declared
/// `allowed-hosts`) to the host BEFORE dispatching any node — the exact
/// cjv.3 shape above, for outbound HTTP instead of credentials. The host
/// intersects it with its own host-level list; an undeclared (or empty)
/// flow is deny-all. Called on every walk (including a resume) since the
/// declaration lives on the long-lived instance and each run overwrites it.
fn declare_run_egress(flow: &Flow) {
    wamn::runner::egress::set_allowed_hosts(&flow.allowed_hosts);
}

/// l5i9.12.2: declare the run this component is driving to the host's trusted
/// causation channel, so the `wamn:postgres` plugin stamps a TRANSACTIONAL
/// `wamn.causation` message ({run, root, depth}) onto every run-owned txn it
/// opens — which the CDC reader (l5i9.12.1) stitches onto the txn's row events.
/// A root run — a cron/webhook firing — is its own root at depth 0.
fn declare_run_context(run_id: &str) {
    declare_run_context_at(run_id, run_id.to_string(), 0);
}

/// l5i9.17: the event-chain thread. An evt run's input envelope (minted by the
/// TRUSTED materializer) carries `causation: {run, root, depth}` — the chain
/// position the materializer computed from the CDC envelope's stitched stamp
/// (parent depth + 1, bounded at 16 with an alertable refusal). Declaring THAT
/// root/depth here makes this run's own writes emit the incremented stamp, so
/// the NEXT hop's events carry it and the loop budget is real — without it,
/// every run would re-root at depth 0 and the materializer's ceiling could
/// never trip. `run` is ALWAYS the claimed run id (never read from input); a
/// missing/malformed `causation` falls back to self-root depth 0 (every
/// non-evt trigger: cron, webhook, manual). Trust note: `input_json`
/// is minted by platform writers (dispatcher / materializer / gateway
/// envelope), not raw tenant bytes — a tenant's webhook BODY lands under
/// `payload`, never at the envelope's top level.
fn declare_run_context_from(run_id: &str, input: &Value) {
    let causation = input.get("causation");
    let root = causation
        .and_then(|c| c.get("root"))
        .and_then(Value::as_str)
        .unwrap_or(run_id)
        .to_string();
    let depth = causation
        .and_then(|c| c.get("depth"))
        .and_then(Value::as_u64)
        .and_then(|d| u32::try_from(d).ok())
        .unwrap_or(0);
    declare_run_context_at(run_id, root, depth);
}

fn declare_run_context_at(run_id: &str, root: String, depth: u32) {
    let ctx = wamn::runner::causation::RunContext {
        run: run_id.to_string(),
        root,
        depth,
    };
    wamn::runner::causation::set_run_context(Some(&ctx));
}

/// Clears the host's causation context when a run's driver returns (ANY path,
/// including an early `?`), so between-run bookkeeping writes carry no stale
/// causation. One flow-runner drains runs strictly sequentially, so a single
/// live guard per [`execute`] call is sufficient.
struct RunContextGuard;

impl Drop for RunContextGuard {
    fn drop(&mut self) {
        wamn::runner::causation::set_run_context(None);
    }
}

fn execute(
    run_id: &str,
    payload: &str,
    kill_after_write: bool,
    flow_id: &str,
) -> Result<RunOutcome, String> {
    // l5i9.12.2: declare this run's causation to the host BEFORE any write, so
    // the wamn:postgres plugin stamps {run, root, depth} onto every run-owned
    // txn. The guard clears it on return (any path) so the next claim starts
    // clean and between-run bookkeeping carries no stale causation.
    declare_run_context(run_id);
    let _run_ctx = RunContextGuard;

    // v1 reconstructs against the ACTIVE flow version (safe while a flow's
    // versions stay structurally compatible — `Plan::resume` raises Mismatch if
    // not); pinning a resume to the run's persisted `flow_version` is a follow-up
    // (docs/run-state.md).
    let flow = load_active_flow(flow_id)?;
    declare_run_grant(&flow);
    declare_run_egress(&flow);
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
    // R32: restore an in-flight retry parked on a prior invocation — the
    // outstanding node re-enters carrying its persisted attempt (the queue served
    // the backoff) so the retry budget advances instead of resetting to 0.
    if let Some((node, attempt)) = load_retry(run_id)? {
        plan.restore_retry(&mut st, &node, attempt);
    }
    let mut http_status: u32 = 0;

    loop {
        // now_ms = 0: the queue's available_at is the retry clock (R32), so a
        // scheduled retry re-enters DUE after its park; `delay` parks via
        // NodeAction::Park.
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
            // R32: a scheduled retry not yet due. Cross-invocation retry belongs
            // to the queue layer (run_queue.available_at / park_sql) — persist the
            // attempt and PARK the run (outcome=1), so the next invocation restores
            // the attempt (DUE now, the park served the backoff) and re-dispatches;
            // the budget advances until success, error-route, or RetryExhausted.
            Step::Wait { node, attempt, .. } => {
                save_retry(run_id, &node, attempt)?;
                return Ok(RunOutcome {
                    version,
                    outcome: 1,
                    http_status,
                });
            }
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
// Guest-side queue claim (fqg.4): claim -> drive (heartbeat) -> dequeue/park.
// The production dispatch path, guest-side. The runner reads its OWN work from
// run_queue instead of being handed a run_id — the same builders the host-side
// dispatcher/claimers use (wamn-run-queue), called through wamn:postgres.
// ---------------------------------------------------------------------------

/// The claim-path result: `outcome` (0 = completed, 1 = parked, 2 = failed) plus
/// the wake delay to park the queue row by when `outcome == 1`.
struct ClaimOutcome {
    outcome: u32,
    park_ms: u64,
}

/// The host-injected durable-queue lease owner (`app.runner`, fqg.4). The plugin
/// sets it per replica (`wamn.runner` config), so a run's lease + heartbeat are
/// owner-scoped and a reclaim after a replica dies is attributable. Read fresh
/// per claim (a `SET LOCAL` GUC lives for one transaction).
fn read_runner_owner() -> Result<String, String> {
    let rs = client::query("SELECT current_setting('app.runner', true)", &[])
        .map_err(|e| err_name(&e))?;
    match rs.rows.first().and_then(|r| r.first()) {
        Some(SqlValue::Text(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err("no runner identity: app.runner is unset (host must inject wamn.runner)".into()),
    }
}

thread_local! {
    /// The `app.runner` owner, read once per instance (fqg.18): the host sets it
    /// from per-replica config at instantiate and never re-sets it, so the value
    /// is immutable for this instance's lifetime.
    static RUNNER_OWNER: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Parsed flows keyed by `flow_id` -> (version, flow) — the fqg.18 plan
    /// cache. Probed against the ACTIVE version the claim statement returns, so
    /// a version flip (hot reload) invalidates on the very next record; an
    /// in-place graph edit that does NOT bump the version is not picked up, and
    /// registration always bumps versions (register_flow + i7i).
    static FLOW_CACHE: RefCell<HashMap<String, (u32, Rc<Flow>)>> = RefCell::new(HashMap::new());
}

/// The instance-cached lease owner (see [`RUNNER_OWNER`]).
fn runner_owner() -> Result<String, String> {
    if let Some(owner) = RUNNER_OWNER.with(|c| c.borrow().clone()) {
        return Ok(owner);
    }
    let owner = read_runner_owner()?;
    RUNNER_OWNER.with(|c| *c.borrow_mut() = Some(owner.clone()));
    Ok(owner)
}

/// A run claimed with its dispatch inputs in one statement (fqg.18).
struct ClaimedRun {
    run_id: String,
    flow_id: String,
    input: Value,
    /// The ACTIVE flow version at claim time — the plan-cache probe. `None`
    /// when no version is active (the flow load then reports it).
    active_version: Option<u32>,
}

/// Claim ONE currently-claimable **unpartitioned** run for `owner` and return
/// its dispatch inputs — the single [`claim_dispatch_sql`] statement that also
/// flips the run `running` and reads the active flow version (what the split
/// path spent three round trips on). Returns None when the queue is drained.
/// Partitioned runs stay on the per-partition ownership path (fqg.1/fqg.9).
fn claim_dispatch(owner: &str, ttl_ms: i64) -> Result<Option<ClaimedRun>, String> {
    let rs = client::query(&claim_dispatch_sql(), &[text(owner), int64(ttl_ms)])
        .map_err(|e| err_name(&e))?;
    let Some(row) = rs.rows.first() else {
        return Ok(None);
    };
    let run_id = match row.first() {
        Some(SqlValue::Text(s)) => s.clone(),
        other => return Err(format!("claim run_id shape: {other:?}")),
    };
    let flow_id = match row.get(1) {
        Some(SqlValue::Text(s)) => s.clone(),
        other => return Err(format!("runs.flow_id shape: {other:?}")),
    };
    let input = match row.get(2) {
        Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => {
            serde_json::from_str(s).map_err(|e| format!("runs.input_json parse: {e}"))?
        }
        _ => Value::Null,
    };
    let active_version = match row.get(3) {
        Some(SqlValue::Int32(v)) => u32::try_from(*v).ok(),
        Some(SqlValue::Int64(v)) => u32::try_from(*v).ok(),
        _ => None,
    };
    Ok(Some(ClaimedRun {
        run_id,
        flow_id,
        input,
        active_version,
    }))
}

/// The flow to drive: the cached parse when it matches the active version,
/// else a fresh load (which also refreshes the cache). See [`FLOW_CACHE`].
fn active_flow(flow_id: &str, active_version: Option<u32>) -> Result<Rc<Flow>, String> {
    if let Some(v) = active_version {
        let hit = FLOW_CACHE.with(|c| {
            c.borrow()
                .get(flow_id)
                .and_then(|(ver, f)| (*ver == v).then(|| f.clone()))
        });
        if let Some(flow) = hit {
            return Ok(flow);
        }
    }
    let flow = Rc::new(load_active_flow(flow_id)?);
    FLOW_CACHE.with(|c| {
        c.borrow_mut()
            .insert(flow_id.to_string(), (flow.version, flow.clone()));
    });
    Ok(flow)
}

/// Per-node checkpoint + lease heartbeat in ONE statement (fqg.18). The renew
/// fires even when the record is an idempotency no-op (a cycle revisiting a
/// node), so a long walk's lease stays live exactly as the split
/// renew-before-dispatch kept it: the claim's fresh lease covers the first
/// node, each record covers the next.
#[expect(
    clippy::too_many_arguments,
    reason = "the checkpoint row's six columns plus the renew pair, mirroring the statement"
)]
fn record_node_run_and_renew(
    run_id: &str,
    node_id: &str,
    seq: i32,
    port: &str,
    output: &Value,
    input: &Value,
    ttl_ms: i64,
    owner: &str,
) -> Result<(), String> {
    client::execute(
        &record_success_and_renew_sql(),
        &[
            text(run_id),
            text(node_id),
            int32(seq),
            text(port),
            jsonb(output),
            jsonb(input),
            int64(ttl_ms),
            text(owner),
        ],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// The error-routed twin of [`record_node_run_and_renew`].
fn record_error_and_renew(
    run_id: &str,
    node_id: &str,
    seq: i32,
    err: &NodeError,
    input: &Value,
    ttl_ms: i64,
    owner: &str,
) -> Result<(), String> {
    let (kind, payload, detail_json) = error_row_values(err);
    client::execute(
        &record_error_and_renew_sql(),
        &[
            text(run_id),
            text(node_id),
            int32(seq),
            jsonb(&payload),
            jsonb(input),
            text(kind),
            jsonb(&detail_json),
            int64(ttl_ms),
            text(owner),
        ],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Mark the run completed AND drop its queue row in one atomic statement
/// (fqg.18) — the claim path's terminal write; [`run_next`] skips its dequeue
/// for a completed run.
fn complete_and_dequeue(run_id: &str, result: &Value) -> Result<(), String> {
    client::execute(&complete_dequeue_sql(), &[text(run_id), jsonb(result)])
        .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Remove a run's queue row on a terminal outcome (the `runs` history stays).
fn dequeue(run_id: &str) -> Result<(), String> {
    client::execute(&dequeue_sql(), &[text(run_id)]).map_err(|e| err_name(&e))?;
    Ok(())
}

/// Park a run for a later wake: push `available_at` by `park_ms` and RELEASE the
/// lease so no replica holds it while it sleeps (the wake re-claim is free —
/// wamn-fqg.5/.7). Reconciliation/doorbell re-offers it at `available_at`.
fn park(run_id: &str, park_ms: u64) -> Result<(), String> {
    let ms = i64::try_from(park_ms).unwrap_or(i64::MAX);
    client::execute(&park_sql(), &[text(run_id), int64(ms)]).map_err(|e| err_name(&e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Guest-side PARTITIONED claim (fqg.9): the per-partition counterpart of the
// unpartitioned self-claim above. A `partitioned(key)` run is dispatched ONLY
// through per-partition ownership — the guest leases a partition, claims its
// HEAD in stream order (one in flight per key), drives it, renews the partition
// lease alongside the run lease, and steps down (releases the lease) when the
// partition drains — so a key's runs never dispatch out of order or two at once
// across replicas. The pure decisions (`plan_acquire` / `plan_partition_claim`)
// and the SQL (`acquire_partitions_sql` / `claim_partition_head_sql`) live in
// wamn-run-queue and are host-gated by queuebench; fqg.9 is their first GUEST
// caller, mirroring how fqg.4 was the first guest caller of `claim_batch_sql`.
// ---------------------------------------------------------------------------

/// A partition head claimed for this replica: the run to drive plus the key it
/// belongs to (needed to renew/release the partition lease around the walk).
struct PartitionHead {
    run_id: String,
    partition_key: String,
}

/// Lease up to one ACQUIRABLE partition for `owner` (unowned, or lease-expired =
/// failover). Idempotent for partitions this replica already holds live — the
/// `acquire_partitions_sql` `ON CONFLICT` only steals an *expired* lease — so a
/// replica accrues ownership across `run-next` calls without churning its live
/// leases. Returns the partition keys this call newly leased (0 or 1).
fn acquire_partitions(owner: &str, ttl_ms: i64) -> Result<Vec<String>, String> {
    let rs = client::query(&acquire_partitions_sql(1), &[text(owner), int64(ttl_ms)])
        .map_err(|e| err_name(&e))?;
    let mut keys = Vec::with_capacity(rs.rows.len());
    for row in &rs.rows {
        match row.first() {
            Some(SqlValue::Text(s)) => keys.push(s.clone()),
            other => return Err(format!("acquire partition_key shape: {other:?}")),
        }
    }
    Ok(keys)
}

/// Claim the single globally-earliest HEAD across every partition `owner` holds a
/// live lease on — head-first, one in flight per key (`claim_partition_head_sql`
/// encodes the D20 policy + the one-in-flight guard). Returns None when no owned
/// partition has a claimable head (drained, or the head is unavailable/blocked).
fn claim_partition_head(owner: &str, ttl_ms: i64) -> Result<Option<PartitionHead>, String> {
    let rs = client::query(&claim_partition_head_sql(1), &[text(owner), int64(ttl_ms)])
        .map_err(|e| err_name(&e))?;
    let Some(row) = rs.rows.first() else {
        return Ok(None);
    };
    let run_id = match row.first() {
        Some(SqlValue::Text(s)) => s.clone(),
        other => return Err(format!("partition head run_id shape: {other:?}")),
    };
    let partition_key = match row.get(1) {
        Some(SqlValue::Text(s)) => s.clone(),
        other => return Err(format!("partition head partition_key shape: {other:?}")),
    };
    Ok(Some(PartitionHead {
        run_id,
        partition_key,
    }))
}

/// Read a claimed run's dispatch inputs (the recorded flow + trigger input) — the
/// partition head claim returns only `(run_id, partition_key)`, so the guest reads
/// what the combined unpartitioned `claim_dispatch_sql` returns inline.
fn read_dispatch(run_id: &str) -> Result<(String, Value), String> {
    let rs = client::query(&run_sql::select_run_dispatch_sql(), &[text(run_id)])
        .map_err(|e| err_name(&e))?;
    let row = rs
        .rows
        .first()
        .ok_or("claimed partition head has no runs row")?;
    let flow_id = match row.first() {
        Some(SqlValue::Text(s)) => s.clone(),
        other => return Err(format!("runs.flow_id shape: {other:?}")),
    };
    let input = match row.get(1) {
        Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => {
            serde_json::from_str(s).map_err(|e| format!("runs.input_json parse: {e}"))?
        }
        _ => Value::Null,
    };
    Ok((flow_id, input))
}

/// Flip a partition-head run `dispatched` -> `running`. The unpartitioned
/// `claim_dispatch_sql` does this inline (its `marked` CTE); the partition head
/// claim does not, so the guest marks it before driving.
fn mark_running(run_id: &str) -> Result<(), String> {
    client::execute(&mark_running_sql(), &[text(run_id)]).map_err(|e| err_name(&e))?;
    Ok(())
}

/// Heartbeat a held partition lease (owner-guarded — a no-op if this replica lost
/// it). Extends the lease by `ttl_ms`, keeping the replica the key's owner across
/// a long head walk. See [`execute_claimed`]'s per-node renewal.
fn renew_partition(partition_key: &str, ttl_ms: i64, owner: &str) -> Result<(), String> {
    client::execute(
        &renew_partition_sql(),
        &[text(partition_key), int64(ttl_ms), text(owner)],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Release a held partition lease (a drained key / step-down), owner-guarded.
fn release_partition(partition_key: &str, owner: &str) -> Result<(), String> {
    client::execute(
        &release_partition_sql(),
        &[text(partition_key), text(owner)],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Settle a driven run's terminal outcome (shared by both claim paths): completed
/// (0) already dropped its queue row inside [`complete_and_dequeue`]; parked (1)
/// pushes `available_at` and releases the run lease; failed (2) dequeues.
fn settle(run_id: &str, claim: &ClaimOutcome) -> Result<(), String> {
    match claim.outcome {
        0 => {}                            // completed: already dequeued
        1 => park(run_id, claim.park_ms)?, // parked -> re-offered at wake
        _ => dequeue(run_id)?,             // failed -> terminal
    }
    Ok(())
}

/// The PARTITIONED turn of the dispatch loop (fqg.9): lease a partition, claim
/// the earliest head across the partitions this replica owns, and drive it via
/// the shared [`execute_claimed`] path (renewing the partition lease per node).
/// On drain — no owned partition has a claimable head — STEP DOWN from the
/// partition just acquired ([`release_partition`]) so another replica (or a later
/// wake) can take it; a lease retained from a served-then-drained partition ages
/// out (`gc_orphan_partitions_sql`). Returns the driven `(run_id, outcome)`, or
/// None when there is no partitioned work to do this turn.
fn claim_partition_run(owner: &str, ttl_ms: i64) -> Result<Option<(String, u32)>, String> {
    let acquired = acquire_partitions(owner, ttl_ms)?;
    let Some(head) = claim_partition_head(owner, ttl_ms)? else {
        for key in &acquired {
            release_partition(key, owner)?;
        }
        return Ok(None);
    };
    mark_running(&head.run_id)?;
    let (flow_id, input) = read_dispatch(&head.run_id)?;
    let flow = active_flow(&flow_id, None)?;
    let claim = execute_claimed(
        &head.run_id,
        &flow,
        input,
        owner,
        ttl_ms,
        Some(&head.partition_key),
    )?;
    settle(&head.run_id, &claim)?;
    Ok(Some((head.run_id, claim.outcome)))
}

/// Drive a run CLAIMED from the queue: like [`execute`] but the flow + input come
/// from the dispatcher-persisted `runs` row (not a fixture id / wrapped string),
/// the lease is renewed per node, and terminal states become an `outcome` code
/// (the caller dequeues/parks) rather than a `Result` return. The dispatcher
/// already wrote the `runs` row and the claim flipped it `running`, so this does
/// NOT re-open the run — it reconstructs from `node_runs` and continues.
fn execute_claimed(
    run_id: &str,
    flow: &Flow,
    input: Value,
    owner: &str,
    ttl_ms: i64,
    partition: Option<&str>,
) -> Result<ClaimOutcome, String> {
    // l5i9.12.2: the production dispatch path (run-next) — declare this run's
    // causation BEFORE any write so the wamn:postgres plugin stamps
    // {run, root, depth} onto every run-owned txn (checkpoints included; the CDC
    // reader drops the platform-schema ones and stitches the app-table writes).
    // l5i9.17: an evt run's input carries the materializer-minted chain
    // position (root, depth) — declared here so the chain budget accumulates.
    // The guard clears it on return so the next claim starts clean.
    declare_run_context_from(run_id, &input);
    let _run_ctx = RunContextGuard;

    declare_run_grant(flow);
    declare_run_egress(flow);
    let plan = Plan::compile(flow).map_err(|e| e.to_string())?;
    let version = plan.version();
    let completed = load_completed(run_id)?;
    let mut next_seq = completed.len() as i32;
    let run_rec = RunRecord::new(run_id, &flow.flow_id, version, input);
    let mut st =
        wamn_run_store::reconstruct(&plan, &run_rec, &completed).map_err(|e| e.to_string())?;
    // R32: restore an in-flight retry parked on a prior claim — the outstanding
    // node re-enters carrying its persisted attempt (the queue served the
    // backoff) so the retry budget advances instead of resetting to 0.
    if let Some((node, attempt)) = load_retry(run_id)? {
        plan.restore_retry(&mut st, &node, attempt);
    }
    let mut http_status: u32 = 0;

    loop {
        match plan.next(&mut st, 0) {
            Step::Done(RunStatus::Completed) => {
                complete_and_dequeue(run_id, st.result())?;
                return Ok(ClaimOutcome {
                    outcome: 0,
                    park_ms: 0,
                });
            }
            Step::Done(status) => {
                if let Some(f) = st.failure() {
                    let _ = mark_failed(run_id, fail_kind_sql(&f.kind), &f.node, &f.detail.message);
                }
                let _ = status;
                return Ok(ClaimOutcome {
                    outcome: 2,
                    park_ms: 0,
                });
            }
            // R32: a scheduled retry not yet due — persist the attempt and PARK
            // the queue row for the backoff (release the lease), the
            // cross-invocation retry the `execute` note deferred to the queue
            // layer. `now_ms` is 0, so `until_ms` IS the backoff to wait; the next
            // claim reconstructs, restores the attempt (DUE now, the park served
            // the wait), and re-dispatches — the budget advances until success,
            // error-route, or RetryExhausted.
            Step::Wait {
                node,
                until_ms,
                attempt,
                ..
            } => {
                save_retry(run_id, &node, attempt)?;
                return Ok(ClaimOutcome {
                    outcome: 1,
                    park_ms: until_ms,
                });
            }
            Step::Dispatch(d) => {
                // The lease heartbeat rides each node's checkpoint statement
                // (fqg.18): the claim's fresh lease covers the first node, each
                // record's renew covers the next — the same coverage the split
                // renew-before-dispatch gave, one round trip cheaper.
                match dispatch_node(&d, run_id, flow, false, &mut http_status)? {
                    NodeAction::Emit(outcome) => {
                        match &outcome {
                            NodeOutcome::Success { payload, port } => {
                                record_node_run_and_renew(
                                    run_id, &d.node, next_seq, port, payload, &d.payload, ttl_ms,
                                    owner,
                                )?;
                                next_seq += 1;
                            }
                            NodeOutcome::Error(err)
                                if will_error_route(err, &d)
                                    && !plan.successors(&d.node, ERROR_PORT).is_empty() =>
                            {
                                record_error_and_renew(
                                    run_id, &d.node, next_seq, err, &d.payload, ttl_ms, owner,
                                )?;
                                next_seq += 1;
                            }
                            NodeOutcome::Error(_) => {}
                        }
                        // fqg.9: when driving a partition HEAD, renew the partition
                        // lease alongside the run lease so this replica stays the
                        // key's stable owner across a long head walk (no needless
                        // mid-run partition steal). Owner-guarded, so a lease this
                        // replica already lost is a no-op. Inert (never called) for
                        // the unpartitioned path.
                        if let Some(pk) = partition {
                            renew_partition(pk, ttl_ms, owner)?;
                        }
                        plan.apply(&mut st, &d, outcome, 0);
                    }
                    NodeAction::Park => {
                        // The delay node recorded a wake deadline in state_json;
                        // park the queue row until then (the wall clock is the
                        // host's — the test host virtualizes it, so a 24h delay is
                        // instant there and the wake is immediate).
                        let now = wall_now_secs();
                        let park_ms = load_wake(run_id)?
                            .map(|wake| wake.saturating_sub(now).saturating_mul(1000))
                            .unwrap_or(0);
                        return Ok(ClaimOutcome {
                            outcome: 1,
                            park_ms,
                        });
                    }
                }
            }
        }
    }
}

/// One turn of the production dispatch loop: claim the next run, drive it with a
/// per-node heartbeat, and dequeue (terminal) or park (delay). See the WIT doc.
fn run_next(lease_ttl_ms: u64) -> Result<(bool, Option<String>, u32), String> {
    let ttl = i64::try_from(lease_ttl_ms).map_err(|_| "lease-ttl-ms too large".to_string())?;
    let owner = runner_owner()?;
    // Unpartitioned first: the global `FOR UPDATE SKIP LOCKED` claim drains
    // unordered NULL-key runs concurrently across replicas (the fqg.4 path).
    if let Some(claimed) = claim_dispatch(&owner, ttl)? {
        let flow = active_flow(&claimed.flow_id, claimed.active_version)?;
        let claim = execute_claimed(&claimed.run_id, &flow, claimed.input, &owner, ttl, None)?;
        settle(&claimed.run_id, &claim)?;
        return Ok((true, Some(claimed.run_id), claim.outcome));
    }
    // Then partitioned: lease a partition and drive its head in order (fqg.9).
    if let Some((run_id, outcome)) = claim_partition_run(&owner, ttl)? {
        return Ok((true, Some(run_id), outcome));
    }
    Ok((false, None, 0)) // queue drained (unpartitioned + owned partitions)
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

    fn run_next(lease_ttl_ms: u64) -> Result<(bool, Option<String>, u32), String> {
        run_next(lease_ttl_ms)
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
