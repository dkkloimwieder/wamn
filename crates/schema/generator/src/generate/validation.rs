//! Manifest, query, and authored SQL validation.

use super::{
    AuthoredSql, AuthoredSqlDeclaration, BTreeMap, BTreeSet, CatalogIr, Column, ColumnType,
    Constraint, ConstraintType, CrudAction, CursorDirection, CustomOperationDeclaration,
    DeleteMode, GenerateError, GenerateErrorType, GenerationInput, ModelDeclaration,
    OperationDeclaration, PackageManifest, QUERY_LIMIT, RecordHistoryColumn, ResultClass,
    SortDeclaration, Table, TombstoneColumn, column, contains_schema_qualified_reference,
    custom_operation_constraint_origin, logged_history_tables, relation, rust_identifier,
    server_owned_fields, validate_identifier, validate_operation_vocabulary,
};
use crate::FilterMatch;
use wamn_record_history::HISTORY_COLUMNS;

/// Refuse a client package name that a package manifest cannot carry.
///
/// The rule is npm's own: at most 214 characters, lowercase, one optional
/// `@scope/` prefix, and each segment starting with a letter or a digit.
fn validate_client_package_name(package: &str, name: &str) -> Result<(), GenerateError> {
    let refuse = |reason: &str| {
        Err(GenerateError::new(
            GenerateErrorType::InvalidClientPackage,
            format!("{package} declares client package name {name:?}: {reason}"),
        ))
    };
    if name.is_empty() {
        return refuse("the name must not be empty");
    }
    if name.len() > 214 {
        return refuse("the name must be 214 characters or fewer");
    }
    let segments = match name.strip_prefix('@') {
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, package)) => vec![scope, package],
            None => return refuse("a scoped name needs a scope and a package, as @scope/name"),
        },
        None => vec![name],
    };
    for segment in segments {
        if !segment
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        {
            return refuse("each part must start with a lowercase letter or a digit");
        }
        if !segment.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.' | b'_')
        }) {
            return refuse("each part admits only lowercase letters, digits, and - . _");
        }
    }
    Ok(())
}

/// Refuse screen text on the envelope bound, which has no screen.
///
/// The `line` bound owns the repeated group a form renders. The envelope is
/// the submission itself, and a label on it would be a member nothing reads.
fn validate_count_text(
    operation_name: &str,
    operation: &crate::CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    let Some(envelope) = &operation.input.envelope else {
        return Ok(());
    };
    if envelope.label.is_some() || envelope.description.is_some() {
        return Err(GenerateError::new(
            GenerateErrorType::InvalidOperation,
            format!(
                "{operation_name} states screen text on its envelope bound, which has no screen. \
                 The line bound carries the repeated group's text."
            ),
        ));
    }
    Ok(())
}

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
                GenerateErrorType::InvalidIdentity,
                "generation provenance values must not be empty",
            ));
        }
    }
    if manifest.models.is_empty() && !manifest.declares_no_sql() {
        return Err(GenerateError::new(
            GenerateErrorType::InvalidManifest,
            "manifest must declare at least one model",
        ));
    }
    if let Some(client) = &manifest.client_package {
        validate_client_package_name(&manifest.package.id, &client.name)?;
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
                GenerateErrorType::UnknownRelation,
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
    validate_authored_query_filters(manifest, input.authored_sql)?;
    for (operation_name, operation) in &manifest.custom_operations {
        validate_custom_operation_sql(
            input.catalog,
            manifest,
            input.authored_sql,
            operation_name,
            operation,
        )?;
        validate_count_text(operation_name, operation)?;
        validate_lists(manifest, operation_name, operation)?;
    }
    Ok(())
}

/// Refuse a read that a table shows when it states no row key, and a `lists`
/// whose key or model the package does not declare.
///
/// A table names each row by its key: a load refuses a key it reads twice, and
/// an edit or a child table finds its row by it. A row position is no key,
/// because the same row moves when the list changes.
fn validate_lists(
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &crate::CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    let refuse = |message: String| {
        Err(GenerateError::new(
            GenerateErrorType::InvalidOperation,
            message,
        ))
    };
    let table = operation.type_ == crate::CustomOperationType::Projection
        && operation.result.as_ref().is_some_and(|result| {
            matches!(result.class, ResultClass::BoundedList | ResultClass::Page)
        });
    let Some(lists) = &operation.lists else {
        if table {
            return refuse(format!(
                "{operation_name} answers a list of rows and states no `lists` key. \
                 State the result fields that name one row."
            ));
        }
        return Ok(());
    };
    if let Some(model) = &lists.model
        && !manifest.models.contains_key(model)
    {
        return refuse(format!(
            "{operation_name} lists records of {model}, which the package does not declare"
        ));
    }
    let fields = operation
        .result
        .as_ref()
        .map_or(&[][..], |result| &result.fields[..]);
    if lists.key_field.is_empty() {
        return refuse(format!(
            "{operation_name} states a `lists` key with no field"
        ));
    }
    for key in &lists.key_field {
        if !fields.iter().any(|field| &field.path == key) {
            return refuse(format!(
                "{operation_name} keys its rows on {key}, which is not a field of its result"
            ));
        }
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
            GenerateErrorType::UnknownRelation,
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
            GenerateErrorType::InvalidIdentity,
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
            GenerateErrorType::InvalidModel,
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
                GenerateErrorType::InvalidModel,
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
                GenerateErrorType::InvalidModel,
                format!("{model_name} repeats server-owned field {field}"),
            ));
        }
    }
    validate_audit_log_columns(manifest, model_name, model, table)?;
    validate_tombstone_columns(manifest, model_name, model, table)?;
    for (field, values) in &model.enum_fields {
        let column = validate_field(table, model_name, field)?;
        if column.column_type() != ColumnType::Text || values.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidModel,
                format!("{model_name}.{field} enum must be nonempty text"),
            ));
        }
        let unique = values.iter().collect::<BTreeSet<_>>();
        if unique.len() != values.len() || values.iter().any(String::is_empty) {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidModel,
                format!("{model_name}.{field} enum values must be unique and nonempty"),
            ));
        }
    }
    for (field, minimum) in &model.min_lengths {
        let column = validate_field(table, model_name, field)?;
        if column.column_type() != ColumnType::Text || *minimum == 0 {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidModel,
                format!("{model_name}.{field} minimum length must be at least 1 on a text column"),
            ));
        }
        // The author writes the guard in the migration; generation writes
        // nothing outside `generated/`.
        let expression = super::min_length_check(field, *minimum);
        let guarded = table.constraints().iter().any(|constraint| {
            matches!(constraint.constraint_type(), ConstraintType::Check { expression: found } if **found == *expression)
        });
        if !guarded {
            return Err(GenerateError::for_object(
                GenerateErrorType::InvalidModel,
                format!(
                    "{model_name}.{field} declares minimum length {minimum}; the table needs CHECK {expression}"
                ),
                format!("{}.{}.{field}", model.schema, model.table),
            ));
        }
    }

    // A get reads its revision from the model, so the model has one revision
    // column, whichever operations name it.
    let revisions = model
        .operations
        .values()
        .filter_map(|operation| operation.revision_field.as_deref())
        .collect::<BTreeSet<_>>();
    if revisions.len() > 1 {
        return Err(GenerateError::new(
            GenerateErrorType::InvalidModel,
            format!("{model_name} operations name more than one revision field"),
        ));
    }
    for (action, operation) in &model.operations {
        validate_operation(model_name, model, table, *action, operation)?;
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
            GenerateErrorType::InvalidModel,
            format!("{model}.{definition} owner {owner} is not the package or a declared base"),
        ))
    }
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

    let server_owned = server_owned_fields(model, table);
    let mut writable = BTreeSet::new();
    for field in &operation.writable_fields {
        let column = validate_field(table, model_name, field)?;
        if !writable.insert(field) {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!("{context} repeats writable field {field}"),
            ));
        }
        if server_owned.contains(&field.as_str()) || column.generation().is_some() {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!("{context} exposes server-owned field {field}"),
            ));
        }
    }
    // A create narrows a writable enum field to some of the values the model
    // declares, and never to none, so a form always offers a choice.
    for (field, values) in &operation.values {
        let declared = model.enum_fields.get(field);
        if action != CrudAction::Create
            || !writable.contains(field)
            || values.is_empty()
            || values.iter().collect::<BTreeSet<_>>().len() != values.len()
            || !declared.is_some_and(|declared| values.iter().all(|value| declared.contains(value)))
        {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!(
                    "{context} values for {field} must be a create's writable enum field, narrowed to distinct values the model declares"
                ),
            ));
        }
    }

    if let Some(revision_field) = &operation.revision_field {
        let column = validate_field(table, model_name, revision_field)?;
        // An application integer is int32 by default, and int64 is opt-in, so a
        // revision column carries either width.
        if !matches!(column.column_type(), ColumnType::Int32 | ColumnType::Int64)
            || column.nullable()
        {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!("{context} revision field must be a non-null int32 or int64"),
            ));
        }
        if writable.contains(revision_field) {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
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
            if !operation.writable_fields.is_empty() {
                return Err(GenerateError::new(
                    GenerateErrorType::InvalidOperation,
                    format!("{context} query cannot declare mutation fields"),
                ));
            }
            validate_query(model, model_name, table, operation)?;
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
                    GenerateErrorType::InvalidOperation,
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
            GenerateErrorType::InvalidOperation,
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
        || (!allow_query_fields
            && (!operation.filters.is_empty()
                || operation.search.is_some()
                || operation.sort.is_some()
                || operation.pagination.is_some()
                || operation.limit.is_some()))
        || operation.authored_sql.is_some()
    {
        return Err(GenerateError::new(
            GenerateErrorType::InvalidOperation,
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
        || operation.search.is_some()
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
            GenerateErrorType::InvalidOperation,
            format!("{context} carries fields outside its mutation shape"),
        ));
    }
    Ok(())
}

fn require_column(
    type_: GenerateErrorType,
    context: &str,
    table: &Table,
    name: &str,
    ty: ColumnType,
) -> Result<(), GenerateError> {
    let valid =
        column(table, name).is_some_and(|column| column.column_type() == ty && !column.nullable());
    if valid {
        Ok(())
    } else {
        Err(GenerateError::for_object(
            type_,
            format!("{context} must carry non-null {name} {}", ty.as_str()),
            format!("{}.{}.{name}", table.schema(), table.name()),
        ))
    }
}

/// Refuse a record-history declaration that the relation cannot carry.
///
/// Every selected column exists as a non-null `timestamptz` time or `uuid`
/// actor. Every reserved-name column is selected, except a base column under
/// an overlay, which the relation owner's declaration selects. A relation that
/// keeps a log has a primary key, because each entry keys the row by it.
fn validate_audit_log_columns(
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
) -> Result<(), GenerateError> {
    let context = format!("{model_name} audit_log");
    let selected = model
        .audit_log
        .as_ref()
        .map_or(&[][..], |audit_log| audit_log.columns.as_slice());
    for selection in selected {
        let ty = match selection {
            RecordHistoryColumn::CreatedAt | RecordHistoryColumn::UpdatedAt => {
                ColumnType::Timestamptz
            }
            RecordHistoryColumn::CreatedBy | RecordHistoryColumn::UpdatedBy => ColumnType::Uuid,
        };
        require_column(
            GenerateErrorType::InvalidModel,
            &context,
            table,
            selection.as_str(),
            ty,
        )?;
    }
    for reserved in RecordHistoryColumn::ALL {
        let name = reserved.as_str();
        let base_column_under_overlay =
            model.owner != manifest.package.id && model.field_owner(name) != manifest.package.id;
        if column(table, name).is_some()
            && !selected.contains(&reserved)
            && !base_column_under_overlay
        {
            return Err(GenerateError::for_object(
                GenerateErrorType::InvalidModel,
                format!("{context} must select reserved column {name}"),
                format!("{}.{}.{name}", table.schema(), table.name()),
            ));
        }
    }
    if model.log_retention().is_some()
        && !table.constraints().iter().any(|constraint| {
            matches!(
                constraint.constraint_type(),
                ConstraintType::PrimaryKey { .. }
            )
        })
    {
        return Err(GenerateError::for_object(
            GenerateErrorType::InvalidModel,
            format!("{context} keeps a log, so its relation must have a primary key"),
            format!("{}.{}", table.schema(), table.name()),
        ));
    }
    Ok(())
}

/// Refuse a relation whose tombstone columns disagree with its delete mode.
///
/// A tombstone model carries both reserved columns, and every other model
/// carries neither. An overlay does not own a base column, so the base
/// declaration answers for it.
fn validate_tombstone_columns(
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
) -> Result<(), GenerateError> {
    let context = format!("{model_name} delete_mode");
    if model.delete_mode == Some(DeleteMode::Tombstone) {
        for reserved in TombstoneColumn::ALL {
            let ty = match reserved {
                TombstoneColumn::DeletedAt => ColumnType::Timestamptz,
                TombstoneColumn::DeletedBy => ColumnType::Uuid,
            };
            let name = reserved.as_str();
            // A marker is NULL on a live row, so it is the one reserved pair
            // that must be nullable. A record-history stamp always has a value
            // and is refused when it is nullable.
            let valid = column(table, name)
                .is_some_and(|column| column.column_type() == ty && column.nullable());
            if !valid {
                return Err(GenerateError::for_object(
                    GenerateErrorType::InvalidModel,
                    format!("{context} must carry nullable {name} {}", ty.as_str()),
                    format!("{}.{}.{name}", table.schema(), table.name()),
                ));
            }
        }
        return Ok(());
    }
    for reserved in TombstoneColumn::ALL {
        let name = reserved.as_str();
        let base_column_under_overlay =
            model.owner != manifest.package.id && model.field_owner(name) != manifest.package.id;
        if column(table, name).is_some() && !base_column_under_overlay {
            return Err(GenerateError::for_object(
                GenerateErrorType::InvalidModel,
                format!("{model_name} carries reserved column {name} without a tombstone delete"),
                format!("{}.{}.{name}", table.schema(), table.name()),
            ));
        }
    }
    Ok(())
}

fn validate_query(
    model: &ModelDeclaration,
    model_name: &str,
    table: &Table,
    operation: &OperationDeclaration,
) -> Result<(), GenerateError> {
    let context = format!("{model_name}.query");
    let mut filter_fields = BTreeSet::new();
    for filter in &operation.filters {
        validate_field(table, model_name, &filter.field)?;
        let column = table
            .columns()
            .iter()
            .find(|column| column.name() == filter.field)
            .expect("the filter field was validated");
        // A value domain is matched whole, and only text holds a part. A range
        // orders numbers and times, and only a nullable column is ever empty.
        let takes = match filter.match_mode {
            FilterMatch::Exact => None,
            FilterMatch::Contains | FilterMatch::Prefix => Some((
                column.column_type() == ColumnType::Text
                    && !model.enum_fields.contains_key(&filter.field),
                "only a text field with no declared values takes",
            )),
            FilterMatch::Range => Some((
                matches!(
                    column.column_type(),
                    ColumnType::Int32
                        | ColumnType::Float64
                        | ColumnType::Numeric
                        | ColumnType::Timestamptz
                ),
                "only an int32, float64, numeric or timestamptz field takes",
            )),
            FilterMatch::IsNull => Some((column.nullable(), "only a nullable field takes")),
        };
        // A band is a required range over time, and only a band has a default.
        if (filter.required
            && (filter.match_mode != FilterMatch::Range
                || column.column_type() != ColumnType::Timestamptz))
            || (filter.default.is_some() && !filter.required)
            || filter.default.is_some_and(|default| default.last_days == 0)
        {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!(
                    "{context} filter {} is required or has a default, which only a range on a timestamptz field that is required takes, with a default of at least one day",
                    filter.field
                ),
            ));
        }
        if let Some((false, which)) = takes {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!(
                    "{context} filter {} matches by {}, which {which} it",
                    filter.field,
                    filter.match_mode.as_str()
                ),
            ));
        }
        if !filter_fields.insert(filter.field.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!("{context} repeats filter field {}", filter.field),
            ));
        }
    }
    if let Some(search) = &operation.search {
        let mut fields = BTreeSet::new();
        if search.fields.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!("{context} search names no field"),
            ));
        }
        for field in &search.fields {
            validate_field(table, model_name, field)?;
            // The search matches a part of the text, as a contains filter does.
            let text = table
                .columns()
                .iter()
                .any(|column| column.name() == field && column.column_type() == ColumnType::Text);
            if !text || !fields.insert(field.as_str()) {
                return Err(GenerateError::new(
                    GenerateErrorType::InvalidOperation,
                    format!("{context} search field {field} must be a text field named once"),
                ));
            }
        }
    }
    if let Some(sort) = &operation.sort {
        if sort.fields.is_empty()
            || sort.directions.is_empty()
            || sort.fields.iter().collect::<BTreeSet<_>>().len() != sort.fields.len()
            || sort.directions.iter().collect::<BTreeSet<_>>().len() != sort.directions.len()
        {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!("{context} sort must be a nonempty finite product"),
            ));
        }
        for field in &sort.fields {
            validate_sort_field(table, model_name, &context, field)?;
        }
    }
    let limit = operation.limit.as_ref().ok_or_else(|| {
        GenerateError::new(
            GenerateErrorType::InvalidOperation,
            format!("{context} requires an explicit limit contract"),
        )
    })?;
    if limit.default != QUERY_LIMIT || limit.minimum != 1 || limit.maximum != QUERY_LIMIT {
        return Err(GenerateError::new(
            GenerateErrorType::InvalidOperation,
            format!("{context} limit must default to 100 and accept exactly 1..=100"),
        ));
    }
    let pagination = operation.pagination.as_ref().ok_or_else(|| {
        GenerateError::new(
            GenerateErrorType::InvalidOperation,
            format!("{context} requires keyset pagination"),
        )
    })?;
    if pagination.default_sort.field != "created_at"
        || pagination.default_sort.direction != CursorDirection::Ascending
        || pagination.tie_breaker.field != "id"
        || operation.sort.as_ref().is_some_and(|sort| {
            !sort.fields.iter().any(|field| field == "created_at")
                || !sort.directions.contains(&CursorDirection::Ascending)
        })
    {
        return Err(GenerateError::new(
            GenerateErrorType::InvalidOperation,
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
            GenerateErrorType::InvalidOperation,
            format!("{context} keyset fields must be non-null timestamptz and uuid"),
        ));
    }
    if let Some(authored) = &operation.authored_sql {
        let sort = operation.sort.as_ref().ok_or_else(|| {
            GenerateError::new(
                GenerateErrorType::InvalidOperation,
                format!("{context} authored variants require an explicit sort declaration"),
            )
        })?;
        validate_authored_variants(&context, sort, authored)?;
    }
    Ok(())
}

fn validate_custom_operation_sql(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    authored_sql: &[AuthoredSql<'_>],
    operation: &str,
    declaration: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    for relation in &declaration.relations {
        let table = catalog
            .tables()
            .iter()
            .find(|table| table.schema() == relation.schema && table.name() == relation.table);
        // A history table has the fixed columns and no constraint that an operation maps.
        let (columns, constraints) = match table {
            Some(table) => (
                table.columns().iter().map(Column::name).collect(),
                table.constraints().iter().map(Constraint::name).collect(),
            ),
            None if logged_history_tables(manifest).any(|(schema, history)| {
                schema == relation.schema && history == relation.table
            }) =>
            {
                (
                    HISTORY_COLUMNS.iter().map(|(name, _)| *name).collect(),
                    BTreeSet::new(),
                )
            }
            None => {
                return Err(GenerateError::for_object(
                    GenerateErrorType::UnknownRelation,
                    format!("{operation} references an unknown relation"),
                    format!("{}.{}", relation.schema, relation.table),
                ));
            }
        };
        for fields in [
            &relation.select_fields,
            &relation.insert_fields,
            &relation.update_fields,
        ] {
            validate_static_sql_relation_fields(operation, relation, &columns, fields)?;
        }
        for name in &relation.constraints {
            if !constraints.contains(name.as_str()) {
                return Err(GenerateError::for_object(
                    GenerateErrorType::InvalidOperation,
                    format!("{operation} requires named constraint {name}"),
                    format!("{}.{}", relation.schema, relation.table),
                ));
            }
        }
    }
    validate_constraint_error_mappings(catalog, operation, declaration)?;
    validate_static_sql_relation_access(catalog, manifest, authored_sql, operation, declaration)
}

fn validate_static_sql_relation_fields(
    operation: &str,
    relation: &crate::manifest::StaticSqlRelationDeclaration,
    columns: &BTreeSet<&str>,
    fields: &[String],
) -> Result<(), GenerateError> {
    if let Some(field) = fields
        .iter()
        .find(|field| !columns.contains(field.as_str()))
    {
        return Err(GenerateError::for_object(
            GenerateErrorType::UnknownColumn,
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
    for name in declaration.constraint_errors.keys() {
        custom_operation_constraint_origin(catalog, declaration, name).ok_or_else(|| {
            GenerateError::for_object(
                GenerateErrorType::InvalidOperation,
                format!("{operation} maps undeclared constraint {name}"),
                name.clone(),
            )
        })?;
    }
    Ok(())
}

/// Admit one reported DELETE target, or refuse it with the reason.
///
/// A DELETE FROM removes the row, so only a model that declares
/// `delete_mode: hard` admits one. A tombstone delete is an UPDATE and removes
/// no row, and a relation that no model declares carries no delete authority at
/// all. The lexer reports the target; this decides it against the manifest.
fn validate_delete_target(
    manifest: &PackageManifest,
    schemas: &BTreeSet<&str>,
    operation: &str,
    path: &str,
    target: &str,
) -> Result<(), GenerateError> {
    let model = manifest
        .models
        .values()
        .find(|model| model.table == target && schemas.contains(model.schema.as_str()));
    let refusal = match model.map(|model| model.delete_mode) {
        Some(Some(DeleteMode::Hard)) => return Ok(()),
        Some(Some(DeleteMode::Tombstone)) => {
            "declares delete_mode: tombstone, and a tombstone delete is an UPDATE that removes no row"
        }
        Some(None) => "declares no delete_mode",
        None => "is not a declared model",
    };
    Err(GenerateError::for_path(
        GenerateErrorType::InvalidOperation,
        format!("{operation} SQL deletes from {target}, which {refusal}"),
        path,
    ))
}

fn validate_static_sql_relation_access(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
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
        .chain(
            logged_history_tables(manifest)
                .filter(|(schema, _)| schemas.contains(schema))
                .map(|(_, history)| {
                    (
                        history,
                        HISTORY_COLUMNS
                            .iter()
                            .map(|(name, _)| (*name).to_owned())
                            .collect(),
                    )
                }),
        )
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
                    GenerateErrorType::InvalidOperation,
                    format!(
                        "{} cannot derive exact relation access: {detail}",
                        statement.path
                    ),
                    statement.path.as_str(),
                )
            })?;
        let deleted = crate::sql_lex::delete_targets(source.bytes).map_err(|detail| {
            GenerateError::for_path(
                GenerateErrorType::InvalidOperation,
                format!("{}: {detail}", statement.path),
                statement.path.as_str(),
            )
        })?;
        for target in &deleted {
            validate_delete_target(manifest, &schemas, operation, &statement.path, target)?;
        }
        for (table, observed) in statement_access {
            let aggregate = actual.entry(table).or_default();
            aggregate.select_fields.extend(observed.select_fields);
            aggregate.insert_fields.extend(observed.insert_fields);
            aggregate.update_fields.extend(observed.update_fields);
            aggregate.lock |= observed.lock;
            aggregate.delete |= observed.delete;
        }
    }
    for relation in &declaration.relations {
        let observed = actual.remove(&relation.table).unwrap_or_default();
        let declared = crate::sql_lex::RelationAccess {
            select_fields: relation.select_fields.iter().cloned().collect(),
            insert_fields: relation.insert_fields.iter().cloned().collect(),
            update_fields: relation.update_fields.iter().cloned().collect(),
            lock: relation.lock,
            delete: relation.delete,
        };
        if observed != declared {
            return Err(GenerateError::for_object(
                GenerateErrorType::InvalidOperation,
                format!(
                    "{operation} {}.{} privilege declaration does not match verified SQL reads, writes, and row locks.\n\
                     Verified SQL: {}\n\
                     Declared: {}\n\
                     DELETE requires delete=true. RETURNING columns require select_fields. Row-lock clauses such as FOR UPDATE require lock=true.",
                    relation.schema,
                    relation.table,
                    serde_json::json!({
                        "select_fields": observed.select_fields,
                        "insert_fields": observed.insert_fields,
                        "update_fields": observed.update_fields,
                        "lock": observed.lock,
                        "delete": observed.delete,
                    }),
                    serde_json::json!({
                        "select_fields": declared.select_fields,
                        "insert_fields": declared.insert_fields,
                        "update_fields": declared.update_fields,
                        "lock": declared.lock,
                        "delete": declared.delete,
                    }),
                ),
                format!("{}.{}", relation.schema, relation.table),
            ));
        }
    }
    if let Some(table) = actual.keys().next() {
        return Err(GenerateError::for_object(
            GenerateErrorType::InvalidOperation,
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
            GenerateErrorType::InvalidOperation,
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
            GenerateErrorType::InvalidOperation,
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
                GenerateErrorType::InvalidOperation,
                format!("{context} authored variants must follow declared field/direction order"),
            ));
        }
    }
    let default_variant = authored.variants.iter().find(|variant| {
        variant.field == "created_at" && variant.direction == CursorDirection::Ascending
    });
    if default_variant.map(|variant| variant.path.as_str()) != Some(authored.default.as_str()) {
        return Err(GenerateError::new(
            GenerateErrorType::InvalidOperation,
            format!("{context} default SQL must be the created_at ascending variant"),
        ));
    }
    Ok(())
}

fn validate_connections(manifest: &PackageManifest) -> Result<(), GenerateError> {
    if manifest.connections.is_empty() && !manifest.declares_no_sql() {
        return Err(GenerateError::new(
            GenerateErrorType::InvalidConnection,
            "manifest declares no database connection",
        ));
    }
    for name in &manifest.connections {
        validate_identifier(name, "connection")?;
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
                GenerateErrorType::DuplicatePath,
                "authored SQL path is unsafe or repeated",
                source.path,
            ));
        }
    }
    if let Some(path) = expected.difference(&supplied).next() {
        return Err(GenerateError::for_path(
            GenerateErrorType::MissingAuthoredSql,
            "manifest-authored SQL path was not supplied",
            *path,
        ));
    }
    if let Some(path) = supplied.difference(&expected).next() {
        return Err(GenerateError::for_path(
            GenerateErrorType::UnexpectedAuthoredSql,
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
                    GenerateErrorType::SchemaQualifiedSql,
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

/// Each authored query file implements the match mode that each filter of its
/// operation declares (wamn-efxf). Filter `n` binds `$n`, as it does in a
/// generated query.
fn validate_authored_query_filters(
    manifest: &PackageManifest,
    authored_sql: &[AuthoredSql<'_>],
) -> Result<(), GenerateError> {
    for (model_name, model) in &manifest.models {
        for (operation_name, operation) in &model.operations {
            let Some(authored) = &operation.authored_sql else {
                continue;
            };
            let operation_name = operation_name.as_str();
            for variant in &authored.variants {
                let path = variant.path.as_str();
                let Some(source) = authored_sql.iter().find(|source| source.path == path) else {
                    continue;
                };
                for (index, filter) in operation.filters.iter().enumerate() {
                    let parameter = u32::try_from(index + 1).expect("a query has few filters");
                    let declared = filter.match_mode;
                    let refusal = match crate::sql_lex::filter_predicate(source.bytes, parameter) {
                        Some((Some(found), _)) if found == declared => continue,
                        Some((Some(found), line)) => format!(
                            "{path}:{line}: {model_name}.{operation_name} filter `{}` declares \
                             match {}, but its predicate on ${parameter} matches {}",
                            filter.field,
                            declared.as_str(),
                            found.as_str()
                        ),
                        Some((None, line)) => format!(
                            "{path}:{line}: {model_name}.{operation_name} filter `{}` declares \
                             match {}, but its predicate on ${parameter} matches no filter mode",
                            filter.field,
                            declared.as_str()
                        ),
                        None => format!(
                            "{path}: {model_name}.{operation_name} filter `{}` declares match \
                             {}, but the file never reads ${parameter}",
                            filter.field,
                            declared.as_str()
                        ),
                    };
                    return Err(GenerateError::for_path(
                        GenerateErrorType::InvalidOperation,
                        refusal,
                        path,
                    ));
                }
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
                GenerateErrorType::DuplicatePath,
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
            GenerateErrorType::UnknownColumn,
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
