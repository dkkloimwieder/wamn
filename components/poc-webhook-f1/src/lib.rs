//! poc-webhook-f1 — the sync-webhook ingress component (POC-F1, D15 sync path).
//!
//! Exports `wasi:http/incoming-handler` (deployed exactly like the 4.1b
//! api-gateway: one WorkloadDeployment per project, routed by Host header) and
//! imports ONLY `wamn:postgres` — no raw sockets, no outbound HTTP. On each
//! POST it:
//!
//!   1. matches the path against the project's ACTIVE sync-webhook flows
//!      (`flows` registry, re-read per request — the S3 hot-reload discipline),
//!   2. WRITE-AHEADS a `runs` row (`status='dispatched'`, `trigger_source=
//!      'webhook'`, `input_json` = the payload verbatim) BEFORE any effect —
//!      the D15 audit row; the run id is minted server-side
//!      (`gen_random_uuid()`), each POST is a new run,
//!   3. drives the flow SYNCHRONOUSLY through the wamn-runner engine (5.2),
//!      recording a `node_runs` row per completed node in the 5.7 shape (an
//!      error-routed node is recorded as an emission on the `error` port), so
//!      every run is traceable and reconstruction-compatible,
//!   4. answers within the request: the terminal `respond` node's payload is
//!      the body, its config the status (`{receipt_id, holds: [...]}` on the
//!      happy path; 400 `invalid-input` when validation routed the error edge).
//!
//! The F1 node semantics (`validate-receipt` / `upsert-receipt` /
//! `evaluate-specs` / `create-holds` / `respond`) are the thin DB shell around
//! the PURE poc/f1 logic — see that crate and docs/poc-f1.md. The
//! tenant and schema come from the host-injected claims (`wamn.tenant` /
//! `wamn.schema` via localResources.config): the guest never chooses either.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        world: "poc-webhook-f1",
        path: "wit",
        generate_all,
    });
}

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wamn::postgres::client::{self, Transaction};
use bindings::wamn::postgres::types::{PgError, SqlValue};
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method as HttpMethod, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use bindings::wasi::io::streams::StreamError;
use serde_json::{Value, json};
use wamn_f1 as f1;
use wamn_flow::{Flow, Trigger};
use wamn_run_store::sql as run_sql;
use wamn_runner::{
    Dispatch, ERROR_PORT, ErrorDetail, NodeError, NodeOutcome, Plan, RunStatus, Step,
};

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let (status, body) = process(&request);
        send_response(response_out, status, body);
    }
}

bindings::export!(Component with_types_in bindings);

// ---------------------------------------------------------------------------
// Request processing
// ---------------------------------------------------------------------------

fn process(request: &IncomingRequest) -> (u16, Value) {
    let target = request.path_with_query().unwrap_or_default();
    let path = target.split('?').next().unwrap_or("").to_string();

    // Route: the active flow whose sync-webhook trigger owns this path. Re-read
    // per request so activating a new flow version needs no restart.
    let flows = match load_active_flows() {
        Ok(f) => f,
        Err(e) => return (503, error_body("unavailable", &e)),
    };
    let Some(flow) = flows.into_iter().find(
        |f| matches!(&f.trigger, Trigger::Webhook { sync: true, path: Some(p) } if *p == path),
    ) else {
        return (
            404,
            error_body("not-found", "no active sync-webhook flow on this path"),
        );
    };
    if !matches!(request.method(), HttpMethod::Post) {
        return (405, error_body("method-not-allowed", "POST required"));
    }

    // The payload verbatim. A body that is not JSON still gets a run (and its
    // 400): it is carried as a JSON string so the write-ahead row records
    // exactly what arrived.
    let raw = read_body(request);
    let input: Value = serde_json::from_slice(&raw)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()));

    let plan = match Plan::compile(&flow) {
        Ok(p) => p,
        Err(e) => return (500, error_body("bad-flow", &e.to_string())),
    };

    // D15: the audit row exists before any node effect.
    let run_id = match write_ahead(plan.flow_id(), plan.version(), &input) {
        Ok(id) => id,
        Err(e) => return (503, error_body("unavailable", &e)),
    };
    if let Err(e) = mark_running(&run_id) {
        return (503, error_body("unavailable", &e));
    }

    drive(&plan, &run_id, input)
}

/// Drive the compiled flow to completion, recording each completed node. The
/// terminal `respond` node sets the HTTP status; its payload — the engine's
/// final `result` — is the body. Client-facing bodies on INFRASTRUCTURE
/// failures are generic: the pg-error detail is persisted in
/// `runs`/`node_runs` for operators, never echoed to the untrusted caller
/// (only `invalid-input` payloads — the client's own fault, with its issue
/// list — flow through verbatim).
fn drive(plan: &Plan<'_>, run_id: &str, input: Value) -> (u16, Value) {
    let mut st = plan.start(run_id, input);
    let mut next_seq: i32 = 0;
    let mut http_status: u16 = 200;

    loop {
        match plan.next(&mut st, 0) {
            Step::Done(RunStatus::Completed) => {
                if let Err(e) = mark_completed(run_id, st.result()) {
                    return (503, error_body("unavailable", &e));
                }
                // A respond-node 503 override = an infra failure routed down
                // the error edge: sanitize the body (the raw error payload is
                // audit material in result_json, not a client response).
                if http_status == 503 {
                    return (
                        503,
                        error_body("unavailable", "the request could not be completed"),
                    );
                }
                return (http_status, st.result().clone());
            }
            Step::Done(_) => {
                let (kind, node, reason) = match st.failure() {
                    Some(f) => (
                        fail_kind_sql(&f.kind),
                        f.node.clone(),
                        f.detail.message.clone(),
                    ),
                    None => ("terminal", String::new(), "run ended abnormally".into()),
                };
                let _ = mark_failed(run_id, kind, &node, &reason);
                // The detailed reason (raw pg error) stays in runs.fail_reason.
                return (
                    500,
                    error_body(
                        "run-failed",
                        &format!("node {node} failed; see run history"),
                    ),
                );
            }
            Step::Wait { node, .. } => {
                // F1 nodes never emit Retryable, so a scheduled retry cannot
                // occur; treat one defensively as a failed run.
                let _ = mark_failed(run_id, "terminal", &node, "unexpected retry wait");
                return (500, error_body("run-failed", "unexpected retry wait"));
            }
            Step::Dispatch(d) => {
                let outcome = dispatch_node(&d, &mut http_status);
                let recorded = match &outcome {
                    NodeOutcome::Success { payload, port } => {
                        record_success(run_id, &d.node, next_seq, port, payload, &d.payload)
                    }
                    // Record an error row ONLY when the node has an error edge
                    // (the emission actually routes): 5.7 reconstruction folds
                    // every completed row as an emission on its port, so an
                    // 'error' row for an edge-less node would reconstruct a
                    // FAILED run as Completed. A run-failing node's record is
                    // the runs.fail_* columns — the flowrunner contract.
                    NodeOutcome::Error(err) if !plan.successors(&d.node, ERROR_PORT).is_empty() => {
                        record_error(run_id, &d.node, next_seq, err, &d.payload)
                    }
                    NodeOutcome::Error(_) => Ok(()),
                };
                if let Err(e) = recorded {
                    let _ = mark_failed(run_id, "terminal", &d.node, &e);
                    return (503, error_body("unavailable", &e));
                }
                next_seq += 1;
                plan.apply(&mut st, &d, outcome, 0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// F1 node dispatch
// ---------------------------------------------------------------------------

fn dispatch_node(d: &Dispatch, http_status: &mut u16) -> NodeOutcome {
    match d.node_type.as_str() {
        "validate-receipt" => validate_receipt(&d.payload),
        "upsert-receipt" => upsert_receipt(&d.payload),
        "evaluate-specs" => evaluate_specs(&d.payload),
        "create-holds" => create_holds(&d.payload),
        "respond" => respond(d, http_status),
        other => NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg(format!(
            "unknown node type: {other}"
        )))),
    }
}

/// `validate-receipt`: payload shape (pure, wamn-f1), business-key resolution,
/// and spec prefetch. Every client-payload problem (malformed shape, floats,
/// out-of-range decimals, unknown supplier/site/material) is `invalid-input`
/// here, routed down the flow's single error edge to `respond-bad` (400).
fn validate_receipt(payload: &Value) -> NodeOutcome {
    let receipt = match f1::parse_receipt(payload) {
        Ok(r) => r,
        Err(issues) => {
            return invalid_input("receipt validation failed", json!({ "issues": issues }));
        }
    };

    let mut unknown = Vec::new();
    let supplier_id = match resolve_one(f1::sql::RESOLVE_SUPPLIER, &receipt.supplier) {
        Ok(Some(id)) => id,
        Ok(None) => {
            unknown.push(format!("unknown supplier {:?}", receipt.supplier));
            String::new()
        }
        Err(e) => return pg_terminal(e),
    };
    let site_id = match resolve_one(f1::sql::RESOLVE_SITE, &receipt.site) {
        Ok(Some(id)) => id,
        Ok(None) => {
            unknown.push(format!("unknown site {:?}", receipt.site));
            String::new()
        }
        Err(e) => return pg_terminal(e),
    };
    let mut line_specs = Vec::new();
    for (i, line) in receipt.lines.iter().enumerate() {
        match resolve_material(&line.material) {
            Ok(Some(spec)) => line_specs.push(spec),
            Ok(None) => unknown.push(format!(
                "unknown material {:?} (line {})",
                line.material,
                i + 1
            )),
            Err(e) => return pg_terminal(e),
        }
    }
    if !unknown.is_empty() {
        return invalid_input("unknown business keys", json!({ "issues": unknown }));
    }

    NodeOutcome::ok(
        f1::ValidateOut {
            receipt,
            supplier_id,
            site_id,
            line_specs,
        }
        .to_value(),
    )
}

/// `upsert-receipt`: ONE wamn:postgres transaction — receipt upsert on the
/// composite natural key, then replace the line set. Dropping the transaction
/// resource without commit rolls back (host guarantee), so any error leaves
/// nothing behind.
fn upsert_receipt(payload: &Value) -> NodeOutcome {
    let v = match f1::ValidateOut::from_value(payload) {
        Ok(v) => v,
        Err(e) => return NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg(e))),
    };
    match upsert_tx(&v) {
        Ok((receipt_id, line_ids)) => NodeOutcome::ok(
            f1::UpsertOut {
                receipt: v.receipt,
                supplier_id: v.supplier_id,
                site_id: v.site_id,
                line_specs: v.line_specs,
                receipt_id,
                line_ids,
            }
            .to_value(),
        ),
        Err(e) => pg_terminal(e),
    }
}

fn upsert_tx(v: &f1::ValidateOut) -> Result<(String, Vec<String>), PgError> {
    let tx: Transaction = client::begin()?;
    let rs = tx.query(
        f1::sql::UPSERT_RECEIPT,
        &[
            text(&v.receipt.receipt_no),
            SqlValue::Uuid(v.supplier_id.clone()),
            SqlValue::Uuid(v.site_id.clone()),
            SqlValue::Timestamptz(v.receipt.received_at.clone()),
        ],
    )?;
    let receipt_id = first_text(&rs).ok_or_else(no_row)?;
    tx.execute(f1::sql::DELETE_LINES, &[SqlValue::Uuid(receipt_id.clone())])?;
    let mut line_ids = Vec::with_capacity(v.receipt.lines.len());
    for (line, spec) in v.receipt.lines.iter().zip(&v.line_specs) {
        let rs = tx.query(
            f1::sql::INSERT_LINE,
            &[
                SqlValue::Uuid(receipt_id.clone()),
                SqlValue::Uuid(spec.material_id.clone()),
                SqlValue::Numeric(line.quantity.clone()),
            ],
        )?;
        line_ids.push(first_text(&rs).ok_or_else(no_row)?);
    }
    tx.commit()?;
    Ok((receipt_id, line_ids))
}

/// `evaluate-specs`: pure exact-decimal evaluation (wamn-f1). All lines
/// in-spec => the final `{receipt_id, holds: []}` body on the main port;
/// any exceedance => the `out-of-spec` branch toward `create-holds`.
fn evaluate_specs(payload: &Value) -> NodeOutcome {
    let u = match f1::UpsertOut::from_value(payload) {
        Ok(u) => u,
        Err(e) => return NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg(e))),
    };
    let mut out_of_spec = Vec::new();
    for (i, (line, spec)) in u.receipt.lines.iter().zip(&u.line_specs).enumerate() {
        let reasons = match f1::evaluate_line(
            &line.quantity,
            &line.moisture_pct,
            &line.weight_kg,
            &spec.moisture_max_pct,
            &spec.weight_tolerance_kg,
        ) {
            Ok(r) => r,
            Err(e) => return NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg(e))),
        };
        if !reasons.is_empty() {
            out_of_spec.push(f1::OutOfSpec {
                line: (i + 1) as u32,
                line_id: u.line_ids.get(i).cloned().unwrap_or_default(),
                material: line.material.clone(),
                reason: reasons.join("; "),
            });
        }
    }
    if out_of_spec.is_empty() {
        NodeOutcome::ok(f1::ok_body(&u.receipt_id, &[]))
    } else {
        NodeOutcome::ok_on(
            f1::EvalBranchOut {
                receipt_id: u.receipt_id,
                site_id: u.site_id,
                out_of_spec,
            }
            .to_value(),
            "out-of-spec",
        )
    }
}

/// `create-holds`: one `quality_holds` row per out-of-spec line (`status
/// 'open'`, opened server-side), in ONE transaction — a mid-loop failure rolls
/// back every partial hold, so a transient DB error never strands an orphaned
/// hold whose FK would block the receipt's replace-lines forever. Output is
/// the final `{receipt_id, holds}` body carrying the persisted hold ids.
fn create_holds(payload: &Value) -> NodeOutcome {
    let b = match f1::EvalBranchOut::from_value(payload) {
        Ok(b) => b,
        Err(e) => return NodeOutcome::Error(NodeError::Terminal(ErrorDetail::msg(e))),
    };
    match holds_tx(&b) {
        Ok(holds) => NodeOutcome::ok(f1::ok_body(&b.receipt_id, &holds)),
        Err(e) => pg_terminal(e),
    }
}

fn holds_tx(b: &f1::EvalBranchOut) -> Result<Vec<f1::HoldEntry>, PgError> {
    let tx: Transaction = client::begin()?;
    let mut holds = Vec::with_capacity(b.out_of_spec.len());
    for entry in &b.out_of_spec {
        let rs = tx.query(
            f1::sql::INSERT_HOLD,
            &[
                SqlValue::Uuid(entry.line_id.clone()),
                SqlValue::Uuid(b.site_id.clone()),
            ],
        )?;
        let hold_id = first_text(&rs).ok_or_else(no_row)?;
        holds.push(f1::HoldEntry {
            hold_id,
            line: entry.line,
            material: entry.material.clone(),
            reason: entry.reason.clone(),
            status: "open".to_string(),
        });
    }
    tx.commit()?;
    Ok(holds)
}

/// `respond`: terminal passthrough that fixes the HTTP status from its config
/// via the pure [`wamn_f1::respond_status`] rule (unit-tested there): the
/// configured status, except an error-path respond answering for a DIFFERENT
/// error code than configured — an infrastructure failure routed down the
/// error edge — which answers 503.
fn respond(d: &Dispatch, http_status: &mut u16) -> NodeOutcome {
    *http_status = f1::respond_status(&d.config, &d.payload);
    NodeOutcome::ok(d.payload.clone())
}

// ---------------------------------------------------------------------------
// wamn:postgres helpers
// ---------------------------------------------------------------------------

fn text(s: &str) -> SqlValue {
    SqlValue::Text(s.to_string())
}

/// Encode a payload Value for a jsonb column: sent as a TEXT param the server
/// parses into jsonb — so serde_json::Value round-trips exactly (no float
/// lossiness). The flowrunner pattern.
fn jsonb(v: &Value) -> SqlValue {
    SqlValue::Text(v.to_string())
}

fn first_text(rs: &bindings::wamn::postgres::types::RowSet) -> Option<String> {
    match rs.rows.first()?.first()? {
        SqlValue::Text(s) | SqlValue::Uuid(s) | SqlValue::Numeric(s) => Some(s.clone()),
        _ => None,
    }
}

fn no_row() -> PgError {
    PgError::QueryError(("0".into(), "expected a returned row".into()))
}

fn resolve_one(sql: &str, key: &str) -> Result<Option<String>, PgError> {
    let rs = client::query(sql, &[text(key)])?;
    Ok(rs.rows.first().and_then(|row| match row.first() {
        Some(SqlValue::Text(s)) | Some(SqlValue::Uuid(s)) => Some(s.clone()),
        _ => None,
    }))
}

fn resolve_material(name: &str) -> Result<Option<f1::LineSpec>, PgError> {
    let rs = client::query(f1::sql::RESOLVE_MATERIAL, &[text(name)])?;
    let Some(row) = rs.rows.first() else {
        return Ok(None);
    };
    let cell = |i: usize| match row.get(i) {
        Some(SqlValue::Text(s)) | Some(SqlValue::Uuid(s)) | Some(SqlValue::Numeric(s)) => {
            Some(s.clone())
        }
        _ => None,
    };
    Ok(match (cell(0), cell(1), cell(2)) {
        (Some(material_id), Some(moisture_max_pct), Some(weight_tolerance_kg)) => {
            Some(f1::LineSpec {
                material_id,
                moisture_max_pct,
                weight_tolerance_kg,
            })
        }
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Run state (runs / node_runs — the 5.7 shape)
// ---------------------------------------------------------------------------

/// The D15 write-ahead: the audit row exists — status `'dispatched'`, payload
/// verbatim — before any node runs. The run id is minted server-side; every
/// POST is a distinct run.
fn write_ahead(flow_id: &str, version: u32, input: &Value) -> Result<String, String> {
    let rs = client::query(
        &run_sql::insert_run_returning_id_sql(),
        &[
            text(flow_id),
            SqlValue::Int32(version as i32),
            text(wamn_run_store::RunStatus::Dispatched.as_sql()),
            text("webhook"),
            jsonb(input),
        ],
    )
    .map_err(|e| pg_tag(&e))?;
    match rs.rows.first().and_then(|r| r.first()) {
        Some(SqlValue::Text(s)) => Ok(s.clone()),
        _ => Err("write-ahead returned no run id".to_string()),
    }
}

fn mark_running(run_id: &str) -> Result<(), String> {
    client::execute(&run_sql::update_run_running_sql(), &[text(run_id)])
    .map(|_| ())
    .map_err(|e| pg_tag(&e))
}

fn mark_completed(run_id: &str, result: &Value) -> Result<(), String> {
    client::execute(
        &run_sql::update_run_completed_sql(),
        &[text(run_id), jsonb(result)],
    )
    .map(|_| ())
    .map_err(|e| pg_tag(&e))
}

fn mark_failed(run_id: &str, kind: &str, node: &str, reason: &str) -> Result<(), String> {
    client::execute(
        &run_sql::update_run_failed_sql(),
        &[text(run_id), text(kind), text(node), text(reason)],
    )
    .map(|_| ())
    .map_err(|e| pg_tag(&e))
}

/// Record a successful node emission — the flowrunner (5.7) shape verbatim, so
/// reconstruction and the run-history read model see one format.
fn record_success(
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
            SqlValue::Int32(seq),
            text(port),
            jsonb(output),
            jsonb(input),
        ],
    )
    .map(|_| ())
    .map_err(|e| pg_tag(&e))
}

/// Record an errored node as an emission on the `error` port carrying the same
/// `{"error": {...}}` payload the engine routes down the error edge — exactly
/// what 5.7 reconstruction replays (it needs no error taxonomy; the taxonomy
/// lands in `error_kind`/`error_detail` for the run history).
fn record_error(
    run_id: &str,
    node_id: &str,
    seq: i32,
    err: &NodeError,
    input: &Value,
) -> Result<(), String> {
    let (kind, detail) = error_parts(err);
    client::execute(
        &run_sql::insert_node_run_error_sql(),
        &[
            text(run_id),
            text(node_id),
            SqlValue::Int32(seq),
            jsonb(&error_payload(detail)),
            jsonb(input),
            text(kind),
            jsonb(&detail_json(detail)),
        ],
    )
    .map(|_| ())
    .map_err(|e| pg_tag(&e))
}

// ---------------------------------------------------------------------------
// Error plumbing
// ---------------------------------------------------------------------------

fn invalid_input(message: &str, data: Value) -> NodeOutcome {
    NodeOutcome::Error(NodeError::InvalidInput(ErrorDetail {
        message: message.to_string(),
        code: Some("invalid-input".to_string()),
        data: Some(data),
    }))
}

/// A Postgres failure in a node effect: permanent for this synchronous request
/// (the ERP retries the POST; the sync path never retries internally), tagged
/// with the pg-error taxonomy so the run history shows what actually failed.
fn pg_terminal(e: PgError) -> NodeOutcome {
    NodeOutcome::Error(NodeError::Terminal(ErrorDetail {
        message: format!("postgres: {e:?}"),
        code: Some(pg_tag(&e)),
        data: None,
    }))
}

fn pg_tag(e: &PgError) -> String {
    match e {
        PgError::SerializationFailure => "serialization-failure",
        PgError::ConnectionUnavailable => "connection-unavailable",
        PgError::StatementTimeout => "statement-timeout",
        PgError::RowLimitExceeded(_) => "row-limit-exceeded",
        PgError::UniqueViolation(_) => "unique-violation",
        PgError::ForeignKeyViolation(_) => "foreign-key-violation",
        PgError::CheckViolation(_) => "check-violation",
        PgError::PermissionDenied => "permission-denied",
        PgError::QueryError(_) => "query-error",
    }
    .to_string()
}

fn error_parts(err: &NodeError) -> (&'static str, Option<&ErrorDetail>) {
    match err {
        NodeError::Retryable(d) => ("retryable", Some(d)),
        NodeError::RateLimited(r) => ("rate-limited", Some(&r.detail)),
        NodeError::Terminal(d) => ("terminal", Some(d)),
        NodeError::InvalidInput(d) => ("invalid-input", Some(d)),
        NodeError::Cancelled => ("cancelled", None),
    }
}

/// The `{"error": {...}}` payload the engine hands the error edge — mirrored
/// here (the engine's builder is crate-private) so the recorded emission is
/// byte-equivalent to what respond-bad received.
fn error_payload(detail: Option<&ErrorDetail>) -> Value {
    let mut err = serde_json::Map::new();
    if let Some(d) = detail {
        err.insert("message".into(), Value::String(d.message.clone()));
        if let Some(code) = &d.code {
            err.insert("code".into(), Value::String(code.clone()));
        }
        if let Some(data) = &d.data {
            err.insert("data".into(), data.clone());
        }
    }
    Value::Object(serde_json::Map::from_iter([(
        "error".to_string(),
        Value::Object(err),
    )]))
}

fn detail_json(detail: Option<&ErrorDetail>) -> Value {
    match detail {
        Some(d) => json!({ "message": d.message, "code": d.code, "data": d.data }),
        None => Value::Null,
    }
}

fn fail_kind_sql(kind: &wamn_runner::FailKind) -> &'static str {
    match kind {
        wamn_runner::FailKind::Terminal => "terminal",
        wamn_runner::FailKind::RetryExhausted => "retry-exhausted",
        wamn_runner::FailKind::InvalidInput => "invalid-input",
    }
}

fn error_body(code: &str, message: &str) -> Value {
    json!({ "error": { "code": code, "message": message } })
}

// ---------------------------------------------------------------------------
// Flow registry
// ---------------------------------------------------------------------------

/// The project's active flows (RLS-scoped to the injected tenant, resolved in
/// the injected schema). A row whose graph fails to parse is skipped — this
/// ingress serves the flows it understands; registration (`publish-catalog
/// --flow`) validates before activating. Registration rejects a webhook path
/// another active flow already serves (pre-check + the
/// flows_active_webhook_path unique index — wamn-i7i), so a collision can only
/// be pre-index residue; ORDER BY flow_id keeps even that pick deterministic.
fn load_active_flows() -> Result<Vec<Flow>, String> {
    let rs = client::query(
        "SELECT graph_json::text FROM flows WHERE active ORDER BY flow_id",
        &[],
    )
    .map_err(|e| pg_tag(&e))?;
    let mut flows = Vec::new();
    for row in &rs.rows {
        if let Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) = row.first()
            && let Ok(flow) = Flow::from_json(s)
        {
            flows.push(flow);
        }
    }
    Ok(flows)
}

// ---------------------------------------------------------------------------
// wasi:http plumbing (the api-gateway shapes)
// ---------------------------------------------------------------------------

fn read_body(request: &IncomingRequest) -> Vec<u8> {
    let Ok(body) = request.consume() else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    if let Ok(stream) = body.stream() {
        loop {
            match stream.blocking_read(8192) {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => buf.extend_from_slice(&chunk),
                Err(StreamError::Closed) => break,
                Err(_) => break,
            }
        }
    }
    buf
}

fn send_response(response_out: ResponseOutparam, status: u16, body: Value) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let resp = OutgoingResponse::new(headers);
    let _ = resp.set_status_code(status);
    let outgoing_body = resp.body().expect("outgoing-response body");
    ResponseOutparam::set(response_out, Ok(resp));

    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    if let Ok(stream) = outgoing_body.write() {
        for chunk in bytes.chunks(4096) {
            if stream.blocking_write_and_flush(chunk).is_err() {
                break;
            }
        }
    }
    let _ = OutgoingBody::finish(outgoing_body, None);
}
