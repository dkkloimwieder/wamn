//! Authored feeding reads use the generated snapshot and cursor contract.

use quote::quote;
use std::collections::BTreeMap;
use wamn_scalar::SqlValueType;

use super::rust::projected_rust_type;
use super::rust_source::{insert_rust, rust_ident};
use super::{CatalogIr, sql};
use wamn_package::manifest::ManifestError as GenerateError;
use wamn_package::manifest::{CustomOperationDeclaration, rust_type_identifier};
use wamn_package::manifest::{
    StaticSqlFetch, StaticSqlStatementDeclaration, StaticSqlValueDeclaration, custom_artifact_stem,
};

fn value(
    name: wamn_package::manifest::source_name::SourceName,
    ty: SqlValueType,
    nullable: bool,
) -> StaticSqlValueDeclaration {
    StaticSqlValueDeclaration { name, ty, nullable }
}

/// Add the snapshot statements and the host data function for one authored read.
pub(super) fn prepare_sql(
    corpus: &mut BTreeMap<String, Vec<u8>>,
    catalog: &CatalogIr,
    name: &wamn_package::local_operation::LocalOperation,
    operation: &CustomOperationDeclaration,
) -> Option<CustomOperationDeclaration> {
    if operation.feed != Some(true) {
        return None;
    }
    let source = operation
        .feed_source
        .as_ref()
        .expect("validated authored feed source");
    let statement = operation
        .statements
        .values()
        .next()
        .expect("validated authored statement");
    let text = std::str::from_utf8(&corpus[&statement.path]).expect("validated authored SQL");
    let fields = statement
        .row
        .iter()
        .map(|field| field.name.to_string())
        .collect::<Vec<_>>();
    let key = &operation
        .lists
        .as_ref()
        .expect("validated feed list")
        .key_field[0];
    let key_type = statement
        .row
        .iter()
        .find(|field| &field.name == key)
        .expect("validated root key")
        .ty;
    let stem = custom_artifact_stem(name);
    let page_path = format!("generated/sql/{stem}/feed_page.sql");
    let changes_path = format!("generated/sql/{stem}/feed_changes.sql");
    for (path, text) in [
        (&page_path, sql::authored_page(text, &fields)),
        (
            &changes_path,
            sql::authored_changes(
                catalog,
                source,
                text,
                key,
                &fields,
                statement.parameters.len() + 1,
            ),
        ),
    ] {
        corpus.insert(path.clone(), text.into_bytes());
    }
    let mut prepared = operation.clone();
    prepared.statements.clear();
    let nullable = statement
        .row
        .iter()
        .map(|field| value(field.name.clone(), field.ty, true))
        .collect::<Vec<_>>();
    prepared.statements.insert(
        "feed_page".parse().expect("fixed identifier"),
        StaticSqlStatementDeclaration {
            path: page_path,
            fetch: StaticSqlFetch::BoundedList,
            parameters: statement.parameters.clone(),
            row: std::iter::once(value(
                "feed_cursor".parse().expect("fixed identifier"),
                SqlValueType::Int64,
                true,
            ))
            .chain(nullable.clone())
            .collect(),
        },
    );
    prepared.statements.insert(
        "feed_changes".parse().expect("fixed identifier"),
        StaticSqlStatementDeclaration {
            path: changes_path,
            fetch: StaticSqlFetch::BoundedList,
            parameters: statement
                .parameters
                .iter()
                .cloned()
                .chain([
                    value(
                        "cursor_transaction".parse().expect("fixed identifier"),
                        SqlValueType::Int64,
                        true,
                    ),
                    value(
                        "cursor_id".parse().expect("fixed identifier"),
                        key_type,
                        true,
                    ),
                    value(
                        "feed_limit".parse().expect("fixed identifier"),
                        SqlValueType::Int64,
                        false,
                    ),
                ])
                .collect(),
            row: [
                value(
                    "feed_end".parse().expect("fixed identifier"),
                    SqlValueType::Int64,
                    true,
                ),
                value(
                    "expired".parse().expect("fixed identifier"),
                    SqlValueType::Boolean,
                    true,
                ),
                value(
                    "change_id".parse().expect("fixed identifier"),
                    key_type,
                    true,
                ),
                value(
                    "change_transaction".parse().expect("fixed identifier"),
                    SqlValueType::Int64,
                    true,
                ),
                value(
                    "present".parse().expect("fixed identifier"),
                    SqlValueType::Boolean,
                    true,
                ),
            ]
            .into_iter()
            .chain(nullable)
            .collect(),
        },
    );
    Some(prepared)
}

pub(super) fn emit(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &wamn_component_policy::PackageManifest,
    name: &wamn_package::local_operation::LocalOperation,
    compiled: &super::operations::CustomDescription<'_>,
) -> Result<(), GenerateError> {
    let operation = compiled.declaration;
    if operation.feed != Some(true) {
        return Ok(());
    }
    let key_type = compiled.feed_key.expect("compiled feed key");
    emit_data(files, &manifest.package, name, compiled, key_type)?;
    emit_component(files, &manifest.package.id, name, compiled, key_type)
}

const HEADER: &str = "// @generated from the authored feed; do not edit.\n";

fn emit_data(
    files: &mut BTreeMap<String, Vec<u8>>,
    package: &wamn_package::manifest::PackageIdentity,
    name: &wamn_package::local_operation::LocalOperation,
    compiled: &super::operations::CustomDescription<'_>,
    key_type: SqlValueType,
) -> Result<(), GenerateError> {
    let operation = compiled.declaration;
    let stem = custom_artifact_stem(name);
    let statement = operation
        .statements
        .values()
        .next()
        .expect("validated statement");
    let key = rust_ident(&operation.lists.as_ref().expect("validated list").key_field[0]);
    let id_type = super::data::cursor_key_type(key_type);
    let permission = wamn_package::manifest::canonical_operation_identity(package, name)?;
    let query = name.as_str();
    let permission_text = permission.as_str();
    let result = compiled.result.as_ref().expect("compiled feed result");
    let row_fields = result
        .fields
        .iter()
        .filter(|field| field.path != "row_version")
        .map(|field| {
            let name = rust_ident(&field.path);
            let ty = projected_rust_type(field.ty, field.nullable);
            quote!(pub #name: #ty,)
        });
    let input_fields = statement.parameters.iter().map(|field| {
        let name = rust_ident(field.name.as_str());
        let ty = super::data::request_type(field.ty);
        let ty = if field.nullable {
            quote!(Option<#ty>)
        } else {
            ty
        };
        quote!(pub #name: #ty,)
    });
    let binds = statement.parameters.iter().map(|field| {
        let path = field.name.as_str();
        let field_name = rust_ident(path);
        let input = quote!(input.#field_name);
        let parsed = if field.nullable {
            super::data::parse(field.ty, &quote!(value), path).map_or_else(
                || quote!(#input),
                |value| quote!(#input.map(|value| #value).transpose()?),
            )
        } else {
            super::data::parse(field.ty, &input, path)
                .map_or_else(|| quote!(#input), |value| quote!(#value?))
        };
        quote!(let #field_name = #parsed;)
    });
    let args = statement
        .parameters
        .iter()
        .map(|field| rust_ident(field.name.as_str()));
    let args = quote!(#(#args,)*);
    let members = result
        .fields
        .iter()
        .filter(|field| field.path != "row_version")
        .map(|field| {
            let name = rust_ident(&field.path);
            if field.nullable {
                quote!(#name: row.#name,)
            } else {
                quote!(#name: row.#name.ok_or_else(Error::internal)?,)
            }
        });
    let input = if statement.parameters.is_empty() {
        quote!(_input)
    } else {
        quote!(input)
    };
    let (row_id, change_id) = if key_type == SqlValueType::Uuid {
        (quote!(row.change_id.clone()), quote!(id.clone()))
    } else {
        (quote!(row.change_id), quote!(id))
    };
    let include = format!("../wamn/{stem}.rs");
    let source = quote! {
        #[allow(unused_imports)]
        use wamn_guest::data_access::{Invalid, feed, scalar};
        use wamn_guest::postgres_statements::Connection;
        use super::error::{Constraints, Error};
        /// The statement accessors of the authored read.
        pub mod sql { include!(#include); }
        const QUERY: &str = #query;
        const PERMISSION: &str = #permission_text;
        #[derive(Debug)]
        pub struct Row { #(#row_fields)* pub row_version: i64 }
        #[derive(Debug)]
        pub struct Input { #(#input_fields)* }
        #[derive(Debug)]
        pub struct FeedInput { pub cursor: Option<String>, pub limit: i64 }
        #[derive(Debug)]
        pub enum Output { Page { rows: Vec<Row>, cursor: String }, Changes(feed::id::Changes<Row, #id_type>) }
        macro_rules! live_row { ($row:expr, $version:expr) => {{
            let row = $row;
            if row.#key.is_none() { None } else { Some(Row { #(#members)* row_version: $version }) }
        }}; }
        /// Run the authored page or its changes in one statement.
        ///
        /// # Errors
        ///
        /// The operation contract refusal.
        pub async fn read(connection: &mut Connection, #input: Input, part: Option<FeedInput>) -> Result<Output, Error> {
            #(#binds)*
            let refusal = |error| Error::from_statement(&error, &Constraints::NONE, PERMISSION);
            if let Some(part) = part {
                let cursor = part.cursor.as_deref().map(|encoded| feed::id::decode::<#id_type>(encoded, QUERY)).transpose().map_err(|Invalid| Error::invalid("cursor"))?;
                let (transaction, after) = cursor.map_or((None, None), |cursor| (Some(cursor.transaction), cursor.after));
                let rows = sql::feed_changes(connection, #args transaction, after, part.limit + 1).await.map_err(refusal)?;
                let mut end = None;
                let mut changed = Vec::with_capacity(rows.len());
                for row in rows {
                    if row.expired == Some(true) { return Err(Error::cursor_expired()); }
                    let version = row.feed_end.ok_or_else(Error::internal)?;
                    end = Some(version);
                    if let (Some(id), Some(transaction)) = (#row_id, row.change_transaction) {
                        let present = row.present == Some(true);
                        let data = live_row!(row, version).filter(|_| present);
                        let change = feed::id::Change { id: #change_id, row_version: version, data };
                        changed.push(feed::id::Changed { transaction, id, change });
                    }
                }
                return feed::id::Changes::new(QUERY, part.cursor.as_deref(), end.ok_or_else(Error::internal)?, changed, part.limit).map(Output::Changes).map_err(|Invalid| Error::internal());
            }
            let rows = sql::feed_page(connection, #args).await.map_err(refusal)?;
            let mut end = None;
            let mut result = Vec::with_capacity(rows.len());
            for row in rows {
                let version = row.feed_cursor.ok_or_else(Error::internal)?;
                end = Some(version);
                if let Some(row) = live_row!(row, version) { result.push(row); }
            }
            let cursor = feed::id::encode::<#id_type>(QUERY, &feed::id::Cursor { transaction: end.ok_or_else(Error::internal)?, after: None }).map_err(|Invalid| Error::internal())?;
            Ok(Output::Page { rows: result, cursor })
        }
    };
    insert_rust(files, &format!("generated/data/{stem}.rs"), HEADER, source)
}

fn emit_component(
    files: &mut BTreeMap<String, Vec<u8>>,
    namespace: &wamn_package::PackageName,
    name: &wamn_package::local_operation::LocalOperation,
    compiled: &super::operations::CustomDescription<'_>,
    key_type: SqlValueType,
) -> Result<(), GenerateError> {
    let operation = compiled.declaration;
    let stem = custom_artifact_stem(name);
    let (group, local) = name.as_str().split_once('.').expect("validated operation");
    let namespace = rust_ident(namespace.as_str());
    let group = rust_ident(group);
    let local_module = rust_ident(local);
    let module = rust_ident(&stem);
    let ty = rust_type_identifier(local);
    let request = rust_ident(&[&ty, "Request"].concat());
    let result = rust_ident(&[&ty, "Result"].concat());
    let error = rust_ident(&[&ty, "Error"].concat());
    let page = rust_ident(&[&ty, "Page"].concat());
    let changes = rust_ident(&[&ty, "Changes"].concat());
    let change = rust_ident(&[&ty, "Change"].concat());
    let row = rust_ident(&[&ty, "Row"].concat());
    let statement = operation
        .statements
        .values()
        .next()
        .expect("validated statement");
    let inputs = statement.parameters.iter().map(|field| {
        let name = rust_ident(field.name.as_str());
        let value = match (field.ty, field.nullable) {
            (SqlValueType::Json, true) => quote!(request.#name.map(|value| value.encoded_json)),
            (SqlValueType::Json, false) => quote!(request.#name.encoded_json),
            _ => quote!(request.#name),
        };
        quote!(#name: #value,)
    });
    let id = if key_type == SqlValueType::Uuid {
        quote!(change.id.0)
    } else {
        quote!(change.id)
    };
    let row = quote!(operation::row!(row, contract::#row));
    let (page_rows, change_rows) = if compiled
        .result
        .as_ref()
        .is_some_and(|result| result.fields.iter().any(|field| !field.values.is_empty()))
    {
        (
            quote!(rows.into_iter().map(|row| Ok(#row)).collect::<Result<_, contract::#error>>()?),
            quote!(changes.rows.into_iter().map(|change| Ok(contract::#change {
                id: #id,
                row_version: change.row_version,
                data: change.data.map(|row| Ok(#row)).transpose()?,
            })).collect::<Result<_, contract::#error>>()?),
        )
    } else {
        (
            quote!(rows.into_iter().map(|row| #row).collect()),
            quote!(changes.rows.into_iter().map(|change| contract::#change {
                id: #id,
                row_version: change.row_version,
                data: change.data.map(|row| #row),
            }).collect()),
        )
    };
    let include = format!("../wit/{stem}_operation.rs");
    let source = quote! {
        use super::data::#module as model;
        use wamn_guest::postgres_statements::Connection;
        use crate::exports::#namespace::#group::#local_module as contract;
        pub(crate) mod operation { use super::contract; include!(#include); }
        pub(crate) async fn handle(connection: &mut Connection, request: contract::#request) -> Result<contract::#result, contract::#error> {
            let input = model::Input { #(#inputs)* };
            let part = request.feed.map(|part| model::FeedInput { cursor: part.cursor, limit: i64::from(part.limit.unwrap_or(100)) });
            let output = model::read(connection, input, part).await.map_err(|error| operation::map_error(error.literal(), |key| error.detail(key)))?;
            Ok(match output {
                model::Output::Page { rows, cursor } => contract::#result::Page(contract::#page { item: #page_rows, next_cursor: None, cursor }),
                model::Output::Changes(changes) => contract::#result::Changes(contract::#changes { rows: #change_rows, next_cursor: changes.next_cursor, complete: changes.complete }),
            })
        }
        operation::export_operation!(crate::Component, contract, crate::wamn::node::types, Connection::new(), handle, operation);
    };
    insert_rust(
        files,
        &format!("generated/component/{stem}.rs"),
        HEADER,
        source,
    )
}
