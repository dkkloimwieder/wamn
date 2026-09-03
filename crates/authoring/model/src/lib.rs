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

/// Complete two-command authoring inventory.
///
/// `gate` is spelled `gate` on the wire (wamn-0h0g.7.11). The variant name and
/// the wire literal are the one honest name the owner ratified — one verb
/// produces reports, and that judgment is gating — so the kebab-case rule alone
/// renders it and no `rename` apologises for a second spelling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "input",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringCommand {
    Gate(Gate),
    Publish(PublishValidatedDraft),
}

/// Stable command names used by response and ledger vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AuthoringCommandKind {
    Gate,
    Publish,
}

/// Complete one-query authoring inventory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "input",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum AuthoringQuery {
    GetReport(GetReport),
}

/// Stable query names used only for typed response attribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AuthoringQueryKind {
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
    Gate(GateReceipt),
    Publish(PublishedWiringIdentity),
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
    Gate(GateRefusal),
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

/// An opaque reference to the exact validated executable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidatedDraftRef {
    pub validated_draft_id: String,
}

/// Judge one wiring document against its own bounded `cases` array.
///
/// # This carries the DOCUMENT, and that is the whole of wamn-0h0g.8.28
///
/// It used to carry a reference the server resolved out of `catalog.wirings`.
/// That was leftover coupling from the retired reservation protocol, and
/// EXECUTION refuted it: authorship refuses to insert a wiring without a green
/// report for its own hash, and the only producer of that report resolved its
/// candidate from the very row that could not exist yet. Nothing could ever be
/// gated a first time. The deadlock is PER-DOCUMENT, not per-catalog, so no
/// bootstrap step would have sufficed.
///
/// The ratified stateless-gate model already said the answer in its own
/// sentence: the report is REPRODUCIBLE FROM THE DOCUMENT. So the document IS
/// the input, the judgment is a total function of it, and the gate reads
/// `catalog.wirings` not at all.
///
/// `package_id` and `package_version` ride with it for the same reason
/// `publish` carries them (wamn-0h0g.7.10): compatibility and admitted effect
/// posture are facts of one exact package coordinate, and neither value rides
/// the document. They came off the stored row before; now they are stated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Gate {
    pub scope: AuthoringScope,
    /// Package identity whose admitted component facts judge this document.
    pub package_id: String,
    /// Exact package version those facts are read at.
    pub package_version: String,
    /// The wiring document itself — the same bytes `publish` carries.
    pub document: serde_json::Value,
}

/// Publish one gated wiring document into a named package version.
///
/// # What this carries, and why each field is here (wamn-0h0g.7.10)
///
/// `catalog.wirings` needs seven values. `tenant_id` is the authenticated
/// scope's, `wiring_id` and `version` ride the document, and `graph_json` IS the
/// document — but `package_id` and `package_version` ride NEITHER the
/// document nor its hash, which is why `ctl author-wiring` takes both as
/// separate argv. So the command carries them too, and the row is writable from
/// this input alone.
///
/// `wiring_hash` is DERIVED SERVER-SIDE and is deliberately absent here. The
/// wamn-0h0g.7.8 close ruling rejected a carried proof value as forgeable and
/// replayable and made the verb compute the identity from the bytes; a literal
/// wire hash field the server trusted would reopen it. A client that wants to
/// state its expectation states it out of band, not as the value written.
///
/// `successful_report_id` is GONE. wamn-0h0g.8.5.6 collapsed the report id into
/// `wiring_hash`, and the green-report guard reads the report BY that hash — so
/// the field named an identity the document already determines and had no
/// remaining reader.
///
/// The document rides as its own JSON rather than a typed carrier: these are the
/// exact bytes `catalog.wirings.graph_json` stores, and
/// [`wamn_catalog::WiringDocument::parse`] is the ONE validating reader for
/// them. Typing the field here would put a second structural dialect in front of
/// that one, and — measured — would need `schemars::JsonSchema` derives across
/// `wamn-catalog` and the FROZEN `wamn-event-wire`, which `WiringEventOperation`
/// aliases into. The server parses, validates and hashes; this is the carrier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PublishValidatedDraft {
    pub scope: AuthoringScope,
    /// Package identity the wiring is published into.
    pub package_id: String,
    /// Exact package version whose component facts gate this document.
    pub package_version: String,
    /// The wiring document itself — `catalog.wirings.graph_json`.
    pub document: serde_json::Value,
    /// Optional source attribution written only to the authenticated command audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<CommitProvenance>,
}

/// Read one immutable report projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GetReport {
    pub scope: AuthoringScope,
    pub report_id: String,
}

/// Receipt for one accepted gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GateReceipt {
    pub report_id: String,
    pub validated_draft: ValidatedDraftRef,
}

/// Immutable identity produced by publishing the tested draft.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PublishedWiringIdentity {
    pub wiring_id: String,
    pub version: u32,
    pub artifact_hash: String,
}

/// One immutable finalized gate report.
///
/// `Pending` is DELETED (wamn-0h0g.8.5.5). There is no pending state in a
/// stateless gate: the variant was reachable only while the reservation protocol
/// stood, and the owner ruling of 2026-08-25 struck the whole run-plane report
/// lineage — reservation included. A report either exists for its key or it does
/// not, and `report-not-found` is the truthful answer for the second case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum ReportProjection {
    #[schemars(rename_all = "kebab-case")]
    Finalized {
        report_id: String,
        validated_draft: ValidatedDraftRef,
        passed: bool,
        summary: Value,
    },
}

/// Refusals owned by `gate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum GateRefusal {
    AuthorizationDenied,
    #[schemars(rename_all = "kebab-case")]
    UnsupportedContractVersion {
        requested: String,
        supported: String,
    },
    /// The submitted bytes are not a wiring document valid in the named
    /// package scope.
    ///
    /// This REPLACES the two retired lookup refusals (wamn-0h0g.8.28). Both
    /// described a search of `catalog.wirings` that the gate no longer performs:
    /// a document cannot be missing when the command carries it, and it cannot
    /// diverge from a stored row it is never compared to. What CAN go wrong is
    /// that the bytes do not parse, do not validate, or do not resolve against
    /// the package's admitted component facts, which is this.
    ///
    /// Their exact spellings are pinned as retired in this crate's contract test,
    /// so they are named there and deliberately not repeated here — a doc comment
    /// becomes a schema description, and would resurrect the literal it retires.
    #[schemars(rename_all = "kebab-case")]
    InvalidDocument {
        detail: String,
    },
    #[schemars(rename_all = "kebab-case")]
    InvalidTestSet {
        detail: String,
    },
    /// The candidate reaches a component whose admitted effects projection is
    /// non-empty.
    ///
    /// A gate is a JUDGMENT ABOUT A DOCUMENT, not an execution of it: effects
    /// belong to admitted runs under run identity, and a report must be
    /// reproducible from the document alone or its hash-keyed identity is a lie.
    /// The refusal names the exact components that carry effects rather than a
    /// free-text detail, so a client can act on it.
    #[schemars(rename_all = "kebab-case")]
    EffectfulComponentReached {
        components: Vec<String>,
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
    /// The submitted bytes are not a wiring document.
    ///
    /// Same replacement, same reason as [`GateRefusal::InvalidDocument`]:
    /// `publish` carries the document (wamn-0h0g.7.10), so it cannot be missing.
    #[schemars(rename_all = "kebab-case")]
    InvalidDocument {
        detail: String,
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
