//! Generate Rust adapters for model and custom operations.

use std::fmt::Write as _;
use wamn_scalar::SqlValueType;

use super::operations::{CustomDescription, ModelDescription};
use super::typed_exports::{emit_error_mapper, emit_export_adapter, emit_row_adapter};
use super::wit::{access_error_literal, crud_is_keyed, custom_is_keyed, detail_name, model_column};
use super::{BTreeMap, insert_bytes};
use crate::client_ir::FieldIr;
use wamn_catalog::manifest::ManifestError as GenerateError;
use wamn_catalog::manifest::{
    AccessOperationErrorLiteral, ContractFieldDeclaration, CrudAction, FilterMatch,
    OperationDeclaration, OperationErrorDetailDeclaration, OperationErrorDetailKey, authored_name,
    binding_identifier, rust_identifier, rust_type_identifier,
};

pub(super) fn emit_model_adapters(
    files: &mut BTreeMap<String, Vec<u8>>,
    compiled: &ModelDescription<'_>,
) -> Result<(), GenerateError> {
    let model_name = compiled.name;
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

pub(super) fn emit_custom_adapter(
    files: &mut BTreeMap<String, Vec<u8>>,
    group: &str,
    local_name: &str,
    compiled: &CustomDescription<'_>,
) -> Result<(), GenerateError> {
    let export = emit_custom_export(local_name, compiled);
    insert_bytes(
        files,
        &format!(
            "generated/wit/{}_{}_operation.rs",
            artifact_name(group),
            artifact_name(local_name)
        ),
        export.into_bytes(),
    )
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
            .map(|f| (f.path.as_str(), f.ty, f.nullable, None)),
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
        "// @generated from operation declarations; do not edit.\n#[allow(unused_imports)]\npub(crate) use wamn_guest::data_access::InputError;\n\n",
    )
}

fn scalar_refusal(type_name: &str, value: &str) -> Option<String> {
    if type_name == "json" {
        return Some(format!(
            "wamn_guest::data_access::scalar::json(&({value}).encoded_json).map(|parsed| {{ ({value}).encoded_json = parsed.0; }}).is_err()"
        ));
    }
    matches!(type_name, "uuid" | "numeric" | "timestamptz").then(|| {
        format!("wamn_guest::data_access::scalar::{type_name}({value}).map(|parsed| {{ *{value} = parsed.0; }}).is_err()")
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
    wamn_guest::data_access::operation::call(request, state, |request| normalize(request).and_then(|()| limit(request)), contract::QueryError::InvalidInput, handler).await
}
";

/// The typed export of every query. The host retains its read metadata.
const QUERY_EXPORT: &str = "pub(crate) type Input = contract::QueryRequest;\npub(crate) type Output = contract::QueryOutcome;\npub(crate) use wamn_guest::export_query as export_operation;\n";

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
    wamn_guest::data_access::operation::dispatch(input.into_iter().map(|item| ({key}, item.input)), state, normalize, contract::{type_name}Error::InvalidInput, handler).await.into_iter().map(|(request_id, outcome)| {{ let _ = &request_id; contract::{type_name}Outcome {{ {outcome} }} }}).collect()
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
        "#[allow(dead_code)]\npub(crate) fn validate(input: &[contract::{item}]) -> Result<(), InputError> {{\nwamn_guest::data_access::operation::validate_envelope(input.len(), {minimum}..={maximum}, {correlations})\n}}\n"
    )
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
        let error_name = format!("{name}Error");
        source.push_str(&emit_row_adapter(result.fields.iter().map(|field| {
            let enum_error = (operation.feed == Some(true) && !field.values.is_empty())
                .then_some(error_name.as_str());
            (field.path.as_str(), field.ty, field.nullable, enum_error)
        })));
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
