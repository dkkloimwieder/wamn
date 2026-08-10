//! Frontend-neutral authoring command and projection contracts.
//!
//! This crate is data only. Git, HTTP, CLI, and future visual clients adapt
//! the same messages to the canonical application handlers. Authenticated
//! principal, Git provenance, credentials, endpoints, database authority, and
//! frontend state are deliberately absent from client-controlled documents.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Authoring contract version implemented by this crate.
///
/// `0.1.x` changes may only add compatible semantics. A breaking wire change
/// requires a new minor contract and an explicit compatibility path.
pub const SCHEMA_VERSION: &str = "0.1";

/// Largest integer every `format: uint64` field on this contract may carry.
///
/// `2^53 - 1` is the largest integer an IEEE-754 double holds exactly, so it is
/// the whole wire domain a JavaScript `Number` can round-trip without loss. The
/// schema publishes it as `maximum`; see `docs/archive/authoring/authoring-surface.md`.
pub const SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

/// A `uint64` wire value inside the exactly representable domain `[0, 2^53-1]`.
///
/// The type is the boundary: an out-of-domain integer is refused on decode and
/// is unrepresentable on encode, so no client ever receives a rounded counter.
/// Storage behind every one of these fields is PostgreSQL `bigint`, and each
/// one carries a server-assigned counter or a latency that cannot legitimately
/// approach the bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SafeUint64(u64);

impl fmt::Display for SafeUint64 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<u64> for SafeUint64 {
    type Error = SafeIntegerError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value > SAFE_INTEGER_MAX {
            return Err(SafeIntegerError {
                value: i128::from(value),
            });
        }
        Ok(Self(value))
    }
}

/// Accepts the `bigint` column values these fields are stored in.
impl TryFrom<i64> for SafeUint64 {
    type Error = SafeIntegerError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        u64::try_from(value)
            .map_err(|_| SafeIntegerError {
                value: i128::from(value),
            })
            .and_then(Self::try_from)
    }
}

impl From<SafeUint64> for u64 {
    fn from(value: SafeUint64) -> Self {
        value.0
    }
}

/// The domain fits `bigint`, so the storage conversion is total.
impl From<SafeUint64> for i64 {
    fn from(value: SafeUint64) -> Self {
        // `SAFE_INTEGER_MAX` is `2^53 - 1`, well inside `i64`.
        value.0 as Self
    }
}

impl Serialize for SafeUint64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0)
    }
}

impl<'de> Deserialize<'de> for SafeUint64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u64::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for SafeUint64 {
    /// Inlined so each `format: uint64` site publishes `maximum` in place.
    fn is_referenceable() -> bool {
        false
    }

    fn schema_name() -> String {
        "SafeUint64".to_owned()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        let mut schema = <u64 as JsonSchema>::json_schema(generator).into_object();
        // `NumberValidation::maximum` is `f64`, and an `f64` bound serializes as
        // `9007199254740991.0` — which `serde_json` reads back as
        // `9007199254740990`, one below the bound this contract exists to state
        // exactly. Publishing through `extensions` keeps `maximum` an integer
        // literal that every parser reproduces exactly.
        schema
            .extensions
            .insert("maximum".to_owned(), Value::from(SAFE_INTEGER_MAX));
        schemars::schema::Schema::Object(schema)
    }
}

/// An integer outside the exactly representable `uint64` wire domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SafeIntegerError {
    /// The refused value, as read from the wire or from `bigint` storage.
    pub value: i128,
}

impl fmt::Display for SafeIntegerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.value;
        write!(
            formatter,
            "uint64 value {value} is outside the exactly representable authoring wire domain [0, {SAFE_INTEGER_MAX}]"
        )
    }
}

impl std::error::Error for SafeIntegerError {}

/// A complete request or response document on the authoring contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "document",
    content = "body",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringDocument {
    // Both arms are boxed: a request carries the largest command input and a
    // response the largest projection, and neither should set the size of every
    // document value in the process.
    Request(Box<AuthoringRequest>),
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

/// Where a client read the submitted definition from.
///
/// **Attribution only.** A checkout client knows the commit its working tree
/// came from; recording that makes a stored revision traceable to the source it
/// was edited in. It authorizes nothing: it selects no principal, widens no
/// role, and changes no result. Two otherwise identical commands, one with this
/// and one without, produce the same outcome and the same stored document.
///
/// The platform never clones a repository, runs Git, or verifies any of these
/// values. They are the client's claim about its own working tree, retained
/// verbatim beside the verified principal that actually authorized the command.
/// No field here is an identity: a subject lives on the audited principal, and
/// nothing in this object may be read as one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CommitProvenance {
    /// Exact commit the working tree was at.
    pub commit: String,
    /// Ref the checkout was on, or null when the client cannot name one.
    pub r#ref: Option<String>,
    /// Whether the submitted content differs from that commit. A dirty tree
    /// means the content is descended from `commit`, not equal to it.
    pub dirty: bool,
}

/// Save one flow draft document under optimistic revision control.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SaveFlowDraft {
    pub scope: AuthoringScope,
    pub draft_id: String,
    pub flow_id: String,
    /// Zero creates a draft; a positive value replaces exactly that revision.
    pub expected_revision: SafeUint64,
    /// Exact UTF-8 flow document text, stored byte-for-byte. Save does not
    /// parse or validate it, so a half-finished edit is preserved rather than
    /// refused; [`AuthoringCommand::Validate`] owns that boundary.
    pub definition: String,
    /// Optional source attribution. Omitting it is always allowed and always
    /// equivalent in outcome.
    #[serde(default)]
    pub provenance: Option<CommitProvenance>,
}

/// Select one exact mutable draft revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DraftRevisionRef {
    pub draft_id: String,
    pub revision: SafeUint64,
}

/// Select one stored suite without exposing its storage location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SuiteRef {
    pub suite_id: String,
    pub flow_version: u32,
}

/// Inline, self-describing test-set document submitted to `test-set-run`.
///
/// `definition` is exact UTF-8 text. Its bytes are the test-set identity
/// preimage; parsing and validation must never normalize or reserialize it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TestSetInput {
    pub definition: String,
}

/// Content identity for one immutable inline test set.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TestSetIdentity {
    pub hash: String,
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
    pub revision: SafeUint64,
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
    // `schemars` 0.8 discards `rename_all_fields`, so every field-carrying
    // variant mirrors it for the generated schema. The serde wire names are
    // unchanged; the schema stops advertising the Rust field spelling.
    #[schemars(rename_all = "kebab-case")]
    UnsupportedContractVersion {
        requested: String,
        supported: String,
    },
    #[schemars(rename_all = "kebab-case")]
    RevisionConflict {
        expected_revision: SafeUint64,
        actual_revision: Option<SafeUint64>,
    },
    #[schemars(rename_all = "kebab-case")]
    ResourceNotFound {
        resource: ResourceKind,
        id: String,
    },
    #[schemars(rename_all = "kebab-case")]
    InvalidDraft {
        issues: Vec<ValidationIssue>,
    },
    CatalogDrift,
    #[schemars(rename_all = "kebab-case")]
    UnresolvedNodes {
        node_types: Vec<String>,
    },
    ValidatedDraftDrift,
    #[schemars(rename_all = "kebab-case")]
    DraftConnectionsDenied {
        connection_names: Vec<String>,
    },
    #[schemars(rename_all = "kebab-case")]
    PublishBlockedBySuite {
        report_id: String,
    },
    PublishExecutableDrift,
    #[schemars(rename_all = "kebab-case")]
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
    // Mirrors `rename_all_fields` for the generated schema; see `AuthoringRefusal`.
    #[schemars(rename_all = "kebab-case")]
    CaptureInterrupted {
        run_ids: Vec<String>,
    },
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
    pub edit_to_run_ms: Option<SafeUint64>,
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
    // Mirrors `rename_all_fields` for the generated schema; see `AuthoringRefusal`.
    #[schemars(rename_all = "kebab-case")]
    UndrivableNodes {
        node_types: Vec<String>,
    },
    ValidatedDraftDrift,
    #[schemars(rename_all = "kebab-case")]
    DraftConnectionsDenied {
        connection_names: Vec<String>,
    },
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
