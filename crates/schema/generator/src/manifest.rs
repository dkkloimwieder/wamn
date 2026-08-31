use std::collections::{BTreeMap, BTreeSet};

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
    pub error_details: BTreeMap<CommandErrorLiteral, OperationErrorDetailDeclaration>,
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

/// Closed operation-error detail keys serialized on per-item refusals.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationErrorDetailKey {
    Field,
    Id,
    ExpectedRowVersion,
    ObservedRowVersion,
    Minimum,
    Maximum,
    Observed,
    Constraint,
    Operation,
}

/// Required and optional keys for one exact error code.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationErrorDetailDeclaration {
    #[serde(default)]
    pub required: Vec<OperationErrorDetailKey>,
    #[serde(default)]
    pub optional: Vec<OperationErrorDetailKey>,
}

/// Closed generated-operation refusal vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessOperationErrorLiteral {
    InvalidInput,
    NotFound,
    ConcurrencyConflict,
    UniqueViolation,
    ForeignKeyViolation,
    CheckViolation,
    Retry,
    Timeout,
    PermissionDenied,
    InternalError,
}

impl AccessOperationErrorLiteral {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::ConcurrencyConflict => "concurrency_conflict",
            Self::UniqueViolation => "unique_violation",
            Self::ForeignKeyViolation => "foreign_key_violation",
            Self::CheckViolation => "check_violation",
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
    pub select_fields: Vec<String>,
    pub insert_fields: Vec<String>,
    pub update_fields: Vec<String>,
    /// Whether verified SQL takes a row lock; generation owns any PostgreSQL
    /// UPDATE carrier needed for the lock and does not treat it as DML intent.
    pub lock: bool,
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
        let manifest: Self = serde_json::from_slice(bytes).map_err(|source| {
            GenerateError::with_source(
                GenerateErrorKind::InvalidManifest,
                "wamn.json does not match the closed manifest vocabulary",
                source,
            )
        })?;
        if manifest
            .package
            .predecessor_version
            .as_deref()
            .is_some_and(str::is_empty)
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "package predecessor version must not be empty",
            ));
        }
        if manifest.package.predecessor_version.as_deref()
            == Some(manifest.package.version.as_str())
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "package predecessor version must differ from the package version",
            ));
        }
        Ok(manifest)
    }
}

/// Validate and return the manifest's exact package-local operation vocabulary.
///
/// This is the shared semantic authority for generation and production grant
/// reconciliation. The package and local operation identities obey the naming
/// law, every operation names itself as its permission token, and every
/// declared operation owns exactly one component artifact. Components with
/// zero or multiple operations, and unknown, repeated, or missing members,
/// refuse.
pub fn validate_operation_vocabulary(
    manifest: &PackageManifest,
) -> Result<BTreeSet<String>, GenerateError> {
    validate_package_identity(&manifest.package)?;

    let mut declared = BTreeSet::new();
    for (model_name, model) in &manifest.models {
        validate_identifier(model_name, "operation module")?;
        for (action, operation) in &model.operations {
            let identity = format!("{model_name}.{}", action.as_str());
            if operation.permission != identity {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "{identity} permission must equal its package-local operation identity"
                    ),
                ));
            }
            if !declared.insert(identity.clone()) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("manifest repeats declared operation {identity}"),
                ));
            }
            validate_access_error_details(&identity, *action, &operation.error_details)?;
        }
    }
    for (command_name, command) in &manifest.commands {
        validate_operation_identity(command_name)?;
        if command.permission != command_name.as_str() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{command_name} permission must equal its package-local operation identity"
                ),
            ));
        }
        if !declared.insert(command_name.clone()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("manifest repeats declared operation {command_name}"),
            ));
        }
        validate_command_error_details(command_name, &command.error_details)?;
    }

    if manifest.components.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidComponent,
            "manifest declares no component grouping",
        ));
    }
    let mut grouped = BTreeSet::new();
    for (name, component) in &manifest.components {
        validate_identifier(name, "component")?;
        if component.operations.len() != 1 {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidComponent,
                format!("{name} must declare exactly one operation"),
            ));
        }
        let operation = &component.operations[0];
        if !declared.contains(operation) || !grouped.insert(operation.clone()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidComponent,
                format!("{name} references unknown or repeated operation {operation}"),
            ));
        }
    }
    if grouped != declared {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidComponent,
            "every operation must be grouped exactly once",
        ));
    }
    Ok(declared)
}

fn validate_access_error_details(
    operation: &str,
    action: CrudAction,
    details: &BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
) -> Result<(), GenerateError> {
    use AccessOperationErrorLiteral as Code;
    use OperationErrorDetailKey as Key;

    let mut expected = BTreeSet::from([
        Code::InvalidInput,
        Code::Retry,
        Code::Timeout,
        Code::PermissionDenied,
        Code::InternalError,
    ]);
    if matches!(
        action,
        CrudAction::Get | CrudAction::Update | CrudAction::Delete
    ) {
        expected.insert(Code::NotFound);
    }
    if matches!(action, CrudAction::Update | CrudAction::Delete) {
        expected.insert(Code::ConcurrencyConflict);
    }
    for constraint in [
        Code::UniqueViolation,
        Code::ForeignKeyViolation,
        Code::CheckViolation,
    ] {
        if details.contains_key(&constraint) {
            expected.insert(constraint);
        }
    }
    if details.keys().copied().collect::<BTreeSet<_>>() != expected {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} must declare its exact closed error-detail code set"),
        ));
    }

    for (code, detail) in details {
        let (required, optional): (&[Key], &[Key]) = match code {
            Code::InvalidInput if action == CrudAction::Query => {
                (&[Key::Field], &[Key::Minimum, Key::Maximum, Key::Observed])
            }
            Code::InvalidInput => (&[Key::Field], &[]),
            Code::NotFound => (&[Key::Field, Key::Id], &[]),
            Code::ConcurrencyConflict => (&[Key::ExpectedRowVersion, Key::ObservedRowVersion], &[]),
            Code::UniqueViolation | Code::ForeignKeyViolation | Code::CheckViolation => {
                (&[Key::Constraint], &[])
            }
            Code::PermissionDenied => (&[Key::Operation], &[]),
            Code::Retry | Code::Timeout | Code::InternalError => (&[], &[]),
        };
        validate_detail_keys(operation, code.as_str(), detail, required, optional)?;
    }
    Ok(())
}

fn validate_command_error_details(
    operation: &str,
    details: &BTreeMap<CommandErrorLiteral, OperationErrorDetailDeclaration>,
) -> Result<(), GenerateError> {
    use CommandErrorLiteral as Code;
    use OperationErrorDetailKey as Key;

    let expected = BTreeSet::from([
        Code::InvalidInput,
        Code::PurchaseOrderNotFound,
        Code::PurchaseOrderNotOpen,
        Code::PurchaseOrderLineNotFound,
        Code::PurchaseOrderLineMismatch,
        Code::LocationNotFound,
        Code::QuantityExceedsRemaining,
        Code::ReceiptReferenceConflict,
        Code::IdempotencyConflict,
        Code::Retry,
        Code::Timeout,
        Code::PermissionDenied,
        Code::InternalError,
    ]);
    if details.keys().copied().collect::<BTreeSet<_>>() != expected {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} must declare its exact closed error-detail code set"),
        ));
    }

    for (code, detail) in details {
        let (required, optional): (&[Key], &[Key]) = match code {
            Code::InvalidInput => (&[Key::Field], &[Key::Minimum, Key::Maximum, Key::Observed]),
            Code::PurchaseOrderNotFound
            | Code::PurchaseOrderNotOpen
            | Code::IdempotencyConflict => (&[Key::Field], &[]),
            Code::PurchaseOrderLineNotFound
            | Code::PurchaseOrderLineMismatch
            | Code::LocationNotFound
            | Code::QuantityExceedsRemaining => (&[Key::Field, Key::Id], &[]),
            Code::ReceiptReferenceConflict => (&[Key::Constraint], &[]),
            Code::PermissionDenied => (&[Key::Operation], &[]),
            Code::Retry | Code::Timeout | Code::InternalError => (&[], &[]),
        };
        validate_detail_keys(operation, code.as_str(), detail, required, optional)?;
    }
    Ok(())
}

fn validate_detail_keys(
    operation: &str,
    code: &str,
    detail: &OperationErrorDetailDeclaration,
    required: &[OperationErrorDetailKey],
    optional: &[OperationErrorDetailKey],
) -> Result<(), GenerateError> {
    let actual_required = detail.required.iter().copied().collect::<BTreeSet<_>>();
    let actual_optional = detail.optional.iter().copied().collect::<BTreeSet<_>>();
    let valid = actual_required.len() == detail.required.len()
        && actual_optional.len() == detail.optional.len()
        && actual_required.is_disjoint(&actual_optional)
        && actual_required == required.iter().copied().collect()
        && actual_optional == optional.iter().copied().collect();
    if valid {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} error {code} must declare its exact structured-detail keys"),
        ))
    }
}

/// Construct the one canonical package-qualified identity for a local operation.
pub fn canonical_operation_identity(
    package: &PackageIdentity,
    local_operation: &str,
) -> Result<String, GenerateError> {
    validate_operation_identity(local_operation)?;
    Ok(format!(
        "{}{local_operation}",
        canonical_operation_prefix(package)?
    ))
}

/// Construct the canonical namespace prefix owned by one exact package coordinate.
pub fn canonical_operation_prefix(package: &PackageIdentity) -> Result<String, GenerateError> {
    validate_package_identity(package)?;
    Ok(format!("{}@{}::", package.id, package.version))
}

fn validate_package_identity(package: &PackageIdentity) -> Result<(), GenerateError> {
    validate_identifier(&package.id, "package id")?;
    if package.version.is_empty()
        || package.version.trim() != package.version
        || package.version.as_bytes().contains(&0)
        || package.version.contains('@')
        || package.version.contains("::")
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
            "package version must be canonical text without operation-coordinate separators",
        ));
    }
    Ok(())
}

fn validate_operation_identity(value: &str) -> Result<(), GenerateError> {
    let Some((module, operation)) = value.split_once('.') else {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
            format!("operation `{value}` must have canonical module.operation form"),
        ));
    };
    if operation.contains('.') {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
            format!("operation `{value}` must contain exactly one module separator"),
        ));
    }
    validate_identifier(module, "operation module")?;
    validate_identifier(operation, "operation name")
}

pub(crate) fn validate_identifier(value: &str, object: &str) -> Result<(), GenerateError> {
    let mut bytes = value.bytes();
    let valid_start = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
    let valid_tail =
        bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid_start && valid_tail && !value.ends_with('_') && !value.contains("__") {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
            format!("{object} `{value}` must be singular snake_case"),
        ))
    }
}

/// Immutable package identity and sole operation-version coordinate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIdentity {
    pub id: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_version: Option<String>,
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
    pub error_details: BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
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
