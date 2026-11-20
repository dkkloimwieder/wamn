//! Typed component contracts generated from operation declarations.

use std::fmt::Write as _;
use wamn_scalar::SqlValueType;

use super::{BTreeMap, Column, insert_bytes};
use crate::client_ir::FieldIr;
use wamn_catalog::manifest::{
    AccessOperationErrorLiteral, ContractFieldDeclaration, CrudAction, CustomOperationDeclaration,
    OperationDeclaration, OperationErrorDetailDeclaration, PackageManifest, ResultClass,
};
use wamn_catalog::manifest::{FilterMatch, OperationErrorDetailKey};
use wamn_catalog::manifest::ManifestError as GenerateError;

pub(super) const CRUD_REQUEST_ELEMENTS_MINIMUM: u32 = 1;
pub(super) const CRUD_REQUEST_ELEMENTS_MAXIMUM: u32 = 100;

/// Emit WIT for every declared model operation.
pub(super) fn emit_model_wit(
    files: &mut BTreeMap<String, Vec<u8>>,
    compiled: &super::operations::ModelDescription<'_>,
) -> Result<(), GenerateError> {
    let package = compiled.package.id.as_str().replace('_', "-");
    let model_name = compiled.name;
    let directory = format!(
        "generated/wit/deps/{package}-{}",
        wit_name(model_name.as_str())
    );
    insert_bytes(
        files,
        &format!("{directory}/package.wit"),
        emit_model_package_wit(compiled).into_bytes(),
    )
}

fn emit_model_package_wit(compiled: &super::operations::ModelDescription<'_>) -> String {
    let package = compiled.package.id.as_str().replace('_', "-");
    let mut source = format!(
        "package {package}:{}@{};\n\n",
        wit_name(compiled.name.as_str()),
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
    class: wamn_catalog::manifest::ResultClass,
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
        wamn_catalog::manifest::ResultClass::One => format!("{name}-row"),
        wamn_catalog::manifest::ResultClass::OptionalOne => format!("option<{name}-row>"),
        wamn_catalog::manifest::ResultClass::BoundedList | wamn_catalog::manifest::ResultClass::Page => {
            format!("list<{name}-row>")
        }
    };
    writeln!(source, "  record {name}-result {{\n    value: {carrier},")
        .expect("writing to a String cannot fail");
    if action == CrudAction::Query && class == wamn_catalog::manifest::ResultClass::Page {
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

pub(super) fn model_column<'a>(
    table: &'a super::operations::ModelShape<'_>,
    field: &str,
) -> &'a Column {
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

/// Whether the request elements of a generated CRUD operation carry a `request_id`.
///
/// A read carries none (`docs/architecture/execution.md`): its one request element
/// travels in a GET query string, and its outcomes match its request elements by list
/// position.
pub(super) fn crud_is_keyed(action: CrudAction) -> bool {
    !matches!(action, CrudAction::Get | CrudAction::Query)
}

/// Whether the request elements of a custom operation carry a `request_id`: every type
/// but a projection, which is a read.
pub(super) fn custom_is_keyed(operation: &CustomOperationDeclaration) -> bool {
    operation.type_ != wamn_catalog::manifest::CustomOperationType::Projection
}

/// The WIT field that keys a request element or an outcome, or nothing for a read.
fn request_key_field(keyed: bool) -> &'static str {
    if keyed {
        "    request-id: string,\n"
    } else {
        ""
    }
}

/// Emit the WIT package for a custom operation group.
pub(super) fn emit_custom_operation_wit(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    group: &str,
    compiled: &BTreeMap<
        wamn_catalog::local_operation::LocalOperation,
        super::operations::CustomDescription<'_>,
    >,
) -> Result<(), GenerateError> {
    let package = manifest.package.id.as_str().replace('_', "-");
    let version = &manifest.package.version;
    let directory = format!("generated/wit/deps/{package}-{}", wit_name(group));
    let package_path = format!("{directory}/package.wit");
    let group_marker = manifest
        .custom_operations
        .keys()
        .find_map(|candidate| {
            candidate
                .as_str()
                .strip_prefix(&format!("{group}."))
                .map(|name| format!("interface {} {{", wit_name(name)))
        })
        .expect("current custom operation belongs to its group");
    let group_is_emitted = files
        .get(&package_path)
        .is_some_and(|existing| String::from_utf8_lossy(existing).contains(&group_marker));
    if !group_is_emitted {
        let source = emit_custom_group(&manifest.package.id, version, group, manifest, compiled);
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
    Ok(())
}

fn emit_custom_group(
    package: &wamn_catalog::PackageName,
    version: &wamn_catalog::package_version::PackageVersion,
    group: &str,
    manifest: &PackageManifest,
    compiled: &BTreeMap<
        wamn_catalog::local_operation::LocalOperation,
        super::operations::CustomDescription<'_>,
    >,
) -> String {
    let mut source = format!(
        "package {}:{}@{version};\n\n",
        package.as_str().replace('_', "-"),
        wit_name(group)
    );
    for (operation_name, facts) in compiled {
        let Some(local_name) = operation_name.as_str().strip_prefix(&format!("{group}.")) else {
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
    package: &wamn_catalog::PackageName,
    version: &wamn_catalog::package_version::PackageVersion,
    group: &str,
    local_name: &str,
    dependency: &wamn_catalog::manifest::BaseDependencyRequirement,
    _operation: &CustomOperationDeclaration,
) -> String {
    let dependency_package = dependency.package.as_str().replace('_', "-");
    let interface = wit_name(local_name);
    format!(
        "package {}:{}@{version};\n\ninterface {interface} {{\n  use wamn:node/types@0.1.0.{{node-context, node-error, json-value}};\n  use {dependency_package}:{}/{interface}@{}.{{{interface}-item, {interface}-outcome}};\n\n  run: async func(ctx: node-context, input: list<{interface}-item>) -> result<list<{interface}-outcome>, node-error>;\n}}\n",
        package.as_str().replace('_', "-"),
        wit_name(group),
        wit_name(group),
        dependency.version
    )
}

fn emit_owned_interface(
    package: &wamn_catalog::PackageName,
    version: &wamn_catalog::package_version::PackageVersion,
    group: &str,
    local_name: &str,
    manifest: &PackageManifest,
    compiled: &super::operations::CustomDescription<'_>,
) -> String {
    let operation = compiled.declaration;
    let result = compiled.result.as_ref();
    let request_fields = &compiled.request;
    let interface = wit_name(local_name);
    let mut source = format!(
        "package {}:{}@{version};\n\n",
        package.as_str().replace('_', "-"),
        wit_name(group)
    );
    for operation_name in manifest.custom_operations.keys() {
        let Some(other_name) = operation_name.as_str().strip_prefix(&format!("{group}.")) else {
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
            emit_error_detail(&mut source, literal.as_str(), detail, revision);
        }
    }
    writeln!(source, "  variant {interface}-error {{").expect("writing to a String cannot fail");
    for literal in details.keys() {
        let detail = &details[literal];
        if detail.required.is_empty() && detail.optional.is_empty() {
            writeln!(source, "    {},", wit_name(literal.as_str()))
                .expect("writing to a String cannot fail");
        } else {
            writeln!(
                source,
                "    {}({}-detail),",
                wit_name(literal.as_str()),
                wit_name(literal.as_str())
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

pub(super) fn detail_name(key: OperationErrorDetailKey) -> &'static str {
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
