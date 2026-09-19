//! Typed component contracts generated from custom-operation declarations.

use std::fmt::Write as _;

use super::{
    BTreeMap, ColumnType, ContractFieldDeclaration, CustomOperationDeclaration, GenerateError,
    GenerateErrorKind, OperationErrorDetailDeclaration, PackageManifest, insert_bytes,
    rust_identifier, rust_type_identifier,
};
use crate::manifest::OperationErrorDetailKey;

/// Emit the first typed component boundary for the receipt pilot.
///
/// The manifest remains the source of every field, value domain, and error detail. Other
/// operations keep their current interface until the pilot establishes the complete pattern.
pub(super) fn emit_custom_operation_wit(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    if operation_name != "receiving.record_receipt" {
        return Ok(());
    }

    let package = manifest.package.id.replace('_', "-");
    let version = &manifest.package.version;
    let directory = format!("generated/wit/deps/{package}-receiving");
    let source = if let Some((_, dependency)) =
        manifest.base_dependencies.iter().find(|(_, item)| {
            item.operations
                .iter()
                .any(|candidate| candidate == operation_name)
        }) {
        emit_forwarding_interface(&package, version, dependency)
    } else {
        emit_owned_interface(&package, version, manifest, operation)?
    };
    insert_bytes(
        files,
        &format!("{directory}/package.wit"),
        source.into_bytes(),
    )?;
    let codec = emit_receipt_codec(operation)?;
    insert_bytes(
        files,
        "generated/wit/receiving_record_receipt_codec.rs",
        codec.into_bytes(),
    )
}

fn emit_forwarding_interface(
    package: &str,
    version: &str,
    dependency: &crate::manifest::BaseDependencyRequirement,
) -> String {
    let dependency_package = dependency.package.replace('_', "-");
    format!(
        "package {package}:receiving@{version};\n\ninterface record-receipt {{\n  use wamn:node/types@0.1.0.{{emission, node-context, node-error}};\n  use {dependency_package}:receiving/record-receipt@{}.{{record-receipt-item, record-receipt-outcome}};\n\n  run: async func(ctx: node-context, input: list<record-receipt-item>) -> result<list<record-receipt-outcome>, node-error>;\n  run-json: async func(ctx: node-context, input: string) -> result<emission, node-error>;\n}}\n",
        dependency.version
    )
}

fn emit_owned_interface(
    package: &str,
    version: &str,
    manifest: &PackageManifest,
    operation: &CustomOperationDeclaration,
) -> Result<String, GenerateError> {
    let result = operation.result.as_ref().ok_or_else(|| {
        GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            "receiving.record_receipt needs a result for its typed WIT contract",
        )
    })?;
    let mut source = format!("package {package}:receiving@{version};\n\n");
    for operation_name in manifest.custom_operations.keys() {
        let Some(local_name) = operation_name.strip_prefix("receiving.") else {
            continue;
        };
        if operation_name == "receiving.record_receipt" {
            continue;
        }
        writeln!(
            source,
            "interface {} {{\n  use wamn:node/types@0.1.0.{{json, node-context, emission, node-error}};\n\n  run: async func(ctx: node-context, input: json) -> result<emission, node-error>;\n}}\n",
            wit_name(local_name)
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str("interface record-receipt {\n  use wamn:node/types@0.1.0.{emission, node-context, node-error};\n\n");
    source.push_str("  record record-receipt-line {\n");
    emit_record_fields(&mut source, &operation.input.fields, "value.line[].", false);
    source.push_str("  }\n\n  record record-receipt-request {\n");
    emit_record_fields(&mut source, &operation.input.fields, "value.", true);
    source.push_str("    line: list<record-receipt-line>,\n  }\n\n");

    for (literal, detail) in &operation.error_details {
        if !detail.required.is_empty() || !detail.optional.is_empty() {
            emit_error_detail(&mut source, literal, detail);
        }
    }
    source.push_str("  variant record-receipt-error {\n");
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
    source.push_str("  }\n\n  record record-receipt-item {\n    request-id: string,\n    input: result<record-receipt-request, record-receipt-error>,\n  }\n\n");
    source.push_str("  record record-receipt-result {\n");
    emit_record_fields(&mut source, &result.fields, "", false);
    source.push_str("  }\n\n  record record-receipt-outcome {\n    request-id: string,\n    outcome: result<record-receipt-result, record-receipt-error>,\n  }\n\n");
    source.push_str("  run: async func(ctx: node-context, input: list<record-receipt-item>) -> result<list<record-receipt-outcome>, node-error>;\n  run-json: async func(ctx: node-context, input: string) -> result<emission, node-error>;\n}\n");
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
        if path.contains("[]") || path.contains('.') || (omit_lines && path == "line") {
            continue;
        }
        let mut ty = wit_type(field.ty);
        if field.nullable {
            ty = format!("option<{ty}>");
        }
        writeln!(source, "    {}: {ty},", wit_name(path)).expect("writing to a String cannot fail");
    }
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

fn emit_receipt_codec(operation: &CustomOperationDeclaration) -> Result<String, GenerateError> {
    let result = operation.result.as_ref().ok_or_else(|| {
        GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            "receiving.record_receipt needs a result for its typed codec",
        )
    })?;
    let value_fields = codec_fields(&operation.input.fields, "value.");
    let line_fields = codec_fields(&operation.input.fields, "value.line[].");
    let mut source = String::from(RECEIPT_CODEC_HEADER);

    emit_json_struct(&mut source, "JsonValue", &value_fields);
    source.push_str("    line: Vec<JsonLine>,\n}\n\n");
    source.push_str("#[derive(Deserialize)]\n#[serde(deny_unknown_fields)]\n");
    emit_json_struct(&mut source, "JsonLine", &line_fields);
    source.push_str("}\n");
    let envelope = operation.input.envelope.as_ref().ok_or_else(|| {
        GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            "receiving.record_receipt needs envelope bounds for its JSON codec",
        )
    })?;
    source.push_str(
        &RECEIPT_CODEC_DECODE_PREFIX
            .replace("$MINIMUM", &envelope.minimum.to_string())
            .replace("$MAXIMUM", &envelope.maximum.to_string()),
    );
    emit_codec_field_assignments(&mut source, &value_fields, "request.value", 16);
    source.push_str(
        "                line: request.value.line.into_iter().map(|line| contract::RecordReceiptLine {\n",
    );
    emit_codec_field_assignments(&mut source, &line_fields, "line", 20);
    source.push_str(RECEIPT_CODEC_ENCODE_PREFIX);
    for field in &result.fields {
        if field.path.contains("[]") || field.path.contains('.') {
            continue;
        }
        let name = rust_identifier(&field.path).expect("validated result field has a Rust name");
        if field.path == "row_version" {
            writeln!(
                source,
                "                    {:?}: value.{name}.to_string(),",
                field.path
            )
            .expect("writing to a String cannot fail");
        } else {
            writeln!(
                source,
                "                    {:?}: value.{name},",
                field.path
            )
            .expect("writing to a String cannot fail");
        }
    }
    source.push_str(RECEIPT_CODEC_ERROR_PREFIX);
    emit_codec_error_arms(&mut source, operation);
    source.push_str(RECEIPT_CODEC_FOOTER);
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
        ColumnType::Int64 => "i64",
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
    for (_, path) in fields {
        let name = rust_identifier(path).expect("validated input field has a Rust name");
        writeln!(source, "{indentation}{name}: {value}.{name},")
            .expect("writing to a String cannot fail");
    }
}

fn emit_codec_error_arms(source: &mut String, operation: &CustomOperationDeclaration) {
    for literal in &operation.errors {
        let variant = rust_type_identifier(literal);
        let detail = operation
            .error_details
            .get(literal)
            .expect("validated error has a detail declaration");
        if detail.required.is_empty() && detail.optional.is_empty() {
            writeln!(
                source,
                "        contract::RecordReceiptError::{variant} => ({literal:?}, Map::new()),"
            )
            .expect("writing to a String cannot fail");
            continue;
        }

        writeln!(
            source,
            "        contract::RecordReceiptError::{variant}(value) => {{"
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
            writeln!(
                source,
                "            if let Some(detail_value) = &value.{name} {{ detail.insert({name:?}.to_owned(), json!(detail_value)); }}"
            )
            .expect("writing to a String cannot fail");
        }
        writeln!(source, "            ({literal:?}, detail)")
            .expect("writing to a String cannot fail");
        source.push_str("        }\n");
    }
}

const RECEIPT_CODEC_HEADER: &str = r"// @generated from wamn.json; do not edit.

use serde::Deserialize;
use serde_json::{Map, Value, json};

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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest { value: JsonValue }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
";

const RECEIPT_CODEC_DECODE_PREFIX: &str = r#"
pub(crate) fn decode(input: &str) -> Result<Vec<contract::RecordReceiptItem>, CodecError> {
    let Value::Array(values) = serde_json::from_str(input)
        .map_err(|_| CodecError("operation input must be a JSON array"))?
    else { return Err(CodecError("operation input must be a JSON array")); };
    if !($MINIMUM..=$MAXIMUM).contains(&values.len()) {
        return Err(CodecError("operation input item count must be $MINIMUM..=$MAXIMUM"));
    }
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
        let input = match serde_json::from_value::<JsonRequest>(Value::Object(object)) {
            Ok(request) => Ok(contract::RecordReceiptRequest {
"#;

const RECEIPT_CODEC_ENCODE_PREFIX: &str = r#"                }).collect(),
            }),
            Err(_) => Err(contract::RecordReceiptError::InvalidInput(
                contract::InvalidInputDetail {
                    field: "input".to_owned(), minimum: None, maximum: None, observed: None,
                },
            )),
        };
        Ok(contract::RecordReceiptItem { request_id, input })
    }).collect()
}

pub(crate) fn encode(output: &[contract::RecordReceiptOutcome]) -> String {
    let values = output.iter().map(|item| {
        match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
"#;

const RECEIPT_CODEC_ERROR_PREFIX: &str = r#"                }
            }),
            Err(error) => json!({
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
            let codec =
                std::str::from_utf8(&files["generated/wit/receiving_record_receipt_codec.rs"])
                    .unwrap();
            assert!(codec.contains("row_version.to_string()"));
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
        let codec =
            emit_receipt_codec(&manifest.custom_operations["receiving.record_receipt"]).unwrap();
        assert!(codec.contains("(2..=7).contains"));
        assert!(codec.contains("item count must be 2..=7"));
    }
}
