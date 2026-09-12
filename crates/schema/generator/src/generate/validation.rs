//! Manifest, claim, query, and authored SQL validation.

use super::{
    AccessOperationErrorLiteral, AuthoredSql, AuthoredSqlDeclaration, BTreeMap, BTreeSet,
    CLAIM_COMMAND_COLUMN, CLAIM_KEY_COLUMN, CURSOR_VERSION, CatalogIr, Column, ColumnDefault,
    ColumnType, ConstraintKind, CrudAction, CursorDirection, CustomOperationDeclaration,
    GenerateError, GenerateErrorKind, GenerationInput, ModelDeclaration, OperationDeclaration,
    POSTGRES_INTERFACE, PackageManifest, QUERY_LIMIT, ResultClass, SortDeclaration, StaticSqlFetch,
    Table, column, constraint_error_code, contains_schema_qualified_reference,
    custom_operation_constraint_origin, operation_constraints, operation_exclusions, relation,
    rust_identifier, sql, validate_identifier, validate_operation_vocabulary,
};

pub(super) fn validate(
    input: &GenerationInput<'_>,
    manifest: &PackageManifest,
) -> Result<(), GenerateError> {
    validate_operation_vocabulary(manifest)?;
    validate_identifier(
        &manifest.required_platform_policy_contract.id,
        "platform policy contract",
    )?;
    for value in [input.provenance.generator, input.provenance.toolchain] {
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
        validate_model(input.catalog, manifest, model_name, model)?;
    }
    for (relation_name, relation) in &manifest.internal_relations {
        if !input
            .catalog
            .tables()
            .iter()
            .any(|table| table.schema() == relation.schema && table.name() == relation.table)
        {
            return Err(GenerateError::for_object(
                GenerateErrorKind::UnknownRelation,
                format!(
                    "CDC-excluded relation {relation_name} references unknown {}.{}",
                    relation.schema, relation.table
                ),
                format!("{}.{}", relation.schema, relation.table),
            ));
        }
    }
    validate_connections(manifest)?;
    validate_authored_sources(manifest, input.authored_sql)?;
    for (operation_name, operation) in &manifest.custom_operations {
        validate_custom_operation_sql(
            input.catalog,
            input.authored_sql,
            operation_name,
            operation,
        )?;
        validate_custom_claim(input.catalog, manifest, operation_name, operation)?;
    }
    Ok(())
}

fn validate_model(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
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
    if let Some(column) = table
        .columns()
        .iter()
        .find(|column| rust_identifier(column.name()).is_none())
    {
        return Err(GenerateError::for_object(
            GenerateErrorKind::InvalidIdentity,
            "model column has no lossless Rust 2024 identifier spelling",
            format!("{}.{}.{}", model.schema, model.table, column.name()),
        ));
    }
    let admitted_owners = std::iter::once(manifest.package.id.as_str())
        .chain(
            manifest
                .base_dependencies
                .values()
                .map(|dependency| dependency.package.as_str()),
        )
        .collect::<BTreeSet<_>>();
    validate_definition_owner(model_name, "relation", &model.owner, &admitted_owners)?;
    if model.client_field_extensible && model.owner != manifest.package.id {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidModel,
            format!(
                "{model_name} may declare client field extensibility only for its own relation"
            ),
        ));
    }
    for (field, owner) in &model.field_owners {
        validate_field(table, model_name, field)?;
        validate_definition_owner(model_name, field, owner, &admitted_owners)?;
    }
    for (constraint, owner) in &model.constraint_owners {
        if !table
            .constraints()
            .iter()
            .any(|candidate| candidate.name() == constraint)
            && !table
                .exclusions()
                .iter()
                .any(|candidate| candidate.name() == constraint)
        {
            return Err(GenerateError::for_object(
                GenerateErrorKind::InvalidModel,
                format!("{model_name} owns unknown constraint {constraint}"),
                format!("{}.{}.{}", model.schema, model.table, constraint),
            ));
        }
        validate_definition_owner(model_name, constraint, owner, &admitted_owners)?;
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
        validate_operation(
            catalog, manifest, model_name, model, table, *action, operation,
        )?;
    }
    Ok(())
}

fn validate_definition_owner(
    model: &str,
    definition: &str,
    owner: &str,
    admitted: &BTreeSet<&str>,
) -> Result<(), GenerateError> {
    validate_identifier(owner, "definition owner")?;
    if admitted.contains(owner) {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidModel,
            format!("{model}.{definition} owner {owner} is not the package or a declared base"),
        ))
    }
}

fn validate_operation(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Result<(), GenerateError> {
    let context = format!("{model_name}.{}", action.as_str());
    validate_field(table, model_name, "id")?;
    if operation.claim.is_some() && action != CrudAction::Create {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} declares a command claim, which only a create carries"),
        ));
    }

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
            validate_claim(catalog, manifest, &context, model, table, operation)?;
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

    let mut expected = operation_constraints(table, action, operation)
        .into_iter()
        .map(|constraint| constraint_error_code(constraint.kind()))
        .collect::<BTreeSet<_>>();
    if !operation_exclusions(table, action, operation).is_empty() {
        expected.insert(Code::ExclusionViolation);
    }
    let declared = operation
        .error_details
        .keys()
        .copied()
        .filter(|code| {
            matches!(
                code,
                Code::UniqueViolation
                    | Code::ForeignKeyViolation
                    | Code::CheckViolation
                    | Code::ExclusionViolation
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

fn validate_claim(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    context: &str,
    model: &ModelDeclaration,
    table: &Table,
    operation: &OperationDeclaration,
) -> Result<(), GenerateError> {
    resolve_claim(catalog, manifest, context, model, table, operation).map(|_| ())
}

/// The identities one generated create would otherwise let PostgreSQL mint.
///
/// Exactly these must come from the claim. A column PostgreSQL defaults is
/// minted fresh on every attempt, so a replay that re-ran the insert would hand
/// out a SECOND identity for the same command.
fn minted_identities<'a>(table: &'a Table, operation: &OperationDeclaration) -> Vec<&'a Column> {
    table
        .columns()
        .iter()
        .filter(|column| {
            column.column_type() == ColumnType::Uuid
                && !column.nullable()
                && column.default() == Some(&ColumnDefault::GenRandomUuid)
                && column.generation().is_none()
                && !operation
                    .writable_fields
                    .iter()
                    .any(|field| field == column.name())
        })
        .collect()
}

/// Resolve and verify one create's claim against the catalog.
///
/// The claim is checked structurally, never by convention: the key column under
/// a primary key, and every minted identity under its own `UNIQUE` column that
/// PostgreSQL defaulted once. That is why a replay returns the same value BY
/// CONSTRUCTION rather than because some caller took an early return.
pub(super) fn resolve_claim<'a>(
    catalog: &'a CatalogIr,
    manifest: &PackageManifest,
    context: &str,
    model: &ModelDeclaration,
    table: &'a Table,
    operation: &'a OperationDeclaration,
) -> Result<sql::Claim<'a>, GenerateError> {
    let declaration = operation.claim.as_ref().ok_or_else(|| {
        GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} must declare the command claim its identity comes from"),
        )
    })?;
    let (claim, primary_key) = require_claim_relation(
        catalog,
        manifest,
        context,
        &model.schema,
        &declaration.table,
    )?;
    // The emitted claim INSERT writes the key and the canonical command and
    // nothing else, so every other column must have a value without one.
    for column in claim.columns() {
        if column.name() != CLAIM_KEY_COLUMN
            && column.name() != CLAIM_COMMAND_COLUMN
            && !column.nullable()
            && column.default().is_none()
            && column.generation().is_none()
        {
            return Err(GenerateError::for_object(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{context} claim column {} has no value the claim can supply",
                    column.name()
                ),
                format!("{}.{}.{}", model.schema, declaration.table, column.name()),
            ));
        }
    }

    let minted = minted_identities(table, operation)
        .into_iter()
        .map(Column::name)
        .collect::<BTreeSet<_>>();
    let declared = declaration
        .identities
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if minted != declared || !declared.contains("id") {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{context} claim must pre-generate exactly the identities the create mints"),
        ));
    }
    let mut claim_columns = BTreeSet::new();
    let mut identities = Vec::with_capacity(declaration.identities.len());
    for (field, claim_column) in &declaration.identities {
        if !claim_columns.insert(claim_column.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{context} claim reuses column {claim_column} for two identities"),
            ));
        }
        require_pre_generated_identity(context, claim, claim_column)?;
        identities.push((field.as_str(), claim_column.as_str()));
    }
    Ok(sql::Claim {
        table: claim,
        primary_key,
        identities,
    })
}

/// Find one claim relation and check the shape the law needs from it.
///
/// A generated create and an authored command share this check, because the law
/// is one law. The relation is CDC-excluded, it keys the idempotency key under a
/// primary key alone, and it stores the canonical command beside that key. The
/// primary key is what makes a second call with the same key mint nothing.
fn require_claim_relation<'a>(
    catalog: &'a CatalogIr,
    manifest: &PackageManifest,
    context: &str,
    schema: &str,
    table: &str,
) -> Result<(&'a Table, &'a str), GenerateError> {
    validate_identifier(table, "claim table")?;
    let claim = catalog
        .tables()
        .iter()
        .find(|candidate| candidate.schema() == schema && candidate.name() == table)
        .ok_or_else(|| {
            GenerateError::for_object(
                GenerateErrorKind::UnknownRelation,
                format!("{context} references unknown claim relation"),
                format!("{schema}.{table}"),
            )
        })?;
    if !manifest
        .internal_relations
        .values()
        .any(|relation| relation.schema == schema && relation.table == table)
    {
        return Err(GenerateError::for_object(
            GenerateErrorKind::InvalidOperation,
            format!("{context} claim must be a CDC-excluded internal relation"),
            format!("{schema}.{table}"),
        ));
    }
    let primary_key = claim_primary_key(claim).ok_or_else(|| {
        GenerateError::for_object(
            GenerateErrorKind::InvalidOperation,
            format!("{context} claim must key {CLAIM_KEY_COLUMN} under a primary key alone"),
            format!("{schema}.{table}"),
        )
    })?;
    require_claim_column(context, claim, CLAIM_KEY_COLUMN, ColumnType::Text)?;
    require_claim_column(context, claim, CLAIM_COMMAND_COLUMN, ColumnType::Bytes)?;
    Ok((claim, primary_key))
}

/// Refuse a claim column PostgreSQL can mint a second time.
///
/// The column defaults `gen_random_uuid()` once, under its own `UNIQUE`
/// constraint, so the claim row holds one value for the life of the key. That
/// is why a replay returns the same identity BY CONSTRUCTION rather than
/// because some caller took an early return.
fn require_pre_generated_identity(
    context: &str,
    claim: &Table,
    claim_column: &str,
) -> Result<(), GenerateError> {
    let object = format!("{}.{}.{claim_column}", claim.schema(), claim.name());
    let column = column(claim, claim_column).ok_or_else(|| {
        GenerateError::for_object(
            GenerateErrorKind::UnknownColumn,
            format!("{context} claim has no column {claim_column}"),
            object.clone(),
        )
    })?;
    let pre_generated = column.column_type() == ColumnType::Uuid
        && !column.nullable()
        && column.default() == Some(&ColumnDefault::GenRandomUuid)
        && claim.constraints().iter().any(|constraint| {
            matches!(
                constraint.kind(),
                ConstraintKind::Unique { columns }
                    if columns.len() == 1 && columns[0].as_ref() == claim_column
            )
        });
    if pre_generated {
        Ok(())
    } else {
        Err(GenerateError::for_object(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{context} claim column {claim_column} must be a unique non-null uuid defaulting to gen_random_uuid()"
            ),
            object,
        ))
    }
}

fn claim_primary_key(claim: &Table) -> Option<&str> {
    claim.constraints().iter().find_map(|constraint| {
        matches!(
            constraint.kind(),
            ConstraintKind::PrimaryKey { columns }
                if columns.len() == 1 && columns[0].as_ref() == CLAIM_KEY_COLUMN
        )
        .then(|| constraint.name())
    })
}

fn require_claim_column(
    context: &str,
    claim: &Table,
    name: &str,
    ty: ColumnType,
) -> Result<(), GenerateError> {
    let valid =
        column(claim, name).is_some_and(|column| column.column_type() == ty && !column.nullable());
    if valid {
        Ok(())
    } else {
        Err(GenerateError::for_object(
            GenerateErrorKind::InvalidOperation,
            format!("{context} claim must carry non-null {name} {}", ty.as_str()),
            format!("{}.{}.{name}", claim.schema(), claim.name()),
        ))
    }
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

/// Check one authored command's claim against the catalog it runs on.
///
/// The law is the same law a generated create carries. The relation shape and
/// the pre-generated identity columns go through the same helpers. What differs
/// is where the ids are named. A generated create mints them in the model
/// table, and an authored command hands them out in its own result. The
/// identity map is therefore read against the result fields and against the row
/// the claim statement returns.
///
/// The three statements are named because the emitted contract tests name them.
/// The claim statement returns exactly the pre-generated columns, and the
/// replay statement returns the canonical command beside every one of them.
/// A command missing either cannot return the immutable original on a replay.
fn validate_custom_claim(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    let Some(declaration) = &operation.claim else {
        return Ok(());
    };
    // The claim relation is one the operation already declares, so the schema
    // comes from that declaration and the command holds the access it needs.
    let relation = operation
        .relations
        .iter()
        .find(|relation| relation.table == declaration.table)
        .ok_or_else(|| {
            GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} claim {} must be one of the operation's declared relations",
                    declaration.table
                ),
            )
        })?;
    let (claim, _) = require_claim_relation(
        catalog,
        manifest,
        operation_name,
        &relation.schema,
        &declaration.table,
    )?;
    let result = operation.result.as_ref().ok_or_else(|| {
        GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} claim needs a declared result to hand identities out in"),
        )
    })?;
    if declaration.identities.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} claim must pre-generate at least one identity"),
        ));
    }
    let mut claim_columns = BTreeSet::new();
    for (field, claim_column) in &declaration.identities {
        if !claim_columns.insert(claim_column.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{operation_name} claim reuses column {claim_column} for two identities"),
            ));
        }
        if !result
            .fields
            .iter()
            .any(|candidate| candidate.path == *field)
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{operation_name} claim identity {field} is not a result field"),
            ));
        }
        require_pre_generated_identity(operation_name, claim, claim_column)?;
    }
    let minted = claim_statement_row(operation_name, operation, &declaration.claim, "claim")?;
    if minted != claim_columns {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} statement {} must return exactly the pre-generated identities",
                declaration.claim
            ),
        ));
    }
    let replayed = claim_statement_row(operation_name, operation, &declaration.replay, "replay")?;
    if !replayed.contains(CLAIM_COMMAND_COLUMN) || !claim_columns.is_subset(&replayed) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} statement {} must return {CLAIM_COMMAND_COLUMN} and every pre-generated identity",
                declaration.replay
            ),
        ));
    }
    claim_statement_row(operation_name, operation, &declaration.finalize, "finalize")?;
    if operation.statements[&declaration.finalize].fetch != StaticSqlFetch::One {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} claim finalization must return exactly one row; set {} fetch to one",
                declaration.finalize
            ),
        ));
    }
    Ok(())
}

/// The row one named claim statement returns, refusing an undeclared name.
fn claim_statement_row<'a>(
    operation_name: &str,
    operation: &'a CustomOperationDeclaration,
    statement: &str,
    role: &str,
) -> Result<BTreeSet<&'a str>, GenerateError> {
    let declaration = operation.statements.get(statement).ok_or_else(|| {
        GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} claim names unknown {role} statement {statement}"),
        )
    })?;
    Ok(declaration
        .row
        .iter()
        .map(|value| value.name.as_str())
        .collect())
}

fn validate_custom_operation_sql(
    catalog: &CatalogIr,
    authored_sql: &[AuthoredSql<'_>],
    operation: &str,
    declaration: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    for relation in &declaration.relations {
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
        for fields in [
            &relation.select_fields,
            &relation.insert_fields,
            &relation.update_fields,
        ] {
            validate_static_sql_relation_fields(operation, relation, table, fields)?;
        }
        for name in &relation.constraints {
            if !table
                .constraints()
                .iter()
                .any(|constraint| constraint.name() == name)
            {
                return Err(GenerateError::for_object(
                    GenerateErrorKind::InvalidOperation,
                    format!("{operation} requires named constraint {name}"),
                    format!("{}.{}", relation.schema, relation.table),
                ));
            }
        }
    }
    validate_constraint_error_mappings(catalog, operation, declaration)?;
    validate_static_sql_relation_access(catalog, authored_sql, operation, declaration)
}

fn validate_static_sql_relation_fields(
    operation: &str,
    relation: &crate::manifest::StaticSqlRelationDeclaration,
    table: &Table,
    fields: &[String],
) -> Result<(), GenerateError> {
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

fn validate_constraint_error_mappings(
    catalog: &CatalogIr,
    operation: &str,
    declaration: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    for (name, _) in &declaration.constraint_errors {
        custom_operation_constraint_origin(catalog, declaration, name).ok_or_else(|| {
            GenerateError::for_object(
                GenerateErrorKind::InvalidOperation,
                format!("{operation} maps undeclared constraint {name}"),
                name.clone(),
            )
        })?;
    }
    Ok(())
}

fn validate_static_sql_relation_access(
    catalog: &CatalogIr,
    authored_sql: &[AuthoredSql<'_>],
    operation: &str,
    declaration: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    let schemas = declaration
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
    for statement in declaration.statements.values() {
        let source = authored_sql
            .iter()
            .find(|source| source.path == statement.path)
            .expect("authored-source validation supplied every custom-operation statement");
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
    for relation in &declaration.relations {
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
                    "{operation} {}.{} privilege declaration does not match verified SQL reads, writes, and row locks.\n\
                     Verified SQL: {}\n\
                     Declared: {}\n\
                     RETURNING columns require select_fields. Row-lock clauses such as FOR UPDATE require lock=true.",
                    relation.schema,
                    relation.table,
                    serde_json::json!({
                        "select_fields": observed.select_fields,
                        "insert_fields": observed.insert_fields,
                        "update_fields": observed.update_fields,
                        "lock": observed.lock,
                    }),
                    serde_json::json!({
                        "select_fields": declared.select_fields,
                        "insert_fields": declared.insert_fields,
                        "update_fields": declared.update_fields,
                        "lock": declared.lock,
                    }),
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
        .chain(manifest.custom_operations.values().flat_map(|operation| {
            operation
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
            .custom_operations
            .values()
            .flat_map(|operation| operation.statements.values())
            .map(|statement| statement.path.as_str()),
    );
    paths
}

pub(super) fn authored_sql_map(
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
