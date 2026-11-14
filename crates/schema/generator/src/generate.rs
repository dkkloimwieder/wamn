use wamn_scalar::SqlValueType;
mod authored_feed;
mod component;
mod data;
mod numbering;
mod operations;
pub(crate) mod publication;
mod routes;
mod rust;
mod validation;
mod wit;
mod wit_adapters;
use operations::{emit_custom_operation, emit_model, required_schema_contract};
use validation::{authored_sql_map, validate};

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};
use wamn_digest::canonical_json_bytes;
use wamn_record_history::history_table_name;
use wamn_schema_introspection::ir::{
    CatalogIr, Column, Constraint, ConstraintType, Exclusion, ForeignKeyAction, Table,
};

use crate::manifest::{
    AccessOperationErrorLiteral, CrudAction, CursorDirection, CustomOperationDeclaration,
    DeleteMode, ModelDeclaration, OperationDeclaration, OperationErrorDetailDeclaration,
    PackageManifest, RecordHistoryColumn, StaticSqlFetch, TombstoneColumn,
};
use crate::sql;
use crate::sql_analysis::contains_schema_qualified_reference;
use crate::{ManifestError as GenerateError, ManifestErrorType as GenerateErrorType};

const QUERY_LIMIT: u32 = 100;
const CURSOR_VERSION: u8 = 1;
/// The create input field that carries the caller's idempotency key.
pub(crate) const CREATE_KEY_FIELD: &str = "idempotency_key";
/// Insert the row and return it, including any database-generated key.
const CREATE_STATEMENT: &str = "create";
/// The change feed statement of a list read with a feed.
const CHANGES_STATEMENT: &str = "changes";

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
/// SQL is emitted once. Build describes and classifies that corpus before
/// any consumer reads these facts.
#[derive(Debug, Clone)]
pub struct StatementTransactionality {
    paths: BTreeMap<String, bool>,
    pub(crate) described: BTreeMap<String, crate::describe::DescribedStatement>,
}

impl StatementTransactionality {
    /// Build from the server's verdicts, keyed by corpus path.
    #[must_use]
    pub const fn from_paths(paths: BTreeMap<String, bool>) -> Self {
        Self {
            paths,
            described: BTreeMap::new(),
        }
    }

    /// The effect of one statement. Pure callers may supply their own facts;
    /// the package build supplies a verdict for every corpus member.
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
    transactional: &'a StatementTransactionality,
}

impl<'a> GenerationInput<'a> {
    /// Construct a generation input from exact in-memory artifacts.
    /// Complete inherited declarations with [`derive_manifest`] before generation.
    pub const fn new(
        catalog: &'a CatalogIr,
        manifest_json: &'a [u8],
        authored_sql: &'a [AuthoredSql<'a>],
        transactional: &'a StatementTransactionality,
    ) -> Self {
        Self {
            catalog,
            manifest_json,
            authored_sql,
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
    /// the drift test compares the same bytes a write would have placed on
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

pub use wamn_catalog::OperationDescription;

/// Generated package files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedPackage {
    files: Box<[GeneratedFile]>,
    operations: BTreeMap<String, OperationDescription>,
}

impl GeneratedPackage {
    pub(crate) fn take_operations(&mut self) -> BTreeMap<String, OperationDescription> {
        std::mem::take(&mut self.operations)
    }
    /// Compiled descriptions keyed by local operation name.
    pub fn operations(&self) -> &BTreeMap<String, OperationDescription> {
        &self.operations
    }

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
}

/// The generated file that names the schema an installed package needs.
pub const REQUIRED_SCHEMA_CONTRACT_PATH: &str = "generated/required-schema-contract.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct RequiredSchemaContract {
    tables: Box<[RequiredTable]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct RequiredTable {
    schema: Box<str>,
    table: Box<str>,
    fields: Box<[RequiredField]>,
    constraints: Box<[RequiredConstraint]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct RequiredField {
    name: Box<str>,
    #[serde(rename = "type")]
    ty: Box<str>,
    nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct RequiredConstraint {
    name: Box<str>,
    definition: Value,
}

/// Compile an authored manifest: add the members that generation derives.
///
/// The author states no relation of a custom operation and no stamp column
/// (docs/architecture/data-access.md#application-declarations). Each relation
/// comes from the operation's statements over the catalog, and the compiled
/// manifest that every reader reads carries it. The compiler writes the typed model once. A model's `enum_fields` are validated here: an authored
/// list only narrows a CHECK's value set or names a set that no CHECK carries,
/// and generation reads the rest from the CHECKs ([`generate`]).
///
/// Supply the resolved compiled bases for every inherited relation owner.
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
    bases: &[PackageManifest],
) -> Result<Vec<u8>, GenerateError> {
    let mut manifest = PackageManifest::parse_authored(manifest_json)?;
    derive_model(catalog, &mut manifest, authored_sql, bases)?;
    serde_json::to_vec(&manifest).map_err(|source| {
        GenerateError::with_source(
            GenerateErrorType::InvalidManifest,
            "serialize compiled facts",
            source,
        )
    })
}

pub(crate) fn derive_model(
    catalog: &CatalogIr,
    manifest: &mut PackageManifest,
    authored_sql: &[AuthoredSql<'_>],
    bases: &[PackageManifest],
) -> Result<(), GenerateError> {
    manifest.refuse_authored_stamps()?;
    crate::inheritance::inherit_model_fields(manifest, bases)?;
    validation::validate_authored_sources(manifest, authored_sql)?;
    let mut relations = BTreeMap::new();
    for (name, operation) in &manifest.custom_operations {
        if operation.statements.is_empty() {
            continue;
        }
        relations.insert(
            name.clone(),
            validation::derive_relations(catalog, manifest, authored_sql, name, operation)?,
        );
    }
    let mut feed_sources = BTreeMap::new();
    for (name, operation) in &manifest.custom_operations {
        if let Some(source) = crate::feed::derive(catalog, manifest, authored_sql, name, operation)?
        {
            feed_sources.insert(name.clone(), source);
        }
    }
    for (name, derived) in relations {
        manifest
            .custom_operations
            .get_mut(&name)
            .expect("declared operation")
            .relations = derived;
    }
    for (name, source) in feed_sources {
        manifest
            .custom_operations
            .get_mut(&name)
            .expect("declared operation")
            .feed_source = Some(source);
    }
    complete_enum_fields(catalog, manifest)?;
    manifest.validate()?;
    Ok(())
}

/// Generate a package without filesystem, database, clock, or environment I/O.
///
/// The one child process is `rustfmt`, which formats each emitted `.rs`
/// artifact. It reads and writes only the bytes it is handed.
pub fn generate(input: &GenerationInput<'_>) -> Result<GeneratedPackage, GenerateError> {
    let manifest = PackageManifest::from_slice(input.manifest_json)?;
    manifest.validate()?;
    generate_compiled(input, &manifest)
}

#[derive(Debug)]
pub(crate) struct SqlCorpus {
    pub(crate) statements: BTreeMap<String, Vec<u8>>,
    pub(crate) relations: BTreeMap<String, Vec<wamn_catalog::profile::RelationProfile>>,
    model_paths: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    custom_operations: BTreeMap<String, crate::manifest::CustomOperationDeclaration>,
}

pub(crate) fn sql_corpus(
    input: &GenerationInput<'_>,
    manifest: &PackageManifest,
) -> Result<SqlCorpus, GenerateError> {
    validate(input, manifest)?;
    let mut statements = authored_sql_map(input.authored_sql)?;
    let mut model_paths = BTreeMap::new();
    for (name, model) in &manifest.models {
        let table = relation(input.catalog, model).expect("validated relation");
        let mut operations = BTreeMap::new();
        for (action, operation) in &model.operations {
            operations.insert(
                action.as_str().to_owned(),
                operations::emit_operation_sql(
                    &mut statements,
                    name,
                    table,
                    *action,
                    operation,
                    model,
                )?,
            );
        }
        model_paths.insert(name.clone(), operations);
    }
    let mut custom_operations = BTreeMap::new();
    for (name, operation) in &manifest.custom_operations {
        if let Some(prepared) =
            authored_feed::prepare_sql(&mut statements, input.catalog, name, operation)
        {
            custom_operations.insert(name.clone(), prepared);
        }
    }
    let mut available = input
        .catalog
        .tables()
        .iter()
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
        .chain(input.catalog.views().iter().map(|view| {
            (
                view.name().to_owned(),
                view.columns()
                    .iter()
                    .map(|column| column.name().to_owned())
                    .collect(),
            )
        }))
        .collect::<BTreeMap<_, _>>();
    for (_, table) in logged_history_tables(manifest) {
        available.insert(
            table,
            wamn_record_history::HISTORY_COLUMNS
                .iter()
                .map(|(name, _)| (*name).to_owned())
                .collect(),
        );
    }
    available.insert(
        "wamn_history.pruning_watermarks".into(),
        BTreeSet::from(["history_table".into(), "transaction_id".into()]),
    );
    let relations = statements
        .iter()
        .map(|(path, bytes)| {
            let effects =
                crate::sql_analysis::relation_access(bytes, &available).map_err(|detail| {
                    GenerateError::for_path(
                        GenerateErrorType::InvalidOperation,
                        detail,
                        path.as_str(),
                    )
                })?;
            Ok((
                path.clone(),
                effects
                    .into_iter()
                    .map(|(table, access)| wamn_catalog::profile::RelationProfile {
                        table,
                        reads: access.select_fields.into_iter().collect(),
                        writes: access
                            .insert_fields
                            .union(&access.update_fields)
                            .cloned()
                            .collect(),
                        deletes: access.delete,
                        locks: access.lock,
                    })
                    .collect(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, GenerateError>>()?;
    Ok(SqlCorpus {
        statements,
        relations,
        model_paths,
        custom_operations,
    })
}

pub(crate) fn generate_compiled(
    input: &GenerationInput<'_>,
    manifest: &PackageManifest,
) -> Result<GeneratedPackage, GenerateError> {
    let corpus = sql_corpus(input, manifest)?;
    generate_described(input, manifest, &corpus)
}

pub(crate) fn generate_described(
    input: &GenerationInput<'_>,
    manifest: &PackageManifest,
    corpus: &SqlCorpus,
) -> Result<GeneratedPackage, GenerateError> {
    let mut files = BTreeMap::<String, Vec<u8>>::new();
    let sql_corpus = &corpus.statements;
    for (path, bytes) in sql_corpus {
        if path.starts_with("generated/") {
            insert_bytes(&mut files, path, bytes.clone())?;
        }
    }
    let mut operations = BTreeMap::new();

    for (model_name, model) in &manifest.models {
        let table = relation(input.catalog, model).expect("validation resolved every relation");
        emit_model(
            &mut files,
            &mut operations,
            corpus,
            input.transactional,
            &corpus.model_paths[model_name],
            input.catalog,
            manifest,
            model_name,
            model,
            table,
        )?;
    }
    let custom = manifest
        .custom_operations
        .iter()
        .map(|(name, operation)| {
            (
                name.clone(),
                operations::compile_custom(
                    operation,
                    corpus.custom_operations.get(name).unwrap_or(operation),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for operation_name in custom.keys() {
        emit_custom_operation(
            &mut files,
            &mut operations,
            corpus,
            input.transactional,
            input.catalog,
            manifest,
            operation_name,
            &custom,
        )?;
    }
    data::emit_package_data(&mut files, manifest)?;
    component::emit_package_component(&mut files, manifest)?;
    publication::emit_package_publication(&mut files, manifest, &operations)?;
    insert_canonical_json(
        &mut files,
        REQUIRED_SCHEMA_CONTRACT_PATH,
        &required_schema_contract(input.catalog, manifest),
    )?;

    let files = files
        .into_iter()
        .map(|(path, bytes)| GeneratedFile::new(path.into_boxed_str(), bytes.into_boxed_slice()))
        .collect::<Vec<_>>()
        .into_boxed_slice();

    Ok(GeneratedPackage { files, operations })
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

/// The `from` literal for the constraint a custom operation maps, whether the
/// database enforces it as a [`Constraint`] or as an exclusion constraint.
fn custom_operation_constraint_origin(
    catalog: &CatalogIr,
    operation: &CustomOperationDeclaration,
    name: &str,
) -> Option<AccessOperationErrorLiteral> {
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
            return Some(constraint_error_code(constraint.constraint_type()));
        }
        if table
            .exclusions()
            .iter()
            .any(|exclusion| exclusion.name() == name)
        {
            return Some(AccessOperationErrorLiteral::ExclusionViolation);
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

fn constraint_error_code(type_: &ConstraintType) -> AccessOperationErrorLiteral {
    match type_ {
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
/// several columns, a CHECK constraint, or a column the caller does not write
/// names none.
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
    use crate::manifest::AccessOperationErrorLiteral as Code;

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
    if action == CrudAction::Query && model.feed_enabled() {
        codes.insert(Code::CursorExpired);
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
/// DELETE cannot: removing a row breaks no primary key, unique key, CHECK
/// constraint, or outbound foreign key that the row itself carries. What it breaks is an
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
        guards_length(expression, field, *minimum)
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
        .chain(catalog.referencing_constraints())
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

fn update_can_violate(type_: &ConstraintType, writable_fields: &[String]) -> bool {
    match type_ {
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

#[derive(Debug)]
struct WamnApi {
    statement_visibility: RustVisibility,
    mutation_constraints: Vec<MutationConstraintNames>,
    operation_rows: Vec<RustRow>,
    accessors: Vec<WamnAccessor>,
}

#[derive(Debug)]
struct MutationConstraintNames {
    unique: ConstraintNameSlice,
    foreign_key: ConstraintNameSlice,
    check: ConstraintNameSlice,
    exclusion: ConstraintNameSlice,
}

#[derive(Debug)]
struct ConstraintNameSlice {
    constant: String,
    visibility: RustVisibility,
    names: Vec<String>,
}

#[derive(Debug, Clone)]
struct RustRow {
    name: String,
    visibility: RustVisibility,
    fields: Vec<RustMember>,
}

#[derive(Debug, Clone)]
struct RustMember {
    name: String,
    rust_type: String,
    statement_type: SqlValueType,
    nullable: bool,
}

#[derive(Debug, Clone)]
struct AccessorBind {
    parameter: String,
    nullable: bool,
    wamn_rust: String,
    statement_type: SqlValueType,
}

#[derive(Debug)]
struct WamnAccessor {
    name: String,
    visibility: RustVisibility,
    operation: CrudAction,
    statement_constant: String,
    sql_path: String,
    row: String,
    fetch: AccessorFetch,
    binds: Vec<AccessorBind>,
}

#[derive(Debug)]
struct StaticSqlAccessor {
    name: String,
    statement_constant: String,
    row: String,
    fetch: StaticSqlFetch,
    binds: Vec<AccessorBind>,
}

type StatementContract = wamn_catalog::StatementDescription;

pub(crate) type StatementValueContract = wamn_catalog::StatementValue;

#[derive(Debug, Clone, Copy)]
enum AccessorFetch {
    Optional,
    /// Every row: a query's page and the row past it.
    All,
    One,
}

#[derive(Debug, Clone, Copy)]
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
/// The compiled manifest carries the effective sets. Authored lists can narrow
/// a database set but cannot repeat or widen it.
fn complete_enum_fields(
    catalog: &CatalogIr,
    manifest: &mut PackageManifest,
) -> Result<(), GenerateError> {
    for (name, model) in &mut manifest.models {
        if let Some(table) = relation(catalog, model) {
            model.enum_fields = model_enum_fields(name, model, table)?;
        }
    }
    Ok(())
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
                column(table, field).is_some_and(|column| column.column_type() == SqlValueType::Text)
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

/// Parse a CHECK as an expression, without matching PostgreSQL's printed SQL.
fn check_expression(expression: &str) -> Option<pg_query::NodeEnum> {
    let parsed = pg_query::parse(&format!("SELECT 1 WHERE {expression}")).ok()?;
    let pg_query::NodeEnum::SelectStmt(statement) =
        parsed.protobuf.stmts.into_iter().next()?.stmt?.node?
    else {
        return None;
    };
    statement.where_clause?.node
}

fn node_string(node: &pg_query::protobuf::Node) -> Option<&str> {
    match node.node.as_ref()? {
        pg_query::NodeEnum::String(value) => Some(&value.sval),
        _ => None,
    }
}

/// The selected column and literal text values of an IN or ANY expression.
fn check_value_set(expression: &str) -> Option<(String, Vec<String>)> {
    use pg_query::{NodeEnum, protobuf::AExprKind, protobuf::a_const::Val};
    let NodeEnum::AExpr(expression) = check_expression(expression)? else {
        return None;
    };
    if expression.kind != AExprKind::AexprOpAny as i32
        && expression.kind != AExprKind::AexprIn as i32
    {
        return None;
    }
    if expression.name.first().and_then(node_string) != Some("=") {
        return None;
    }
    let NodeEnum::ColumnRef(column) = expression.lexpr?.node? else {
        return None;
    };
    let [field] = column.fields.as_slice() else {
        return None;
    };
    let field = node_string(field)?.to_owned();
    let elements = match expression.rexpr?.node? {
        NodeEnum::AArrayExpr(array) => array.elements,
        NodeEnum::List(list) => list.items,
        _ => return None,
    };
    let values = elements
        .into_iter()
        .map(|node| {
            let expression = match node.node? {
                NodeEnum::TypeCast(cast) => {
                    if cast.type_name?.names.last().and_then(node_string) != Some("text") {
                        return None;
                    }
                    cast.arg?.node?
                }
                expression => expression,
            };
            let NodeEnum::AConst(value) = expression else {
                return None;
            };
            let Val::Sval(value) = value.val? else {
                return None;
            };
            Some(value.sval)
        })
        .collect::<Option<Vec<_>>>()?;
    Some((field, values))
}

fn guards_length(expression: &str, field: &str, minimum: u32) -> bool {
    use pg_query::{NodeEnum, protobuf::a_const::Val};
    let Some(NodeEnum::AExpr(expression)) = check_expression(expression) else {
        return false;
    };
    if expression.name.first().and_then(node_string) != Some(">=") {
        return false;
    }
    let Some(NodeEnum::FuncCall(length)) = expression.lexpr.and_then(|node| node.node) else {
        return false;
    };
    if length.funcname.last().and_then(node_string) != Some("char_length") {
        return false;
    }
    let [arg] = length.args.as_slice() else {
        return false;
    };
    let Some(NodeEnum::FuncCall(trim)) = &arg.node else {
        return false;
    };
    if trim.funcname.last().and_then(node_string) != Some("btrim") {
        return false;
    }
    let [arg] = trim.args.as_slice() else {
        return false;
    };
    let Some(NodeEnum::ColumnRef(column)) = &arg.node else {
        return false;
    };
    if column.fields.len() != 1 || column.fields.first().and_then(node_string) != Some(field) {
        return false;
    }
    let Some(NodeEnum::AConst(value)) = expression.rexpr.and_then(|node| node.node) else {
        return false;
    };
    match value.val {
        Some(Val::Ival(value)) => u32::try_from(value.ival).ok() == Some(minimum),
        Some(Val::Fval(value)) => value.fval.parse::<u32>().ok() == Some(minimum),
        _ => false,
    }
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
    if crate::key::database_generated(table) && !fields.contains(&"id") {
        fields.push("id");
    }
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

#[cfg(test)]
mod tests {
    use super::{Column, Constraint, Table, model_enum_fields};
    use crate::ManifestErrorType as GenerateErrorType;
    use crate::manifest::ModelDeclaration;
    use wamn_scalar::SqlValueType;

    #[test]
    fn derivation_inherits_fields_from_the_resolved_base() {
        use crate::manifest::{FieldRole, PackageManifest};
        let base: PackageManifest = serde_json::from_value(serde_json::json!({
            "package": {"id": "base", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "base_access", "state": "satisfied"},
            "models": {"record": {
                "schema": "records", "table": "record", "owner": "base",
                "audit_log": {"columns": ["created_at", "created_by"], "retention": "none"},
                "fields": {"created_by": {"role": "actor"}},
                "operations": {"get": {"permission": "record.get", "result": "one"}}
            }},
            "connections": {}, "components": {"data": {"connections": ["postgres"]}}
        }))
        .unwrap();
        let authored = serde_json::to_vec(&serde_json::json!({
            "package": {"id": "overlay", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "overlay_access", "state": "satisfied"},
            "base_dependencies": {"base": {
                "package": "base", "version": "1.0.0", "digest": format!("sha256:{}", "a".repeat(64)),
                "operations": ["record.get"]
            }},
            "models": {"record": {
                "schema": "records", "table": "record", "owner": "base",
                "operations": {"get": {"permission": "record.get", "result": "one"}}
            }},
            "connections": {}, "components": {"data": {"connections": ["postgres"]}}
        })).unwrap();
        let catalog = super::CatalogIr::new(vec![Table::new(
            "records",
            "record",
            vec![
                Column::new("id", SqlValueType::Uuid, false, None, None),
                Column::new("created_at", SqlValueType::Timestamptz, false, None, None),
                Column::new("created_by", SqlValueType::Uuid, false, None, None),
            ],
            vec![Constraint::primary_key("record_id_pkey", ["id"]).unwrap()],
            Vec::new(),
        )]);
        let bytes = super::derive_manifest(&catalog, &authored, &[], &[base]).unwrap();
        let compiled = PackageManifest::from_slice(&bytes).unwrap();
        assert_eq!(
            compiled.models["record"].fields["created_by"].role,
            Some(FieldRole::Actor)
        );
        let generated = super::generate(&super::GenerationInput::new(
            &catalog,
            &bytes,
            &[],
            &super::StatementTransactionality::from_paths(std::collections::BTreeMap::new()),
        ))
        .unwrap();
        let contract = &generated.operations()["record.get"].operation;
        let relations: Vec<wamn_catalog::profile::RelationProfile> =
            contract.statements[0].relations.clone();
        assert_eq!(
            relations[0]
                .reads
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            catalog.tables()[0]
                .columns()
                .iter()
                .map(|column| column.name())
                .collect::<Vec<_>>()
        );
        assert!(relations[0].writes.is_empty());
        assert!(!relations[0].deletes);
        assert_eq!(
            super::derive_manifest(&catalog, &authored, &[], &[])
                .unwrap_err()
                .error_type(),
            GenerateErrorType::InvalidModel
        );
    }

    #[test]
    fn a_quoted_check_of_another_form_refuses_with_its_name() {
        let table = Table::new(
            "application",
            "widget",
            vec![Column::new("status", SqlValueType::Text, false, None, None)],
            vec![
                Constraint::check(
                    "widget_status_check",
                    "((status = 'open'::text) OR (status = 'complete'::text))",
                )
                .expect("construct check"),
            ],
            Vec::new(),
        );
        let model: ModelDeclaration = serde_json::from_value(serde_json::json!({
            "schema": "application",
            "table": "widget",
            "owner": "widgets",
            "operations": {},
        }))
        .expect("model");
        let error = model_enum_fields("widget", &model, &table).expect_err("refused");
        assert_eq!(error.error_type(), GenerateErrorType::InvalidModel);
    }
}
