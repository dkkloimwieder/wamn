//! Single-shot flow-runner component.
//!
//! MVP outcome: crash floor · M0 execution · flow composition.
//!
//! The public world has one product operation, `run`. Production execution
//! remains fail-closed until effect activation lands in its owning change.
//! Immutable claimed-run plans cross the trusted host
//! supply, are hash-verified, and feed the in-memory frame interpreter.
//! This component retains
//! the standard-node capability adapter, including the self-describing trusted
//! HTTP-effect context consumed by that later execution path.

mod frames;

pub use frames::{
    DEFAULT_ROOT_DISPATCH_BUDGET, FrameCompletion, FrameExecutionError, FrameExecutionErrorKind,
    FrameFailure, FrameStack, MAX_CALL_DEPTH, NodeFactOutcome, NodeFactSink, NodeFactSinkError,
    PureNodeDispatcher, TrustedFrame, TrustedFrameFacts, TrustedNodeFact,
};

wit_bindgen::generate!({
    world: "flowrunner",
    path: "wit",
    generate_all,
});

use wamn_flow::node_contract::{self as sdk};

use wamn::postgres::client;
use wamn::postgres::types::{PgError, SqlValue};
use wamn::runner::http_effect;
#[cfg(any(target_arch = "wasm32", test))]
use wamn::runner::plan_supply;

use std::collections::BTreeMap;

/// One hash-verified plan and its immutable resolution facts.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedResolutionPlan {
    flow_id: String,
    execution_bundle_hash: String,
    source_artifact_hash: String,
    plan: wamn_catalog::ExecutionPlanV2,
}

impl VerifiedResolutionPlan {
    pub fn flow_id(&self) -> &str {
        &self.flow_id
    }

    pub fn execution_bundle_hash(&self) -> &str {
        &self.execution_bundle_hash
    }

    pub fn source_artifact_hash(&self) -> &str {
        &self.source_artifact_hash
    }

    pub fn plan(&self) -> &wamn_catalog::ExecutionPlanV2 {
        &self.plan
    }
}

/// Complete immutable plan world for one claimed root run.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedResolutionSnapshot {
    root_flow_id: String,
    root_execution_bundle_hash: String,
    plans: BTreeMap<String, VerifiedResolutionPlan>,
}

impl VerifiedResolutionSnapshot {
    pub fn root_flow_id(&self) -> &str {
        &self.root_flow_id
    }

    pub fn root_execution_bundle_hash(&self) -> &str {
        &self.root_execution_bundle_hash
    }

    pub fn root(&self) -> &VerifiedResolutionPlan {
        self.plans
            .get(&self.root_flow_id)
            .expect("verified snapshot retains its exact root")
    }

    pub fn plan(&self, flow_id: &str) -> Option<&VerifiedResolutionPlan> {
        self.plans.get(flow_id)
    }

    pub fn len(&self) -> usize {
        self.plans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }
}

#[cfg(target_arch = "wasm32")]
fn load_verified_snapshot(run_id: &str) -> Result<VerifiedResolutionSnapshot, String> {
    let supplied = plan_supply::load_run_snapshot(run_id).map_err(|error| match error {
        plan_supply::SupplyError::NotFound => "run-resolution-not-found",
        plan_supply::SupplyError::Incomplete => "run-resolution-incomplete",
        plan_supply::SupplyError::HashMismatch => "run-resolution-hash-mismatch",
        plan_supply::SupplyError::Unavailable => "run-resolution-unavailable",
    })?;
    verify_resolution_snapshot(supplied)
}

#[cfg(any(target_arch = "wasm32", test))]
fn verify_resolution_snapshot(
    supplied: plan_supply::RunResolutionSnapshot,
) -> Result<VerifiedResolutionSnapshot, String> {
    let mut plans = BTreeMap::new();
    for supplied_plan in supplied.plans {
        let plan = wamn_catalog::read_execution_plan(
            &supplied_plan.execution_bundle_hash,
            &supplied_plan.exact_bytes,
        )
        .map_err(|_| "run-resolution-hash-mismatch".to_string())?;
        if plan.header.root_artifact_hash != supplied_plan.source_artifact_hash {
            return Err("run-resolution-source-mismatch".into());
        }
        let flow_id = supplied_plan.flow_id;
        let verified = VerifiedResolutionPlan {
            flow_id: flow_id.clone(),
            execution_bundle_hash: supplied_plan.execution_bundle_hash,
            source_artifact_hash: supplied_plan.source_artifact_hash,
            plan,
        };
        if plans.insert(flow_id, verified).is_some() {
            return Err("run-resolution-duplicate-flow".into());
        }
    }
    let root = plans
        .get(&supplied.root_flow_id)
        .ok_or_else(|| "run-resolution-root-missing".to_string())?;
    if root.execution_bundle_hash != supplied.root_execution_bundle_hash {
        return Err("run-resolution-root-mismatch".into());
    }
    Ok(VerifiedResolutionSnapshot {
        root_flow_id: supplied.root_flow_id,
        root_execution_bundle_hash: supplied.root_execution_bundle_hash,
        plans,
    })
}

struct Component;
export!(Component);

/// Trusted identity facts passed with one HTTP effect attempt.
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
    fn http(&mut self, request: &sdk::HttpRequest) -> Result<sdk::HttpResponse, sdk::HttpCapError> {
        trusted_http_effect(self.http_effect.as_ref(), request)
    }

    fn pg_query(
        &mut self,
        sql: &str,
        params: &[sdk::PgValue],
    ) -> Result<sdk::PgRows, sdk::PgCapError> {
        let params: Vec<SqlValue> = params.iter().map(sdk_to_wit).collect();
        let result = client::query(sql, &params).map_err(wit_err_to_sdk)?;
        Ok(sdk::PgRows {
            columns: result
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
            rows: result
                .rows
                .iter()
                .map(|row| row.iter().map(wit_to_sdk).collect())
                .collect(),
        })
    }

    fn pg_execute(&mut self, sql: &str, params: &[sdk::PgValue]) -> Result<u64, sdk::PgCapError> {
        let params: Vec<SqlValue> = params.iter().map(sdk_to_wit).collect();
        client::execute(sql, &params).map_err(wit_err_to_sdk)
    }

    /// Parked closed for deletion — `wamn-0h0g.26.8`.
    ///
    /// `a1ecc789` deleted the only installer of `<app_schema>.wamn_catalog`, so
    /// on a freshly provisioned project the relation does not exist and never
    /// will; `wamn-0h0g.11.46` removed it from the ownership manifests. Until
    /// `wamn-0h0g.26.8` deletes this capability along with the entity node that
    /// consumes it, the absent relation is mapped onto the designed refusal
    /// below instead of escaping as a raw `42P01 undefined_table` — the path
    /// refuses by design rather than being left dead-ish for something to wire
    /// a draft run into.
    fn catalog_json(&mut self) -> Result<String, sdk::PgCapError> {
        catalog_from_rows(
            client::query("SELECT document::text FROM wamn_catalog LIMIT 1", &[])
                .map(|result| result.rows),
        )
    }

    fn raw_sql_enabled(&self) -> bool {
        self.raw_sql
    }
}

fn sdk_to_wit(value: &sdk::PgValue) -> SqlValue {
    match value {
        sdk::PgValue::Null => SqlValue::Null,
        sdk::PgValue::Bool(value) => SqlValue::Boolean(*value),
        sdk::PgValue::Int32(value) => SqlValue::Int32(*value),
        sdk::PgValue::Int64(value) => SqlValue::Int64(*value),
        sdk::PgValue::Float64(value) => SqlValue::Float64(*value),
        sdk::PgValue::Text(value) => SqlValue::Text(value.clone()),
        sdk::PgValue::Bytes(value) => SqlValue::Bytes(value.clone()),
        sdk::PgValue::Numeric(value) => SqlValue::Numeric(value.clone()),
        sdk::PgValue::Timestamptz(value) => SqlValue::Timestamptz(value.clone()),
        sdk::PgValue::Json(value) => SqlValue::Json(value.clone()),
        sdk::PgValue::Uuid(value) => SqlValue::Uuid(value.clone()),
    }
}

fn wit_to_sdk(value: &SqlValue) -> sdk::PgValue {
    match value {
        SqlValue::Null => sdk::PgValue::Null,
        SqlValue::Boolean(value) => sdk::PgValue::Bool(*value),
        SqlValue::Int32(value) => sdk::PgValue::Int32(*value),
        SqlValue::Int64(value) => sdk::PgValue::Int64(*value),
        SqlValue::Float64(value) => sdk::PgValue::Float64(*value),
        SqlValue::Text(value) => sdk::PgValue::Text(value.clone()),
        SqlValue::Bytes(value) => sdk::PgValue::Bytes(value.clone()),
        SqlValue::Numeric(value) => sdk::PgValue::Numeric(value.clone()),
        SqlValue::Timestamptz(value) => sdk::PgValue::Timestamptz(value.clone()),
        SqlValue::Json(value) => sdk::PgValue::Json(value.clone()),
        SqlValue::Uuid(value) => sdk::PgValue::Uuid(value.clone()),
    }
}

/// The designed refusal for a project with no readable catalog snapshot. Both
/// the absent relation and an installed-but-empty one answer with this, so the
/// guest sees one refusal rather than a raw SQLSTATE it cannot act on.
fn no_catalog_snapshot() -> sdk::PgCapError {
    sdk::PgCapError::QueryError {
        code: String::new(),
        message: "no catalog snapshot published for this project".into(),
    }
}

/// Decide the catalog capability's answer from the snapshot query's outcome.
///
/// Split out of `CapsCtx::catalog_json` so the parked-closed behaviour is
/// testable without the host: `42P01 undefined_table` — which is
/// what a project provisioned after `a1ecc789` raises, the installer having
/// been deleted — answers with the same designed refusal as an installed but
/// unpublished snapshot, instead of escaping as a raw SQLSTATE.
fn catalog_from_rows(
    result: Result<Vec<Vec<SqlValue>>, PgError>,
) -> Result<String, sdk::PgCapError> {
    let rows = match result {
        Ok(rows) => rows,
        Err(PgError::QueryError((code, _))) if code == UNDEFINED_TABLE => {
            return Err(no_catalog_snapshot());
        }
        Err(error) => return Err(wit_err_to_sdk(error)),
    };
    match rows.first().and_then(|row| row.first()) {
        Some(SqlValue::Text(value)) | Some(SqlValue::Json(value)) => Ok(value.clone()),
        _ => Err(no_catalog_snapshot()),
    }
}

/// SQLSTATE `42P01`, raised when the relation in a statement does not exist.
const UNDEFINED_TABLE: &str = "42P01";

fn wit_err_to_sdk(error: PgError) -> sdk::PgCapError {
    match error {
        PgError::SerializationFailure => sdk::PgCapError::SerializationFailure,
        PgError::ConnectionUnavailable => sdk::PgCapError::ConnectionUnavailable,
        PgError::StatementTimeout => sdk::PgCapError::StatementTimeout,
        PgError::RowLimitExceeded(limit) => sdk::PgCapError::RowLimitExceeded(limit),
        PgError::UniqueViolation(constraint) => sdk::PgCapError::UniqueViolation(constraint),
        PgError::ForeignKeyViolation(constraint) => {
            sdk::PgCapError::ForeignKeyViolation(constraint)
        }
        PgError::CheckViolation(constraint) => sdk::PgCapError::CheckViolation(constraint),
        PgError::PermissionDenied => sdk::PgCapError::PermissionDenied,
        PgError::QueryError((code, message)) => sdk::PgCapError::QueryError { code, message },
    }
}

fn trusted_http_effect(
    context: Option<&HttpEffectContext>,
    request: &sdk::HttpRequest,
) -> Result<sdk::HttpResponse, sdk::HttpCapError> {
    let context = context.ok_or(sdk::HttpCapError::NotGranted)?;
    if context.requirement_name != request.requirement {
        return Err(sdk::HttpCapError::BadRequest(
            "request requirement does not match the attempt context".into(),
        ));
    }
    let context = http_effect_context_to_wit(context);
    let request = http_effect::RelativeRequest {
        method: request.method.clone(),
        path_and_query: request.path_and_query.clone(),
        headers: request
            .headers
            .iter()
            .map(|(name, value)| http_effect::Header {
                name: name.clone(),
                value: value.as_bytes().to_vec(),
            })
            .collect(),
        body: request.body.clone(),
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

const EXECUTION_INTERPRETER_REFUSAL: &str =
    "execution refuses until authoritative execution-plan interpretation is installed";

impl Guest for Component {
    fn run(run_id: String, _payload: String) -> Result<u32, String> {
        #[cfg(target_arch = "wasm32")]
        let _snapshot = load_verified_snapshot(&run_id)?;
        #[cfg(not(target_arch = "wasm32"))]
        let _ = run_id;
        Err(EXECUTION_INTERPRETER_REFUSAL.to_string())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::{
        CallableContract, CallableEffectCeiling, CallableReturnContract, ExecutionEffectPolicy,
        ExecutionNodeId, ExecutionPlanBody, ExecutionPlanNode, ExecutionPlanV2,
        ExecutionRuntimeRevision, HOST_EFFECT_CONTRACT_VERSION, RootTerminalBehavior,
        entry_input_schema_hash, execution_bundle_hash,
    };

    use super::*;

    fn hash(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    /// The snapshot relation has had no installer since `a1ecc789`, so a project
    /// provisioned after it raises `42P01` here. That must reach the guest as
    /// the designed refusal, not as a raw SQLSTATE: the capability is parked
    /// closed until `wamn-0h0g.26.8` deletes it. Deleting the `UNDEFINED_TABLE`
    /// arm fails this case.
    #[test]
    fn an_absent_snapshot_relation_answers_with_the_designed_refusal() {
        let absent = Err(PgError::QueryError((
            UNDEFINED_TABLE.to_string(),
            "relation \"wamn_catalog\" does not exist".to_string(),
        )));
        assert_eq!(catalog_from_rows(absent), Err(no_catalog_snapshot()));
    }

    /// The refusal is reached by two roads — absent relation and installed but
    /// unpublished — and both must be indistinguishable to the guest.
    #[test]
    fn an_unpublished_snapshot_answers_the_same_way_as_an_absent_one() {
        assert_eq!(
            catalog_from_rows(Ok(Vec::new())),
            Err(no_catalog_snapshot())
        );
    }

    /// Only `42P01` is absorbed. Every other failure still surfaces its own
    /// error, so parking this path did not swallow real faults.
    #[test]
    fn other_query_failures_are_not_absorbed_into_the_refusal() {
        let denied = catalog_from_rows(Err(PgError::PermissionDenied));
        assert_eq!(denied, Err(sdk::PgCapError::PermissionDenied));

        let other = catalog_from_rows(Err(PgError::QueryError((
            "42501".to_string(),
            "permission denied for table wamn_catalog".to_string(),
        ))));
        assert_ne!(other, Err(no_catalog_snapshot()));
    }

    /// The success path is unchanged by the parking.
    #[test]
    fn a_published_snapshot_still_returns_its_document() {
        let published = Ok(vec![vec![SqlValue::Text("{\"catalog-id\":\"c\"}".into())]]);
        assert_eq!(
            catalog_from_rows(published),
            Ok("{\"catalog-id\":\"c\"}".to_string())
        );
    }

    fn supplied_plan(flow_id: &str, source_artifact_hash: &str) -> plan_supply::ResolutionPlan {
        let entry_id = ExecutionNodeId::new("request").unwrap();
        let respond_id = ExecutionNodeId::new("respond").unwrap();
        let guard = json!({"type": "object"});
        let plan = ExecutionPlanV2::new(
            ExecutionRuntimeRevision {
                flowrunner_component_digest: hash('a'),
                effect_provider_revision: hash('b'),
                host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.into(),
            },
            source_artifact_hash,
            ExecutionPlanBody {
                entry_instruction: entry_id.clone(),
                nodes: vec![
                    ExecutionPlanNode {
                        local_node_id: entry_id.clone(),
                        source_node_id: "request".into(),
                        node_type: "request".into(),
                        config: json!({"input-schema": guard.clone()}),
                        effect_policy: ExecutionEffectPolicy::Pure,
                        source_connection_requirement: None,
                    },
                    ExecutionPlanNode {
                        local_node_id: respond_id.clone(),
                        source_node_id: "respond".into(),
                        node_type: "respond".into(),
                        config: json!({"status": 200}),
                        effect_policy: ExecutionEffectPolicy::Pure,
                        source_connection_requirement: None,
                    },
                ],
                edges: vec![wamn_catalog::ExecutionPlanEdge {
                    source: entry_id.clone(),
                    source_port: "main".into(),
                    destination: respond_id.clone(),
                    destination_port: None,
                    fan_out_ordinal: 0,
                }],
                root_terminal_behavior: RootTerminalBehavior::Respond {
                    responders: vec![respond_id],
                },
                entry_input_schema_guard: guard.clone(),
                callable_contract: Some(CallableContract {
                    version: wamn_catalog::CALLABLE_CONTRACT_VERSION.into(),
                    input_schema_hash: entry_input_schema_hash(&guard),
                    return_contract: CallableReturnContract::UntypedJsonBody,
                    effect_ceiling: CallableEffectCeiling::Effectful,
                }),
                source_map: vec![
                    wamn_catalog::ExecutionSourceMapEntry {
                        local_node_id: entry_id,
                        source_node_id: "request".into(),
                    },
                    wamn_catalog::ExecutionSourceMapEntry {
                        local_node_id: ExecutionNodeId::new("respond").unwrap(),
                        source_node_id: "respond".into(),
                    },
                ],
            },
        )
        .unwrap();
        let exact_bytes = serde_json::to_vec(&plan).unwrap();
        plan_supply::ResolutionPlan {
            flow_id: flow_id.into(),
            execution_bundle_hash: execution_bundle_hash(&exact_bytes),
            source_artifact_hash: source_artifact_hash.into(),
            exact_bytes,
        }
    }

    #[test]
    fn run_remains_hard_refused_before_effect_activation() {
        assert_eq!(
            <Component as Guest>::run("run".into(), "{}".into()),
            Err(EXECUTION_INTERPRETER_REFUSAL.to_string())
        );
    }

    #[test]
    fn supplied_snapshot_verifies_every_plan_and_exact_root() {
        let root = supplied_plan("root", &hash('c'));
        let callee = supplied_plan("callee", &hash('d'));
        let snapshot = verify_resolution_snapshot(plan_supply::RunResolutionSnapshot {
            root_flow_id: "root".into(),
            root_execution_bundle_hash: root.execution_bundle_hash.clone(),
            plans: vec![callee, root],
        })
        .unwrap();

        assert_eq!(snapshot.root().flow_id(), "root");
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.plan("callee").is_some());
        assert!(snapshot.plan("moving-head-name").is_none());
    }

    #[test]
    fn forged_callee_bytes_are_refused_before_snapshot_construction() {
        let mut root = supplied_plan("root", &hash('c'));
        root.exact_bytes.push(b' ');
        let error = verify_resolution_snapshot(plan_supply::RunResolutionSnapshot {
            root_flow_id: "root".into(),
            root_execution_bundle_hash: root.execution_bundle_hash.clone(),
            plans: vec![root],
        })
        .unwrap_err();
        assert_eq!(error, "run-resolution-hash-mismatch");
    }

    #[test]
    fn duplicate_flow_or_wrong_root_identity_is_refused() {
        let root = supplied_plan("root", &hash('c'));
        let duplicate = root.clone();
        let error = verify_resolution_snapshot(plan_supply::RunResolutionSnapshot {
            root_flow_id: "root".into(),
            root_execution_bundle_hash: root.execution_bundle_hash.clone(),
            plans: vec![root.clone(), duplicate],
        })
        .unwrap_err();
        assert_eq!(error, "run-resolution-duplicate-flow");

        let error = verify_resolution_snapshot(plan_supply::RunResolutionSnapshot {
            root_flow_id: "root".into(),
            root_execution_bundle_hash: hash('f'),
            plans: vec![root],
        })
        .unwrap_err();
        assert_eq!(error, "run-resolution-root-mismatch");
    }
}
