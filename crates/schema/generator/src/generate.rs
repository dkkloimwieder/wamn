use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wamn_execution_contract::canonical_json_bytes;
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnType, Constraint, ConstraintKind, Table,
};

use crate::manifest::{
    AccessOperationErrorLiteral, AuthoredSqlDeclaration, CommandDeclaration, CommandErrorLiteral,
    CommandFetch, CommandTransaction, CommandValueDeclaration, ContractFieldDeclaration,
    CrudAction, CursorDirection, ModelDeclaration, OperationDeclaration,
    OperationErrorDetailDeclaration, PackageManifest, PolicyContractRequirement,
    PolicyContractState, ResultClass, SortDeclaration, canonical_operation_identity,
    validate_identifier, validate_operation_vocabulary,
};
use crate::sql;
use crate::sql_lex::contains_schema_qualified_reference;
use crate::{GenerateError, GenerateErrorKind};

const POSTGRES_INTERFACE: &str = "wamn:postgres@0.1.0";
const QUERY_LIMIT: u32 = 100;
const CURSOR_VERSION: u8 = 1;
const COMMAND_LIMIT: u32 = 100;
const RECORD_RECEIPT: &str = "receiving.record_receipt";

#[derive(Debug, Clone, Copy)]
struct ExpectedCommandValue {
    name: &'static str,
    ty: ColumnType,
    nullable: bool,
}

#[derive(Debug, Clone, Copy)]
struct ExpectedCommandStatement {
    name: &'static str,
    fetch: CommandFetch,
    parameters: &'static [ExpectedCommandValue],
    row: &'static [ExpectedCommandValue],
}

const fn command_value(name: &'static str, ty: ColumnType, nullable: bool) -> ExpectedCommandValue {
    ExpectedCommandValue { name, ty, nullable }
}

const RECORD_RECEIPT_STATEMENTS: &[ExpectedCommandStatement] = &[
    ExpectedCommandStatement {
        name: "claim_command",
        fetch: CommandFetch::OptionalOne,
        parameters: &[
            command_value("idempotency_key", ColumnType::Text, false),
            command_value("canonical_command", ColumnType::Bytes, false),
            command_value("purchase_order_id", ColumnType::Uuid, false),
        ],
        row: &[command_value("receipt_id", ColumnType::Uuid, false)],
    },
    ExpectedCommandStatement {
        name: "finalize_command",
        fetch: CommandFetch::One,
        parameters: &[
            command_value("idempotency_key", ColumnType::Text, false),
            command_value("canonical_command", ColumnType::Bytes, false),
            command_value("receipt_id", ColumnType::Uuid, false),
            command_value("purchase_order_status", ColumnType::Text, false),
            command_value("row_version", ColumnType::Int64, false),
        ],
        row: &[
            command_value("purchase_order_status", ColumnType::Text, true),
            command_value("row_version", ColumnType::Int64, true),
        ],
    },
    ExpectedCommandStatement {
        name: "find_replay",
        fetch: CommandFetch::OptionalOne,
        parameters: &[command_value("idempotency_key", ColumnType::Text, false)],
        row: &[
            command_value("canonical_command", ColumnType::Bytes, false),
            command_value("receipt_id", ColumnType::Uuid, false),
            command_value("purchase_order_id", ColumnType::Uuid, false),
            command_value("purchase_order_status", ColumnType::Text, true),
            command_value("row_version", ColumnType::Int64, true),
        ],
    },
    ExpectedCommandStatement {
        name: "finish_purchase_order",
        fetch: CommandFetch::One,
        parameters: &[command_value("purchase_order_id", ColumnType::Uuid, false)],
        row: &[
            command_value("status", ColumnType::Text, false),
            command_value("row_version", ColumnType::Int64, false),
        ],
    },
    ExpectedCommandStatement {
        name: "insert_receipt",
        fetch: CommandFetch::One,
        parameters: &[
            command_value("receipt_id", ColumnType::Uuid, false),
            command_value("idempotency_key", ColumnType::Text, false),
            command_value("purchase_order_id", ColumnType::Uuid, false),
            command_value("receipt_reference", ColumnType::Text, false),
            command_value("occurred_at", ColumnType::Timestamptz, false),
        ],
        row: &[command_value("id", ColumnType::Uuid, false)],
    },
    ExpectedCommandStatement {
        name: "insert_receipt_line",
        fetch: CommandFetch::BoundedList,
        parameters: &[
            command_value("receipt_id", ColumnType::Uuid, false),
            command_value("line", ColumnType::Json, false),
        ],
        row: &[command_value("id", ColumnType::Uuid, false)],
    },
    ExpectedCommandStatement {
        name: "lock_purchase_order",
        fetch: CommandFetch::OptionalOne,
        parameters: &[command_value("purchase_order_id", ColumnType::Uuid, false)],
        row: &[command_value("status", ColumnType::Text, false)],
    },
    ExpectedCommandStatement {
        name: "update_purchase_order_line",
        fetch: CommandFetch::BoundedList,
        parameters: &[
            command_value("purchase_order_id", ColumnType::Uuid, false),
            command_value("line", ColumnType::Json, false),
        ],
        row: &[command_value("id", ColumnType::Uuid, false)],
    },
    ExpectedCommandStatement {
        name: "validate_receipt_line",
        fetch: CommandFetch::One,
        parameters: &[
            command_value("purchase_order_id", ColumnType::Uuid, false),
            command_value("line", ColumnType::Json, false),
        ],
        row: &[
            command_value("outcome", ColumnType::Text, true),
            command_value("id", ColumnType::Uuid, true),
        ],
    },
];

const RECORD_RECEIPT_LOCATION_FIELDS: &[ExpectedCommandValue] =
    &[command_value("id", ColumnType::Uuid, false)];
const RECORD_RECEIPT_PURCHASE_ORDER_FIELDS: &[ExpectedCommandValue] = &[
    command_value("id", ColumnType::Uuid, false),
    command_value("status", ColumnType::Text, false),
    command_value("row_version", ColumnType::Int64, false),
    command_value("updated_at", ColumnType::Timestamptz, false),
];
const RECORD_RECEIPT_PURCHASE_ORDER_LINE_FIELDS: &[ExpectedCommandValue] = &[
    command_value("id", ColumnType::Uuid, false),
    command_value("purchase_order_id", ColumnType::Uuid, false),
    command_value("ordered_quantity", ColumnType::Numeric, false),
    command_value("received_quantity", ColumnType::Numeric, false),
];
const RECORD_RECEIPT_LEDGER_FIELDS: &[ExpectedCommandValue] = &[
    command_value("idempotency_key", ColumnType::Text, false),
    command_value("canonical_command", ColumnType::Bytes, false),
    command_value("receipt_id", ColumnType::Uuid, false),
    command_value("purchase_order_id", ColumnType::Uuid, false),
    command_value("purchase_order_status", ColumnType::Text, true),
    command_value("row_version", ColumnType::Int64, true),
];
const RECORD_RECEIPT_RECEIPT_FIELDS: &[ExpectedCommandValue] = &[
    command_value("id", ColumnType::Uuid, false),
    command_value("idempotency_key", ColumnType::Text, false),
    command_value("purchase_order_id", ColumnType::Uuid, false),
    command_value("receipt_reference", ColumnType::Text, false),
    command_value("occurred_at", ColumnType::Timestamptz, false),
];
const RECORD_RECEIPT_RECEIPT_LINE_FIELDS: &[ExpectedCommandValue] = &[
    command_value("id", ColumnType::Uuid, false),
    command_value("receipt_id", ColumnType::Uuid, false),
    command_value("purchase_order_line_id", ColumnType::Uuid, false),
    command_value("quantity", ColumnType::Numeric, false),
    command_value("location_id", ColumnType::Uuid, false),
];

/// One package-owned authored SQL source supplied without filesystem access.
#[derive(Debug, Clone, Copy)]
pub struct AuthoredSql<'a> {
    path: &'a str,
    bytes: &'a [u8],
}

impl<'a> AuthoredSql<'a> {
    /// Pair a package-relative path with its exact source bytes.
    pub const fn new(path: &'a str, bytes: &'a [u8]) -> Self {
        Self { path, bytes }
    }

    /// Package-relative corpus path.
    pub const fn path(&self) -> &'a str {
        self.path
    }

    /// Exact authored bytes welded into the SQL corpus.
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// Explicit source and toolchain facts embedded in every package weld.
#[derive(Debug, Clone, Copy)]
pub struct GenerationProvenance<'a> {
    source_commit: &'a str,
    generator: &'a str,
    toolchain: &'a str,
}

impl<'a> GenerationProvenance<'a> {
    /// Construct provenance without consulting git, environment, or a clock.
    pub const fn new(source_commit: &'a str, generator: &'a str, toolchain: &'a str) -> Self {
        Self {
            source_commit,
            generator,
            toolchain,
        }
    }
}

/// Complete pure input to [`generate`].
#[derive(Debug)]
pub struct GenerationInput<'a> {
    catalog: &'a CatalogIr,
    manifest_json: &'a [u8],
    authored_sql: &'a [AuthoredSql<'a>],
    provenance: GenerationProvenance<'a>,
}

impl<'a> GenerationInput<'a> {
    /// Construct a generation input from exact in-memory artifacts.
    pub const fn new(
        catalog: &'a CatalogIr,
        manifest_json: &'a [u8],
        authored_sql: &'a [AuthoredSql<'a>],
        provenance: GenerationProvenance<'a>,
    ) -> Self {
        Self {
            catalog,
            manifest_json,
            authored_sql,
            provenance,
        }
    }
}

/// One generated package-relative artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    path: Box<str>,
    bytes: Box<[u8]>,
}

impl GeneratedFile {
    /// Package-relative artifact path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact deterministic artifact bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Canonical immutable package weld emitted as `generated/package-weld.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageWeld {
    verified_schema_state_id: Box<str>,
    required_schema_contract: RequiredSchemaContract,
    required_platform_policy_contract: PolicyContractRequirement,
    application_sql_corpus_identity: Box<str>,
    provenance: OwnedProvenance,
    promotion_state: PromotionState,
}

impl PackageWeld {
    /// Digest of the complete normalized catalog IR used for generation.
    pub fn verified_schema_state_id(&self) -> &str {
        &self.verified_schema_state_id
    }

    /// Digest of the exact authored and generated SQL files.
    pub fn application_sql_corpus_identity(&self) -> &str {
        &self.application_sql_corpus_identity
    }

    /// Required opaque platform policy contract and current satisfaction state.
    pub const fn required_platform_policy_contract(&self) -> &PolicyContractRequirement {
        &self.required_platform_policy_contract
    }

    /// Whether the typed policy requirement permits package promotion.
    pub const fn promotion_eligible(&self) -> bool {
        matches!(self.promotion_state, PromotionState::Eligible)
    }
}

/// Deterministically generated package artifacts and their weld.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedPackage {
    files: Box<[GeneratedFile]>,
    weld: PackageWeld,
}

impl GeneratedPackage {
    /// Artifacts ordered by package-relative path.
    pub fn files(&self) -> &[GeneratedFile] {
        &self.files
    }

    /// Find one generated artifact without touching the filesystem.
    pub fn file(&self, path: &str) -> Option<&GeneratedFile> {
        self.files
            .binary_search_by_key(&path, |file| file.path())
            .ok()
            .map(|index| &self.files[index])
    }

    /// Canonical package weld also present in the generated file set.
    pub const fn weld(&self) -> &PackageWeld {
        &self.weld
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RequiredSchemaContract {
    tables: Box<[RequiredTable]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RequiredTable {
    schema: Box<str>,
    table: Box<str>,
    fields: Box<[RequiredField]>,
    constraints: Box<[RequiredConstraint]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RequiredField {
    name: Box<str>,
    #[serde(rename = "type")]
    ty: Box<str>,
    nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RequiredConstraint {
    name: Box<str>,
    definition: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct OwnedProvenance {
    source_commit: Box<str>,
    generator: Box<str>,
    toolchain: Box<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PromotionState {
    BlockedUnsatisfiedPolicyContract,
    Eligible,
}

/// Generate a package without filesystem, database, clock, or environment I/O.
pub fn generate(input: &GenerationInput<'_>) -> Result<GeneratedPackage, GenerateError> {
    let manifest = PackageManifest::from_slice(input.manifest_json)?;
    validate(input, &manifest)?;

    let mut files = BTreeMap::<String, Vec<u8>>::new();
    let mut sql_corpus = authored_sql_map(input.authored_sql)?;
    emit_cursor_contract(&mut files)?;

    for (model_name, model) in &manifest.models {
        let table = relation(input.catalog, model).expect("validation resolved every relation");
        emit_model(
            &mut files,
            &mut sql_corpus,
            &manifest,
            model_name,
            model,
            table,
        )?;
    }
    for (command_name, command) in &manifest.commands {
        emit_command(&mut files, &manifest, command_name, command)?;
    }
    let data_access = crate::data_access::derive_data_access_overlay(
        input.catalog,
        input.manifest_json,
        &manifest,
    )?;
    insert_canonical_json(
        &mut files,
        crate::data_access::DATA_ACCESS_OVERLAY_PATH,
        &data_access,
    )?;

    let weld = PackageWeld {
        verified_schema_state_id: sha256(&canonical_json_bytes(
            &serde_json::to_value(input.catalog).expect("schema IR always serializes"),
        ))
        .into(),
        required_schema_contract: required_schema_contract(input.catalog, &manifest),
        required_platform_policy_contract: manifest.required_platform_policy_contract.clone(),
        application_sql_corpus_identity: corpus_sha256(
            sql_corpus
                .iter()
                .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
        )
        .into(),
        provenance: OwnedProvenance {
            source_commit: input.provenance.source_commit.into(),
            generator: input.provenance.generator.into(),
            toolchain: input.provenance.toolchain.into(),
        },
        promotion_state: match manifest.required_platform_policy_contract.state {
            PolicyContractState::Unsatisfied => PromotionState::BlockedUnsatisfiedPolicyContract,
            PolicyContractState::Satisfied => PromotionState::Eligible,
        },
    };
    insert_canonical_json(&mut files, "generated/package-weld.json", &weld)?;

    let files = files
        .into_iter()
        .map(|(path, bytes)| GeneratedFile {
            path: path.into_boxed_str(),
            bytes: bytes.into_boxed_slice(),
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();

    Ok(GeneratedPackage { files, weld })
}

/// Hash sorted path/byte entries with unambiguous big-endian length framing.
pub fn corpus_sha256<'a>(entries: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> String {
    let mut entries = entries.into_iter().collect::<Vec<_>>();
    entries.sort_by_key(|(path, _)| *path);

    let mut hasher = Sha256::new();
    for (path, bytes) in entries {
        let path = path.as_bytes();
        hasher.update(
            u64::try_from(path.len())
                .expect("path length fits u64")
                .to_be_bytes(),
        );
        hasher.update(path);
        hasher.update(
            u64::try_from(bytes.len())
                .expect("artifact length fits u64")
                .to_be_bytes(),
        );
        hasher.update(bytes);
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn validate(input: &GenerationInput<'_>, manifest: &PackageManifest) -> Result<(), GenerateError> {
    validate_operation_vocabulary(manifest)?;
    validate_identifier(
        &manifest.required_platform_policy_contract.id,
        "platform policy contract",
    )?;
    for value in [
        input.provenance.source_commit,
        input.provenance.generator,
        input.provenance.toolchain,
    ] {
        if value.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidIdentity,
                "generation provenance values must not be empty",
            ));
        }
    }
    if manifest.models.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidManifest,
            "manifest must declare at least one model",
        ));
    }

    for (model_name, model) in &manifest.models {
        validate_model(input.catalog, model_name, model)?;
    }
    for (command_name, command) in &manifest.commands {
        validate_command(
            input.catalog,
            input.authored_sql,
            command_name,
            command,
            manifest,
        )?;
    }
    validate_connections(manifest)?;
    validate_components(manifest)?;
    validate_authored_sources(manifest, input.authored_sql)?;
    Ok(())
}

fn validate_model(
    catalog: &CatalogIr,
    model_name: &str,
    model: &ModelDeclaration,
) -> Result<(), GenerateError> {
    validate_identifier(model_name, "model")?;
    validate_identifier(&model.schema, "schema")?;
    validate_identifier(&model.table, "table")?;
    validate_identifier(&model.owner, "owner")?;
    let table = relation(catalog, model).ok_or_else(|| {
        GenerateError::for_object(
            GenerateErrorKind::UnknownRelation,
            format!(
                "{model_name} references unknown {}.{}",
                model.schema, model.table
            ),
            format!("{}.{}", model.schema, model.table),
        )
    })?;
    if model.operations.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidModel,
            format!("{model_name} declares no operations"),
        ));
    }

    let mut seen = BTreeSet::new();
    for field in &model.server_owned_fields {
        validate_field(table, model_name, field)?;
        if !seen.insert(field) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidModel,
                format!("{model_name} repeats server-owned field {field}"),
            ));
        }
    }
    for (field, values) in &model.enum_fields {
        let column = validate_field(table, model_name, field)?;
        if column.column_type() != ColumnType::Text || values.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidModel,
                format!("{model_name}.{field} enum must be nonempty text"),
            ));
        }
        let unique = values.iter().collect::<BTreeSet<_>>();
        if unique.len() != values.len() || values.iter().any(String::is_empty) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidModel,
                format!("{model_name}.{field} enum values must be unique and nonempty"),
            ));
        }
    }

    for (action, operation) in &model.operations {
        validate_operation(model_name, model, table, *action, operation)?;
    }
    Ok(())
}

fn validate_operation(
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Result<(), GenerateError> {
    let context = format!("{model_name}.{}", action.as_str());
    validate_field(table, model_name, "id")?;

    let server_owned = model.server_owned_fields.iter().collect::<BTreeSet<_>>();
    let mut writable = BTreeSet::new();
    for field in &operation.writable_fields {
        let column = validate_field(table, model_name, field)?;
        if !writable.insert(field) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{context} repeats writable field {field}"),
            ));
        }
        if server_owned.contains(field) || column.generation().is_some() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{context} exposes server-owned field {field}"),
            ));
        }
    }

    if let Some(revision_field) = &operation.revision_field {
        let column = validate_field(table, model_name, revision_field)?;
        if column.column_type() != ColumnType::Int64 || column.nullable() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{context} revision field must be non-null int64"),
            ));
        }
        if writable.contains(revision_field) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{context} revision field cannot be writable"),
            ));
        }
    }

    match action {
        CrudAction::Get => {
            require_result(
                &context,
                operation.result,
                &[ResultClass::One, ResultClass::OptionalOne],
            )?;
            require_read_shape(&context, operation, false)?;
        }
        CrudAction::Query => {
            require_result(&context, operation.result, &[ResultClass::Page])?;
            if !operation.writable_fields.is_empty() || operation.revision_field.is_some() {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("{context} query cannot declare mutation fields"),
                ));
            }
            validate_query(model_name, table, operation)?;
        }
        CrudAction::Create => {
            require_result(&context, operation.result, &[ResultClass::One])?;
            require_mutation_shape(&context, operation, false)?;
        }
        CrudAction::Update => {
            require_result(&context, operation.result, &[ResultClass::One])?;
            require_mutation_shape(&context, operation, true)?;
        }
        CrudAction::Delete => {
            require_result(&context, operation.result, &[ResultClass::One])?;
            if !operation.writable_fields.is_empty() {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("{context} delete cannot declare writable fields"),
                ));
            }
            require_mutation_shape(&context, operation, true)?;
        }
    }
    validate_constraint_error_details(&context, table, action, operation)?;
    Ok(())
}

fn validate_constraint_error_details(
    context: &str,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Result<(), GenerateError> {
    use AccessOperationErrorLiteral as Code;

    let expected = operation_constraints(table, action, operation)
        .into_iter()
        .map(|constraint| constraint_error_code(constraint.kind()))
        .collect::<BTreeSet<_>>();
    let declared = operation
        .error_details
        .keys()
        .copied()
        .filter(|code| {
            matches!(
                code,
                Code::UniqueViolation | Code::ForeignKeyViolation | Code::CheckViolation
            )
        })
        .collect::<BTreeSet<_>>();
    if declared == expected {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} must declare error details for its exact constraint kinds"),
        ))
    }
}

fn require_result(
    context: &str,
    actual: ResultClass,
    expected: &[ResultClass],
) -> Result<(), GenerateError> {
    if expected.contains(&actual) {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} has an incompatible result class"),
        ))
    }
}

fn require_read_shape(
    context: &str,
    operation: &OperationDeclaration,
    allow_query_fields: bool,
) -> Result<(), GenerateError> {
    if !operation.writable_fields.is_empty()
        || operation.revision_field.is_some()
        || (!allow_query_fields
            && (!operation.filters.is_empty()
                || operation.sort.is_some()
                || operation.pagination.is_some()
                || operation.limit.is_some()))
        || operation.authored_sql.is_some()
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} carries fields outside its operation shape"),
        ));
    }
    Ok(())
}

fn require_mutation_shape(
    context: &str,
    operation: &OperationDeclaration,
    revision_required: bool,
) -> Result<(), GenerateError> {
    if operation.authored_sql.is_some()
        || !operation.filters.is_empty()
        || operation.sort.is_some()
        || operation.pagination.is_some()
        || operation.limit.is_some()
        || revision_required != operation.revision_field.is_some()
        || (!revision_required && operation.writable_fields.is_empty())
        || (revision_required
            && context.ends_with(".update")
            && operation.writable_fields.is_empty())
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} carries fields outside its mutation shape"),
        ));
    }
    Ok(())
}

fn validate_query(
    model_name: &str,
    table: &Table,
    operation: &OperationDeclaration,
) -> Result<(), GenerateError> {
    let context = format!("{model_name}.query");
    let mut filter_fields = BTreeSet::new();
    for filter in &operation.filters {
        validate_field(table, model_name, &filter.field)?;
        if !filter_fields.insert(filter.field.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{context} repeats filter field {}", filter.field),
            ));
        }
    }
    if let Some(sort) = &operation.sort {
        if sort.fields.is_empty()
            || sort.directions.is_empty()
            || sort.max_fields != 1
            || sort.fields.iter().collect::<BTreeSet<_>>().len() != sort.fields.len()
            || sort.directions.iter().collect::<BTreeSet<_>>().len() != sort.directions.len()
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{context} sort must be a nonempty finite product with max_fields 1"),
            ));
        }
        for field in &sort.fields {
            validate_sort_field(table, model_name, &context, field)?;
        }
    }
    let limit = operation.limit.as_ref().ok_or_else(|| {
        GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} requires an explicit limit contract"),
        )
    })?;
    if limit.default != QUERY_LIMIT || limit.minimum != 1 || limit.maximum != QUERY_LIMIT {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} limit must default to 100 and accept exactly 1..=100"),
        ));
    }
    let pagination = operation.pagination.as_ref().ok_or_else(|| {
        GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} requires keyset pagination"),
        )
    })?;
    if pagination.cursor.version != CURSOR_VERSION
        || !pagination.cursor.opaque
        || pagination.default_sort.field != "created_at"
        || pagination.default_sort.direction != CursorDirection::Ascending
        || pagination.tie_breaker.field != "id"
        || operation.sort.as_ref().is_some_and(|sort| {
            !sort.fields.iter().any(|field| field == "created_at")
                || !sort.directions.contains(&CursorDirection::Ascending)
        })
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} requires opaque v1 cursor, created_at ASC, id tie-breaker"),
        ));
    }
    let created_at = validate_field(table, model_name, "created_at")?;
    let id = validate_field(table, model_name, "id")?;
    if created_at.column_type() != ColumnType::Timestamptz
        || created_at.nullable()
        || id.column_type() != ColumnType::Uuid
        || id.nullable()
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} keyset fields must be non-null timestamptz and uuid"),
        ));
    }
    if let Some(authored) = &operation.authored_sql {
        let sort = operation.sort.as_ref().ok_or_else(|| {
            GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{context} authored variants require an explicit sort declaration"),
            )
        })?;
        validate_authored_variants(&context, sort, authored)?;
    }
    Ok(())
}

fn validate_command(
    catalog: &CatalogIr,
    authored_sql: &[AuthoredSql<'_>],
    command_name: &str,
    command: &CommandDeclaration,
    manifest: &PackageManifest,
) -> Result<(), GenerateError> {
    if command_name != RECORD_RECEIPT {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "custom command `{command_name}` must be the package-local `{RECORD_RECEIPT}` operation"
            ),
        ));
    }
    if !manifest.connections.contains_key(&command.connection) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{command_name} references unknown connection {}",
                command.connection
            ),
        ));
    }
    if command.transaction != CommandTransaction::ExplicitPerInput || command.automatic_retry {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{command_name} requires one explicit transaction per input and no retry"),
        ));
    }
    if command.input.raw_body_maximum != 1_048_576
        || command.input.envelope.minimum != 1
        || command.input.envelope.maximum != COMMAND_LIMIT
        || command.input.line.minimum != 1
        || command.input.line.maximum != COMMAND_LIMIT
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{command_name} raw body must cap at 1 MiB and counts must accept exactly 1..=100"
            ),
        ));
    }
    require_contract_fields(
        command_name,
        &command.input.fields,
        &[
            ("request_id", ColumnType::Text, false, &[]),
            ("value.idempotency_key", ColumnType::Text, false, &[]),
            ("value.purchase_order_id", ColumnType::Uuid, false, &[]),
            ("value.receipt_reference", ColumnType::Text, false, &[]),
            ("value.occurred_at", ColumnType::Timestamptz, false, &[]),
            (
                "value.line[].purchase_order_line_id",
                ColumnType::Uuid,
                false,
                &[],
            ),
            ("value.line[].quantity", ColumnType::Numeric, false, &[]),
            ("value.line[].location_id", ColumnType::Uuid, false, &[]),
        ],
    )?;
    if command.result.class != ResultClass::One {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{command_name} result must be one"),
        ));
    }
    require_contract_fields(
        command_name,
        &command.result.fields,
        &[
            ("receipt_id", ColumnType::Uuid, false, &[]),
            ("purchase_order_id", ColumnType::Uuid, false, &[]),
            (
                "purchase_order_status",
                ColumnType::Text,
                false,
                &["open", "complete"],
            ),
            ("row_version", ColumnType::Int64, false, &[]),
        ],
    )?;
    if command.canonicalization.excluded_fields
        != ["request_id".to_owned(), "value.idempotency_key".to_owned()]
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{command_name} canonical identity must exclude correlation and lookup keys"),
        ));
    }
    let expected_errors = [
        CommandErrorLiteral::InvalidInput,
        CommandErrorLiteral::PurchaseOrderNotFound,
        CommandErrorLiteral::PurchaseOrderNotOpen,
        CommandErrorLiteral::PurchaseOrderLineNotFound,
        CommandErrorLiteral::PurchaseOrderLineMismatch,
        CommandErrorLiteral::LocationNotFound,
        CommandErrorLiteral::QuantityExceedsRemaining,
        CommandErrorLiteral::ReceiptReferenceConflict,
        CommandErrorLiteral::IdempotencyConflict,
        CommandErrorLiteral::Retry,
        CommandErrorLiteral::Timeout,
        CommandErrorLiteral::PermissionDenied,
        CommandErrorLiteral::InternalError,
    ];
    if command.errors.as_slice() != expected_errors
        || command.constraint_errors.len() != 1
        || command
            .constraint_errors
            .get("receipt_purchase_order_id_receipt_reference_key")
            != Some(&CommandErrorLiteral::ReceiptReferenceConflict)
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{command_name} must declare the exact closed error and constraint mapping"),
        ));
    }
    validate_command_relations(catalog, authored_sql, command_name, command)?;
    validate_command_statements(command_name, command)
}

fn require_contract_fields(
    operation: &str,
    actual: &[ContractFieldDeclaration],
    expected: &[(&str, ColumnType, bool, &[&str])],
) -> Result<(), GenerateError> {
    let matches = actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(field, expected)| {
            field.path == expected.0
                && field.ty == expected.1
                && field.nullable == expected.2
                && field
                    .values
                    .iter()
                    .map(String::as_str)
                    .eq(expected.3.iter().copied())
        });
    if matches {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} fields do not match the closed command contract"),
        ))
    }
}

fn validate_command_relations(
    catalog: &CatalogIr,
    authored_sql: &[AuthoredSql<'_>],
    operation: &str,
    command: &CommandDeclaration,
) -> Result<(), GenerateError> {
    let expected_tables = [
        "location",
        "purchase_order",
        "purchase_order_line",
        "record_receipt_command",
        "receipt",
        "receipt_line",
    ];
    if !command
        .relations
        .iter()
        .map(|relation| relation.schema.as_str())
        .all(|schema| schema == "receiving")
        || !command
            .relations
            .iter()
            .map(|relation| relation.table.as_str())
            .eq(expected_tables)
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} must declare its exact six-relation dependency set"),
        ));
    }
    for relation in &command.relations {
        validate_identifier(&relation.schema, "command relation schema")?;
        validate_identifier(&relation.table, "command relation table")?;
        let table = catalog
            .tables()
            .iter()
            .find(|table| table.schema() == relation.schema && table.name() == relation.table)
            .ok_or_else(|| {
                GenerateError::for_object(
                    GenerateErrorKind::UnknownRelation,
                    format!("{operation} references an unknown relation"),
                    format!("{}.{}", relation.schema, relation.table),
                )
            })?;
        let expected_fields = record_receipt_relation_fields(&relation.table)
            .expect("the exact table list has a field contract");
        for fields in [
            &relation.select_fields,
            &relation.insert_fields,
            &relation.update_fields,
        ] {
            validate_command_relation_fields(operation, relation, table, fields)?;
        }
        let consumed = relation
            .select_fields
            .iter()
            .chain(&relation.insert_fields)
            .chain(&relation.update_fields)
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = expected_fields
            .iter()
            .map(|field| field.name)
            .collect::<BTreeSet<_>>();
        if consumed != expected {
            return Err(GenerateError::for_object(
                GenerateErrorKind::InvalidOperation,
                format!("{operation} relation does not declare its exact consumed field set"),
                format!("{}.{}", relation.schema, relation.table),
            ));
        }
        for expected in expected_fields {
            let actual = column(table, expected.name).ok_or_else(|| {
                GenerateError::for_object(
                    GenerateErrorKind::UnknownColumn,
                    format!("{operation} requires consumed field {}", expected.name),
                    format!("{}.{}.{}", relation.schema, relation.table, expected.name),
                )
            })?;
            if actual.column_type() != expected.ty || actual.nullable() != expected.nullable {
                return Err(GenerateError::for_object(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "{operation} consumed field {} has the wrong type or nullability",
                        expected.name
                    ),
                    format!("{}.{}.{}", relation.schema, relation.table, expected.name),
                ));
            }
        }
        let expected_constraints = match relation.table.as_str() {
            "record_receipt_command" => &["record_receipt_command_idempotency_key_pkey"][..],
            "receipt" => &["receipt_purchase_order_id_receipt_reference_key"][..],
            _ => &[],
        };
        if !relation
            .constraints
            .iter()
            .map(String::as_str)
            .eq(expected_constraints.iter().copied())
        {
            return Err(GenerateError::for_object(
                GenerateErrorKind::InvalidOperation,
                format!("{operation} relation does not declare its exact consumed constraints"),
                format!("{}.{}", relation.schema, relation.table),
            ));
        }
        for name in expected_constraints {
            let constraint = table
                .constraints()
                .iter()
                .find(|constraint| constraint.name() == *name)
                .ok_or_else(|| {
                    GenerateError::for_object(
                        GenerateErrorKind::InvalidOperation,
                        format!("{operation} requires named constraint {name}"),
                        format!("{}.{}", relation.schema, relation.table),
                    )
                })?;
            let definition_matches = match *name {
                "record_receipt_command_idempotency_key_pkey" => matches!(
                    constraint.kind(),
                    ConstraintKind::PrimaryKey { columns }
                        if columns.iter().map(AsRef::as_ref).eq(["idempotency_key"])
                ),
                "receipt_purchase_order_id_receipt_reference_key" => matches!(
                    constraint.kind(),
                    ConstraintKind::Unique { columns }
                        if columns.iter().map(AsRef::as_ref).eq(["purchase_order_id", "receipt_reference"])
                ),
                _ => false,
            };
            if !definition_matches {
                return Err(GenerateError::for_object(
                    GenerateErrorKind::InvalidOperation,
                    format!("{operation} named constraint {name} has the wrong definition"),
                    format!("{}.{}", relation.schema, relation.table),
                ));
            }
        }
    }
    validate_command_relation_access(catalog, authored_sql, operation, command)
}

fn validate_command_relation_fields(
    operation: &str,
    relation: &crate::manifest::CommandRelationDeclaration,
    table: &Table,
    fields: &[String],
) -> Result<(), GenerateError> {
    let declared = fields.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if declared.len() != fields.len() {
        return Err(GenerateError::for_object(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation} {}.{} privilege fields must be unique",
                relation.schema, relation.table
            ),
            format!("{}.{}", relation.schema, relation.table),
        ));
    }
    if let Some(field) = fields
        .iter()
        .find(|field| !table.columns().iter().any(|column| column.name() == *field))
    {
        return Err(GenerateError::for_object(
            GenerateErrorKind::UnknownColumn,
            format!("{operation} privilege declaration names an unknown column"),
            format!("{}.{}.{}", relation.schema, relation.table, field),
        ));
    }
    Ok(())
}

fn validate_command_relation_access(
    catalog: &CatalogIr,
    authored_sql: &[AuthoredSql<'_>],
    operation: &str,
    command: &CommandDeclaration,
) -> Result<(), GenerateError> {
    let schemas = command
        .relations
        .iter()
        .map(|relation| relation.schema.as_str())
        .collect::<BTreeSet<_>>();
    let relation_fields = catalog
        .tables()
        .iter()
        .filter(|table| schemas.contains(table.schema()))
        .map(|table| {
            (
                table.name().to_owned(),
                table
                    .columns()
                    .iter()
                    .map(|column| column.name().to_owned())
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut actual = BTreeMap::<String, crate::sql_lex::RelationAccess>::new();
    for statement in command.statements.values() {
        let source = authored_sql
            .iter()
            .find(|source| source.path == statement.path)
            .expect("authored-source validation supplied every command statement");
        let statement_access = crate::sql_lex::relation_access(source.bytes, &relation_fields)
            .map_err(|detail| {
                GenerateError::for_path(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "{} cannot derive exact relation access: {detail}",
                        statement.path
                    ),
                    statement.path.as_str(),
                )
            })?;
        for (table, observed) in statement_access {
            let aggregate = actual.entry(table).or_default();
            aggregate.select_fields.extend(observed.select_fields);
            aggregate.insert_fields.extend(observed.insert_fields);
            aggregate.update_fields.extend(observed.update_fields);
            aggregate.lock |= observed.lock;
        }
    }
    for relation in &command.relations {
        let observed = actual.remove(&relation.table).unwrap_or_default();
        let declared = crate::sql_lex::RelationAccess {
            select_fields: relation.select_fields.iter().cloned().collect(),
            insert_fields: relation.insert_fields.iter().cloned().collect(),
            update_fields: relation.update_fields.iter().cloned().collect(),
            lock: relation.lock,
        };
        if observed != declared {
            return Err(GenerateError::for_object(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation} {}.{} privilege declaration does not match verified SQL reads, writes, and row locks",
                    relation.schema, relation.table
                ),
                format!("{}.{}", relation.schema, relation.table),
            ));
        }
    }
    if let Some(table) = actual.keys().next() {
        return Err(GenerateError::for_object(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} SQL reaches undeclared relation {table}"),
            table.clone(),
        ));
    }
    Ok(())
}

fn record_receipt_relation_fields(table: &str) -> Option<&'static [ExpectedCommandValue]> {
    match table {
        "location" => Some(RECORD_RECEIPT_LOCATION_FIELDS),
        "purchase_order" => Some(RECORD_RECEIPT_PURCHASE_ORDER_FIELDS),
        "purchase_order_line" => Some(RECORD_RECEIPT_PURCHASE_ORDER_LINE_FIELDS),
        "record_receipt_command" => Some(RECORD_RECEIPT_LEDGER_FIELDS),
        "receipt" => Some(RECORD_RECEIPT_RECEIPT_FIELDS),
        "receipt_line" => Some(RECORD_RECEIPT_RECEIPT_LINE_FIELDS),
        _ => None,
    }
}

fn validate_command_statements(
    operation: &str,
    command: &CommandDeclaration,
) -> Result<(), GenerateError> {
    if !command
        .statements
        .keys()
        .map(String::as_str)
        .eq(RECORD_RECEIPT_STATEMENTS
            .iter()
            .map(|statement| statement.name))
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} must declare the complete static command accessor set"),
        ));
    }
    for expected in RECORD_RECEIPT_STATEMENTS {
        let statement = command
            .statements
            .get(expected.name)
            .expect("the exact key set contains every statement");
        let expected_path = format!("command/record_receipt/{}.sql", expected.name);
        if statement.path != expected_path
            || statement.fetch != expected.fetch
            || !command_values_match(&statement.parameters, expected.parameters)
            || !command_values_match(&statement.row, expected.row)
        {
            return Err(GenerateError::for_path(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation}.{} does not match its exact path, fetch, parameter, and row signature",
                    expected.name
                ),
                statement.path.as_str(),
            ));
        }
    }
    Ok(())
}

fn command_values_match(
    actual: &[CommandValueDeclaration],
    expected: &[ExpectedCommandValue],
) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(actual, expected)| {
            actual.name == expected.name
                && actual.ty == expected.ty
                && actual.nullable == expected.nullable
        })
}

fn validate_sort_field(
    table: &Table,
    model_name: &str,
    context: &str,
    field: &str,
) -> Result<(), GenerateError> {
    let column = validate_field(table, model_name, field)?;
    if column.nullable()
        || matches!(
            column.column_type(),
            ColumnType::Float64 | ColumnType::Bytes | ColumnType::Json
        )
    {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} cannot keyset-sort nullable or unsupported field {field}"),
        ))
    } else {
        Ok(())
    }
}

fn validate_authored_variants(
    context: &str,
    sort: &SortDeclaration,
    authored: &AuthoredSqlDeclaration,
) -> Result<(), GenerateError> {
    let variant_count = sort.fields.len() * sort.directions.len();
    if !safe_sql_path(&authored.default) || authored.variants.len() != variant_count {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{context} authored SQL must provide {variant_count} safe package-relative variants"
            ),
        ));
    }
    let expected = sort.fields.iter().flat_map(|field| {
        sort.directions
            .iter()
            .map(move |direction| (field.as_str(), *direction))
    });
    let mut paths = BTreeSet::new();
    for ((expected_field, expected_direction), variant) in expected.zip(&authored.variants) {
        if variant.field != expected_field
            || variant.direction != expected_direction
            || !safe_sql_path(&variant.path)
            || !paths.insert(variant.path.as_str())
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{context} authored variants must follow declared field/direction order"),
            ));
        }
    }
    let default_variant = authored.variants.iter().find(|variant| {
        variant.field == "created_at" && variant.direction == CursorDirection::Ascending
    });
    if default_variant.map(|variant| variant.path.as_str()) != Some(authored.default.as_str()) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} default SQL must be the created_at ascending variant"),
        ));
    }
    Ok(())
}

fn validate_connections(manifest: &PackageManifest) -> Result<(), GenerateError> {
    if manifest.connections.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidConnection,
            "manifest declares no database connection",
        ));
    }
    for (name, connection) in &manifest.connections {
        validate_identifier(name, "connection")?;
        if connection.interface != POSTGRES_INTERFACE {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidConnection,
                format!("{name} must import {POSTGRES_INTERFACE}"),
            ));
        }
    }
    Ok(())
}

fn validate_components(manifest: &PackageManifest) -> Result<(), GenerateError> {
    for (name, component) in &manifest.components {
        if component.connections.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidComponent,
                format!("{name} must declare connections"),
            ));
        }
        for connection in &component.connections {
            if !manifest.connections.contains_key(connection) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidComponent,
                    format!("{name} references unknown connection {connection}"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_authored_sources(
    manifest: &PackageManifest,
    authored_sql: &[AuthoredSql<'_>],
) -> Result<(), GenerateError> {
    let expected = authored_paths(manifest);
    let mut supplied = BTreeSet::new();
    for source in authored_sql {
        if !safe_sql_path(source.path) || !supplied.insert(source.path) {
            return Err(GenerateError::for_path(
                GenerateErrorKind::DuplicatePath,
                "authored SQL path is unsafe or repeated",
                source.path,
            ));
        }
    }
    if let Some(path) = expected.difference(&supplied).next() {
        return Err(GenerateError::for_path(
            GenerateErrorKind::MissingAuthoredSql,
            "manifest-authored SQL path was not supplied",
            *path,
        ));
    }
    if let Some(path) = supplied.difference(&expected).next() {
        return Err(GenerateError::for_path(
            GenerateErrorKind::UnexpectedAuthoredSql,
            "supplied SQL path is not referenced by the manifest",
            *path,
        ));
    }
    let schemas = manifest
        .models
        .values()
        .map(|model| model.schema.as_str())
        .chain(manifest.commands.values().flat_map(|command| {
            command
                .relations
                .iter()
                .map(|relation| relation.schema.as_str())
        }))
        .collect::<BTreeSet<_>>();
    for source in authored_sql {
        for schema in &schemas {
            if contains_schema_qualified_reference(source.bytes, schema) {
                return Err(GenerateError::for_path(
                    GenerateErrorKind::SchemaQualifiedSql,
                    format!(
                        "{} selects schema `{schema}`; the corpus must inherit the host search path",
                        source.path
                    ),
                    source.path,
                ));
            }
        }
    }
    Ok(())
}

fn authored_paths(manifest: &PackageManifest) -> BTreeSet<&str> {
    let mut paths = manifest
        .models
        .values()
        .flat_map(|model| model.operations.values())
        .filter_map(|operation| operation.authored_sql.as_ref())
        .flat_map(|authored| {
            authored
                .variants
                .iter()
                .map(|variant| variant.path.as_str())
        })
        .collect::<BTreeSet<_>>();
    paths.extend(
        manifest
            .commands
            .values()
            .flat_map(|command| command.statements.values())
            .map(|statement| statement.path.as_str()),
    );
    paths
}

fn authored_sql_map(
    sources: &[AuthoredSql<'_>],
) -> Result<BTreeMap<String, Vec<u8>>, GenerateError> {
    let mut map = BTreeMap::new();
    for source in sources {
        if map
            .insert(source.path.to_owned(), source.bytes.to_vec())
            .is_some()
        {
            return Err(GenerateError::for_path(
                GenerateErrorKind::DuplicatePath,
                "authored SQL path is repeated",
                source.path,
            ));
        }
    }
    Ok(map)
}

fn emit_model(
    files: &mut BTreeMap<String, Vec<u8>>,
    sql_corpus: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
) -> Result<(), GenerateError> {
    emit_model_contract(files, model_name, model, table)?;

    let mut operation_sql = BTreeMap::<String, Vec<String>>::new();
    for (action, operation) in &model.operations {
        let paths = emit_operation_sql(
            files, sql_corpus, model_name, model, table, *action, operation,
        )?;
        operation_sql.insert(action.as_str().to_owned(), paths.clone());
        emit_operation_contracts(
            files, manifest, model_name, model, table, *action, operation, &paths,
        )?;
    }

    let native_operation_rows = operation_result_rows(model_name, model, table, Projection::Native);
    let wamn_api = wamn_api(model_name, model, table, &operation_sql);
    let native_bind_fixtures = native_bind_fixtures(&wamn_api);
    emit_parity(files, model_name, table, &wamn_api)?;
    emit_projection(
        files,
        model_name,
        table,
        &operation_sql,
        ProjectionContents::Native {
            operation_rows: &native_operation_rows,
            bind_fixtures: &native_bind_fixtures,
        },
    )?;
    emit_projection(
        files,
        model_name,
        table,
        &operation_sql,
        ProjectionContents::Wamn(&wamn_api),
    )?;
    insert_json(
        files,
        &format!("generated/source-map/{model_name}.json"),
        &json!({
            "model": model_name,
            "relation": format!("catalog-ir://{}.{}", model.schema, model.table),
            "manifest": format!("wamn.json#/models/{model_name}"),
            "operations": operation_sql,
            "native_operation_rows": native_operation_rows,
            "native_bind_fixtures": native_bind_fixtures,
            "wamn_api": wamn_api,
        }),
    )
}

fn emit_command(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    command_name: &str,
    command: &CommandDeclaration,
) -> Result<(), GenerateError> {
    let module_name = command_name.replace('.', "_");
    let native_rows = command_rows(command, Projection::Native);
    let wamn_rows = command_rows(command, Projection::Wamn);
    let accessors = command_accessors(command);
    let bind_fixtures = command_native_bind_fixtures(&accessors);

    emit_command_contracts(files, manifest, command_name, command)?;
    emit_command_parity(files, &module_name, command, &accessors)?;
    emit_command_projection(
        files,
        &module_name,
        command,
        &native_rows,
        &bind_fixtures,
        Projection::Native,
    )?;
    emit_command_projection(
        files,
        &module_name,
        command,
        &wamn_rows,
        &[],
        Projection::Wamn,
    )?;
    insert_json(
        files,
        &format!("generated/source-map/{module_name}.json"),
        &json!({
            "command": command_name,
            "manifest": format!("wamn.json#/commands/{command_name}"),
            "relations": command.relations,
            "statements": command.statements,
            "native_rows": native_rows,
            "native_bind_fixtures": bind_fixtures,
            "wamn_rows": wamn_rows,
            "wamn_accessors": accessors,
        }),
    )
}

fn emit_command_contracts(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    command_name: &str,
    command: &CommandDeclaration,
) -> Result<(), GenerateError> {
    let (module, operation) = command_name
        .split_once('.')
        .expect("command validation requires module.operation");
    let root = format!("generated/contracts/{module}/{operation}");
    let operation_id = canonical_operation_identity(&manifest.package, command_name)?;
    let sql_files = command
        .statements
        .values()
        .map(|statement| statement.path.as_str())
        .collect::<Vec<_>>();
    insert_json(
        files,
        &format!("{root}.operation.json"),
        &json!({
            "operation": operation_id,
            "permission_token": command.permission,
            "grant": operation_id,
            "connection": command.connection,
            "result": command.result.class,
            "sql_files": sql_files,
            "transaction": command.transaction,
            "automatic_retry": command.automatic_retry,
        }),
    )?;
    insert_json(
        files,
        &format!("{root}.input.json"),
        &json!({
            "raw_body_bytes": {
                "maximum": command.input.raw_body_maximum,
                "owner": "ingress_pre_parse",
                "refusal": "http_413",
            },
            "envelope": command.input.envelope,
            "item_semantics": command.input.item_semantics,
            "line": command.input.line,
            "fields": command.input.fields,
            "canonicalization": command.canonicalization,
        }),
    )?;
    insert_json(files, &format!("{root}.result.json"), &command.result)?;
    insert_json(
        files,
        &format!("{root}.errors.json"),
        &command_error_contract(command),
    )
}

fn command_error_contract(command: &CommandDeclaration) -> Value {
    let cases = command
        .errors
        .iter()
        .map(|literal| {
            let mut case = match literal {
                CommandErrorLiteral::InvalidInput => json!({
                    "literal": literal.as_str(),
                    "from": [
                        "malformed_input",
                        "envelope_count",
                        "line_count",
                        "duplicate_line",
                        "nonpositive_quantity",
                    ],
                }),
                CommandErrorLiteral::ReceiptReferenceConflict => json!({
                    "literal": literal.as_str(),
                    "from": "unique_violation",
                    "constraint": "receipt_purchase_order_id_receipt_reference_key",
                }),
                CommandErrorLiteral::Retry => json!({
                    "literal": literal.as_str(),
                    "from": ["serialization_failure", "connection_unavailable"],
                    "automatic": false,
                }),
                CommandErrorLiteral::Timeout => json!({
                    "literal": literal.as_str(),
                    "from": "statement_timeout",
                }),
                CommandErrorLiteral::PermissionDenied => json!({
                    "literal": literal.as_str(),
                    "from": "permission_denied",
                }),
                CommandErrorLiteral::InternalError => json!({
                    "literal": literal.as_str(),
                    "from": ["query_error", "row_limit_exceeded", "undeclared_constraint"],
                }),
                CommandErrorLiteral::IdempotencyConflict => json!({
                    "literal": literal.as_str(),
                    "from": "same_key_different_canonical_command",
                }),
                _ => json!({
                "literal": literal.as_str(),
                "from": "transaction_invariant",
                }),
            };
            case.as_object_mut()
                .expect("error contract case is an object")
                .insert(
                    "detail".to_owned(),
                    error_detail_contract(
                        command
                            .error_details
                            .get(literal)
                            .expect("manifest validation closed command error details"),
                    ),
                );
            case
        })
        .collect::<Vec<_>>();
    json!({"closed": true, "cases": cases})
}

fn error_detail_contract(detail: &OperationErrorDetailDeclaration) -> Value {
    let mut contract = serde_json::Map::new();
    if !detail.required.is_empty() {
        contract.insert("required".to_owned(), json!(detail.required));
    }
    if !detail.optional.is_empty() {
        contract.insert("optional".to_owned(), json!(detail.optional));
    }
    Value::Object(contract)
}

fn command_rows(command: &CommandDeclaration, projection: Projection) -> Vec<RustRow> {
    command
        .statements
        .iter()
        .map(|(name, statement)| RustRow {
            name: format!("{}Row", pascal_case(name)),
            visibility: RustVisibility::Crate,
            fields: statement
                .row
                .iter()
                .map(|field| RustMember {
                    name: rust_field(&field.name),
                    rust_type: projected_rust_type(field.ty, projection, field.nullable),
                })
                .collect(),
        })
        .collect()
}

fn command_accessors(command: &CommandDeclaration) -> Vec<CommandAccessor> {
    command
        .statements
        .iter()
        .map(|(name, statement)| CommandAccessor {
            name: name.clone(),
            sql_constant: format!("{}_SQL", name.to_ascii_uppercase()),
            row: format!("{}Row", pascal_case(name)),
            fetch: statement.fetch,
            binds: statement
                .parameters
                .iter()
                .map(|parameter| accessor_bind(&parameter.name, parameter.ty, parameter.nullable))
                .collect(),
        })
        .collect()
}

fn command_native_bind_fixtures(accessors: &[CommandAccessor]) -> Vec<NativeBindFixture> {
    accessors
        .iter()
        .flat_map(|accessor| {
            accessor.binds.iter().map(|bind| NativeBindFixture {
                accessor: accessor.name.clone(),
                parameter: bind.parameter.clone(),
                function: format!("{}_{}_bind_fixture", accessor.name, bind.parameter),
                visibility: RustVisibility::Crate,
                rust_type: bind.native_rust.clone(),
                value: native_inert_value(bind),
            })
        })
        .collect()
}

fn emit_command_parity(
    files: &mut BTreeMap<String, Vec<u8>>,
    module_name: &str,
    command: &CommandDeclaration,
    accessors: &[CommandAccessor],
) -> Result<(), GenerateError> {
    let fields = command
        .statements
        .iter()
        .flat_map(|(statement, declaration)| {
            declaration.row.iter().map(move |field| {
                json!({
                    "field": format!("{statement}.{}", field.name),
                    "postgres": sql::postgres_type(field.ty),
                    "wamn_sql_value": field.ty.as_str(),
                    "native_rust": projected_rust_type(field.ty, Projection::Native, field.nullable),
                    "wamn_rust": projected_rust_type(field.ty, Projection::Wamn, field.nullable),
                    "nullable": field.nullable,
                })
            })
        })
        .collect::<Vec<_>>();
    let accessor_binds = accessors
        .iter()
        .flat_map(|accessor| {
            accessor.binds.iter().map(|bind| {
                json!({
                    "accessor": accessor.name,
                    "parameter": bind.parameter,
                    "postgres": bind.postgres,
                    "nullable": bind.nullable,
                    "native_rust": bind.native_rust,
                    "wamn_rust": bind.wamn_rust,
                })
            })
        })
        .collect::<Vec<_>>();
    insert_json(
        files,
        &format!("generated/parity/{module_name}.json"),
        &json!({
            "model": module_name,
            "rule": "same_sql_file_two_projection_structs",
            "fields": fields,
            "accessor_binds": accessor_binds,
        }),
    )
}

fn emit_command_projection(
    files: &mut BTreeMap<String, Vec<u8>>,
    module_name: &str,
    command: &CommandDeclaration,
    rows: &[RustRow],
    bind_fixtures: &[NativeBindFixture],
    projection: Projection,
) -> Result<(), GenerateError> {
    let mut source = String::from("// @generated from migration IR; do not edit.\n\n");
    if matches!(projection, Projection::Wamn) {
        source.push_str(
            "use sqlx_core::query_as::query_as;\nuse sqlx_core::transaction::Transaction;\nuse wamn_postgres_sqlx::WamnPostgres;\n\n",
        );
    }
    for row in rows {
        emit_rust_row(&mut source, row);
    }
    for (name, statement) in &command.statements {
        writeln!(
            &mut source,
            "pub(crate) const {}_SQL: &str = include_str!(\"../../{}\");",
            name.to_ascii_uppercase(),
            statement.path,
        )
        .expect("writing to a String cannot fail");
    }
    source.push('\n');
    for fixture in bind_fixtures {
        emit_native_bind_fixture(&mut source, fixture);
    }
    if !bind_fixtures.is_empty() {
        source.push('\n');
    }
    if matches!(projection, Projection::Wamn) {
        for accessor in command_accessors(command) {
            emit_command_wamn_accessor(&mut source, &accessor);
        }
    }
    while source.ends_with("\n\n") {
        source.pop();
    }
    let directory = match projection {
        Projection::Native => "native-verifier",
        Projection::Wamn => "wamn",
    };
    insert_bytes(
        files,
        &format!("generated/{directory}/{module_name}.rs"),
        source.into_bytes(),
    )
}

fn emit_command_wamn_accessor(source: &mut String, accessor: &CommandAccessor) {
    writeln!(source, "pub(crate) async fn {}(", accessor.name)
        .expect("writing to a String cannot fail");
    source.push_str("    transaction: &mut Transaction<'_, WamnPostgres>,\n");
    for bind in &accessor.binds {
        writeln!(source, "    {}: {},", bind.parameter, bind.wamn_rust)
            .expect("writing to a String cannot fail");
    }
    writeln!(
        source,
        ") -> Result<{}, sqlx_core::error::Error> {{",
        command_accessor_result_type(accessor),
    )
    .expect("writing to a String cannot fail");
    writeln!(
        source,
        "    query_as::<WamnPostgres, {}>({})",
        accessor.row, accessor.sql_constant,
    )
    .expect("writing to a String cannot fail");
    for bind in &accessor.binds {
        writeln!(source, "        .bind({})", bind.parameter)
            .expect("writing to a String cannot fail");
    }
    writeln!(
        source,
        "        .{}(&mut **transaction)\n        .await\n}}\n",
        match accessor.fetch {
            CommandFetch::OptionalOne => "fetch_optional",
            CommandFetch::BoundedList => "fetch_all",
            CommandFetch::One => "fetch_one",
        },
    )
    .expect("writing to a String cannot fail");
}

fn command_accessor_result_type(accessor: &CommandAccessor) -> String {
    match accessor.fetch {
        CommandFetch::OptionalOne => format!("Option<{}>", accessor.row),
        CommandFetch::BoundedList => format!("Vec<{}>", accessor.row),
        CommandFetch::One => accessor.row.clone(),
    }
}

fn emit_cursor_contract(files: &mut BTreeMap<String, Vec<u8>>) -> Result<(), GenerateError> {
    insert_json(
        files,
        "generated/contracts/cursor-v1.json",
        &json!({
            "version": CURSOR_VERSION,
            "payload": "canonical_compact_json",
            "member_order": ["direction", "field", "id", "key", "v"],
            "key": "bare_value_typed_by_manifest_ir",
            "timestamptz": "utc_rfc3339_six_fractional_digits",
            "numeric": "postgresql_lexical_scale_preserved",
            "encoding": "base64url_unpadded",
            "invalid": [
                "decode_failure",
                "unknown_version",
                "field_mismatch",
                "direction_mismatch",
                "key_parse_failure",
                "noncanonical_payload",
            ],
            "refusal": "invalid_input",
            "fallback_to_first_page": false,
        }),
    )
}

fn emit_model_contract(
    files: &mut BTreeMap<String, Vec<u8>>,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
) -> Result<(), GenerateError> {
    let server_owned = model.server_owned_fields.iter().collect::<BTreeSet<_>>();
    let fields = table
        .columns()
        .iter()
        .map(|column| {
            json!({
                "name": column.name(),
                "type": column.column_type().as_str(),
                "nullable": column.nullable(),
                "server_owned": server_owned.contains(&column.name().to_owned()),
                "enum_values": model.enum_fields.get(column.name()),
            })
        })
        .collect::<Vec<_>>();
    insert_json(
        files,
        &format!("generated/models/{model_name}.json"),
        &json!({
            "model": model_name,
            "schema": model.schema,
            "table": model.table,
            "owner": model.owner,
            "fields": fields,
        }),
    )
}

fn emit_parity(
    files: &mut BTreeMap<String, Vec<u8>>,
    model_name: &str,
    table: &Table,
    wamn_api: &WamnApi,
) -> Result<(), GenerateError> {
    let fields = table
        .columns()
        .iter()
        .map(|column| {
            json!({
                "field": column.name(),
                "postgres": sql::postgres_type(column.column_type()),
                "wamn_sql_value": column.column_type().as_str(),
                "native_rust": rust_type(column, Projection::Native),
                "wamn_rust": rust_type(column, Projection::Wamn),
                "nullable": column.nullable(),
            })
        })
        .collect::<Vec<_>>();
    let accessor_binds = wamn_api
        .accessors
        .iter()
        .flat_map(|accessor| {
            accessor.binds.iter().map(|bind| {
                json!({
                    "accessor": accessor.name,
                    "parameter": bind.parameter,
                    "postgres": bind.postgres,
                    "nullable": bind.nullable,
                    "native_rust": bind.native_rust,
                    "wamn_rust": bind.wamn_rust,
                })
            })
        })
        .collect::<Vec<_>>();
    insert_json(
        files,
        &format!("generated/parity/{model_name}.json"),
        &json!({
            "model": model_name,
            "rule": "same_sql_file_two_projection_structs",
            "fields": fields,
            "accessor_binds": accessor_binds,
        }),
    )
}

fn emit_operation_sql(
    files: &mut BTreeMap<String, Vec<u8>>,
    sql_corpus: &mut BTreeMap<String, Vec<u8>>,
    model_name: &str,
    _model: &ModelDeclaration,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Result<Vec<String>, GenerateError> {
    if action == CrudAction::Query {
        if let Some(authored) = &operation.authored_sql {
            return Ok(authored
                .variants
                .iter()
                .map(|variant| variant.path.clone())
                .collect());
        }
        let variants = query_variants(operation);
        let mut paths = Vec::with_capacity(variants.len());
        for (field, direction) in variants {
            let path = format!(
                "generated/sql/{model_name}/query_{field}_{}.sql",
                sql::direction_name(direction)
            );
            let bytes = sql::query(table, operation, field, direction).into_bytes();
            insert_bytes(files, &path, bytes.clone())?;
            if sql_corpus.insert(path.clone(), bytes).is_some() {
                return Err(GenerateError::for_path(
                    GenerateErrorKind::DuplicatePath,
                    "generated SQL collides with the corpus",
                    path,
                ));
            }
            paths.push(path);
        }
        return Ok(paths);
    }

    let sql = match action {
        CrudAction::Get => sql::get(table),
        CrudAction::Create => sql::create(table, operation),
        CrudAction::Update => sql::update(table, operation),
        CrudAction::Delete => sql::delete(table, operation),
        CrudAction::Query => unreachable!("query returned above"),
    };
    let path = format!("generated/sql/{model_name}/{}.sql", action.as_str());
    let bytes = sql.into_bytes();
    insert_bytes(files, &path, bytes.clone())?;
    if sql_corpus.insert(path.clone(), bytes).is_some() {
        return Err(GenerateError::for_path(
            GenerateErrorKind::DuplicatePath,
            "generated SQL collides with the corpus",
            path,
        ));
    }
    Ok(vec![path])
}

fn query_variants(operation: &OperationDeclaration) -> Vec<(&str, CursorDirection)> {
    if let Some(authored) = &operation.authored_sql {
        return authored
            .variants
            .iter()
            .map(|variant| (variant.field.as_str(), variant.direction))
            .collect();
    }
    operation.sort.as_ref().map_or_else(
        || {
            let pagination = operation
                .pagination
                .as_ref()
                .expect("query validation requires pagination");
            vec![(
                pagination.default_sort.field.as_str(),
                pagination.default_sort.direction,
            )]
        },
        |sort| {
            sort.fields
                .iter()
                .flat_map(|field| {
                    sort.directions
                        .iter()
                        .map(move |direction| (field.as_str(), *direction))
                })
                .collect()
        },
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "operation contract generation owns this complete validated context"
)]
fn emit_operation_contracts(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
    sql_paths: &[String],
) -> Result<(), GenerateError> {
    let operation_id = canonical_operation_identity(
        &manifest.package,
        &format!("{model_name}.{}", action.as_str()),
    )?;
    let root = format!("generated/contracts/{model_name}/{}", action.as_str());
    insert_json(
        files,
        &format!("{root}.operation.json"),
        &json!({
            "operation": operation_id,
            "permission_token": operation.permission,
            "grant": operation_id,
            "result": operation.result,
            "sql_files": sql_paths,
            "transaction": "implicit",
            "automatic_retry": false,
        }),
    )?;
    insert_json(
        files,
        &format!("{root}.input.json"),
        &input_contract(model, table, action, operation),
    )?;
    insert_json(
        files,
        &format!("{root}.errors.json"),
        &error_contract(table, action, operation),
    )
}

fn input_contract(
    model: &ModelDeclaration,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Value {
    let writable = operation
        .writable_fields
        .iter()
        .map(|field| {
            let column = column(table, field).expect("validation resolved writable fields");
            json!({
                "field": field,
                "type": column.column_type().as_str(),
                "omitted": if action == CrudAction::Update { "unchanged" } else { "postgres_default" },
                "explicit_null": if column.nullable() { "accepted" } else { "invalid_input" },
            })
        })
        .collect::<Vec<_>>();
    let common = json!({
        "request_id": {"type": "string", "required": true},
        "server_owned_fields": {
            "fields": model.server_owned_fields,
            "if_supplied": "invalid_input",
        },
        "writable_fields": writable,
    });
    match action {
        CrudAction::Get => merge_json(common, &json!({"id": {"type": "uuid", "required": true}})),
        CrudAction::Query => merge_json(
            common,
            &json!({
                "filters": operation.filters,
                "sort": operation.sort,
                "pagination": operation.pagination,
                "limit": operation.limit,
                "validation_order": ["cursor", "limit", "sql"],
                "invalid_cursor": "invalid_input",
                "invalid_limit": "invalid_input",
            }),
        ),
        CrudAction::Create => common,
        CrudAction::Update | CrudAction::Delete => {
            let revision = operation
                .revision_field
                .as_deref()
                .expect("mutation validation requires a revision field");
            let mut mutation = json!({
                "id": {"type": "uuid", "required": true},
            });
            mutation
                .as_object_mut()
                .expect("mutation input contract is an object")
                .insert(
                    format!("expected_{revision}"),
                    json!({
                        "field": revision,
                        "type": "int64",
                        "required": true,
                    }),
                );
            merge_json(common, &mutation)
        }
    }
}

fn error_contract(table: &Table, action: CrudAction, operation: &OperationDeclaration) -> Value {
    use AccessOperationErrorLiteral as Code;

    let mut cases = vec![
        (Code::InvalidInput, json!({"literal": "invalid_input"})),
        (
            Code::PermissionDenied,
            json!({"literal": "permission_denied", "from": "permission_denied"}),
        ),
        (
            Code::Retry,
            json!({
                "literal": "retry",
                "from": ["serialization_failure", "connection_unavailable"],
                "automatic": false,
            }),
        ),
        (
            Code::Timeout,
            json!({"literal": "timeout", "from": "statement_timeout"}),
        ),
        (
            Code::InternalError,
            json!({
                "literal": "internal_error",
                "from": ["query_error", "row_limit_exceeded"],
            }),
        ),
    ];
    if matches!(
        action,
        CrudAction::Get | CrudAction::Update | CrudAction::Delete
    ) {
        cases.push((Code::NotFound, json!({"literal": "not_found"})));
    }
    if matches!(action, CrudAction::Update | CrudAction::Delete) {
        cases.push((
            Code::ConcurrencyConflict,
            json!({"literal": "concurrency_conflict"}),
        ));
    }
    for constraint in operation_constraints(table, action, operation) {
        let code = constraint_error_code(constraint.kind());
        cases.push((
            code,
            json!({
                "literal": constraint_error(constraint.kind()),
                "from": constraint_error(constraint.kind()),
                "constraint": constraint.name(),
            }),
        ));
    }
    let cases = cases
        .into_iter()
        .map(|(code, mut case)| {
            case.as_object_mut()
                .expect("error contract case is an object")
                .insert(
                    "detail".to_owned(),
                    error_detail_contract(
                        operation
                            .error_details
                            .get(&code)
                            .expect("manifest validation closed access error details"),
                    ),
                );
            case
        })
        .collect::<Vec<_>>();
    json!({"closed": true, "cases": cases})
}

fn constraint_error_code(kind: &ConstraintKind) -> AccessOperationErrorLiteral {
    match kind {
        ConstraintKind::PrimaryKey { .. } | ConstraintKind::Unique { .. } => {
            AccessOperationErrorLiteral::UniqueViolation
        }
        ConstraintKind::ForeignKey { .. } => AccessOperationErrorLiteral::ForeignKeyViolation,
        ConstraintKind::Check { .. } => AccessOperationErrorLiteral::CheckViolation,
    }
}

fn operation_constraints<'a>(
    table: &'a Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Vec<&'a Constraint> {
    if !matches!(
        action,
        CrudAction::Create | CrudAction::Update | CrudAction::Delete
    ) {
        return Vec::new();
    }
    table
        .constraints()
        .iter()
        .filter(|constraint| {
            action != CrudAction::Update
                || update_can_violate(constraint.kind(), &operation.writable_fields)
        })
        .collect()
}

fn update_can_violate(kind: &ConstraintKind, writable_fields: &[String]) -> bool {
    match kind {
        // Opaque CHECK expressions expose no structural field set to intersect.
        ConstraintKind::Check { .. } => false,
        ConstraintKind::PrimaryKey { columns } | ConstraintKind::Unique { columns } => {
            columns.iter().any(|column| {
                writable_fields
                    .iter()
                    .any(|field| field.as_str() == column.as_ref())
            })
        }
        ConstraintKind::ForeignKey { columns, .. } => columns.iter().any(|column| {
            writable_fields
                .iter()
                .any(|field| field.as_str() == column.column())
        }),
    }
}

#[derive(Debug, Clone, Copy)]
enum Projection {
    Native,
    Wamn,
}

#[derive(Debug, Clone, Copy)]
enum ProjectionContents<'a> {
    Native {
        operation_rows: &'a [RustRow],
        bind_fixtures: &'a [NativeBindFixture],
    },
    Wamn(&'a WamnApi),
}

#[derive(Debug, Serialize)]
struct WamnApi {
    sql_constant_visibility: RustVisibility,
    mutation_constraints: Vec<MutationConstraintNames>,
    operation_rows: Vec<RustRow>,
    accessors: Vec<WamnAccessor>,
}

#[derive(Debug, Serialize)]
struct MutationConstraintNames {
    operation: CrudAction,
    unique: ConstraintNameSlice,
    foreign_key: ConstraintNameSlice,
    check: ConstraintNameSlice,
}

#[derive(Debug, Serialize)]
struct ConstraintNameSlice {
    constant: String,
    visibility: RustVisibility,
    names: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RustRow {
    name: String,
    visibility: RustVisibility,
    fields: Vec<RustMember>,
}

#[derive(Debug, Serialize)]
struct RustMember {
    name: String,
    #[serde(rename = "type")]
    rust_type: String,
}

#[derive(Debug, Serialize)]
struct AccessorBind {
    parameter: String,
    postgres: String,
    nullable: bool,
    native_rust: String,
    wamn_rust: String,
}

#[derive(Debug, Serialize)]
struct WamnAccessor {
    name: String,
    visibility: RustVisibility,
    operation: CrudAction,
    sql_constant: String,
    row: String,
    fetch: AccessorFetch,
    binds: Vec<AccessorBind>,
}

#[derive(Debug, Serialize)]
struct CommandAccessor {
    name: String,
    sql_constant: String,
    row: String,
    fetch: CommandFetch,
    binds: Vec<AccessorBind>,
}

#[derive(Debug, Serialize)]
struct NativeBindFixture {
    accessor: String,
    parameter: String,
    function: String,
    visibility: RustVisibility,
    #[serde(rename = "type")]
    rust_type: String,
    #[serde(skip)]
    value: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum AccessorFetch {
    Optional,
    All,
    One,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RustVisibility {
    Public,
    Crate,
}

impl RustVisibility {
    const fn source(self) -> &'static str {
        match self {
            Self::Public => "pub",
            Self::Crate => "pub(crate)",
        }
    }
}

fn wamn_api(
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    operation_sql: &BTreeMap<String, Vec<String>>,
) -> WamnApi {
    let model_row = format!("{}Row", pascal_case(model_name));
    let mut mutation_constraints = Vec::new();
    let mut operation_rows = Vec::new();
    let mut accessors = Vec::new();

    for (action, operation) in &model.operations {
        let paths = operation_sql
            .get(action.as_str())
            .expect("operation SQL was emitted from the same manifest");
        if matches!(
            action,
            CrudAction::Create | CrudAction::Update | CrudAction::Delete
        ) {
            mutation_constraints.push(mutation_constraint_names(table, *action, operation));
        }
        match action {
            CrudAction::Get => accessors.push(WamnAccessor {
                name: "get".to_owned(),
                visibility: RustVisibility::Crate,
                operation: *action,
                sql_constant: sql_constant_name(action.as_str(), 0, paths.len()),
                row: model_row.clone(),
                fetch: AccessorFetch::Optional,
                binds: vec![bind_for_column(table, "id", "id", false)],
            }),
            CrudAction::Query => {
                for (index, (field, direction)) in query_variants(operation).into_iter().enumerate()
                {
                    let mut binds = operation
                        .filters
                        .iter()
                        .map(|filter| {
                            accessor_bind(
                                &format!("{}_filter", filter.field),
                                ColumnType::Json,
                                true,
                            )
                        })
                        .collect::<Vec<_>>();
                    binds.push(bind_for_column(table, field, "cursor_key", true));
                    let tie_breaker = &operation
                        .pagination
                        .as_ref()
                        .expect("query validation requires pagination")
                        .tie_breaker
                        .field;
                    binds.push(bind_for_column(table, tie_breaker, "cursor_id", true));
                    binds.push(accessor_bind("limit", ColumnType::Int64, false));
                    accessors.push(WamnAccessor {
                        name: format!("query_{field}_{}", sql::direction_name(direction)),
                        visibility: RustVisibility::Crate,
                        operation: *action,
                        sql_constant: sql_constant_name(action.as_str(), index, paths.len()),
                        row: model_row.clone(),
                        fetch: AccessorFetch::All,
                        binds,
                    });
                }
            }
            CrudAction::Create => accessors.push(WamnAccessor {
                name: "create".to_owned(),
                visibility: RustVisibility::Crate,
                operation: *action,
                sql_constant: sql_constant_name(action.as_str(), 0, paths.len()),
                row: model_row.clone(),
                fetch: AccessorFetch::One,
                binds: operation
                    .writable_fields
                    .iter()
                    .map(|field| bind_for_column(table, field, &rust_field(field), false))
                    .collect(),
            }),
            CrudAction::Update => {
                let result_row =
                    operation_result_row(model_name, table, *action, operation, Projection::Wamn);
                let mut binds = vec![bind_for_column(table, "id", "id", false)];
                let revision = operation
                    .revision_field
                    .as_deref()
                    .expect("update validation requires a revision field");
                binds.push(bind_for_column(
                    table,
                    revision,
                    &format!("expected_{revision}"),
                    false,
                ));
                for field in &operation.writable_fields {
                    binds.push(accessor_bind(
                        &format!("{field}_present"),
                        ColumnType::Boolean,
                        false,
                    ));
                    binds.push(bind_for_column(
                        table,
                        field,
                        &format!("{field}_value"),
                        true,
                    ));
                }
                accessors.push(WamnAccessor {
                    name: "update".to_owned(),
                    visibility: RustVisibility::Crate,
                    operation: *action,
                    sql_constant: sql_constant_name(action.as_str(), 0, paths.len()),
                    row: result_row.name.clone(),
                    fetch: AccessorFetch::One,
                    binds,
                });
                operation_rows.push(result_row);
            }
            CrudAction::Delete => {
                let result_row =
                    operation_result_row(model_name, table, *action, operation, Projection::Wamn);
                let revision = operation
                    .revision_field
                    .as_deref()
                    .expect("delete validation requires a revision field");
                accessors.push(WamnAccessor {
                    name: "delete".to_owned(),
                    visibility: RustVisibility::Crate,
                    operation: *action,
                    sql_constant: sql_constant_name(action.as_str(), 0, paths.len()),
                    row: result_row.name.clone(),
                    fetch: AccessorFetch::One,
                    binds: vec![
                        bind_for_column(table, "id", "id", false),
                        bind_for_column(table, revision, &format!("expected_{revision}"), false),
                    ],
                });
                operation_rows.push(result_row);
            }
        }
    }

    WamnApi {
        sql_constant_visibility: RustVisibility::Crate,
        mutation_constraints,
        operation_rows,
        accessors,
    }
}

fn mutation_constraint_names(
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> MutationConstraintNames {
    let mut unique = Vec::new();
    let mut foreign_key = Vec::new();
    let mut check = Vec::new();
    for constraint in operation_constraints(table, action, operation) {
        match constraint.kind() {
            ConstraintKind::PrimaryKey { .. } | ConstraintKind::Unique { .. } => {
                unique.push(constraint.name().to_owned());
            }
            ConstraintKind::ForeignKey { .. } => {
                foreign_key.push(constraint.name().to_owned());
            }
            ConstraintKind::Check { .. } => {
                check.push(constraint.name().to_owned());
            }
        }
    }
    MutationConstraintNames {
        operation: action,
        unique: constraint_name_slice(action, "unique", unique),
        foreign_key: constraint_name_slice(action, "foreign_key", foreign_key),
        check: constraint_name_slice(action, "check", check),
    }
}

fn constraint_name_slice(
    action: CrudAction,
    category: &str,
    names: Vec<String>,
) -> ConstraintNameSlice {
    ConstraintNameSlice {
        constant: format!(
            "{}_{}_CONSTRAINTS",
            action.as_str().to_ascii_uppercase(),
            category.to_ascii_uppercase()
        ),
        visibility: RustVisibility::Crate,
        names,
    }
}

fn native_bind_fixtures(api: &WamnApi) -> Vec<NativeBindFixture> {
    api.accessors
        .iter()
        .flat_map(|accessor| {
            accessor.binds.iter().map(|bind| NativeBindFixture {
                accessor: accessor.name.clone(),
                parameter: bind.parameter.clone(),
                function: format!("{}_{}_bind_fixture", accessor.name, bind.parameter),
                visibility: RustVisibility::Crate,
                rust_type: bind.native_rust.clone(),
                value: native_inert_value(bind),
            })
        })
        .collect()
}

fn native_inert_value(bind: &AccessorBind) -> String {
    if bind.nullable {
        return "None".to_owned();
    }
    match bind.postgres.as_str() {
        "boolean" => "false".to_owned(),
        "int4" => "0_i32".to_owned(),
        "int8" => "0_i64".to_owned(),
        "float8" => "0.0_f64".to_owned(),
        "text" => "String::new()".to_owned(),
        "bytea" => "Vec::new()".to_owned(),
        "numeric" => "rust_decimal::Decimal::ZERO".to_owned(),
        "timestamptz" => "chrono::DateTime::<chrono::Utc>::UNIX_EPOCH".to_owned(),
        "jsonb" => "serde_json::Value::Null".to_owned(),
        "uuid" => "uuid::Uuid::nil()".to_owned(),
        _ => unreachable!("accessor binds use the closed PostgreSQL vocabulary"),
    }
}

fn operation_result_rows(
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    projection: Projection,
) -> Vec<RustRow> {
    model
        .operations
        .iter()
        .filter(|(action, _)| matches!(action, CrudAction::Update | CrudAction::Delete))
        .map(|(action, operation)| {
            operation_result_row(model_name, table, *action, operation, projection)
        })
        .collect()
}

fn operation_result_row(
    model_name: &str,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
    projection: Projection,
) -> RustRow {
    let mut fields = vec![RustMember {
        name: "outcome".to_owned(),
        rust_type: "Option<String>".to_owned(),
    }];
    if action == CrudAction::Update {
        let revision = operation
            .revision_field
            .as_deref()
            .expect("update validation requires a revision field");
        fields.push(RustMember {
            name: format!("observed_{}", rust_field(revision)),
            rust_type: "Option<i64>".to_owned(),
        });
        fields.extend(table.columns().iter().map(|column| RustMember {
            name: rust_field(column.name()),
            rust_type: optional_rust_type(column, projection),
        }));
    }
    RustRow {
        name: format!(
            "{}{}Row",
            pascal_case(model_name),
            pascal_case(action.as_str())
        ),
        visibility: RustVisibility::Public,
        fields,
    }
}

fn bind_for_column(
    table: &Table,
    column_name: &str,
    parameter_name: &str,
    optional: bool,
) -> AccessorBind {
    let column = column(table, column_name).expect("operation validation resolved the column");
    accessor_bind(
        parameter_name,
        column.column_type(),
        optional || column.nullable(),
    )
}

fn accessor_bind(parameter: &str, ty: ColumnType, nullable: bool) -> AccessorBind {
    AccessorBind {
        parameter: parameter.to_owned(),
        postgres: sql::postgres_type(ty).to_owned(),
        nullable,
        native_rust: projected_rust_type(ty, Projection::Native, nullable),
        wamn_rust: projected_rust_type(ty, Projection::Wamn, nullable),
    }
}

fn emit_projection(
    files: &mut BTreeMap<String, Vec<u8>>,
    model_name: &str,
    table: &Table,
    operation_sql: &BTreeMap<String, Vec<String>>,
    contents: ProjectionContents<'_>,
) -> Result<(), GenerateError> {
    let (projection, operation_rows, native_bind_fixtures, wamn_api) = match contents {
        ProjectionContents::Native {
            operation_rows,
            bind_fixtures,
        } => (Projection::Native, operation_rows, bind_fixtures, None),
        ProjectionContents::Wamn(api) => (
            Projection::Wamn,
            api.operation_rows.as_slice(),
            &[][..],
            Some(api),
        ),
    };
    let mut source = String::from("// @generated from migration IR; do not edit.\n\n");
    if matches!(projection, Projection::Wamn) {
        source.push_str(
            "use sqlx_core::query_as::query_as;\nuse wamn_postgres_sqlx::{WamnConnection, WamnPostgres};\n\n",
        );
    }
    let model_row = RustRow {
        name: format!("{}Row", pascal_case(model_name)),
        visibility: RustVisibility::Public,
        fields: table
            .columns()
            .iter()
            .map(|column| RustMember {
                name: rust_field(column.name()),
                rust_type: rust_type(column, projection),
            })
            .collect(),
    };
    emit_rust_row(&mut source, &model_row);
    for row in operation_rows {
        emit_rust_row(&mut source, row);
    }

    for (action, paths) in operation_sql {
        for (index, path) in paths.iter().enumerate() {
            let include_path = if path.starts_with("generated/") {
                path.strip_prefix("generated/")
                    .map(|path| format!("../{path}"))
                    .expect("prefix checked")
            } else {
                format!("../../{path}")
            };
            writeln!(
                &mut source,
                "{} const {}: &str = include_str!(\"{}\");",
                wamn_api
                    .map_or(RustVisibility::Crate, |api| api.sql_constant_visibility)
                    .source(),
                sql_constant_name(action, index, paths.len()),
                include_path
            )
            .expect("writing to a String cannot fail");
        }
    }
    source.push('\n');
    for fixture in native_bind_fixtures {
        emit_native_bind_fixture(&mut source, fixture);
    }
    if !native_bind_fixtures.is_empty() {
        source.push('\n');
    }
    if let Some(api) = wamn_api {
        for constraints in &api.mutation_constraints {
            emit_constraint_name_slice(&mut source, &constraints.unique);
            emit_constraint_name_slice(&mut source, &constraints.foreign_key);
            emit_constraint_name_slice(&mut source, &constraints.check);
        }
        if !api.mutation_constraints.is_empty() {
            source.push('\n');
        }
        for accessor in &api.accessors {
            emit_wamn_accessor(&mut source, accessor);
        }
    }
    while source.ends_with("\n\n") {
        source.pop();
    }
    let directory = match projection {
        Projection::Native => "native-verifier",
        Projection::Wamn => "wamn",
    };
    insert_bytes(
        files,
        &format!("generated/{directory}/{model_name}.rs"),
        source.into_bytes(),
    )
}

fn emit_native_bind_fixture(source: &mut String, fixture: &NativeBindFixture) {
    writeln!(
        source,
        "{} fn {}() -> {} {{\n    {}\n}}",
        fixture.visibility.source(),
        fixture.function,
        fixture.rust_type,
        fixture.value
    )
    .expect("writing to a String cannot fail");
}

fn emit_constraint_name_slice(source: &mut String, constraints: &ConstraintNameSlice) {
    let names = constraints
        .names
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        source,
        "{} const {}: &[&str] = &[{}];",
        constraints.visibility.source(),
        constraints.constant,
        names
    )
    .expect("writing to a String cannot fail");
}

fn emit_rust_row(source: &mut String, row: &RustRow) {
    source.push_str("#[derive(Debug, sqlx::FromRow)]\n");
    writeln!(source, "{} struct {} {{", row.visibility.source(), row.name)
        .expect("writing to a String cannot fail");
    for field in &row.fields {
        writeln!(source, "    pub {}: {},", field.name, field.rust_type)
            .expect("writing to a String cannot fail");
    }
    source.push_str("}\n\n");
}

fn emit_wamn_accessor(source: &mut String, accessor: &WamnAccessor) {
    writeln!(
        source,
        "{} async fn {}(",
        accessor.visibility.source(),
        accessor.name
    )
    .expect("writing to a String cannot fail");
    source.push_str("    connection: &mut WamnConnection,\n");
    for bind in &accessor.binds {
        writeln!(source, "    {}: {},", bind.parameter, bind.wamn_rust)
            .expect("writing to a String cannot fail");
    }
    writeln!(
        source,
        ") -> Result<{}, sqlx_core::error::Error> {{",
        accessor_result_type(accessor)
    )
    .expect("writing to a String cannot fail");
    writeln!(
        source,
        "    query_as::<WamnPostgres, {}>({})",
        accessor.row, accessor.sql_constant
    )
    .expect("writing to a String cannot fail");
    for bind in &accessor.binds {
        writeln!(source, "        .bind({})", bind.parameter)
            .expect("writing to a String cannot fail");
    }
    writeln!(
        source,
        "        .{}(connection)\n        .await\n}}\n",
        match accessor.fetch {
            AccessorFetch::Optional => "fetch_optional",
            AccessorFetch::All => "fetch_all",
            AccessorFetch::One => "fetch_one",
        }
    )
    .expect("writing to a String cannot fail");
}

fn accessor_result_type(accessor: &WamnAccessor) -> String {
    match accessor.fetch {
        AccessorFetch::Optional => format!("Option<{}>", accessor.row),
        AccessorFetch::All => format!("Vec<{}>", accessor.row),
        AccessorFetch::One => accessor.row.clone(),
    }
}

fn sql_constant_name(action: &str, index: usize, path_count: usize) -> String {
    let suffix = if path_count == 1 {
        String::new()
    } else {
        format!("_{index}")
    };
    format!("{}{}_SQL", action.to_ascii_uppercase(), suffix)
}

fn required_schema_contract(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
) -> RequiredSchemaContract {
    let mut consumed =
        BTreeMap::<(String, String), (&Table, BTreeSet<String>, BTreeSet<String>)>::new();
    for model in manifest.models.values() {
        let table = relation(catalog, model).expect("validation resolved relation");
        let entry = consumed
            .entry((model.schema.clone(), model.table.clone()))
            .or_insert_with(|| (table, BTreeSet::new(), BTreeSet::new()));
        entry.1.extend(
            table
                .columns()
                .iter()
                .map(|column| column.name().to_owned()),
        );
        entry.2.extend(
            table
                .constraints()
                .iter()
                .map(|constraint| constraint.name().to_owned()),
        );
    }
    for command in manifest.commands.values() {
        for relation in &command.relations {
            let table = catalog
                .tables()
                .iter()
                .find(|table| table.schema() == relation.schema && table.name() == relation.table)
                .expect("command validation resolved relation");
            let entry = consumed
                .entry((relation.schema.clone(), relation.table.clone()))
                .or_insert_with(|| (table, BTreeSet::new(), BTreeSet::new()));
            entry.1.extend(
                relation
                    .select_fields
                    .iter()
                    .chain(&relation.insert_fields)
                    .chain(&relation.update_fields)
                    .cloned(),
            );
            entry.2.extend(relation.constraints.iter().cloned());
        }
    }
    let tables = consumed
        .into_values()
        .map(|(table, fields, constraints)| RequiredTable {
            schema: table.schema().into(),
            table: table.name().into(),
            fields: table
                .columns()
                .iter()
                .filter(|column| fields.contains(column.name()))
                .map(|column| RequiredField {
                    name: column.name().into(),
                    ty: column.column_type().as_str().into(),
                    nullable: column.nullable(),
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            constraints: table
                .constraints()
                .iter()
                .filter(|constraint| constraints.contains(constraint.name()))
                .map(|constraint| RequiredConstraint {
                    name: constraint.name().into(),
                    definition: serde_json::to_value(constraint.kind())
                        .expect("constraint IR always serializes"),
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    RequiredSchemaContract { tables }
}

fn relation<'a>(catalog: &'a CatalogIr, model: &ModelDeclaration) -> Option<&'a Table> {
    catalog
        .tables()
        .iter()
        .find(|table| table.schema() == model.schema && table.name() == model.table)
}

fn column<'a>(table: &'a Table, name: &str) -> Option<&'a Column> {
    table.columns().iter().find(|column| column.name() == name)
}

fn validate_field<'a>(
    table: &'a Table,
    model_name: &str,
    field: &str,
) -> Result<&'a Column, GenerateError> {
    column(table, field).ok_or_else(|| {
        GenerateError::for_object(
            GenerateErrorKind::UnknownColumn,
            format!("{model_name} references unknown field {field}"),
            field,
        )
    })
}

fn safe_sql_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension == "sql")
        && !path.contains('\\')
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn insert_json(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    value: &impl Serialize,
) -> Result<(), GenerateError> {
    insert_bytes(
        files,
        path,
        serde_json::to_vec(value).expect("generated JSON values always serialize"),
    )
}

fn insert_canonical_json(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    value: &impl Serialize,
) -> Result<(), GenerateError> {
    insert_bytes(
        files,
        path,
        canonical_json_bytes(
            &serde_json::to_value(value).expect("generated JSON values always serialize"),
        ),
    )
}

fn insert_bytes(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    bytes: Vec<u8>,
) -> Result<(), GenerateError> {
    if files.insert(path.to_owned(), bytes).is_some() {
        Err(GenerateError::for_path(
            GenerateErrorKind::DuplicatePath,
            "generated artifact path is repeated",
            path,
        ))
    } else {
        Ok(())
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn merge_json(mut left: Value, right: &Value) -> Value {
    let left = left
        .as_object_mut()
        .expect("input contracts are JSON objects");
    left.extend(
        right
            .as_object()
            .expect("input contract additions are JSON objects")
            .clone(),
    );
    Value::Object(left.clone())
}

fn constraint_error(kind: &ConstraintKind) -> &'static str {
    match kind {
        ConstraintKind::PrimaryKey { .. } | ConstraintKind::Unique { .. } => "unique_violation",
        ConstraintKind::ForeignKey { .. } => "foreign_key_violation",
        ConstraintKind::Check { .. } => "check_violation",
    }
}

fn rust_type(column: &Column, projection: Projection) -> String {
    projected_rust_type(column.column_type(), projection, column.nullable())
}

fn optional_rust_type(column: &Column, projection: Projection) -> String {
    projected_rust_type(column.column_type(), projection, true)
}

fn projected_rust_type(ty: ColumnType, projection: Projection, optional: bool) -> String {
    let base = match (projection, ty) {
        (_, ColumnType::Boolean) => "bool",
        (_, ColumnType::Int32) => "i32",
        (_, ColumnType::Int64) => "i64",
        (_, ColumnType::Float64) => "f64",
        (_, ColumnType::Text) => "String",
        (_, ColumnType::Bytes) => "Vec<u8>",
        (Projection::Native, ColumnType::Numeric) => "rust_decimal::Decimal",
        (Projection::Native, ColumnType::Timestamptz) => "chrono::DateTime<chrono::Utc>",
        (Projection::Native, ColumnType::Json) => "serde_json::Value",
        (Projection::Native, ColumnType::Uuid) => "uuid::Uuid",
        (Projection::Wamn, ColumnType::Numeric) => "wamn_postgres_sqlx::Numeric",
        (Projection::Wamn, ColumnType::Timestamptz) => "wamn_postgres_sqlx::TimestampTz",
        (Projection::Wamn, ColumnType::Json) => "wamn_postgres_sqlx::Json",
        (Projection::Wamn, ColumnType::Uuid) => "wamn_postgres_sqlx::Uuid",
    };
    if optional {
        format!("Option<{base}>")
    } else {
        base.to_owned()
    }
}

fn pascal_case(value: &str) -> String {
    value
        .split('_')
        .map(|part| {
            let mut characters = part.chars();
            match characters.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn rust_field(value: &str) -> String {
    const KEYWORDS: [&str; 8] = [
        "as", "crate", "enum", "match", "mod", "self", "struct", "type",
    ];
    if KEYWORDS.contains(&value) {
        format!("r#{value}")
    } else {
        value.to_owned()
    }
}
