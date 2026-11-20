//! Generated handlers and the generated component world.
//!
//! Each model with generated operations gets `generated/component/<model>.rs`:
//! one module for each operation, with its validation, the `handle` function that
//! calls the generated data function, and its export. The package gets
//! `generated/component/mod.rs`, which declares each model module, and the
//! world `generated/wit/world.wit`, which imports the statements and exports
//! every generated operation.
//!
//! A component includes the modules in a `mod generated` that names its data
//! crate's generated module as `data`. Each operation's validation and handler are
//! visible in the crate, so a component test can call them. Its authored world, in
//! `component/wit/authored.wit`, includes the generated world. The authored
//! world must include it, not the other way round: an authored export names a
//! package under `generated/wit/deps`, and WIT resolves a directory and its
//! dependencies as one group.

use std::collections::BTreeMap;

use proc_macro2::TokenStream;
use quote::quote;
use wit_encoder::{Ident, Package, PackageName, World};

use super::insert_bytes;
use super::rust_source::{insert_rust, rust_ident};
use super::wit::wit_ident;
use wamn_component_policy::PackageManifest;
use wamn_package::manifest::FilterMatch;
use wamn_package::manifest::ManifestError as GenerateError;
use wamn_package::manifest::{
    CrudAction, ModelDeclaration, OperationDeclaration, rust_type_identifier,
};

const HEADER: &str = "// @generated from the package manifest; do not edit.\n\n";

/// The WIT package of the generated world.
pub(crate) const WORLD_PACKAGE: &str = "generated";

/// Emit `generated/component/<model>.rs`.
pub(super) fn emit_model_component(
    files: &mut BTreeMap<String, Vec<u8>>,
    compiled: &super::operations::ModelDescription<'_>,
) -> Result<(), GenerateError> {
    let model_name = compiled.name;
    let namespace = rust_ident(compiled.package.id.as_str());
    let model = rust_ident(model_name.as_str());
    let mut source = quote! {
        #[allow(unused_imports)]
        use super::data::#model as model;
        #[allow(unused_imports)]
        use super::data::error::Error;
        #[allow(unused_imports)]
        use wamn_guest::postgres_statements::{Connection, Transaction};
    };
    for (action, facts) in &compiled.operations {
        let name = rust_ident(action.as_str());
        let stem = rust_type_identifier(action.as_str());
        let request = rust_ident(&[&stem, "Request"].concat());
        let result = rust_ident(&[&stem, "Result"].concat());
        let error = rust_ident(&[&stem, "Error"].concat());
        let body = handler(
            *action,
            &stem,
            facts.request,
            compiled.feed,
            &compiled.shape,
        );
        let (state_argument, state_type, export_state) = if action.changes_records() {
            (quote!(transaction), quote!(Transaction), None)
        } else {
            (
                quote!(connection),
                quote!(Connection),
                Some(quote!(Connection::new(),)),
            )
        };
        let state = quote!(#state_argument: &mut #state_type);
        let path = format!(
            "../wit/{model_name}_{action}_operation.rs",
            action = action.as_str()
        );
        source.extend(quote! {
            pub mod #name {
                #[allow(unused_imports)]
                use super::{Connection, Error, Transaction, model};
                use crate::exports::#namespace::#model::#name as contract;
                pub(crate) mod operation {
                    use super::contract;
                    include!(#path);
                }
                /// Override this action or call `default_handle` from the override.
                pub trait Action {
                    fn run(#state, request: contract::#request) -> impl std::future::Future<Output = Result<contract::#result, contract::#error>> {
                        default_handle(#state_argument, request)
                    }
                }
                /// The generated implementation, available to an override and eject.
                pub async fn default_handle(#state, request: contract::#request) -> Result<contract::#result, contract::#error> {
                    #body
                }
                pub(crate) async fn handle(#state, request: contract::#request) -> Result<contract::#result, contract::#error> {
                    <crate::Component as Action>::run(#state_argument, request).await
                }
                operation::export_operation!(crate::Component, contract, crate::wamn::node::types, #export_state handle, operation);
            }
        });
    }
    insert_rust(
        files,
        &format!("generated/component/{model_name}.rs"),
        HEADER,
        source,
    )
}

fn handler(
    action: CrudAction,
    contract: &str,
    operation: &OperationDeclaration,
    feed: bool,
    shape: &super::operations::ModelShape<'_>,
) -> TokenStream {
    let row = rust_ident(&[contract, "Row"].concat());
    let result = rust_ident(&[contract, "Result"].concat());
    let wrap = quote!(|row| contract::#result { value: operation::row!(row, contract::#row) });
    let refuse =
        quote!(.map_err(|error| operation::map_error(error.literal(), |key| error.detail(key))));
    let id = if shape.key == crate::key::KeyType::Uuid {
        quote!(&request.id)
    } else {
        quote!(request.id)
    };
    let revision = || {
        let revision = operation
            .revision_field
            .as_deref()
            .expect("update and delete validation require a revision field");
        let field = rust_ident(&["expected_", revision].concat());
        quote!(request.#field)
    };
    let is_json = |field: &str| {
        shape
            .column(field)
            .is_some_and(|column| column.column_type() == wamn_scalar::SqlValueType::Json)
    };
    match action {
        CrudAction::Get => quote!(model::get(connection, #id).await.map(#wrap) #refuse),
        CrudAction::Create => {
            let arguments = std::iter::once(quote!(&request.id))
                .filter(|_| shape.key == crate::key::KeyType::Uuid)
                .chain(operation.writable_fields.iter().map(|field| {
                    let member = rust_ident(field);
                    let value = quote!(request.#member.flatten());
                    if is_json(field) {
                        quote!(#value.map(|value| value.encoded_json))
                    } else {
                        value
                    }
                }));
            quote!(model::create(transaction, #(#arguments),*).await.map(#wrap) #refuse)
        }
        CrudAction::Update => {
            let revision = revision();
            let arguments = operation.writable_fields.iter().map(|field| {
                let member = rust_ident(field);
                let value = quote!(request.change.#member);
                if is_json(field) {
                    quote!(, #value.map(|value| value.map(|value| value.encoded_json)))
                } else {
                    quote!(, #value)
                }
            });
            quote! {
                model::update(transaction, #id, #revision #(#arguments)*).await
                    .map(|row| operation::row!(row, contract::#result)) #refuse
            }
        }
        CrudAction::Delete => {
            let revision = revision();
            quote!(model::delete(transaction, #id, #revision).await.map(#wrap) #refuse)
        }
        CrudAction::Query => {
            let members = operation.filters.iter().map(|filter| {
                let member = rust_ident(&filter.field);
                let json = is_json(&filter.field);
                let value = match (filter.match_mode, json) {
                    (FilterMatch::Range, true) => quote! {
                        request.#member.map(|range| model::Range {
                            min: range.min.map(|value| value.encoded_json),
                            max: range.max.map(|value| value.encoded_json)
                        })
                    },
                    (FilterMatch::Range, false) => quote! {
                        request.#member.map(|range| model::Range { min: range.min, max: range.max })
                    },
                    (FilterMatch::IsNull, _) | (_, false) => quote!(request.#member),
                    (_, true) => quote! {
                        request.#member.map(|values| values.into_iter().map(|value| value.encoded_json).collect())
                    },
                };
                quote!(#member: #value,)
            });
            let search = operation
                .search
                .as_ref()
                .map(|_| quote!(search: request.search,));
            let sort = if operation.sort.is_some() {
                quote!(sort_field: request.sort_field, sort_direction: request.sort_direction,)
            } else {
                quote!(sort_field: None, sort_direction: None,)
            };
            let input = quote! {
                let input = model::QueryInput {
                    #(#members)* #search #sort
                    cursor: request.cursor,
                    limit: i64::from(request.limit.expect("the operation fills the default limit")),
                };
                let refuse = |error: Error| operation::map_error(error.literal(), |key| error.detail(key));
            };
            if feed {
                let changes = rust_ident(&[contract, "Changes"].concat());
                let change = rust_ident(&[contract, "Change"].concat());
                let page = rust_ident(&[contract, "Page"].concat());
                let error = rust_ident(&[contract, "Error"].concat());
                let change_id = if shape.key == crate::key::KeyType::Uuid {
                    quote!(change.id.0)
                } else {
                    quote!(change.id)
                };
                quote! {
                    #input
                    if let Some(part) = request.feed {
                        let part = model::FeedInput {
                            cursor: part.cursor,
                            limit: i64::from(part.limit.expect("the operation fills the default limit")),
                        };
                        let changes = model::changes(connection, input, part).await.map_err(refuse)?;
                        return Ok(contract::#result::Changes(contract::#changes {
                            rows: changes.rows.into_iter().map(|change| contract::#change {
                                id: #change_id,
                                row_version: change.row_version,
                                data: change.data.map(|row| operation::row!(row, contract::#row)),
                            }).collect(),
                            next_cursor: changes.next_cursor,
                            complete: changes.complete,
                        }));
                    }
                    let page = model::query(connection, input).await.map_err(refuse)?;
                    let cursor = page.feed_cursor().map(str::to_owned).ok_or(contract::#error::InternalError)?;
                    let (rows, next_cursor) = page.into_parts();
                    Ok(contract::#result::Page(contract::#page {
                        item: rows.into_iter().map(|row| operation::row!(row, contract::#row)).collect(),
                        next_cursor,
                        cursor,
                    }))
                }
            } else {
                quote! {
                    #input
                    let (rows, next_cursor) = model::query(connection, input).await.map_err(refuse)?.into_parts();
                    Ok(contract::#result {
                        item: rows.into_iter().map(|row| operation::row!(row, contract::#row)).collect(),
                        next_cursor,
                    })
                }
            }
        }
    }
}

/// Emit the model and feed modules and the component's WIT world.
pub(super) fn emit_package_component(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
) -> Result<(), GenerateError> {
    let models = manifest
        .models
        .iter()
        .filter(|(_, model)| !model.operations.is_empty())
        .collect::<Vec<_>>();
    let feeds = manifest
        .custom_operations
        .iter()
        .filter(|(_, operation)| operation.feed == Some(true))
        .collect::<Vec<_>>();
    if models.is_empty() && feeds.is_empty() {
        return emit_package_world(files, manifest, &models);
    }
    let modules = models.iter().map(|(name, _)| {
        let doc = format!(" The generated `{name}` handlers.");
        let path = format!("{name}.rs");
        let name = rust_ident(name.as_str());
        quote!(#[doc = #doc] pub mod #name { include!(#path); })
    });
    let feeds = feeds.into_iter().map(|(name, _)| {
        let stem = wamn_package::manifest::custom_artifact_stem(name);
        let doc = format!(" The authored `{name}` handler.");
        let path = format!("{stem}.rs");
        let module = rust_ident(&stem);
        quote!(#[doc = #doc] pub mod #module { include!(#path); })
    });
    insert_rust(
        files,
        "generated/component/mod.rs",
        HEADER,
        quote!(#(#modules)* #(#feeds)*),
    )?;
    emit_package_world(files, manifest, &models)
}

/// Emit `generated/wit/world.wit`: the world `generated`, which exports every
/// operation of the package that the component runs and imports what those
/// operations call. An authored world includes the one it builds, so no
/// authored file names a sealed operation id.
fn emit_package_world(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
    models: &[(
        &wamn_package::manifest::source_name::SourceName,
        &ModelDeclaration,
    )],
) -> Result<(), GenerateError> {
    let package = &manifest.package;
    let namespace = wit_ident(package.id.as_str());
    let mut world = World::new(WORLD_PACKAGE);
    for (name, declaration) in &manifest.connections {
        if !wamn_package::manifest::host_connection_interface(&declaration.interface) {
            let label = wit_ident(name.as_str());
            // The encoder takes the label and path as one identifier.
            world.named_interface_import(format!("{label}: {}", declaration.interface));
        }
    }
    let mut exports = Vec::new();
    let mut postgres = !models.is_empty();
    for (local, operation) in &manifest.custom_operations {
        let (group, name) = local
            .as_str()
            .split_once('.')
            .expect("validated operation identity has one separator");
        let group = wit_ident(group);
        let name = wit_ident(name);
        postgres |= operation.connection.is_some();
        // An operation that bears a base operation's name forwards to it, so
        // the component imports that operation to call it.
        if let Some(base) = manifest
            .base_dependencies
            .values()
            .find(|dependency| dependency.operations.iter().any(|name| name == local))
        {
            world.named_interface_import(operation_interface(
                &wit_ident(base.package.as_str()),
                &group,
                &name,
                &base.version,
            ));
        }
        exports.push(operation_interface(
            &namespace,
            &group,
            &name,
            &package.version,
        ));
    }
    for (model_name, model) in models {
        let group = wit_ident(model_name.as_str());
        for action in model.operations.keys() {
            exports.push(operation_interface(
                &namespace,
                &group,
                &Ident::new(action.as_str()),
                &package.version,
            ));
        }
    }
    if exports.is_empty() && world.items().is_empty() {
        return Ok(());
    }
    if postgres {
        world.named_interface_import("wamn:postgres/types@0.4.0");
        world.named_interface_import("wamn:postgres/statements@0.4.0");
    }
    for export in exports {
        world.named_interface_export(export);
    }

    let mut wit = Package::new(PackageName::new(namespace.to_string(), WORLD_PACKAGE, None));
    wit.world(world);
    let source = format!(
        "{HEADER}// Every operation of the package. An authored world includes it.\n\n{wit}"
    );
    insert_bytes(files, "generated/wit/world.wit", source.into_bytes())
}

fn operation_interface(
    namespace: &Ident,
    group: &Ident,
    name: &Ident,
    version: &wamn_package::package_version::PackageVersion,
) -> Ident {
    Ident::new(format!("{namespace}:{group}/{name}@{version}"))
}
