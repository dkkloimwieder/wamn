//! Typed component contracts generated from operation declarations.

use std::fmt::Write as _;
use wamn_scalar::SqlValueType;
use wit_encoder::{
    Field, Ident, Interface, Package, PackageName, StandaloneFunc, Type, TypeDef, Use, VariantCase,
};

use super::{BTreeMap, Column, insert_bytes};
use crate::client_ir::FieldIr;
use wamn_catalog::manifest::{
    AccessOperationErrorLiteral, ContractFieldDeclaration, CrudAction, CustomOperationDeclaration,
    OperationDeclaration, OperationErrorDetailDeclaration, PackageManifest, ResultClass,
};
use wamn_catalog::manifest::{FilterMatch, OperationErrorDetailKey};
use wamn_catalog::manifest::{
    ManifestError as GenerateError, ManifestErrorType as GenerateErrorType,
};

pub(super) const CRUD_REQUEST_ELEMENTS_MINIMUM: u32 = 1;
pub(super) const CRUD_REQUEST_ELEMENTS_MAXIMUM: u32 = 100;

/// Emit WIT for every declared model operation.
pub(super) fn emit_model_wit(
    files: &mut BTreeMap<String, Vec<u8>>,
    compiled: &super::operations::ModelDescription<'_>,
) -> Result<(), GenerateError> {
    let namespace = wit_ident(compiled.package.id.as_str());
    let model = wit_ident(compiled.name.as_str());
    let path = format!(
        "generated/wit/deps/{}-{model}/package.wit",
        namespace.raw_name()
    );
    let version = compiled
        .package
        .version
        .as_str()
        .parse()
        .map_err(|source| {
            GenerateError::with_source(
                GenerateErrorType::InvalidIdentity,
                format!("invalid WIT package version {}", compiled.package.version),
                source,
            )
        })?;
    let mut package = Package::new(PackageName::new(
        namespace.to_string(),
        model,
        Some(version),
    ));
    for (action, facts) in &compiled.operations {
        package.interface(crud_interface(
            *action,
            &compiled.shape,
            facts,
            compiled.feed,
        ));
    }
    let source = format!("// @generated from operation declarations; do not edit.\n\n{package}");
    insert_bytes(files, &path, source.into_bytes())
}

fn crud_interface(
    action: CrudAction,
    table: &super::operations::ModelShape<'_>,
    facts: &super::operations::ModelOperation<'_>,
    feed: bool,
) -> Interface {
    let name = action.as_str();
    let operation = facts.request;
    let mut interface = Interface::new(name);
    let mut context = Use::new("wamn:node/types@0.1.0");
    for ty in ["node-context", "node-error", "json-value"] {
        context.item(ty, None);
    }
    interface.use_(context);

    let mut request = Vec::new();
    match action {
        CrudAction::Get | CrudAction::Delete | CrudAction::Update => {
            request.push(Field::new("id", model_type(table.key.column_type())));
        }
        CrudAction::Create => {
            if !table.generated_id {
                request.push(Field::new("id", Type::String));
            }
            request.push(Field::new("idempotency-key", Type::String));
            request.extend(writable_fields(table, operation));
        }
        CrudAction::Query => {
            query_request_fields(&mut interface, &mut request, table, operation, feed);
        }
    }
    if matches!(action, CrudAction::Delete | CrudAction::Update) {
        let revision = operation
            .revision_field
            .as_deref()
            .expect("validated mutation revision exists");
        request.push(Field::new(
            format!("expected-{}", wit_ident(revision).raw_name()),
            model_type(model_column(table, revision).column_type()),
        ));
    }
    if action == CrudAction::Update {
        interface.type_def(TypeDef::record(
            "update-change",
            writable_fields(table, operation),
        ));
        request.push(Field::new("change", Type::named("update-change")));
    }
    interface.type_def(TypeDef::record(format!("{name}-request"), request));
    emit_operation_errors(&mut interface, name, &facts.refusals, facts.revision);

    let (input, output) = if action == CrudAction::Query {
        emit_query_page(&mut interface, &facts.result.fields, feed, table);
        (Type::named("query-request"), Type::named("query-outcome"))
    } else {
        let key = || crud_is_keyed(action).then(|| Field::new("request-id", Type::String));
        let item = format!("{name}-item");
        interface.type_def(TypeDef::record(
            item.clone(),
            key().into_iter().chain([Field::new(
                "input",
                Type::result_both(
                    Type::named(format!("{name}-request")),
                    Type::named("invalid-input-detail"),
                ),
            )]),
        ));
        let result = format!("{name}-result");
        if action == CrudAction::Update {
            interface.type_def(TypeDef::record(
                result.clone(),
                result_fields(&facts.result.fields),
            ));
        } else {
            let row = format!("{name}-row");
            interface.type_def(TypeDef::record(
                row.clone(),
                result_fields(&facts.result.fields),
            ));
            let row = Type::named(row);
            let value = match operation.result {
                ResultClass::One => row,
                ResultClass::OptionalOne => Type::option(row),
                ResultClass::BoundedList | ResultClass::Page => Type::list(row),
            };
            interface.type_def(TypeDef::record(result.clone(), [("value", value)]));
        }
        let outcome = format!("{name}-outcome");
        interface.type_def(TypeDef::record(
            outcome.clone(),
            key().into_iter().chain([Field::new(
                "outcome",
                Type::result_both(Type::named(result), Type::named(format!("{name}-error"))),
            )]),
        ));
        (
            Type::list(Type::named(item)),
            Type::list(Type::named(outcome)),
        )
    };
    let mut run = StandaloneFunc::new("run", true);
    run.params_mut().push("ctx", Type::named("node-context"));
    run.params_mut().push("input", input);
    run.set_result(Some(Type::result_both(output, Type::named("node-error"))));
    interface.function(run);
    interface
}

fn query_request_fields(
    interface: &mut Interface,
    request: &mut Vec<Field>,
    table: &super::operations::ModelShape<'_>,
    operation: &OperationDeclaration,
    feed: bool,
) {
    for filter in &operation.filters {
        let field = wit_ident(&filter.field);
        let value = match filter.match_mode {
            FilterMatch::Range => {
                let range = format!("{}-range", field.raw_name());
                let column = model_column(table, &filter.field);
                let fields = ["min", "max"]
                    .map(|name| (name, Type::option(model_type(column.column_type()))));
                interface.type_def(TypeDef::record(range.clone(), fields));
                Type::named(range)
            }
            FilterMatch::IsNull => Type::Bool,
            _ => Type::list(model_type(model_column(table, &filter.field).column_type())),
        };
        request.push(Field::new(field, Type::option(value)));
    }
    if operation.search.is_some() {
        request.push(Field::new("search", Type::option(Type::String)));
    }
    if operation.sort.is_some() {
        request.push(Field::new("sort-field", Type::option(Type::String)));
        request.push(Field::new("sort-direction", Type::option(Type::String)));
    }
    if operation.pagination.is_some() {
        request.push(Field::new("cursor", Type::option(Type::String)));
    }
    if operation.limit.is_some() {
        request.push(Field::new("limit", Type::option(Type::U32)));
    }
    if feed {
        let mut query_feed = TypeDef::record(
            "query-feed",
            [
                ("cursor", Type::option(Type::String)),
                ("limit", Type::option(Type::U32)),
            ],
        );
        query_feed.set_docs(Some("The feed part: present, the read returns the changes since the\ncursor instead of a page."));
        interface.type_def(query_feed);
        request.push(Field::new("feed", Type::option(Type::named("query-feed"))));
    }
}

fn emit_query_page(
    interface: &mut Interface,
    fields: &[ContractFieldDeclaration],
    feed: bool,
    table: &super::operations::ModelShape<'_>,
) {
    interface.type_def(TypeDef::record("query-row", result_fields(fields)));
    let page = [
        ("item", Type::list(Type::named("query-row"))),
        ("next-cursor", Type::option(Type::String)),
    ];
    let result = if feed {
        let mut page = TypeDef::record(
            "query-page",
            page.into_iter().chain([("cursor", Type::String)]),
        );
        page.set_docs(Some("One page. A next cursor means more rows follow. The cursor starts\nthe change feed at this page's snapshot."));
        interface.type_def(page);

        let mut change = TypeDef::record(
            "query-change",
            [
                ("id", model_type(table.key.column_type())),
                ("row-version", Type::S64),
                ("data", Type::option(Type::named("query-row"))),
            ],
        );
        change.set_docs(Some("One changed row. A change with no data is a row that was deleted or\nleft the target of the read."));
        interface.type_def(change);

        let mut changes = TypeDef::record(
            "query-changes",
            [
                ("rows", Type::list(Type::named("query-change"))),
                ("next-cursor", Type::String),
                ("complete", Type::Bool),
            ],
        );
        changes.set_docs(Some("The changes since a cursor, one per row. Not complete means more\nchanges follow the next cursor."));
        interface.type_def(changes);

        TypeDef::variant(
            "query-result",
            [
                VariantCase::value("page", Type::named("query-page")),
                VariantCase::value("changes", Type::named("query-changes")),
            ],
        )
    } else {
        let mut result = TypeDef::record("query-result", page);
        result.set_docs(Some("One page. A cursor means more rows follow."));
        result
    };
    interface.type_def(result);
    interface.type_def(TypeDef::record(
        "query-outcome",
        [(
            "outcome",
            Type::result_both(Type::named("query-result"), Type::named("query-error")),
        )],
    ));
}

fn writable_fields<'a>(
    table: &'a super::operations::ModelShape<'_>,
    operation: &'a OperationDeclaration,
) -> impl Iterator<Item = Field> + 'a {
    operation.writable_fields.iter().map(move |field| {
        let ty = model_type(model_column(table, field).column_type());
        Field::new(wit_ident(field), Type::option(Type::option(ty)))
    })
}

fn result_fields(fields: &[ContractFieldDeclaration]) -> impl Iterator<Item = Field> + '_ {
    fields.iter().map(|field| {
        let ty = model_type(field.ty);
        let ty = if field.nullable { Type::option(ty) } else { ty };
        Field::new(wit_ident(&field.path), ty)
    })
}

fn emit_operation_errors(
    interface: &mut Interface,
    name: &str,
    details: &BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
    revision: Option<SqlValueType>,
) {
    let cases = details.iter().map(|(literal, detail)| {
        let literal = wit_ident(access_error_literal(*literal));
        if detail.required.is_empty() && detail.optional.is_empty() {
            VariantCase::empty(literal)
        } else {
            let record_name = format!("{}-detail", literal.raw_name());
            let fields = detail
                .required
                .iter()
                .map(|key| (*key, false))
                .chain(detail.optional.iter().map(|key| (*key, true)))
                .map(|(key, optional)| {
                    let ty = match key {
                        OperationErrorDetailKey::ExpectedRowVersion
                        | OperationErrorDetailKey::ObservedRowVersion => match revision {
                            Some(SqlValueType::Int32) => Type::S32,
                            _ => Type::S64,
                        },
                        _ => Type::String,
                    };
                    let ty = if optional { Type::option(ty) } else { ty };
                    Field::new(detail_name(key), ty)
                });
            interface.type_def(TypeDef::record(record_name.clone(), fields));
            VariantCase::value(literal, Type::named(record_name))
        }
    });
    let error = TypeDef::variant(format!("{name}-error"), cases);
    interface.type_def(error);
}

fn model_type(ty: SqlValueType) -> Type {
    match ty {
        SqlValueType::Boolean => Type::Bool,
        SqlValueType::Int32 => Type::S32,
        SqlValueType::Int64 => Type::S64,
        SqlValueType::Float64 => Type::F64,
        SqlValueType::Text
        | SqlValueType::Numeric
        | SqlValueType::Timestamptz
        | SqlValueType::Uuid => Type::String,
        SqlValueType::Json => Type::named("json-value"),
        SqlValueType::Bytes => Type::list(Type::U8),
    }
}

pub(super) fn wit_ident(value: &str) -> Ident {
    Ident::new(value.replace('_', "-"))
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
