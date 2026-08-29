use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnType, Constraint, ConstraintKind, Table,
};

use crate::manifest::{
    AuthoredSqlDeclaration, CrudAction, CursorDirection, ModelDeclaration, OperationDeclaration,
    PackageManifest, PolicyContractRequirement, PolicyContractState, ResultClass, SortDeclaration,
};
use crate::sql;
use crate::sql_lex::contains_schema_qualified_reference;
use crate::{GenerateError, GenerateErrorKind};

const POSTGRES_INTERFACE: &str = "wamn:postgres@0.1.0";
const QUERY_LIMIT: u32 = 100;
const CURSOR_VERSION: u8 = 1;

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

    let weld = PackageWeld {
        verified_schema_state_id: sha256(&input.catalog.canonical_json_bytes()).into(),
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
    insert_json(&mut files, "generated/package-weld.json", &weld)?;

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
    validate_identifier(&manifest.package.id, "package id")?;
    if manifest.package.version.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
            "package version must not be empty",
        ));
    }
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

    let mut operation_ids = BTreeSet::new();
    for (model_name, model) in &manifest.models {
        validate_model(input.catalog, model_name, model)?;
        for action in model.operations.keys() {
            operation_ids.insert(format!("{model_name}.{}", action.as_str()));
        }
    }
    validate_connections(manifest)?;
    validate_components(manifest, &operation_ids)?;
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
    if operation.permission != context {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} permission must equal its package-local operation identity"),
        ));
    }
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
    Ok(())
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

fn validate_components(
    manifest: &PackageManifest,
    operation_ids: &BTreeSet<String>,
) -> Result<(), GenerateError> {
    if manifest.components.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidComponent,
            "manifest declares no component grouping",
        ));
    }
    let mut grouped = BTreeSet::<String>::new();
    for (name, component) in &manifest.components {
        validate_identifier(name, "component")?;
        if component.operations.is_empty() || component.connections.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidComponent,
                format!("{name} must declare operations and connections"),
            ));
        }
        for operation in &component.operations {
            if !operation_ids.contains(operation) || !grouped.insert(operation.clone()) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidComponent,
                    format!("{name} references unknown or repeated operation {operation}"),
                ));
            }
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
    if &grouped != operation_ids {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidComponent,
            "every operation must be grouped exactly once",
        ));
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
    manifest
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
        .collect()
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

fn emit_cursor_contract(files: &mut BTreeMap<String, Vec<u8>>) -> Result<(), GenerateError> {
    insert_json(
        files,
        "generated/contracts/cursor-v1.json",
        &json!({
            "version": CURSOR_VERSION,
            "payload": "canonical_compact_json",
            "member_order": ["v", "field", "direction", "key", "id"],
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
    let operation_id = format!(
        "{}@{}::{model_name}.{}",
        manifest.package.id,
        manifest.package.version,
        action.as_str()
    );
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
        CrudAction::Update | CrudAction::Delete => merge_json(
            common,
            &json!({
                "id": {"type": "uuid", "required": true},
                "expected_revision": {
                    "field": operation.revision_field,
                    "type": "int64",
                    "required": true,
                },
            }),
        ),
    }
}

fn error_contract(table: &Table, action: CrudAction, operation: &OperationDeclaration) -> Value {
    let mut literals = vec![
        json!({"literal": "invalid_input"}),
        json!({"literal": "permission_denied", "from": "permission_denied"}),
        json!({
            "literal": "retry",
            "from": ["serialization_failure", "connection_unavailable"],
            "automatic": false,
        }),
        json!({"literal": "timeout", "from": "statement_timeout"}),
        json!({
            "literal": "internal_error",
            "from": ["query_error", "row_limit_exceeded"],
            "detail": "opaque",
        }),
    ];
    if matches!(
        action,
        CrudAction::Get | CrudAction::Update | CrudAction::Delete
    ) {
        literals.push(json!({"literal": "not_found"}));
    }
    if matches!(action, CrudAction::Update | CrudAction::Delete) {
        literals.push(json!({"literal": "concurrency_conflict"}));
    }
    for constraint in operation_constraints(table, action, operation) {
        literals.push(json!({
            "literal": constraint_error(constraint.kind()),
            "from": constraint_error(constraint.kind()),
            "constraint": constraint.name(),
        }));
    }
    json!({"closed": true, "cases": literals})
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
                let result_row = operation_result_row(model_name, table, *action, Projection::Wamn);
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
                let result_row = operation_result_row(model_name, table, *action, Projection::Wamn);
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
        .keys()
        .copied()
        .filter(|action| matches!(action, CrudAction::Update | CrudAction::Delete))
        .map(|action| operation_result_row(model_name, table, action, projection))
        .collect()
}

fn operation_result_row(
    model_name: &str,
    table: &Table,
    action: CrudAction,
    projection: Projection,
) -> RustRow {
    let mut fields = vec![RustMember {
        name: "outcome".to_owned(),
        rust_type: "Option<String>".to_owned(),
    }];
    if action == CrudAction::Update {
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
    let tables = manifest
        .models
        .values()
        .map(|model| {
            let table = relation(catalog, model).expect("validation resolved relation");
            RequiredTable {
                schema: model.schema.clone().into_boxed_str(),
                table: model.table.clone().into_boxed_str(),
                fields: table
                    .columns()
                    .iter()
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
                    .map(|constraint| RequiredConstraint {
                        name: constraint.name().into(),
                        definition: serde_json::to_value(constraint.kind())
                            .expect("constraint IR always serializes"),
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            }
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

fn validate_identifier(value: &str, object: &str) -> Result<(), GenerateError> {
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
