//! Standard adapters between application values and declared typed contracts.

use std::fmt::Write as _;

use super::{ColumnType, OperationErrorDetailDeclaration, rust_identifier, rust_type_identifier};
use crate::manifest::OperationErrorDetailKey;

/// Emit a row conversion without requiring an application crate dependency.
pub(super) fn emit_row_adapter<'a>(
    fields: impl IntoIterator<Item = (&'a str, ColumnType, bool)>,
) -> String {
    let mut source = String::from(
        "\n#[allow(unused_macros)]\nmacro_rules! row {\n    ($row:expr, $target:path) => {{\n        let row = $row;\n        $target {\n",
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
        let value = if nullable && value != "value" {
            format!("row.{field}.map(|value| {value})")
        } else {
            value.replace("value", &format!("row.{field}"))
        };
        writeln!(source, "            {}: {value},", wit_rust_member(name))
            .expect("writing to a String cannot fail");
    }
    source.push_str("        }\n    }};\n}\n#[allow(unused_imports)]\npub(crate) use row;\n");
    source
}

/// Emit the declared error vocabulary and expose only permitted details.
pub(super) fn emit_error_mapper<'a>(
    error_type: &str,
    cases: impl IntoIterator<Item = (&'a str, &'a OperationErrorDetailDeclaration)>,
    revision: Option<ColumnType>,
) -> String {
    // A revision detail parses at the width its own column declares.
    let revision_type = if revision == Some(ColumnType::Int32) {
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
            let value = format!("detail({name:?})");
            let numeric = matches!(
                key,
                OperationErrorDetailKey::Minimum
                    | OperationErrorDetailKey::Maximum
                    | OperationErrorDetailKey::Observed
                    | OperationErrorDetailKey::ExpectedRowVersion
                    | OperationErrorDetailKey::ObservedRowVersion
            );
            let parsed = if matches!(
                key,
                OperationErrorDetailKey::ExpectedRowVersion
                    | OperationErrorDetailKey::ObservedRowVersion
            ) {
                revision_type
            } else {
                "i64"
            };
            if required {
                let value = if numeric {
                    format!("{value}.and_then(|value| value.parse::<{parsed}>().ok())")
                } else {
                    value
                };
                writeln!(source, "            let Some({name}) = {value} else {{ return contract::{error_type}::InternalError; }};")
                    .expect("writing to a String cannot fail");
            } else if numeric {
                writeln!(source, "            let Ok({name}) = {value}.map(|value| value.parse::<{parsed}>()).transpose() else {{ return contract::{error_type}::InternalError; }};")
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

/// Emit component exports around an application's typed handler.
pub(super) fn emit_export_adapter(type_name: &str, direct: bool, pass_context: bool) -> String {
    let input_type = if direct {
        format!("__contract::{type_name}Request")
    } else {
        format!("Vec<__contract::{type_name}Item>")
    };
    let output_type = if direct {
        input_type.clone()
    } else {
        format!("Vec<__contract::{type_name}Outcome>")
    };
    let run = if direct {
        "let mut input = input;
                    __codec::normalize(&mut input).map_err(invalid)?;
                    $handler(&mut state, input).await"
            .to_owned()
    } else if pass_context {
        "__codec::validate(&input).map_err(invalid)?;
                    Ok(__codec::run(input, &mut state, async |state, request| $handler(context.clone(), state, request).await).await)"
            .to_owned()
    } else {
        "__codec::validate(&input).map_err(invalid)?;
                    Ok(__codec::run(input, &mut state, $handler).await)"
            .to_owned()
    };
    let run_json = if direct {
        "let raw = input;\n                    let input = __codec::decode(&raw).map_err(invalid)?;\n                    <Self as __contract::Guest>::run(context, input).await?;\n                    Ok(__node::Emission { payload: raw, port: None })"
            .to_owned()
    } else {
        "let input = __codec::decode(&input).map_err(invalid)?;\n                    let output = <Self as __contract::Guest>::run(context, input).await?;\n                    Ok(__node::Emission {\n                        payload: __codec::encode(&output),\n                        port: None,\n                    })"
            .to_owned()
    };
    let context_parameter = if pass_context { "context" } else { "_context" };
    format!(
        r#"
#[allow(unused_macros)]
macro_rules! export_operation {{
    ($component:ty, $contract:path, $node:path, $state:expr, $handler:path, $codec:ident) => {{
        const _: () = {{
            use $contract as __contract;
            use $node as __node;
            use $codec as __codec;

            fn invalid(error: __codec::CodecError) -> __node::NodeError {{
                __node::NodeError::InvalidInput(__node::ErrorDetail {{
                    message: error.context().to_owned(),
                    code: Some("invalid_input".to_owned()),
                }})
            }}

            impl __contract::Guest for $component {{
                async fn run(
                    {context_parameter}: __node::NodeContext,
                    input: {input_type},
                ) -> Result<{output_type}, __node::NodeError> {{
                    let mut state = $state;
                    {run}
                }}

                async fn run_json(
                    context: __node::NodeContext,
                    input: String,
                ) -> Result<__node::Emission, __node::NodeError> {{
                    {run_json}
                }}
            }}
        }};
    }};
}}
#[allow(unused_imports)]
pub(crate) use export_operation;
"#
    )
}

/// WIT's Rust binding suffixes escaped keywords; SQL rows use raw identifiers.
pub(super) fn wit_rust_member(name: &str) -> String {
    let member = rust_identifier(name).expect("validated field has a Rust name");
    match member.strip_prefix("r#") {
        Some(keyword) => format!("{keyword}_"),
        None => member,
    }
}

#[cfg(test)]
mod keyword_tests {
    use super::*;

    #[test]
    fn type_crosses_the_sql_to_wit_boundary_without_changing_its_wire_name() {
        assert_eq!(wit_rust_member("type"), "type_");
        let source = emit_row_adapter([("type", ColumnType::Text, false)]);
        assert!(source.contains("type_: row.r#type,"));
    }
}
