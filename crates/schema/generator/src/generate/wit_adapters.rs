//! Standard adapters between application values and declared typed contracts.

use std::fmt::Write as _;

use super::{ColumnType, OperationErrorDetailDeclaration, rust_identifier, rust_type_identifier};
use crate::manifest::OperationErrorDetailKey;

/// Emit a row conversion without requiring an application crate dependency.
pub(super) fn emit_row_adapter<'a>(
    fields: impl IntoIterator<Item = (&'a str, ColumnType, bool)>,
) -> String {
    let mut source = String::from(
        "\nmacro_rules! row {\n    ($row:expr, $target:path) => {{\n        let row = $row;\n        $target {\n",
    );
    for (name, ty, nullable) in fields {
        let field = rust_identifier(name).expect("validated field has a Rust name");
        let value = match ty {
            ColumnType::Uuid | ColumnType::Numeric | ColumnType::Timestamptz | ColumnType::Json => {
                "value.0"
            }
            ColumnType::Boolean
            | ColumnType::Int32
            | ColumnType::Int64
            | ColumnType::Float64
            | ColumnType::Text
            | ColumnType::Bytes => "value",
        };
        let value = if nullable {
            format!("row.{field}.map(|value| {value})")
        } else {
            value.replace("value", &format!("row.{field}"))
        };
        writeln!(source, "            {field}: {value},").expect("writing to a String cannot fail");
    }
    source.push_str("        }\n    }};\n}\npub(crate) use row;\n");
    source
}

/// Emit the declared error vocabulary and expose only permitted details.
pub(super) fn emit_error_mapper<'a>(
    error_type: &str,
    cases: impl IntoIterator<Item = (&'a str, &'a OperationErrorDetailDeclaration)>,
) -> String {
    let mut source = format!(
        "\npub(crate) fn map_error(code: &str, mut detail: impl FnMut(&str) -> Option<String>) -> contract::{error_type} {{\n    match code {{\n"
    );
    for (literal, declaration) in cases {
        let variant = rust_type_identifier(literal);
        writeln!(source, "        {literal:?} => {{").expect("writing to a String cannot fail");
        for (key, required) in declaration
            .required
            .iter()
            .map(|key| (*key, true))
            .chain(declaration.optional.iter().map(|key| (*key, false)))
        {
            let name = detail_name(key);
            let value = format!("detail({name:?})");
            let numeric = matches!(
                key,
                OperationErrorDetailKey::Minimum
                    | OperationErrorDetailKey::Maximum
                    | OperationErrorDetailKey::Observed
            );
            if required {
                let value = if numeric {
                    format!("{value}.and_then(|value| value.parse::<i64>().ok())")
                } else {
                    value
                };
                writeln!(source, "            let Some({name}) = {value} else {{ return contract::{error_type}::InternalError; }};")
                    .expect("writing to a String cannot fail");
            } else if numeric {
                writeln!(source, "            let Ok({name}) = {value}.map(|value| value.parse::<i64>()).transpose() else {{ return contract::{error_type}::InternalError; }};")
                    .expect("writing to a String cannot fail");
            } else {
                writeln!(source, "            let {name} = {value};")
                    .expect("writing to a String cannot fail");
            }
        }
        if declaration.required.is_empty() && declaration.optional.is_empty() {
            writeln!(source, "            contract::{error_type}::{variant}")
                .expect("writing to a String cannot fail");
        } else {
            writeln!(
                source,
                "            contract::{error_type}::{variant}(contract::{variant}Detail {{"
            )
            .expect("writing to a String cannot fail");
            for key in declaration.required.iter().chain(&declaration.optional) {
                writeln!(source, "                {},", detail_name(*key))
                    .expect("writing to a String cannot fail");
            }
            source.push_str("            })\n");
        }
        source.push_str("        }\n");
    }
    writeln!(
        source,
        "        _ => contract::{error_type}::InternalError,\n    }}\n}}"
    )
    .expect("writing to a String cannot fail");
    source
}

fn detail_name(key: OperationErrorDetailKey) -> &'static str {
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
