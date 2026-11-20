//! Generated data functions.
//!
//! Each model with generated operations gets `generated/data/<model>.rs`, with
//! one `pub async fn` for each operation. A function takes the request values
//! the typed export already validated, parses each into its one spelling,
//! runs the generated statement, and answers the contract's row or refusal.
//! The shared data support owns the refusal type. The package gets one
//! `generated/data/mod.rs` that the data crate includes.

use std::collections::BTreeMap;

use proc_macro2::{Ident, TokenStream};
use quote::quote;
use wamn_scalar::SqlValueType;

use super::rust_source::{insert_rust, rust_ident};
use super::{AccessorBind, WamnApi, query_variants, sql};
use crate::key::KeyType;
use wamn_component_policy::PackageManifest;
use wamn_package::manifest::ManifestError as GenerateError;
use wamn_package::manifest::{CrudAction, FilterMatch, OperationDeclaration, rust_type_identifier};

const HEADER: &str = "// @generated from the package manifest and migration IR; do not edit.\n\n";

/// The Rust type a request carries for this column.
pub(super) fn request_type(ty: SqlValueType) -> TokenStream {
    match ty {
        SqlValueType::Boolean => quote!(bool),
        SqlValueType::Int32 => quote!(i32),
        SqlValueType::Int64 => quote!(i64),
        SqlValueType::Float64 => quote!(f64),
        SqlValueType::Bytes => quote!(Vec<u8>),
        _ => quote!(String),
    }
}

/// Parse a request value when the statement uses a different Rust type.
pub(super) fn parse(ty: SqlValueType, value: &TokenStream, path: &str) -> Option<TokenStream> {
    let parser = match ty {
        SqlValueType::Uuid => quote!(uuid),
        SqlValueType::Timestamptz => quote!(timestamptz),
        SqlValueType::Numeric => quote!(numeric),
        SqlValueType::Json => quote!(json),
        _ => return None,
    };
    Some(quote!(scalar::#parser(&#value).map_err(|Invalid| Error::invalid(#path))))
}

/// The Rust type a cursor carries for a sort field.
pub(super) fn cursor_key_type(ty: SqlValueType) -> TokenStream {
    match ty {
        SqlValueType::Float64 | SqlValueType::Bytes | SqlValueType::Json => {
            unreachable!("query validation admits only cursor key types as sort fields")
        }
        _ => super::rust::projected_rust_type(ty, false),
    }
}

fn widen(ty: SqlValueType, value: TokenStream) -> TokenStream {
    if ty == SqlValueType::Int32 {
        quote!(i64::from(#value))
    } else {
        value
    }
}

fn permission_constant(action: CrudAction) -> Ident {
    rust_ident(&[&action.as_str().to_ascii_uppercase(), "_OPERATION"].concat())
}

fn constraints(action: CrudAction) -> TokenStream {
    let action = action.as_str().to_ascii_uppercase();
    let unique = rust_ident(&[&action, "_UNIQUE_CONSTRAINTS"].concat());
    let foreign_key = rust_ident(&[&action, "_FOREIGN_KEY_CONSTRAINTS"].concat());
    let check = rust_ident(&[&action, "_CHECK_CONSTRAINTS"].concat());
    let exclusion = rust_ident(&[&action, "_EXCLUSION_CONSTRAINTS"].concat());
    quote! {
        Constraints {
            unique: sql::#unique, foreign_key: sql::#foreign_key,
            check: sql::#check, exclusion: sql::#exclusion
        }
    }
}

pub(super) fn emit_model_data(
    files: &mut BTreeMap<String, Vec<u8>>,
    compiled: &super::operations::ModelDescription<'_>,
) -> Result<(), GenerateError> {
    let model_name = compiled.name;
    let table = &compiled.shape;
    let api = &compiled.api;
    let model_type = rust_type_identifier(model_name.as_str());
    let row = rust_ident(&[&model_type, "Row"].concat());
    let include = format!("../wamn/{model_name}.rs");
    let connection = api
        .accessors
        .iter()
        .any(|accessor| !accessor.operation.changes_records())
        .then(|| {
            quote!(
                use wamn_guest::postgres_statements::Connection;
            )
        });
    let rows = api.operation_rows.iter().map(|row| rust_ident(&row.name));
    let timestamptz = table
        .columns
        .iter()
        .filter(|column| column.column_type() == SqlValueType::Timestamptz)
        .map(wamn_schema_introspection::ir::Column::name);
    let permissions = compiled.operations.iter().map(|(action, facts)| {
        let name = permission_constant(*action);
        let value = facts.request.permission.as_str();
        quote!(const #name: &str = #value;)
    });
    let handlers = compiled.operations.iter().map(|(action, facts)| {
        let operation = facts.request;
        match action {
            CrudAction::Get => emit_get(model_name, &row, table),
            CrudAction::Query => {
                emit_query(model_name, &model_type, table, operation, compiled.feed)
            }
            CrudAction::Create => emit_create(
                model_name,
                &row,
                table,
                operation,
                api,
                compiled.number.as_ref(),
            ),
            CrudAction::Update => emit_update(model_name, &row, table, operation),
            CrudAction::Delete => emit_delete(model_name, &model_type, table, operation),
        }
    });
    let source = quote! {
        #[allow(unused_imports)]
        use wamn_guest::data_access::{Direction, Invalid, Page, cursor, feed, scalar};
        #connection
        #[allow(unused_imports)]
        use super::error::{Constraints, Error};
        /// The statement accessors of the model.
        pub mod sql { include!(#include); }
        pub use sql::#row;
        #(pub use sql::#rows;)*
        /// The `timestamptz` columns of the model's relation.
        #[allow(dead_code)]
        pub const TIMESTAMPTZ_COLUMNS: &[&str] = &[#(#timestamptz),*];
        #(#permissions)*
        #(#handlers)*
    };
    insert_rust(
        files,
        &format!("generated/data/{model_name}.rs"),
        HEADER,
        source,
    )
}

fn statement_error(action: CrudAction, constraints: &TokenStream) -> TokenStream {
    let permission = permission_constant(action);
    quote!(|error| Error::from_statement(&error, &#constraints, #permission))
}

/// The data layer receives integers already parsed by the operation.
fn key_request(
    table: &super::operations::ModelShape<'_>,
) -> (TokenStream, TokenStream, TokenStream, TokenStream) {
    match table.key {
        KeyType::Uuid => (
            quote!(&str),
            quote!(let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;),
            quote!(id.clone()),
            quote!(Error::not_found(&id)),
        ),
        key => (
            request_type(key.column_type()),
            TokenStream::new(),
            quote!(id),
            quote!(Error::NotFound {
                field: "id".to_owned(),
                id: id.to_string()
            }),
        ),
    }
}

type SourceName = wamn_package::manifest::source_name::SourceName;

fn emit_get(
    model: &SourceName,
    row: &Ident,
    table: &super::operations::ModelShape<'_>,
) -> TokenStream {
    let (id_type, parse_id, id, not_found) = key_request(table);
    let refuse = statement_error(CrudAction::Get, &quote!(Constraints::NONE));
    let doc = format!(" `{model}.get`: load one row by id.");
    quote! {
        #[doc = #doc]
        ///
        /// # Errors
        ///
        /// [`Error`] carrying the literal the operation contract declares.
        pub async fn get(connection: &mut Connection, id: #id_type) -> Result<#row, Error> {
            #parse_id
            sql::get(connection, #id).await.map_err(#refuse)?.ok_or_else(|| #not_found)
        }
    }
}

fn emit_create(
    model: &SourceName,
    row: &Ident,
    table: &super::operations::ModelShape<'_>,
    operation: &OperationDeclaration,
    api: &WamnApi,
    number: Option<&(&str, &SourceName, wamn_number_pattern::Pattern)>,
) -> TokenStream {
    let accessor = api
        .accessors
        .iter()
        .find(|accessor| accessor.operation == CrudAction::Create)
        .expect("a create has its accessor");
    let client_id = !table.generated_id;
    let id_parameter = client_id.then(|| quote!(id: &str,));
    let id_argument = client_id.then(|| quote!(id,));
    let parse_id = client_id
        .then(|| quote!(let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;));
    let parameters = operation.writable_fields.iter().map(|field| {
        let column = table.column(field).expect("validation resolved the column");
        let name = rust_ident(field);
        let ty = request_type(column.column_type());
        quote!(#name: Option<#ty>)
    });
    let values = operation
        .writable_fields
        .iter()
        .zip(accessor.binds.iter().skip(usize::from(client_id)))
        .map(|(field, bind)| emit_value(&rust_ident(field), field, bind));
    let mut arguments = operation
        .writable_fields
        .iter()
        .map(|field| rust_ident(field))
        .collect::<Vec<_>>();
    let refuse = statement_error(CrudAction::Create, &constraints(CrudAction::Create));
    let allocate_number = number.map(|(field, sequence, pattern)| {
        let keys = pattern.fields().into_iter().map(|name| {
            let column = table.column(name).expect("validation resolved the pattern field");
            let value = key_text(column.column_type(), &rust_ident(name), column.nullable());
            quote!((#name, #value))
        });
        let sequence = sequence.as_str();
        let target = rust_ident(field);
        let call = quote! {
            let #target = wamn_guest::postgres_statements::next_number(transaction, #sequence, &[#(#keys),*])
                .await.map_err(#refuse)?;
        };
        arguments.push(target);
        call
    });
    let doc = format!(" `{model}.create`: create one row in the transaction the host began");
    quote! {
        #[doc = #doc]
        /// for the operation.
        ///
        /// # Errors
        ///
        /// [`Error`] carrying the literal the operation contract declares.
        #[allow(clippy::too_many_arguments)]
        pub async fn create(transaction: &mut wamn_guest::postgres_statements::Transaction, #id_parameter #(#parameters),*) -> Result<#row, Error> {
            #parse_id
            #(#values)*
            #allocate_number
            sql::create(transaction, #id_argument #(#arguments),*).await.map_err(#refuse)
        }
    }
}

fn key_text(ty: SqlValueType, value: &Ident, nullable: bool) -> TokenStream {
    let text = |value: TokenStream| match ty {
        SqlValueType::Text => quote!(#value.clone()),
        SqlValueType::Uuid
        | SqlValueType::Numeric
        | SqlValueType::Timestamptz
        | SqlValueType::Json => {
            quote!(#value.0.clone())
        }
        SqlValueType::Boolean
        | SqlValueType::Int32
        | SqlValueType::Int64
        | SqlValueType::Float64 => {
            quote!(#value.to_string())
        }
        SqlValueType::Bytes => unreachable!("validation refuses a bytes field in a pattern"),
    };
    if nullable {
        let text = text(quote!(value));
        quote!(#value.as_ref().map(|value| #text))
    } else {
        let text = text(quote!(#value));
        quote!(Some(#text))
    }
}

fn emit_value(name: &Ident, path: &str, bind: &AccessorBind) -> TokenStream {
    let parsed = parse(bind.statement_type, &quote!(value), path);
    if bind.nullable {
        parsed.map_or_default(|parsed| {
            quote! {
                let #name = match #name { Some(value) => Some(#parsed?), None => None };
            }
        })
    } else {
        let parsed = parsed.map_or_else(|| quote!(value), |parsed| quote!(#parsed?));
        quote! {
            let Some(value) = #name else { return Err(Error::invalid(#path)); };
            let #name = #parsed;
        }
    }
}

fn emit_update(
    model: &SourceName,
    row: &Ident,
    table: &super::operations::ModelShape<'_>,
    operation: &OperationDeclaration,
) -> TokenStream {
    let revision = operation
        .revision_field
        .as_deref()
        .expect("update validation requires a revision field");
    let revision_type = table
        .column(revision)
        .expect("the revision field is a column")
        .column_type();
    let revision_rust = request_type(revision_type);
    let expected = rust_ident(&["expected_", revision].concat());
    let expected_value = widen(revision_type, quote!(#expected));
    let observed = rust_ident(&["observed_", revision].concat());
    let observed_value = widen(
        revision_type,
        quote!(row.#observed.ok_or_else(Error::internal)?),
    );
    let (id_type, parse_id, id, not_found) = key_request(table);
    let mut parameters = TokenStream::new();
    let mut body = TokenStream::new();
    let mut arguments = TokenStream::new();
    for field in &operation.writable_fields {
        let column = table.column(field).expect("validation resolved the column");
        let name = rust_ident(field);
        let path = ["change.", field].concat();
        let present = rust_ident(&[field, "_present"].concat());
        let ty = request_type(column.column_type());
        parameters.extend(quote!(, #name: Option<Option<#ty>>));
        if !column.nullable() {
            body.extend(
                quote!(if matches!(#name, Some(None)) { return Err(Error::invalid(#path)); }),
            );
        }
        let parsed = parse(column.column_type(), &quote!(value), &path);
        let value = parsed.map_or_else(
            || quote!(#name.flatten()),
            |parsed| quote!(match #name.flatten() { Some(value) => Some(#parsed?), None => None }),
        );
        body.extend(quote! {
            let #present = #name.is_some();
            let #name = #value;
        });
        arguments.extend(quote!(, #present, #name));
    }
    let members = table.columns.iter().map(|column| {
        let name = rust_ident(column.name());
        if column.nullable() {
            quote!(#name: row.#name)
        } else {
            quote!(#name: row.#name.ok_or_else(Error::internal)?)
        }
    });
    let refuse = statement_error(CrudAction::Update, &constraints(CrudAction::Update));
    let doc = format!(" `{model}.update`: change one row at the revision the caller last read.");
    let updated = sql::OUTCOME_UPDATED;
    let not_found_outcome = sql::OUTCOME_NOT_FOUND;
    let conflict = sql::OUTCOME_CONCURRENCY_CONFLICT;
    quote! {
        #[doc = #doc]
        ///
        /// # Errors
        ///
        /// [`Error`] carrying the literal the operation contract declares.
        #[allow(clippy::too_many_arguments)]
        pub async fn update(transaction: &mut wamn_guest::postgres_statements::Transaction, id: #id_type, #expected: #revision_rust #parameters) -> Result<#row, Error> {
            #parse_id
            #body
            let row = sql::update(transaction, #id, #expected #arguments).await.map_err(#refuse)?;
            match row.outcome.as_deref() {
                Some(#updated) => Ok(#row { #(#members),* }),
                Some(#not_found_outcome) => Err(#not_found),
                Some(#conflict) => Err(Error::conflict(#expected_value, #observed_value)),
                _ => Err(Error::internal()),
            }
        }
    }
}

fn emit_delete(
    model: &SourceName,
    model_type: &str,
    table: &super::operations::ModelShape<'_>,
    operation: &OperationDeclaration,
) -> TokenStream {
    let revision = operation
        .revision_field
        .as_deref()
        .expect("delete validation requires a revision field");
    let revision_type = table
        .column(revision)
        .expect("the revision field is a column")
        .column_type();
    let revision_rust = request_type(revision_type);
    let expected = rust_ident(&["expected_", revision].concat());
    let expected_value = widen(revision_type, quote!(#expected));
    let observed = rust_ident(&["observed_", revision].concat());
    let observed_value = widen(
        revision_type,
        quote!(row.#observed.ok_or_else(Error::internal)?),
    );
    let (id_type, parse_id, id, not_found) = key_request(table);
    let refuse = statement_error(CrudAction::Delete, &constraints(CrudAction::Delete));
    let doc = format!(" `{model}.delete`: delete one row at the revision the caller last read.");
    let row = rust_ident(&[model_type, "DeleteRow"].concat());
    let deleted = sql::OUTCOME_DELETED;
    let not_found_outcome = sql::OUTCOME_NOT_FOUND;
    let conflict = sql::OUTCOME_CONCURRENCY_CONFLICT;
    quote! {
        #[doc = #doc]
        ///
        /// # Errors
        ///
        /// [`Error`] carrying the literal the operation contract declares.
        pub async fn delete(transaction: &mut wamn_guest::postgres_statements::Transaction, id: #id_type, #expected: #revision_rust) -> Result<#row, Error> {
            #parse_id
            let row = sql::delete(transaction, #id, #expected).await.map_err(#refuse)?;
            match row.outcome.as_deref() {
                Some(#deleted) => Ok(row),
                Some(#not_found_outcome) => Err(#not_found),
                Some(#conflict) => Err(Error::conflict(#expected_value, #observed_value)),
                _ => Err(Error::internal()),
            }
        }
    }
}

fn direction_tokens(direction: wamn_package::manifest::CursorDirection) -> TokenStream {
    match direction {
        wamn_package::manifest::CursorDirection::Ascending => quote!(Direction::Ascending),
        wamn_package::manifest::CursorDirection::Descending => quote!(Direction::Descending),
    }
}

fn emit_query(
    model: &SourceName,
    model_type: &str,
    table: &super::operations::ModelShape<'_>,
    operation: &OperationDeclaration,
    feed: bool,
) -> TokenStream {
    let row = rust_ident(&[model_type, if feed { "FeedRow" } else { "Row" }].concat());
    let pagination = operation
        .pagination
        .as_ref()
        .expect("query validation requires pagination");
    let tie_breaker = rust_ident(&pagination.tie_breaker.field);
    let id_type = cursor_key_type(table.key.column_type());

    let mut members = TokenStream::new();
    let mut binds = TokenStream::new();
    let mut arguments = TokenStream::new();
    for filter in &operation.filters {
        let name = rust_ident(&filter.field);
        let parameter = rust_ident(&[&filter.field, "_filter"].concat());
        let (member, bind) = match filter.match_mode {
            FilterMatch::Exact | FilterMatch::Contains | FilterMatch::Prefix => (
                quote!(Option<Vec<String>>),
                quote!(input.#name.as_deref().map(scalar::json_list)),
            ),
            FilterMatch::IsNull => (
                quote!(Option<bool>),
                quote!(input.#name.map(scalar::json_boolean)),
            ),
            FilterMatch::Range => (
                quote!(Option<wamn_guest::data_access::Range>),
                quote!(input.#name.as_ref().map(|range| scalar::json_range(range.min.as_deref(), range.max.as_deref()))),
            ),
        };
        members.extend(quote!(pub #name: #member,));
        binds.extend(quote!(let #parameter = #bind;));
        arguments.extend(quote!(#parameter,));
    }
    if operation.search.is_some() {
        members.extend(quote!(pub search: Option<String>,));
        binds.extend(quote!(let search = input.search;));
        arguments.extend(quote!(search,));
    }
    let feed_split = feed.then(|| quote!(let (feed_cursor, rows) = feed_page(rows)?;));
    let feed_wrap = feed.then(|| quote!(.map(|page| page.with_feed_cursor(feed_cursor))));
    let arms = query_variants(operation).into_iter().map(|(field, direction)| {
        let direction_name = sql::direction_name(direction);
        let direction = direction_tokens(direction);
        let function = rust_ident(&["query_", field, "_", direction_name].concat());
        let key_column = table.column(field).expect("validation resolved the sort field");
        let key_type = cursor_key_type(key_column.column_type());
        let key_member = rust_ident(if feed && field == "row_version" { "model_row_version" } else { field });
        let encode = if key_column.nullable() {
            quote!(row.#key_member.as_ref().ok_or(Invalid).and_then(|key| cursor::id::encode_with_id(#field, #direction, key, &row.#tie_breaker)))
        } else {
            quote!(cursor::id::encode_with_id(#field, #direction, &row.#key_member, &row.#tie_breaker))
        };
        quote! {
            (#field, #direction) => {
                let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                    Some(encoded) => {
                        let (key, id) = cursor::id::decode_with_id::<#key_type, #id_type>(encoded, #field, #direction)
                            .map_err(|Invalid| Error::invalid("cursor"))?;
                        (Some(key), Some(id))
                    }
                    None => (None, None),
                };
                let rows = sql::#function(connection, #arguments cursor_key, cursor_id, input.limit + 1)
                    .await.map_err(|error| query_statement(&error))?;
                #feed_split
                Page::new(rows, input.limit, |row| { #encode.map_err(|Invalid| Error::internal()) }) #feed_wrap
            }
        }
    });
    let default_field = &pagination.default_sort.field;
    let default_direction = direction_tokens(pagination.default_sort.direction);
    let range = operation
        .filters
        .iter()
        .any(|filter| matches!(filter.match_mode, FilterMatch::Range))
        .then(|| {
            quote!(
                pub use wamn_guest::data_access::Range;
            )
        });
    let input_doc = format!(" The `{model}.query` request, after its operation filled the limit.");
    let query_doc = format!(" `{model}.query`: one keyset page in the requested sort.");
    let permission = permission_constant(CrudAction::Query);
    let feed = feed.then(|| emit_feed(model, model_type, table, &binds, &arguments));
    quote! {
        #range
        #[doc = #input_doc]
        #[derive(Clone, Debug, Default, Eq, PartialEq)]
        pub struct QueryInput {
            #members
            pub sort_field: Option<String>,
            pub sort_direction: Option<String>,
            pub cursor: Option<String>,
            pub limit: i64,
        }
        fn query_statement(error: &wamn_guest::postgres_statements::StatementError) -> Error {
            Error::from_statement(error, &Constraints::NONE, #permission)
        }
        #[doc = #query_doc]
        ///
        /// The contract's order holds: the sort and the cursor first, then the
        /// statement. The operation validated the limit.
        ///
        /// # Errors
        ///
        /// [`Error`] carrying the literal the operation contract declares.
        pub async fn query(connection: &mut Connection, input: QueryInput) -> Result<Page<#row>, Error> {
            let sort = match (input.sort_field.as_deref(), input.sort_direction.as_deref()) {
                (None, None) => (#default_field, #default_direction),
                (Some(field), Some("ascending")) => (field, Direction::Ascending),
                (Some(field), Some("descending")) => (field, Direction::Descending),
                _ => return Err(Error::invalid("sort")),
            };
            #binds
            match sort {
                #(#arms)*
                _ => Err(Error::invalid("sort")),
            }
        }
        #feed
    }
}

/// Emit the page helper and the change feed read.
fn emit_feed(
    model: &SourceName,
    model_type: &str,
    table: &super::operations::ModelShape<'_>,
    binds: &TokenStream,
    arguments: &TokenStream,
) -> TokenStream {
    let key = table.key;
    let id_type = cursor_key_type(key.column_type());
    let id = if key == KeyType::Uuid {
        quote!(id: id.clone())
    } else {
        quote!(id)
    };
    let input = if binds.is_empty() {
        quote!(_input)
    } else {
        quote!(input)
    };
    let members = table.columns.iter().map(|column| {
        let name = rust_ident(column.name());
        let member = if column.name() == "row_version" {
            rust_ident("model_row_version")
        } else {
            rust_ident(column.name())
        };
        if column.name() == "id" {
            quote!(id,)
        } else if column.nullable() {
            quote!(#member: $row.#name,)
        } else {
            quote!(#member: $row.#name.ok_or_else(Error::internal)?,)
        }
    });
    let model_name = model.as_str();
    let row = rust_ident(&[model_type, "FeedRow"].concat());
    let query_row = rust_ident(&[model_type, "QueryFeedRow"].concat());
    let input_doc = format!(" The feed part of a `{model}.query` request, after its operation");
    let changes_doc = format!(" `{model}.query` with a feed part: the rows that changed in the");
    quote! {
        /// The model name a feed cursor carries.
        const MODEL: &str = #model_name;
        /// The model row of one row of a feed statement, or `None` for the row
        /// that only carries the snapshot.
        macro_rules! live_row {
            ($row:expr, $version:expr) => {
                match $row.id {
                    None => None,
                    Some(id) => Some(#row {
                        row_version: $version,
                        #(#members)*
                    }),
                }
            };
        }
        /// The rows of one list page, and the cursor its change feed starts from.
        fn feed_page(rows: Vec<sql::#query_row>) -> Result<(String, Vec<#row>), Error> {
            let mut end = None;
            let mut page = Vec::with_capacity(rows.len());
            for row in rows {
                let row_version = row.feed_cursor.ok_or_else(Error::internal)?;
                end = Some(row_version);
                if let Some(data) = live_row!(row, row_version) {
                    page.push(data);
                }
            }
            let transaction = end.ok_or_else(Error::internal)?;
            let cursor = feed::id::encode::<#id_type>(MODEL, &feed::id::Cursor { transaction, after: None })
                .map_err(|Invalid| Error::internal())?;
            Ok((cursor, page))
        }
        #[doc = #input_doc]
        /// filled the limit.
        #[derive(Clone, Debug, Default, Eq, PartialEq)]
        pub struct FeedInput {
            pub cursor: Option<String>,
            pub limit: i64,
        }
        #[doc = #changes_doc]
        /// target of the list's filters since the cursor, one entry per row.
        ///
        /// A row that left the target, or was deleted, is a change with no data.
        ///
        /// # Errors
        ///
        /// [`Error`] carrying the literal the operation contract declares.
        pub async fn changes(connection: &mut Connection, #input: QueryInput, part: FeedInput) -> Result<feed::id::Changes<#row, #id_type>, Error> {
            let cursor = part.cursor.as_deref().map(|encoded| feed::id::decode::<#id_type>(encoded, MODEL))
                .transpose().map_err(|Invalid| Error::invalid("cursor"))?;
            #binds
            let (cursor_transaction, cursor_id) = match cursor {
                Some(cursor) => (Some(cursor.transaction), cursor.after),
                None => (None, None),
            };
            let rows = sql::changes(connection, #arguments cursor_transaction, cursor_id, part.limit + 1)
                .await.map_err(|error| query_statement(&error))?;
            let mut end = None;
            let mut changed = Vec::with_capacity(rows.len());
            for row in rows {
                if row.expired == Some(true) {
                    return Err(Error::cursor_expired());
                }
                let row_version = row.feed_end.ok_or_else(Error::internal)?;
                end = Some(row_version);
                let (Some(id), Some(transaction)) = (row.change_id, row.change_transaction) else {
                    continue;
                };
                let present = row.present == Some(true);
                let data = live_row!(row, row_version).filter(|_| present);
                let change = feed::id::Change { #id, row_version, data };
                changed.push(feed::id::Changed { transaction, id, change });
            }
            let end = end.ok_or_else(Error::internal)?;
            feed::id::Changes::new(MODEL, part.cursor.as_deref(), end, changed, part.limit)
                .map_err(|Invalid| Error::internal())
        }
    }
}

/// Emit the data index and the package's sequences.
pub(super) fn emit_package_data(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
) -> Result<(), GenerateError> {
    let mut models = manifest
        .models
        .iter()
        .filter(|(_, model)| !model.operations.is_empty())
        .peekable();
    let mut feeds = manifest
        .custom_operations
        .iter()
        .filter(|(_, operation)| operation.feed == Some(true))
        .peekable();
    if models.peek().is_none() && feeds.peek().is_none() && manifest.sequences.is_empty() {
        return Ok(());
    }
    let models = models.map(|(name, _)| {
        let doc = format!(" The generated `{name}` operations.");
        let path = format!("{name}.rs");
        let name = rust_ident(name.as_str());
        quote!(#[doc = #doc] pub mod #name { include!(#path); })
    });
    let feeds = feeds.map(|(name, _)| {
        let stem = wamn_package::manifest::custom_artifact_stem(name);
        let doc = format!(" The authored `{name}` feed.");
        let path = format!("{stem}.rs");
        let module = rust_ident(&stem);
        quote!(#[doc = #doc] pub mod #module { include!(#path); })
    });
    let sequences = if manifest.sequences.is_empty() {
        None
    } else {
        insert_rust(
            files,
            "generated/data/sequences.rs",
            HEADER,
            emit_sequences(manifest),
        )?;
        Some(quote! {
            /// The package's sequences.
            pub mod sequences { include!("sequences.rs"); }
        })
    };
    insert_rust(
        files,
        "generated/data/mod.rs",
        HEADER,
        quote! {
            pub use wamn_guest::data_access::error;
            #(#models)*
            #(#feeds)*
            #sequences
        },
    )
}

fn emit_sequences(manifest: &PackageManifest) -> TokenStream {
    let modules = manifest.sequences.iter().map(|(name, sequence)| {
        let name_text = name.as_str();
        let module = rust_ident(name_text);
        let pattern = wamn_number_pattern::parse(&sequence.pattern)
            .expect("validation parsed every sequence pattern");
        let field_names = pattern.fields();
        let fields = field_names.iter().map(|name| rust_ident(name)).collect::<Vec<_>>();
        let keys = field_names.iter().zip(&fields)
            .map(|(name, field)| quote!((#name, Some(#field))));
        let (key_type, parameter, bind) = if fields.is_empty() {
            (None, None, None)
        } else {
            let key = rust_ident(&[&rust_type_identifier(name_text), "Key"].concat());
            (
                Some(quote! {
                    /// The values the pattern names, as text.
                    #[derive(Clone, Debug, Default, Eq, PartialEq)]
                    pub struct #key { #(pub #fields: String,)* }
                }),
                Some(quote!(key: #key)),
                Some(quote!(let #key { #(#fields),* } = key;)),
            )
        };
        let keys = quote!(#(#keys),*);
        let doc = format!(" The `{name}` sequence, pattern `{}` by default.", sequence.pattern.escape_debug());
        quote! {
            #[doc = #doc]
            pub mod #module {
                #key_type
                /// The next number, which advances the counter in the operation
                /// transaction: consumed on commit, never on rollback.
                ///
                /// # Errors
                ///
                /// The host's refusal.
                pub async fn next(#parameter) -> Result<String, wamn_guest::postgres_statements::StatementError> {
                    #bind
                    let mut transaction = wamn_guest::postgres_statements::operation_transaction().await?;
                    wamn_guest::postgres_statements::next_number(&mut transaction, #name_text, &[#keys]).await
                }
                /// The current number, the last one issued for the key, with no
                /// lock and no advance. Never for assignment.
                ///
                /// # Errors
                ///
                /// The host's refusal, and SQLSTATE `P0002` when no number was
                /// issued yet.
                pub async fn peek(#parameter) -> Result<String, wamn_guest::postgres_statements::StatementError> {
                    #bind
                    let mut transaction = wamn_guest::postgres_statements::operation_transaction().await?;
                    wamn_guest::postgres_statements::peek_number(&mut transaction, #name_text, &[#keys]).await
                }
            }
        }
    });
    quote!(#(#modules)*)
}
