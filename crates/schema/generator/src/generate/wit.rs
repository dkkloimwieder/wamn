//! Typed component contracts generated from operation declarations.

use std::fmt::Write as _;

use super::wit_adapters::{emit_error_mapper, emit_export_adapter, emit_row_adapter};
use super::{
    AccessOperationErrorLiteral, BTreeMap, Column, ColumnType, ContractFieldDeclaration,
    CrudAction, CustomOperationDeclaration, GenerateError, GenerateErrorKind, ModelDeclaration,
    OperationDeclaration, OperationErrorDetailDeclaration, PackageManifest, ResultClass, Table,
    insert_bytes, rust_identifier, rust_type_identifier,
};
use crate::client_fields::input_fields_of;
use crate::client_ir::FieldIr;
use crate::manifest::OperationErrorDetailKey;

/// Emit typed boundaries for every declared model operation.
pub(super) fn emit_model_wit(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
) -> Result<(), GenerateError> {
    if model.operations.is_empty() {
        return Ok(());
    }
    emit_codec_support(files)?;
    let package = manifest.package.id.replace('_', "-");
    let directory = format!("generated/wit/deps/{package}-{}", wit_name(model_name));
    insert_bytes(
        files,
        &format!("{directory}/package.wit"),
        emit_model_package_wit(manifest, model_name, model, table).into_bytes(),
    )?;
    for (action, operation) in &model.operations {
        let codec = emit_crud_codec(*action, table, operation);
        insert_bytes(
            files,
            &format!("generated/wit/{model_name}_{}_codec.rs", action.as_str()),
            codec.into_bytes(),
        )?;
    }
    Ok(())
}

fn emit_model_package_wit(
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
) -> String {
    let package = manifest.package.id.replace('_', "-");
    let mut source = format!(
        "package {package}:{}@{};\n\n",
        wit_name(model_name),
        manifest.package.version
    );
    for action in model.operations.keys() {
        emit_crud_interface(&mut source, *action, table, &model.operations[action]);
    }
    source
}

fn emit_crud_interface(
    source: &mut String,
    action: CrudAction,
    table: &Table,
    operation: &OperationDeclaration,
) {
    if action == CrudAction::Update {
        emit_update_interface(source, table, operation);
        return;
    }
    let name = action.as_str();
    writeln!(
        source,
        "interface {name} {{\n  use wamn:node/types@0.1.0.{{emission, node-context, node-error}};\n"
    )
    .expect("writing to a String cannot fail");
    writeln!(source, "  record {name}-request {{").expect("writing to a String cannot fail");
    match action {
        CrudAction::Get | CrudAction::Delete => source.push_str("    id: string,\n"),
        CrudAction::Create => {
            source.push_str("    idempotency-key: string,\n");
            emit_writable_fields(source, table, operation, false);
        }
        CrudAction::Query => {
            for filter in &operation.filters {
                let column = model_column(table, &filter.field);
                writeln!(
                    source,
                    "    {}: option<list<{}>>,",
                    wit_name(&filter.field),
                    wit_type(column.column_type())
                )
                .expect("writing to a String cannot fail");
            }
            source.push_str("    sort-field: option<string>,\n    sort-direction: option<string>,\n    cursor: option<string>,\n    limit: option<s64>,\n");
        }
        CrudAction::Update => unreachable!("update uses its compatibility emitter"),
    }
    if action == CrudAction::Delete {
        let revision = operation
            .revision_field
            .as_deref()
            .expect("validated delete revision exists");
        writeln!(source, "    expected-{}: s64,", wit_name(revision))
            .expect("writing to a String cannot fail");
    }
    source.push_str("  }\n\n");
    emit_operation_errors(source, name, operation);
    writeln!(source, "  record {name}-item {{\n    request-id: string,\n    input: result<{name}-request, invalid-input-detail>,\n  }}\n")
        .expect("writing to a String cannot fail");
    emit_crud_result(source, name, action, table, operation.result);
    writeln!(source, "  record {name}-outcome {{\n    request-id: string,\n    outcome: result<{}{}, {name}-error>,\n  }}\n", if operation.result == super::ResultClass::Page { "" } else { "" }, format!("{name}-result"))
        .expect("writing to a String cannot fail");
    writeln!(source, "  run: async func(ctx: node-context, input: list<{name}-item>) -> result<list<{name}-outcome>, node-error>;\n  run-json: async func(ctx: node-context, input: string) -> result<emission, node-error>;\n}}\n")
        .expect("writing to a String cannot fail");
}

fn emit_writable_fields(
    source: &mut String,
    table: &Table,
    operation: &OperationDeclaration,
    tri_state: bool,
) {
    for field in &operation.writable_fields {
        let column = model_column(table, field);
        let mut ty = wit_type(column.column_type());
        if column.nullable() {
            ty = format!("option<{ty}>");
        }
        if tri_state {
            ty = format!("option<{ty}>");
        }
        writeln!(source, "    {}: {ty},", wit_name(field))
            .expect("writing to a String cannot fail");
    }
}

fn emit_operation_errors(source: &mut String, name: &str, operation: &OperationDeclaration) {
    for (literal, detail) in &operation.error_details {
        if !detail.required.is_empty() || !detail.optional.is_empty() {
            emit_error_detail(source, access_error_literal(*literal), detail);
        }
    }
    writeln!(source, "  variant {name}-error {{").expect("writing to a String cannot fail");
    for (literal, detail) in &operation.error_details {
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
    table: &Table,
    class: super::ResultClass,
) {
    writeln!(source, "  record {name}-row {{").expect("writing to a String cannot fail");
    for column in table.columns() {
        let mut ty = wit_type(column.column_type());
        if column.nullable() {
            ty = format!("option<{ty}>");
        }
        writeln!(source, "    {}: {ty},", wit_name(column.name()))
            .expect("writing to a String cannot fail");
    }
    source.push_str("  }\n\n");
    let carrier = match class {
        super::ResultClass::One => format!("{name}-row"),
        super::ResultClass::OptionalOne => format!("option<{name}-row>"),
        super::ResultClass::BoundedList => format!("list<{name}-row>"),
        super::ResultClass::Page => format!("list<{name}-row>"),
    };
    writeln!(source, "  record {name}-result {{\n    value: {carrier},")
        .expect("writing to a String cannot fail");
    if action == CrudAction::Query && class == super::ResultClass::Page {
        source.push_str("    next-cursor: option<string>,\n");
    }
    source.push_str("  }\n\n");
}

fn emit_update_interface(source: &mut String, table: &Table, operation: &OperationDeclaration) {
    source.push_str("interface update {\n  use wamn:node/types@0.1.0.{emission, node-context, node-error};\n\n  record update-change {\n");
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
    let revision = operation
        .revision_field
        .as_deref()
        .expect("validated update revision exists");
    source.push_str("  }\n\n  record update-request {\n    id: string,\n");
    writeln!(source, "    expected-{}: s64,", wit_name(revision))
        .expect("writing to a String cannot fail");
    source.push_str("    change: update-change,\n  }\n\n");
    for (literal, detail) in &operation.error_details {
        if !detail.required.is_empty() || !detail.optional.is_empty() {
            emit_error_detail(source, access_error_literal(*literal), detail);
        }
    }
    source.push_str("  variant update-error {\n");
    for (literal, detail) in &operation.error_details {
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
    for column in table.columns() {
        let mut ty = wit_type(column.column_type());
        if column.nullable() {
            ty = format!("option<{ty}>");
        }
        writeln!(source, "    {}: {ty},", wit_name(column.name()))
            .expect("writing to a String cannot fail");
    }
    source.push_str("  }\n\n  record update-outcome {\n    request-id: string,\n    outcome: result<update-result, update-error>,\n  }\n\n  run: async func(ctx: node-context, input: list<update-item>) -> result<list<update-outcome>, node-error>;\n  run-json: async func(ctx: node-context, input: string) -> result<emission, node-error>;\n}\n");
}

fn model_column<'a>(table: &'a Table, field: &str) -> &'a Column {
    table
        .columns()
        .iter()
        .find(|column| column.name() == field)
        .expect("validated model operation field exists")
}

fn access_error_literal(literal: AccessOperationErrorLiteral) -> &'static str {
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
    }
}

fn emit_update_codec(table: &Table, operation: &OperationDeclaration) -> String {
    let mut source = codec_prelude("UpdateItem", 1, 100);
    source.push_str(UPDATE_CODEC_HEADER);
    for field in &operation.writable_fields {
        let column = model_column(table, field);
        writeln!(
            source,
            "    #[serde(default)]\n    {}: JsonChange<{}>,",
            rust_identifier(field).expect("validated update field has a Rust name"),
            codec_rust_type(column.column_type(), false)
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str(UPDATE_CODEC_DECODE_PREFIX);
    for field in &operation.writable_fields {
        let name = rust_identifier(field).expect("validated update field has a Rust name");
        writeln!(
            source,
            "                        {name}: change(request.change.{name}),"
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str(UPDATE_CODEC_VALIDATE_PREFIX);
    source.push_str(UPDATE_CODEC_ENCODE_PREFIX);
    for column in table.columns() {
        emit_codec_result_field(
            &mut source,
            column.name(),
            column.column_type(),
            column.nullable(),
        );
    }
    source.push_str(UPDATE_CODEC_ERROR_PREFIX);
    for (literal, detail) in &operation.error_details {
        emit_codec_error_arm(
            &mut source,
            "UpdateError",
            access_error_literal(*literal),
            detail,
        );
    }
    source.push_str(UPDATE_CODEC_FOOTER);
    source.push_str(&emit_handler("Update"));
    source.push_str(&emit_row_adapter(
        table
            .columns()
            .iter()
            .map(|column| (column.name(), column.column_type(), column.nullable())),
    ));
    source.push_str(&emit_error_mapper(
        "UpdateError",
        operation
            .error_details
            .iter()
            .map(|(literal, detail)| (access_error_literal(*literal), detail)),
    ));
    source.push_str(&emit_export_adapter("Update", false));
    source
}

fn emit_crud_codec(action: CrudAction, table: &Table, operation: &OperationDeclaration) -> String {
    if action == CrudAction::Update {
        return emit_update_codec(table, operation);
    }
    let type_name = rust_type_identifier(action.as_str());
    let mut source = codec_prelude(&format!("{type_name}Item"), 1, 100);
    source.push_str(&emit_crud_json_codec(action, table, operation));
    source.push_str(&emit_handler(&type_name));
    source.push_str(&emit_row_adapter(
        table
            .columns()
            .iter()
            .map(|column| (column.name(), column.column_type(), column.nullable())),
    ));
    source.push_str(&emit_error_mapper(
        &format!("{type_name}Error"),
        operation
            .error_details
            .iter()
            .map(|(literal, detail)| (access_error_literal(*literal), detail)),
    ));
    source.push_str(&emit_export_adapter(&type_name, false));
    source
}

fn emit_crud_json_codec(
    action: CrudAction,
    table: &Table,
    operation: &OperationDeclaration,
) -> String {
    let type_name = rust_type_identifier(action.as_str());
    let mut source = String::from("#[derive(Deserialize)]\n#[serde(deny_unknown_fields)]\n");
    match action {
        CrudAction::Get | CrudAction::Delete => {
            source.push_str("struct JsonRequest { id: String");
            if action == CrudAction::Delete {
                let revision = rust_identifier(
                    operation
                        .revision_field
                        .as_deref()
                        .expect("validated delete revision exists"),
                )
                .expect("validated revision has a Rust name");
                write!(source, ", expected_{revision}: JsonInt64")
                    .expect("writing to a String cannot fail");
            }
            source.push_str(" }\n\n");
        }
        CrudAction::Create => {
            source.push_str("struct JsonRequest { idempotency_key: String,\n");
            emit_json_columns(&mut source, table, &operation.writable_fields);
            source.push_str("}\n\n");
        }
        CrudAction::Query => emit_query_json_types(&mut source, table, operation),
        CrudAction::Update => unreachable!("update owns its compatibility JSON codec"),
    }
    writeln!(source, "pub(crate) fn decode(input: &str) -> Result<Vec<contract::{type_name}Item>, CodecError> {{\n    decode_envelope(input)?.into_iter().map(|(request_id, body)| {{\n        let input = serde_json::from_value::<JsonRequest>(body).map(|request| contract::{type_name}Request {{")
        .expect("writing to a String cannot fail");
    emit_crud_request_assignments(&mut source, action, table, operation);
    writeln!(source, "        }}).map_err(|_| invalid(\"input\"));\n        Ok(contract::{type_name}Item {{ request_id, input }})\n    }}).collect()\n}}\n")
        .expect("writing to a String cannot fail");
    emit_crud_invalid_detail(&mut source, operation);
    emit_crud_encoder(&mut source, action, table, operation);
    source
}

fn emit_json_columns(source: &mut String, table: &Table, fields: &[String]) {
    for field in fields {
        let column = model_column(table, field);
        writeln!(
            source,
            "    {}: {},",
            rust_identifier(field).expect("validated field has a Rust name"),
            codec_rust_type(column.column_type(), column.nullable())
        )
        .expect("writing to a String cannot fail");
    }
}

fn emit_query_json_types(source: &mut String, table: &Table, operation: &OperationDeclaration) {
    source.push_str("struct JsonRequest { #[serde(default)] filter: Option<JsonFilter>, #[serde(default)] sort: Option<JsonSort>, #[serde(default)] cursor: Option<String>, #[serde(default)] limit: Option<JsonInt64> }\n\n#[derive(Deserialize)]\n#[serde(deny_unknown_fields)]\nstruct JsonFilter {\n");
    for filter in &operation.filters {
        let column = model_column(table, &filter.field);
        writeln!(
            source,
            "    #[serde(default)] {}: Option<Vec<{}>> ,",
            rust_identifier(&filter.field).expect("validated filter has a Rust name"),
            codec_rust_type(column.column_type(), false)
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str("}\n\n#[derive(Deserialize)]\n#[serde(deny_unknown_fields)]\nstruct JsonSort { field: String, direction: String }\n\n");
}

fn emit_crud_request_assignments(
    source: &mut String,
    action: CrudAction,
    table: &Table,
    operation: &OperationDeclaration,
) {
    match action {
        CrudAction::Get => source.push_str("            id: request.id,\n"),
        CrudAction::Delete => {
            source.push_str("            id: request.id,\n");
            let revision = rust_identifier(
                operation
                    .revision_field
                    .as_deref()
                    .expect("delete revision exists"),
            )
            .expect("validated revision has a Rust name");
            writeln!(
                source,
                "            expected_{revision}: request.expected_{revision}.0,"
            )
            .expect("writing to a String cannot fail");
        }
        CrudAction::Create => {
            source.push_str("            idempotency_key: request.idempotency_key,\n");
            for field in &operation.writable_fields {
                let column = model_column(table, field);
                let field = rust_identifier(field).expect("validated field has a Rust name");
                let conversion = match (column.column_type(), column.nullable()) {
                    (ColumnType::Int64, true) => ".map(|value| value.0)",
                    (ColumnType::Int64, false) => ".0",
                    _ => "",
                };
                writeln!(source, "            {field}: request.{field}{conversion},")
                    .expect("writing to a String cannot fail");
            }
        }
        CrudAction::Query => {
            for filter in &operation.filters {
                let field =
                    rust_identifier(&filter.field).expect("validated filter has a Rust name");
                writeln!(source, "            {field}: request.filter.as_mut().and_then(|filter| filter.{field}.take()),")
                    .expect("writing to a String cannot fail");
            }
            source.push_str("            sort_field: request.sort.as_ref().map(|sort| sort.field.clone()),\n            sort_direction: request.sort.map(|sort| sort.direction),\n            cursor: request.cursor,\n            limit: request.limit.map(|value| value.0),\n");
        }
        CrudAction::Update => unreachable!("update has a separate codec"),
    }
}

fn emit_crud_invalid_detail(source: &mut String, operation: &OperationDeclaration) {
    let detail = &operation.error_details[&AccessOperationErrorLiteral::InvalidInput];
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

fn emit_crud_encoder(
    source: &mut String,
    action: CrudAction,
    table: &Table,
    operation: &OperationDeclaration,
) {
    let type_name = rust_type_identifier(action.as_str());
    writeln!(source, "pub(crate) fn encode(output: &[contract::{type_name}Outcome]) -> String {{\n    let values = output.iter().map(|item| match &item.outcome {{\n        Ok(value) => json!({{ \"request_id\": item.request_id, \"value\":")
        .expect("writing to a String cannot fail");
    match operation.result {
        ResultClass::One => emit_json_row(source, table, "value.value", 12),
        ResultClass::OptionalOne => {
            source.push_str("value.value.as_ref().map(|row| json!({\n");
            emit_json_row_fields(source, table, "row");
            source.push_str("            }))\n");
        }
        ResultClass::BoundedList => {
            source.push_str("value.value.iter().map(|row| json!({\n");
            emit_json_row_fields(source, table, "row");
            source.push_str("            })).collect::<Vec<_>>()\n");
        }
        ResultClass::Page => {
            source.push_str("{ \"item\": value.value.iter().map(|row| json!({\n");
            emit_json_row_fields(source, table, "row");
            source.push_str(
                "            })).collect::<Vec<_>>(), \"next_cursor\": value.next_cursor }\n",
            );
        }
    }
    source.push_str("        }),\n        Err(error) => json!({ \"request_id\": item.request_id, \"error\": error_value(error) }),\n    }).collect::<Vec<_>>();\n    serde_json::to_string(&values).expect(\"typed operation outcomes always serialize\")\n}\n\n");
    writeln!(source, "fn error_value(error: &contract::{type_name}Error) -> Value {{\n    let (code, detail) = match error {{")
        .expect("writing to a String cannot fail");
    for (literal, detail) in &operation.error_details {
        emit_codec_error_arm(
            source,
            &format!("{type_name}Error"),
            access_error_literal(*literal),
            detail,
        );
    }
    source.push_str("    };\n    json!({\"code\": code, \"detail\": detail})\n}\n");
}

fn emit_json_row(source: &mut String, table: &Table, carrier: &str, indentation: usize) {
    writeln!(source, "json!({{").expect("writing to a String cannot fail");
    emit_json_row_fields(source, table, carrier);
    writeln!(source, "{}}})", " ".repeat(indentation)).expect("writing to a String cannot fail");
}

fn emit_json_row_fields(source: &mut String, table: &Table, carrier: &str) {
    for column in table.columns() {
        emit_codec_result_field_for(
            source,
            column.name(),
            column.column_type(),
            column.nullable(),
            carrier,
        );
    }
}

fn emit_handler(type_name: &str) -> String {
    format!(
        r#"
pub(crate) async fn run<S, F>(input: Vec<contract::{type_name}Item>, state: &mut S, mut handler: F) -> Vec<contract::{type_name}Outcome>
where
    F: AsyncFnMut(&mut S, contract::{type_name}Request) -> Result<contract::{type_name}Result, contract::{type_name}Error>,
{{
    let mut output = Vec::with_capacity(input.len());
    for item in input {{
        let outcome = match item.input {{
            Ok(request) => handler(state, request).await,
            Err(error) => Err(contract::{type_name}Error::InvalidInput(error)),
        }};
        output.push(contract::{type_name}Outcome {{ request_id: item.request_id, outcome }});
    }}
    output
}}
"#
    )
}

fn emit_codec_support(files: &mut BTreeMap<String, Vec<u8>>) -> Result<(), GenerateError> {
    let path = "generated/wit/operation_codec.rs";
    if !files.contains_key(path) {
        insert_bytes(
            files,
            path,
            format!("{CODEC_HEADER}{CODEC_ENVELOPE}").into_bytes(),
        )?;
    }
    Ok(())
}

fn codec_prelude(item: &str, minimum: u32, maximum: u32) -> String {
    format!(
        "// @generated from operation declarations; do not edit.\n\ninclude!(\"operation_codec.rs\");\ntype Item = contract::{item};\nconst MINIMUM: usize = {minimum};\nconst MAXIMUM: usize = {maximum};\nconst COUNT_ERROR: &str = \"operation input item count must be {minimum}..={maximum}\";\n\n"
    )
}

const CODEC_ENVELOPE: &str = r#"
fn validate_count(count: usize) -> Result<(), CodecError> {
    if !(MINIMUM..=MAXIMUM).contains(&count) {
        return Err(CodecError(COUNT_ERROR));
    }
    Ok(())
}

pub(crate) fn validate(input: &[Item]) -> Result<(), CodecError> {
    validate_count(input.len())?;
    if input.iter().any(|item| item.request_id.is_empty()) {
        return Err(CodecError("every operation item must carry a nonempty string request_id"));
    }
    Ok(())
}

fn decode_envelope(input: &str) -> Result<Vec<(String, Value)>, CodecError> {
    let Value::Array(values) = serde_json::from_str(input)
        .map_err(|_| CodecError("operation input must be a JSON array"))?
    else { return Err(CodecError("operation input must be a JSON array")); };
    validate_count(values.len())?;
    values.into_iter().map(|value| {
        let Value::Object(mut object) = value else {
            return Err(CodecError("every operation item must be a JSON object"));
        };
        let Some(Value::String(request_id)) = object.remove("request_id") else {
            return Err(CodecError("every operation item must carry a nonempty string request_id"));
        };
        if request_id.is_empty() {
            return Err(CodecError("every operation item must carry a nonempty string request_id"));
        }
        Ok((request_id, Value::Object(object)))
    }).collect()
}
"#;

const CODEC_HEADER: &str = r"// @generated from wamn.json and schema IR; do not edit.

use serde::Deserialize;
#[allow(unused_imports)]
use serde_json::{Map, Value, json};

#[allow(dead_code)]
struct JsonInt64(i64);

impl<'de> Deserialize<'de> for JsonInt64 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map(Self).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug)]
pub(crate) struct CodecError(&'static str);

impl CodecError {
    pub(crate) const fn context(&self) -> &'static str { self.0 }
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for CodecError {}

";

const UPDATE_CODEC_HEADER: &str = r"#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    id: String,
    expected_row_version: String,
    change: JsonUpdateChange,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonUpdateChange {
";

const UPDATE_CODEC_DECODE_PREFIX: &str = r#"}

#[derive(Default)]
enum JsonChange<T> {
    #[default]
    Absent,
    Null,
    Value(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for JsonChange<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[expect(clippy::option_option, reason = "WIT update fields distinguish absent, null, and value")]
fn change<T>(value: JsonChange<T>) -> Option<Option<T>> {
    match value {
        JsonChange::Absent => None,
        JsonChange::Null => Some(None),
        JsonChange::Value(value) => Some(Some(value)),
    }
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::UpdateItem>, CodecError> {
    decode_envelope(input)?.into_iter().map(|(request_id, body)| {
        let input = match serde_json::from_value::<JsonRequest>(body) {
            Ok(request) => match request.expected_row_version.parse::<i64>() {
                Ok(expected_row_version) => {
                    let request = contract::UpdateRequest {
                        id: request.id,
                        expected_row_version,
                        change: contract::UpdateChange {
"#;

const UPDATE_CODEC_VALIDATE_PREFIX: &str = r#"                        },
                    };
                    Ok(request)
                }
                Err(_) => Err(invalid("expected_row_version")),
            },
            Err(_) => Err(invalid("input")),
        };
        Ok(contract::UpdateItem { request_id, input })
    }).collect()
}

"#;

const UPDATE_CODEC_ENCODE_PREFIX: &str = r#"fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail { field: field.to_owned() }
}

pub(crate) fn encode(output: &[contract::UpdateOutcome]) -> String {
    let values = output.iter().map(|item| {
        match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
"#;

const UPDATE_CODEC_ERROR_PREFIX: &str = r#"                }
            }),
            Err(error) => json!({
                "request_id": item.request_id,
                "error": error_value(error),
            }),
        }
    }).collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed update outcomes always serialize")
}

fn error_value(error: &contract::UpdateError) -> Value {
    let (code, detail) = match error {
"#;

const UPDATE_CODEC_FOOTER: &str = r#"    };
    json!({"code": code, "detail": detail})
}
"#;

/// Emit a typed component boundary for a declared custom operation.
pub(super) fn emit_custom_operation_wit(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    emit_codec_support(files)?;
    let (group, local_name) = operation_name.split_once('.').ok_or_else(|| {
        GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            "custom operation needs a group and local name",
        )
    })?;
    let package = manifest.package.id.replace('_', "-");
    let version = &manifest.package.version;
    let directory = format!("generated/wit/deps/{package}-{}", wit_name(group));
    let source = if let Some((_, dependency)) =
        manifest.base_dependencies.iter().find(|(_, item)| {
            item.operations
                .iter()
                .any(|candidate| candidate == operation_name)
        }) {
        emit_forwarding_interface(&package, version, group, local_name, dependency)
    } else {
        emit_owned_group(&package, version, group, manifest)?
    };
    let package_path = format!("{directory}/package.wit");
    if !files.contains_key(&package_path) {
        insert_bytes(files, &package_path, source.into_bytes())?;
    }
    let codec = emit_custom_codec(local_name, operation)?;
    insert_bytes(
        files,
        &format!(
            "generated/wit/{}_{}_codec.rs",
            artifact_name(group),
            artifact_name(local_name)
        ),
        codec.into_bytes(),
    )
}

fn emit_owned_group(
    package: &str,
    version: &str,
    group: &str,
    manifest: &PackageManifest,
) -> Result<String, GenerateError> {
    let mut source = format!("package {package}:{}@{version};\n\n", wit_name(group));
    for (operation_name, operation) in &manifest.custom_operations {
        let Some(local_name) = operation_name.strip_prefix(&format!("{group}.")) else {
            continue;
        };
        let emitted =
            emit_owned_interface(package, version, group, local_name, manifest, operation)?;
        let marker = format!("interface {} {{", wit_name(local_name));
        let start = emitted
            .rfind(&marker)
            .expect("owned interface emitter includes its operation");
        source.push_str(&emitted[start..]);
    }
    Ok(source)
}

fn emit_forwarding_interface(
    package: &str,
    version: &str,
    group: &str,
    local_name: &str,
    dependency: &crate::manifest::BaseDependencyRequirement,
) -> String {
    let dependency_package = dependency.package.replace('_', "-");
    let interface = wit_name(local_name);
    format!(
        "package {package}:{}@{version};\n\ninterface {interface} {{\n  use wamn:node/types@0.1.0.{{emission, node-context, node-error}};\n  use {dependency_package}:{}/{interface}@{}.{{{interface}-item, {interface}-outcome}};\n\n  run: async func(ctx: node-context, input: list<{interface}-item>) -> result<list<{interface}-outcome>, node-error>;\n  run-json: async func(ctx: node-context, input: string) -> result<emission, node-error>;\n}}\n",
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
    operation: &CustomOperationDeclaration,
) -> Result<String, GenerateError> {
    let result = operation.result.as_ref();
    let input_tree = input_fields_of(
        &serde_json::to_value(&operation.input).expect("validated custom input serializes"),
    );
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
    writeln!(source, "interface {interface} {{\n  use wamn:node/types@0.1.0.{{emission, node-context, node-error}};\n").expect("writing to a String cannot fail");
    if operation.input.envelope.is_none() && result.is_none() {
        let request_fields = input_tree
            .iter()
            .filter(|field| field.path != "request_id")
            .cloned()
            .collect::<Vec<_>>();
        emit_nested_wit_records(&mut source, &interface, &request_fields, "");
        writeln!(source, "  record {interface}-request {{")
            .expect("writing to a String cannot fail");
        emit_wit_tree_fields(&mut source, &interface, &request_fields, "");
        writeln!(source, "  }}\n\n  run: async func(ctx: node-context, input: {interface}-request) -> result<{interface}-request, node-error>;\n  run-json: async func(ctx: node-context, input: string) -> result<emission, node-error>;\n}}\n")
            .expect("writing to a String cannot fail");
        return Ok(source);
    }
    let value = input_tree.iter().find(|field| field.path == "value");
    let request_fields = value.map_or_else(
        || {
            input_tree
                .iter()
                .filter(|field| field.path != "request_id")
                .cloned()
                .collect::<Vec<_>>()
        },
        |value| value.children.clone(),
    );
    let root = if value.is_some() { "value." } else { "" };
    emit_nested_wit_records(&mut source, &interface, &request_fields, root);
    writeln!(source, "  record {interface}-request {{").expect("writing to a String cannot fail");
    emit_wit_tree_fields(&mut source, &interface, &request_fields, root);
    source.push_str("  }\n\n");

    for (literal, detail) in &operation.error_details {
        if !detail.required.is_empty() || !detail.optional.is_empty() {
            emit_error_detail(&mut source, literal, detail);
        }
    }
    writeln!(source, "  variant {interface}-error {{").expect("writing to a String cannot fail");
    for literal in &operation.errors {
        let detail = operation
            .error_details
            .get(literal)
            .expect("validated error has a detail declaration");
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
    writeln!(source, "  }}\n\n  record {interface}-item {{\n    request-id: string,\n    input: result<{interface}-request, invalid-input-detail>,\n  }}\n").expect("writing to a String cannot fail");
    if result.is_some_and(|result| result.class == ResultClass::BoundedList) {
        writeln!(source, "  record {interface}-row {{").expect("writing to a String cannot fail");
        emit_record_fields(
            &mut source,
            &result.expect("bounded list result exists").fields,
            "",
            false,
        );
        writeln!(
            source,
            "  }}\n\n  record {interface}-result {{\n    rows: list<{interface}-row>,"
        )
        .expect("writing to a String cannot fail");
    } else {
        writeln!(source, "  record {interface}-result {{")
            .expect("writing to a String cannot fail");
        if let Some(result) = result {
            emit_record_fields(&mut source, &result.fields, "", false);
        }
    }
    writeln!(source, "  }}\n\n  record {interface}-outcome {{\n    request-id: string,\n    outcome: result<{interface}-result, {interface}-error>,\n  }}\n").expect("writing to a String cannot fail");
    writeln!(source, "  run: async func(ctx: node-context, input: list<{interface}-item>) -> result<list<{interface}-outcome>, node-error>;\n  run-json: async func(ctx: node-context, input: string) -> result<emission, node-error>;\n}}\n").expect("writing to a String cannot fail");
    Ok(source)
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
        writeln!(source, "    {}: {ty},", wit_name(path)).expect("writing to a String cannot fail");
    }
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
        _ => "string",
    }
    .to_owned()
}

fn emit_error_detail(source: &mut String, literal: &str, detail: &OperationErrorDetailDeclaration) {
    writeln!(source, "  record {}-detail {{", wit_name(literal))
        .expect("writing to a String cannot fail");
    for key in &detail.required {
        writeln!(source, "    {}: {},", detail_name(*key), detail_type(*key))
            .expect("writing to a String cannot fail");
    }
    for key in &detail.optional {
        writeln!(
            source,
            "    {}: option<{}>,",
            detail_name(*key),
            detail_type(*key)
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

fn detail_type(key: OperationErrorDetailKey) -> &'static str {
    match key {
        OperationErrorDetailKey::Minimum
        | OperationErrorDetailKey::Maximum
        | OperationErrorDetailKey::Observed => "s64",
        OperationErrorDetailKey::Field
        | OperationErrorDetailKey::Id
        | OperationErrorDetailKey::ExpectedRowVersion
        | OperationErrorDetailKey::ObservedRowVersion
        | OperationErrorDetailKey::Constraint
        | OperationErrorDetailKey::Operation => "string",
    }
}

fn wit_type(ty: ColumnType) -> String {
    let value = match ty {
        ColumnType::Boolean => "bool",
        ColumnType::Int32 => "s32",
        ColumnType::Int64 => "s64",
        ColumnType::Float64 => "f64",
        ColumnType::Text
        | ColumnType::Numeric
        | ColumnType::Timestamptz
        | ColumnType::Json
        | ColumnType::Uuid => "string",
        ColumnType::Bytes => "list<u8>",
    };
    value.to_owned()
}

fn wit_name(value: &str) -> String {
    value.replace('_', "-")
}

fn artifact_name(value: &str) -> String {
    rust_identifier(value)
        .expect("validated operation name has a Rust spelling")
        .trim_start_matches("r#")
        .to_owned()
}

fn emit_custom_codec(
    local_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<String, GenerateError> {
    let Some(result) = operation.result.as_ref() else {
        let type_name = rust_type_identifier(local_name);
        let mut source = format!(
            "// @generated from operation declarations; do not edit.\n\nuse serde::Deserialize;\n\n#[derive(Debug)]\npub(crate) struct CodecError(&'static str);\nimpl CodecError {{ pub(crate) const fn context(&self) -> &'static str {{ self.0 }} }}\nimpl std::fmt::Display for CodecError {{ fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{ formatter.write_str(self.0) }} }}\nimpl std::error::Error for CodecError {{}}\n\n#[derive(Deserialize)]\n#[serde(deny_unknown_fields)]\nstruct JsonInput {{ event: String, new: JsonNew }}\n#[derive(Deserialize)]\n#[serde(deny_unknown_fields)]\nstruct JsonNew {{ id: String }}\n\npub(crate) fn decode(input: &str) -> Result<contract::{type_name}Request, CodecError> {{\n    let value: JsonInput = serde_json::from_str(input).map_err(|_| CodecError(\"operation input does not match its declared object\"))?;\n    Ok(contract::{type_name}Request {{ event: value.event, new: contract::{type_name}New {{ id: value.new.id }} }})\n}}\n\npub(crate) fn encode(value: &contract::{type_name}Request) -> String {{\n    serde_json::json!({{\"event\": value.event, \"new\": {{\"id\": value.new.id}}}}).to_string()\n}}\n"
        );
        source.push_str(&emit_export_adapter(&type_name, true));
        return Ok(source);
    };
    let value_fields = codec_fields(&operation.input.fields, "value.");
    let line_fields = codec_fields(&operation.input.fields, "value.line[].");
    let flat_fields = operation
        .input
        .fields
        .iter()
        .filter_map(|field| {
            (!field.path.contains('.') && field.path != "request_id")
                .then_some((field, field.path.as_str()))
        })
        .collect::<Vec<_>>();
    let (minimum, maximum) = operation
        .input
        .envelope
        .as_ref()
        .map_or((1, 100), |limit| (limit.minimum, limit.maximum));
    let type_name = rust_type_identifier(local_name);
    let mut source = codec_prelude(&format!("{type_name}Item"), minimum, maximum);
    if value_fields.is_empty() && line_fields.is_empty() {
        source.push_str("#[derive(Deserialize)]\n#[serde(deny_unknown_fields)]\n");
        emit_json_struct(&mut source, "JsonRequest", &flat_fields);
        let request = if flat_fields.is_empty() {
            "_request"
        } else {
            "request"
        };
        write!(source, "}}\n\npub(crate) fn decode(input: &str) -> Result<Vec<contract::RecordReceiptItem>, CodecError> {{\n    decode_envelope(input)?.into_iter().map(|(request_id, body)| {{\n        let input = serde_json::from_value::<JsonRequest>(body).map(|{request}| contract::RecordReceiptRequest {{\n")
            .expect("writing to a String cannot fail");
        emit_codec_field_assignments(&mut source, &flat_fields, "request", 12);
        source.push_str("        }).map_err(|_| invalid(\"input\"));\n        Ok(contract::RecordReceiptItem { request_id, input })\n    }).collect()\n}\n\n");
    } else {
        source.push_str(RECEIPT_CODEC_HEADER);
        emit_json_struct(&mut source, "JsonValue", &value_fields);
        if !line_fields.is_empty() {
            source.push_str("    line: Vec<JsonLine>,\n");
        }
        source.push_str("}\n\n");
        if !line_fields.is_empty() {
            source.push_str("#[derive(Deserialize)]\n#[serde(deny_unknown_fields)]\n");
            emit_json_struct(&mut source, "JsonLine", &line_fields);
            source.push_str("}\n");
        }
        source.push_str(RECEIPT_CODEC_DECODE_PREFIX);
        emit_codec_field_assignments(&mut source, &value_fields, "request.value", 16);
        if !line_fields.is_empty() {
            source.push_str("                line: request.value.line.into_iter().map(|line| contract::RecordReceiptLine {\n");
            emit_codec_field_assignments(&mut source, &line_fields, "line", 20);
            source.push_str("                }).collect(),\n");
        }
        source.push_str("            }),\n            Err(_) => Err(invalid(\"input\")),\n        };\n        Ok(contract::RecordReceiptItem { request_id, input })\n    }).collect()\n}\n\n");
    }
    emit_invalid_detail(&mut source, operation);
    source.push_str("pub(crate) fn encode(output: &[contract::RecordReceiptOutcome]) -> String {\n    let values = output.iter().map(|item| {\n        match &item.outcome {\n            Ok(value) => json!({\n                \"request_id\": item.request_id,\n                \"value\": ");
    if result.class == ResultClass::BoundedList {
        source.push_str("{ \"rows\": value.rows.iter().map(|row| json!({\n");
        for field in &result.fields {
            emit_codec_result_field_for(&mut source, &field.path, field.ty, field.nullable, "row");
        }
        source.push_str("                })).collect::<Vec<_>>() }\n");
    } else {
        source.push_str("{\n");
        for field in &result.fields {
            if field.path.contains("[]") || field.path.contains('.') {
                continue;
            }
            emit_codec_result_field(&mut source, &field.path, field.ty, field.nullable);
        }
        source.push_str("                }\n");
    }
    source.push_str("            }),\n");
    source.push_str(RECEIPT_CODEC_ERROR_PREFIX);
    for literal in &operation.errors {
        emit_codec_error_arm(
            &mut source,
            "RecordReceiptError",
            literal,
            &operation.error_details[literal],
        );
    }
    source.push_str(RECEIPT_CODEC_FOOTER);
    source = source.replace("RecordReceipt", &type_name);
    source.push_str(&emit_handler(&type_name));
    source.push_str(&emit_row_adapter(
        result
            .fields
            .iter()
            .map(|field| (field.path.as_str(), field.ty, field.nullable)),
    ));
    source.push_str(&emit_error_mapper(
        &format!("{type_name}Error"),
        operation
            .errors
            .iter()
            .map(|literal| (literal.as_str(), &operation.error_details[literal])),
    ));
    source.push_str(&emit_export_adapter(&type_name, false));
    Ok(source)
}

fn codec_fields<'a>(
    fields: &'a [ContractFieldDeclaration],
    prefix: &str,
) -> Vec<(&'a ContractFieldDeclaration, &'a str)> {
    fields
        .iter()
        .filter_map(|field| {
            let path = field.path.strip_prefix(prefix)?;
            (!path.contains("[]") && !path.contains('.')).then_some((field, path))
        })
        .collect()
}

fn emit_json_struct(source: &mut String, name: &str, fields: &[(&ContractFieldDeclaration, &str)]) {
    writeln!(source, "struct {name} {{").expect("writing to a String cannot fail");
    for (field, path) in fields {
        let name = rust_identifier(path).expect("validated input field has a Rust name");
        let ty = codec_rust_type(field.ty, field.nullable);
        writeln!(source, "    {name}: {ty},").expect("writing to a String cannot fail");
    }
}

fn codec_rust_type(ty: ColumnType, nullable: bool) -> String {
    let ty = match ty {
        ColumnType::Boolean => "bool",
        ColumnType::Int32 => "i32",
        ColumnType::Int64 => "JsonInt64",
        ColumnType::Float64 => "f64",
        ColumnType::Text
        | ColumnType::Numeric
        | ColumnType::Timestamptz
        | ColumnType::Json
        | ColumnType::Uuid => "String",
        ColumnType::Bytes => "Vec<u8>",
    };
    if nullable {
        format!("Option<{ty}>")
    } else {
        ty.to_owned()
    }
}

fn emit_codec_field_assignments(
    source: &mut String,
    fields: &[(&ContractFieldDeclaration, &str)],
    value: &str,
    indentation: usize,
) {
    let indentation = " ".repeat(indentation);
    for (field, path) in fields {
        let name = rust_identifier(path).expect("validated input field has a Rust name");
        let conversion = match (field.ty, field.nullable) {
            (ColumnType::Int64, true) => ".map(|value| value.0)",
            (ColumnType::Int64, false) => ".0",
            _ => "",
        };
        writeln!(source, "{indentation}{name}: {value}.{name}{conversion},")
            .expect("writing to a String cannot fail");
    }
}

fn emit_invalid_detail(source: &mut String, operation: &CustomOperationDeclaration) {
    let detail = &operation.error_details["invalid_input"];
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

fn emit_codec_error_arm(
    source: &mut String,
    error_type: &str,
    literal: &str,
    detail: &OperationErrorDetailDeclaration,
) {
    let variant = rust_type_identifier(literal);
    if detail.required.is_empty() && detail.optional.is_empty() {
        writeln!(
            source,
            "        contract::{error_type}::{variant} => ({literal:?}, Map::new()),"
        )
        .expect("writing to a String cannot fail");
        return;
    }
    writeln!(
        source,
        "        contract::{error_type}::{variant}(value) => {{"
    )
    .expect("writing to a String cannot fail");
    source.push_str("            let mut detail = Map::new();\n");
    for key in &detail.required {
        let name = detail_name(*key).replace('-', "_");
        writeln!(
            source,
            "            detail.insert({name:?}.to_owned(), json!(value.{name}));"
        )
        .expect("writing to a String cannot fail");
    }
    for key in &detail.optional {
        let name = detail_name(*key).replace('-', "_");
        writeln!(source, "            if let Some(detail_value) = &value.{name} {{ detail.insert({name:?}.to_owned(), json!(detail_value)); }}")
            .expect("writing to a String cannot fail");
    }
    writeln!(source, "            ({literal:?}, detail)\n        }}")
        .expect("writing to a String cannot fail");
}

fn emit_codec_result_field(source: &mut String, field: &str, ty: ColumnType, nullable: bool) {
    emit_codec_result_field_for(source, field, ty, nullable, "value");
}

fn emit_codec_result_field_for(
    source: &mut String,
    field: &str,
    ty: ColumnType,
    nullable: bool,
    carrier: &str,
) {
    let name = rust_identifier(field).expect("validated result field has a Rust name");
    let conversion = match (ty, nullable) {
        (ColumnType::Int64, true) => ".map(|value| value.to_string())",
        (ColumnType::Int64, false) => ".to_string()",
        _ => "",
    };
    writeln!(
        source,
        "                    {field:?}: {carrier}.{name}{conversion},"
    )
    .expect("writing to a String cannot fail");
}

const RECEIPT_CODEC_HEADER: &str = r"#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest { value: JsonValue }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
";

const RECEIPT_CODEC_DECODE_PREFIX: &str = r"
pub(crate) fn decode(input: &str) -> Result<Vec<contract::RecordReceiptItem>, CodecError> {
    decode_envelope(input)?.into_iter().map(|(request_id, body)| {
        let input = match serde_json::from_value::<JsonRequest>(body) {
            Ok(request) => Ok(contract::RecordReceiptRequest {
";

const RECEIPT_CODEC_ERROR_PREFIX: &str = r#"            Err(error) => json!({
                "request_id": item.request_id,
                "error": error_value(error),
            }),
        }
    }).collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed receipt outcomes always serialize")
}

fn error_value(error: &contract::RecordReceiptError) -> Value {
    let (code, detail) = match error {
"#;

const RECEIPT_CODEC_FOOTER: &str = r#"    };
    json!({"code": code, "detail": detail})
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_wit_and_codec_come_from_each_package_manifest() {
        for bytes in [
            include_bytes!("../../../../../apps/wamn_receiving/wamn.json").as_slice(),
            include_bytes!("../../../../../apps/client_acme_receiving/wamn.json").as_slice(),
        ] {
            let manifest: PackageManifest = serde_json::from_slice(bytes).unwrap();
            let operation = &manifest.custom_operations["receiving.record_receipt"];
            let mut files = BTreeMap::new();
            emit_custom_operation_wit(&mut files, &manifest, "receiving.record_receipt", operation)
                .unwrap();
            let package = manifest.package.id.replace('_', "-");
            let wit = std::str::from_utf8(
                &files[&format!("generated/wit/deps/{package}-receiving/package.wit")],
            )
            .unwrap();
            assert!(wit.contains("run: async func"));
            assert!(wit.contains("run-json: async func"));
            if manifest.base_dependencies.is_empty() {
                assert!(
                    wit.contains("input: result<record-receipt-request, invalid-input-detail>")
                );
            }
            let codec =
                std::str::from_utf8(&files["generated/wit/receiving_record_receipt_codec.rs"])
                    .unwrap();
            assert!(codec.contains("row_version.to_string()"));
            assert!(codec.contains("Err(contract::InvalidInputDetail"));
            assert!(codec.contains("RecordReceiptError::QuantityExceedsRemaining"));
        }
    }

    #[test]
    fn receipt_codec_uses_manifest_envelope_bounds() {
        let mut manifest: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../../../../apps/wamn_receiving/wamn.json"
        ))
        .unwrap();
        manifest["custom_operations"]["receiving.record_receipt"]["input"]["envelope"]["minimum"] =
            serde_json::json!(2);
        manifest["custom_operations"]["receiving.record_receipt"]["input"]["envelope"]["maximum"] =
            serde_json::json!(7);
        let manifest: PackageManifest = serde_json::from_value(manifest).unwrap();
        let codec = emit_custom_codec(
            "record_receipt",
            &manifest.custom_operations["receiving.record_receipt"],
        )
        .unwrap();
        assert!(codec.contains("const MINIMUM: usize = 2;"));
        assert!(codec.contains("const MAXIMUM: usize = 7;"));
        assert!(codec.contains("item count must be 2..=7"));
    }
}
