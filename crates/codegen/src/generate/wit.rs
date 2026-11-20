//! Typed component contracts generated from operation declarations.

use wamn_scalar::SqlValueType;
use wit_encoder::{
    Field, Ident, Interface, Package, PackageName, StandaloneFunc, Type, TypeDef, Use, VariantCase,
};

use super::{BTreeMap, Column, insert_bytes};
use wamn_catalog::manifest::{
    AccessOperationErrorLiteral, ContractFieldDeclaration, CrudAction, CustomOperationDeclaration,
    OperationDeclaration, OperationErrorDetailDeclaration, ResultClass,
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
