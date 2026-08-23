//! Frontend-neutral authoring command and query contracts.
//!
//! MVP outcome: publish gate.
//!
//! This crate is data only. Git, HTTP, CLI, and future visual clients adapt
//! the same messages to canonical application handlers. Authenticated
//! principals, credentials, endpoints, database authority, and frontend state
//! are deliberately absent from client-controlled documents.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Authoring contract version shipped for the MVP.
pub const SCHEMA_VERSION: &str = "0.1";

/// Largest query correlation identifier, measured in exact UTF-8 bytes.
///
/// Sixty-four bytes admits UUIDs and trace-style identifiers with a short
/// human-readable prefix while keeping the trace-only value tightly bounded.
pub const MAX_QUERY_ID_BYTES: usize = 64;

/// Maximum number of cases one flow document may carry.
pub const MAX_TEST_SET_CASES: usize = 256;

/// Largest integer every `format: uint64` field on this contract may carry.
pub const SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

/// A `uint64` wire value inside the exactly representable domain `[0, 2^53-1]`.
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

impl From<SafeUint64> for i64 {
    fn from(value: SafeUint64) -> Self {
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
    fn is_referenceable() -> bool {
        false
    }

    fn schema_name() -> String {
        "SafeUint64".to_owned()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        let mut schema = <u64 as JsonSchema>::json_schema(generator).into_object();
        schema
            .extensions
            .insert("maximum".to_owned(), Value::from(SAFE_INTEGER_MAX));
        schemars::schema::Schema::Object(schema)
    }
}

/// An integer outside the exactly representable `uint64` wire domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SafeIntegerError {
    pub value: i128,
}

impl fmt::Display for SafeIntegerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "uint64 value {} is outside the exactly representable authoring wire domain [0, {SAFE_INTEGER_MAX}]",
            self.value
        )
    }
}

impl std::error::Error for SafeIntegerError {}

/// Non-empty, bounded, trace-only query correlation identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QueryId(Box<str>);

impl QueryId {
    /// Borrow the exact correlation identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for QueryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for QueryId {
    type Error = QueryIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.len() > MAX_QUERY_ID_BYTES {
            return Err(QueryIdError {
                byte_length: value.len(),
            });
        }
        Ok(Self(value.into_boxed_str()))
    }
}

impl Serialize for QueryId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for QueryId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for QueryId {
    fn schema_name() -> String {
        "QueryId".to_owned()
    }

    fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        let mut schema = schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            ..Default::default()
        };
        schema.string = Some(Box::new(schemars::schema::StringValidation {
            min_length: Some(1),
            ..Default::default()
        }));
        schema.extensions.insert(
            "x-max-utf8-bytes".to_owned(),
            Value::from(MAX_QUERY_ID_BYTES),
        );
        schemars::schema::Schema::Object(schema)
    }
}

/// A query correlation identifier outside its public byte bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryIdError {
    pub byte_length: usize,
}

impl fmt::Display for QueryIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "query-id is {} bytes; expected 1..={MAX_QUERY_ID_BYTES}",
            self.byte_length
        )
    }
}

impl std::error::Error for QueryIdError {}

/// A complete request or response document on the authoring contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "document",
    content = "body",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringDocument {
    Request(Box<AuthoringRequestEnvelope>),
    Response(Box<AuthoringResponseEnvelope>),
}

/// The structurally disjoint command or query request body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AuthoringRequestEnvelope {
    Command(AuthoringRequest),
    Query(AuthoringQueryRequest),
}

/// The structurally disjoint command or query response body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AuthoringResponseEnvelope {
    Command(AuthoringResponse),
    Query(AuthoringQueryResponse),
}

/// One idempotent ledgered command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AuthoringRequest {
    #[schemars(schema_with = "schema_version_schema")]
    pub schema_version: String,
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

/// One correlation-only, non-ledgered query.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AuthoringQueryRequest {
    #[schemars(schema_with = "schema_version_schema")]
    pub schema_version: String,
    pub query_id: QueryId,
    pub query: AuthoringQuery,
}

/// Result or typed refusal for one correlation-only query.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AuthoringQueryResponse {
    #[schemars(schema_with = "schema_version_schema")]
    pub schema_version: String,
    pub query_id: QueryId,
    pub outcome: AuthoringQueryOutcome,
}

/// Complete five-command authoring inventory.
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
    TestSetRun(TestSetRun),
    Publish(PublishValidatedDraft),
}

/// Stable command names used by response and ledger vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AuthoringCommandKind {
    SaveFlowDraft,
    Validate,
    DraftRun,
    TestSetRun,
    Publish,
}

/// Complete two-query authoring inventory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "input",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringQuery {
    ReadDraft(ReadDraft),
    GetReport(GetReport),
}

/// Stable query names used only for typed response attribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AuthoringQueryKind {
    ReadDraft,
    GetReport,
}

/// A successful command result or operation-specific refusal.
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

/// Successful result paired with each command.
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
    TestSetRun(TestSetRunReceipt),
    Publish(PublishedFlowIdentity),
}

/// An operation-specific command refusal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "command",
    content = "reason",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum CommandRefusal {
    SaveFlowDraft(SaveFlowDraftRefusal),
    Validate(ValidateRefusal),
    DraftRun(DraftRunRefusal),
    TestSetRun(TestSetRunRefusal),
    Publish(PublishRefusal),
}

/// A successful query result or operation-specific refusal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "status",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringQueryOutcome {
    Completed(Box<AuthoringQuerySuccess>),
    Refused(QueryRefusal),
}

/// Successful result paired with each query.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "query",
    content = "result",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringQuerySuccess {
    ReadDraft(DraftDocument),
    GetReport(ReportProjection),
}

/// An operation-specific query refusal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "query",
    content = "reason",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum QueryRefusal {
    ReadDraft(ReadDraftRefusal),
    GetReport(GetReportRefusal),
}

/// Project and environment selected by a client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AuthoringScope {
    pub project_id: String,
    pub environment: String,
}

/// Optional, untrusted source attribution supplied by a checkout client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CommitProvenance {
    pub commit: String,
    pub r#ref: Option<String>,
    pub dirty: bool,
}

/// Save one flow draft document under optimistic revision control.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SaveFlowDraft {
    pub scope: AuthoringScope,
    pub draft_id: String,
    pub flow_id: String,
    pub expected_revision: SafeUint64,
    pub definition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<CommitProvenance>,
}

/// Select one exact mutable draft revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DraftRevisionRef {
    pub draft_id: String,
    pub revision: SafeUint64,
}

/// Validate one exact saved draft revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidateDraft {
    pub scope: AuthoringScope,
    pub draft: DraftRevisionRef,
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
    #[serde(default, skip_serializing_if = "draft_run_capture_is_full")]
    pub capture: DraftRunCapture,
}

/// Capture choice for one direct draft run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DraftRunCapture {
    #[default]
    Full,
    Off,
}

/// Execute the validated draft's own bounded `cases` array.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TestSetRun {
    pub scope: AuthoringScope,
    pub validated_draft: ValidatedDraftRef,
}

/// Publish exactly the executable proven by a successful report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PublishValidatedDraft {
    pub scope: AuthoringScope,
    pub validated_draft: ValidatedDraftRef,
    pub successful_report_id: String,
}

/// Read one exact saved draft revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReadDraft {
    pub scope: AuthoringScope,
    pub draft: DraftRevisionRef,
}

/// Read one pending or finalized immutable report projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GetReport {
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

/// Public pins for the one own-flow executable accepted by validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidatedDraftIdentity {
    pub validated_draft_id: String,
    pub draft: DraftIdentity,
    pub runtime_flow_version: u32,
    pub artifact_hash: String,
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

/// Receipt for one accepted test-set run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TestSetRunReceipt {
    pub report_id: String,
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

/// Exact saved draft projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DraftDocument {
    pub draft: DraftIdentity,
    pub definition: String,
}

/// Pending or immutable finalized test report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum ReportProjection {
    #[schemars(rename_all = "kebab-case")]
    Pending {
        report_id: String,
        validated_draft: ValidatedDraftRef,
    },
    #[schemars(rename_all = "kebab-case")]
    Finalized {
        report_id: String,
        validated_draft: ValidatedDraftRef,
        passed: bool,
        summary: Value,
        resolution_map: Value,
    },
}

/// Refusals owned by `save-flow-draft`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum SaveFlowDraftRefusal {
    AuthorizationDenied,
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
    CommandIdReuse,
}

/// Refusals owned by `validate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum ValidateRefusal {
    AuthorizationDenied,
    #[schemars(rename_all = "kebab-case")]
    UnsupportedContractVersion {
        requested: String,
        supported: String,
    },
    #[schemars(rename_all = "kebab-case")]
    DraftRevisionNotFound {
        draft_id: String,
        revision: SafeUint64,
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
    #[schemars(rename_all = "kebab-case")]
    UnresolvableCalleeName {
        site: String,
        flow_id: String,
    },
    #[schemars(rename_all = "kebab-case")]
    MissingRecordedCallability {
        site: String,
        flow_id: String,
    },
    #[schemars(rename_all = "kebab-case")]
    ContractIncompatibility {
        site: String,
        flow_id: String,
    },
    #[schemars(rename_all = "kebab-case")]
    DraftConnectionsDenied {
        connection_names: Vec<String>,
    },
    CommandIdReuse,
}

/// Refusals owned by `draft-run`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum DraftRunRefusal {
    AuthorizationDenied,
    #[schemars(rename_all = "kebab-case")]
    UnsupportedContractVersion {
        requested: String,
        supported: String,
    },
    #[schemars(rename_all = "kebab-case")]
    ValidatedDraftNotFound {
        validated_draft_id: String,
    },
    ValidatedDraftDrift,
    #[schemars(rename_all = "kebab-case")]
    DraftConnectionsDenied {
        connection_names: Vec<String>,
    },
    CommandIdReuse,
}

/// Refusals owned by `test-set-run`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum TestSetRunRefusal {
    AuthorizationDenied,
    #[schemars(rename_all = "kebab-case")]
    UnsupportedContractVersion {
        requested: String,
        supported: String,
    },
    #[schemars(rename_all = "kebab-case")]
    ValidatedDraftNotFound {
        validated_draft_id: String,
    },
    ValidatedDraftDrift,
    #[schemars(rename_all = "kebab-case")]
    InvalidTestSet {
        detail: String,
    },
    #[schemars(rename_all = "kebab-case")]
    DraftConnectionsDenied {
        connection_names: Vec<String>,
    },
    CommandIdReuse,
}

/// Refusals owned by `publish`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum PublishRefusal {
    AuthorizationDenied,
    #[schemars(rename_all = "kebab-case")]
    UnsupportedContractVersion {
        requested: String,
        supported: String,
    },
    #[schemars(rename_all = "kebab-case")]
    ValidatedDraftNotFound {
        validated_draft_id: String,
    },
    #[schemars(rename_all = "kebab-case")]
    ReportNotFound {
        report_id: String,
    },
    ReportNotSuccessful,
    PublishExecutableDrift,
    #[schemars(rename_all = "kebab-case")]
    PublishBlockedByNonterminalRuns {
        run_ids: Vec<String>,
    },
    CommandIdReuse,
}

/// Refusals owned by `read-draft`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum ReadDraftRefusal {
    AuthorizationDenied,
    #[schemars(rename_all = "kebab-case")]
    UnsupportedContractVersion {
        requested: String,
        supported: String,
    },
    #[schemars(rename_all = "kebab-case")]
    DraftRevisionNotFound {
        draft_id: String,
        revision: SafeUint64,
    },
}

/// Refusals owned by `get-report`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum GetReportRefusal {
    AuthorizationDenied,
    #[schemars(rename_all = "kebab-case")]
    UnsupportedContractVersion {
        requested: String,
        supported: String,
    },
    #[schemars(rename_all = "kebab-case")]
    ReportNotFound {
        report_id: String,
    },
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

/// Validation issue severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ValidationSeverity {
    Error,
    Warning,
}

/// Decode a contract document and reject unsupported versions before dispatch.
pub fn decode_document(input: &str) -> Result<AuthoringDocument, ContractDecodeError> {
    let document: AuthoringDocument =
        serde_json::from_str(input).map_err(ContractDecodeError::json)?;
    let version = match &document {
        AuthoringDocument::Request(request) => match request.as_ref() {
            AuthoringRequestEnvelope::Command(request) => &request.schema_version,
            AuthoringRequestEnvelope::Query(request) => &request.schema_version,
        },
        AuthoringDocument::Response(response) => match response.as_ref() {
            AuthoringResponseEnvelope::Command(response) => &response.schema_version,
            AuthoringResponseEnvelope::Query(response) => &response.schema_version,
        },
    };
    if version != SCHEMA_VERSION {
        return Err(ContractDecodeError::unsupported(version.clone()));
    }
    Ok(document)
}

/// Stable decode failure classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContractDecodeErrorKind {
    Json,
    UnsupportedContractVersion,
}

/// Decode failure before application dispatch.
#[derive(Debug)]
pub struct ContractDecodeError {
    kind: ContractDecodeErrorKind,
    requested: Option<Box<str>>,
    source: Option<serde_json::Error>,
}

impl ContractDecodeError {
    fn json(source: serde_json::Error) -> Self {
        Self {
            kind: ContractDecodeErrorKind::Json,
            requested: None,
            source: Some(source),
        }
    }

    fn unsupported(requested: String) -> Self {
        Self {
            kind: ContractDecodeErrorKind::UnsupportedContractVersion,
            requested: Some(requested.into_boxed_str()),
            source: None,
        }
    }

    pub const fn kind(&self) -> ContractDecodeErrorKind {
        self.kind
    }

    pub fn requested(&self) -> Option<&str> {
        self.requested.as_deref()
    }
}

impl fmt::Display for ContractDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ContractDecodeErrorKind::Json => write!(
                formatter,
                "invalid authoring document: {}",
                self.source
                    .as_ref()
                    .expect("JSON decode error retains its source")
            ),
            ContractDecodeErrorKind::UnsupportedContractVersion => write!(
                formatter,
                "unsupported authoring contract version {}; supported version is {SCHEMA_VERSION}",
                self.requested
                    .as_deref()
                    .expect("unsupported version retains its literal")
            ),
        }
    }
}

impl std::error::Error for ContractDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// Language-neutral JSON Schema for every public authoring document.
pub fn json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(AuthoringDocument)).expect("schema serializes")
}

/// Canonical pretty JSON bytes for the authoring contract.
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

fn draft_run_capture_is_full(capture: &DraftRunCapture) -> bool {
    *capture == DraftRunCapture::Full
}
