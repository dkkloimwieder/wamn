//! Frontend-neutral authoring command and projection contracts.
//!
//! This crate is data only. Git, HTTP, CLI, and future visual clients adapt
//! the same messages to the canonical application handlers. Authenticated
//! principal, Git provenance, credentials, endpoints, database authority, and
//! frontend state are deliberately absent from client-controlled documents.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Authoring contract version implemented by this crate.
///
/// `0.1.x` changes may only add compatible semantics. A breaking wire change
/// requires a new minor contract and an explicit compatibility path.
pub const SCHEMA_VERSION: &str = "0.1";

/// A complete request or response document on the authoring contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "document",
    content = "body",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringDocument {
    Request(AuthoringRequest),
    Response(Box<AuthoringResponse>),
}

/// One idempotent client command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AuthoringRequest {
    #[schemars(schema_with = "schema_version_schema")]
    pub schema_version: String,
    /// Client-generated correlation and exact-retry identity.
    pub command_id: String,
    pub command: AuthoringCommand,
}

/// Result or typed refusal for one command identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AuthoringResponse {
    #[schemars(schema_with = "schema_version_schema")]
    pub schema_version: String,
    pub command_id: String,
    pub outcome: AuthoringOutcome,
}

/// Frontend-independent command inventory for the minimum authoring loop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "input",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringCommand {
    SaveFlowDraft(SaveFlowDraft),
    Validate(ValidateDraft),
    DraftRun(DraftRun),
    SuiteRun(SuiteRun),
    Publish(PublishValidatedDraft),
    SuiteProjection(AuthoringReportQuery),
}

/// Stable command names used to attribute a refusal without parsing prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AuthoringCommandKind {
    SaveFlowDraft,
    Validate,
    DraftRun,
    SuiteRun,
    Publish,
    SuiteProjection,
}

/// A successful typed result or a typed product refusal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "status",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringOutcome {
    Completed(Box<AuthoringSuccess>),
    Refused(CommandRefusal),
}

/// The successful result paired with each command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "command",
    content = "result",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringSuccess {
    SaveFlowDraft(DraftIdentity),
    Validate(ValidatedDraftIdentity),
    DraftRun(DraftRunReceipt),
    SuiteRun(SuiteRunReceipt),
    Publish(PublishedFlowIdentity),
    SuiteProjection(Box<SuiteProjectionState>),
}

/// Project and environment selected by a client.
///
/// Organization and principal are authenticated adapter context, not fields a
/// client may assert in this document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AuthoringScope {
    pub project_id: String,
    pub environment: String,
}

/// Save one flow draft document under optimistic revision control.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SaveFlowDraft {
    pub scope: AuthoringScope,
    pub draft_id: String,
    pub flow_id: String,
    /// Zero creates a draft; a positive value replaces exactly that revision.
    pub expected_revision: u64,
    /// Exact UTF-8 flow document text. Save does not parse or validate it;
    /// [`AuthoringCommand::Validate`] owns that boundary.
    pub definition: String,
}

/// Select one exact mutable draft revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DraftRevisionRef {
    pub draft_id: String,
    pub revision: u64,
}

/// Select one stored suite without exposing its storage location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SuiteRef {
    pub suite_id: String,
    pub flow_version: u32,
}

/// Validate one exact saved revision for the selected stored suite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidateDraft {
    pub scope: AuthoringScope,
    pub draft: DraftRevisionRef,
    pub suite: SuiteRef,
}

/// An opaque reference to the exact validated executable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidatedDraftRef {
    pub validated_draft_id: String,
}

/// Execute one input against an exact validated draft.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DraftRun {
    pub scope: AuthoringScope,
    pub validated_draft: ValidatedDraftRef,
    pub input: Value,
}

/// Execute a stored suite against an exact validated draft.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SuiteRun {
    pub scope: AuthoringScope,
    pub validated_draft: ValidatedDraftRef,
    pub suite: SuiteRef,
}

/// Publish exactly the executable proven by a successful suite report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PublishValidatedDraft {
    pub scope: AuthoringScope,
    pub validated_draft: ValidatedDraftRef,
    pub successful_report_id: String,
}

/// Query a durable suite report projection by stable identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AuthoringReportQuery {
    pub scope: AuthoringScope,
    pub report_id: String,
}

/// Stable identity returned after a draft save.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DraftIdentity {
    pub draft_id: String,
    pub flow_id: String,
    pub revision: u64,
}

/// Applied catalog identity pinned by validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CatalogIdentity {
    pub catalog_id: String,
    pub version: u32,
}

/// Public pins for the exact executable accepted by validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidatedDraftIdentity {
    pub validated_draft_id: String,
    pub draft: DraftIdentity,
    pub runtime_flow_version: u32,
    pub artifact_hash: String,
    pub execution_bundle_hash: String,
    pub catalog: CatalogIdentity,
    pub environment: String,
}

/// Receipt for one admitted draft run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DraftRunReceipt {
    pub run_id: String,
    pub validated_draft: ValidatedDraftRef,
}

/// Receipt for one durable suite command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SuiteRunReceipt {
    pub report_id: String,
    pub execution_id: String,
    pub validated_draft: ValidatedDraftRef,
}

/// Immutable identity produced by publishing the tested draft.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PublishedFlowIdentity {
    pub flow_id: String,
    pub version: u32,
    pub artifact_hash: String,
}

/// A refusal attributed to an exact command kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CommandRefusal {
    pub command: AuthoringCommandKind,
    pub reason: AuthoringRefusal,
}

/// Product refusals; infrastructure faults remain outside this taxonomy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringRefusal {
    AuthorizationDenied,
    UnsupportedContractVersion {
        requested: String,
        supported: String,
    },
    RevisionConflict {
        expected_revision: u64,
        actual_revision: Option<u64>,
    },
    ResourceNotFound {
        resource: ResourceKind,
        id: String,
    },
    InvalidDraft {
        issues: Vec<ValidationIssue>,
    },
    CatalogDrift,
    UnresolvedNodes {
        node_types: Vec<String>,
    },
    ValidatedDraftDrift,
    DraftConnectionsDenied {
        connection_names: Vec<String>,
    },
    PublishBlockedBySuite {
        report_id: String,
    },
    PublishExecutableDrift,
    PublishBlockedByNonterminalRuns {
        run_ids: Vec<String>,
    },
}

/// Stable resource categories for typed not-found refusals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceKind {
    Draft,
    DraftRevision,
    ValidatedDraft,
    Suite,
    Report,
}

/// Machine-readable validation issue; `message` is explanatory only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidationIssue {
    pub severity: ValidationSeverity,
    pub code: String,
    pub path: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ValidationSeverity {
    Error,
    Warning,
}

/// Durable read state for a suite projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "state",
    content = "report",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum SuiteProjectionState {
    NotFound,
    Pending(PendingSuiteProjection),
    Finalized(Box<DraftSuiteProjection>),
}

/// A report reservation that cannot yet finalize without fabricating evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PendingSuiteProjection {
    pub report_id: String,
    pub execution_id: String,
    pub validated_draft: ValidatedDraftRef,
    pub reason: PendingReportReason,
    pub captured_case_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum PendingReportReason {
    AwaitingAdmission,
    CaptureInterrupted { run_ids: Vec<String> },
}

/// Final client-renderable result for one draft suite.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DraftSuiteProjection {
    #[schemars(schema_with = "schema_version_schema")]
    pub projection_version: String,
    pub report_id: String,
    pub execution_id: String,
    pub draft: ValidatedDraftIdentity,
    pub suite: SuiteRef,
    pub outcome: SuiteOutcome,
    pub edit_to_run_ms: Option<u64>,
    pub cases: Vec<CaseResultProjection>,
    pub nodes: Vec<NodeResultProjection>,
    pub branches: Vec<BranchCoverageProjection>,
    pub edges: Vec<EdgeCoverageProjection>,
}

/// Suite completion is either pass/fail or a typed refusal; contradictory
/// `passed + refusal` documents are not representable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "state",
    content = "refusal",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum SuiteOutcome {
    Passed,
    Failed,
    Refused(SuiteExecutionRefusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PassFail {
    Passed,
    Failed,
}

/// One stored case's stable result and failure link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CaseResultProjection {
    pub case_id: String,
    pub run_id: String,
    pub outcome: PassFail,
    pub failure: Option<FailureDetail>,
}

/// Failure classification; clients never need to parse an error message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FailureDetail {
    pub kind: FailureKind,
    pub node_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    Terminal,
    RetryExhausted,
    InvalidInput,
    RunawayBudget,
    Cancelled,
    InfrastructureFault,
}

/// Aggregate result for one stable node across the suite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NodeResultProjection {
    pub node_id: String,
    pub outcome: NodeOutcome,
    pub observed_case_ids: Vec<String>,
    pub failed_case_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NodeOutcome {
    Passed,
    Failed,
    NotObserved,
    Unknown,
}

/// Stable identity of one authored branch: source node plus emitted port.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BranchIdentity {
    pub from_node_id: String,
    pub from_port: String,
}

/// Required, nullable target-port component of an edge identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum EdgeInputPort {
    Named(String),
    Default,
}

/// Stable identity of one authored edge.
///
/// `to_port` is required on the wire and may be null, preserving the full
/// canonical `(from, from-port, to, to-port)` tuple without omission rules.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct EdgeIdentity {
    pub from_node_id: String,
    pub from_port: String,
    pub to_node_id: String,
    pub to_port: EdgeInputPort,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BranchCoverageProjection {
    pub branch: BranchIdentity,
    pub coverage: CoverageState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct EdgeCoverageProjection {
    pub edge: EdgeIdentity,
    pub coverage: CoverageState,
}

/// Explicit coverage state; absence never means uncovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CoverageState {
    Covered,
    NotCovered,
    NotObserved,
    Unknown,
}

/// Typed refusal retained in a finalized suite report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum SuiteExecutionRefusal {
    UndrivableNodes { node_types: Vec<String> },
    ValidatedDraftDrift,
    DraftConnectionsDenied { connection_names: Vec<String> },
}

/// Decode a contract document and reject missing, malformed, or unsupported
/// versions before an application handler is selected.
pub fn decode_document(input: &str) -> Result<AuthoringDocument, ContractDecodeError> {
    let document: AuthoringDocument =
        serde_json::from_str(input).map_err(ContractDecodeError::Json)?;
    let version = match &document {
        AuthoringDocument::Request(request) => &request.schema_version,
        AuthoringDocument::Response(response) => &response.schema_version,
    };
    if version != SCHEMA_VERSION {
        return Err(ContractDecodeError::UnsupportedContractVersion {
            requested: version.clone(),
        });
    }

    if let Some(projection) = finalized_projection(&document)
        && projection.projection_version != SCHEMA_VERSION
    {
        return Err(ContractDecodeError::UnsupportedProjectionVersion {
            requested: projection.projection_version.clone(),
        });
    }

    Ok(document)
}

fn finalized_projection(document: &AuthoringDocument) -> Option<&DraftSuiteProjection> {
    let AuthoringDocument::Response(response) = document else {
        return None;
    };
    let AuthoringOutcome::Completed(success) = &response.outcome else {
        return None;
    };
    let AuthoringSuccess::SuiteProjection(state) = success.as_ref() else {
        return None;
    };
    let SuiteProjectionState::Finalized(projection) = state.as_ref() else {
        return None;
    };
    Some(projection)
}

/// Decode failure before application dispatch.
#[derive(Debug)]
pub enum ContractDecodeError {
    Json(serde_json::Error),
    UnsupportedContractVersion { requested: String },
    UnsupportedProjectionVersion { requested: String },
}

impl fmt::Display for ContractDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid authoring document: {error}"),
            Self::UnsupportedContractVersion { requested } => write!(
                formatter,
                "unsupported authoring contract version {requested}; supported version is {SCHEMA_VERSION}"
            ),
            Self::UnsupportedProjectionVersion { requested } => write!(
                formatter,
                "unsupported authoring projection version {requested}; supported version is {SCHEMA_VERSION}"
            ),
        }
    }
}

impl std::error::Error for ContractDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::UnsupportedContractVersion { .. } | Self::UnsupportedProjectionVersion { .. } => {
                None
            }
        }
    }
}

/// Language-neutral JSON Schema for every public request, result, refusal,
/// identity, and projection reachable from [`AuthoringDocument`].
pub fn json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(AuthoringDocument)).expect("schema serializes")
}

/// Canonical pretty JSON bytes for the checked-in schema contract.
pub fn json_schema_string() -> String {
    let mut schema = serde_json::to_string_pretty(&json_schema()).expect("schema serializes");
    schema.push('\n');
    schema
}

fn schema_version_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    let mut schema = schemars::schema::SchemaObject {
        instance_type: Some(schemars::schema::InstanceType::String.into()),
        ..Default::default()
    };
    schema.enum_values = Some(vec![Value::String(SCHEMA_VERSION.to_owned())]);
    schemars::schema::Schema::Object(schema)
}
