use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{GenerateError, GenerateErrorKind};

/// Strict package-owned behavior declaration parsed from `wamn.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub package: PackageIdentity,
    pub required_platform_policy_contract: PolicyContractRequirement,
    pub models: BTreeMap<String, ModelDeclaration>,
    #[serde(default)]
    pub commands: BTreeMap<String, CommandDeclaration>,
    pub connections: BTreeMap<String, ConnectionDeclaration>,
    pub components: BTreeMap<String, ComponentDeclaration>,
}

/// One explicit package command backed only by declared static SQL accessors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDeclaration {
    pub permission: String,
    pub connection: String,
    pub transaction: CommandTransaction,
    pub automatic_retry: bool,
    pub input: CommandInputDeclaration,
    pub result: CommandResultDeclaration,
    pub canonicalization: CommandCanonicalization,
    pub errors: Vec<CommandErrorLiteral>,
    #[serde(default)]
    pub constraint_errors: BTreeMap<String, CommandErrorLiteral>,
    pub relations: Vec<CommandRelationDeclaration>,
    pub statements: BTreeMap<String, CommandStatementDeclaration>,
}

/// Transaction boundary admitted for custom commands in the POC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandTransaction {
    ExplicitPerInput,
}

/// Closed array-envelope and leaf-field command input contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputDeclaration {
    pub raw_body_maximum: u32,
    pub envelope: CountLimitDeclaration,
    pub item_semantics: ItemSemantics,
    pub line: CountLimitDeclaration,
    pub fields: Vec<ContractFieldDeclaration>,
}

/// Independent result semantics for each array-envelope item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemSemantics {
    PerInput,
}

/// One explicit count bound whose refusal stays at the operation layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountLimitDeclaration {
    pub minimum: u32,
    pub maximum: u32,
    pub invalid: InputRefusal,
}

/// One typed leaf in an input or result contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractFieldDeclaration {
    pub path: String,
    #[serde(rename = "type")]
    pub ty: wamn_schema_introspection::ir::ColumnType,
    pub nullable: bool,
    #[serde(default)]
    pub values: Vec<String>,
}

/// Closed one-result command contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResultDeclaration {
    pub class: ResultClass,
    pub fields: Vec<ContractFieldDeclaration>,
}

/// Byte-stable command identity rules consumed by the runtime implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandCanonicalization {
    pub payload: CursorPayload,
    pub excluded_fields: Vec<String>,
    pub line_order: CommandLineOrder,
    pub duplicate_line: InputRefusal,
    pub uuid: UuidSpelling,
    pub timestamptz: TimestamptzSpelling,
    pub numeric: NumericSpelling,
}

/// Canonical semantic ordering for receipt facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandLineOrder {
    PurchaseOrderLineIdAscending,
}

/// Frozen UUID spelling at durable JSON boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UuidSpelling {
    LowercaseHyphenated,
}

/// Frozen timestamp spelling at durable JSON boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestamptzSpelling {
    UtcRfc3339SixFractionalDigits,
}

/// Frozen numeric identity rule; scale is semantic command input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumericSpelling {
    PostgresqlLexicalScalePreserved,
}

/// Closed operation error literals available to the Receiving command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandErrorLiteral {
    InvalidInput,
    PurchaseOrderNotFound,
    PurchaseOrderNotOpen,
    PurchaseOrderLineNotFound,
    PurchaseOrderLineMismatch,
    LocationNotFound,
    QuantityExceedsRemaining,
    ReceiptReferenceConflict,
    IdempotencyConflict,
    Retry,
    Timeout,
    PermissionDenied,
    InternalError,
}

impl CommandErrorLiteral {
    /// Frozen serialized operation literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::PurchaseOrderNotFound => "purchase_order_not_found",
            Self::PurchaseOrderNotOpen => "purchase_order_not_open",
            Self::PurchaseOrderLineNotFound => "purchase_order_line_not_found",
            Self::PurchaseOrderLineMismatch => "purchase_order_line_mismatch",
            Self::LocationNotFound => "location_not_found",
            Self::QuantityExceedsRemaining => "quantity_exceeds_remaining",
            Self::ReceiptReferenceConflict => "receipt_reference_conflict",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::Retry => "retry",
            Self::Timeout => "timeout",
            Self::PermissionDenied => "permission_denied",
            Self::InternalError => "internal_error",
        }
    }
}

/// One migration-derived relation consumed by a custom command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRelationDeclaration {
    pub schema: String,
    pub table: String,
    pub fields: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
}

/// One static SQL accessor signature shared by native and Wamn projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandStatementDeclaration {
    pub path: String,
    pub fetch: CommandFetch,
    #[serde(default)]
    pub parameters: Vec<CommandValueDeclaration>,
    pub row: Vec<CommandValueDeclaration>,
}

/// One named SQL parameter or result member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandValueDeclaration {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: wamn_schema_introspection::ir::ColumnType,
    pub nullable: bool,
}

/// Static SQL result cardinality used by generated accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandFetch {
    One,
    OptionalOne,
    BoundedList,
}

impl PackageManifest {
    /// Parse one complete manifest, refusing unknown fields at every level.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, GenerateError> {
        serde_json::from_slice(bytes).map_err(|source| {
            GenerateError::with_source(
                GenerateErrorKind::InvalidManifest,
                "wamn.json does not match the closed manifest vocabulary",
                source,
            )
        })
    }
}

/// Immutable package identity and sole operation-version coordinate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIdentity {
    pub id: String,
    pub version: String,
}

/// Platform policy contract required before package promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyContractRequirement {
    pub id: String,
    pub state: PolicyContractState,
}

/// Slice-ii policy requirements remain explicitly unsatisfied until wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyContractState {
    Unsatisfied,
    Satisfied,
}

/// Behavior attached to one introspected relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDeclaration {
    pub schema: String,
    pub table: String,
    pub owner: String,
    #[serde(default)]
    pub server_owned_fields: Vec<String>,
    #[serde(default)]
    pub enum_fields: BTreeMap<String, Vec<String>>,
    pub operations: BTreeMap<CrudAction, OperationDeclaration>,
}

/// Closed generated CRUD action vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrudAction {
    Get,
    Query,
    Create,
    Update,
    Delete,
}

impl CrudAction {
    /// Canonical local action spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Query => "query",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// One generated operation's behavior and authority declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationDeclaration {
    pub permission: String,
    #[serde(default)]
    pub authored_sql: Option<AuthoredSqlDeclaration>,
    #[serde(default)]
    pub writable_fields: Vec<String>,
    #[serde(default)]
    pub revision_field: Option<String>,
    #[serde(default)]
    pub filters: Vec<FilterDeclaration>,
    #[serde(default)]
    pub sort: Option<SortDeclaration>,
    #[serde(default)]
    pub pagination: Option<PaginationDeclaration>,
    #[serde(default)]
    pub limit: Option<LimitDeclaration>,
    pub result: ResultClass,
}

/// Package-owned static SQL files for every declared query ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredSqlDeclaration {
    pub default: String,
    pub variants: Vec<AuthoredSqlVariant>,
}

/// One authored query file selected by a finite field/direction pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredSqlVariant {
    pub field: String,
    pub direction: CursorDirection,
    pub path: String,
}

/// Closed result cardinality vocabulary from the POC design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultClass {
    One,
    OptionalOne,
    Page,
    BoundedList,
}

/// Binding strategy for one query filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterDeclaration {
    pub field: String,
    pub binding: FilterBinding,
}

/// Frozen filter binding strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterBinding {
    JsonArray,
}

/// Finite query sorting vocabulary with at most one requested field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortDeclaration {
    pub fields: Vec<String>,
    pub directions: Vec<CursorDirection>,
    pub max_fields: u8,
}

/// Keyset pagination and opaque cursor contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaginationDeclaration {
    pub kind: PaginationKind,
    pub cursor: CursorDeclaration,
    pub default_sort: SortKey,
    pub tie_breaker: TieBreakerDeclaration,
}

/// Supported pagination strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaginationKind {
    Keyset,
}

/// Opaque, versioned wire cursor declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorDeclaration {
    pub version: u8,
    pub payload: CursorPayload,
    pub encoding: CursorEncoding,
    pub opaque: bool,
    pub invalid: InputRefusal,
}

/// Canonical payload serialized before cursor encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorPayload {
    CanonicalCompactJson,
}

/// Supported wire cursor encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorEncoding {
    Base64urlUnpadded,
}

/// One field and direction used by deterministic keyset ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortKey {
    pub field: String,
    pub direction: CursorDirection,
}

/// Stable secondary key whose direction inherits the selected primary sort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TieBreakerDeclaration {
    pub field: String,
}

/// Request limit contract enforced before SQL execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitDeclaration {
    pub default: u32,
    pub minimum: u32,
    pub maximum: u32,
    pub invalid: InputRefusal,
}

/// Typed refusal used for malformed operation inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputRefusal {
    InvalidInput,
}

/// Closed sort direction vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorDirection {
    Ascending,
    Descending,
}

/// One package-required connection capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionDeclaration {
    pub interface: String,
}

/// Grouping of registered operations into a future component artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentDeclaration {
    pub operations: Vec<String>,
    pub connections: Vec<String>,
}
