mod component;
mod contracts;
mod data;
mod publication;
mod routes;
mod rust;
mod validation;
mod wit;
mod wit_adapters;

use contracts::{
    emit_cursor_contract, emit_custom_operation, emit_model, required_schema_contract,
};
use validation::{authored_sql_map, validate};

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wamn_execution_contract::canonical_json_bytes;
use wamn_record_history::history_table_name;
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnType, Constraint, ConstraintType, Exclusion, ForeignKeyAction, Table,
};

use crate::manifest::{
    AccessOperationErrorLiteral, AuthoredSqlDeclaration, ContractFieldDeclaration, CrudAction,
    CursorDirection, CustomOperationDeclaration, CustomOperationResultDeclaration,
    CustomOperationType, DeleteMode, FieldText, ModelDeclaration, OperationDeclaration,
    OperationErrorDetailDeclaration, PackageManifest, PolicyContractRequirement,
    PolicyContractState, RecordHistoryColumn, ResultClass, SortDeclaration, StaticSqlFetch,
    TombstoneColumn, binding_identifier, canonical_operation_identity, custom_artifact_stem,
    rust_identifier, rust_type_identifier, validate_identifier, validate_operation_vocabulary,
};
use crate::sql;
use crate::sql_lex::contains_schema_qualified_reference;
use crate::{GenerateError, GenerateErrorType};

const QUERY_LIMIT: u32 = 100;
const CURSOR_VERSION: u8 = 1;
/// The create input field that carries the caller's idempotency key.
pub(crate) const CREATE_KEY_FIELD: &str = "idempotency_key";
/// Insert the row. Its defaults mint its identities, and `RETURNING` hands them back.
const CREATE_STATEMENT: &str = "create";

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

    /// Exact authored bytes included in the SQL corpus.
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// Generator and toolchain facts in each generated package.
#[derive(Debug, Clone, Copy)]
pub struct GenerationProvenance<'a> {
    generator: &'a str,
    toolchain: &'a str,
}

impl<'a> GenerationProvenance<'a> {
    /// Construct generator provenance without consulting git, environment, or a clock.
    pub const fn new(generator: &'a str, toolchain: &'a str) -> Self {
        Self {
            generator,
            toolchain,
        }
    }
}

/// PostgreSQL's verdict on which statements need a transaction, keyed by the
/// statement's corpus path.
///
/// A statement NEEDS A TRANSACTION when its generic plan carries a
/// `ModifyTable` node (it writes -- data-modifying CTEs included) or a
/// `LockRows` node (it takes a row lock, which autocommit would drop the
/// instant the statement returned). The verdict is the server's, taken at
/// generation time against the already-migrated database; nothing here reads
/// SQL text.
///
/// Generation runs TWICE: once to obtain the corpus with every verdict absent,
/// then again with the verdicts the server gave for that corpus. The bit is
/// contract-only and never reaches SQL bytes, so the corpus is identical across
/// both passes -- which `application_sql_corpus_identity` would catch if it
/// were not.
#[derive(Debug, Clone, Default)]
pub struct StatementTransactionality {
    paths: BTreeMap<String, bool>,
}

impl StatementTransactionality {
    /// Build from the server's verdicts, keyed by corpus path.
    #[must_use]
    pub const fn from_paths(paths: BTreeMap<String, bool>) -> Self {
        Self { paths }
    }

    /// Absent verdicts, for the corpus-discovery pass.
    #[must_use]
    pub fn unclassified() -> Self {
        Self::from_paths(BTreeMap::new())
    }

    /// The server's verdict for one corpus path. An unclassified path reads
    /// `false` ONLY during the discovery pass, whose contracts are discarded.
    #[must_use]
    pub fn needs_transaction(&self, path: &str) -> bool {
        self.paths.get(path).copied().unwrap_or(false)
    }
}

/// Complete pure input to [`generate`].
#[derive(Debug)]
pub struct GenerationInput<'a> {
    catalog: &'a CatalogIr,
    manifest_json: &'a [u8],
    authored_sql: &'a [AuthoredSql<'a>],
    provenance: GenerationProvenance<'a>,
    transactional: &'a StatementTransactionality,
}

impl<'a> GenerationInput<'a> {
    /// Construct a generation input from exact in-memory artifacts.
    pub const fn new(
        catalog: &'a CatalogIr,
        manifest_json: &'a [u8],
        authored_sql: &'a [AuthoredSql<'a>],
        provenance: GenerationProvenance<'a>,
        transactional: &'a StatementTransactionality,
    ) -> Self {
        Self {
            catalog,
            manifest_json,
            authored_sql,
            provenance,
            transactional,
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
    /// One generated artifact. Sibling modules emit their own files; the
    /// fields stay private so a path and its bytes are always set together.
    ///
    /// A `.rs` artifact is formatted here, at the single point every emitter
    /// passes through, so that a generated Rust file is committed formatted and
    /// the drift check compares the same bytes a write would have placed on
    /// disk. JSON, TOML, and SQL bytes are kept exactly as emitted.
    pub(crate) fn new(path: Box<str>, bytes: Box<[u8]>) -> Self {
        let bytes = if path.ends_with(".rs") {
            crate::rustfmt::format_rust(&bytes).into_boxed_slice()
        } else {
            bytes
        };
        Self { path, bytes }
    }

    /// Package-relative artifact path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact deterministic artifact bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Canonical immutable package metadata emitted as `generated/package-identity.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename = "PackageIdentity",
    expecting = "struct PackageIdentity",
    deny_unknown_fields
)]
pub struct GeneratedPackageMetadata {
    verified_schema_state_id: Box<str>,
    required_schema_contract: RequiredSchemaContract,
    required_platform_policy_contract: PolicyContractRequirement,
    application_sql_corpus_identity: Box<str>,
    provenance: OwnedProvenance,
    promotion_state: PromotionState,
}

impl GeneratedPackageMetadata {
    /// Parse the canonical generated metadata and refuse alternate spellings.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, GenerateError> {
        let metadata: Self = serde_json::from_slice(bytes).map_err(|source| {
            GenerateError::with_source(
                GenerateErrorType::InvalidManifest,
                "package-identity.json does not match the package contract",
                source,
            )
        })?;
        let canonical = canonical_json_bytes(
            &serde_json::to_value(&metadata).expect("generated package metadata always serializes"),
        );
        if canonical != bytes {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidManifest,
                "package-identity.json is not canonical compact JSON",
            ));
        }
        for (field, value) in [
            (
                "verified_schema_state_id",
                metadata.verified_schema_state_id(),
            ),
            (
                "application_sql_corpus_identity",
                metadata.application_sql_corpus_identity(),
            ),
        ] {
            if !valid_sha256(value) {
                return Err(GenerateError::new(
                    GenerateErrorType::InvalidManifest,
                    format!("package-identity.json {field} is not sha256:<64 lowercase hex>"),
                ));
            }
        }
        let expected_promotion_state = match metadata.required_platform_policy_contract.state {
            PolicyContractState::Unsatisfied => PromotionState::BlockedUnsatisfiedPolicyContract,
            PolicyContractState::Satisfied => PromotionState::Eligible,
        };
        if metadata.promotion_state != expected_promotion_state {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidManifest,
                "package-identity.json promotion_state disagrees with required_platform_policy_contract.state",
            ));
        }
        Ok(metadata)
    }

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

/// Generated package files and their metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedPackage {
    files: Box<[GeneratedFile]>,
    metadata: GeneratedPackageMetadata,
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

    /// Canonical metadata included in the generated file set.
    pub const fn metadata(&self) -> &GeneratedPackageMetadata {
        &self.metadata
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredSchemaContract {
    tables: Box<[RequiredTable]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredTable {
    schema: Box<str>,
    table: Box<str>,
    fields: Box<[RequiredField]>,
    constraints: Box<[RequiredConstraint]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredField {
    name: Box<str>,
    #[serde(rename = "type")]
    ty: Box<str>,
    nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredConstraint {
    name: Box<str>,
    definition: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedProvenance {
    generator: Box<str>,
    toolchain: Box<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PromotionState {
    BlockedUnsatisfiedPolicyContract,
    Eligible,
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Compile an authored manifest: add the members that generation derives.
///
/// The author states no relation of a custom operation and no stamp column
/// (docs/plan/platform-deploy.md §6.1). Each relation comes from the
/// operation's statements over the catalog, and the compiled manifest that
/// every reader reads carries it. The other authored members keep their bytes
/// and order. A model's `enum_fields` are checked here: an authored list only
/// narrows a CHECK's value set or names a set that no CHECK carries, and
/// generation reads the rest from the CHECKs ([`generate`]).
///
/// # Errors
///
/// When the manifest is invalid or states a derived member, when an authored
/// statement is missing, when the SQL cannot be derived, or when a CHECK that
/// carries a quoted value is not of the form `col IN ('a', ...)`.
pub fn derive_manifest(
    catalog: &CatalogIr,
    manifest_json: &[u8],
    authored_sql: &[AuthoredSql<'_>],
) -> Result<Vec<u8>, GenerateError> {
    let manifest = PackageManifest::from_slice(manifest_json)?;
    validation::validate_authored_sources(&manifest, authored_sql)?;
    let mut relations = BTreeMap::new();
    for (name, operation) in &manifest.custom_operations {
        if !operation.relations.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!(
                    "{name} states its relations, which generation derives from its statements"
                ),
            ));
        }
        if operation.statements.is_empty() {
            continue;
        }
        relations.insert(
            name.clone(),
            validation::derive_relations(catalog, &manifest, authored_sql, name, operation)?,
        );
    }
    for (name, model) in &manifest.models {
        if let Some(stamp) = model.server_owned_fields.iter().find(|field| {
            RecordHistoryColumn::ALL
                .iter()
                .any(|column| column.as_str() == field.as_str())
        }) {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidModel,
                format!(
                    "{name} states stamp column {stamp} as server-owned; generation reads each stamp from the table, so remove it"
                ),
            ));
        }
        let Some(table) = relation(catalog, model) else {
            continue;
        };
        model_enum_fields(name, model, table)?;
    }
    let compiled =
        crate::authoring::with_derived_relations(manifest_json, &relations).map_err(|source| {
            GenerateError::with_source(
                GenerateErrorType::InvalidManifest,
                "write the derived members into the compiled manifest",
                source,
            )
        })?;
    PackageManifest::from_slice(&compiled)?;
    Ok(compiled)
}

/// Generate a package without filesystem, database, clock, or environment I/O.
///
/// The one child process is `rustfmt`, which formats each emitted `.rs`
/// artifact. It reads and writes only the bytes it is handed.
pub fn generate(input: &GenerationInput<'_>) -> Result<GeneratedPackage, GenerateError> {
    let mut manifest = PackageManifest::from_slice(input.manifest_json)?;
    complete_enum_fields(input.catalog, &mut manifest);
    validate(input, &manifest)?;

    let mut files = BTreeMap::<String, Vec<u8>>::new();
    let mut sql_corpus = authored_sql_map(input.authored_sql)?;
    emit_cursor_contract(&mut files)?;

    for (model_name, model) in &manifest.models {
        let table = relation(input.catalog, model).expect("validation resolved every relation");
        emit_model(
            &mut files,
            &mut sql_corpus,
            input.transactional,
            input.catalog,
            &manifest,
            model_name,
            model,
            table,
        )?;
    }
    for (operation_name, operation) in &manifest.custom_operations {
        emit_custom_operation(
            &mut files,
            &sql_corpus,
            input.transactional,
            input.catalog,
            &manifest,
            operation_name,
            operation,
        )?;
    }
    data::emit_package_data(&mut files, &manifest)?;
    component::emit_package_component(&mut files, &manifest)?;
    publication::emit_package_publication(&mut files, &manifest)?;
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

    let metadata = GeneratedPackageMetadata {
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
            generator: input.provenance.generator.into(),
            toolchain: input.provenance.toolchain.into(),
        },
        promotion_state: match manifest.required_platform_policy_contract.state {
            PolicyContractState::Unsatisfied => PromotionState::BlockedUnsatisfiedPolicyContract,
            PolicyContractState::Satisfied => PromotionState::Eligible,
        },
    };
    insert_canonical_json(&mut files, "generated/package-identity.json", &metadata)?;

    let files = files
        .into_iter()
        .map(|(path, bytes)| GeneratedFile::new(path.into_boxed_str(), bytes.into_boxed_slice()))
        .collect::<Vec<_>>()
        .into_boxed_slice();

    Ok(GeneratedPackage { files, metadata })
}

/// The schema and history table name of each logged model.
///
/// A history table stays out of the catalog. Generation recognizes it by the
/// history suffix, and its columns are [`wamn_record_history::HISTORY_COLUMNS`].
pub(crate) fn logged_history_tables(
    manifest: &PackageManifest,
) -> impl Iterator<Item = (&str, String)> {
    manifest
        .models
        .values()
        .filter(|model| model.log_retention().is_some())
        .map(|model| (model.schema.as_str(), history_table_name(&model.table)))
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

/// The `from` literal for the constraint a custom operation maps, whether the
/// database enforces it as a [`Constraint`] or as an exclusion constraint.
fn custom_operation_constraint_origin(
    catalog: &CatalogIr,
    operation: &CustomOperationDeclaration,
    name: &str,
) -> Option<&'static str> {
    for relation in &operation.relations {
        if !relation
            .constraints
            .iter()
            .any(|constraint| constraint == name)
        {
            continue;
        }
        let table = catalog
            .tables()
            .iter()
            .find(|table| table.schema() == relation.schema && table.name() == relation.table)?;
        if let Some(constraint) = table
            .constraints()
            .iter()
            .find(|constraint| constraint.name() == name)
        {
            return Some(constraint_error(constraint.constraint_type()));
        }
        if table
            .exclusions()
            .iter()
            .any(|exclusion| exclusion.name() == name)
        {
            return Some("exclusion_violation");
        }
    }
    None
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

fn constraint_error_code(kind: &ConstraintType) -> AccessOperationErrorLiteral {
    match kind {
        ConstraintType::PrimaryKey { .. } | ConstraintType::Unique { .. } => {
            AccessOperationErrorLiteral::UniqueViolation
        }
        ConstraintType::ForeignKey { .. } => AccessOperationErrorLiteral::ForeignKeyViolation,
        ConstraintType::Check { .. } => AccessOperationErrorLiteral::CheckViolation,
    }
}

/// The input path of one field this operation writes, as its input states it.
fn writable_path(action: CrudAction, field: &str) -> String {
    if action == CrudAction::Update {
        format!("change.{field}")
    } else {
        field.to_owned()
    }
}

/// The input path that a refusal of this constraint names, or none.
///
/// A constraint over one column that the operation writes guards that field,
/// so its refusal names the field as the input states it. A constraint over
/// several columns, a check, or a column the caller does not write names none.
fn constraint_field(
    constraint: &Constraint,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Option<String> {
    let column = match constraint.constraint_type() {
        ConstraintType::PrimaryKey { columns } | ConstraintType::Unique { columns } => {
            match &**columns {
                [column] => &**column,
                _ => return None,
            }
        }
        ConstraintType::ForeignKey { columns, .. } => match &**columns {
            [column] => column.column(),
            _ => return None,
        },
        ConstraintType::Check { .. } => return None,
    };
    operation
        .writable_fields
        .iter()
        .any(|field| field == column)
        .then(|| writable_path(action, column))
}

fn operation_error_details(
    catalog: &CatalogIr,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
    model: &ModelDeclaration,
) -> BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration> {
    use AccessOperationErrorLiteral as Code;

    let mut codes = BTreeSet::from([
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
        codes.insert(Code::NotFound);
    }
    if matches!(action, CrudAction::Update | CrudAction::Delete) {
        codes.insert(Code::ConcurrencyConflict);
    }
    if action == CrudAction::Create {
        codes.insert(Code::IdempotencyConflict);
    }
    codes.extend(
        operation_constraints(catalog, table, action, operation, model)
            .into_iter()
            .map(|constraint| constraint_error_code(constraint.constraint_type())),
    );
    if !operation_exclusions(table, action, operation).is_empty() {
        codes.insert(Code::ExclusionViolation);
    }
    codes
        .into_iter()
        .map(|code| {
            (
                code,
                crate::manifest::access_operation_error_detail(action, code),
            )
        })
        .collect()
}

/// Constraints the operation can violate.
///
/// An INSERT or an UPDATE can violate a constraint OF ITS OWN TABLE. A hard
/// DELETE cannot: removing a row breaks no primary key, unique key, check, or
/// outbound foreign key that the row itself carries. What it breaks is an
/// INBOUND foreign key, held by another table whose row still references this
/// one, and PostgreSQL names THAT constraint in its 23503.
///
/// A tombstone is an UPDATE of its two marker columns, so it runs through the
/// same filter every other update runs through. The answer is usually empty,
/// but it is DERIVED rather than assumed: a unique or foreign key that names a
/// marker column is reported like any other. It writes no referenced column, so
/// no inbound key fires.
///
/// This set predicts CONSTRAINTS only. A trigger guard, such as the immutable
/// row guards in `deploy/sql`, refuses a write without any constraint, and no
/// constraint set of any shape predicts that refusal.
fn operation_constraints<'a>(
    catalog: &'a CatalogIr,
    table: &'a Table,
    action: CrudAction,
    operation: &OperationDeclaration,
    model: &ModelDeclaration,
) -> Vec<&'a Constraint> {
    if action == CrudAction::Delete {
        return match model.delete_mode {
            Some(DeleteMode::Hard) => inbound_foreign_keys(catalog, table),
            Some(DeleteMode::Tombstone) => {
                let marker = TombstoneColumn::ALL.map(|column| column.as_str().to_owned());
                table
                    .constraints()
                    .iter()
                    .filter(|constraint| update_can_violate(constraint.constraint_type(), &marker))
                    .collect()
            }
            None => Vec::new(),
        };
    }
    if !matches!(action, CrudAction::Create | CrudAction::Update) {
        return Vec::new();
    }
    table
        .constraints()
        .iter()
        .filter(|constraint| {
            action != CrudAction::Update
                || update_can_violate(constraint.constraint_type(), &operation.writable_fields)
        })
        // The codec refuses a value shorter than its minimum first, so the
        // CHECK that guards it is never the answer.
        .filter(|constraint| !guards_min_length(constraint, model))
        .collect()
}

/// The CHECK expression that guards a minimum length, as PostgreSQL prints it.
fn min_length_check(field: &str, minimum: u32) -> String {
    format!("(char_length(btrim({field})) >= {minimum})")
}

/// Whether a constraint is the CHECK of a minimum length the model declares.
fn guards_min_length(constraint: &Constraint, model: &ModelDeclaration) -> bool {
    matches!(constraint.constraint_type(), ConstraintType::Check { expression }
    if model.min_lengths.iter().any(|(field, minimum)| {
        **expression == *min_length_check(field, *minimum)
    }))
}

/// Every foreign key in the catalog that references this relation and can
/// refuse a delete.
///
/// A key whose `on_delete` removes or clears the referencing row cannot refuse,
/// so it is not a constraint the caller ever sees.
fn inbound_foreign_keys<'a>(catalog: &'a CatalogIr, table: &'a Table) -> Vec<&'a Constraint> {
    catalog
        .tables()
        .iter()
        .flat_map(Table::constraints)
        .filter(|constraint| match constraint.constraint_type() {
            ConstraintType::ForeignKey {
                referenced_schema,
                referenced_table,
                on_delete,
                ..
            } => {
                referenced_schema.as_ref() == table.schema()
                    && referenced_table.as_ref() == table.name()
                    && matches!(
                        on_delete,
                        ForeignKeyAction::NoAction | ForeignKeyAction::Restrict
                    )
            }
            _ => false,
        })
        .collect()
}

/// Exclusions the operation can violate.
///
/// An INSERT can always collide with a row already stored. An UPDATE can only
/// collide when it writes a column the constraint depends on -- including a
/// column an expression key reads, which PostgreSQL records as a dependency and
/// [`Exclusion::columns`] carries. A DELETE never can: removing a row cannot
/// create an overlap.
fn operation_exclusions<'a>(
    table: &'a Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Vec<&'a Exclusion> {
    if !matches!(action, CrudAction::Create | CrudAction::Update) {
        return Vec::new();
    }
    table
        .exclusions()
        .iter()
        .filter(|exclusion| {
            action != CrudAction::Update
                || exclusion.columns().iter().any(|column| {
                    operation
                        .writable_fields
                        .iter()
                        .any(|field| field.as_str() == column.as_ref())
                })
        })
        .collect()
}

fn update_can_violate(kind: &ConstraintType, writable_fields: &[String]) -> bool {
    match kind {
        // Opaque CHECK expressions expose no structural field set to intersect.
        ConstraintType::Check { .. } => false,
        ConstraintType::PrimaryKey { columns } | ConstraintType::Unique { columns } => {
            columns.iter().any(|column| {
                writable_fields
                    .iter()
                    .any(|field| field.as_str() == column.as_ref())
            })
        }
        ConstraintType::ForeignKey { columns, .. } => columns.iter().any(|column| {
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
    statement_digest_visibility: RustVisibility,
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
    exclusion: ConstraintNameSlice,
}

#[derive(Debug, Serialize)]
struct ConstraintNameSlice {
    constant: String,
    visibility: RustVisibility,
    names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RustRow {
    name: String,
    visibility: RustVisibility,
    fields: Vec<RustMember>,
}

#[derive(Debug, Clone, Serialize)]
struct RustMember {
    name: String,
    #[serde(rename = "type")]
    rust_type: String,
    #[serde(skip)]
    statement_type: ColumnType,
    #[serde(skip)]
    nullable: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AccessorBind {
    parameter: String,
    postgres: String,
    nullable: bool,
    native_rust: String,
    wamn_rust: String,
    #[serde(skip)]
    statement_type: ColumnType,
}

#[derive(Debug, Serialize)]
struct WamnAccessor {
    name: String,
    visibility: RustVisibility,
    operation: CrudAction,
    statement_digest_constant: String,
    #[serde(skip)]
    sql_path: String,
    row: String,
    fetch: AccessorFetch,
    binds: Vec<AccessorBind>,
}

#[derive(Debug, Serialize)]
struct StaticSqlAccessor {
    name: String,
    statement_digest_constant: String,
    row: String,
    fetch: StaticSqlFetch,
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

#[derive(Debug, Serialize)]
struct StatementContract {
    name: String,
    path: String,
    digest: String,
    binds: Vec<StatementValueContract>,
    columns: Vec<StatementValueContract>,
    /// PostgreSQL's own verdict, from the statement's generic plan against the
    /// migrated database: a `ModifyTable` node (it writes, data-modifying CTEs
    /// included) or a `LockRows` node (it takes a row lock, which autocommit
    /// would drop the instant the statement returned). Never read off the text.
    transactional: bool,
}

#[derive(Debug, Clone, Serialize)]
struct StatementValueContract {
    name: String,
    #[serde(rename = "type")]
    ty: ColumnType,
    nullable: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum AccessorFetch {
    Optional,
    /// Every row, as the server sends it: a query's page or streamed load.
    Stream,
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

fn relation<'a>(catalog: &'a CatalogIr, model: &ModelDeclaration) -> Option<&'a Table> {
    catalog
        .tables()
        .iter()
        .find(|table| table.schema() == model.schema && table.name() == model.table)
}

/// Give each model the value set of every text column whose CHECK has the form
/// `col IN ('a', ...)`, unless the manifest states a list for that column.
///
/// The schema owns these sets, so the compiled manifest carries only the
/// authored lists, and [`derive_manifest`] has refused an authored list that
/// widens a set or states it again.
fn complete_enum_fields(catalog: &CatalogIr, manifest: &mut PackageManifest) {
    for model in manifest.models.values_mut() {
        let Some(table) = relation(catalog, model) else {
            continue;
        };
        for constraint in table.constraints() {
            let ConstraintType::Check { expression } = constraint.constraint_type() else {
                continue;
            };
            if let Some((field, values)) = check_value_set(expression).filter(|(field, _)| {
                column(table, field).is_some_and(|column| column.column_type() == ColumnType::Text)
            }) {
                model.enum_fields.entry(field).or_insert(values);
            }
        }
    }
}

/// The value set of each text column of the model's table, read from its
/// CHECKs, with each authored list that narrows one.
///
/// A CHECK that carries a quoted value is a value set, and it takes the form
/// `col IN ('a', ...)`, which PostgreSQL stores as
/// `(col = ANY (ARRAY['a'::text, ...]))`. Any other CHECK with a quoted value
/// refuses with its name, and a CHECK without one is not a value set. An
/// authored list equal to the CHECK's set is the derived set stated again, and
/// one that names a value outside it widens it: both refuse.
fn model_enum_fields(
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
) -> Result<BTreeMap<String, Vec<String>>, GenerateError> {
    let mut values = BTreeMap::new();
    for constraint in table.constraints() {
        let ConstraintType::Check { expression } = constraint.constraint_type() else {
            continue;
        };
        if !expression.contains('\'') {
            continue;
        }
        let (field, set) = check_value_set(expression)
            .filter(|(field, _)| {
                column(table, field).is_some_and(|column| column.column_type() == ColumnType::Text)
            })
            .ok_or_else(|| {
                GenerateError::for_object(
                    GenerateErrorType::InvalidModel,
                    format!(
                        "{model_name} CHECK {} carries a quoted value but is not of the form col IN ('a', ...) on one text column: {expression}",
                        constraint.name()
                    ),
                    format!("{}.{}.{}", table.schema(), table.name(), constraint.name()),
                )
            })?;
        if values.insert(field.clone(), set).is_some() {
            return Err(GenerateError::for_object(
                GenerateErrorType::InvalidModel,
                format!(
                    "{model_name}.{field} has more than one value-set CHECK; {} is the second",
                    constraint.name()
                ),
                format!("{}.{}.{}", table.schema(), table.name(), constraint.name()),
            ));
        }
    }
    for (field, declared) in &model.enum_fields {
        if let Some(derived) = values.get(field) {
            let derived_set = derived.iter().collect::<BTreeSet<_>>();
            let declared_set = declared.iter().collect::<BTreeSet<_>>();
            if declared_set == derived_set {
                return Err(GenerateError::new(
                    GenerateErrorType::InvalidModel,
                    format!(
                        "{model_name}.{field} states enum_fields that its CHECK already carries; remove it"
                    ),
                ));
            }
            if !declared_set.is_subset(&derived_set) {
                return Err(GenerateError::new(
                    GenerateErrorType::InvalidModel,
                    format!(
                        "{model_name}.{field} enum_fields names a value that its CHECK refuses; a declared list narrows the CHECK and never widens it"
                    ),
                ));
            }
        }
        values.insert(field.clone(), declared.clone());
    }
    Ok(values)
}

/// The column and values of `(col = ANY (ARRAY['a'::text, ...]))`, the one
/// stored form of `col IN ('a', ...)`, or `None` for any other expression.
fn check_value_set(expression: &str) -> Option<(String, Vec<String>)> {
    let expression = expression
        .strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
        .unwrap_or(expression);
    let (field, items) = expression
        .strip_suffix("])")?
        .split_once(" = ANY (ARRAY[")?;
    if field.is_empty()
        || !field
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }
    let values = items
        .split(", ")
        .map(|item| {
            item.strip_prefix('\'')?
                .strip_suffix("'::text")
                .filter(|value| !value.is_empty() && !value.contains('\''))
                .map(str::to_owned)
        })
        .collect::<Option<Vec<_>>>()?;
    Some((field.to_owned(), values))
}

fn column<'a>(table: &'a Table, name: &str) -> Option<&'a Column> {
    table.columns().iter().find(|column| column.name() == name)
}

/// Declared server-owned fields, then every reserved record-history and
/// tombstone column.
///
/// Validation refuses an unselected reserved-name column on an owned relation.
/// On an overlay, the base declaration selects every reserved-name base column.
/// A tombstone marker is set by the delete statement, so no caller writes it.
fn server_owned_fields<'a>(model: &'a ModelDeclaration, table: &'a Table) -> Vec<&'a str> {
    let mut fields = model
        .server_owned_fields
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let reserved_names = RecordHistoryColumn::ALL
        .map(RecordHistoryColumn::as_str)
        .into_iter()
        .chain(TombstoneColumn::ALL.map(TombstoneColumn::as_str));
    for reserved in reserved_names {
        if column(table, reserved).is_some() && !fields.contains(&reserved) {
            fields.push(reserved);
        }
    }
    fields
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

fn insert_json_line(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    value: &impl Serialize,
) -> Result<(), GenerateError> {
    let mut bytes = serde_json::to_vec(value).expect("generated JSON values always serialize");
    bytes.push(b'\n');
    insert_bytes(files, path, bytes)
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

pub(crate) fn insert_bytes(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    bytes: Vec<u8>,
) -> Result<(), GenerateError> {
    if files.insert(path.to_owned(), bytes).is_some() {
        Err(GenerateError::for_path(
            GenerateErrorType::DuplicatePath,
            "generated artifact path is repeated",
            path,
        ))
    } else {
        Ok(())
    }
}

/// A create cannot omit a NOT NULL column that has no default, because
/// nothing would fill it. Its input contract states `omitted: invalid_input`,
/// and its codec refuses the omission on that path.
fn omission_refused(column: &Column) -> bool {
    !column.nullable() && column.default().is_none() && column.generation().is_none()
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn constraint_error(kind: &ConstraintType) -> &'static str {
    match kind {
        ConstraintType::PrimaryKey { .. } | ConstraintType::Unique { .. } => "unique_violation",
        ConstraintType::ForeignKey { .. } => "foreign_key_violation",
        ConstraintType::Check { .. } => "check_violation",
    }
}

#[cfg(test)]
mod tests {
    use super::check_value_set;

    #[test]
    fn only_the_stored_in_form_is_a_value_set() {
        let set = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
        assert_eq!(
            check_value_set("(status = ANY (ARRAY['open'::text, 'complete'::text]))"),
            Some(("status".to_owned(), set(&["open", "complete"])))
        );
        assert_eq!(
            check_value_set("code = ANY (ARRAY['priority'::text])"),
            Some(("code".to_owned(), set(&["priority"])))
        );
        for other in [
            "((status = 'open'::text) OR (status = 'complete'::text))",
            "(status <> ''::text)",
            "((kind)::text = ANY (ARRAY['a'::text]))",
            "(\"Status\" = ANY (ARRAY['a'::text]))",
            "(status = ANY (ARRAY['it''s'::text]))",
            "(status = ANY (ARRAY['a'::character varying]))",
        ] {
            assert_eq!(check_value_set(other), None, "{other}");
        }
    }
}
