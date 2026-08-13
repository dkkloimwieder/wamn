//! Production flow-runner (5.2), grown from the S3 spike and the S6 test-host
//! spike. The runner is a long-lived component that embeds the standard node
//! library as NATIVE Rust — since 5.3 the `wamn-standard-nodes` vocabulary, dispatched
//! through the SDK capability facade under the policy table
//! (docs/archive/execution/node-library.md), beside the S3/S6 fixture node shapes — and walks
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
//! respond`; the legacy S6 fixture adds an `http-call` through `wasi:http`.
//! The test host interposes that capability while running the same compiled
//! binary as the production host.
//!
//! ## Checkpoint / resume (5.7)
//! Durable run state is the `runs` / `node_runs` tables (`deploy/sql/run-state.sql`):
//! a `runs` row per execution and a `node_runs` row per completed node. On every
//! invocation the runner **reconstructs** the in-memory `ExecutionState` by replaying
//! the persisted `node_runs` through the pure engine (`wamn-run-state`) — the
//! branch-aware durable resume that supersedes the S3 linear `step_seq`. A node
//! with a persisted record is never re-dispatched (its effect does not repeat);
//! a node with none is outstanding and re-runs, so an effect that committed in
//! the crash window between its DB write and its `node_runs` row replays
//! at-least-once and is absorbed by the node's own idempotency (`pg-write`'s
//! `sink` `ON CONFLICT DO NOTHING`). A resumed run finishes down exactly the
//! branch it took.

wit_bindgen::generate!({
    world: "flowrunner",
    path: "wit",
    generate_all,
});

use std::cell::RefCell;
use std::time::Instant;

use serde_json::{Value, json};
use wamn_flow::node_contract::{self as sdk, ErrorDetail, NodeError, RateLimitDetail};
use wamn_flow::{
    ERROR_PORT, FailConfig, Flow, InvokeActorMode, InvokeFlowConfig, MAIN_PORT, ResolvedInterfaces,
    canonical_json_sha256,
};
use wamn_run_state::child::{
    ChildCreateResult, ChildReleaseResult, create_or_recover_child_sql, release_child_sql,
};
use wamn_run_state::transitions::{
    CallerReleaseResult, CheckpointResult, ReservedCheckpointResult, StoredCallerOutcome,
    TerminalizeResult, complete_attempt_error_sql, complete_attempt_success_sql,
    release_caller_sql, reserved_checkpoint_sql, terminalize_sql,
};
use wamn_run_state::{CaptureMode, NodeRunRecord, RunRecord, sql as run_sql};
// The durable-queue claim-path builders (5.14). Scheduling is a separate crate,
// so the cron/calendar dependency closure never enters this guest (fqg.4).
// The combined claim/checkpoint/complete statements are the fqg.18 record-stream
// amortization: one statement where the split path spent two or three.
use wamn_run_state::queue::{
    acquire_partitions_sql, claim_dispatch_sql, claim_partition_head_sql, dead_letter_dequeue_sql,
    mark_running_sql, park_sql as queue_park_sql, record_error_and_renew_sql,
    record_success_and_renew_sql, release_partition_sql, renew_partition_sql,
};
use wamn_runner::{
    CallerState, Dispatch, ExecutionFailureKind, ExecutionState, ExecutionStatus, NodeOutcome,
    Plan, ReservedStep, RetryPolicy, Step, ThrottleKey, validate_cron_outcome,
    validate_event_outcome, validate_fail_outcome, validate_request_outcome,
};

use wamn::postgres::client::{self};
use wamn::postgres::types::{PgError, SqlValue};
use wamn::runner::http_effect;

use wasi::http::outgoing_handler;
use wasi::http::types::{Fields, Method, OutgoingRequest, Scheme};

struct Component;
export!(Component);

/// The S3 PoC flow. Two versions differ only in the transform op, so a
/// hot-reloaded version is observable in the run's return value.
const FLOW_ID: &str = "poc-receipt";
/// The legacy S6 HTTP flow.
const FLOW_ID_S6: &str = "poc-s6";
/// Database-local project-environment setting that grants the POC's RawSql node.
///
/// The explicit tenant predicate is intentional even though the table also has
/// forced RLS: the configuration lookup must stay scoped if an administrative
/// host connection ever bypasses RLS. The primary key makes duplicates
/// impossible in the canonical schema; `LIMIT 2` lets the reader fail closed if
/// a drifted schema admits them.
const RAW_SQL_ENABLED_SQL: &str = "SELECT config_value::text \
    FROM app_system.configurations \
    WHERE tenant_id = NULLIF(current_setting('app.tenant', true), '') \
      AND config_key = $1 \
    ORDER BY tenant_id, config_key \
    LIMIT 2";
const RAW_SQL_ENABLED_KEY: &str = "raw_sql_enabled";

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

/// Read the trusted project-environment RawSql grant.
///
/// Missing storage, query failures, zero/multiple rows, malformed JSON, and
/// every JSON value other than the boolean `true` deny the grant. In
/// particular, node input and node config are not consulted.
fn raw_sql_enabled() -> bool {
    let Ok(result) = client::query(RAW_SQL_ENABLED_SQL, &[text(RAW_SQL_ENABLED_KEY)]) else {
        return false;
    };
    raw_sql_enabled_rows(&result.rows)
}

fn raw_sql_enabled_rows(rows: &[Vec<SqlValue>]) -> bool {
    let [row] = rows else {
        return false;
    };
    let [SqlValue::Text(value) | SqlValue::Json(value)] = row.as_slice() else {
        return false;
    };
    matches!(serde_json::from_str::<Value>(value), Ok(Value::Bool(true)))
}

/// Identity claims passed with one trusted HTTP effect call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpEffectContext {
    pub version: String,
    pub run_id: String,
    pub root_plan_hash: String,
    pub current_plan_hash: String,
    pub frame_id: u64,
    pub local_node_id: String,
    pub occurrence: u32,
    pub source_artifact_hash: String,
    pub requirement_name: String,
}

/// The standard-node capability facade over this component's real imports.
#[derive(Default)]
pub struct CapsCtx {
    /// Whether the `RawSql` capability is granted.
    pub raw_sql: bool,
    /// Complete claims for this single effect attempt.
    pub http_effect: Option<HttpEffectContext>,
}

impl sdk::NodeCtx for CapsCtx {
    fn http(&mut self, req: &sdk::HttpRequest) -> Result<sdk::HttpResponse, sdk::HttpCapError> {
        trusted_http_effect(self.http_effect.as_ref(), req)
    }

    fn pg_query(
        &mut self,
        sql: &str,
        params: &[sdk::PgValue],
    ) -> Result<sdk::PgRows, sdk::PgCapError> {
        let params: Vec<SqlValue> = params.iter().map(sdk_to_wit).collect();
        let rs = client::query(sql, &params).map_err(wit_err_to_sdk)?;
        Ok(sdk::PgRows {
            columns: rs.columns.iter().map(|c| c.name.clone()).collect(),
            rows: rs
                .rows
                .iter()
                .map(|r| r.iter().map(wit_to_sdk).collect())
                .collect(),
        })
    }

    fn pg_execute(&mut self, sql: &str, params: &[sdk::PgValue]) -> Result<u64, sdk::PgCapError> {
        let params: Vec<SqlValue> = params.iter().map(sdk_to_wit).collect();
        client::execute(sql, &params).map_err(wit_err_to_sdk)
    }

    fn catalog_json(&mut self) -> Result<String, sdk::PgCapError> {
        let rs = client::query("SELECT document::text FROM wamn_catalog LIMIT 1", &[])
            .map_err(wit_err_to_sdk)?;
        match rs.rows.first().and_then(|r| r.first()) {
            Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => Ok(s.clone()),
            _ => Err(sdk::PgCapError::QueryError {
                code: String::new(),
                message: "no catalog snapshot published for this project".into(),
            }),
        }
    }

    fn raw_sql_enabled(&self) -> bool {
        self.raw_sql
    }
}

fn sdk_to_wit(v: &sdk::PgValue) -> SqlValue {
    match v {
        sdk::PgValue::Null => SqlValue::Null,
        sdk::PgValue::Bool(b) => SqlValue::Boolean(*b),
        sdk::PgValue::Int32(n) => SqlValue::Int32(*n),
        sdk::PgValue::Int64(n) => SqlValue::Int64(*n),
        sdk::PgValue::Float64(f) => SqlValue::Float64(*f),
        sdk::PgValue::Text(s) => SqlValue::Text(s.clone()),
        sdk::PgValue::Bytes(b) => SqlValue::Bytes(b.clone()),
        sdk::PgValue::Numeric(s) => SqlValue::Numeric(s.clone()),
        sdk::PgValue::Timestamptz(s) => SqlValue::Timestamptz(s.clone()),
        sdk::PgValue::Json(s) => SqlValue::Json(s.clone()),
        sdk::PgValue::Uuid(s) => SqlValue::Uuid(s.clone()),
    }
}

fn wit_to_sdk(v: &SqlValue) -> sdk::PgValue {
    match v {
        SqlValue::Null => sdk::PgValue::Null,
        SqlValue::Boolean(b) => sdk::PgValue::Bool(*b),
        SqlValue::Int32(n) => sdk::PgValue::Int32(*n),
        SqlValue::Int64(n) => sdk::PgValue::Int64(*n),
        SqlValue::Float64(f) => sdk::PgValue::Float64(*f),
        SqlValue::Text(s) => sdk::PgValue::Text(s.clone()),
        SqlValue::Bytes(b) => sdk::PgValue::Bytes(b.clone()),
        SqlValue::Numeric(s) => sdk::PgValue::Numeric(s.clone()),
        SqlValue::Timestamptz(s) => sdk::PgValue::Timestamptz(s.clone()),
        SqlValue::Json(s) => sdk::PgValue::Json(s.clone()),
        SqlValue::Uuid(s) => sdk::PgValue::Uuid(s.clone()),
    }
}

fn wit_err_to_sdk(e: PgError) -> sdk::PgCapError {
    match e {
        PgError::SerializationFailure => sdk::PgCapError::SerializationFailure,
        PgError::ConnectionUnavailable => sdk::PgCapError::ConnectionUnavailable,
        PgError::StatementTimeout => sdk::PgCapError::StatementTimeout,
        PgError::RowLimitExceeded(n) => sdk::PgCapError::RowLimitExceeded(n),
        PgError::UniqueViolation(c) => sdk::PgCapError::UniqueViolation(c),
        PgError::ForeignKeyViolation(c) => sdk::PgCapError::ForeignKeyViolation(c),
        PgError::CheckViolation(c) => sdk::PgCapError::CheckViolation(c),
        PgError::PermissionDenied => sdk::PgCapError::PermissionDenied,
        PgError::QueryError((code, message)) => sdk::PgCapError::QueryError { code, message },
    }
}

fn trusted_http_effect(
    context: Option<&HttpEffectContext>,
    req: &sdk::HttpRequest,
) -> Result<sdk::HttpResponse, sdk::HttpCapError> {
    let context = context.ok_or(sdk::HttpCapError::NotGranted)?;
    if context.requirement_name != req.requirement {
        return Err(sdk::HttpCapError::BadRequest(
            "request requirement does not match the attempt context".into(),
        ));
    }
    let context = http_effect_context_to_wit(context);
    let request = http_effect::RelativeRequest {
        method: req.method.clone(),
        path_and_query: req.path_and_query.clone(),
        headers: req
            .headers
            .iter()
            .map(|(name, value)| http_effect::Header {
                name: name.clone(),
                value: value.as_bytes().to_vec(),
            })
            .collect(),
        body: req.body.clone(),
    };
    http_effect::send(&context, &request)
        .map(|response| sdk::HttpResponse {
            status: response.status,
            headers: response
                .headers
                .into_iter()
                .map(|header| {
                    (
                        header.name,
                        String::from_utf8_lossy(&header.value).into_owned(),
                    )
                })
                .collect(),
            body: response.body,
        })
        .map_err(|error| match error {
            http_effect::EffectError::InvalidContext
            | http_effect::EffectError::UndeclaredRequirement
            | http_effect::EffectError::NodeNotPermitted
            | http_effect::EffectError::Unbound
            | http_effect::EffectError::InactiveGeneration
            | http_effect::EffectError::Incompatible
            | http_effect::EffectError::AuthorityDenied => sdk::HttpCapError::Denied,
            http_effect::EffectError::CredentialUnavailable => {
                sdk::HttpCapError::Transport("credential unavailable".into())
            }
            http_effect::EffectError::Timeout => sdk::HttpCapError::Transport("timeout".into()),
            http_effect::EffectError::Transport(detail) => sdk::HttpCapError::Transport(detail),
        })
}

fn http_effect_context_to_wit(context: &HttpEffectContext) -> http_effect::InvocationContext {
    http_effect::InvocationContext {
        version: context.version.clone(),
        run_id: context.run_id.clone(),
        root_plan_hash: context.root_plan_hash.clone(),
        current_plan_hash: context.current_plan_hash.clone(),
        frame_id: context.frame_id,
        local_node_id: context.local_node_id.clone(),
        occurrence: context.occurrence,
        source_artifact_hash: context.source_artifact_hash.clone(),
        requirement_name: context.requirement_name.clone(),
    }
}

/// An already-serialized jsonb/text value or SQL NULL.
fn opt_text(s: Option<String>) -> SqlValue {
    match s {
        Some(v) => SqlValue::Text(v),
        None => SqlValue::Null,
    }
}

/// An optional bigint bind (the 9.6 `output_size` column), NULL when absent.
fn opt_int64(n: Option<i64>) -> SqlValue {
    match n {
        Some(v) => SqlValue::Int64(v),
        None => SqlValue::Null,
    }
}

/// The four persisted capture facts plus the full-mode scrub decision.
fn capture_binds(capture: CaptureMode, output: &Value, input: &Value) -> ([SqlValue; 4], bool) {
    let c = wamn_run_state::derive_capture(capture, output, input);
    (
        [
            opt_text(c.output_json),
            opt_text(c.input_json),
            opt_int64(c.output_size),
            opt_text(c.payload_hash),
        ],
        capture == CaptureMode::Full,
    )
}

/// Name a pg-error by its variant (no host detail beyond the taxonomy tag), so
/// the harness can assert on the error kind. Mirrors the guest probe error name.
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

/// Read a SPECIFIC flow version's definition from the catalog (wamn-cox): a
/// resume loads the run's persisted `runs.flow_version`, not whatever is active
/// now, so a flow edited mid-run cannot make reconstruction diverge from the
/// graph the run started on. `version` is absent only if the row was deleted
/// under the run — surfaced as an explicit error rather than a silent fallback
/// to the active version.
fn load_flow_at(flow_id: &str, version: u32) -> Result<Flow, String> {
    let rs = client::query(
        "SELECT graph_json::text FROM flows WHERE flow_id = $1 AND version = $2",
        &[text(flow_id), int32(version as i32)],
    )
    .map_err(|e| err_name(&e))?;
    let row = rs
        .rows
        .first()
        .ok_or_else(|| format!("no flow at persisted version {version}"))?;
    let raw = match row.first() {
        Some(SqlValue::Text(s)) => s.clone(),
        Some(SqlValue::Json(s)) => s.clone(),
        other => return Err(format!("unexpected graph_json shape: {other:?}")),
    };
    Flow::from_json(&raw).map_err(|e| format!("flow parse: {e}"))
}

const DRAFT_TRIGGER_SOURCE: &str = "scenario-draft";
const DRAFT_SOURCE_PRODUCER: &str = "draft-scenario";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinnedArtifactLineage {
    Release,
    Draft,
}

/// Classify the two independently persisted source claims as one closed pair.
/// A one-sided draft marker is never allowed to fall through to release lookup.
fn classify_pinned_artifact_lineage(
    trigger_source: Option<&str>,
    source_producer: Option<&str>,
) -> Result<PinnedArtifactLineage, String> {
    match (
        trigger_source == Some(DRAFT_TRIGGER_SOURCE),
        source_producer == Some(DRAFT_SOURCE_PRODUCER),
    ) {
        (true, true) => Ok(PinnedArtifactLineage::Draft),
        (false, false) => Ok(PinnedArtifactLineage::Release),
        _ => Err("run has mismatched draft source identity".to_string()),
    }
}

/// Read and verify the exact immutable execution plan selected at admission.
///
/// `scenario-draft` + `draft-scenario` is one closed lineage branch. A
/// one-sided marker returns only the invalid-source row, so it cannot fall
/// through to another lineage. The draft branch rebinds every persisted run
/// claim to one immutable `validated_flow_drafts` row, joins its exact bytes
/// from `execution_bundles`, and verifies the plan, bundle hash, and
/// root artifact. The release branch returns no executable bytes and the loader
/// refuses it until release and run plan pinning land. Neither branch reads the
/// mutable `flows` or `flow_drafts` heads.
const PINNED_ARTIFACT_SQL: &str = "\
WITH classified_run AS MATERIALIZED ( \
    SELECT r.trigger_source, \
           r.invocation_context #>> '{source,producer}' AS source_producer, \
           r.invocation_context #>> '{principal,artifact-digest}' AS artifact_digest, \
           CASE \
             WHEN r.trigger_source = 'scenario-draft' \
              AND r.invocation_context #>> '{source,producer}' = 'draft-scenario' \
               THEN 'draft' \
             WHEN r.trigger_source IS DISTINCT FROM 'scenario-draft' \
              AND r.invocation_context #>> '{source,producer}' \
                    IS DISTINCT FROM 'draft-scenario' \
               THEN 'release' \
             ELSE 'invalid' \
           END AS artifact_lineage, \
           r.* \
      FROM runs AS r \
     WHERE r.run_id = $1 \
), blocked_lineage AS ( \
    SELECT r.trigger_source, r.source_producer, NULL::bytea AS exact_bytes, \
           NULL::text AS execution_bundle_hash, NULL::text AS artifact_digest \
      FROM classified_run AS r \
     WHERE r.artifact_lineage <> 'draft' \
), draft_plan AS ( \
    SELECT r.trigger_source, r.source_producer, bundle.exact_bytes, \
           r.execution_bundle_hash, d.draft_artifact_hash \
      FROM classified_run AS r \
      JOIN catalog.validated_flow_drafts AS d \
        ON d.tenant_id = r.tenant_id \
       AND d.flow_id = r.flow_id \
       AND d.runtime_flow_version = r.flow_version \
       AND d.catalog_id = r.catalog_id \
       AND d.catalog_version = r.catalog_version \
       AND d.environment = r.environment \
       AND d.draft_artifact_hash = r.artifact_digest \
       AND d.draft_id = r.invocation_context #>> '{principal,draft-id}' \
       AND d.draft_revision::text = \
             r.invocation_context #>> '{principal,draft-revision}' \
       AND d.validated_draft_hash = \
             r.invocation_context #>> '{principal,validated-draft-hash}' \
       AND d.execution_bundle_hash = r.execution_bundle_hash \
       AND d.binding_base_artifact_hash = \
             r.invocation_context #>> '{principal,binding-base-artifact-hash}' \
       AND d.suite_flow_version::text = \
             r.invocation_context #>> '{principal,suite-flow-version}' \
      JOIN catalog.execution_bundles AS bundle \
        ON bundle.tenant_id = r.tenant_id \
       AND bundle.execution_bundle_hash = r.execution_bundle_hash \
     WHERE r.artifact_lineage = 'draft' \
       AND r.admission_context_version = '0.1' \
       AND r.invocation_context ->> 'version' = '0.1' \
       AND r.invocation_context #>> '{principal,tenant-id}' = r.tenant_id \
       AND r.invocation_context #>> '{principal,environment}' = r.environment \
       AND r.invocation_context #>> '{principal,catalog-id}' = r.catalog_id \
       AND r.invocation_context #>> '{principal,catalog-version}' = \
             r.catalog_version::text \
       AND r.invocation_context #>> '{principal,run-id}' = r.run_id \
       AND r.invocation_context #>> '{principal,flow-id}' = r.flow_id \
       AND r.invocation_context #>> '{principal,flow-version}' = \
             r.flow_version::text \
) \
SELECT * FROM blocked_lineage \
UNION ALL SELECT * FROM draft_plan";

fn load_execution_plan(run_id: &str) -> Result<wamn_catalog::ExecutionPlanV2, String> {
    let rs = client::query(PINNED_ARTIFACT_SQL, &[text(run_id)]).map_err(|e| err_name(&e))?;
    if rs.rows.len() != 1 {
        return Err("run does not resolve to exactly one immutable execution plan".into());
    }
    let row = &rs.rows[0];
    let optional_string = |index: usize, name: &str| match row.get(index) {
        Some(SqlValue::Text(value)) | Some(SqlValue::Json(value)) => Ok(Some(value.as_str())),
        Some(SqlValue::Null) => Ok(None),
        other => Err(format!("{name} shape: {other:?}")),
    };
    match classify_pinned_artifact_lineage(
        optional_string(0, "runs.trigger_source")?,
        optional_string(1, "invocation_context.source.producer")?,
    )? {
        PinnedArtifactLineage::Release => {
            return Err(
                "published execution refuses until release and run plan pinning is installed"
                    .to_string(),
            );
        }
        PinnedArtifactLineage::Draft => {}
    }
    let exact_bytes = match row.get(2) {
        Some(SqlValue::Bytes(value)) => value,
        other => return Err(format!("execution_bundles.exact_bytes shape: {other:?}")),
    };
    let bundle_hash = optional_string(3, "runs.execution_bundle_hash")?
        .ok_or("admitted run omits execution bundle hash")?;
    let artifact_hash = optional_string(4, "validated_flow_drafts.draft_artifact_hash")?
        .ok_or("validated draft omits artifact hash")?;
    let plan = wamn_catalog::read_execution_plan(bundle_hash, exact_bytes)
        .map_err(|error| format!("execution plan verification: {error}"))?;
    if plan.header.root_artifact_hash != artifact_hash {
        return Err("execution plan root artifact differs from the validated draft".to_string());
    }
    Ok(plan)
}
/// Read the run's persisted `flow_version` — the version the dispatcher (or the
/// direct driver's own `open_run`) stamped when the run row was written.
/// `Some(v)` on a resume (the row exists); `None` for a fresh run whose row this
/// `execute` call is about to open.
fn load_persisted_version(run_id: &str) -> Result<Option<u32>, String> {
    let rs = client::query(
        "SELECT flow_version FROM runs WHERE run_id = $1",
        &[text(run_id)],
    )
    .map_err(|e| err_name(&e))?;
    match rs.rows.first().and_then(|r| r.first()) {
        Some(SqlValue::Int32(v)) => Ok(u32::try_from(*v).ok()),
        Some(SqlValue::Int64(v)) => Ok(u32::try_from(*v).ok()),
        _ => Ok(None),
    }
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
            text(wamn_run_state::RunStatus::Running.as_sql()),
            SqlValue::Null, // trigger_source: a direct driver, not a dispatcher
            jsonb(input),
        ],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Load a run's already-completed node executions in dispatch (`seq`) order — the
/// branch-aware reconstruction source. Only `success`/`error` rows are completed
/// steps; a `started` row is an outstanding node the walk re-dispatches.
/// An error-routed node was recorded as an emission on the `error` port, so
/// reconstruction needs no error taxonomy here.
fn load_completed(run_id: &str) -> Result<Vec<NodeRunRecord>, String> {
    let rs = client::query(&run_sql::select_completed_node_runs_sql(), &[text(run_id)])
        .map_err(|e| err_name(&e))?;
    let mut out = Vec::with_capacity(rs.rows.len());
    for row in &rs.rows {
        let node_id = match row.first() {
            Some(SqlValue::Text(s)) => s.clone(),
            other => return Err(format!("node_runs.local_node_id shape: {other:?}")),
        };
        let current_plan_hash = match row.get(1) {
            Some(SqlValue::Text(s)) => s.clone(),
            other => return Err(format!("node_runs.current_plan_hash shape: {other:?}")),
        };
        let occurrence = match row.get(2) {
            Some(SqlValue::Int32(n)) => *n as u32,
            Some(SqlValue::Int64(n)) => *n as u32,
            other => return Err(format!("node_runs.occurrence shape: {other:?}")),
        };
        let seq = match row.get(3) {
            Some(SqlValue::Int32(n)) => *n as u32,
            Some(SqlValue::Int64(n)) => *n as u32,
            other => return Err(format!("node_runs.seq shape: {other:?}")),
        };
        let port = match row.get(4) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => MAIN_PORT.to_string(),
        };
        // A JSON value round-trips as `Some`; a SQL NULL output_json (capture off
        // or oversized) is `None`, which reconstruction cannot replay — distinct
        // from a captured JSON `null` payload (Some(Value::Null)).
        let output = match row.get(5) {
            Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => Some(
                serde_json::from_str(s).map_err(|e| format!("node_runs.output_json parse: {e}"))?,
            ),
            _ => None,
        };
        let mut rec =
            NodeRunRecord::success(run_id, current_plan_hash, node_id, seq, port, Value::Null);
        rec.output = output;
        rec.occurrence = occurrence;
        out.push(rec);
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
/// after the node's effect commits. Idempotent by (run_id, frame_id, local_node_id, occurrence):
/// `occurrence` is the engine-computed visit ([`Dispatch::occurrence`]), so a
/// merge/loop node's Nth visit lands as its own row and ON CONFLICT dedupes only
/// a replay of the SAME visit (wamn-03m / R24).
#[expect(
    clippy::too_many_arguments,
    reason = "the checkpoint identity, output facts, capture mode, and context"
)]
fn record_node_run(
    run_id: &str,
    node_id: &str,
    occurrence: u32,
    seq: i32,
    port: &str,
    output: &Value,
    input: &Value,
    capture: CaptureMode,
    context: &Value,
) -> Result<(), String> {
    // Capture is applied before the jsonb write choke point.
    let (binds, _) = capture_binds(capture, output, input);
    let [out_j, in_j, size, hash] = binds;
    let txn = client::begin().map_err(|error| err_name(&error))?;
    txn.execute(
        &run_sql::insert_node_run_success_sql(),
        &[
            text(run_id),
            text(node_id),
            int32(occurrence as i32),
            int32(seq),
            text(port),
            out_j,
            in_j,
            size,
            hash,
        ],
    )
    .map_err(|e| err_name(&e))?;
    txn.execute(
        &run_sql::update_run_context_sql(),
        &[text(run_id), text(context.to_string())],
    )
    .map_err(|error| err_name(&error))?;
    txn.commit().map_err(|error| err_name(&error))
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
// Retry cursor + wasi:http egress
// ---------------------------------------------------------------------------

/// Persist the in-flight retry cursor — the retrying node + the attempt the next
/// dispatch runs as — in the run's `state_json`, so the retry budget survives
/// queue-wait→reclaim→reconstruct (R32): the outstanding node re-enters carrying its
/// attempt instead of resetting to 0 (reconstruction replays only COMPLETED
/// node_runs, so a mid-retry node otherwise loses its count). The write preserves
/// durable run context, and `restore_retry` re-validates the cursor against the
/// reconstructed frontier.
fn save_retry(
    run_id: &str,
    node: &str,
    attempt: u32,
    delay_ms: u64,
    throttle: Option<&ThrottleKey>,
) -> Result<(), String> {
    // wamn-2jkm.66: persist the shared-throttle key alongside (node, attempt) so
    // a `rate-limited` retry's CROSS-run gate identity survives the queue wait.
    // `delay-ms` records the same engine-produced backoff that the production
    // queue materializes in `available_at`, allowing scenarios to replay it
    // without treating that database timestamp as logical time. Absent key =>
    // no `throttle` sub-object, so an ordinary retryable retry round-trips None.
    let mut state = checkpoint_object(load_checkpoint(run_id)?);
    state.insert(
        "retry".to_string(),
        retry_state(node, attempt, delay_ms, throttle)["retry"].clone(),
    );
    client::execute(
        &run_sql::update_run_state_sql(),
        &[text(run_id), text(Value::Object(state).to_string())],
    )
    .map_err(|e| err_name(&e))?;
    Ok(())
}

/// The minimum durable schedule needed to replay a retry without consulting
/// database wall time. Existing readers ignore `delay-ms`, so adding it is
/// backward-compatible; [`load_retry`] still accepts legacy state that has only
/// `(node, attempt[, throttle])`.
fn retry_state(node: &str, attempt: u32, delay_ms: u64, throttle: Option<&ThrottleKey>) -> Value {
    let mut retry = json!({
        "node": node,
        "attempt": attempt,
        "delay-ms": delay_ms,
    });
    if let Some(t) = throttle {
        retry["throttle"] = json!({
            "node-type": t.node_type,
            "credential": t.credential,
            "host": t.host,
        });
    }
    json!({ "retry": retry })
}

/// Load a persisted in-flight retry cursor `(node, attempt, throttle)` from
/// `state_json`, if any — the reconstruction seam feeds it to
/// [`Plan::restore_retry`]. The `throttle` sub-object (wamn-2jkm.66) restores the
/// shared-throttle key a `rate-limited` retry waited with; it is absent for a
/// plain retryable retry, which restores with no key.
fn load_retry(run_id: &str) -> Result<Option<(String, u32, Option<ThrottleKey>)>, String> {
    let v = load_checkpoint(run_id)?;
    Ok(parse_retry(&v))
}

fn load_checkpoint(run_id: &str) -> Result<Value, String> {
    let rs = client::query(&run_sql::select_run_state_sql(), &[text(run_id)])
        .map_err(|e| err_name(&e))?;
    let raw = match rs.rows.first().and_then(|row| row.first()) {
        Some(SqlValue::Text(raw)) | Some(SqlValue::Json(raw)) => raw,
        _ => return Ok(json!({})),
    };
    serde_json::from_str(raw).map_err(|error| format!("state_json parse: {error}"))
}

fn checkpoint_object(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn load_context(run_id: &str) -> Result<Value, String> {
    let checkpoint = load_checkpoint(run_id)?;
    wamn_run_state::context::read(Some(&checkpoint)).map_err(|error| error.to_string())
}

fn parse_retry(v: &Value) -> Option<(String, u32, Option<ThrottleKey>)> {
    let retry = v.get("retry")?;
    match (
        retry.get("node").and_then(|n| n.as_str()),
        retry.get("attempt").and_then(|a| a.as_u64()),
    ) {
        (Some(node), Some(attempt)) => {
            let throttle = retry.get("throttle").map(|t| {
                ThrottleKey::new(
                    t.get("node-type").and_then(|v| v.as_str()).unwrap_or(""),
                    t.get("credential")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    t.get("host").and_then(|v| v.as_str()).map(String::from),
                )
            });
            Some((node.to_string(), attempt as u32, throttle))
        }
        _ => None,
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
// Standard node library glue (5.3): the wamn-standard-nodes vocabulary dispatches
// through the local capability facade over this component's real imports.
// Node-owned HTTP leaves through the trusted `wamn:runner/http-effect` import;
// the separate `wasi:http` import remains for the legacy S6 `http-call` fixture.
// ---------------------------------------------------------------------------

/// Whether the engine will ROUTE this error emission down the node's error
/// edge (versus scheduling a retry) — the exact policy
/// computation `Plan::apply` makes, mirrored so a recorded error row always
/// matches the walk the engine actually took. Terminal / invalid-input route
/// immediately; retryable / rate-limited route only once the retry budget is spent.
fn will_error_route(err: &NodeError, d: &Dispatch) -> bool {
    match err {
        NodeError::Terminal(_) | NodeError::InvalidInput(_) => true,
        NodeError::Retryable(_) | NodeError::RateLimited(_) => {
            !RetryPolicy::from_config(&d.config).may_retry(d.attempt)
        }
    }
}

/// Record an error-ROUTED node as an emission on the `error` port carrying
/// the same `{"error": {...}}` payload the engine routes — exactly what 5.7
/// reconstruction replays (poc-webhook-f1's shape verbatim); the taxonomy
/// lands in `error_kind`/`error_detail` for the run history.
fn record_error(
    run_id: &str,
    node_id: &str,
    occurrence: u32,
    seq: i32,
    err: &NodeError,
    input: &Value,
    capture: CaptureMode,
) -> Result<(), String> {
    let (kind, [out_j, in_j, detail, size, hash]) = error_capture_binds(capture, err, input);
    client::execute(
        &run_sql::insert_node_run_error_sql(),
        &[
            text(run_id),
            text(node_id),
            int32(occurrence as i32),
            int32(seq),
            out_j,
            in_j,
            text(kind),
            detail,
            size,
            hash,
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

/// Bind an error row under the admitted capture policy.
///
/// The typed failure kind is always retained. `full` stores scrubbed payload
/// facts and taxonomy detail; `off` binds SQL NULL for every payload-bearing
/// field, including taxonomy detail because it can echo node I/O.
fn error_capture_binds(
    capture: CaptureMode,
    error: &NodeError,
    input: &Value,
) -> (&'static str, [SqlValue; 5]) {
    let (kind, output, mut detail) = error_row_values(error);
    let (binds, capture_detail) = capture_binds(capture, &output, input);
    let [output, input, size, hash] = binds;
    let detail = if capture_detail {
        wamn_run_state::capture::scrub(&mut detail);
        jsonb(&detail)
    } else {
        SqlValue::Null
    };
    (kind, [output, input, detail, size, hash])
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

fn fail_kind_sql(kind: &ExecutionFailureKind) -> &'static str {
    wamn_run_state::FailKind::from(*kind).as_sql()
}

// ---------------------------------------------------------------------------
// Executor: drive the wamn-runner engine over the loaded flow
// ---------------------------------------------------------------------------

/// The outcome of one `execute` call. The retained top-level ABI uses
/// `outcome`: 0 = completed, 1 = queue-waiting.
struct RunOutcome {
    version: u32,
    outcome: u32,
    http_status: u32,
}

/// The implementation family selected for one node.
///
/// Both dispatch and the side-effect-free flow check go through this resolver,
/// so the compiled runner has one authoritative support decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolvedNode {
    WebhookIn,
    LegacyTransform,
    LegacyConditional,
    PgWrite,
    HttpCall,
    InvokeFlow,
    Standard,
}

fn resolve_node(node_type: &str, config: &Value) -> Option<ResolvedNode> {
    match node_type {
        "webhook-in" => Some(ResolvedNode::WebhookIn),
        "transform" if config.get("expression").is_none() => Some(ResolvedNode::LegacyTransform),
        "conditional" if config.get("expression").is_none() => {
            Some(ResolvedNode::LegacyConditional)
        }
        "pg-write" => Some(ResolvedNode::PgWrite),
        "http-call" => Some(ResolvedNode::HttpCall),
        "invoke-flow" => Some(ResolvedNode::InvokeFlow),
        node_type if wamn_nodes::is_standard(node_type) => Some(ResolvedNode::Standard),
        _ => None,
    }
}

/// Output ports for the legacy S3/S6 fixture nodes compiled inside this
/// component.
///
/// Production execution resolves interfaces from the release's pinned artifact
/// bundle ([`execute_claimed`]); this map is the fixture stand-in for the direct
/// `run`/`run-s6`/`dispatch-bench` paths, whose flow documents carry no pinned
/// resolved public interfaces. Every one of these legacy node types emits exactly one
/// completion on `main` — the shape a real resolution supplies for them — so a
/// missing entry is not a narrower graph but an unvalidatable one
/// (`unresolved-output-ports`, and the request-region checks silently skip the
/// node).
fn s3_fixture_interfaces() -> ResolvedInterfaces {
    ResolvedInterfaces::from([
        ("conditional".to_string(), vec![MAIN_PORT.to_string()]),
        ("http-call".to_string(), vec![MAIN_PORT.to_string()]),
        ("pg-write".to_string(), vec![MAIN_PORT.to_string()]),
        ("transform".to_string(), vec![MAIN_PORT.to_string()]),
    ])
}

fn check_flow(flow_json: &str) -> Result<Vec<String>, String> {
    let flow = Flow::from_json(flow_json).map_err(|error| format!("check flow: {error}"))?;
    let mut unsupported: Vec<String> = flow
        .nodes
        .iter()
        .filter(|node| resolve_node(&node.node_type, &node.config).is_none())
        .map(|node| node.node_type.clone())
        .collect();
    unsupported.sort_unstable();
    unsupported.dedup();
    Ok(unsupported)
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
) -> Result<NodeOutcome, String> {
    let outcome = dispatch_node_unvalidated(d, run_id, flow, kill_after_write, http_status)?;
    validate_dispatched_action(d, &outcome)?;
    Ok(outcome)
}

fn validate_dispatched_action(dispatch: &Dispatch, outcome: &NodeOutcome) -> Result<(), String> {
    validate_request_outcome(dispatch, outcome).map_err(|error| error.to_string())?;
    validate_cron_outcome(dispatch, outcome).map_err(|error| error.to_string())?;
    validate_event_outcome(dispatch, outcome).map_err(|error| error.to_string())?;
    validate_fail_outcome(dispatch, outcome).map_err(|error| error.to_string())?;
    Ok(())
}

fn dispatch_node_unvalidated(
    d: &Dispatch,
    run_id: &str,
    flow: &Flow,
    kill_after_write: bool,
    http_status: &mut u32,
) -> Result<NodeOutcome, String> {
    let resolved = resolve_node(&d.node_type, &d.config)
        .ok_or_else(|| format!("unknown node type: {}", d.node_type))?;
    match resolved {
        // The trigger payload already sits in the node's input.
        ResolvedNode::WebhookIn => Ok(NodeOutcome::ok(d.payload.clone())),
        // An `expression` config routes to the standard library's JMESPath
        // transform/conditional below; the S3 fixture shapes (`op`/`min-len`)
        // keep their legacy semantics byte-identical.
        ResolvedNode::LegacyTransform => {
            let op = d
                .config
                .get("op")
                .and_then(|v| v.as_str())
                .unwrap_or("upper");
            let out = match op {
                "reverse" => value_str(&d.payload).chars().rev().collect::<String>(),
                _ => value_str(&d.payload).to_uppercase(),
            };
            Ok(NodeOutcome::ok(Value::String(out)))
        }
        // Records a branch decision but keeps the fixture's linear main path;
        // true branching is exercised in the wamn-runner / wamn-run-state tests.
        ResolvedNode::LegacyConditional => Ok(NodeOutcome::ok(d.payload.clone())),
        ResolvedNode::PgWrite => {
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
            Ok(NodeOutcome::ok(d.payload.clone()))
        }
        ResolvedNode::HttpCall => {
            let url = d.config.get("url").and_then(|v| v.as_str()).unwrap_or("");
            *http_status = http_get(url);
            Ok(NodeOutcome::ok(d.payload.clone()))
        }
        ResolvedNode::InvokeFlow => {
            Err("invoke-flow is handled by the claimed-run child runtime".to_string())
        }
        // The standard node library (5.3): everything the library ships
        // dispatches through the capability policy table over this
        // component's real imports. A NodeError feeds the engine, which
        // decides retry-vs-error-path-vs-fail mechanically from the variant.
        ResolvedNode::Standard => {
            let node_type = d.node_type.as_str();
            let run_ctx = sdk::RunContext {
                run_id,
                flow_id: &flow.flow_id,
                flow_version: flow.version,
                node_id: &d.node,
                connection: d.connection.as_deref(),
                attempt: d.attempt,
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
                context: &d.context,
            };
            let mut ctx = CapsCtx {
                raw_sql: wamn_nodes::required_capabilities(node_type)
                    .is_some_and(|capabilities| capabilities.contains(&sdk::Capability::RawSql))
                    && raw_sql_enabled(),
                // The claimed-run interpreter is hard-refused until the
                // authoritative frame-fact adapter lands. Supplying any
                // root/current plan or source-artifact hash here would invent
                // authority, so no trusted HTTP effect context is exposed yet.
                http_effect: None,
            };
            let granted = wamn_nodes::granted_for(sdk::NodeCtx::raw_sql_enabled(&ctx));
            Ok(
                match wamn_nodes::dispatch(node_type, granted, &mut ctx, &run_ctx, &d.payload) {
                    Ok(em) => match em.ctx {
                        Some(context) => NodeOutcome::ok_with_context(em.payload, em.port, context),
                        None => NodeOutcome::ok_on(em.payload, em.port),
                    },
                    Err(e) => NodeOutcome::Error(e),
                },
            )
        }
    }
}

/// fqg.11: declare this run's egress allowlist (the flow's declared
/// `allowed-hosts`) to the host BEFORE dispatching any node. The host
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

/// l5i9.17: the event-chain thread. The dispatch SQL synthesizes transient
/// `causation: {run, root, depth}` from the run row's trusted lineage columns;
/// it is never persisted in author-visible business input. Declaring that
/// root/depth here makes this run's writes emit the incremented stamp, so the
/// next hop's events carry it and the loop budget is real. `run` is always the
/// claimed run id (never read from input); missing or malformed causation falls
/// back to self-root depth 0 for non-event triggers.
fn declare_run_context_from(run_id: &str, input: &Value) {
    let (root, depth) = run_context_from(run_id, input);
    declare_run_context_at(run_id, root, depth);
}

fn run_context_from(run_id: &str, input: &Value) -> (String, u32) {
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
    (root, depth)
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

// ---------------------------------------------------------------------------
// Run-path observability (wamn-yf3): a few structured wasi:logging records per
// run — node completion, node error CLASS, and run completion — enriched
// host-side (unspoofable tenant/project) with the run's flow/run/node/seq and a
// per-run W3C traceparent so a log joins its run's trace. NEVER node output
// payloads or credential/secret material: identifiers + outcome + error class
// only. A handful of records per run (not per row), so it never dominates a walk.
// ---------------------------------------------------------------------------

use wasi::logging::logging::{self as wasi_logging, Level as LogLevel};

/// The `node` value on a run-SCOPE (not per-node) record, so every emitted
/// record still carries a non-empty `node` for the enrichment gate.
const RUN_SCOPE_NODE: &str = "<run>";

/// FNV-1a 64 — a tiny, dependency-free, deterministic hash used ONLY to derive a
/// per-run trace id (not security-sensitive).
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Mint this run's W3C `traceparent` deterministically from the `run_id`, so
/// every record of one run shares a trace_id (its logs correlate) and the value
/// is stable across the run's re-claims. Until the persisted per-run traceparent
/// column lands (wamn-fl3) this IS the run's trace identity; when it lands, read
/// it from the runs row instead. Both ids are forced non-zero so the value is a
/// valid (non-INVALID) W3C traceparent.
fn run_traceparent(run_id: &str) -> String {
    let hi = fnv1a64(run_id.as_bytes());
    let lo = fnv1a64(&[run_id.as_bytes(), b"\x01"].concat()) | 1;
    let span = fnv1a64(&[b"span:", run_id.as_bytes()].concat()) | 1;
    format!("00-{hi:016x}{lo:016x}-{span:016x}-01")
}

/// The wasi:logging `context` JSON the plugin enriches: the run's identifiers +
/// its traceparent. serde_json escapes every value. The key SET is a contract
/// with the plugin's `ParsedContext`.
fn log_context(flow_id: &str, run_id: &str, node: &str, seq: i32, traceparent: &str) -> String {
    json!({
        "flow": flow_id,
        "run": run_id,
        "node": node,
        "seq": seq,
        "traceparent": traceparent,
    })
    .to_string()
}

/// Emit a node-completion record (identifiers + the outcome port only).
fn emit_node_complete(flow_id: &str, run_id: &str, node: &str, seq: i32, tp: &str, port: &str) {
    let ctx = log_context(flow_id, run_id, node, seq, tp);
    wasi_logging::log(LogLevel::Info, &ctx, &format!("node completed -> {port}"));
}

/// Emit a node-failure record carrying ONLY the error CLASS — never the message,
/// which could echo node output.
fn emit_node_error(flow_id: &str, run_id: &str, node: &str, seq: i32, tp: &str, class: &str) {
    let ctx = log_context(flow_id, run_id, node, seq, tp);
    wasi_logging::log(LogLevel::Error, &ctx, &format!("node failed: {class}"));
}

/// Emit the run's terminal record (completed / failed-class) at run scope.
fn emit_run_end(flow_id: &str, run_id: &str, seq: i32, tp: &str, outcome: &str) {
    let ctx = log_context(flow_id, run_id, RUN_SCOPE_NODE, seq, tp);
    wasi_logging::log(LogLevel::Info, &ctx, &format!("run {outcome}"));
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

    let input = Value::String(payload.to_string());
    // wamn-cox: a resume pins the run's PERSISTED `flow_version` — the version
    // recorded when the run first opened — so a flow edited mid-run cannot make
    // reconstruction diverge from the graph the run started on. A fresh run
    // (`None`, no row yet) loads the ACTIVE version and `open_run` stamps it; a
    // resume (`Some(v)`) loads exactly that version.
    let flow = match load_persisted_version(run_id)? {
        Some(v) => load_flow_at(flow_id, v)?,
        None => {
            let flow = load_active_flow(flow_id)?;
            open_run(run_id, flow_id, flow.version, &input)?;
            flow
        }
    };
    declare_run_egress(&flow);
    let interfaces = s3_fixture_interfaces();
    let plan = Plan::compile(&flow, &interfaces).map_err(|e| e.to_string())?;
    let version = plan.version();

    // Reconstruct the frontier from what already completed (empty on a fresh run
    // => a plain start); the driver continues from there, re-dispatching only
    // outstanding nodes. `seq` continues past the completed count.
    let completed = load_completed(run_id)?;
    let mut next_seq = completed.len() as i32;
    let run_rec = RunRecord::new(run_id, flow_id, version, input);
    let mut st = wamn_run_state::reconstruct_with_context(
        &plan,
        &run_rec,
        &completed,
        load_context(run_id)?,
    )
    .map_err(|e| e.to_string())?;
    // R32: restore an in-flight retry queue-waiting from a prior invocation — the
    // outstanding node re-enters carrying its persisted attempt (the queue served
    // the backoff) so the retry budget advances instead of resetting to 0.
    if let Some((node, attempt, throttle)) = load_retry(run_id)? {
        plan.restore_retry(&mut st, &node, attempt, throttle);
    }
    let mut http_status: u32 = 0;

    loop {
        // now_ms = 0: the queue's available_at is the retry clock (R32), so a
        // scheduled retry re-enters DUE after its queue wait.
        match plan.next(&mut st, 0) {
            Step::Done(ExecutionStatus::Completed) => {
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
            // to the queue layer (`run_queue.available_at` / `queue::park_sql`) —
            // persist the attempt and return outcome=1, so the next invocation
            // restores the attempt (DUE now, the queue wait served the backoff) and re-dispatches;
            // the budget advances until success, error-route, or RetryExhausted.
            Step::Wait {
                node,
                until_ms,
                attempt,
                throttle,
            } => {
                save_retry(run_id, &node, attempt, until_ms, throttle.as_ref())?;
                return Ok(RunOutcome {
                    version,
                    outcome: 1,
                    http_status,
                });
            }
            Step::Reserved(step) => {
                if let ReservedStep::Fail { status, .. } = &step {
                    http_status = u32::from(*status);
                }
                record_node_run(
                    run_id,
                    step.node(),
                    step.occurrence(),
                    next_seq,
                    MAIN_PORT,
                    step.payload(),
                    step.payload(),
                    CaptureMode::Off,
                    st.context(),
                )?;
                next_seq += 1;
                plan.apply_reserved(&mut st, &step)
                    .map_err(|error| error.to_string())?;
            }
            Step::Dispatch(d) => {
                let outcome = dispatch_node(&d, run_id, &flow, kill_after_write, &mut http_status)?;
                if d.node_type == "fail" {
                    let config: FailConfig =
                        serde_json::from_value(d.config.clone()).expect("validated fail config");
                    http_status = u32::from(config.status);
                    let NodeOutcome::Error(error) = &outcome else {
                        unreachable!("validated fail outcome is terminal")
                    };
                    record_error(
                        run_id,
                        &d.node,
                        d.occurrence,
                        next_seq,
                        error,
                        &d.payload,
                        CaptureMode::Off,
                    )?;
                    next_seq += 1;
                    plan.apply(&mut st, &d, outcome, 0)
                        .map_err(|error| error.to_string())?;
                    continue;
                }
                match &outcome {
                    // Record the completed node (after its effect
                    // commits) so a later invocation reconstructs past it.
                    NodeOutcome::Success {
                        payload,
                        port,
                        context,
                    } => {
                        if d.node_type == "respond" {
                            http_status = u32::from(
                                wamn_nodes::respond::status_for(&d.config)
                                    .expect("validated respond config has an HTTP status"),
                            );
                        }
                        record_node_run(
                            run_id,
                            &d.node,
                            d.occurrence,
                            next_seq,
                            port,
                            payload,
                            &d.payload,
                            CaptureMode::Off,
                            context.as_ref().unwrap_or(&d.context),
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
                        record_error(
                            run_id,
                            &d.node,
                            d.occurrence,
                            next_seq,
                            err,
                            &d.payload,
                            CaptureMode::Off,
                        )?;
                        next_seq += 1;
                    }
                    NodeOutcome::Error(_) => {}
                }
                plan.apply(&mut st, &d, outcome, 0)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Guest-side queue claim (fqg.4): claim -> drive (heartbeat) -> settle queue.
// The production dispatch path, guest-side. The runner reads its OWN work from
// run_queue instead of being handed a run_id — the same builders the host-side
// dispatcher/claimers use (wamn-run-state::queue), called through wamn:postgres.
// ---------------------------------------------------------------------------

/// The claim-path result: `outcome` (0 = completed, 1 = queue-waiting, 2 = failed)
/// plus the wake delay to queue-park the row by when `outcome == 1`, and — for
/// `outcome == 2` — the failure verdict the dead-letter marker records
/// (wamn-v8cv; the structured fail_kind/fail_node/fail_reason stay on `runs`).
struct ClaimOutcome {
    outcome: u32,
    park_ms: u64,
    fail_reason: Option<String>,
    already_settled: bool,
}

const DEFAULT_CHILD_DEPTH_LIMIT: i32 = 8;
const DEFAULT_CHILD_FANOUT_LIMIT: i64 = 64;

enum ChildInvocation {
    Parked,
    Released(NodeOutcome),
}

fn invoke_actor_mode(mode: InvokeActorMode) -> &'static str {
    match mode {
        InvokeActorMode::Inherit => "inherit",
        InvokeActorMode::Service => "service",
        InvokeActorMode::Attenuate => "attenuate",
    }
}

fn child_outcome(outcome: StoredCallerOutcome) -> NodeOutcome {
    if outcome.kind == "responded" {
        return NodeOutcome::ok(outcome.body);
    }
    let error = outcome.body.get("error").unwrap_or(&outcome.body);
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("callee-failed");
    let message = error.get("message").and_then(Value::as_str).unwrap_or(code);
    NodeOutcome::Error(NodeError::Terminal(ErrorDetail {
        message: message.to_string(),
        code: Some(code.to_string()),
        data: Some(outcome.body),
    }))
}

fn invoke_child(
    run_id: &str,
    dispatch: &Dispatch,
    owner: &str,
    lease_generation: i64,
) -> Result<ChildInvocation, String> {
    let config: InvokeFlowConfig = serde_json::from_value(dispatch.config.clone())
        .map_err(|error| format!("invoke-flow config: {error}"))?;
    let child_run_id = format!("child:{run_id}:{}:{}", dispatch.node, dispatch.occurrence);
    let response = client::query(
        &create_or_recover_child_sql(),
        &[
            text(run_id),
            text(run_id),
            text(owner),
            int64(lease_generation),
            text(&dispatch.node),
            int32(dispatch.occurrence as i32),
            text(child_run_id),
            text(config.attachment_id),
            text(config.flow_id),
            text(invoke_actor_mode(config.actor_mode)),
            jsonb(&dispatch.payload),
            text(env!("CARGO_PKG_VERSION")),
            int32(DEFAULT_CHILD_DEPTH_LIMIT),
            int64(DEFAULT_CHILD_FANOUT_LIMIT),
            SqlValue::Null,
            text("blocking"),
        ],
    )
    .map_err(|error| err_name(&error))?;
    let row = response
        .rows
        .first()
        .ok_or("invoke-flow transition returned no result row")?;
    let code = match row.first() {
        Some(SqlValue::Text(value)) => value.as_str(),
        other => return Err(format!("invoke-flow result shape: {other:?}")),
    };
    let run_status = match row.get(1) {
        Some(SqlValue::Text(value)) => value.as_str(),
        Some(SqlValue::Null) => "",
        other => return Err(format!("invoke-flow run status shape: {other:?}")),
    };
    let child_run_id = match row.get(2) {
        Some(SqlValue::Text(value)) => Some(value.clone()),
        Some(SqlValue::Null) => None,
        other => return Err(format!("invoke-flow child id shape: {other:?}")),
    };
    let wait_generation = match row.get(3) {
        Some(SqlValue::Int64(value)) => Some(*value),
        Some(SqlValue::Int32(value)) => Some(i64::from(*value)),
        Some(SqlValue::Null) => None,
        other => return Err(format!("invoke-flow wait generation shape: {other:?}")),
    };
    let kind = match row.get(4) {
        Some(SqlValue::Text(value)) => Some(value.clone()),
        Some(SqlValue::Null) => None,
        other => return Err(format!("invoke-flow outcome kind shape: {other:?}")),
    };
    let body = match row.get(5) {
        Some(SqlValue::Text(value)) | Some(SqlValue::Json(value)) => Some(
            serde_json::from_str(value)
                .map_err(|error| format!("invoke-flow outcome parse: {error}"))?,
        ),
        Some(SqlValue::Null) => None,
        other => return Err(format!("invoke-flow outcome body shape: {other:?}")),
    };
    let http_status = match row.get(6) {
        Some(SqlValue::Int32(value)) => u16::try_from(*value).ok(),
        Some(SqlValue::Int64(value)) => u16::try_from(*value).ok(),
        Some(SqlValue::Null) => None,
        other => return Err(format!("invoke-flow HTTP status shape: {other:?}")),
    };
    let release_node_id = match row.get(7) {
        Some(SqlValue::Text(value)) => Some(value.clone()),
        Some(SqlValue::Null) => None,
        other => return Err(format!("invoke-flow release node shape: {other:?}")),
    };
    let hash = match row.get(8) {
        Some(SqlValue::Text(value)) => Some(value.clone()),
        Some(SqlValue::Null) => None,
        other => return Err(format!("invoke-flow outcome hash shape: {other:?}")),
    };
    match ChildCreateResult::from_parts(
        code,
        run_status,
        child_run_id,
        wait_generation,
        kind,
        body,
        http_status,
        release_node_id,
        hash,
    )
    .ok_or_else(|| format!("unknown invoke-flow result: {code}"))?
    {
        ChildCreateResult::Created { .. } | ChildCreateResult::Recovered { .. } => {
            Ok(ChildInvocation::Parked)
        }
        ChildCreateResult::Released { outcome, .. } => {
            Ok(ChildInvocation::Released(child_outcome(outcome)))
        }
        ChildCreateResult::FenceLost => Err("invoke-flow refused: fence-lost".to_string()),
        other => Err(format!("invoke-flow refused: {other:?}")),
    }
}

fn checkpoint_child_outcome(
    run_id: &str,
    dispatch: &Dispatch,
    seq: i32,
    outcome: &NodeOutcome,
    capture: CaptureMode,
    ttl_ms: i64,
    owner: &str,
) -> Result<(), String> {
    match outcome {
        NodeOutcome::Success { payload, port, .. } => {
            let (binds, _) = capture_binds(capture, payload, &dispatch.payload);
            let [out_j, in_j, size, hash] = binds;
            client::execute(
                &record_success_and_renew_sql(),
                &[
                    text(run_id),
                    text(&dispatch.node),
                    int32(dispatch.occurrence as i32),
                    int32(seq),
                    text(port),
                    out_j,
                    in_j,
                    size,
                    hash,
                    int64(ttl_ms),
                    text(owner),
                ],
            )
            .map_err(|error| err_name(&error))?;
        }
        NodeOutcome::Error(error) => {
            let (kind, [out_j, in_j, detail, size, hash]) =
                error_capture_binds(capture, error, &dispatch.payload);
            client::execute(
                &record_error_and_renew_sql(),
                &[
                    text(run_id),
                    text(&dispatch.node),
                    int32(dispatch.occurrence as i32),
                    int32(seq),
                    out_j,
                    in_j,
                    text(kind),
                    detail,
                    size,
                    hash,
                    int64(ttl_ms),
                    text(owner),
                ],
            )
            .map_err(|error| err_name(&error))?;
        }
    }
    Ok(())
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
    /// The run's PERSISTED `flow_version` (wamn-cox) — the version the run was
    /// dispatched under, the plan-cache probe. `None` only if the column is
    /// somehow unreadable (the flow load then reports it).
    flow_version: Option<u32>,
    capture_mode: CaptureMode,
    lease_generation: i64,
}

/// Claim ONE currently-claimable **unpartitioned** run for `owner` and return
/// its dispatch inputs — the single [`claim_dispatch_sql`] statement that also
/// flips the run `running` and reads the run's persisted flow version (what the
/// split path spent three round trips on). Returns None when the queue is
/// drained. Partitioned runs stay on the per-partition ownership path
/// (fqg.1/fqg.9).
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
    let flow_version = match row.get(3) {
        Some(SqlValue::Int32(v)) => u32::try_from(*v).ok(),
        Some(SqlValue::Int64(v)) => u32::try_from(*v).ok(),
        _ => None,
    };
    let capture_mode = match row.get(4) {
        Some(SqlValue::Text(value)) => CaptureMode::from_sql(value)
            .ok_or_else(|| format!("unknown runs.capture_mode: {value}"))?,
        other => return Err(format!("runs.capture_mode shape: {other:?}")),
    };
    let lease_generation = match row.get(5) {
        Some(SqlValue::Int64(value)) => *value,
        Some(SqlValue::Int32(value)) => i64::from(*value),
        other => return Err(format!("claim lease_generation shape: {other:?}")),
    };
    Ok(Some(ClaimedRun {
        run_id,
        flow_id,
        input,
        flow_version,
        capture_mode,
        lease_generation,
    }))
}

#[expect(
    clippy::too_many_arguments,
    reason = "attempt completion, capture, context, and fence form one transition"
)]
fn complete_attempt_success(
    run_id: &str,
    dispatch: &Dispatch,
    port: &str,
    output: &Value,
    capture: CaptureMode,
    context: &Value,
    ttl_ms: i64,
    owner: &str,
    lease_generation: i64,
) -> Result<(), String> {
    let (binds, _) = capture_binds(capture, output, &dispatch.payload);
    let [out_j, in_j, size, hash] = binds;
    let response = client::query(
        &complete_attempt_success_sql(),
        &[
            text(run_id),
            text(run_id),
            text(owner),
            int64(lease_generation),
            text(&dispatch.node),
            int32(dispatch.occurrence as i32),
            text(port),
            out_j,
            in_j,
            size,
            hash,
            text(context.to_string()),
            int64(ttl_ms),
        ],
    )
    .map_err(|error| err_name(&error))?;
    decode_attempt_completion(&response.rows)
}

fn decode_attempt_completion(rows: &[Vec<SqlValue>]) -> Result<(), String> {
    let row = rows.first().ok_or("attempt completion returned no row")?;
    let code = match row.first() {
        Some(SqlValue::Text(code)) => code.as_str(),
        other => return Err(format!("attempt completion result shape: {other:?}")),
    };
    let status = match row.get(1) {
        Some(SqlValue::Text(status)) => status.as_str(),
        Some(SqlValue::Null) => "",
        other => return Err(format!("attempt completion status shape: {other:?}")),
    };
    match CheckpointResult::from_parts(code, status)
        .ok_or_else(|| format!("unknown attempt completion result: {code}"))?
    {
        CheckpointResult::Applied => Ok(()),
        CheckpointResult::FenceLost => Err("attempt completion refused: fence-lost".to_string()),
        other => Err(format!("attempt completion refused: {other:?}")),
    }
}

/// The error-routed twin of [`record_node_run_and_renew`].
#[expect(
    clippy::too_many_arguments,
    reason = "the error row, capture facts, and queue fence form one transition"
)]
fn record_error_and_renew(
    run_id: &str,
    node_id: &str,
    occurrence: u32,
    _seq: i32,
    err: &NodeError,
    input: &Value,
    capture: CaptureMode,
    ttl_ms: i64,
    owner: &str,
    lease_generation: i64,
) -> Result<(), String> {
    let (kind, [out_j, in_j, detail, size, hash]) = error_capture_binds(capture, err, input);
    let response = client::query(
        &complete_attempt_error_sql(),
        &[
            text(run_id),
            text(run_id),
            text(owner),
            int64(lease_generation),
            text(node_id),
            int32(occurrence as i32),
            out_j,
            in_j,
            text(kind),
            detail,
            size,
            hash,
            int64(ttl_ms),
        ],
    )
    .map_err(|e| err_name(&e))?;
    decode_attempt_completion(&response.rows)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the exact caller outcome and queue fence form one release transition"
)]
fn release_caller_in_transaction(
    txn: &client::Transaction,
    run_id: &str,
    kind: &str,
    body: &Value,
    status: u16,
    node: &str,
    owner: &str,
    lease_generation: i64,
) -> Result<(), String> {
    let release_hash = canonical_json_sha256(body);
    let parent = txn
        .query(
            "SELECT c.parent_run_id, c.parent_node_id, c.parent_occurrence, p.wait_generation \
               FROM runs AS c LEFT JOIN runs AS p \
                 ON p.tenant_id = c.tenant_id AND p.run_id = c.parent_run_id \
              WHERE c.tenant_id = current_setting('app.tenant', true) AND c.run_id = $1",
            &[text(run_id)],
        )
        .map_err(|error| err_name(&error))?
        .rows
        .first()
        .and_then(|row| match row.as_slice() {
            [
                SqlValue::Text(parent_run_id),
                SqlValue::Text(parent_node_id),
                SqlValue::Int32(parent_occurrence),
                SqlValue::Int64(wait_generation),
            ] => Some((
                parent_run_id.clone(),
                parent_node_id.clone(),
                *parent_occurrence,
                *wait_generation,
            )),
            _ => None,
        });
    let response = txn
        .query(
            &if parent.is_some() {
                release_child_sql()
            } else {
                release_caller_sql()
            },
            &match &parent {
                Some((parent_run_id, parent_node_id, parent_occurrence, wait_generation)) => vec![
                    text(run_id),
                    text(run_id),
                    text(owner),
                    int64(lease_generation),
                    text(kind),
                    jsonb(body),
                    int32(i32::from(status)),
                    text(node),
                    text(&release_hash),
                    text(parent_run_id),
                    text(parent_node_id),
                    int32(*parent_occurrence),
                    int64(*wait_generation),
                ],
                None => vec![
                    text(run_id),
                    text(run_id),
                    text(owner),
                    int64(lease_generation),
                    text(kind),
                    jsonb(body),
                    int32(i32::from(status)),
                    text(node),
                    text(&release_hash),
                ],
            },
        )
        .map_err(|error| err_name(&error))?;
    let row = response
        .rows
        .first()
        .ok_or("caller release returned no result row")?;
    let code = match row.first() {
        Some(SqlValue::Text(value)) => value.as_str(),
        other => return Err(format!("caller release result shape: {other:?}")),
    };
    let run_status = match row.get(1) {
        Some(SqlValue::Text(value)) => value.as_str(),
        Some(SqlValue::Null) => "",
        other => return Err(format!("caller release run status shape: {other:?}")),
    };
    let stored_kind = match row.get(2) {
        Some(SqlValue::Text(value)) => Some(value.clone()),
        Some(SqlValue::Null) => None,
        other => return Err(format!("caller release kind shape: {other:?}")),
    };
    let stored_body = match row.get(3) {
        Some(SqlValue::Text(value)) | Some(SqlValue::Json(value)) => Some(
            serde_json::from_str::<Value>(value)
                .map_err(|error| format!("caller release body parse: {error}"))?,
        ),
        Some(SqlValue::Null) => None,
        other => return Err(format!("caller release body shape: {other:?}")),
    };
    let stored_status = match row.get(4) {
        Some(SqlValue::Int32(value)) => u16::try_from(*value).ok(),
        Some(SqlValue::Int64(value)) => u16::try_from(*value).ok(),
        Some(SqlValue::Null) => None,
        other => return Err(format!("caller release HTTP status shape: {other:?}")),
    };
    let stored_node = match row.get(5) {
        Some(SqlValue::Text(value)) => Some(value.clone()),
        Some(SqlValue::Null) => None,
        other => return Err(format!("caller release node shape: {other:?}")),
    };
    let stored_hash = match row.get(6) {
        Some(SqlValue::Text(value)) => Some(value.clone()),
        Some(SqlValue::Null) => None,
        other => return Err(format!("caller release hash shape: {other:?}")),
    };
    if parent.is_some() {
        let release = ChildReleaseResult::from_parts(
            code,
            run_status,
            stored_kind,
            stored_body,
            stored_status,
            stored_node,
            stored_hash,
        )
        .ok_or_else(|| format!("unknown child release result: {code}"))?;
        match release {
            ChildReleaseResult::Released => Ok(()),
            ChildReleaseResult::AlreadyReleased(stored)
                if stored.exactly_matches(kind, body, Some(status), Some(node), &release_hash) =>
            {
                Ok(())
            }
            ChildReleaseResult::AlreadyReleased(_) => {
                Err("child release replay disagrees with stored outcome".to_string())
            }
            ChildReleaseResult::FenceLost => Err("child release refused: fence-lost".to_string()),
            other => Err(format!("child release refused: {other:?}")),
        }
    } else {
        let release = CallerReleaseResult::from_parts(
            code,
            run_status,
            stored_kind,
            stored_body,
            stored_status,
            stored_node,
            stored_hash,
        )
        .ok_or_else(|| format!("unknown caller release result: {code}"))?;
        match release {
            CallerReleaseResult::Released => Ok(()),
            CallerReleaseResult::AlreadyReleased(stored)
                if stored.exactly_matches(kind, body, Some(status), Some(node), &release_hash) =>
            {
                Ok(())
            }
            CallerReleaseResult::AlreadyReleased(_) => {
                Err("caller release replay disagrees with stored outcome".to_string())
            }
            CallerReleaseResult::FenceLost => Err("caller release refused: fence-lost".to_string()),
            other => Err(format!("caller release refused: {other:?}")),
        }
    }
}

fn terminalize_in_transaction(
    txn: &client::Transaction,
    run_id: &str,
    status: &str,
    reason: Option<&str>,
    result: &Value,
    owner: &str,
    lease_generation: i64,
) -> Result<(), String> {
    let response = txn
        .query(
            &terminalize_sql(),
            &[
                text(run_id),
                text(run_id),
                text(owner),
                int64(lease_generation),
                text(status),
                reason.map_or(SqlValue::Null, text),
                jsonb(result),
            ],
        )
        .map_err(|error| err_name(&error))?;
    let row = response
        .rows
        .first()
        .ok_or("terminal transition returned no result row")?;
    let code = match row.first() {
        Some(SqlValue::Text(value)) => value.as_str(),
        other => return Err(format!("terminal result shape: {other:?}")),
    };
    let stored_status = match row.get(1) {
        Some(SqlValue::Text(value)) => value.as_str(),
        Some(SqlValue::Null) => "",
        other => return Err(format!("terminal run status shape: {other:?}")),
    };
    match TerminalizeResult::from_parts(code, stored_status)
        .ok_or_else(|| format!("unknown terminal transition result: {code}"))?
    {
        TerminalizeResult::Terminalized => Ok(()),
        TerminalizeResult::RunTerminal(stored) if stored.as_sql() == status => Ok(()),
        TerminalizeResult::FenceLost => Err("terminal transition refused: fence-lost".to_string()),
        other => Err(format!("terminal transition refused: {other:?}")),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "typed respond emission plus capture and queue fence form one durable transition"
)]
fn complete_respond_and_renew(
    run_id: &str,
    dispatch: &Dispatch,
    port: &str,
    output: &Value,
    capture: CaptureMode,
    context: &Value,
    complete: bool,
    ttl_ms: i64,
    owner: &str,
    lease_generation: i64,
) -> Result<(), String> {
    let txn = client::begin().map_err(|error| err_name(&error))?;
    let (binds, _) = capture_binds(capture, output, &dispatch.payload);
    let [out_j, in_j, size, hash] = binds;
    let response = txn
        .query(
            &complete_attempt_success_sql(),
            &[
                text(run_id),
                text(run_id),
                text(owner),
                int64(lease_generation),
                text(&dispatch.node),
                int32(dispatch.occurrence as i32),
                text(port),
                out_j,
                in_j,
                size,
                hash,
                text(context.to_string()),
                int64(ttl_ms),
            ],
        )
        .map_err(|error| err_name(&error))?;
    decode_attempt_completion(&response.rows)?;

    let status = wamn_nodes::respond::status_for(&dispatch.config)
        .expect("validated respond config has an HTTP status");
    release_caller_in_transaction(
        &txn,
        run_id,
        "responded",
        output,
        status,
        &dispatch.node,
        owner,
        lease_generation,
    )?;
    if complete {
        terminalize_in_transaction(
            &txn,
            run_id,
            "completed",
            None,
            output,
            owner,
            lease_generation,
        )?;
    }
    txn.commit().map_err(|error| err_name(&error))?;
    Ok(())
}

/// Complete the fail attempt, release an attached caller, and terminalize the
/// run under one replay-safe transaction.
#[expect(
    clippy::too_many_arguments,
    reason = "typed fail result plus capture and queue fence form one durable transition"
)]
fn complete_fail_and_renew(
    run_id: &str,
    flow_id: &str,
    flow_version: u32,
    dispatch: &Dispatch,
    error: &NodeError,
    caller: CallerState,
    capture: CaptureMode,
    ttl_ms: i64,
    owner: &str,
    lease_generation: i64,
) -> Result<(), String> {
    let txn = client::begin().map_err(|error| err_name(&error))?;
    let (kind, [out_j, in_j, detail, size, hash]) =
        error_capture_binds(capture, error, &dispatch.payload);
    let response = txn
        .query(
            &complete_attempt_error_sql(),
            &[
                text(run_id),
                text(run_id),
                text(owner),
                int64(lease_generation),
                text(&dispatch.node),
                int32(dispatch.occurrence as i32),
                out_j,
                in_j,
                text(kind),
                detail,
                size,
                hash,
                int64(ttl_ms),
            ],
        )
        .map_err(|error| err_name(&error))?;
    decode_attempt_completion(&response.rows)?;

    let config: FailConfig =
        serde_json::from_value(dispatch.config.clone()).expect("validated fail config");
    let mut error_body = serde_json::Map::from_iter([
        ("code".to_string(), Value::String(config.code.clone())),
        ("run-id".to_string(), Value::String(run_id.to_string())),
        ("flow-id".to_string(), Value::String(flow_id.to_string())),
        (
            "flow-version".to_string(),
            Value::Number(flow_version.into()),
        ),
    ]);
    if let Some(message) = config.message {
        error_body.insert("message".to_string(), Value::String(message));
    }
    let release_body = json!({"error": error_body});
    if caller == CallerState::Attached {
        release_caller_in_transaction(
            &txn,
            run_id,
            "failed",
            &release_body,
            config.status,
            &dispatch.node,
            owner,
            lease_generation,
        )?;
    }
    terminalize_in_transaction(
        &txn,
        run_id,
        "failed",
        Some(&config.code),
        &release_body,
        owner,
        lease_generation,
    )?;
    txn.commit().map_err(|error| err_name(&error))?;
    Ok(())
}

/// Commit one engine-owned boundary under the claimed run's fence. Fail couples
/// caller release, the synthetic node record, and the terminal
/// verdict in one transaction. A replay may observe `already-released`; it is
/// accepted only when every stored outcome field matches the boundary being
/// replayed.
#[expect(
    clippy::too_many_arguments,
    reason = "reserved checkpoint identity plus the queue fence and capture policy"
)]
fn commit_reserved_and_renew(
    run_id: &str,
    flow_id: &str,
    flow_version: u32,
    step: &ReservedStep,
    caller: CallerState,
    seq: i32,
    capture: CaptureMode,
    ttl_ms: i64,
    owner: &str,
    lease_generation: i64,
) -> Result<(), String> {
    if matches!(step, ReservedStep::Entry { .. }) {
        let (binds, _) = capture_binds(capture, step.payload(), step.payload());
        let [out_j, in_j, size, hash] = binds;
        let response = client::query(
            &reserved_checkpoint_sql(),
            &[
                text(run_id),
                text(run_id),
                text(owner),
                int64(lease_generation),
                text(step.node()),
                int32(step.occurrence() as i32),
                int32(seq),
                text(MAIN_PORT),
                out_j,
                in_j,
                size,
                hash,
                int64(ttl_ms),
            ],
        )
        .map_err(|error| err_name(&error))?;
        let row = response
            .rows
            .first()
            .ok_or("reserved entry checkpoint returned no result row")?;
        let code = match row.first() {
            Some(SqlValue::Text(code)) => code.as_str(),
            other => return Err(format!("reserved entry result shape: {other:?}")),
        };
        let run_status = match row.get(1) {
            Some(SqlValue::Text(status)) => status.as_str(),
            Some(SqlValue::Null) => "",
            other => return Err(format!("reserved entry status shape: {other:?}")),
        };
        return match ReservedCheckpointResult::from_parts(code, run_status)
            .ok_or_else(|| format!("unknown reserved entry result: {code}"))?
        {
            ReservedCheckpointResult::Recorded => Ok(()),
            // FenceLost is absolute: the helper returns without a later record,
            // renew, terminal transition, or legacy settle.
            ReservedCheckpointResult::FenceLost => {
                Err("reserved entry checkpoint refused: fence-lost".to_string())
            }
            other => Err(format!("reserved entry checkpoint refused: {other:?}")),
        };
    }

    let txn = client::begin().map_err(|error| err_name(&error))?;
    let (release_body, release) = match step {
        ReservedStep::Fail {
            code,
            message,
            status,
            node,
            ..
        } => {
            let mut error = serde_json::Map::from_iter([
                ("code".to_string(), Value::String(code.clone())),
                ("run-id".to_string(), Value::String(run_id.to_string())),
                ("flow-id".to_string(), Value::String(flow_id.to_string())),
                (
                    "flow-version".to_string(),
                    Value::Number(flow_version.into()),
                ),
            ]);
            if let Some(message) = message {
                error.insert("message".to_string(), Value::String(message.clone()));
            }
            let body = json!({"error": error});
            let release =
                (caller == CallerState::Attached).then_some(("failed", *status, node.as_str()));
            (body, release)
        }
        ReservedStep::Entry { .. } => unreachable!("entry returned above"),
    };

    if let Some((kind, status, node)) = release {
        release_caller_in_transaction(
            &txn,
            run_id,
            kind,
            &release_body,
            status,
            node,
            owner,
            lease_generation,
        )?;
    }

    let (binds, _) = capture_binds(capture, step.payload(), step.payload());
    let [out_j, in_j, size, hash] = binds;
    txn.execute(
        &record_success_and_renew_sql(),
        &[
            text(run_id),
            text(step.node()),
            int32(step.occurrence() as i32),
            int32(seq),
            text(MAIN_PORT),
            out_j,
            in_j,
            size,
            hash,
            int64(ttl_ms),
            text(owner),
        ],
    )
    .map_err(|error| err_name(&error))?;

    let terminal = match step {
        ReservedStep::Fail { code, .. } => {
            Some(("failed", Some(code.as_str()), release_body.clone()))
        }
        ReservedStep::Entry { .. } => None,
    };
    if let Some((status, reason, result)) = terminal {
        terminalize_in_transaction(
            &txn,
            run_id,
            status,
            reason,
            &result,
            owner,
            lease_generation,
        )?;
    }
    txn.commit().map_err(|error| err_name(&error))
}

/// Fenced frontier-exhaustion completion. The transition owns the final result
/// and queue removal in one statement and refuses an unreleased request caller.
fn terminalize_claimed(
    run_id: &str,
    result: &Value,
    owner: &str,
    lease_generation: i64,
) -> Result<(), String> {
    let response = client::query(
        &terminalize_sql(),
        &[
            text(run_id),
            text(run_id),
            text(owner),
            int64(lease_generation),
            text("completed"),
            SqlValue::Null,
            jsonb(result),
        ],
    )
    .map_err(|error| err_name(&error))?;
    let row = response
        .rows
        .first()
        .ok_or("terminal completion returned no result row")?;
    let code = match row.first() {
        Some(SqlValue::Text(code)) => code.as_str(),
        other => return Err(format!("terminal completion result shape: {other:?}")),
    };
    let stored_status = match row.get(1) {
        Some(SqlValue::Text(status)) => status.as_str(),
        Some(SqlValue::Null) => "",
        other => return Err(format!("terminal completion status shape: {other:?}")),
    };
    match TerminalizeResult::from_parts(code, stored_status)
        .ok_or_else(|| format!("unknown terminal completion result: {code}"))?
    {
        TerminalizeResult::Terminalized => Ok(()),
        TerminalizeResult::RunTerminal(stored) if stored.as_sql() == "completed" => Ok(()),
        // FenceLost is absolute: the helper returns without another store
        // access, including a legacy settle attempt.
        TerminalizeResult::FenceLost => Err("terminal completion refused: fence-lost".to_string()),
        other => Err(format!("terminal completion refused: {other:?}")),
    }
}

/// Fenced terminalization for an incomplete attempt that cannot be replayed.
fn terminalize_effect_uncertain(
    run_id: &str,
    node_id: &str,
    owner: &str,
    lease_generation: i64,
) -> Result<(), String> {
    let reason = format!("effect-uncertain:{node_id}");
    let response = client::query(
        &terminalize_sql(),
        &[
            text(run_id),
            text(run_id),
            text(owner),
            int64(lease_generation),
            text("failed"),
            text(&reason),
            jsonb(&json!({"code":"effect-uncertain","node":node_id})),
        ],
    )
    .map_err(|error| err_name(&error))?;
    let row = response
        .rows
        .first()
        .ok_or("effect-uncertain terminalization returned no result row")?;
    let code = match row.first() {
        Some(SqlValue::Text(code)) => code.as_str(),
        other => return Err(format!("effect-uncertain result shape: {other:?}")),
    };
    let status = match row.get(1) {
        Some(SqlValue::Text(status)) => status.as_str(),
        Some(SqlValue::Null) => "",
        other => return Err(format!("effect-uncertain status shape: {other:?}")),
    };
    match TerminalizeResult::from_parts(code, status)
        .ok_or_else(|| format!("unknown effect-uncertain result: {code}"))?
    {
        TerminalizeResult::Terminalized => Ok(()),
        TerminalizeResult::RunTerminal(stored) if stored.as_sql() == "failed" => Ok(()),
        TerminalizeResult::FenceLost => {
            Err("effect-uncertain terminalization refused: fence-lost".to_string())
        }
        other => Err(format!(
            "effect-uncertain terminalization refused: {other:?}"
        )),
    }
}

/// Persist a typed connection-admission refusal before any effect reaches the
/// wire. Request callers receive the same durable failure envelope read by the
/// invocation API; callerless runs retain the typed terminal result only.
#[expect(
    clippy::too_many_arguments,
    reason = "typed refusal identity and queue fence form one durable transition"
)]
fn terminalize_connection_refusal(
    run_id: &str,
    flow_id: &str,
    flow_version: u32,
    node_id: &str,
    caller: CallerState,
    refusal: &str,
    owner: &str,
    lease_generation: i64,
) -> Result<(), String> {
    let txn = client::begin().map_err(|error| err_name(&error))?;
    let result = json!({
        "error": {
            "code": refusal,
            "run-id": run_id,
            "flow-id": flow_id,
            "flow-version": flow_version,
        }
    });
    if caller == CallerState::Attached {
        release_caller_in_transaction(
            &txn,
            run_id,
            "failed",
            &result,
            500,
            node_id,
            owner,
            lease_generation,
        )?;
    }
    terminalize_in_transaction(
        &txn,
        run_id,
        "failed",
        Some(refusal),
        &result,
        owner,
        lease_generation,
    )?;
    txn.commit().map_err(|error| err_name(&error))
}

/// Remove a run's queue row on a guest-observed TERMINAL failure (the `runs`
/// history stays) — and, iff the row is a `blocking`-partition head, land the
/// `run_dead_letters` marker in the SAME statement/transaction (wamn-v8cv, the
/// D20 dead-letter + continue decision): the key continues in order, never
/// silently. Unpartitioned and `leapfrog` rows degenerate to a plain dequeue —
/// the conditionality lives in the SQL, keyed on the row's own materialized
/// policy, so the guest cannot disagree with the queue about which promise was
/// made.
fn dead_letter_dequeue(run_id: &str, reason: &str) -> Result<(), String> {
    client::execute(&dead_letter_dequeue_sql(), &[text(run_id), text(reason)])
        .map_err(|e| err_name(&e))?;
    Ok(())
}

/// Queue-park a run for a bounded-retry wake: push `available_at` by `park_ms`
/// and RELEASE the lease so no replica holds it while it waits (the wake
/// re-claim is free — wamn-fqg.5/.7). This retained queue operation is distinct
/// from the removed node-level parked state.
fn park_queue_for_retry(run_id: &str, park_ms: u64) -> Result<(), String> {
    let ms = i64::try_from(park_ms).unwrap_or(i64::MAX);
    client::execute(&queue_park_sql(), &[text(run_id), int64(ms)]).map_err(|e| err_name(&e))?;
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
// wamn-run-state::queue and are host-gated by queuebench; fqg.9 is their first GUEST
// caller, mirroring how fqg.4 was the first guest caller of `claim_batch_sql`.
// ---------------------------------------------------------------------------

/// A partition head claimed for this replica: the run to drive plus the key it
/// belongs to (needed to renew/release the partition lease around the walk).
struct PartitionHead {
    run_id: String,
    partition_key: String,
    lease_generation: i64,
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
    let lease_generation = match row.get(4) {
        Some(SqlValue::Int64(value)) => *value,
        Some(SqlValue::Int32(value)) => i64::from(*value),
        other => return Err(format!("partition head lease_generation shape: {other:?}")),
    };
    Ok(Some(PartitionHead {
        run_id,
        partition_key,
        lease_generation,
    }))
}

/// Read a claimed run's dispatch inputs (the recorded flow, its persisted
/// `flow_version`, and the trigger input) — the partition head claim returns only
/// `(run_id, partition_key)`, so the guest reads what the combined unpartitioned
/// `claim_dispatch_sql` returns inline. The `flow_version` (wamn-cox) pins the
/// partitioned resume to the version the run started under, exactly as the
/// unpartitioned claim does.
fn read_dispatch(run_id: &str) -> Result<(String, Option<u32>, Value, CaptureMode), String> {
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
    let flow_version = match row.get(1) {
        Some(SqlValue::Int32(v)) => u32::try_from(*v).ok(),
        Some(SqlValue::Int64(v)) => u32::try_from(*v).ok(),
        _ => None,
    };
    let input = match row.get(2) {
        Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => {
            serde_json::from_str(s).map_err(|e| format!("runs.input_json parse: {e}"))?
        }
        _ => Value::Null,
    };
    let capture_mode = match row.get(3) {
        Some(SqlValue::Text(value)) => CaptureMode::from_sql(value)
            .ok_or_else(|| format!("unknown runs.capture_mode: {value}"))?,
        other => return Err(format!("runs.capture_mode shape: {other:?}")),
    };
    Ok((flow_id, flow_version, input, capture_mode))
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
/// (0) already dropped its queue row inside the fenced terminal transition;
/// queue-waiting (1) pushes `available_at` and releases the run lease; failed
/// (2) dequeues — via
/// [`dead_letter_dequeue`], so a `blocking`-partition head's key continues past
/// the failure WITH its ledger marker in the same transaction (wamn-v8cv).
fn settle(run_id: &str, claim: &ClaimOutcome) -> Result<(), String> {
    if claim.already_settled {
        return Ok(());
    }
    match claim.outcome {
        0 => {} // completed: already dequeued
        1 => park_queue_for_retry(run_id, claim.park_ms)?,
        _ => dead_letter_dequeue(run_id, claim.fail_reason.as_deref().unwrap_or("failed"))?,
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
    let (_flow_id, _flow_version, input, capture_mode) = read_dispatch(&head.run_id)?;
    let plan = load_execution_plan(&head.run_id)?;
    let claim = execute_claimed(
        &head.run_id,
        &plan,
        input,
        capture_mode,
        owner,
        ttl_ms,
        Some(&head.partition_key),
        head.lease_generation,
    )?;
    settle(&head.run_id, &claim)?;
    Ok(Some((head.run_id, claim.outcome)))
}

/// Drive a run CLAIMED from the queue: like [`execute`] but the flow + input come
/// from the dispatcher-persisted `runs` row (not a fixture id / wrapped string),
/// the lease is renewed per node, and terminal states become an `outcome` code
/// (the caller settles the queue row) rather than a `Result` return. The dispatcher
/// already wrote the `runs` row and the claim flipped it `running`, so this does
/// NOT re-open the run — it reconstructs from `node_runs` and continues.
fn execute_claimed(
    _run_id: &str,
    _plan: &wamn_catalog::ExecutionPlanV2,
    _input: Value,
    _capture_mode: CaptureMode,
    _owner: &str,
    _ttl_ms: i64,
    _partition: Option<&str>,
    _lease_generation: i64,
) -> Result<ClaimOutcome, String> {
    execution_refusal()
}

const EXECUTION_INTERPRETER_REFUSAL: &str =
    "execution refuses until authoritative execution-plan interpretation is installed";

fn execution_refusal<T>() -> Result<T, String> {
    Err(EXECUTION_INTERPRETER_REFUSAL.to_string())
}

/// One turn of the production dispatch loop: claim the next run, drive it with a
/// per-node heartbeat, and dequeue or queue-wait settlement. See the WIT doc.
fn run_next(_lease_ttl_ms: u64) -> Result<(bool, Option<String>, u32), String> {
    execution_refusal()
}

/// Drive the exact HTTP run/fence claimed by final admission.
///
/// The first statement is the single-driver arbitration point. Every refusal
/// returns before loading the artifact or touching run state again; in
/// particular, `fence-lost` is absolute. Generic `run-next` remains the only
/// path that scans available work.
fn execute_admitted_claimed(
    _run_id: &str,
    _lease_owner: &str,
    _lease_generation: i64,
    _lease_ttl_ms: u64,
) -> Result<u32, String> {
    execution_refusal()
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
fn bench_walk(plan: &Plan, mut on_step: impl FnMut(&Dispatch, NodeOutcome, &mut ExecutionState)) {
    let mut st = plan.start("bench", Value::String("dispatch-probe-payload".into()));
    loop {
        match plan.next(&mut st, 0) {
            Step::Reserved(step) => plan
                .apply_reserved(&mut st, &step)
                .expect("benchmark reserved transition"),
            Step::Dispatch(d) => {
                let outcome = bench_node(&d);
                on_step(&d, outcome, &mut st);
            }
            Step::Done(_) | Step::Wait { .. } => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Guest exports
// ---------------------------------------------------------------------------

impl Guest for Component {
    fn dispatch_bench(iterations: u32, flow_json: String) -> Result<(u64, Vec<u32>), String> {
        let flow = Flow::from_json(&flow_json).map_err(|e| format!("bench flow: {e}"))?;
        let interfaces = s3_fixture_interfaces();
        let plan = Plan::compile(&flow, &interfaces).map_err(|e| e.to_string())?;
        let iters = iterations.max(1) as usize;

        // Warm up (page in, settle the branch predictor) before measuring.
        for _ in 0..1000 {
            bench_walk(&plan, |d, o, st| {
                plan.apply(st, d, o, 0).expect("benchmark dispatch")
            });
        }

        // Un-instrumented pass: one clock read for the whole batch — the
        // harness derives the amortized per-dispatch mean from the total.
        let t_bare = Instant::now();
        for _ in 0..iters {
            bench_walk(&plan, |d, o, st| {
                plan.apply(st, d, o, 0).expect("benchmark dispatch")
            });
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
                plan.apply(st, d, o, 0).expect("benchmark dispatch");
                let dt = t0.elapsed().as_nanos();
                samples.push(dt.min(u32::MAX as u128) as u32);
            });
        }
        Ok((bare_ns, samples))
    }

    fn check_flow(flow_json: String) -> Result<Vec<String>, String> {
        check_flow(&flow_json)
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

    fn run(_run_id: String, _payload: String) -> Result<u32, String> {
        execution_refusal()
    }

    fn run_next(lease_ttl_ms: u64) -> Result<(bool, Option<String>, u32), String> {
        run_next(lease_ttl_ms)
    }

    fn execute_claimed(
        run_id: String,
        lease_owner: String,
        lease_generation: i64,
        lease_ttl_ms: u64,
    ) -> Result<u32, String> {
        execute_admitted_claimed(&run_id, &lease_owner, lease_generation, lease_ttl_ms)
    }

    fn run_until_kill(_run_id: String, _payload: String) -> Result<u32, String> {
        execution_refusal()
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

    fn run_s6(_run_id: String, _payload: String) -> Result<(u32, u32), String> {
        execution_refusal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured_error_with_secret() -> NodeError {
        NodeError::Terminal(ErrorDetail {
            message: "request denied".to_string(),
            code: Some("denied".to_string()),
            data: Some(serde_json::json!({"token": "raw-secret"})),
        })
    }

    #[test]
    fn capture_off_error_binds_retain_only_the_typed_kind() {
        let (kind, binds) = error_capture_binds(
            CaptureMode::Off,
            &captured_error_with_secret(),
            &serde_json::json!({"password": "raw-secret"}),
        );

        assert_eq!(kind, "terminal");
        for (field, bind) in [
            "output_json",
            "input_json",
            "error_detail",
            "output_size",
            "payload_hash",
        ]
        .into_iter()
        .zip(binds)
        {
            assert!(
                matches!(bind, SqlValue::Null),
                "capture-off error bind {field} must be SQL NULL"
            );
        }
    }

    #[test]
    fn full_capture_error_detail_is_scrubbed_before_binding() {
        let (_, [_, _, detail, _, _]) = error_capture_binds(
            CaptureMode::Full,
            &captured_error_with_secret(),
            &serde_json::Value::Null,
        );
        let SqlValue::Text(detail) = detail else {
            panic!("full-capture error detail must be a JSON text bind");
        };

        assert!(!detail.contains("raw-secret"));
        assert!(detail.contains(wamn_run_state::capture::REDACTED));
    }

    #[test]
    fn dispatch_causation_declares_trusted_root_and_depth_for_the_claimed_run() {
        let input = serde_json::json!({
            "event": "update",
            "causation": {
                "run": "author-cannot-select-run",
                "root": "trusted-root",
                "depth": 7
            }
        });

        assert_eq!(
            run_context_from("claimed-run", &input),
            ("trusted-root".to_string(), 7)
        );
    }

    #[test]
    fn ordinary_and_fully_malformed_dispatch_input_self_root() {
        for input in [
            serde_json::json!({"request": 1}),
            serde_json::json!({"causation": {"root": 42, "depth": "7"}}),
        ] {
            assert_eq!(
                run_context_from("ordinary-run", &input),
                ("ordinary-run".to_string(), 0)
            );
        }
    }

    #[test]
    fn check_flow_reports_sorted_unique_types_from_the_dispatch_resolver() {
        let flow = r#"{
            "schema-version":"0.1",
            "flow-id":"check",
            "version":1,
            "nodes":[
                {"id":"request","type":"request"},
                {"id":"known","type":"webhook-in"},
                {"id":"standard","type":"event"},
                {"id":"z1","type":"z-unsupported"},
                {"id":"a","type":"a-unsupported"},
                {"id":"z2","type":"z-unsupported"}
            ],
            "edges":[]
        }"#;

        assert_eq!(
            check_flow(flow).unwrap(),
            vec!["a-unsupported".to_string(), "z-unsupported".to_string()]
        );
    }

    #[test]
    fn s3_fixture_interfaces_compile_the_legacy_main_path() {
        let flow = Flow::from_json(
            r#"{
                "schema-version":"0.1",
                "flow-id":"poc-receipt",
                "version":1,
                "nodes":[
                    {"id":"in","type":"request","config":{"input-schema":true}},
                    {"id":"t","type":"transform","config":{"op":"upper"}},
                    {"id":"w","type":"pg-write"},
                    {"id":"c","type":"conditional","config":{"min-len":3}},
                    {"id":"out","type":"respond","config":{"status":200}}
                ],
                "edges":[
                    {"from":"in","to":"t"},
                    {"from":"t","to":"w"},
                    {"from":"w","to":"c"},
                    {"from":"c","to":"out"}
                ]
            }"#,
        )
        .expect("S3 fixture parses");

        Plan::compile(&flow, &s3_fixture_interfaces())
            .expect("legacy S3 fixture compiles with its explicit interfaces");
    }

    /// wamn-gm5d: `run-s6` compiles its graph against this map, so an S6 node
    /// type missing from it is unresolvable — the flow document cannot declare
    /// its own ports and there is no fallback on this path. Each of these
    /// completes on `main`, matching what resolved public interfaces declare
    /// them to on the production `execute_claimed` path.
    #[test]
    fn s3_fixture_interfaces_resolve_every_s6_node_type_on_main() {
        let interfaces = s3_fixture_interfaces();
        for node_type in ["conditional", "http-call", "pg-write", "transform"] {
            assert_eq!(
                interfaces.get(node_type).map(Vec::as_slice),
                Some([MAIN_PORT.to_string()].as_slice()),
                "fixture interface map is missing {node_type:?}"
            );
        }
    }

    /// The S6 fixture `run-s6` drives (`tests/integration/src/flowbench.rs`
    /// owns the JSON, SR2) compiles against the fixture map.
    #[test]
    fn the_s6_fixture_compiles_under_the_fixture_interfaces() {
        let fixture = r#"{
            "schema-version":"0.1",
            "flow-id":"poc-s6",
            "version":1,
            "allowed-hosts":["127.0.0.1:18080"],
            "nodes":[
                {"id":"in","type":"request","config":{"input-schema":true}},
                {"id":"out","type":"respond","config":{"status":200}},
                {"id":"h","type":"http-call","config":{"url":"http://127.0.0.1:18080/echo"}},
                {"id":"w","type":"pg-write"}
            ],
            "edges":[
                {"from":"in","to":"out"},
                {"from":"out","to":"h"},
                {"from":"h","to":"w"}
            ]
        }"#;
        let flow = Flow::from_json(fixture).expect("S6 fixture shape parses");
        Plan::compile(&flow, &s3_fixture_interfaces())
            .expect("S6 fixture shape compiles with the fixture interfaces");
    }

    #[test]
    fn expression_transform_resolves_through_the_standard_library() {
        let config = serde_json::json!({"expression": "payload"});
        assert_eq!(
            resolve_node("transform", &config),
            Some(ResolvedNode::Standard)
        );
    }

    #[test]
    fn respond_resolves_through_the_standard_node_interface() {
        assert_eq!(
            resolve_node("respond", &serde_json::json!({"status": 202})),
            Some(ResolvedNode::Standard)
        );
        let interface =
            wamn_nodes::describe_interface("respond").expect("respond interface is shipped");
        assert_eq!(interface.node_type, "respond");
    }

    #[test]
    fn request_resolves_through_the_standard_node_interface() {
        assert_eq!(
            resolve_node("request", &serde_json::json!({"input-schema": {}})),
            Some(ResolvedNode::Standard)
        );
        let interface =
            wamn_nodes::describe_interface("request").expect("request interface is shipped");
        assert_eq!(interface.node_type, "request");
        assert_eq!(interface.output_ports, ["main"]);
    }

    #[test]
    fn event_resolves_through_the_standard_node_interface() {
        assert_eq!(
            resolve_node("event", &serde_json::Value::Null),
            Some(ResolvedNode::Standard)
        );
        let interface =
            wamn_nodes::describe_interface("event").expect("event interface is shipped");
        assert_eq!(interface.node_type, "event");
        assert_eq!(interface.output_ports, ["main"]);
    }

    #[test]
    fn fail_resolves_through_the_standard_node_interface() {
        assert_eq!(
            resolve_node(
                "fail",
                &serde_json::json!({"code": "denied", "status": 403}),
            ),
            Some(ResolvedNode::Standard)
        );
        let interface = wamn_nodes::describe_interface("fail").expect("fail interface is shipped");
        assert_eq!(interface.node_type, "fail");
        assert_eq!(interface.output_ports, ["main"]);
    }

    #[test]
    fn malformed_fail_actions_are_refused_before_the_lifecycle_transaction() {
        let dispatch = Dispatch {
            node: "bad".to_string(),
            node_type: "fail".to_string(),
            config: serde_json::json!({
                "code": "denied",
                "message": "not allowed",
                "status": 403
            }),
            connection: None,
            credential: None,
            payload: serde_json::json!({"reason": "policy"}),
            context: serde_json::json!({}),
            attempt: 0,
            occurrence: 0,
            deadline_ms: None,
        };
        for outcome in [
            NodeOutcome::ok(dispatch.payload.clone()),
            NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded(
                "wrong",
                "not allowed",
            ))),
            NodeOutcome::Error(NodeError::InvalidInput(ErrorDetail::coded(
                "denied",
                "not allowed",
            ))),
        ] {
            assert_eq!(
                validate_dispatched_action(&dispatch, &outcome),
                Err("fail must return its authored terminal detail".to_string())
            );
        }
    }

    #[test]
    fn malformed_event_actions_are_refused_before_durable_checkpointing() {
        let dispatch = Dispatch {
            node: "in".to_string(),
            node_type: "event".to_string(),
            config: serde_json::Value::Null,
            connection: None,
            credential: None,
            payload: serde_json::json!({"topic": "orders.created", "id": 42}),
            context: serde_json::json!({}),
            attempt: 0,
            occurrence: 0,
            deadline_ms: None,
        };
        for outcome in [
            NodeOutcome::ok(serde_json::json!({"changed": true})),
            NodeOutcome::ok_on(dispatch.payload.clone(), "alternate"),
            NodeOutcome::ok_with_context(
                dispatch.payload.clone(),
                MAIN_PORT,
                serde_json::json!({"changed": true}),
            ),
        ] {
            assert_eq!(
                validate_dispatched_action(&dispatch, &outcome),
                Err(
                    "event must emit its externally admitted input unchanged on main without context"
                        .to_string()
                )
            );
        }
    }

    #[test]
    fn malformed_cron_actions_are_refused_before_durable_checkpointing() {
        let dispatch = Dispatch {
            node: "in".to_string(),
            node_type: "cron".to_string(),
            config: serde_json::Value::Null,
            connection: None,
            credential: None,
            payload: serde_json::json!({"scheduled-at": 42}),
            context: serde_json::json!({}),
            attempt: 0,
            occurrence: 0,
            deadline_ms: None,
        };
        for outcome in [
            NodeOutcome::ok(serde_json::json!({"changed": true})),
            NodeOutcome::ok_on(dispatch.payload.clone(), "alternate"),
            NodeOutcome::ok_with_context(
                dispatch.payload.clone(),
                MAIN_PORT,
                serde_json::json!({"changed": true}),
            ),
        ] {
            assert_eq!(
                validate_dispatched_action(&dispatch, &outcome),
                Err(
                    "cron must emit its scheduler-admitted input unchanged on main without context"
                        .to_string()
                )
            );
        }
    }

    #[test]
    fn malformed_request_actions_are_refused_before_durable_checkpointing() {
        let dispatch = Dispatch {
            node: "in".to_string(),
            node_type: "request".to_string(),
            config: serde_json::json!({"input-schema": {}}),
            connection: None,
            credential: None,
            payload: serde_json::json!({"admitted": true}),
            context: serde_json::json!({}),
            attempt: 0,
            occurrence: 0,
            deadline_ms: None,
        };
        for outcome in [
            NodeOutcome::ok(serde_json::json!({"changed": true})),
            NodeOutcome::ok_on(dispatch.payload.clone(), "alternate"),
            NodeOutcome::ok_with_context(
                dispatch.payload.clone(),
                MAIN_PORT,
                serde_json::json!({"changed": true}),
            ),
        ] {
            assert_eq!(
                validate_dispatched_action(&dispatch, &outcome),
                Err(
                    "request must emit its admitted input unchanged on main without context"
                        .to_string()
                )
            );
        }
    }

    #[test]
    fn retry_state_records_only_the_deterministic_delay_schedule() {
        assert_eq!(
            retry_state("call", 2, 750, None),
            serde_json::json!({
                "retry": {
                    "node": "call",
                    "attempt": 2,
                    "delay-ms": 750
                }
            })
        );
    }

    #[test]
    fn legacy_retry_state_shape_remains_a_valid_cursor() {
        let legacy = serde_json::json!({"retry": {"node": "call", "attempt": 2}});
        assert!(
            matches!(
                parse_retry(&legacy),
                Some((ref node, 2, None)) if node == "call"
            ),
            "the production parser must not require the additive schedule field"
        );
    }

    #[test]
    fn raw_sql_config_requires_exactly_one_json_true() {
        assert!(raw_sql_enabled_rows(&[vec![SqlValue::Text("true".into())]]));
        assert!(raw_sql_enabled_rows(&[vec![SqlValue::Json("true".into())]]));

        for rows in [
            Vec::new(),
            vec![
                vec![SqlValue::Text("true".into())],
                vec![SqlValue::Text("true".into())],
            ],
            vec![vec![SqlValue::Text("false".into())]],
            vec![vec![SqlValue::Text("\"true\"".into())]],
            vec![vec![SqlValue::Text("{\"enabled\":true}".into())]],
            vec![vec![SqlValue::Text("not-json".into())]],
            vec![vec![SqlValue::Boolean(true)]],
            vec![vec![]],
        ] {
            assert!(
                !raw_sql_enabled_rows(&rows),
                "non-canonical config must deny: {rows:?}"
            );
        }
    }

    #[test]
    fn draft_source_identity_is_an_exact_pair() {
        assert_eq!(
            classify_pinned_artifact_lineage(
                Some(DRAFT_TRIGGER_SOURCE),
                Some(DRAFT_SOURCE_PRODUCER),
            ),
            Ok(PinnedArtifactLineage::Draft)
        );
        for release_pair in [(None, None), (Some("scenario"), Some("scenario"))] {
            assert_eq!(
                classify_pinned_artifact_lineage(release_pair.0, release_pair.1),
                Ok(PinnedArtifactLineage::Release)
            );
        }
        for mismatched_pair in [
            (Some(DRAFT_TRIGGER_SOURCE), None),
            (Some(DRAFT_TRIGGER_SOURCE), Some("scenario")),
            (None, Some(DRAFT_SOURCE_PRODUCER)),
            (Some("scenario"), Some(DRAFT_SOURCE_PRODUCER)),
        ] {
            assert!(
                classify_pinned_artifact_lineage(mismatched_pair.0, mismatched_pair.1).is_err(),
                "one-sided draft source marker must refuse: {mismatched_pair:?}"
            );
        }
    }

    #[test]
    fn release_lineage_has_no_executable_reader_and_draft_joins_exact_plan_bytes() {
        assert!(PINNED_ARTIFACT_SQL.contains("blocked_lineage"));
        assert!(PINNED_ARTIFACT_SQL.contains("r.artifact_lineage <> 'draft'"));
        assert!(PINNED_ARTIFACT_SQL.contains("JOIN catalog.execution_bundles AS bundle"));
        assert!(PINNED_ARTIFACT_SQL.contains("bundle.exact_bytes"));
        assert!(
            PINNED_ARTIFACT_SQL.contains("bundle.execution_bundle_hash = r.execution_bundle_hash")
        );
        assert!(PINNED_ARTIFACT_SQL.contains("d.execution_bundle_hash = r.execution_bundle_hash"));
        let retired_json_pin = ["execution", "bundle", "hash"].join("-");
        assert!(!PINNED_ARTIFACT_SQL.contains(&retired_json_pin));
        assert!(!PINNED_ARTIFACT_SQL.contains("catalog.release_flows"));
        assert!(!PINNED_ARTIFACT_SQL.contains("catalog.flow_artifacts"));
        assert!(!PINNED_ARTIFACT_SQL.contains("graph_json"));
    }

    #[test]
    fn every_execution_entry_refuses_before_database_mutation() {
        assert!(
            <Component as Guest>::run_next(1)
                .unwrap_err()
                .contains("execution refuses")
        );
        assert!(
            <Component as Guest>::run("run".into(), "{}".into())
                .unwrap_err()
                .contains("execution refuses")
        );
        assert!(
            <Component as Guest>::execute_claimed("run".into(), "owner".into(), 1, 1)
                .unwrap_err()
                .contains("execution refuses")
        );
        assert!(
            <Component as Guest>::run_until_kill("run".into(), "{}".into())
                .unwrap_err()
                .contains("execution refuses")
        );
        assert!(
            <Component as Guest>::run_s6("run".into(), "{}".into())
                .unwrap_err()
                .contains("execution refuses")
        );
    }
}
