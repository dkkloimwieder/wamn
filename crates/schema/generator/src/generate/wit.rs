//! Typed component contracts generated from operation declarations.

use std::fmt::Write as _;
use wamn_scalar::SqlValueType;

use super::typed_exports::{emit_error_mapper, emit_export_adapter, emit_row_adapter};
use super::{BTreeMap, Column, insert_bytes};
use crate::client_ir::FieldIr;
use crate::manifest::{
    AccessOperationErrorLiteral, ContractFieldDeclaration, CrudAction, CustomOperationDeclaration,
    OperationDeclaration, OperationErrorDetailDeclaration, PackageManifest, ResultClass,
    binding_identifier, rust_identifier, rust_type_identifier,
};
use crate::manifest::{FilterMatch, OperationErrorDetailKey, authored_name};
use crate::{ManifestError as GenerateError, ManifestErrorType as GenerateErrorType};

pub(super) const CRUD_REQUEST_ELEMENTS_MINIMUM: u32 = 1;
pub(super) const CRUD_REQUEST_ELEMENTS_MAXIMUM: u32 = 100;

/// Emit typed boundaries for every declared model operation.
pub(super) fn emit_model_wit(
    files: &mut BTreeMap<String, Vec<u8>>,
    compiled: &super::operations::ModelDescription<'_>,
) -> Result<(), GenerateError> {
    let package = compiled.package.id.replace('_', "-");
    let model_name = compiled.name;
    let directory = format!("generated/wit/deps/{package}-{}", wit_name(model_name));
    insert_bytes(
        files,
        &format!("{directory}/package.wit"),
        emit_model_package_wit(compiled).into_bytes(),
    )?;
    for (action, facts) in &compiled.operations {
        insert_bytes(
            files,
            &format!(
                "generated/wit/{model_name}_{}_operation.rs",
                action.as_str()
            ),
            emit_model_export(*action, compiled, facts).into_bytes(),
        )?;
    }
    Ok(())
}

fn emit_model_package_wit(compiled: &super::operations::ModelDescription<'_>) -> String {
    let package = compiled.package.id.replace('_', "-");
    let mut source = format!(
        "package {package}:{}@{};\n\n",
        wit_name(compiled.name),
        compiled.package.version
    );
    for (action, facts) in &compiled.operations {
        emit_crud_interface(&mut source, *action, &compiled.shape, facts, compiled.feed);
    }
    source
}

fn emit_crud_interface(
    source: &mut String,
    action: CrudAction,
    table: &super::operations::ModelShape<'_>,
    facts: &super::operations::ModelOperation<'_>,
    feed: bool,
) {
    let operation = facts.request;
    let fields = &facts.result.fields;
    let details = &facts.refusals;
    let revision_type = facts.revision;
    if action == CrudAction::Update {
        emit_update_interface(source, table, operation, fields, details, revision_type);
        return;
    }
    let name = action.as_str();
    writeln!(
        source,
        "interface {name} {{\n  use wamn:node/types@0.1.0.{{node-context, node-error, json-value}};\n"
    )
    .expect("writing to a String cannot fail");
    // A range filter's value is a record of its own, which the request names.
    if action == CrudAction::Query {
        for filter in &operation.filters {
            if filter.match_mode == FilterMatch::Range {
                let ty = wit_type(model_column(table, &filter.field).column_type());
                writeln!(
                    source,
                    "  record {}-range {{\n    min: option<{ty}>,\n    max: option<{ty}>,\n  }}\n",
                    wit_name(&filter.field)
                )
                .expect("writing to a String cannot fail");
            }
        }
        if feed {
            source.push_str("  /// The feed part: present, the read returns the changes since the\n  /// cursor instead of a page.\n  record query-feed {\n    cursor: option<string>,\n    limit: option<u32>,\n  }\n\n");
        }
    }
    writeln!(source, "  record {name}-request {{").expect("writing to a String cannot fail");
    match action {
        CrudAction::Get | CrudAction::Delete => {
            writeln!(source, "    id: {},", wit_type(table.key.column_type()))
                .expect("writing to a String cannot fail");
        }
        CrudAction::Create => {
            if !table.generated_id {
                source.push_str("    id: string,\n");
            }
            source.push_str("    idempotency-key: string,\n");
            emit_writable_fields(source, table, operation);
        }
        CrudAction::Query => {
            for filter in &operation.filters {
                let field = wit_name(&filter.field);
                let value = match filter.match_mode {
                    FilterMatch::Range => format!("{field}-range"),
                    FilterMatch::IsNull => "bool".to_owned(),
                    _ => format!(
                        "list<{}>",
                        wit_type(model_column(table, &filter.field).column_type())
                    ),
                };
                writeln!(source, "    {field}: option<{value}>,")
                    .expect("writing to a String cannot fail");
            }
            if operation.search.is_some() {
                source.push_str("    search: option<string>,\n");
            }
            if operation.sort.is_some() {
                source.push_str(
                    "    sort-field: option<string>,\n    sort-direction: option<string>,\n",
                );
            }
            if operation.pagination.is_some() {
                source.push_str("    cursor: option<string>,\n");
            }
            if operation.limit.is_some() {
                source.push_str("    limit: option<u32>,\n");
            }
            if feed {
                source.push_str("    feed: option<query-feed>,\n");
            }
        }
        CrudAction::Update => unreachable!("update uses its own emitter"),
    }
    if action == CrudAction::Delete {
        let revision = operation
            .revision_field
            .as_deref()
            .expect("validated delete revision exists");
        writeln!(
            source,
            "    expected-{}: {},",
            wit_name(revision),
            wit_type(model_column(table, revision).column_type())
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str("  }\n\n");
    emit_operation_errors(source, name, details, revision_type);
    if action == CrudAction::Query {
        emit_query_page(source, fields, feed, table);
        return;
    }
    let key = request_key_field(crud_is_keyed(action));
    writeln!(source, "  record {name}-item {{\n{key}    input: result<{name}-request, invalid-input-detail>,\n  }}\n")
        .expect("writing to a String cannot fail");
    emit_crud_result(source, name, action, fields, operation.result);
    writeln!(
        source,
        "  record {name}-outcome {{\n{key}    outcome: result<{name}-result, {name}-error>,\n  }}\n"
    )
    .expect("writing to a String cannot fail");
    writeln!(source, "  run: async func(ctx: node-context, input: list<{name}-item>) -> result<list<{name}-outcome>, node-error>;\n}}\n")
        .expect("writing to a String cannot fail");
}

/// A query returns its page as one result: the rows and the cursor that
/// continues the read, or the error that ended it. The host keeps
/// the WAL position for a list ETag.
fn emit_query_page(
    source: &mut String,
    fields: &[ContractFieldDeclaration],
    feed: bool,
    table: &super::operations::ModelShape<'_>,
) {
    source.push_str("  record query-row {\n");
    for column in fields {
        let mut ty = wit_type(column.ty);
        if column.nullable {
            ty = format!("option<{ty}>");
        }
        writeln!(source, "    {}: {ty},", wit_name(&column.path))
            .expect("writing to a String cannot fail");
    }
    source.push_str("  }\n\n");
    if feed {
        // A list read with a feed answers a page, with the cursor its feed
        // starts from, or the changes since a cursor
        // (docs/architecture/execution.md#change-feed).
        source.push_str(&"  /// One page. A next cursor means more rows follow. The cursor starts\n  /// the change feed at this page's snapshot.\n  record query-page {\n    item: list<query-row>,\n    next-cursor: option<string>,\n    cursor: string,\n  }\n\n  /// One changed row. A change with no data is a row that was deleted or\n  /// left the target of the read.\n  record query-change {\n    id: string,\n    row-version: s64,\n    data: option<query-row>,\n  }\n\n  /// The changes since a cursor, one per row. Not complete means more\n  /// changes follow the next cursor.\n  record query-changes {\n    rows: list<query-change>,\n    next-cursor: string,\n    complete: bool,\n  }\n\n  variant query-result {\n    page(query-page),\n    changes(query-changes),\n  }\n\n  record query-outcome {\n    outcome: result<query-result, query-error>,\n  }\n\n".replace("id: string", &format!("id: {}", wit_type(table.key.column_type()))));
    } else {
        source.push_str("  /// One page. A cursor means more rows follow.\n  record query-result {\n    item: list<query-row>,\n    next-cursor: option<string>,\n  }\n\n  record query-outcome {\n    outcome: result<query-result, query-error>,\n  }\n\n");
    }
    source.push_str("  run: async func(ctx: node-context, input: query-request) -> result<query-outcome, node-error>;\n}\n\n");
}

fn emit_writable_fields(
    source: &mut String,
    table: &super::operations::ModelShape<'_>,
    operation: &OperationDeclaration,
) {
    for field in &operation.writable_fields {
        let column = model_column(table, field);
        writeln!(
            source,
            "    {}: option<option<{}>>,",
            wit_name(field),
            wit_type(column.column_type())
        )
        .expect("writing to a String cannot fail");
    }
}

fn emit_operation_errors(
    source: &mut String,
    name: &str,
    details: &BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
    revision: Option<SqlValueType>,
) {
    for (literal, detail) in details {
        if !detail.required.is_empty() || !detail.optional.is_empty() {
            emit_error_detail(source, access_error_literal(*literal), detail, revision);
        }
    }
    writeln!(source, "  variant {name}-error {{").expect("writing to a String cannot fail");
    for (literal, detail) in details {
        let literal = access_error_literal(*literal);
        if detail.required.is_empty() && detail.optional.is_empty() {
            writeln!(source, "    {},", wit_name(literal))
                .expect("writing to a String cannot fail");
        } else {
            writeln!(
                source,
                "    {}({}-detail),",
                wit_name(literal),
                wit_name(literal)
            )
            .expect("writing to a String cannot fail");
        }
    }
    source.push_str("  }\n\n");
}

fn emit_crud_result(
    source: &mut String,
    name: &str,
    action: CrudAction,
    fields: &[ContractFieldDeclaration],
    class: crate::manifest::ResultClass,
) {
    writeln!(source, "  record {name}-row {{").expect("writing to a String cannot fail");
    for column in fields {
        let mut ty = wit_type(column.ty);
        if column.nullable {
            ty = format!("option<{ty}>");
        }
        writeln!(source, "    {}: {ty},", wit_name(&column.path))
            .expect("writing to a String cannot fail");
    }
    source.push_str("  }\n\n");
    let carrier = match class {
        crate::manifest::ResultClass::One => format!("{name}-row"),
        crate::manifest::ResultClass::OptionalOne => format!("option<{name}-row>"),
        crate::manifest::ResultClass::BoundedList | crate::manifest::ResultClass::Page => {
            format!("list<{name}-row>")
        }
    };
    writeln!(source, "  record {name}-result {{\n    value: {carrier},")
        .expect("writing to a String cannot fail");
    if action == CrudAction::Query && class == crate::manifest::ResultClass::Page {
        source.push_str("    next-cursor: option<string>,\n");
    }
    source.push_str("  }\n\n");
}

fn emit_update_interface(
    source: &mut String,
    table: &super::operations::ModelShape<'_>,
    operation: &OperationDeclaration,
    fields: &[ContractFieldDeclaration],
    details: &BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
    revision_type: Option<SqlValueType>,
) {
    source.push_str("interface update {\n  use wamn:node/types@0.1.0.{node-context, node-error, json-value};\n\n  record update-change {\n");
    emit_writable_fields(source, table, operation);
    let revision = operation
        .revision_field
        .as_deref()
        .expect("validated update revision exists");
    writeln!(
        source,
        "  }}\n\n  record update-request {{\n    id: {},",
        wit_type(table.key.column_type())
    )
    .expect("writing to a String cannot fail");
    writeln!(
        source,
        "    expected-{}: {},",
        wit_name(revision),
        wit_type(model_column(table, revision).column_type())
    )
    .expect("writing to a String cannot fail");
    source.push_str("    change: update-change,\n  }\n\n");
    for (literal, detail) in details {
        if !detail.required.is_empty() || !detail.optional.is_empty() {
            emit_error_detail(
                source,
                access_error_literal(*literal),
                detail,
                revision_type,
            );
        }
    }
    source.push_str("  variant update-error {\n");
    for (literal, detail) in details {
        let literal = access_error_literal(*literal);
        if detail.required.is_empty() && detail.optional.is_empty() {
            writeln!(source, "    {},", wit_name(literal))
                .expect("writing to a String cannot fail");
        } else {
            writeln!(
                source,
                "    {}({}-detail),",
                wit_name(literal),
                wit_name(literal)
            )
            .expect("writing to a String cannot fail");
        }
    }
    source.push_str("  }\n\n  record update-item {\n    request-id: string,\n    input: result<update-request, invalid-input-detail>,\n  }\n\n  record update-result {\n");
    for field in fields {
        let mut ty = wit_type(field.ty);
        if field.nullable {
            ty = format!("option<{ty}>");
        }
        writeln!(source, "    {}: {ty},", wit_name(&field.path))
            .expect("writing to a String cannot fail");
    }
    source.push_str("  }\n\n  record update-outcome {\n    request-id: string,\n    outcome: result<update-result, update-error>,\n  }\n\n  run: async func(ctx: node-context, input: list<update-item>) -> result<list<update-outcome>, node-error>;\n}\n");
}

fn model_column<'a>(table: &'a super::operations::ModelShape<'_>, field: &str) -> &'a Column {
    table
        .column(field)
        .expect("validated model operation field exists")
}

pub(super) fn access_error_literal(literal: AccessOperationErrorLiteral) -> &'static str {
    match literal {
        AccessOperationErrorLiteral::InvalidInput => "invalid_input",
        AccessOperationErrorLiteral::NotFound => "not_found",
        AccessOperationErrorLiteral::ConcurrencyConflict => "concurrency_conflict",
        AccessOperationErrorLiteral::IdempotencyConflict => "idempotency_conflict",
        AccessOperationErrorLiteral::UniqueViolation => "unique_violation",
        AccessOperationErrorLiteral::ForeignKeyViolation => "foreign_key_violation",
        AccessOperationErrorLiteral::CheckViolation => "check_violation",
        AccessOperationErrorLiteral::ExclusionViolation => "exclusion_violation",
        AccessOperationErrorLiteral::Retry => "retry",
        AccessOperationErrorLiteral::Timeout => "timeout",
        AccessOperationErrorLiteral::PermissionDenied => "permission_denied",
        AccessOperationErrorLiteral::InternalError => "internal_error",
        AccessOperationErrorLiteral::CursorExpired => "cursor_expired",
    }
}

fn emit_model_export(
    action: CrudAction,
    compiled: &super::operations::ModelDescription<'_>,
    facts: &super::operations::ModelOperation<'_>,
) -> String {
    let name = rust_type_identifier(action.as_str());
    let mut source = support_header();
    emit_crud_invalid_detail(&mut source, &facts.refusals);
    source.push_str(&if action == CrudAction::Update {
        emit_update_normalizer(
            &compiled.shape,
            facts.request,
            &facts.result.fields,
            compiled.min_lengths,
        )
    } else {
        emit_crud_normalizer(
            action,
            &compiled.shape,
            facts.request,
            &facts.result.fields,
            compiled.min_lengths,
        )
    });
    if action == CrudAction::Query {
        emit_query_limit(&mut source, facts.request, compiled.feed, &facts.refusals);
        source.push_str(QUERY_RUN);
        source.push_str(QUERY_EXPORT);
    } else {
        source.push_str(&export_prelude(
            &format!("{name}Item"),
            1,
            100,
            crud_is_keyed(action),
        ));
        source.push_str(&emit_handler(&name, crud_is_keyed(action)));
        source.push_str(&emit_export_adapter(&name, false, action.changes_records()));
    }
    source.push_str(&emit_row_adapter(
        facts
            .result
            .fields
            .iter()
            .map(|f| (f.path.as_str(), f.ty, f.nullable)),
    ));
    source.push_str(&emit_error_mapper(
        &format!("{name}Error"),
        facts
            .refusals
            .iter()
            .map(|(code, detail)| (access_error_literal(*code), detail)),
        facts.revision,
        &facts.guarded_fields,
    ));
    source
}

fn support_header() -> String {
    String::from(
        "// @generated from operation declarations; do not edit.\n#[allow(unused_imports)]\npub(crate) use wamn_data_access::InputError;\n\n",
    )
}

fn scalar_refusal(type_name: &str, value: &str) -> Option<String> {
    if type_name == "json" {
        return Some(format!(
            "wamn_data_access::scalar::json(&({value}).encoded_json).map(|parsed| {{ ({value}).encoded_json = parsed.0; }}).is_err()"
        ));
    }
    matches!(type_name, "uuid" | "numeric" | "timestamptz").then(|| {
        format!("wamn_data_access::scalar::{type_name}({value}).map(|parsed| {{ *{value} = parsed.0; }}).is_err()")
    })
}

/// Apply the declared default and refuse a page or feed outside its bounds.
fn emit_query_limit(
    source: &mut String,
    operation: &OperationDeclaration,
    feed: bool,
    details: &BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
) {
    source.push_str("#[allow(clippy::unnecessary_wraps)]\nfn limit(request: &mut contract::QueryRequest) -> Result<(), contract::InvalidInputDetail> {\n    let _ = &request;\n");
    if let Some(limit) = &operation.limit {
        let optional = &details[&AccessOperationErrorLiteral::InvalidInput].optional;
        writeln!(
            source,
            "    let limit = *request.limit.get_or_insert({});\n    if limit < {} || limit > {} {{\n        #[allow(unused_mut)]\n        let mut detail = invalid(\"limit\");",
            limit.default, limit.minimum, limit.maximum
        )
        .expect("writing to a String cannot fail");
        if optional.contains(&OperationErrorDetailKey::Minimum) {
            writeln!(
                source,
                "        detail.minimum = Some(\"{}\".to_owned());",
                limit.minimum
            )
            .expect("writing to a String cannot fail");
        }
        if optional.contains(&OperationErrorDetailKey::Maximum) {
            writeln!(
                source,
                "        detail.maximum = Some(\"{}\".to_owned());",
                limit.maximum
            )
            .expect("writing to a String cannot fail");
        }
        if optional.contains(&OperationErrorDetailKey::Observed) {
            source.push_str("        detail.observed = Some(limit.to_string());\n");
        }
        source.push_str("        return Err(detail);\n    }\n");
        // The feed uses the same bounds as a page.
        if feed {
            writeln!(
                source,
                "    if let Some(feed) = &mut request.feed {{\n        let limit = *feed.limit.get_or_insert({});\n        if limit < {} || limit > {} {{\n            #[allow(unused_mut)]\n            let mut detail = invalid(\"feed.limit\");",
                limit.default, limit.minimum, limit.maximum
            )
            .expect("writing to a String cannot fail");
            if optional.contains(&OperationErrorDetailKey::Minimum) {
                writeln!(
                    source,
                    "            detail.minimum = Some(\"{}\".to_owned());",
                    limit.minimum
                )
                .expect("writing to a String cannot fail");
            }
            if optional.contains(&OperationErrorDetailKey::Maximum) {
                writeln!(
                    source,
                    "            detail.maximum = Some(\"{}\".to_owned());",
                    limit.maximum
                )
                .expect("writing to a String cannot fail");
            }
            if optional.contains(&OperationErrorDetailKey::Observed) {
                source.push_str("            detail.observed = Some(limit.to_string());\n");
            }
            source.push_str("            return Err(detail);\n        }\n    }\n");
        }
    }
    source.push_str("    Ok(())\n}\n");
}

/// The handler call of every query.
const QUERY_RUN: &str = r"
#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    request: Result<contract::QueryRequest, contract::InvalidInputDetail>,
    state: &mut S,
    mut handler: F,
) -> Result<contract::QueryResult, contract::QueryError>
where
    F: AsyncFnMut(&mut S, contract::QueryRequest) -> Result<contract::QueryResult, contract::QueryError>,
{
    wamn_data_access::operation::call(request, state, |request| normalize(request).and_then(|()| limit(request)), contract::QueryError::InvalidInput, handler).await
}
";

/// The typed export of every query. The host retains its read metadata.
const QUERY_EXPORT: &str = "pub(crate) type Input = contract::QueryRequest;\npub(crate) type Output = contract::QueryOutcome;\npub(crate) use wamn_data_access::export_query as export_operation;\n";

fn emit_mutation_validation(
    source: &mut String,
    table: &super::operations::ModelShape<'_>,
    operation: &OperationDeclaration,
    fields: &[ContractFieldDeclaration],
    min_lengths: &BTreeMap<String, u32>,
    prefix: &str,
    create: bool,
) {
    for field in &operation.writable_fields {
        let column = model_column(table, field);
        let name = binding_identifier(field).expect("validated mutation field has a Rust name");
        let path = format!("{prefix}{field}");
        let access = format!("request.{prefix}{name}");
        // The create contract states `omitted: invalid_input` for this column.
        if create && super::omission_refused(column) {
            writeln!(
                source,
                "    if {access}.is_none() {{ return Err(invalid({path:?})); }}"
            )
            .expect("writing to a String cannot fail");
        }
        if !column.nullable() {
            writeln!(
                source,
                "    if matches!({access}, Some(None)) {{ return Err(invalid({path:?})); }}"
            )
            .expect("writing to a String cannot fail");
        }
        // The values the contract states: the fewer an operation declares, or
        // the model's.
        let values = operation.values.get(field).map_or_else(
            || {
                fields
                    .iter()
                    .find(|item| item.path == *field)
                    .map_or(&[][..], |item| item.values.as_slice())
            },
            Vec::as_slice,
        );
        let mut conditions = Vec::new();
        if let Some(condition) = scalar_refusal(column.column_type().as_str(), "value") {
            conditions.push(condition);
        }
        if !values.is_empty() {
            conditions.push(format!("!{values:?}.contains(&value.as_str())"));
        }
        // The fewest characters after a trim. The table's CHECK guards the
        // same rule, and this refusal answers first.
        if let Some(minimum) = min_lengths.get(field) {
            conditions.push(format!("value.trim().chars().count() < {minimum}"));
        }
        if !conditions.is_empty() {
            let condition = conditions.join(" || ");
            writeln!(source, "    if let Some(Some(value)) = &mut {access} && ({condition}) {{ return Err(invalid({path:?})); }}")
                .expect("writing to a String cannot fail");
        }
    }
}

fn emit_update_normalizer(
    table: &super::operations::ModelShape<'_>,
    operation: &OperationDeclaration,
    fields: &[ContractFieldDeclaration],
    min_lengths: &BTreeMap<String, u32>,
) -> String {
    let mut source = String::from(
        "#[allow(clippy::unnecessary_wraps)]\npub(crate) fn normalize(request: &mut contract::UpdateRequest) -> Result<(), contract::InvalidInputDetail> {\n",
    );
    emit_scalar_validation(
        &mut source,
        "request.id",
        "id",
        table.key.column_type(),
        &[],
        false,
    );
    emit_mutation_validation(
        &mut source,
        table,
        operation,
        fields,
        min_lengths,
        "change.",
        false,
    );
    source.push_str("    Ok(())\n}\n\n");
    source
}

fn emit_crud_normalizer(
    action: CrudAction,
    table: &super::operations::ModelShape<'_>,
    operation: &OperationDeclaration,
    fields: &[ContractFieldDeclaration],
    min_lengths: &BTreeMap<String, u32>,
) -> String {
    let type_name = rust_type_identifier(action.as_str());
    let mut source = format!(
        "#[allow(clippy::unnecessary_wraps)]\npub(crate) fn normalize(request: &mut contract::{type_name}Request) -> Result<(), contract::InvalidInputDetail> {{\n"
    );
    source.push_str("    let _ = &request;\n");
    if matches!(action, CrudAction::Get | CrudAction::Delete)
        || (action == CrudAction::Create && !table.generated_id)
    {
        emit_scalar_validation(
            &mut source,
            "request.id",
            "id",
            table.key.column_type(),
            &[],
            false,
        );
    }
    if action == CrudAction::Create {
        emit_mutation_validation(&mut source, table, operation, fields, min_lengths, "", true);
    }
    if action == CrudAction::Query {
        for filter in &operation.filters {
            let column = model_column(table, &filter.field);
            let name = binding_identifier(&filter.field).expect("validated filter has a Rust name");
            // Lists and ranges carry scalars; an is-null filter carries one boolean.
            if let Some(condition) = scalar_refusal(column.column_type().as_str(), "value") {
                if filter.match_mode.takes_list() {
                    writeln!(source, "    if let Some(values) = &mut request.{name} {{ for value in values {{ if {condition} {{ return Err(invalid({:?})); }} }} }}", format!("filter.{}", filter.field)).expect("write scalar validation");
                } else if filter.match_mode == FilterMatch::Range {
                    writeln!(source, "    if let Some(range) = &mut request.{name} {{ for value in [&mut range.min, &mut range.max].into_iter().flatten() {{ if {condition} {{ return Err(invalid({:?})); }} }} }}", format!("filter.{}", filter.field)).expect("write scalar validation");
                }
            }
            // A filter over a field with a value domain matches only those
            // values, as the route schema states.
            let values = fields
                .iter()
                .find(|item| item.path == filter.field)
                .map_or(&[][..], |item| item.values.as_slice());
            if !values.is_empty() && filter.match_mode.takes_list() {
                writeln!(source, "    if let Some(values) = &request.{name} {{ for value in values {{ if !{values:?}.contains(&value.as_str()) {{ return Err(invalid({:?})); }} }} }}", format!("filter.{}", filter.field))
                    .expect("writing to a String cannot fail");
            }
        }
    }
    source.push_str("    Ok(())\n}\n\n");
    source
}

fn emit_scalar_validation(
    source: &mut String,
    access: &str,
    path: &str,
    ty: SqlValueType,
    values: &[String],
    optional: bool,
) {
    let value = if optional {
        writeln!(source, "    if let Some(value) = &mut {access} {{")
            .expect("writing to a String cannot fail");
        "value"
    } else {
        &format!("&mut {access}")
    };
    if let Some(condition) = scalar_refusal(ty.as_str(), value) {
        writeln!(
            source,
            "        if {condition} {{ return Err(invalid({path:?})); }}"
        )
        .expect("writing to a String cannot fail");
    }
    if !values.is_empty() {
        writeln!(
            source,
            "        if !{values:?}.contains(&{value}.as_str()) {{ return Err(invalid({path:?})); }}"
        )
        .expect("writing to a String cannot fail");
    }
    if optional {
        source.push_str("    }\n");
    }
}

fn emit_crud_invalid_detail(
    source: &mut String,
    details: &BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
) {
    let detail = &details[&AccessOperationErrorLiteral::InvalidInput];
    source.push_str("fn invalid(field: &str) -> contract::InvalidInputDetail { contract::InvalidInputDetail {\n");
    for key in &detail.required {
        if *key == OperationErrorDetailKey::Field {
            source.push_str("    field: field.to_owned(),\n");
        }
    }
    for key in &detail.optional {
        writeln!(source, "    {}: None,", detail_name(*key).replace('-', "_"))
            .expect("writing to a String cannot fail");
    }
    source.push_str("} }\n\n");
}

fn emit_handler(type_name: &str, keyed: bool) -> String {
    let (key, outcome) = if keyed {
        ("item.request_id", "request_id, outcome")
    } else {
        ("()", "outcome")
    };
    format!(
        r"
#[allow(dead_code)]
pub(crate) async fn run<S, F>(input: Vec<contract::{type_name}Item>, state: &mut S, handler: F) -> Vec<contract::{type_name}Outcome>
where
    F: AsyncFnMut(&mut S, contract::{type_name}Request) -> Result<contract::{type_name}Result, contract::{type_name}Error>,
{{
    wamn_data_access::operation::dispatch(input.into_iter().map(|item| ({key}, item.input)), state, normalize, contract::{type_name}Error::InvalidInput, handler).await.into_iter().map(|(request_id, outcome)| {{ let _ = &request_id; contract::{type_name}Outcome {{ {outcome} }} }}).collect()
}}
"
    )
}

fn export_prelude(item: &str, minimum: u32, maximum: u32, keyed: bool) -> String {
    let correlations = if keyed {
        "input.iter().map(|item| item.request_id.as_str())"
    } else {
        "std::iter::empty()"
    };
    format!(
        "#[allow(dead_code)]\npub(crate) fn validate(input: &[contract::{item}]) -> Result<(), InputError> {{\nwamn_data_access::operation::validate_envelope(input.len(), {minimum}..={maximum}, {correlations})\n}}\n"
    )
}

/// Whether the request elements of a generated CRUD operation carry a `request_id`.
///
/// A read carries none (`docs/architecture/execution.md`): its one request element
/// travels in a GET query string, and its outcomes match its request elements by list
/// position.
fn crud_is_keyed(action: CrudAction) -> bool {
    !matches!(action, CrudAction::Get | CrudAction::Query)
}

/// Whether the request elements of a custom operation carry a `request_id`: every type
/// but a projection, which is a read.
fn custom_is_keyed(operation: &CustomOperationDeclaration) -> bool {
    operation.type_ != crate::manifest::CustomOperationType::Projection
}

/// The WIT field that keys a request element or an outcome, or nothing for a read.
fn request_key_field(keyed: bool) -> &'static str {
    if keyed {
        "    request-id: string,\n"
    } else {
        ""
    }
}

/// Emit a typed component boundary for a declared custom operation.
pub(super) fn emit_custom_operation_wit(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    operation_name: &str,
    compiled: &BTreeMap<String, super::operations::CustomDescription<'_>>,
) -> Result<(), GenerateError> {
    let (group, local_name) = operation_name.split_once('.').ok_or_else(|| {
        GenerateError::new(
            GenerateErrorType::InvalidOperation,
            "custom operation needs a group and local name",
        )
    })?;
    let package = manifest.package.id.replace('_', "-");
    let version = &manifest.package.version;
    let directory = format!("generated/wit/deps/{package}-{}", wit_name(group));
    let package_path = format!("{directory}/package.wit");
    let group_marker = manifest
        .custom_operations
        .keys()
        .find_map(|candidate| {
            candidate
                .strip_prefix(&format!("{group}."))
                .map(|name| format!("interface {} {{", wit_name(name)))
        })
        .expect("current custom operation belongs to its group");
    let group_is_emitted = files
        .get(&package_path)
        .is_some_and(|existing| String::from_utf8_lossy(existing).contains(&group_marker));
    if !group_is_emitted {
        let source = emit_custom_group(&package, version, group, manifest, compiled);
        if let Some(existing) = files.get_mut(&package_path) {
            while existing.last().is_some_and(u8::is_ascii_whitespace) {
                existing.pop();
            }
            existing.extend_from_slice(b"\n\n");
            let body = source
                .split_once("\n\n")
                .map_or(source.as_str(), |(_, body)| body);
            existing.extend_from_slice(body.trim_end().as_bytes());
            existing.push(b'\n');
        } else {
            insert_bytes(files, &package_path, source.into_bytes())?;
        }
    }
    let export = emit_custom_export(local_name, &compiled[operation_name]);
    insert_bytes(
        files,
        &format!(
            "generated/wit/{}_{}_operation.rs",
            artifact_name(group),
            artifact_name(local_name)
        ),
        export.into_bytes(),
    )?;
    Ok(())
}

fn emit_custom_group(
    package: &str,
    version: &str,
    group: &str,
    manifest: &PackageManifest,
    compiled: &BTreeMap<String, super::operations::CustomDescription<'_>>,
) -> String {
    let mut source = format!("package {package}:{}@{version};\n\n", wit_name(group));
    for (operation_name, facts) in compiled {
        let Some(local_name) = operation_name.strip_prefix(&format!("{group}.")) else {
            continue;
        };
        let operation = facts.declaration;
        let emitted = if let Some((_, dependency)) =
            manifest.base_dependencies.iter().find(|(_, item)| {
                item.operations
                    .iter()
                    .any(|candidate| candidate == operation_name)
            }) {
            emit_forwarding_interface(package, version, group, local_name, dependency, operation)
        } else {
            emit_owned_interface(package, version, group, local_name, manifest, facts)
        };
        let marker = format!("interface {} {{", wit_name(local_name));
        let start = emitted
            .rfind(&marker)
            .expect("custom interface emitter includes its operation");
        source.push_str(&emitted[start..]);
    }
    source.truncate(source.trim_end().len());
    source.push('\n');
    source
}

fn emit_forwarding_interface(
    package: &str,
    version: &str,
    group: &str,
    local_name: &str,
    dependency: &crate::manifest::BaseDependencyRequirement,
    _operation: &CustomOperationDeclaration,
) -> String {
    let dependency_package = dependency.package.replace('_', "-");
    let interface = wit_name(local_name);
    format!(
        "package {package}:{}@{version};\n\ninterface {interface} {{\n  use wamn:node/types@0.1.0.{{node-context, node-error, json-value}};\n  use {dependency_package}:{}/{interface}@{}.{{{interface}-item, {interface}-outcome}};\n\n  run: async func(ctx: node-context, input: list<{interface}-item>) -> result<list<{interface}-outcome>, node-error>;\n}}\n",
        wit_name(group),
        wit_name(group),
        dependency.version
    )
}

fn emit_owned_interface(
    package: &str,
    version: &str,
    group: &str,
    local_name: &str,
    manifest: &PackageManifest,
    compiled: &super::operations::CustomDescription<'_>,
) -> String {
    let operation = compiled.declaration;
    let result = compiled.result.as_ref();
    let request_fields = &compiled.request;
    let interface = wit_name(local_name);
    let mut source = format!("package {package}:{}@{version};\n\n", wit_name(group));
    for operation_name in manifest.custom_operations.keys() {
        let Some(other_name) = operation_name.strip_prefix(&format!("{group}.")) else {
            continue;
        };
        if other_name == local_name {
            continue;
        }
        writeln!(
            source,
            "interface {} {{\n  use wamn:node/types@0.1.0.{{json, node-context, emission, node-error}};\n\n  run: async func(ctx: node-context, input: json) -> result<emission, node-error>;\n}}\n",
            wit_name(other_name)
        )
        .expect("writing to a String cannot fail");
    }
    writeln!(
        source,
        "interface {interface} {{\n  use wamn:node/types@0.1.0.{{node-context, node-error, json-value}};\n"
    )
    .expect("writing to a String cannot fail");
    if operation.input.envelope.is_none() && result.is_none() {
        emit_nested_wit_records(&mut source, &interface, request_fields, "");
        writeln!(source, "  record {interface}-request {{")
            .expect("writing to a String cannot fail");
        emit_wit_tree_fields(&mut source, &interface, request_fields, "");
        writeln!(source, "  }}\n\n  run: async func(ctx: node-context, input: {interface}-request) -> result<{interface}-request, node-error>;\n}}\n")
            .expect("writing to a String cannot fail");
        return source;
    }
    let root = compiled.request_root;
    emit_nested_wit_records(&mut source, &interface, request_fields, root);
    if operation.feed == Some(true) {
        writeln!(source, "  record {interface}-feed {{\n    cursor: option<string>,\n    limit: option<u32>,\n  }}\n").expect("writing to a String cannot fail");
    }
    if request_fields.is_empty() && operation.feed != Some(true) {
        writeln!(source, "  enum {interface}-request {{ request }}\n")
            .expect("writing to a String cannot fail");
    } else {
        writeln!(source, "  record {interface}-request {{")
            .expect("writing to a String cannot fail");
        emit_wit_tree_fields(&mut source, &interface, request_fields, root);
        if operation.feed == Some(true) {
            writeln!(source, "    feed: option<{interface}-feed>,")
                .expect("writing to a String cannot fail");
        }
        source.push_str("  }\n\n");
    }

    let details = &compiled.refusals;
    let revision = compiled.revision;
    for (literal, detail) in details {
        if !detail.required.is_empty() || !detail.optional.is_empty() {
            emit_error_detail(&mut source, literal, detail, revision);
        }
    }
    writeln!(source, "  variant {interface}-error {{").expect("writing to a String cannot fail");
    for literal in details.keys() {
        let detail = &details[literal];
        if detail.required.is_empty() && detail.optional.is_empty() {
            writeln!(source, "    {},", wit_name(literal))
                .expect("writing to a String cannot fail");
        } else {
            writeln!(
                source,
                "    {}({}-detail),",
                wit_name(literal),
                wit_name(literal)
            )
            .expect("writing to a String cannot fail");
        }
    }
    let key = request_key_field(custom_is_keyed(operation));
    writeln!(source, "  }}\n\n  record {interface}-item {{\n{key}    input: result<{interface}-request, invalid-input-detail>,\n  }}\n").expect("writing to a String cannot fail");
    if result.is_some_and(|result| {
        matches!(
            result.class,
            ResultClass::BoundedList | ResultClass::OptionalOne | ResultClass::Page
        )
    }) {
        writeln!(source, "  record {interface}-row {{").expect("writing to a String cannot fail");
        emit_record_fields(
            &mut source,
            &result.expect("bounded list result exists").fields,
            "",
            false,
        );
        if operation.feed == Some(true) {
            let key_type = compiled.feed_key.expect("compiled feed key");
            writeln!(source, "  }}\n\n  record {interface}-page {{\n    item: list<{interface}-row>,\n    next-cursor: option<string>,\n    cursor: string,\n  }}\n\n  record {interface}-change {{\n    id: {},\n    row-version: s64,\n    data: option<{interface}-row>,\n  }}\n\n  record {interface}-changes {{\n    rows: list<{interface}-change>,\n    next-cursor: string,\n    complete: bool,\n  }}\n\n  variant {interface}-result {{\n    page({interface}-page),\n    changes({interface}-changes),", wit_type(key_type)).expect("writing to a String cannot fail");
        } else {
            let carrier = if result.expect("result exists").class == ResultClass::BoundedList {
                format!("rows: list<{interface}-row>")
            } else {
                format!("value: option<{interface}-row>")
            };
            writeln!(
                source,
                "  }}\n\n  record {interface}-result {{\n    {carrier},"
            )
            .expect("writing to a String cannot fail");
        }
    } else {
        writeln!(source, "  record {interface}-result {{")
            .expect("writing to a String cannot fail");
        if let Some(result) = result {
            emit_record_fields(&mut source, &result.fields, "", false);
        }
    }
    writeln!(source, "  }}\n\n  record {interface}-outcome {{\n{key}    outcome: result<{interface}-result, {interface}-error>,\n  }}\n").expect("writing to a String cannot fail");
    writeln!(source, "  run: async func(ctx: node-context, input: list<{interface}-item>) -> result<list<{interface}-outcome>, node-error>;\n}}\n").expect("writing to a String cannot fail");
    source
}

fn emit_record_fields(
    source: &mut String,
    fields: &[ContractFieldDeclaration],
    prefix: &str,
    omit_lines: bool,
) {
    for field in fields {
        let Some(path) = field.path.strip_prefix(prefix) else {
            continue;
        };
        let list = result_list_member(path);
        let path = list.unwrap_or(path);
        if path == "request_id"
            || path.contains("[]")
            || path.contains('.')
            || (omit_lines && path == "line")
        {
            continue;
        }
        let mut ty = wit_type(field.ty);
        if field.nullable {
            ty = format!("option<{ty}>");
        }
        if list.is_some() {
            ty = format!("list<{ty}>");
        }
        writeln!(source, "    {}: {ty},", wit_name(path)).expect("writing to a String cannot fail");
    }
}

/// The member of a result field that is a list of one scalar type, such as
/// `movement_ids` for `movement_ids[]`. The declared type and nullability
/// are those of each element.
fn result_list_member(path: &str) -> Option<&str> {
    path.strip_suffix("[]")
        .filter(|member| !member.contains('.') && !member.contains("[]"))
}

fn emit_nested_wit_records(source: &mut String, interface: &str, fields: &[FieldIr], root: &str) {
    for field in fields.iter().filter(|field| !field.children.is_empty()) {
        emit_nested_wit_records(source, interface, &field.children, root);
        let suffix = field_type_suffix(&field.path, root);
        writeln!(source, "  record {interface}-{suffix} {{")
            .expect("writing to a String cannot fail");
        emit_wit_tree_fields(source, interface, &field.children, root);
        source.push_str("  }\n\n");
    }
}

fn emit_wit_tree_fields(source: &mut String, interface: &str, fields: &[FieldIr], root: &str) {
    for field in fields {
        let local = field
            .path
            .trim_start_matches(root)
            .rsplit('.')
            .next()
            .expect("declared field path has a member");
        let name = local.trim_end_matches("[]");
        let mut ty = if field.children.is_empty() {
            wit_field_type(&field.type_name)
        } else {
            format!("{interface}-{}", field_type_suffix(&field.path, root))
        };
        if field.type_name == "array" || local.ends_with("[]") {
            ty = format!("list<{ty}>");
        }
        if field.nullable {
            ty = format!("option<{ty}>");
        }
        writeln!(source, "    {}: {ty},", wit_name(name)).expect("writing to a String cannot fail");
    }
}

fn field_type_suffix(path: &str, root: &str) -> String {
    wit_name(
        path.trim_start_matches(root)
            .trim_end_matches("[]")
            .replace("[]", "")
            .replace('.', "-")
            .as_str(),
    )
}

fn wit_field_type(type_name: &str) -> String {
    match type_name {
        "boolean" => "bool",
        "int32" => "s32",
        "int64" => "s64",
        "float64" => "f64",
        "bytes" => "list<u8>",
        "json" => "json-value",
        _ => "string",
    }
    .to_owned()
}

fn emit_error_detail(
    source: &mut String,
    literal: &str,
    detail: &OperationErrorDetailDeclaration,
    revision: Option<SqlValueType>,
) {
    writeln!(source, "  record {}-detail {{", wit_name(literal))
        .expect("writing to a String cannot fail");
    for key in &detail.required {
        writeln!(
            source,
            "    {}: {},",
            detail_name(*key),
            detail_type(*key, revision)
        )
        .expect("writing to a String cannot fail");
    }
    for key in &detail.optional {
        writeln!(
            source,
            "    {}: option<{}>,",
            detail_name(*key),
            detail_type(*key, revision)
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str("  }\n\n");
}

fn detail_name(key: OperationErrorDetailKey) -> &'static str {
    match key {
        OperationErrorDetailKey::Field => "field",
        OperationErrorDetailKey::Id => "id",
        OperationErrorDetailKey::ExpectedRowVersion => "expected-row-version",
        OperationErrorDetailKey::ObservedRowVersion => "observed-row-version",
        OperationErrorDetailKey::Minimum => "minimum",
        OperationErrorDetailKey::Maximum => "maximum",
        OperationErrorDetailKey::Observed => "observed",
        OperationErrorDetailKey::Constraint => "constraint",
        OperationErrorDetailKey::Operation => "operation",
    }
}

/// A revision detail member carries the width of the revision it reports, and
/// an operation that reports none keeps the wider member. A bound or observed
/// value is a decimal string, spelled as a numeric is, so a fractional
/// quantity and an integer count share one member.
fn detail_type(key: OperationErrorDetailKey, revision: Option<SqlValueType>) -> &'static str {
    match key {
        OperationErrorDetailKey::ExpectedRowVersion
        | OperationErrorDetailKey::ObservedRowVersion
            if revision == Some(SqlValueType::Int32) =>
        {
            "s32"
        }
        OperationErrorDetailKey::ExpectedRowVersion
        | OperationErrorDetailKey::ObservedRowVersion => "s64",
        OperationErrorDetailKey::Minimum
        | OperationErrorDetailKey::Maximum
        | OperationErrorDetailKey::Observed
        | OperationErrorDetailKey::Field
        | OperationErrorDetailKey::Id
        | OperationErrorDetailKey::Constraint
        | OperationErrorDetailKey::Operation => "string",
    }
}

/// The revision width of a CRUD operation, or none when it declares no revision.
fn wit_type(ty: SqlValueType) -> String {
    let value = match ty {
        SqlValueType::Boolean => "bool",
        SqlValueType::Int32 => "s32",
        SqlValueType::Int64 => "s64",
        SqlValueType::Float64 => "f64",
        SqlValueType::Text
        | SqlValueType::Numeric
        | SqlValueType::Timestamptz
        | SqlValueType::Uuid => "string",
        SqlValueType::Json => "json-value",
        SqlValueType::Bytes => "list<u8>",
    };
    value.to_owned()
}

pub(super) fn wit_name(value: &str) -> String {
    let name = value.replace('_', "-");
    if matches!(
        name.as_str(),
        "as" | "async"
            | "constructor"
            | "enum"
            | "flags"
            | "func"
            | "include"
            | "interface"
            | "list"
            | "option"
            | "package"
            | "record"
            | "resource"
            | "result"
            | "static"
            | "tuple"
            | "type"
            | "use"
            | "variant"
            | "with"
            | "world"
    ) {
        format!("%{name}")
    } else {
        name
    }
}

fn artifact_name(value: &str) -> String {
    authored_name(&rust_identifier(value).expect("validated operation name has a Rust spelling"))
        .to_owned()
}

fn emit_custom_export(
    local_name: &str,
    compiled: &super::operations::CustomDescription<'_>,
) -> String {
    let operation = compiled.declaration;
    let name = rust_type_identifier(local_name);
    let mut source = support_header();
    if let Some(result) = &compiled.result {
        let (min, max) = operation
            .input
            .envelope
            .as_ref()
            .map_or((1, 100), |b| (b.minimum, b.maximum));
        let (min, max) = if operation.feed == Some(true) {
            (1, 1)
        } else {
            (min, max)
        };
        source.push_str(&export_prelude(
            &format!("{name}Item"),
            min,
            max,
            custom_is_keyed(operation),
        ));
        emit_invalid_detail(&mut source, compiled);
        source.push_str(&emit_custom_normalizer(&name, compiled));
        source.push_str(&emit_handler(&name, custom_is_keyed(operation)));
        source.push_str(&emit_row_adapter(
            result
                .fields
                .iter()
                .map(|f| (f.path.as_str(), f.ty, f.nullable)),
        ));
        source.push_str(&emit_error_mapper(
            &format!("{name}Error"),
            compiled
                .refusals
                .iter()
                .map(|(code, detail)| (code.as_str(), detail)),
            compiled.revision,
            &std::collections::BTreeMap::new(),
        ));
        source.push_str(&emit_export_adapter(
            &name,
            false,
            operation.has_host_transaction(),
        ));
    } else {
        source
            .push_str("fn invalid(field: &'static str) -> InputError { InputError::new(field) }\n");
        writeln!(
            source,
            "pub(crate) fn normalize(request: &mut contract::{name}Request) -> Result<(), InputError> {{"
        )
        .expect("write typed validation");
        emit_tree_validation(&mut source, &compiled.request, "request", 4);
        source.push_str("Ok(())\n}\n");
        source.push_str(&emit_export_adapter(&name, true, false));
    }
    source
}
fn emit_custom_normalizer(
    type_name: &str,
    compiled: &super::operations::CustomDescription<'_>,
) -> String {
    let operation = compiled.declaration;
    let fields = &compiled.request;
    let mut source = format!(
        "#[allow(clippy::unnecessary_wraps)]\npub(crate) fn normalize(request: &mut contract::{type_name}Request) -> Result<(), contract::InvalidInputDetail> {{\n"
    );
    source.push_str("    let _ = &request;\n");
    emit_tree_validation(&mut source, fields, "request", 4);
    if operation.lists.is_some()
        && let Some(field) = fields
            .iter()
            .find(|field| field.path == "limit" || field.path == "value.limit")
    {
        let name = binding_identifier("limit").expect("limit is a Rust member");
        if field.type_name == "int32" || field.type_name == "int64" {
            let check = if field.required && !field.nullable {
                format!(
                    "if !(1..=100).contains(&request.{name}) {{ return Err(invalid(\"limit\")); }}"
                )
            } else {
                format!(
                    "if request.{name}.is_some_and(|limit| !(1..=100).contains(&limit)) {{ return Err(invalid(\"limit\")); }}"
                )
            };
            writeln!(source, "    {check}").expect("writing to a String cannot fail");
        }
    }

    if operation.feed == Some(true) {
        source.push_str("    if let Some(part) = &mut request.feed { let limit = *part.limit.get_or_insert(100); if !(1..=100).contains(&limit) { return Err(invalid(\"feed.limit\")); } }\n");
    }
    source.push_str("    Ok(())\n}\n\n");
    source
}

fn emit_tree_validation(source: &mut String, fields: &[FieldIr], access: &str, indentation: usize) {
    let indent = " ".repeat(indentation);
    for field in fields {
        let member = binding_identifier(
            field
                .path
                .rsplit('.')
                .next()
                .expect("declared field has a member")
                .trim_end_matches("[]"),
        )
        .expect("validated field has a Rust name");
        let field_access = format!("{access}.{member}");
        if !field.children.is_empty() {
            if field.type_name == "array" || field.path.ends_with("[]") {
                if let Some(minimum) = field.minimum {
                    if minimum == 1 {
                        writeln!(
                            source,
                            "{indent}if {field_access}.is_empty() {{ return Err(invalid({:?})); }}",
                            field.path
                        )
                        .expect("writing to a String cannot fail");
                    } else {
                        writeln!(source, "{indent}if {field_access}.len() < {minimum} {{ return Err(invalid({:?})); }}", field.path)
                            .expect("writing to a String cannot fail");
                    }
                }
                if let Some(maximum) = field.maximum {
                    writeln!(source, "{indent}if {field_access}.len() > {maximum} {{ return Err(invalid({:?})); }}", field.path)
                        .expect("writing to a String cannot fail");
                }
                writeln!(source, "{indent}for value in &mut {field_access} {{")
                    .expect("writing to a String cannot fail");
                emit_tree_validation(source, &field.children, "value", indentation + 4);
                writeln!(source, "{indent}}}").expect("writing to a String cannot fail");
            } else {
                emit_tree_validation(source, &field.children, &field_access, indentation);
            }
            continue;
        }
        let mut conditions = Vec::new();
        if let Some(condition) = scalar_refusal(&field.type_name, "value") {
            conditions.push(condition);
        }
        if !field.values.is_empty() {
            conditions.push(format!("!{:?}.contains(&value.as_str())", field.values));
        }
        if conditions.is_empty() {
            continue;
        }
        let condition = conditions.join(" || ");
        let path = &field.path;
        if field.nullable {
            // One let chain, so clippy::collapsible_if accepts the generated code.
            writeln!(
                source,
                "{indent}if let Some(value) = &mut {field_access} && ({condition}) {{ return Err(invalid({path:?})); }}"
            )
            .expect("writing to a String cannot fail");
        } else {
            writeln!(
                source,
                "{indent}{{ let value = &mut {field_access}; if {condition} {{ return Err(invalid({path:?})); }} }}"
            )
            .expect("writing to a String cannot fail");
        }
    }
}

fn emit_invalid_detail(source: &mut String, compiled: &super::operations::CustomDescription<'_>) {
    let detail = &compiled.refusals["invalid_input"];
    source.push_str("fn invalid(field: &str) -> contract::InvalidInputDetail {\n    contract::InvalidInputDetail {\n");
    for key in &detail.required {
        let name = detail_name(*key).replace('-', "_");
        if *key == OperationErrorDetailKey::Field {
            writeln!(source, "        {name}: field.to_owned(),")
                .expect("writing to a String cannot fail");
        }
    }
    for key in &detail.optional {
        writeln!(
            source,
            "        {}: None,",
            detail_name(*key).replace('-', "_")
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str("    }\n}\n\n");
}
