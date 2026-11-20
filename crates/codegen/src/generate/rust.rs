//! Rust projections and accessors.

use proc_macro2::TokenStream;
use quote::quote;
use wamn_scalar::SqlValueType;

use wamn_package::OperationType;

use super::rust_source::{insert_rust, rust_ident};

use super::{
    AccessorBind, AccessorFetch, BTreeMap, CHANGES_STATEMENT, CREATE_STATEMENT, CatalogIr,
    ConstraintNameSlice, ConstraintType, MutationConstraintNames, RustMember, RustRow,
    RustVisibility, StaticSqlAccessor, Table, WamnAccessor, WamnApi, column, operation_constraints,
    operation_exclusions, query_variants, sql,
};
use wamn_package::manifest::ManifestError as GenerateError;
use wamn_package::manifest::{
    CrudAction, CustomOperationDeclaration, ModelDeclaration, OperationDeclaration, StaticSqlFetch,
    rust_type_identifier,
};

/// Clippy's default `too-many-arguments-threshold`. The repository declares no
/// `clippy.toml`, so this default is what the lint uses. Anyone who adds one
/// must make this constant agree with it.
const CLIPPY_ARGUMENT_THRESHOLD: usize = 7;

fn sql_include_path(paths: super::OutputPaths<'_>, module: &str, sql: &str) -> String {
    let module = paths.generated.join("wamn").join(format!("{module}.rs"));
    let sql = match sql.strip_prefix("generated/") {
        Some(sql) => paths.generated.join(sql),
        None => paths.authored.join(sql),
    };
    let mut from = module
        .parent()
        .expect("module directory")
        .components()
        .peekable();
    let mut to = sql.components().peekable();
    while from.peek().is_some() && from.peek() == to.peek() {
        from.next();
        to.next();
    }
    let mut relative = std::path::PathBuf::new();
    for _ in from {
        relative.push("..");
    }
    relative.extend(to);
    relative.to_string_lossy().into_owned()
}

/// An accessor's parameters are the connection or transaction it runs under, plus one
/// per statement bind.
fn accessor_argument_count(binds: usize) -> usize {
    1 + binds
}

pub(super) fn static_sql_rows(operation: &CustomOperationDeclaration) -> Vec<RustRow> {
    // SQLx 0.9 cannot infer non-null for PostgreSQL expressions, so a statement's
    // nullable carrier may be tool-imposed while its public operation remains non-null.
    operation
        .statements
        .iter()
        .map(|(name, statement)| RustRow {
            name: format!("{}Row", rust_type_identifier(name.as_str())),
            visibility: RustVisibility::Public,
            fields: statement
                .row
                .iter()
                .map(|field| RustMember {
                    name: field.name.to_string(),

                    statement_type: field.ty,
                    nullable: field.nullable,
                })
                .collect(),
        })
        .collect()
}

pub(super) fn static_sql_accessors(
    operation: &CustomOperationDeclaration,
) -> Vec<StaticSqlAccessor> {
    operation
        .statements
        .iter()
        .map(|(name, statement)| StaticSqlAccessor {
            name: name.to_string(),
            statement_constant: format!("{}_STATEMENT", name.as_str().to_ascii_uppercase()),
            row: format!("{}Row", rust_type_identifier(name.as_str())),
            fetch: statement.fetch,
            binds: statement
                .parameters
                .iter()
                .map(|parameter| {
                    accessor_bind(parameter.name.as_str(), parameter.ty, parameter.nullable)
                })
                .collect(),
        })
        .collect()
}

const HEADER: &str = "// @generated from migration IR; do not edit.\n\n";

pub(super) fn emit_static_sql_projection(
    files: &mut BTreeMap<String, Vec<u8>>,
    paths: super::OutputPaths<'_>,
    module_name: &str,
    compiled: &super::operations::CustomDescription<'_>,
) -> Result<(), GenerateError> {
    let transactional = compiled.declaration.type_ != OperationType::Projection;
    let mut source = if transactional {
        quote!(
            use wamn_guest::postgres_statements::Transaction;
        )
    } else {
        quote!(
            use wamn_guest::postgres_statements::Connection;
        )
    };
    for row in &compiled.rows {
        source.extend(emit_rust_row(row));
    }
    for (name, statement) in compiled.statements {
        let constant = rust_ident(&statement_constant_name(name.as_str(), 0, 1));
        let path = sql_include_path(paths, module_name, &statement.path);
        source.extend(quote!(pub(crate) const #constant: &str = include_str!(#path);));
    }
    for accessor in &compiled.accessors {
        let row = compiled
            .rows
            .iter()
            .find(|row| row.name == accessor.row)
            .expect("accessor row exists");
        let state = if transactional {
            quote!(transaction: &mut Transaction)
        } else {
            quote!(connection: &mut Connection)
        };
        let (result, decode) = match accessor.fetch {
            StaticSqlFetch::OptionalOne => (AccessorFetch::Optional, "decode_optional"),
            StaticSqlFetch::BoundedList => (AccessorFetch::All, "decode_all"),
            StaticSqlFetch::One => (AccessorFetch::One, "decode_one"),
        };
        let header = accessor_header(
            &accessor.name,
            RustVisibility::Crate,
            &accessor.binds,
            &state,
        );
        source.extend(emit_accessor(
            &header,
            &accessor.binds,
            row,
            result,
            decode,
            &accessor.statement_constant,
            transactional,
        ));
    }
    insert_rust(
        files,
        &format!("generated/wamn/{module_name}.rs"),
        HEADER,
        source,
    )
}

pub(super) fn wamn_api(
    catalog: &CatalogIr,
    model_name: &wamn_package::manifest::source_name::SourceName,
    model: &ModelDeclaration,
    table: &Table,
    operation_sql: &BTreeMap<String, Vec<String>>,
) -> WamnApi {
    let model_row = format!("{}Row", rust_type_identifier(model_name.as_str()));
    let mut mutation_constraints = Vec::new();
    let mut operation_rows = Vec::new();
    let mut accessors = Vec::new();

    for (action, operation) in &model.operations {
        let paths = operation_sql
            .get(action.as_str())
            .expect("operation SQL was emitted from the same manifest");
        if matches!(
            action,
            CrudAction::Create | CrudAction::Update | CrudAction::Delete
        ) {
            mutation_constraints.push(mutation_constraint_names(
                catalog, table, *action, operation, model,
            ));
        }
        match action {
            CrudAction::Get => accessors.push(WamnAccessor {
                name: "get".to_owned(),
                visibility: RustVisibility::Crate,
                operation: *action,
                statement_constant: statement_constant_name(action.as_str(), 0, paths.len()),
                sql_path: paths[0].clone(),
                row: model_row.clone(),
                fetch: AccessorFetch::Optional,
                binds: vec![bind_for_column(table, "id", "id", false)],
            }),
            CrudAction::Query => {
                let feed_rows = feed_rows(model_name, model, table);
                let query_row = feed_rows
                    .first()
                    .map_or_else(|| model_row.clone(), |row| row.name.clone());
                for (index, (field, direction)) in query_variants(operation).into_iter().enumerate()
                {
                    let mut binds = filter_binds(operation);
                    binds.push(bind_for_column(table, field, "cursor_key", true));
                    let tie_breaker = &operation
                        .pagination
                        .as_ref()
                        .expect("query validation requires pagination")
                        .tie_breaker
                        .field;
                    binds.push(bind_for_column(table, tie_breaker, "cursor_id", true));
                    binds.push(accessor_bind("limit", SqlValueType::Int64, false));
                    accessors.push(WamnAccessor {
                        name: format!("query_{field}_{}", sql::direction_name(direction)),
                        visibility: RustVisibility::Crate,
                        operation: *action,
                        statement_constant: statement_constant_name(
                            action.as_str(),
                            index,
                            paths.len(),
                        ),
                        sql_path: paths[index].clone(),
                        row: query_row.clone(),
                        fetch: AccessorFetch::All,
                        binds,
                    });
                }
                if let Some(changes_row) = feed_rows.get(1) {
                    let mut binds = filter_binds(operation);
                    binds.push(accessor_bind(
                        "cursor_transaction",
                        SqlValueType::Int64,
                        true,
                    ));
                    binds.push(accessor_bind(
                        "cursor_id",
                        crate::key::key_type(table)
                            .expect("validated model key")
                            .column_type(),
                        true,
                    ));
                    binds.push(accessor_bind("limit", SqlValueType::Int64, false));
                    let index = paths.len() - 1;
                    accessors.push(WamnAccessor {
                        name: CHANGES_STATEMENT.to_owned(),
                        visibility: RustVisibility::Crate,
                        operation: *action,
                        statement_constant: statement_constant_name(
                            action.as_str(),
                            index,
                            paths.len(),
                        ),
                        sql_path: paths[index].clone(),
                        row: changes_row.name.clone(),
                        fetch: AccessorFetch::All,
                        binds,
                    });
                }
                operation_rows.extend(feed_rows);
            }
            CrudAction::Create => accessors.push(WamnAccessor {
                name: CREATE_STATEMENT.to_owned(),
                visibility: RustVisibility::Crate,
                operation: *action,
                statement_constant: statement_constant_name(action.as_str(), 0, paths.len()),
                sql_path: paths[0].clone(),
                row: model_row.clone(),
                fetch: AccessorFetch::One,
                binds: std::iter::once("id")
                    .filter(|_| !crate::key::database_generated(table))
                    .chain(operation.writable_fields.iter().map(String::as_str))
                    .map(|field| bind_for_column(table, field, field, false))
                    // The numbered field binds the text guest numbering SQL returned.
                    .chain(
                        model
                            .number_field()
                            .map(|(field, _)| accessor_bind(field, SqlValueType::Text, false)),
                    )
                    .collect(),
            }),
            CrudAction::Update => {
                let result_row = operation_result_row(model_name, table, *action, operation);
                let mut binds = vec![bind_for_column(table, "id", "id", false)];
                let revision = operation
                    .revision_field
                    .as_deref()
                    .expect("update validation requires a revision field");
                binds.push(bind_for_column(
                    table,
                    revision,
                    &format!("expected_{revision}"),
                    false,
                ));
                for field in &operation.writable_fields {
                    binds.push(accessor_bind(
                        &format!("{field}_present"),
                        SqlValueType::Boolean,
                        false,
                    ));
                    binds.push(bind_for_column(
                        table,
                        field,
                        &format!("{field}_value"),
                        true,
                    ));
                }
                accessors.push(WamnAccessor {
                    name: "update".to_owned(),
                    visibility: RustVisibility::Crate,
                    operation: *action,
                    statement_constant: statement_constant_name(action.as_str(), 0, paths.len()),
                    sql_path: paths[0].clone(),
                    row: result_row.name.clone(),
                    fetch: AccessorFetch::One,
                    binds,
                });
                operation_rows.push(result_row);
            }
            CrudAction::Delete => {
                let result_row = operation_result_row(model_name, table, *action, operation);
                let revision = operation
                    .revision_field
                    .as_deref()
                    .expect("delete validation requires a revision field");
                accessors.push(WamnAccessor {
                    name: "delete".to_owned(),
                    visibility: RustVisibility::Crate,
                    operation: *action,
                    statement_constant: statement_constant_name(action.as_str(), 0, paths.len()),
                    sql_path: paths[0].clone(),
                    row: result_row.name.clone(),
                    fetch: AccessorFetch::One,
                    binds: vec![
                        bind_for_column(table, "id", "id", false),
                        bind_for_column(table, revision, &format!("expected_{revision}"), false),
                    ],
                });
                operation_rows.push(result_row);
            }
        }
    }

    WamnApi {
        statement_visibility: RustVisibility::Crate,
        mutation_constraints,
        operation_rows,
        accessors,
    }
}

fn mutation_constraint_names(
    catalog: &CatalogIr,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
    model: &ModelDeclaration,
) -> MutationConstraintNames {
    let mut unique = Vec::new();
    let mut foreign_key = Vec::new();
    let mut check = Vec::new();
    for constraint in operation_constraints(catalog, table, action, operation, model) {
        match constraint.constraint_type() {
            ConstraintType::PrimaryKey { .. } | ConstraintType::Unique { .. } => {
                unique.push(constraint.name().to_owned());
            }
            ConstraintType::ForeignKey { .. } => {
                foreign_key.push(constraint.name().to_owned());
            }
            ConstraintType::Check { .. } => {
                check.push(constraint.name().to_owned());
            }
        }
    }
    let exclusion = operation_exclusions(table, action, operation)
        .into_iter()
        .map(|exclusion| exclusion.name().to_owned())
        .collect();
    MutationConstraintNames {
        unique: constraint_name_slice(action, "unique", unique),
        foreign_key: constraint_name_slice(action, "foreign_key", foreign_key),
        check: constraint_name_slice(action, "check", check),
        exclusion: constraint_name_slice(action, "exclusion", exclusion),
    }
}

fn constraint_name_slice(
    action: CrudAction,
    category: &str,
    names: Vec<String>,
) -> ConstraintNameSlice {
    ConstraintNameSlice {
        constant: format!(
            "{}_{}_CONSTRAINTS",
            action.as_str().to_ascii_uppercase(),
            category.to_ascii_uppercase()
        ),
        visibility: RustVisibility::Crate,
        names,
    }
}

fn filter_binds(operation: &OperationDeclaration) -> Vec<AccessorBind> {
    let mut binds = operation
        .filters
        .iter()
        .map(|filter| {
            accessor_bind(
                &format!("{}_filter", filter.field),
                SqlValueType::Json,
                true,
            )
        })
        .collect::<Vec<_>>();
    if operation.search.is_some() {
        binds.push(accessor_bind("search", SqlValueType::Text, true));
    }
    binds
}

/// The rows of a list read with a change feed: the page row and the changes
/// row, or none when the model has no feed.
///
/// Both statements return one outer row even when no model row matched, so
/// every column is nullable (`crate::sql::query`, `crate::sql::changes`).
fn feed_rows(
    model_name: &wamn_package::manifest::source_name::SourceName,
    model: &ModelDeclaration,
    table: &Table,
) -> Vec<RustRow> {
    if !model.feed_enabled() {
        return Vec::new();
    }
    let member = |name: &str, ty: SqlValueType| RustMember {
        name: name.to_owned(),

        statement_type: ty,
        nullable: true,
    };
    let model_members = table.columns().iter().map(|column| RustMember {
        name: column.name().to_owned(),

        statement_type: column.column_type(),
        nullable: true,
    });
    let model_type = rust_type_identifier(model_name.as_str());
    let mut fields = table
        .columns()
        .iter()
        .map(|column| RustMember {
            name: if column.name() == "row_version" {
                "model_row_version".to_owned()
            } else {
                column.name().to_owned()
            },

            statement_type: column.column_type(),
            nullable: column.nullable(),
        })
        .collect::<Vec<_>>();
    fields.push(RustMember {
        name: "row_version".to_owned(),

        statement_type: SqlValueType::Int64,
        nullable: false,
    });
    vec![
        RustRow {
            name: format!("{model_type}QueryFeedRow"),
            visibility: RustVisibility::Public,
            fields: std::iter::once(member("feed_cursor", SqlValueType::Int64))
                .chain(model_members.clone())
                .collect(),
        },
        RustRow {
            name: format!("{model_type}ChangesRow"),
            visibility: RustVisibility::Public,
            fields: [
                member("feed_end", SqlValueType::Int64),
                member("expired", SqlValueType::Boolean),
                member(
                    "change_id",
                    crate::key::key_type(table)
                        .expect("validated model key")
                        .column_type(),
                ),
                member("change_transaction", SqlValueType::Int64),
                member("present", SqlValueType::Boolean),
            ]
            .into_iter()
            .chain(model_members)
            .collect(),
        },
        RustRow {
            name: format!("{model_type}FeedRow"),
            visibility: RustVisibility::Public,
            fields,
        },
    ]
}

fn operation_result_row(
    model_name: &wamn_package::manifest::source_name::SourceName,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> RustRow {
    let mut fields = vec![RustMember {
        name: "outcome".to_owned(),

        statement_type: SqlValueType::Text,
        nullable: true,
    }];
    // Update and delete both report the revision the row carried.
    let revision = operation
        .revision_field
        .as_deref()
        .expect("update and delete validation require a revision field");
    // The observed revision is the column the model declares, at its width.
    let revision_type = table
        .columns()
        .iter()
        .find(|column| column.name() == revision)
        .expect("the revision field is a column")
        .column_type();
    fields.push(RustMember {
        name: ["observed_", revision].concat(),

        statement_type: revision_type,
        nullable: true,
    });
    if action == CrudAction::Update {
        fields.extend(table.columns().iter().map(|column| RustMember {
            name: column.name().to_owned(),

            statement_type: column.column_type(),
            nullable: true,
        }));
    }
    RustRow {
        name: format!(
            "{}{}Row",
            rust_type_identifier(model_name.as_str()),
            rust_type_identifier(action.as_str())
        ),
        visibility: RustVisibility::Public,
        fields,
    }
}

fn bind_for_column(
    table: &Table,
    column_name: &str,
    parameter_name: &str,
    optional: bool,
) -> AccessorBind {
    let column = column(table, column_name).expect("operation validation resolved the column");
    accessor_bind(
        parameter_name,
        column.column_type(),
        optional || column.nullable(),
    )
}

fn accessor_bind(parameter: &str, ty: SqlValueType, nullable: bool) -> AccessorBind {
    AccessorBind {
        parameter: parameter.to_owned(),
        nullable,

        statement_type: ty,
    }
}

pub(super) fn emit_projection(
    files: &mut BTreeMap<String, Vec<u8>>,
    paths: super::OutputPaths<'_>,
    compiled: &super::operations::ModelDescription<'_>,
) -> Result<(), GenerateError> {
    let model_name = compiled.name;
    let table = &compiled.shape;
    let operation_sql = compiled.paths;
    let api = &compiled.api;
    let mut source = TokenStream::new();
    if api
        .accessors
        .iter()
        .any(|accessor| !accessor.operation.changes_records())
    {
        source.extend(quote!(
            use wamn_guest::postgres_statements::Connection;
        ));
    }
    let model_row = RustRow {
        name: format!("{}Row", rust_type_identifier(model_name.as_str())),
        visibility: RustVisibility::Public,
        fields: table
            .columns
            .iter()
            .map(|column| RustMember {
                name: column.name().to_owned(),

                statement_type: column.column_type(),
                nullable: column.nullable(),
            })
            .collect(),
    };
    source.extend(emit_rust_row(&model_row));
    for row in &api.operation_rows {
        source.extend(emit_rust_row(row));
    }
    for (action, sql_paths) in operation_sql {
        for (index, path) in sql_paths.iter().enumerate() {
            let visibility = visibility(api.statement_visibility);
            let name = rust_ident(&statement_constant_name(action, index, sql_paths.len()));
            let path = sql_include_path(paths, model_name.as_str(), path);
            source.extend(quote!(#visibility const #name: &str = include_str!(#path);));
        }
    }
    for constraints in &api.mutation_constraints {
        source.extend(emit_constraint_name_slice(&constraints.unique));
        source.extend(emit_constraint_name_slice(&constraints.foreign_key));
        source.extend(emit_constraint_name_slice(&constraints.check));
        source.extend(emit_constraint_name_slice(&constraints.exclusion));
    }
    for accessor in &api.accessors {
        let row = if accessor.row == model_row.name {
            &model_row
        } else {
            api.operation_rows
                .iter()
                .find(|row| row.name == accessor.row)
                .expect("accessor row exists")
        };
        source.extend(emit_wamn_accessor(accessor, row));
    }
    insert_rust(
        files,
        &format!("generated/wamn/{model_name}.rs"),
        HEADER,
        source,
    )
}

fn visibility(visibility: RustVisibility) -> TokenStream {
    match visibility {
        RustVisibility::Public => quote!(pub),
        RustVisibility::Crate => quote!(pub(crate)),
    }
}

fn emit_constraint_name_slice(constraints: &ConstraintNameSlice) -> TokenStream {
    let names = &constraints.names;
    let visibility = visibility(constraints.visibility);
    let name = rust_ident(&constraints.constant);
    quote!(#visibility const #name: &[&str] = &[#(#names),*];)
}

fn emit_rust_row(row: &RustRow) -> TokenStream {
    let name = rust_ident(&row.name);
    let visibility = visibility(row.visibility);
    let fields = row.fields.iter().map(|field| {
        let name = rust_ident(&field.name);
        let ty = projected_rust_type(field.statement_type, field.nullable);
        quote!(pub #name: #ty,)
    });
    quote!(#[derive(Debug)] #visibility struct #name { #(#fields)* })
}

fn emit_wamn_accessor(accessor: &WamnAccessor, row: &RustRow) -> TokenStream {
    let transactional = accessor.operation.changes_records();
    let state = if transactional {
        quote!(transaction: &mut wamn_guest::postgres_statements::Transaction)
    } else {
        quote!(connection: &mut Connection)
    };
    let decode = match accessor.fetch {
        AccessorFetch::Optional => "decode_optional",
        AccessorFetch::One => "decode_one",
        AccessorFetch::All => "decode_all",
    };
    let header = accessor_header(&accessor.name, accessor.visibility, &accessor.binds, &state);
    emit_accessor(
        &header,
        &accessor.binds,
        row,
        accessor.fetch,
        decode,
        &accessor.statement_constant,
        transactional,
    )
}

fn accessor_header(
    name: &str,
    visibility_value: RustVisibility,
    binds: &[AccessorBind],
    state: &TokenStream,
) -> TokenStream {
    // The bind list fixes the number of parameters.
    let arity = (accessor_argument_count(binds.len()) > CLIPPY_ARGUMENT_THRESHOLD).then(|| {
        quote!(#[expect(clippy::too_many_arguments, reason = "the parameters are the statement's bind list")])
    });
    let visibility = visibility(visibility_value);
    let name = rust_ident(name);
    let parameters = binds.iter().map(|bind| {
        let parameter = rust_ident(&bind.parameter);
        let ty = projected_rust_type(bind.statement_type, bind.nullable);
        quote!(#parameter: #ty,)
    });
    quote!(#arity #visibility async fn #name(#state, #(#parameters)*))
}

fn emit_accessor(
    header: &TokenStream,
    binds: &[AccessorBind],
    row: &RustRow,
    fetch: AccessorFetch,
    decode: &str,
    statement: &str,
    transactional: bool,
) -> TokenStream {
    let statement = rust_ident(statement);
    let decode = rust_ident(decode);
    let state = if transactional {
        quote!(transaction)
    } else {
        quote!(connection)
    };
    let arguments = binds.iter().map(|bind| {
        let parameter = rust_ident(&bind.parameter);
        quote!(wamn_guest::postgres_statements::into_sql_value(#parameter),)
    });
    let row_name = rust_ident(&row.name);
    let result = match fetch {
        AccessorFetch::Optional => quote!(Option<#row_name>),
        AccessorFetch::All => quote!(Vec<#row_name>),
        AccessorFetch::One => quote!(#row_name),
    };
    let fields = row.fields.iter().map(|field| {
        let column = &field.name;
        let name = rust_ident(column);
        quote!(#name: row.decode(#column)?,)
    });
    quote! {
        #header -> Result<#result, wamn_guest::postgres_statements::StatementError> {
            let rows = #state.query(#statement, vec![#(#arguments)*]).await?;
            wamn_guest::postgres_statements::#decode(#statement, rows, |row| {
                Ok(#row_name { #(#fields)* })
            })
        }
    }
}

fn statement_constant_name(action: &str, index: usize, path_count: usize) -> String {
    let suffix = if path_count == 1 {
        String::new()
    } else {
        format!("_{index}")
    };
    format!("{}{}_STATEMENT", action.to_ascii_uppercase(), suffix)
}

pub(super) fn projected_rust_type(ty: SqlValueType, optional: bool) -> TokenStream {
    let base = match ty {
        SqlValueType::Boolean => quote!(bool),
        SqlValueType::Int32 => quote!(i32),
        SqlValueType::Int64 => quote!(i64),
        SqlValueType::Float64 => quote!(f64),
        SqlValueType::Text => quote!(String),
        SqlValueType::Bytes => quote!(Vec<u8>),
        SqlValueType::Numeric => quote!(wamn_guest::postgres_statements::Numeric),
        SqlValueType::Timestamptz => quote!(wamn_guest::postgres_statements::TimestampTz),
        SqlValueType::Json => quote!(wamn_guest::postgres_statements::Json),
        SqlValueType::Uuid => quote!(wamn_guest::postgres_statements::Uuid),
    };
    if optional {
        quote!(Option<#base>)
    } else {
        base
    }
}
