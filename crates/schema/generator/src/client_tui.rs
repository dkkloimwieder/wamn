//! Emits an operator crate from the declared client contract.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::Value;

use crate::client_ir::{
    ClientContractIr, ModelIr, OperationIr, ReplayIr, leaf_fields, revision_inputs,
};
use crate::client_rust::route_helper_names;
use crate::generate::GeneratedFile;
use crate::manifest::rust_identifier;
use crate::{
    GenerateError, GenerateErrorKind, PackageManifest, canonical_operation_identity,
    validate_operation_vocabulary,
};

/// Why an operator crate could not be emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTuiErrorKind {
    InvalidName,
    NameCollision,
}

/// An emission refusal with the declared name that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTuiError {
    kind: ClientTuiErrorKind,
    name: String,
}

impl ClientTuiError {
    #[must_use]
    pub const fn kind(&self) -> ClientTuiErrorKind {
        self.kind
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl core::fmt::Display for ClientTuiError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let reason = match self.kind {
            ClientTuiErrorKind::InvalidName => "name has no safe generated spelling",
            ClientTuiErrorKind::NameCollision => {
                "name collides with a generated module or operation"
            }
        };
        write!(formatter, "{}: {reason}", self.name)
    }
}

impl std::error::Error for ClientTuiError {}

fn error(kind: ClientTuiErrorKind, name: &str) -> ClientTuiError {
    ClientTuiError {
        kind,
        name: name.to_owned(),
    }
}

fn identifier(name: &str) -> Result<String, ClientTuiError> {
    let valid = name
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && name != "_";
    if valid {
        rust_identifier(name).ok_or_else(|| error(ClientTuiErrorKind::InvalidName, name))
    } else {
        Err(error(ClientTuiErrorKind::InvalidName, name))
    }
}

/// Select the operations exported by one explicitly declared component.
///
/// # Errors
/// Refuses invalid declarations, an unknown component, or a different package.
pub fn component_contract(
    ir: &ClientContractIr,
    manifest: &PackageManifest,
    component: &str,
) -> Result<ClientContractIr, GenerateError> {
    validate_operation_vocabulary(manifest)?;
    if ir.package != manifest.package.id || !manifest.components.contains_key(component) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidComponent,
            format!(
                "{component} must name a component declared by {}",
                ir.package
            ),
        ));
    }
    let implicit = (manifest.components.len() == 1).then_some(component);
    let mut operations = BTreeSet::new();
    for (model, declaration) in &manifest.models {
        for (action, operation) in &declaration.operations {
            if operation.component.as_deref().or(implicit) == Some(component) {
                operations.insert(canonical_operation_identity(
                    &manifest.package,
                    &format!("{model}.{}", action.as_str()),
                )?);
            }
        }
    }
    for (name, operation) in &manifest.custom_operations {
        if operation.component().or(implicit) == Some(component) {
            operations.insert(canonical_operation_identity(&manifest.package, name)?);
        }
    }
    let mut selected = ir.clone();
    for model in &mut selected.models {
        model
            .operations
            .retain(|operation| operations.contains(&operation.operation));
    }
    selected.models.retain(|model| !model.operations.is_empty());
    Ok(selected)
}

/// Emit the selected component's operator beside its package client bindings.
///
/// # Errors
/// Refuses names that cannot form safe paths or distinct Rust identifiers.
pub fn emit_tui(
    ir: &ClientContractIr,
    component: &str,
) -> Result<Vec<GeneratedFile>, ClientTuiError> {
    if !component
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        || !component.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        return Err(error(ClientTuiErrorKind::InvalidName, component));
    }
    let slug = component.replace('_', "-");
    let crate_name = format!("wamn_generated_{}_tui", slug.replace('-', "_"));
    let prefix = format!("generated/{component}-tui");
    let mut files = BTreeMap::new();
    files.insert(format!("{prefix}/Cargo.toml"), cargo_manifest(&slug));
    files.insert(
        format!("{prefix}/src/main.rs"),
        format!(
            "// @generated; do not edit.\n\n#[tokio::main]\nasync fn main() -> Result<wamn_client_terminal::operator::ExitReason, Box<dyn std::error::Error>> {{\n    wamn_client_terminal::operator::run({:?}, {crate_name}::screens).await\n}}\n",
            ir.package
        ),
    );

    let mut library = String::from(
        "// @generated; do not edit.\n\nuse wamn_client_tui::screen::Screen;\nuse wamn_client_tui::submission::SessionBinding;\n\npub mod screens;\n",
    );
    let mut modules = String::from("// @generated; do not edit.\n");
    let mut calls = Vec::new();
    let mut models: Vec<_> = ir.models.iter().collect();
    models.sort_by(|left, right| left.name.cmp(&right.name));
    let mut names = BTreeSet::from(["screens"]);
    for model in models {
        let module = identifier(&model.name)?;
        if !names.insert(&model.name) {
            return Err(error(ClientTuiErrorKind::NameCollision, &model.name));
        }
        writeln!(
            library,
            "\n#[path = {:?}]\npub mod {module};",
            format!("../../client/{}.rs", model.name)
        )
        .expect("write to String");
        writeln!(modules, "pub mod {module};").expect("write to String");
        let (source, functions) = emit_model(model, &module)?;
        for function in functions {
            calls.push(format!("screens::{module}::{function}"));
        }
        files.insert(format!("{prefix}/src/screens/{}.rs", model.name), source);
    }
    writeln!(
        library,
        "\n#[must_use]\npub fn screens(binding: SessionBinding) -> Vec<Screen> {{"
    )
    .expect("write to String");
    if let Some((last, earlier)) = calls.split_last() {
        library.push_str("    vec![\n");
        for call in earlier {
            writeln!(library, "        {call}(binding.clone()),").expect("write to String");
        }
        writeln!(library, "        {last}(binding),").expect("write to String");
        library.push_str("    ]\n");
    } else {
        library.push_str("    let _ = binding;\n    Vec::new()\n");
    }
    library.push_str("}\n");
    files.insert(format!("{prefix}/src/lib.rs"), library);
    files.insert(format!("{prefix}/src/screens/mod.rs"), modules);
    Ok(files
        .into_iter()
        .map(|(path, source)| {
            GeneratedFile::new(
                path.into_boxed_str(),
                source.into_bytes().into_boxed_slice(),
            )
        })
        .collect())
}

fn cargo_manifest(slug: &str) -> String {
    format!(
        "# @generated; do not edit.\n[package]\nworkspace = \"../../../..\"\nname = \"wamn-generated-{slug}-tui\"\nversion.workspace = true\nedition.workspace = true\nlicense.workspace = true\n\n[[bin]]\nname = \"wamn-{slug}-tui\"\npath = \"src/main.rs\"\n\n[dependencies]\nwamn-client = {{ workspace = true }}\nwamn-client-tui = {{ workspace = true }}\nwamn-client-terminal = {{ workspace = true }}\nserde_json = {{ workspace = true }}\nchrono = {{ workspace = true }}\nrust_decimal = {{ workspace = true }}\nuuid = {{ workspace = true }}\ntokio = {{ workspace = true, features = [\"macros\", \"rt-multi-thread\"] }}\n\n[lints]\nworkspace = true\n"
    )
}

fn emit_model(model: &ModelIr, module: &str) -> Result<(String, Vec<String>), ClientTuiError> {
    let mut source = String::from("// @generated; do not edit.\n");
    let mut operations: Vec<_> = model
        .operations
        .iter()
        .filter(|operation| operation.kind != "event_handler")
        .collect();
    operations.sort_by(|left, right| left.name.cmp(&right.name));
    if !operations.is_empty() {
        source.push_str("\nuse wamn_client_tui::{screen, submission};\n");
    }
    let mut functions = Vec::new();
    let route_names = route_helper_names(model);
    let mut names = BTreeSet::new();
    for operation in operations {
        let function = identifier(&operation.name)?;
        if !names.insert(&operation.name) {
            return Err(error(ClientTuiErrorKind::NameCollision, &operation.name));
        }
        let spec = format!("{}_SPEC", operation.name.to_uppercase());
        let fields = format!(
            "crate::{module}::{}_{}",
            model.name.to_uppercase(),
            operation.name.to_uppercase()
        );
        writeln!(
            source,
            "\npub static {spec}: screen::ScreenSpec = screen::ScreenSpec {{"
        )
        .expect("write to String");
        for (key, value) in [
            ("model", &model.name),
            ("name", &operation.name),
            ("operation", &operation.operation),
            ("kind", &operation.kind),
        ] {
            writeln!(source, "    {key}: {value:?},").expect("write to String");
        }
        writeln!(source, "    input: {fields}_INPUT_SCHEMA,").expect("write to String");
        let input_schema = operation
            .route
            .as_ref()
            .and_then(|route| route.input_schema.as_ref());
        writeln!(source, "    input_schema: {:?},", json_text(input_schema))
            .expect("write to String");
        emit_response(&mut source, operation, &fields);
        let route = operation.route.as_ref().map_or_else(
            || "None".to_owned(),
            |_| {
                format!(
                    "Some(crate::{module}::{})",
                    route_names[operation.name.as_str()]
                )
            },
        );
        writeln!(source, "    route: {route},").expect("write to String");
        writeln!(source, "    fresh_only: {},", operation.fresh_only).expect("write to String");
        if let Some(record) = &operation.record {
            writeln!(source, "    record: Some(screen::RecordLink {{ relation: {:?}, key_field: {:?}, key_input: {:?} }}),", record.relation, record.key_field, record.key_input).expect("write to String");
        } else {
            source.push_str("    record: None,\n");
        }
        if let Some(binding) = &operation.revision_binding {
            writeln!(source, "    revision: Some(screen::RevisionBinding {{")
                .expect("write to String");
            for (key, value) in [
                ("read_operation", &binding.read_operation),
                ("read_key_input", &binding.read_key_input),
                ("key_field", &binding.key_field),
                ("revision_field", &binding.revision_field),
                ("command_key_input", &binding.command_key_input),
                ("command_revision_input", &binding.command_revision_input),
            ] {
                writeln!(source, "        {key}: {value:?},").expect("write to String");
            }
            source.push_str("    }),\n");
        } else {
            source.push_str("    revision: None,\n");
        }
        writeln!(
            source,
            "    revision_inputs: &{:?},",
            revision_inputs(operation)
        )
        .expect("write to String");
        writeln!(
            source,
            "    requires_composition: {},",
            operation.requires_composition
        )
        .expect("write to String");
        source.push_str("    supplied: &[\n");
        for field in leaf_fields(&operation.input_fields) {
            let kind = match field.path.as_str() {
                "request_id" => "RequestId",
                "idempotency_key" | "value.idempotency_key" => "IdempotencyKey",
                "occurred_at" | "value.occurred_at" => "OccurredAt",
                _ => continue,
            };
            writeln!(source, "        screen::SuppliedField {{ path: {:?}, kind: screen::SuppliedKind::{kind} }},", field.path).expect("write to String");
        }
        source.push_str("    ],\n};\n");
        writeln!(source, "\n#[must_use]\npub fn {function}(binding: submission::SessionBinding) -> screen::Screen {{\n    screen::Screen::new(&{spec}, binding)\n}}").expect("write to String");
        functions.push(function);
    }
    Ok((source, functions))
}

fn json_text(schema: Option<&Value>) -> Option<String> {
    schema.map(Value::to_string)
}

fn emit_response(source: &mut String, operation: &OperationIr, fields: &str) {
    let route = operation.route.as_ref();
    let response = route.map(|route| &route.response);
    let errors = response.map_or(operation.errors.as_slice(), |response| {
        response.errors.as_slice()
    });
    let result_class = response.map_or(Some(operation.result_class.as_str()), |response| {
        response.result_class.as_deref()
    });
    let schema = response.and_then(|response| response.schema.as_ref());
    let replay = match route.and_then(|route| route.replay) {
        Some(ReplayIr::Claim) => "Claim",
        Some(ReplayIr::State) => "State",
        None => "Unknown",
    };
    source.push_str("    response: submission::ResponseContract {\n");
    writeln!(source, "        schema: {:?},", json_text(schema)).expect("write to String");
    writeln!(
        source,
        "        partial_schema: {:?},",
        json_text(response.and_then(|response| response.partial_schema.as_ref()))
    )
    .expect("write to String");
    writeln!(source, "        fields: {fields}_RESULT_SCHEMA,").expect("write to String");
    writeln!(source, "        result_class: {result_class:?},").expect("write to String");
    source.push_str("        errors: &[\n");
    for error in errors {
        writeln!(source, "            submission::ErrorCase {{ literal: {:?}, required: &{:?}, sources: &{:?} }},", error.literal, error.detail_required, error.sources).expect("write to String");
    }
    source.push_str("        ],\n");
    writeln!(source, "        kind: {:?},", operation.kind).expect("write to String");
    writeln!(source, "        transaction: {:?},", operation.transaction).expect("write to String");
    writeln!(
        source,
        "        direct: {},",
        route.is_some_and(|route| route.direct)
    )
    .expect("write to String");
    writeln!(source, "        replay: submission::Replay::{replay},").expect("write to String");
    source.push_str("    },\n");
}
