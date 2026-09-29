//! Generated handlers and the generated component world.
//!
//! Each model with generated operations gets `generated/component/<model>.rs`:
//! one module for each operation, with its codec, the `handle` function that
//! calls the generated data function, and its export. The package gets
//! `generated/component/mod.rs`, which declares each model module, and the
//! world `generated/wit/world.wit`, which imports the statements and exports
//! every generated operation.
//!
//! A component includes the modules in a `mod generated` that names its data
//! crate's generated module as `data`. Each operation's codec and handler are
//! visible in the crate, so a component test can call them. Its authored world, in
//! `component/wit/authored.wit`, includes the generated world. The authored
//! world must include it, not the other way round: an authored export names a
//! package under `generated/wit/deps`, and WIT resolves a directory and its
//! dependencies as one group.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::{
    CrudAction, GenerateError, ModelDeclaration, OperationDeclaration, PackageManifest,
    binding_identifier, insert_bytes, rust_identifier, rust_type_identifier,
};
use crate::manifest::{
    CommandIdempotence, CommandTransaction, CustomOperationDeclaration, FilterMatch,
    InheritedClaimDeclaration, PackageIdentity, canonical_operation_prefix,
};

const HEADER: &str = "// @generated from the package manifest; do not edit.\n\n";

/// The WIT package of the generated world.
pub(crate) const WORLD_PACKAGE: &str = "generated";

/// The spelling wit-bindgen gives a WIT package or interface name in a Rust
/// module path.
fn module_name(value: &str) -> String {
    value.replace('-', "_")
}

fn field_name(field: &str) -> String {
    rust_identifier(field).expect("model field names were validated for Rust")
}

fn member_name(field: &str) -> String {
    binding_identifier(field).expect("model field names were validated for Rust")
}

/// Emit `generated/component/<model>.rs`.
pub(super) fn emit_model_component(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
) -> Result<(), GenerateError> {
    if model.operations.is_empty() {
        return Ok(());
    }
    let namespace = module_name(&manifest.package.id);
    let package = module_name(model_name);
    let mut source = String::from(HEADER);
    writeln!(
        source,
        "// The generated `{model_name}` handlers.\n\n\
         #[allow(unused_imports)]\n\
         use super::data::{model_name} as model;\n\
         #[allow(unused_imports)]\n\
         use super::data::error::Error;\n\
         #[allow(unused_imports)]\n\
         use wamn_postgres_statements::{{Connection, Transaction}};\n"
    )
    .expect("writing to a String cannot fail");
    for (action, operation) in &model.operations {
        let name = action.as_str();
        let contract = rust_type_identifier(name);
        let (state, signature, body) = handler(*action, &contract, operation);
        // A change works in the host's operation transaction, so its export
        // takes no state.
        let export_state = if action.changes_records() {
            ""
        } else {
            " Connection::new(),"
        };
        writeln!(
            source,
            "pub(crate) mod {name} {{\n\
             #[allow(unused_imports)]\n\
             use super::{{Connection, Error, Transaction, model}};\n\
             use crate::exports::{namespace}::{package}::{name} as contract;\n\
             pub(crate) mod codec {{\n\
             use super::contract;\n\
             include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../generated/wit/{model_name}_{name}_codec.rs\"));\n\
             }}\n\n\
             pub(crate) async fn handle({state}, request: contract::{contract}Request{signature}) -> Result<contract::{contract}{end}, contract::{contract}Error> {{\n\
             {body}\
             }}\n\
             codec::export_operation!(crate::Component, contract, crate::wamn::node::types,{export_state} handle, codec);\n\
             }}\n",
            end = if *action == CrudAction::Query { "End" } else { "Result" },
        )
        .expect("writing to a String cannot fail");
    }
    while source.ends_with("\n\n") {
        source.pop();
    }
    insert_bytes(
        files,
        &format!("generated/component/{model_name}.rs"),
        source.into_bytes(),
    )
}

const REFUSE: &str = ".map_err(|error| codec::map_error(error.literal(), |key| error.detail(key)))";

/// The state parameter, the rest of the signature, and the body of one
/// handler.
fn handler(
    action: CrudAction,
    contract: &str,
    operation: &OperationDeclaration,
) -> (&'static str, String, String) {
    let connection = "connection: &mut Connection";
    let transaction = "transaction: &mut Transaction";
    let revision = || {
        let revision = operation
            .revision_field
            .as_deref()
            .expect("update and delete validation require a revision field");
        format!("request.expected_{}", member_name(revision))
    };
    match action {
        CrudAction::Get => (
            connection,
            String::new(),
            format!(
                "model::get(connection, &request.id).await.map(|row| contract::{contract}Result {{ value: codec::row!(row, contract::{contract}Row) }}){REFUSE}\n"
            ),
        ),
        CrudAction::Create => {
            let arguments = operation
                .writable_fields
                .iter()
                .map(|field| format!("request.{}.flatten()", member_name(field)))
                .collect::<Vec<_>>()
                .join(", ");
            (
                transaction,
                String::new(),
                format!(
                    "model::create(transaction, {arguments}).await.map(|row| contract::{contract}Result {{ value: codec::row!(row, contract::{contract}Row) }}){REFUSE}\n"
                ),
            )
        }
        CrudAction::Update => {
            let mut arguments = format!("&request.id, {}", revision());
            for field in &operation.writable_fields {
                write!(arguments, ", request.change.{}", member_name(field))
                    .expect("writing to a String cannot fail");
            }
            (
                transaction,
                String::new(),
                format!(
                    "model::update(transaction, {arguments}).await.map(|row| codec::row!(row, contract::{contract}Result)){REFUSE}\n"
                ),
            )
        }
        CrudAction::Delete => (
            transaction,
            String::new(),
            format!(
                "model::delete(transaction, &request.id, {}).await.map(|row| contract::{contract}Result {{ value: codec::row!(row, contract::{contract}Row) }}){REFUSE}\n",
                revision()
            ),
        ),
        CrudAction::Query => {
            let mut members = String::new();
            for filter in &operation.filters {
                let value = if matches!(filter.match_mode, FilterMatch::Range) {
                    format!(
                        "request.{}.map(|range| model::Range {{ min: range.min, max: range.max }})",
                        member_name(&filter.field)
                    )
                } else {
                    format!("request.{}", member_name(&filter.field))
                };
                writeln!(members, "{}: {value},", field_name(&filter.field))
                    .expect("writing to a String cannot fail");
            }
            if operation.search.is_some() {
                members.push_str("search: request.search,\n");
            }
            // A query that declares no sort takes its default one, and its
            // request carries no sort members.
            if operation.sort.is_some() {
                members.push_str(
                    "sort_field: request.sort_field,\nsort_direction: request.sort_direction,\n",
                );
            } else {
                members.push_str("sort_field: None,\nsort_direction: None,\n");
            }
            (
                connection,
                ", rows: &mut codec::Rows".to_owned(),
                format!(
                    "let input = model::QueryInput {{\n\
                     {members}\
                     cursor: request.cursor,\n\
                     limit: request.limit.expect(\"the codec fills the default limit\"),\n\
                     }};\n\
                     let refuse = |error: Error| codec::map_error(error.literal(), |key| error.detail(key));\n\
                     let mut page = model::query(connection, input).await.map_err(refuse)?;\n\
                     while let Some(row) = page.next().await.map_err(refuse)? {{\n\
                     rows.push(codec::row!(row, contract::{contract}Row)).await?;\n\
                     }}\n\
                     Ok(contract::{contract}End {{ next_cursor: page.next_cursor() }})\n"
                ),
            )
        }
    }
}

/// Emit `generated/component/mod.rs`, when any model of the package has a
/// generated operation, and `generated/wit/world.wit`, when the package has
/// any operation.
pub(super) fn emit_package_component(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
) -> Result<(), GenerateError> {
    let models = manifest
        .models
        .iter()
        .filter(|(_, model)| !model.operations.is_empty())
        .collect::<Vec<_>>();
    if models.is_empty() {
        return emit_package_world(files, manifest, &models);
    }
    let mut source = String::from(HEADER);
    source.push_str(
        "// The generated handlers. The including module names its data crate's\n\
         // generated module as `data`.\n",
    );
    for (model_name, _) in &models {
        writeln!(
            source,
            "\n/// The generated `{model_name}` handlers.\npub mod {model_name} {{\n    include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../generated/component/{model_name}.rs\"));\n}}"
        )
        .expect("writing to a String cannot fail");
    }
    insert_bytes(files, "generated/component/mod.rs", source.into_bytes())?;
    emit_package_world(files, manifest, &models)
}

/// Emit `generated/wit/world.wit`: the world `generated`, which exports every
/// operation of the package that the component runs and imports what those
/// operations call, and the world `participant`, when the package has a
/// participant. An authored world includes the one it builds, so no authored
/// file names a sealed operation id.
fn emit_package_world(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    models: &[(&String, &ModelDeclaration)],
) -> Result<(), GenerateError> {
    const POSTGRES: &str =
        "  import wamn:postgres/types@0.3.0;\n  import wamn:postgres/statements@0.3.0;\n";
    let package = &manifest.package;
    let sealed = |package: &PackageIdentity, local: &str| -> Result<String, GenerateError> {
        let (group, name) = local
            .split_once('.')
            .expect("validated operation identity has one separator");
        Ok(format!(
            "{}{}/{}@{}",
            canonical_operation_prefix(package)?,
            super::wit::wit_name(group),
            super::wit::wit_name(name),
            package.version
        ))
    };
    let base = |alias: &str| -> PackageIdentity {
        let base = &manifest.base_dependencies[alias];
        PackageIdentity {
            id: base.package.clone(),
            version: base.version.clone(),
            predecessor_version: None,
        }
    };
    // The custom lines come first, where the authored world had them, so each
    // component keeps its world byte for byte.
    let mut imports = String::new();
    let mut exports = String::new();
    let mut participant = String::new();
    let mut postgres = !models.is_empty();
    for (local, operation) in &manifest.custom_operations {
        if operation.transaction == Some(CommandTransaction::Participant) {
            let inherited = inherited(operation).expect("a participant inherits its base claim");
            let (group, name) = inherited
                .operation
                .split_once('.')
                .expect("validated operation identity has one separator");
            writeln!(
                participant,
                "  export {};\n  export {};",
                sealed(
                    &base(&inherited.base),
                    &format!("{group}.{name}_pre_commit")
                )?,
                sealed(package, local)?
            )
            .expect("writing to a String cannot fail");
            continue;
        }
        postgres |= operation.connection.is_some();
        if operation.pre_commit.is_some() {
            writeln!(
                imports,
                "  import {};",
                sealed(package, &format!("{local}_pre_commit"))?
            )
            .expect("writing to a String cannot fail");
        }
        if operation.participant.is_some()
            && let Some(inherited) = inherited(operation)
        {
            writeln!(
                imports,
                "  import {};",
                sealed(&base(&inherited.base), &inherited.operation)?
            )
            .expect("writing to a String cannot fail");
        }
        writeln!(exports, "  export {};", sealed(package, local)?)
            .expect("writing to a String cannot fail");
    }
    for (model_name, model) in models {
        for action in model.operations.keys() {
            writeln!(
                exports,
                "  export {};",
                sealed(package, &format!("{model_name}.{}", action.as_str()))?
            )
            .expect("writing to a String cannot fail");
        }
    }
    if exports.is_empty() {
        return Ok(());
    }

    let namespace = package.id.replace('_', "-");
    let postgres = if postgres { POSTGRES } else { "" };
    let mut world = format!(
        "// @generated from the package manifest; do not edit.\n\n\
         package {namespace}:{WORLD_PACKAGE};\n\n\
         /// Every operation of the package. An authored world includes it.\n\
         world {WORLD_PACKAGE} {{\n{imports}{postgres}{exports}}}\n"
    );
    if !participant.is_empty() {
        write!(
            world,
            "\n/// The participant of the package. An authored participant world includes it.\n\
             world participant {{\n{POSTGRES}{participant}}}\n"
        )
        .expect("writing to a String cannot fail");
    }
    insert_bytes(files, "generated/wit/world.wit", world.into_bytes())
}

/// The base claim an operation inherits, when it inherits one.
fn inherited(operation: &CustomOperationDeclaration) -> Option<&InheritedClaimDeclaration> {
    match &operation.idempotent_by {
        Some(CommandIdempotence::Inherited(inherited)) => Some(inherited),
        _ => None,
    }
}
