//! Authored feeding reads use the generated snapshot and cursor contract.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::rust::projected_rust_type;
use super::{
    CatalogIr, ColumnType, CustomOperationDeclaration, GenerateError, Projection, insert_bytes,
    rust_identifier, rust_type_identifier, sql,
};
use crate::manifest::{
    StaticSqlFetch, StaticSqlStatementDeclaration, StaticSqlValueDeclaration, custom_artifact_stem,
};

fn value(name: &str, ty: ColumnType, nullable: bool) -> StaticSqlValueDeclaration {
    StaticSqlValueDeclaration {
        name: name.to_owned(),
        ty,
        nullable,
    }
}

/// Add the snapshot statements and the host data function for one authored read.
pub(super) fn prepare(
    files: &mut BTreeMap<String, Vec<u8>>,
    corpus: &mut BTreeMap<String, Vec<u8>>,
    catalog: &CatalogIr,
    manifest: &super::PackageManifest,
    name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<Option<CustomOperationDeclaration>, GenerateError> {
    if operation.feed != Some(true) {
        return Ok(None);
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
    let text = std::str::from_utf8(&corpus[&statement.path])
        .expect("validated authored SQL")
        .to_owned();
    let fields = statement
        .row
        .iter()
        .map(|field| field.name.clone())
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
        (&page_path, sql::authored_page(&text, &fields)),
        (
            &changes_path,
            sql::authored_changes(
                catalog,
                source,
                &text,
                key,
                &fields,
                statement.parameters.len() + 1,
            ),
        ),
    ] {
        insert_bytes(files, path, text.as_bytes().to_vec())?;
        corpus.insert(path.clone(), text.into_bytes());
    }
    let mut prepared = contract(operation);
    prepared.statements.clear();
    let nullable = statement
        .row
        .iter()
        .map(|field| value(&field.name, field.ty, true))
        .collect::<Vec<_>>();
    prepared.statements.insert(
        "feed_page".to_owned(),
        StaticSqlStatementDeclaration {
            path: page_path,
            fetch: StaticSqlFetch::BoundedList,
            parameters: statement.parameters.clone(),
            row: std::iter::once(value("feed_cursor", ColumnType::Int64, true))
                .chain(nullable.clone())
                .collect(),
        },
    );
    prepared.statements.insert(
        "feed_changes".to_owned(),
        StaticSqlStatementDeclaration {
            path: changes_path,
            fetch: StaticSqlFetch::BoundedList,
            parameters: statement
                .parameters
                .iter()
                .cloned()
                .chain([
                    value("cursor_transaction", ColumnType::Int64, true),
                    value("cursor_id", key_type, true),
                    value("feed_limit", ColumnType::Int64, false),
                ])
                .collect(),
            row: [
                value("feed_end", ColumnType::Int64, true),
                value("expired", ColumnType::Boolean, true),
                value("change_id", key_type, true),
                value("change_transaction", ColumnType::Int64, true),
                value("present", ColumnType::Boolean, true),
            ]
            .into_iter()
            .chain(nullable)
            .collect(),
        },
    );
    emit_data(files, &manifest.package, name, operation, key_type)?;
    emit_component(files, &manifest.package.id, name, operation, key_type)?;
    Ok(Some(prepared))
}

fn member(name: &str) -> String {
    rust_identifier(name).expect("validated field")
}

fn emit_data(
    files: &mut BTreeMap<String, Vec<u8>>,
    package: &crate::manifest::PackageIdentity,
    name: &str,
    operation: &CustomOperationDeclaration,
    key_type: ColumnType,
) -> Result<(), GenerateError> {
    let stem = custom_artifact_stem(name);
    let statement = operation
        .statements
        .values()
        .next()
        .expect("validated statement");
    let key = member(&operation.lists.as_ref().expect("validated list").key_field[0]);
    let id_type = super::data::cursor_key_type(key_type);
    let permission = crate::manifest::canonical_operation_identity(package, name)?;
    let result = operation.result.as_ref().expect("validated feed result");
    let row_fields = result
        .fields
        .iter()
        .fold(String::new(), |mut fields, field| {
            write!(
                fields,
                "pub {}: {},",
                member(&field.path),
                projected_rust_type(field.ty, Projection::Wamn, field.nullable)
            )
            .expect("writing to a String cannot fail");
            fields
        });
    let input_fields = statement
        .parameters
        .iter()
        .fold(String::new(), |mut fields, field| {
            let ty = super::data::request_type(field.ty);
            write!(
                fields,
                "pub {}: {},",
                member(&field.name),
                if field.nullable {
                    format!("Option<{ty}>")
                } else {
                    ty.to_owned()
                }
            )
            .expect("writing to a String cannot fail");
            fields
        });
    let binds = statement
        .parameters
        .iter()
        .fold(String::new(), |mut binds, field| {
            let field_name = member(&field.name);
            let parsed = if field.nullable {
                let parsed = super::data::parse(field.ty, "value", &field.name);
                parsed.strip_suffix('?').map_or_else(
                    || format!("input.{field_name}"),
                    |parsed| format!("input.{field_name}.map(|value| {parsed}).transpose()?"),
                )
            } else {
                super::data::parse(field.ty, &format!("input.{field_name}"), &field.name)
            };
            writeln!(binds, "let {field_name} = {parsed};")
                .expect("writing to a String cannot fail");
            binds
        });
    let args = statement
        .parameters
        .iter()
        .map(|field| member(&field.name))
        .collect::<Vec<_>>()
        .join(", ");
    let args = if args.is_empty() {
        args
    } else {
        format!("{args}, ")
    };
    let members = result
        .fields
        .iter()
        .map(|field| {
            let name = member(&field.path);
            if field.nullable {
                format!("{name}: row.{name},")
            } else {
                format!("{name}: row.{name}.ok_or_else(Error::internal)?,")
            }
        })
        .collect::<String>();
    let input = if statement.parameters.is_empty() {
        "_input"
    } else {
        "input"
    };
    let row_id = if key_type == ColumnType::Uuid {
        "row.change_id.clone()"
    } else {
        "row.change_id"
    };
    let change_id = if key_type == ColumnType::Uuid {
        "id.clone()"
    } else {
        "id"
    };
    let text = format!(
        r#"// @generated from the authored feed; do not edit.
#[allow(unused_imports)]
use wamn_data_access::{{Invalid, feed, scalar}};
use wamn_postgres_statements::Connection;
use super::error::{{Constraints, Error}};
/// The statement accessors of the authored read.
pub mod sql {{ include!("../wamn/{stem}.rs"); }}
const QUERY: &str = {name:?};
const PERMISSION: &str = {permission:?};
#[derive(Debug)]
pub struct Row {{ {row_fields} pub row_version: i64 }}
#[derive(Debug)]
pub struct Input {{ {input_fields} }}
#[derive(Debug)]
pub struct FeedInput {{ pub cursor: Option<String>, pub limit: i64 }}
#[derive(Debug)]
pub enum Output {{ Page {{ rows: Vec<Row>, cursor: String }}, Changes(feed::id::Changes<Row, {id_type}>) }}
macro_rules! live_row {{ ($row:expr, $version:expr) => {{{{
    let row = $row;
    if row.{key}.is_none() {{ None }} else {{ Some(Row {{ {members} row_version: $version }}) }}
}}}}; }}
/// Run the authored page or its changes in one statement.
///
/// # Errors
///
/// The operation contract refusal.
pub async fn read(connection: &mut Connection, {input}: Input, part: Option<FeedInput>) -> Result<Output, Error> {{
    {binds}
    let refusal = |error| Error::from_statement(&error, &Constraints::NONE, PERMISSION);
    if let Some(part) = part {{
        let cursor = part.cursor.as_deref().map(|encoded| feed::id::decode::<{id_type}>(encoded, QUERY)).transpose().map_err(|Invalid| Error::invalid("cursor"))?;
        let (transaction, after) = cursor.map_or((None, None), |cursor| (Some(cursor.transaction), cursor.after));
        let rows = sql::feed_changes(connection, {args}transaction, after, part.limit + 1).await.map_err(refusal)?;
        let mut end = None;
        let mut changed = Vec::with_capacity(rows.len());
        for row in rows {{
            if row.expired == Some(true) {{ return Err(Error::cursor_expired()); }}
            let version = row.feed_end.ok_or_else(Error::internal)?;
            end = Some(version);
            if let (Some(id), Some(transaction)) = ({row_id}, row.change_transaction) {{
                let present = row.present == Some(true);
                let data = live_row!(row, version).filter(|_| present);
                let change = feed::id::Change {{ id: {change_id}, row_version: version, data }};
                changed.push(feed::id::Changed {{ transaction, id, change }});
            }}
        }}
        return feed::id::Changes::new(QUERY, part.cursor.as_deref(), end.ok_or_else(Error::internal)?, changed, part.limit).map(Output::Changes).map_err(|Invalid| Error::internal());
    }}
    let rows = sql::feed_page(connection, {args}).await.map_err(refusal)?;
    let mut end = None;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {{
        let version = row.feed_cursor.ok_or_else(Error::internal)?;
        end = Some(version);
        if let Some(row) = live_row!(row, version) {{ result.push(row); }}
    }}
    let cursor = feed::id::encode::<{id_type}>(QUERY, &feed::id::Cursor {{ transaction: end.ok_or_else(Error::internal)?, after: None }}).map_err(|Invalid| Error::internal())?;
    Ok(Output::Page {{ rows: result, cursor }})
}}
"#
    );
    insert_bytes(
        files,
        &format!("generated/data/{stem}.rs"),
        text.into_bytes(),
    )
}

fn emit_component(
    files: &mut BTreeMap<String, Vec<u8>>,
    namespace: &str,
    name: &str,
    operation: &CustomOperationDeclaration,
    key_type: ColumnType,
) -> Result<(), GenerateError> {
    let stem = custom_artifact_stem(name);
    let (group, local) = name.split_once('.').expect("validated operation");
    let namespace = namespace.replace('-', "_");
    let group = group.replace('-', "_");
    let local_module = local.replace('-', "_");
    let ty = rust_type_identifier(local);
    let statement = operation
        .statements
        .values()
        .next()
        .expect("validated statement");
    let inputs = statement
        .parameters
        .iter()
        .fold(String::new(), |mut inputs, field| {
            write!(
                inputs,
                "{}: request.{},",
                member(&field.name),
                crate::manifest::binding_identifier(&field.name).expect("validated input")
            )
            .expect("writing to a String cannot fail");
            inputs
        });
    let id = if key_type == ColumnType::Uuid {
        "change.id.0"
    } else {
        "change.id"
    };
    let text = format!(
        r#"// @generated from the authored feed; do not edit.
use super::data::{stem} as model;
use wamn_postgres_statements::Connection;
use crate::exports::{namespace}::{group}::{local_module} as contract;
pub(crate) mod codec {{ use super::contract; include!("../wit/{stem}_codec.rs"); }}
pub(crate) async fn handle(connection: &mut Connection, request: contract::{ty}Request) -> Result<contract::{ty}Result, contract::{ty}Error> {{
    let input = model::Input {{ {inputs} }};
    let part = request.feed.map(|part| model::FeedInput {{ cursor: part.cursor, limit: part.limit.unwrap_or(100) }});
    let output = model::read(connection, input, part).await.map_err(|error| codec::map_error(error.literal(), |key| error.detail(key)))?;
    Ok(match output {{
        model::Output::Page {{ rows, cursor }} => contract::{ty}Result::Page(contract::{ty}Page {{ item: rows.into_iter().map(|row| codec::row!(row, contract::{ty}Row)).collect(), next_cursor: None, cursor }}),
        model::Output::Changes(changes) => contract::{ty}Result::Changes(contract::{ty}Changes {{ rows: changes.rows.into_iter().map(|change| contract::{ty}Change {{ id: {id}, row_version: change.row_version, data: change.data.map(|row| codec::row!(row, contract::{ty}Row)) }}).collect(), next_cursor: changes.next_cursor, complete: changes.complete }}),
    }})
}}
codec::export_operation!(crate::Component, contract, crate::wamn::node::types, Connection::new(), handle, codec);
"#
    );
    insert_bytes(
        files,
        &format!("generated/component/{stem}.rs"),
        text.into_bytes(),
    )
}

/// WIT and codecs see the same derived snapshot field and expiry refusal.
pub(super) fn contract(operation: &CustomOperationDeclaration) -> CustomOperationDeclaration {
    let mut result = operation.clone();
    if operation.feed == Some(true) {
        if !result.errors.iter().any(|error| error == "cursor_expired") {
            result.errors.push("cursor_expired".to_owned());
        }
        let fields = &mut result
            .result
            .as_mut()
            .expect("validated feed result")
            .fields;
        if !fields.iter().any(|field| field.path == "row_version") {
            fields.push(
                serde_json::from_value(
                    serde_json::json!({"path": "row_version", "type": "int64", "nullable": false}),
                )
                .expect("snapshot field"),
            );
        }
    }
    result
}
