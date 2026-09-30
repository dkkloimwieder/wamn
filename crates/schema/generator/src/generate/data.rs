//! Generated data functions.
//!
//! Each model with generated operations gets `generated/data/<model>.rs`, with
//! one `pub async fn` for each operation. A function takes the request values
//! the operation's codec already checked, parses each into its one spelling,
//! runs the generated statement, and answers the contract's row or refusal.
//! The package gets one refusal type, `generated/data/error.rs`, whose detail
//! members are the ones the error contracts declare, and one
//! `generated/data/mod.rs` that the data crate includes.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::wit::access_error_literal;
use super::{
    AccessOperationErrorLiteral, AccessorBind, ColumnType, CrudAction, GenerateError,
    ModelDeclaration, OperationDeclaration, PackageManifest, Table, WamnApi, column, insert_bytes,
    query_variants, rust_identifier, rust_type_identifier, sql,
};
use crate::manifest::{FilterMatch, OperationErrorDetailKey, access_operation_error_detail};

const HEADER: &str = "// @generated from the package manifest and migration IR; do not edit.\n\n";

/// Every operation kind, for the detail members its error contract declares.
const ACTIONS: [CrudAction; 5] = [
    CrudAction::Get,
    CrudAction::Query,
    CrudAction::Create,
    CrudAction::Update,
    CrudAction::Delete,
];

/// The literals a data function can answer. The codec answers
/// `idempotency_conflict` itself, from the write log, so no data function
/// produces it.
const CODES: [AccessOperationErrorLiteral; 11] = [
    AccessOperationErrorLiteral::InvalidInput,
    AccessOperationErrorLiteral::NotFound,
    AccessOperationErrorLiteral::ConcurrencyConflict,
    AccessOperationErrorLiteral::UniqueViolation,
    AccessOperationErrorLiteral::ForeignKeyViolation,
    AccessOperationErrorLiteral::CheckViolation,
    AccessOperationErrorLiteral::ExclusionViolation,
    AccessOperationErrorLiteral::Retry,
    AccessOperationErrorLiteral::Timeout,
    AccessOperationErrorLiteral::PermissionDenied,
    AccessOperationErrorLiteral::InternalError,
];

/// The Rust type a request carries a value of this column type in: the type
/// of the operation's WIT member.
fn request_type(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Boolean => "bool",
        ColumnType::Int32 => "i32",
        ColumnType::Int64 => "i64",
        ColumnType::Float64 => "f64",
        ColumnType::Bytes => "Vec<u8>",
        ColumnType::Text
        | ColumnType::Numeric
        | ColumnType::Timestamptz
        | ColumnType::Json
        | ColumnType::Uuid => "String",
    }
}

/// The expression that turns one request value into its statement value,
/// refusing on `path` when it is not in its one spelling.
fn parse(ty: ColumnType, value: &str, path: &str) -> String {
    let parser = match ty {
        ColumnType::Uuid => "uuid",
        ColumnType::Timestamptz => "timestamptz",
        ColumnType::Numeric => "numeric",
        ColumnType::Json => "json",
        ColumnType::Boolean
        | ColumnType::Int32
        | ColumnType::Int64
        | ColumnType::Float64
        | ColumnType::Text
        | ColumnType::Bytes => return value.to_owned(),
    };
    format!("scalar::{parser}(&{value}).map_err(|Invalid| Error::invalid({path:?}))?")
}

/// The cursor key type of a sort field, the type its row member holds.
fn cursor_key_type(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Boolean => "bool",
        ColumnType::Int32 => "i32",
        ColumnType::Int64 => "i64",
        ColumnType::Text => "String",
        ColumnType::Numeric => "wamn_postgres_statements::Numeric",
        ColumnType::Timestamptz => "wamn_postgres_statements::TimestampTz",
        ColumnType::Uuid => "wamn_postgres_statements::Uuid",
        ColumnType::Float64 | ColumnType::Bytes | ColumnType::Json => {
            unreachable!("query validation admits only cursor key types as sort fields")
        }
    }
}

/// A revision as the conflict detail carries it, which is 64 bits wide.
fn widen(ty: ColumnType, value: &str) -> String {
    if ty == ColumnType::Int32 {
        format!("i64::from({value})")
    } else {
        value.to_owned()
    }
}

fn permission_constant(action: CrudAction) -> String {
    format!("{}_OPERATION", action.as_str().to_ascii_uppercase())
}

fn constraints(action: CrudAction) -> String {
    let action = action.as_str().to_ascii_uppercase();
    format!(
        "Constraints {{ unique: sql::{action}_UNIQUE_CONSTRAINTS, foreign_key: sql::{action}_FOREIGN_KEY_CONSTRAINTS, check: sql::{action}_CHECK_CONSTRAINTS, exclusion: sql::{action}_EXCLUSION_CONSTRAINTS }}"
    )
}

fn field_name(field: &str) -> String {
    rust_identifier(field).expect("model field names were validated for Rust")
}

/// Emit `generated/data/<model>.rs`.
pub(super) fn emit_model_data(
    files: &mut BTreeMap<String, Vec<u8>>,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    api: &WamnApi,
) -> Result<(), GenerateError> {
    let model_type = rust_type_identifier(model_name);
    // A read runs on the connection; a change runs in the host's transaction.
    let connection = if api
        .accessors
        .iter()
        .any(|accessor| !accessor.operation.changes_records())
    {
        "use wamn_postgres_statements::Connection;\n"
    } else {
        ""
    };
    let mut source = String::from(HEADER);
    writeln!(
        source,
        "// The generated `{model_name}` operations.\n\n\
         #[allow(unused_imports)]\n\
         use wamn_data_access::{{Direction, Invalid, Page, cursor, scalar}};\n\
         {connection}\n\
         #[allow(unused_imports)]\n\
         use super::error::{{Constraints, Error}};\n\n\
         /// The statement accessors of the model.\n\
         pub mod sql {{\n    include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../generated/wamn/{model_name}.rs\"));\n}}\n\n\
         pub use sql::{model_type}Row;"
    )
    .expect("writing to a String cannot fail");
    for row in &api.operation_rows {
        writeln!(source, "pub use sql::{};", row.name).expect("writing to a String cannot fail");
    }
    source.push('\n');
    for (action, operation) in &model.operations {
        writeln!(
            source,
            "const {}: &str = {:?};",
            permission_constant(*action),
            operation.permission
        )
        .expect("writing to a String cannot fail");
    }
    source.push('\n');
    for (action, operation) in &model.operations {
        match action {
            CrudAction::Get => emit_get(&mut source, model_name, &model_type),
            CrudAction::Query => emit_query(&mut source, model_name, &model_type, table, operation),
            CrudAction::Create => {
                emit_create(&mut source, model_name, &model_type, table, operation, api);
            }
            CrudAction::Update => {
                emit_update(&mut source, model_name, &model_type, table, operation);
            }
            CrudAction::Delete => {
                emit_delete(&mut source, model_name, &model_type, table, operation);
            }
        }
    }
    while source.ends_with("\n\n") {
        source.pop();
    }
    insert_bytes(
        files,
        &format!("generated/data/{model_name}.rs"),
        source.into_bytes(),
    )
}

fn statement_error(action: CrudAction, constraints_expression: &str) -> String {
    format!(
        "|error| Error::from_statement(&error, &{constraints_expression}, {})",
        permission_constant(action)
    )
}

fn emit_get(source: &mut String, model_name: &str, model_type: &str) {
    let refuse = statement_error(CrudAction::Get, "Constraints::NONE");
    writeln!(
        source,
        "/// `{model_name}.get`: load one row by id.\n\
         ///\n/// # Errors\n///\n/// [`Error`] carrying the literal the operation contract declares.\n\
         pub async fn get(connection: &mut Connection, id: &str) -> Result<{model_type}Row, Error> {{\n\
         let id = scalar::uuid(id).map_err(|Invalid| Error::invalid(\"id\"))?;\n\
         sql::get(connection, id.clone()).await.map_err({refuse})?.ok_or_else(|| Error::not_found(&id))\n\
         }}\n"
    )
    .expect("writing to a String cannot fail");
}

fn emit_create(
    source: &mut String,
    model_name: &str,
    model_type: &str,
    table: &Table,
    operation: &OperationDeclaration,
    api: &WamnApi,
) {
    let accessor = api
        .accessors
        .iter()
        .find(|accessor| accessor.operation == CrudAction::Create)
        .expect("a create has its accessor");
    let parameters = operation
        .writable_fields
        .iter()
        .map(|field| {
            let column = column(table, field).expect("validation resolved the column");
            format!(
                "{}: Option<{}>",
                field_name(field),
                request_type(column.column_type())
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut body = String::new();
    for (field, bind) in operation.writable_fields.iter().zip(&accessor.binds) {
        let name = field_name(field);
        emit_value(&mut body, &name, field, bind, &name);
    }
    let arguments = operation
        .writable_fields
        .iter()
        .map(|field| field_name(field))
        .collect::<Vec<_>>()
        .join(", ");
    let refuse = statement_error(CrudAction::Create, &constraints(CrudAction::Create));
    writeln!(
        source,
        "/// `{model_name}.create`: create one row in the transaction the host began\n\
         /// for the operation.\n\
         ///\n/// # Errors\n///\n/// [`Error`] carrying the literal the operation contract declares.\n\
         #[allow(clippy::too_many_arguments)]\n\
         pub async fn create(transaction: &mut wamn_postgres_statements::Transaction, {parameters}) -> Result<{model_type}Row, Error> {{\n\
         {body}\
         sql::create(transaction, {arguments}).await.map_err({refuse})\n\
         }}\n"
    )
    .expect("writing to a String cannot fail");
}

/// Bind one optional request value, `source`, as `target`: a required bind
/// refuses when the value is absent.
fn emit_value(body: &mut String, target: &str, path: &str, bind: &AccessorBind, source: &str) {
    let ty = bind.statement_type;
    let parsed = parse(ty, "value", path);
    if bind.nullable {
        if parsed == "value" {
            return;
        }
        writeln!(
            body,
            "let {target} = match {source} {{ Some(value) => Some({parsed}), None => None }};"
        )
    } else {
        writeln!(
            body,
            "let Some(value) = {source} else {{ return Err(Error::invalid({path:?})); }};\nlet {target} = {parsed};"
        )
    }
    .expect("writing to a String cannot fail");
}

fn emit_update(
    source: &mut String,
    model_name: &str,
    model_type: &str,
    table: &Table,
    operation: &OperationDeclaration,
) {
    let revision = operation
        .revision_field
        .as_deref()
        .expect("update validation requires a revision field");
    let revision_type = column(table, revision)
        .expect("the revision field is a column")
        .column_type();
    let revision_name = field_name(revision);
    let mut parameters = format!(
        "id: &str, expected_{revision_name}: {}",
        request_type(revision_type)
    );
    let mut body =
        String::from("let id = scalar::uuid(id).map_err(|Invalid| Error::invalid(\"id\"))?;\n");
    let mut arguments = format!("id.clone(), expected_{revision_name}");
    for field in &operation.writable_fields {
        let column = column(table, field).expect("validation resolved the column");
        let name = field_name(field);
        let path = format!("change.{field}");
        write!(
            parameters,
            ", {name}: Option<Option<{}>>",
            request_type(column.column_type())
        )
        .expect("writing to a String cannot fail");
        if !column.nullable() {
            writeln!(
                body,
                "if matches!({name}, Some(None)) {{ return Err(Error::invalid({path:?})); }}"
            )
            .expect("writing to a String cannot fail");
        }
        let parsed = parse(column.column_type(), "value", &path);
        writeln!(body, "let {name}_present = {name}.is_some();")
            .expect("writing to a String cannot fail");
        if parsed == "value" {
            writeln!(body, "let {name} = {name}.flatten();")
        } else {
            writeln!(
                body,
                "let {name} = match {name}.flatten() {{ Some(value) => Some({parsed}), None => None }};"
            )
        }
        .expect("writing to a String cannot fail");
        write!(arguments, ", {name}_present, {name}").expect("writing to a String cannot fail");
    }
    let members = table
        .columns()
        .iter()
        .map(|column| {
            let name = field_name(column.name());
            if column.nullable() {
                format!("{name}: row.{name}")
            } else {
                format!("{name}: row.{name}.ok_or_else(Error::internal)?")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let refuse = statement_error(CrudAction::Update, &constraints(CrudAction::Update));
    writeln!(
        source,
        "/// `{model_name}.update`: change one row at the revision the caller last read.\n\
         ///\n/// # Errors\n///\n/// [`Error`] carrying the literal the operation contract declares.\n\
         #[allow(clippy::too_many_arguments)]\n\
         pub async fn update(transaction: &mut wamn_postgres_statements::Transaction, {parameters}) -> Result<{model_type}Row, Error> {{\n\
         {body}\
         let row = sql::update(transaction, {arguments}).await.map_err({refuse})?;\n\
         match row.outcome.as_deref() {{\n\
         Some({updated:?}) => Ok({model_type}Row {{ {members} }}),\n\
         Some({not_found:?}) => Err(Error::not_found(&id)),\n\
         Some({conflict:?}) => Err(Error::conflict({expected}, {observed})),\n\
         _ => Err(Error::internal()),\n\
         }}\n\
         }}\n",
        expected = widen(revision_type, &format!("expected_{revision_name}")),
        observed = widen(revision_type, &format!("row.observed_{revision_name}.ok_or_else(Error::internal)?")),
        updated = sql::OUTCOME_UPDATED,
        not_found = sql::OUTCOME_NOT_FOUND,
        conflict = sql::OUTCOME_CONCURRENCY_CONFLICT,
    )
    .expect("writing to a String cannot fail");
}

fn emit_delete(
    source: &mut String,
    model_name: &str,
    model_type: &str,
    table: &Table,
    operation: &OperationDeclaration,
) {
    let revision = operation
        .revision_field
        .as_deref()
        .expect("delete validation requires a revision field");
    let revision_type = column(table, revision)
        .expect("the revision field is a column")
        .column_type();
    let revision_name = field_name(revision);
    let refuse = statement_error(CrudAction::Delete, &constraints(CrudAction::Delete));
    writeln!(
        source,
        "/// `{model_name}.delete`: delete one row at the revision the caller last read.\n\
         ///\n/// # Errors\n///\n/// [`Error`] carrying the literal the operation contract declares.\n\
         pub async fn delete(transaction: &mut wamn_postgres_statements::Transaction, id: &str, expected_{revision_name}: {revision_rust}) -> Result<{model_type}DeleteRow, Error> {{\n\
         let id = scalar::uuid(id).map_err(|Invalid| Error::invalid(\"id\"))?;\n\
         let row = sql::delete(transaction, id.clone(), expected_{revision_name}).await.map_err({refuse})?;\n\
         match row.outcome.as_deref() {{\n\
         Some({deleted:?}) => Ok(row),\n\
         Some({not_found:?}) => Err(Error::not_found(&id)),\n\
         Some({conflict:?}) => Err(Error::conflict({expected}, {observed})),\n\
         _ => Err(Error::internal()),\n\
         }}\n\
         }}\n",
        revision_rust = request_type(revision_type),
        expected = widen(revision_type, &format!("expected_{revision_name}")),
        observed = widen(revision_type, &format!("row.observed_{revision_name}.ok_or_else(Error::internal)?")),
        deleted = sql::OUTCOME_DELETED,
        not_found = sql::OUTCOME_NOT_FOUND,
        conflict = sql::OUTCOME_CONCURRENCY_CONFLICT,
    )
    .expect("writing to a String cannot fail");
}

fn emit_query(
    source: &mut String,
    model_name: &str,
    model_type: &str,
    table: &Table,
    operation: &OperationDeclaration,
) {
    let pagination = operation
        .pagination
        .as_ref()
        .expect("query validation requires pagination");
    let tie_breaker = field_name(&pagination.tie_breaker.field);

    // The request: one member for each filter, the search, the sort, the
    // cursor, and the limit the codec filled.
    let mut members = String::new();
    let mut binds = String::new();
    let mut arguments = Vec::new();
    for filter in &operation.filters {
        let name = field_name(&filter.field);
        let (member, bind) = match filter.match_mode {
            FilterMatch::Exact | FilterMatch::Contains | FilterMatch::Prefix => (
                "Option<Vec<String>>".to_owned(),
                format!("input.{name}.as_deref().map(scalar::json_list)"),
            ),
            FilterMatch::IsNull => (
                "Option<bool>".to_owned(),
                format!("input.{name}.map(scalar::json_boolean)"),
            ),
            FilterMatch::Range => (
                "Option<wamn_data_access::Range>".to_owned(),
                format!(
                    "input.{name}.as_ref().map(|range| scalar::json_range(range.min.as_deref(), range.max.as_deref()))"
                ),
            ),
        };
        writeln!(members, "pub {name}: {member},").expect("writing to a String cannot fail");
        writeln!(binds, "let {name}_filter = {bind};").expect("writing to a String cannot fail");
        arguments.push(format!("{name}_filter"));
    }
    if operation.search.is_some() {
        members.push_str("pub search: Option<String>,\n");
        binds.push_str("let search = input.search;\n");
        arguments.push("search".to_owned());
    }
    let arguments = arguments.join(", ");

    let default = &pagination.default_sort;
    let mut arms = String::new();
    for (field, direction) in query_variants(operation) {
        let direction_name = sql::direction_name(direction);
        let direction_type = match direction {
            crate::manifest::CursorDirection::Ascending => "Direction::Ascending",
            crate::manifest::CursorDirection::Descending => "Direction::Descending",
        };
        let key_column = column(table, field).expect("validation resolved the sort field");
        let key_type = cursor_key_type(key_column.column_type());
        let key_member = field_name(field);
        let encode = if key_column.nullable() {
            format!(
                "row.{key_member}.as_ref().ok_or(Invalid).and_then(|key| cursor::encode({field:?}, {direction_type}, key, &row.{tie_breaker}))"
            )
        } else {
            format!(
                "cursor::encode({field:?}, {direction_type}, &row.{key_member}, &row.{tie_breaker})"
            )
        };
        writeln!(
            arms,
            "({field:?}, {direction_type}) => {{\n\
             let (cursor_key, cursor_id) = match input.cursor.as_deref() {{\n\
             Some(encoded) => {{ let (key, id) = cursor::decode::<{key_type}>(encoded, {field:?}, {direction_type}).map_err(|Invalid| Error::invalid(\"cursor\"))?; (Some(key), Some(id)) }}\n\
             None => (None, None),\n\
             }};\n\
             let rows = sql::query_{field}_{direction_name}(connection, {arguments}{comma} cursor_key, cursor_id, input.limit + 1).await.map_err(|error| query_statement(&error))?;\n\
             Ok(Page::new(rows, input.limit, query_statement, |row| {{\n\
             {encode}.map_err(|Invalid| Error::internal())\n\
             }}))\n\
             }}",
            comma = if arguments.is_empty() { "" } else { "," },
        )
        .expect("writing to a String cannot fail");
    }
    let default_direction = match default.direction {
        crate::manifest::CursorDirection::Ascending => "Direction::Ascending",
        crate::manifest::CursorDirection::Descending => "Direction::Descending",
    };
    if operation
        .filters
        .iter()
        .any(|filter| matches!(filter.match_mode, FilterMatch::Range))
    {
        source.push_str("pub use wamn_data_access::Range;\n\n");
    }
    writeln!(
        source,
        "/// The `{model_name}.query` request, after its codec filled the limit.\n\
         #[derive(Clone, Debug, Default, Eq, PartialEq)]\n\
         pub struct QueryInput {{\n\
         {members}\
         pub sort_field: Option<String>,\n\
         pub sort_direction: Option<String>,\n\
         pub cursor: Option<String>,\n\
         pub limit: i64,\n\
         }}\n\n\
         fn query_statement(error: &wamn_postgres_statements::StatementError) -> Error {{\n\
         Error::from_statement(error, &Constraints::NONE, {permission})\n\
         }}\n\n\
         /// `{model_name}.query`: one keyset page in the requested sort.\n\
         ///\n/// The contract's order holds: the sort and the cursor first, then the\n\
         /// statement. The codec checked the limit.\n\
         ///\n/// # Errors\n///\n/// [`Error`] carrying the literal the operation contract declares.\n\
         pub async fn query(connection: &mut Connection, input: QueryInput) -> Result<Page<{model_type}Row, Error>, Error> {{\n\
         let sort = match (input.sort_field.as_deref(), input.sort_direction.as_deref()) {{\n\
         (None, None) => ({default_field:?}, {default_direction}),\n\
         (Some(field), Some(\"ascending\")) => (field, Direction::Ascending),\n\
         (Some(field), Some(\"descending\")) => (field, Direction::Descending),\n\
         _ => return Err(Error::invalid(\"sort\")),\n\
         }};\n\
         {binds}\
         match sort {{\n\
         {arms}\n\
         _ => Err(Error::invalid(\"sort\")),\n\
         }}\n\
         }}\n",
        permission = permission_constant(CrudAction::Query),
        default_field = default.field,
    )
    .expect("writing to a String cannot fail");
}

/// Emit `generated/data/error.rs` and `generated/data/mod.rs`, when any model
/// of the package has a generated operation.
pub(super) fn emit_package_data(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
) -> Result<(), GenerateError> {
    let models = manifest
        .models
        .iter()
        .filter(|(_, model)| !model.operations.is_empty())
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Ok(());
    }
    // The refusal is the closed vocabulary, so every package spells it the
    // same way. Each literal's members are the ones its error contract
    // declares: required for every operation kind that declares the literal,
    // or optional.
    let mut declared =
        BTreeMap::<AccessOperationErrorLiteral, Vec<(OperationErrorDetailKey, bool)>>::new();
    for code in CODES {
        let details = ACTIONS.map(|action| access_operation_error_detail(action, code));
        let keys = declared.entry(code).or_default();
        for detail in &details {
            for key in detail.required.iter().chain(&detail.optional) {
                if !keys.iter().any(|(known, _)| known == key) {
                    let always = details.iter().all(|detail| detail.required.contains(key));
                    keys.push((*key, always));
                }
            }
        }
    }
    insert_bytes(
        files,
        "generated/data/error.rs",
        emit_error(&declared).into_bytes(),
    )?;

    let mut source = String::from(HEADER);
    source.push_str("// The generated data functions and their refusal.\n\n");
    source.push_str(
        "/// The one refusal of every generated operation.\npub mod error {\n    include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../generated/data/error.rs\"));\n}\n",
    );
    for (model_name, _) in &models {
        writeln!(
            source,
            "\n/// The generated `{model_name}` operations.\npub mod {model_name} {{\n    include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../generated/data/{model_name}.rs\"));\n}}"
        )
        .expect("writing to a String cannot fail");
    }
    insert_bytes(files, "generated/data/mod.rs", source.into_bytes())
}

fn key_name(key: OperationErrorDetailKey) -> &'static str {
    match key {
        OperationErrorDetailKey::Field => "field",
        OperationErrorDetailKey::Id => "id",
        OperationErrorDetailKey::ExpectedRowVersion => "expected_row_version",
        OperationErrorDetailKey::ObservedRowVersion => "observed_row_version",
        OperationErrorDetailKey::Minimum => "minimum",
        OperationErrorDetailKey::Maximum => "maximum",
        OperationErrorDetailKey::Observed => "observed",
        OperationErrorDetailKey::Constraint => "constraint",
        OperationErrorDetailKey::Operation => "operation",
    }
}

const fn revision_key(key: OperationErrorDetailKey) -> bool {
    matches!(
        key,
        OperationErrorDetailKey::ExpectedRowVersion | OperationErrorDetailKey::ObservedRowVersion
    )
}

fn emit_error(
    declared: &BTreeMap<AccessOperationErrorLiteral, Vec<(OperationErrorDetailKey, bool)>>,
) -> String {
    let variant =
        |literal: AccessOperationErrorLiteral| rust_type_identifier(access_error_literal(literal));
    let mut variants = String::new();
    let mut literals = String::new();
    let mut details = String::new();
    let mut carriers = BTreeMap::<(OperationErrorDetailKey, bool), Vec<String>>::new();
    for (literal, keys) in declared {
        let name = variant(*literal);
        writeln!(
            literals,
            "Self::{name}{} => {:?},",
            if keys.is_empty() { "" } else { " { .. }" },
            access_error_literal(*literal)
        )
        .expect("writing to a String cannot fail");
        if keys.is_empty() {
            writeln!(variants, "{name},").expect("writing to a String cannot fail");
            continue;
        }
        let members = keys
            .iter()
            .map(|(key, always)| {
                let ty = if revision_key(*key) { "i64" } else { "String" };
                if *always {
                    format!("{}: {ty}", key_name(*key))
                } else {
                    format!("{}: Option<{ty}>", key_name(*key))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(variants, "{name} {{ {members} }},").expect("writing to a String cannot fail");
        for (key, always) in keys {
            carriers
                .entry((*key, *always))
                .or_default()
                .push(format!("Self::{name} {{ {}, .. }}", key_name(*key)));
        }
    }
    // One arm for each member, over every variant that carries it the same
    // way.
    for ((key, always), patterns) in &carriers {
        let member = key_name(*key);
        let value = match (revision_key(*key), *always) {
            (true, true) => format!("Some({member}.to_string())"),
            (true, false) => format!("{member}.map(|value| value.to_string())"),
            (false, true) => format!("Some({member}.clone())"),
            (false, false) => format!("{member}.clone()"),
        };
        writeln!(
            details,
            "({}, {member:?}) => {value},",
            patterns.join(" | ")
        )
        .expect("writing to a String cannot fail");
    }
    let has = |literal| declared.contains_key(&literal);
    let optional = |literal, key| {
        declared
            .get(&literal)
            .is_some_and(|keys| keys.iter().any(|(known, always)| *known == key && !always))
    };
    let mut constructors = String::new();
    // invalid_input names the refused field, and a data function reports no
    // bound: the codec checked every bound.
    let bounds = [
        OperationErrorDetailKey::Minimum,
        OperationErrorDetailKey::Maximum,
        OperationErrorDetailKey::Observed,
    ]
    .into_iter()
    .filter(|key| optional(AccessOperationErrorLiteral::InvalidInput, *key))
    .fold(String::new(), |mut bounds, key| {
        write!(bounds, ", {}: None", key_name(key)).expect("writing to a String cannot fail");
        bounds
    });
    writeln!(
        constructors,
        "/// Refuse the value at `field`.\n#[must_use]\npub fn invalid(field: &str) -> Self {{ Self::InvalidInput {{ field: field.to_owned(){bounds} }} }}\n"
    )
    .expect("writing to a String cannot fail");
    if has(AccessOperationErrorLiteral::NotFound) {
        constructors.push_str("/// No row has this id.\n#[must_use]\npub fn not_found(id: &wamn_postgres_statements::Uuid) -> Self { Self::NotFound { field: \"id\".to_owned(), id: id.0.clone() } }\n\n");
    }
    if has(AccessOperationErrorLiteral::ConcurrencyConflict) {
        constructors.push_str("/// The row carries another revision than the caller read.\n#[must_use]\npub fn conflict(expected_row_version: i64, observed_row_version: i64) -> Self { Self::ConcurrencyConflict { expected_row_version, observed_row_version } }\n\n");
    }
    let violation = |literal, kind: &str, names: &str| {
        if has(literal) {
            format!(
                "StatementErrorType::{kind} => named(constraints.{names}, |constraint| Self::{} {{ constraint }}),\n",
                variant(literal)
            )
        } else {
            String::new()
        }
    };
    let statement_arms = [
        violation(
            AccessOperationErrorLiteral::UniqueViolation,
            "UniqueViolation",
            "unique",
        ),
        violation(
            AccessOperationErrorLiteral::ForeignKeyViolation,
            "ForeignKeyViolation",
            "foreign_key",
        ),
        violation(
            AccessOperationErrorLiteral::CheckViolation,
            "CheckViolation",
            "check",
        ),
        violation(
            AccessOperationErrorLiteral::ExclusionViolation,
            "ExclusionViolation",
            "exclusion",
        ),
    ]
    .concat();
    let named = if statement_arms.is_empty() {
        "let _ = (constraint, constraints);\n"
    } else {
        "let named = |names: &[&str], refusal: fn(String) -> Self| match constraint.filter(|name| names.contains(name)) {\nSome(constraint) => refusal(constraint.to_owned()),\nNone => Self::InternalError,\n};\n"
    };
    format!(
        "{HEADER}\
         // The one refusal of the generated operations: the contract's literal and\n\
         // the detail members its error contract declares.\n\n\
         use std::fmt;\n\n\
         use wamn_postgres_statements::{{StatementError, StatementErrorType}};\n\n\
         /// One refusal, with the detail members its literal declares.\n\
         #[derive(Clone, Debug, Eq, PartialEq)]\n\
         pub enum Error {{\n{variants}}}\n\n\
         impl Error {{\n\
         /// The operation-contract literal.\n\
         #[must_use]\n\
         pub const fn literal(&self) -> &'static str {{\n match self {{\n{literals} }}\n }}\n\n\
         /// One declared detail member, spelled as the codec reads it.\n\
         #[must_use]\n\
         pub fn detail(&self, key: &str) -> Option<String> {{\n match (self, key) {{\n{details} _ => None,\n }}\n }}\n\n\
         {constructors}\
         /// A fault the contract does not name.\n\
         #[must_use]\n\
         pub fn internal() -> Self {{ Self::InternalError }}\n\n\
         /// The one translation of a statement failure. A constraint the\n\
         /// operation does not name is an `internal_error`, as is every kind the\n\
         /// contract does not name.\n\
         #[must_use]\n\
         pub fn from_statement(error: &StatementError, constraints: &Constraints, operation: &str) -> Self {{\n\
         Self::from_parts(error.kind(), error.constraint(), constraints, operation)\n\
         }}\n\n\
         /// [`Error::from_statement`] over the kind and the constraint of a\n\
         /// statement failure.\n\
         #[must_use]\n\
         pub fn from_parts(kind: StatementErrorType, constraint: Option<&str>, constraints: &Constraints, operation: &str) -> Self {{\n\
         {named}\
         match kind {{\n\
         StatementErrorType::SerializationFailure | StatementErrorType::ConnectionUnavailable => Self::Retry,\n\
         StatementErrorType::StatementTimeout => Self::Timeout,\n\
         StatementErrorType::PermissionDenied => Self::PermissionDenied {{ operation: operation.to_owned() }},\n\
         {statement_arms}\
         _ => Self::InternalError,\n\
         }}\n\
         }}\n\
         }}\n\n\
         impl fmt::Display for Error {{\n\
         fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {{ formatter.write_str(self.literal()) }}\n\
         }}\n\n\
         impl std::error::Error for Error {{}}\n\n\
         /// The constraint names one operation may report to its caller.\n\
         #[derive(Clone, Copy, Debug)]\n\
         pub struct Constraints {{\n\
         pub unique: &'static [&'static str],\n\
         pub foreign_key: &'static [&'static str],\n\
         pub check: &'static [&'static str],\n\
         pub exclusion: &'static [&'static str],\n\
         }}\n\n\
         impl Constraints {{\n\
         /// An operation that names no constraint.\n\
         pub const NONE: Self = Self {{ unique: &[], foreign_key: &[], check: &[], exclusion: &[] }};\n\
         }}\n"
    )
}
