//! Standard adapters between application values and declared typed contracts.

use std::fmt::Write as _;
use wamn_scalar::SqlValueType;

use crate::manifest::OperationErrorDetailKey;
use crate::manifest::{
    OperationErrorDetailDeclaration, binding_identifier, rust_identifier, rust_type_identifier,
};

/// Emit a row conversion without requiring an application crate dependency.
pub(super) fn emit_row_adapter<'a>(
    fields: impl IntoIterator<Item = (&'a str, SqlValueType, bool)>,
) -> String {
    let mut source = String::from(
        "\n#[allow(unused_macros)]\nmacro_rules! row {\n    ($row:expr, $target:path) => {{\n        let row = $row;\n        $target {\n",
    );
    for (name, ty, nullable) in fields {
        // A list result field takes each element the way a single field of
        // its type does.
        let list = name
            .strip_suffix("[]")
            .filter(|member| !member.contains('.') && !member.contains("[]"));
        let field = rust_identifier(list.unwrap_or(name)).expect("validated field has a Rust name");
        let member =
            binding_identifier(list.unwrap_or(name)).expect("validated field has a Rust name");
        let value = match ty {
            SqlValueType::Uuid | SqlValueType::Numeric | SqlValueType::Timestamptz => "value.0",
            SqlValueType::Json => "crate::wamn::node::types::JsonValue { encoded_json: value.0 }",
            SqlValueType::Boolean
            | SqlValueType::Int32
            | SqlValueType::Int64
            | SqlValueType::Float64
            | SqlValueType::Text
            | SqlValueType::Bytes => "value",
        };
        let value = if list.is_some() {
            if value == "value" {
                format!("row.{field}")
            } else if nullable {
                format!("row.{field}.into_iter().map(|value| value.map(|value| {value})).collect()")
            } else {
                format!("row.{field}.into_iter().map(|value| {value}).collect()")
            }
        } else if nullable && value != "value" {
            format!("row.{field}.map(|value| {value})")
        } else {
            value.replace("value", &format!("row.{field}"))
        };
        writeln!(source, "            {member}: {value},")
            .expect("writing to a String cannot fail");
    }
    source.push_str("        }\n    }};\n}\n#[allow(unused_imports)]\npub(crate) use row;\n");
    source
}

/// Emit the declared error vocabulary and expose only permitted details.
pub(super) fn emit_error_mapper<'a>(
    error_type: &str,
    cases: impl IntoIterator<Item = (&'a str, &'a OperationErrorDetailDeclaration)>,
    revision: Option<SqlValueType>,
    guarded_fields: &std::collections::BTreeMap<String, String>,
) -> String {
    // A revision detail parses at the width its own column declares.
    let revision_type = if revision == Some(SqlValueType::Int32) {
        "i32"
    } else {
        "i64"
    };
    let mut source = format!(
        "\n#[allow(dead_code)]\npub(crate) fn map_error(code: &str, mut detail: impl FnMut(&str) -> Option<String>) -> contract::{error_type} {{\n    match code {{\n"
    );
    for (literal, declaration) in cases {
        if literal == "internal_error" {
            continue;
        }
        let variant = rust_type_identifier(literal);
        writeln!(source, "        {literal:?} => {{").expect("writing to a String cannot fail");
        for (key, required) in declaration
            .required
            .iter()
            .map(|key| (*key, true))
            .chain(declaration.optional.iter().map(|key| (*key, false)))
        {
            let name = detail_name(key);
            let mut value = format!("detail({name:?})");
            if key == OperationErrorDetailKey::Field
                && !required
                && declaration
                    .required
                    .contains(&OperationErrorDetailKey::Constraint)
                && !guarded_fields.is_empty()
            {
                value.push_str(".or_else(|| match constraint.as_str() {");
                for (constraint, field) in guarded_fields {
                    write!(value, "{constraint:?} => Some({field:?}.to_owned()),")
                        .expect("write constraint field");
                }
                value.push_str("_ => None })");
            }
            // Only a revision is a number; a bound or observed value stays
            // the decimal string its producer wrote.
            let numeric = matches!(
                key,
                OperationErrorDetailKey::ExpectedRowVersion
                    | OperationErrorDetailKey::ObservedRowVersion
            );
            if required {
                let value = if numeric {
                    format!("{value}.and_then(|value| value.parse::<{revision_type}>().ok())")
                } else {
                    value
                };
                writeln!(source, "            let Some({name}) = {value} else {{ return contract::{error_type}::InternalError; }};")
                    .expect("writing to a String cannot fail");
            } else if numeric {
                writeln!(source, "            let Ok({name}) = {value}.map(|value| value.parse::<{revision_type}>()).transpose() else {{ return contract::{error_type}::InternalError; }};")
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

/// Emit an operation export that validates before calling the application.
pub(super) fn emit_export_adapter(type_name: &str, direct: bool, transacted: bool) -> String {
    let (input, output, export) = if direct {
        (
            format!("contract::{type_name}Request"),
            format!("contract::{type_name}Request"),
            "export_direct",
        )
    } else {
        (
            format!("Vec<contract::{type_name}Item>"),
            format!("Vec<contract::{type_name}Outcome>"),
            if transacted {
                "export_transaction"
            } else {
                "export_batch"
            },
        )
    };
    let error = if direct {
        String::new()
    } else {
        format!(
            "#[allow(dead_code)]\npub(crate) type OperationError = contract::{type_name}Error;\n"
        )
    };
    format!(
        "pub(crate) type Input = {input};\npub(crate) type Output = {output};\n{error}pub(crate) use wamn_data_access::{export} as export_operation;\n"
    )
}
