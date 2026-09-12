//! Rust projections, accessors, and parity output.

use std::fmt::Write as _;

use super::{
    AccessorBind, AccessorFetch, BTreeMap, CLAIM_COMMAND_COLUMN, CLAIM_KEY_COLUMN,
    CREATE_CLAIM_STATEMENT, CREATE_REPLAY_STATEMENT, CREATE_STATEMENT, Column, ColumnType,
    ConstraintKind, ConstraintNameSlice, CrudAction, CustomOperationDeclaration, GenerateError,
    ModelDeclaration, MutationConstraintNames, NativeBindFixture, OperationDeclaration, Projection,
    ProjectionContents, RustMember, RustRow, RustVisibility, StaticSqlAccessor, StaticSqlFetch,
    Table, WamnAccessor, WamnApi, column, insert_bytes, insert_json, json, operation_constraints,
    operation_exclusions, query_variants, rust_identifier, rust_type_identifier, sha256, sql,
};

pub(super) fn static_sql_rows(
    operation: &CustomOperationDeclaration,
    projection: Projection,
) -> Vec<RustRow> {
    // SQLx 0.9 cannot infer non-null for PostgreSQL expressions, so a statement's
    // nullable carrier may be tool-imposed while its public operation remains non-null.
    operation
        .statements
        .iter()
        .map(|(name, statement)| RustRow {
            name: format!("{}Row", rust_type_identifier(name)),
            visibility: RustVisibility::Crate,
            fields: statement
                .row
                .iter()
                .map(|field| RustMember {
                    name: rust_identifier(&field.name)
                        .expect("static SQL value names were validated for Rust"),
                    rust_type: projected_rust_type(field.ty, projection, field.nullable),
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
            name: name.clone(),
            statement_digest_constant: format!("{}_DIGEST", name.to_ascii_uppercase()),
            row: format!("{}Row", rust_type_identifier(name)),
            fetch: statement.fetch,
            binds: statement
                .parameters
                .iter()
                .map(|parameter| accessor_bind(&parameter.name, parameter.ty, parameter.nullable))
                .collect(),
        })
        .collect()
}

pub(super) fn static_sql_native_bind_fixtures(
    accessors: &[StaticSqlAccessor],
) -> Vec<NativeBindFixture> {
    accessors
        .iter()
        .flat_map(|accessor| {
            accessor.binds.iter().map(|bind| NativeBindFixture {
                accessor: accessor.name.clone(),
                parameter: bind.parameter.clone(),
                function: format!("{}_{}_bind_fixture", accessor.name, bind.parameter),
                visibility: RustVisibility::Crate,
                rust_type: bind.native_rust.clone(),
                value: native_inert_value(bind),
            })
        })
        .collect()
}

pub(super) fn emit_static_sql_parity(
    files: &mut BTreeMap<String, Vec<u8>>,
    module_name: &str,
    operation: &CustomOperationDeclaration,
    accessors: &[StaticSqlAccessor],
) -> Result<(), GenerateError> {
    let fields = operation
        .statements
        .iter()
        .flat_map(|(statement, declaration)| {
            declaration.row.iter().map(move |field| {
                json!({
                    "field": format!("{statement}.{}", field.name),
                    "postgres": sql::postgres_type(field.ty),
                    "wamn_sql_value": field.ty.as_str(),
                    "native_rust": projected_rust_type(field.ty, Projection::Native, field.nullable),
                    "wamn_rust": projected_rust_type(field.ty, Projection::Wamn, field.nullable),
                    "nullable": field.nullable,
                })
            })
        })
        .collect::<Vec<_>>();
    let accessor_binds = accessors
        .iter()
        .flat_map(|accessor| {
            accessor.binds.iter().map(|bind| {
                json!({
                    "accessor": accessor.name,
                    "parameter": bind.parameter,
                    "postgres": bind.postgres,
                    "nullable": bind.nullable,
                    "native_rust": bind.native_rust,
                    "wamn_rust": bind.wamn_rust,
                })
            })
        })
        .collect::<Vec<_>>();
    insert_json(
        files,
        &format!("generated/parity/{module_name}.json"),
        &json!({
            "model": module_name,
            "rule": "same_sql_file_two_projection_structs",
            "fields": fields,
            "accessor_binds": accessor_binds,
        }),
    )
}

pub(super) fn emit_static_sql_projection(
    files: &mut BTreeMap<String, Vec<u8>>,
    sql_corpus: &BTreeMap<String, Vec<u8>>,
    module_name: &str,
    operation: &CustomOperationDeclaration,
    rows: &[RustRow],
    bind_fixtures: &[NativeBindFixture],
    projection: Projection,
) -> Result<(), GenerateError> {
    let mut source = String::from("// @generated from migration IR; do not edit.\n\n");
    if matches!(projection, Projection::Wamn) && operation.claim.is_none() {
        source.push_str("use wamn_postgres_statements::Transaction;\n\n");
    }
    for row in rows {
        emit_rust_row(&mut source, row, projection);
    }
    for (name, statement) in &operation.statements {
        match projection {
            Projection::Native => {
                writeln!(
                    &mut source,
                    "pub(crate) const {}_SQL: &str = include_str!(\"../../{}\");",
                    name.to_ascii_uppercase(),
                    statement.path,
                )
                .expect("writing to a String cannot fail");
            }
            Projection::Wamn => {
                let digest = sha256(
                    sql_corpus
                        .get(&statement.path)
                        .expect("validated statement is present in the SQL corpus"),
                );
                writeln!(
                    &mut source,
                    "pub(crate) const {}_DIGEST: &str = {digest:?};",
                    name.to_ascii_uppercase(),
                )
                .expect("writing to a String cannot fail");
            }
        }
    }
    source.push('\n');
    for fixture in bind_fixtures {
        emit_native_bind_fixture(&mut source, fixture);
    }
    if !bind_fixtures.is_empty() {
        source.push('\n');
    }
    if matches!(projection, Projection::Wamn) {
        let accessors = static_sql_accessors(operation);
        if let Some(claim) = &operation.claim {
            let finalizer = accessors
                .iter()
                .find(|accessor| accessor.name == claim.finalize)
                .expect("claim validation resolved the finalizer");
            emit_claim_transaction(&mut source, &finalizer.row);
        }
        for accessor in accessors {
            let row = rows
                .iter()
                .find(|row| row.name == accessor.row)
                .expect("static accessor row was generated from the same statement");
            emit_static_sql_wamn_accessor(
                &mut source,
                &accessor,
                row,
                operation
                    .claim
                    .as_ref()
                    .map(|claim| claim.finalize.as_str()),
            );
        }
    }
    while source.ends_with("\n\n") {
        source.pop();
    }
    let directory = match projection {
        Projection::Native => "native-verifier",
        Projection::Wamn => "wamn",
    };
    insert_bytes(
        files,
        &format!("generated/{directory}/{module_name}.rs"),
        source.into_bytes(),
    )
}

/// Keep the transaction private until the declared finalizer succeeds.
fn emit_claim_transaction(source: &mut String, finalized_row: &str) {
    writeln!(
        source,
        r#"/// One claim and its work, with no commit before finalization.
#[derive(Debug)]
pub(crate) struct PendingClaim {{
    transaction: wamn_postgres_statements::Transaction,
}}

/// Transfer the open transaction into this command's claim scope.
pub(crate) fn begin_claim(transaction: wamn_postgres_statements::Transaction) -> PendingClaim {{
    PendingClaim {{ transaction }}
}}

/// A finalized claim whose transaction can now commit.
#[derive(Debug)]
pub(crate) struct FinalizedClaim {{
    transaction: wamn_postgres_statements::Transaction,
    pub row: {finalized_row},
}}

impl FinalizedClaim {{
    /// Commit the claim and its work together.
    pub(crate) async fn commit(self) -> Result<(), wamn_postgres_statements::StatementError> {{
        self.transaction.commit().await
    }}
}}
"#
    )
    .expect("writing to a String cannot fail");
}

fn emit_static_sql_wamn_accessor(
    source: &mut String,
    accessor: &StaticSqlAccessor,
    row: &RustRow,
    claim_finalize: Option<&str>,
) {
    let finalizes_claim = claim_finalize == Some(accessor.name.as_str());
    let function = rust_identifier(&accessor.name)
        .expect("static SQL statement names were validated for Rust");
    writeln!(source, "pub(crate) async fn {function}(").expect("writing to a String cannot fail");
    if finalizes_claim {
        source.push_str("    mut claim: PendingClaim,\n");
    } else if claim_finalize.is_some() {
        source.push_str("    claim: &mut PendingClaim,\n");
    } else {
        source.push_str("    transaction: &mut Transaction,\n");
    }
    for bind in &accessor.binds {
        let parameter = rust_identifier(&bind.parameter)
            .expect("static SQL parameter names were validated for Rust");
        writeln!(source, "    {parameter}: {},", bind.wamn_rust)
            .expect("writing to a String cannot fail");
    }
    writeln!(
        source,
        ") -> Result<{}, wamn_postgres_statements::StatementError> {{",
        if finalizes_claim {
            "FinalizedClaim".to_owned()
        } else {
            static_sql_accessor_result_type(accessor)
        },
    )
    .expect("writing to a String cannot fail");
    writeln!(
        source,
        "    let rows = {}.run({}, vec![",
        if claim_finalize.is_some() {
            "claim.transaction"
        } else {
            "transaction"
        },
        accessor.statement_digest_constant,
    )
    .expect("writing to a String cannot fail");
    for bind in &accessor.binds {
        let parameter = rust_identifier(&bind.parameter)
            .expect("static SQL parameter names were validated for Rust");
        writeln!(
            source,
            "        wamn_postgres_statements::into_sql_value({parameter}),"
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str("    ]).await?;\n");
    let decode_function = match accessor.fetch {
        StaticSqlFetch::OptionalOne => "decode_optional",
        StaticSqlFetch::BoundedList => "decode_all",
        StaticSqlFetch::One => "decode_one",
    };
    emit_decode_result(
        source,
        row,
        decode_function,
        &accessor.statement_digest_constant,
        finalizes_claim,
    );
}

fn static_sql_accessor_result_type(accessor: &StaticSqlAccessor) -> String {
    match accessor.fetch {
        StaticSqlFetch::OptionalOne => format!("Option<{}>", accessor.row),
        StaticSqlFetch::BoundedList => format!("Vec<{}>", accessor.row),
        StaticSqlFetch::One => accessor.row.clone(),
    }
}

pub(super) fn emit_parity(
    files: &mut BTreeMap<String, Vec<u8>>,
    model_name: &str,
    table: &Table,
    wamn_api: &WamnApi,
) -> Result<(), GenerateError> {
    let fields = table
        .columns()
        .iter()
        .map(|column| {
            json!({
                "field": column.name(),
                "postgres": sql::postgres_type(column.column_type()),
                "wamn_sql_value": column.column_type().as_str(),
                "native_rust": rust_type(column, Projection::Native),
                "wamn_rust": rust_type(column, Projection::Wamn),
                "nullable": column.nullable(),
            })
        })
        .collect::<Vec<_>>();
    let accessor_binds = wamn_api
        .accessors
        .iter()
        .flat_map(|accessor| {
            accessor.binds.iter().map(|bind| {
                json!({
                    "accessor": accessor.name,
                    "parameter": bind.parameter,
                    "postgres": bind.postgres,
                    "nullable": bind.nullable,
                    "native_rust": bind.native_rust,
                    "wamn_rust": bind.wamn_rust,
                })
            })
        })
        .collect::<Vec<_>>();
    insert_json(
        files,
        &format!("generated/parity/{model_name}.json"),
        &json!({
            "model": model_name,
            "rule": "same_sql_file_two_projection_structs",
            "fields": fields,
            "accessor_binds": accessor_binds,
        }),
    )
}

pub(super) fn wamn_api(
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    claim: Option<&sql::Claim<'_>>,
    operation_sql: &BTreeMap<String, Vec<String>>,
) -> WamnApi {
    let model_row = format!("{}Row", rust_type_identifier(model_name));
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
            mutation_constraints.push(mutation_constraint_names(table, *action, operation));
        }
        match action {
            CrudAction::Get => accessors.push(WamnAccessor {
                name: "get".to_owned(),
                visibility: RustVisibility::Crate,
                operation: *action,
                statement_digest_constant: statement_digest_constant_name(
                    action.as_str(),
                    0,
                    paths.len(),
                ),
                sql_path: paths[0].clone(),
                row: model_row.clone(),
                fetch: AccessorFetch::Optional,
                binds: vec![bind_for_column(table, "id", "id", false)],
            }),
            CrudAction::Query => {
                for (index, (field, direction)) in query_variants(operation).into_iter().enumerate()
                {
                    let mut binds = operation
                        .filters
                        .iter()
                        .map(|filter| {
                            accessor_bind(
                                &format!("{}_filter", filter.field),
                                ColumnType::Json,
                                true,
                            )
                        })
                        .collect::<Vec<_>>();
                    binds.push(bind_for_column(table, field, "cursor_key", true));
                    let tie_breaker = &operation
                        .pagination
                        .as_ref()
                        .expect("query validation requires pagination")
                        .tie_breaker
                        .field;
                    binds.push(bind_for_column(table, tie_breaker, "cursor_id", true));
                    binds.push(accessor_bind("limit", ColumnType::Int64, false));
                    accessors.push(WamnAccessor {
                        name: format!("query_{field}_{}", sql::direction_name(direction)),
                        visibility: RustVisibility::Crate,
                        operation: *action,
                        statement_digest_constant: statement_digest_constant_name(
                            action.as_str(),
                            index,
                            paths.len(),
                        ),
                        sql_path: paths[index].clone(),
                        row: model_row.clone(),
                        fetch: AccessorFetch::All,
                        binds,
                    });
                }
            }
            CrudAction::Create => {
                let claim = claim.expect("create validation resolved the command claim");
                let rows = create_rows(model_name, table, claim, Projection::Wamn);
                let key_bind = accessor_bind(CLAIM_KEY_COLUMN, ColumnType::Text, false);
                let mut binds = claim
                    .identities
                    .iter()
                    .map(|(field, _)| {
                        bind_for_column(
                            table,
                            field,
                            &rust_identifier(field)
                                .expect("model field names were validated for Rust"),
                            false,
                        )
                    })
                    .collect::<Vec<_>>();
                binds.extend(operation.writable_fields.iter().map(|field| {
                    bind_for_column(
                        table,
                        field,
                        &rust_identifier(field).expect("model field names were validated for Rust"),
                        false,
                    )
                }));
                for (index, (name, row, fetch, accessor_binds)) in [
                    (
                        CREATE_CLAIM_STATEMENT,
                        rows[0].name.clone(),
                        AccessorFetch::Optional,
                        vec![
                            key_bind.clone(),
                            accessor_bind(CLAIM_COMMAND_COLUMN, ColumnType::Bytes, false),
                        ],
                    ),
                    (
                        CREATE_REPLAY_STATEMENT,
                        rows[1].name.clone(),
                        AccessorFetch::Optional,
                        vec![key_bind],
                    ),
                    (
                        CREATE_STATEMENT,
                        model_row.clone(),
                        AccessorFetch::One,
                        binds,
                    ),
                ]
                .into_iter()
                .enumerate()
                {
                    accessors.push(WamnAccessor {
                        name: name.to_owned(),
                        visibility: RustVisibility::Crate,
                        operation: *action,
                        statement_digest_constant: statement_digest_constant_name(
                            action.as_str(),
                            index,
                            paths.len(),
                        ),
                        sql_path: paths[index].clone(),
                        row,
                        fetch,
                        binds: accessor_binds,
                    });
                }
                operation_rows.extend(rows);
            }
            CrudAction::Update => {
                let result_row =
                    operation_result_row(model_name, table, *action, operation, Projection::Wamn);
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
                        ColumnType::Boolean,
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
                    statement_digest_constant: statement_digest_constant_name(
                        action.as_str(),
                        0,
                        paths.len(),
                    ),
                    sql_path: paths[0].clone(),
                    row: result_row.name.clone(),
                    fetch: AccessorFetch::One,
                    binds,
                });
                operation_rows.push(result_row);
            }
            CrudAction::Delete => {
                let result_row =
                    operation_result_row(model_name, table, *action, operation, Projection::Wamn);
                let revision = operation
                    .revision_field
                    .as_deref()
                    .expect("delete validation requires a revision field");
                accessors.push(WamnAccessor {
                    name: "delete".to_owned(),
                    visibility: RustVisibility::Crate,
                    operation: *action,
                    statement_digest_constant: statement_digest_constant_name(
                        action.as_str(),
                        0,
                        paths.len(),
                    ),
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
        statement_digest_visibility: RustVisibility::Crate,
        mutation_constraints,
        operation_rows,
        accessors,
    }
}

fn mutation_constraint_names(
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> MutationConstraintNames {
    let mut unique = Vec::new();
    let mut foreign_key = Vec::new();
    let mut check = Vec::new();
    for constraint in operation_constraints(table, action, operation) {
        match constraint.kind() {
            ConstraintKind::PrimaryKey { .. } | ConstraintKind::Unique { .. } => {
                unique.push(constraint.name().to_owned());
            }
            ConstraintKind::ForeignKey { .. } => {
                foreign_key.push(constraint.name().to_owned());
            }
            ConstraintKind::Check { .. } => {
                check.push(constraint.name().to_owned());
            }
        }
    }
    let exclusion = operation_exclusions(table, action, operation)
        .into_iter()
        .map(|exclusion| exclusion.name().to_owned())
        .collect();
    MutationConstraintNames {
        operation: action,
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

pub(super) fn native_bind_fixtures(api: &WamnApi) -> Vec<NativeBindFixture> {
    api.accessors
        .iter()
        .flat_map(|accessor| {
            accessor.binds.iter().map(|bind| NativeBindFixture {
                accessor: accessor.name.clone(),
                parameter: bind.parameter.clone(),
                function: format!("{}_{}_bind_fixture", accessor.name, bind.parameter),
                visibility: RustVisibility::Crate,
                rust_type: bind.native_rust.clone(),
                value: native_inert_value(bind),
            })
        })
        .collect()
}

fn native_inert_value(bind: &AccessorBind) -> String {
    if bind.nullable {
        return "None".to_owned();
    }
    match bind.postgres.as_str() {
        "boolean" => "false".to_owned(),
        "int4" => "0_i32".to_owned(),
        "int8" => "0_i64".to_owned(),
        "float8" => "0.0_f64".to_owned(),
        "text" => "String::new()".to_owned(),
        "bytea" => "Vec::new()".to_owned(),
        "numeric" => "rust_decimal::Decimal::ZERO".to_owned(),
        "timestamptz" => "chrono::DateTime::<chrono::Utc>::UNIX_EPOCH".to_owned(),
        "jsonb" => "serde_json::Value::Null".to_owned(),
        "uuid" => "uuid::Uuid::nil()".to_owned(),
        _ => unreachable!("accessor binds use the closed PostgreSQL vocabulary"),
    }
}

pub(super) fn operation_result_rows(
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    claim: Option<&sql::Claim<'_>>,
    projection: Projection,
) -> Vec<RustRow> {
    model
        .operations
        .iter()
        .flat_map(|(action, operation)| match action {
            CrudAction::Create => create_rows(
                model_name,
                table,
                claim.expect("create validation resolved the command claim"),
                projection,
            )
            .to_vec(),
            CrudAction::Update | CrudAction::Delete => vec![operation_result_row(
                model_name, table, *action, operation, projection,
            )],
            CrudAction::Get | CrudAction::Query => Vec::new(),
        })
        .collect()
}

/// The two rows a generated create needs beyond the model row: what the claim
/// minted, and what a replay reads back.
///
/// The replay row carries the canonical command beside the row so the caller
/// can refuse a key rebound to a different request without a second read.
fn create_rows(
    model_name: &str,
    table: &Table,
    claim: &sql::Claim<'_>,
    projection: Projection,
) -> [RustRow; 2] {
    let model_type = rust_type_identifier(model_name);
    let claim_fields = claim
        .identities
        .iter()
        .map(|(_, claim_column)| {
            let column = column(claim.table, claim_column)
                .expect("claim validation resolved every identity column");
            RustMember {
                name: rust_identifier(claim_column)
                    .expect("claim column names were validated for Rust"),
                rust_type: rust_type(column, projection),
                statement_type: column.column_type(),
                nullable: column.nullable(),
            }
        })
        .collect::<Vec<_>>();
    let mut replay_fields = vec![RustMember {
        name: CLAIM_COMMAND_COLUMN.to_owned(),
        rust_type: projected_rust_type(ColumnType::Bytes, projection, false),
        statement_type: ColumnType::Bytes,
        nullable: false,
    }];
    replay_fields.extend(table.columns().iter().map(|column| RustMember {
        name: rust_identifier(column.name()).expect("model fields were validated for Rust"),
        rust_type: rust_type(column, projection),
        statement_type: column.column_type(),
        nullable: column.nullable(),
    }));
    [
        RustRow {
            name: format!("{model_type}CreateClaimRow"),
            visibility: RustVisibility::Public,
            fields: claim_fields,
        },
        RustRow {
            name: format!("{model_type}CreateReplayRow"),
            visibility: RustVisibility::Public,
            fields: replay_fields,
        },
    ]
}

fn operation_result_row(
    model_name: &str,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
    projection: Projection,
) -> RustRow {
    let mut fields = vec![RustMember {
        name: "outcome".to_owned(),
        rust_type: "Option<String>".to_owned(),
        statement_type: ColumnType::Text,
        nullable: true,
    }];
    if action == CrudAction::Update {
        let revision = operation
            .revision_field
            .as_deref()
            .expect("update validation requires a revision field");
        fields.push(RustMember {
            name: format!(
                "observed_{}",
                rust_identifier(revision).expect("model field names were validated for Rust")
            ),
            rust_type: "Option<i64>".to_owned(),
            statement_type: ColumnType::Int64,
            nullable: true,
        });
        fields.extend(table.columns().iter().map(|column| RustMember {
            name: rust_identifier(column.name()).expect("model fields were validated for Rust"),
            rust_type: optional_rust_type(column, projection),
            statement_type: column.column_type(),
            nullable: true,
        }));
    }
    RustRow {
        name: format!(
            "{}{}Row",
            rust_type_identifier(model_name),
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

fn accessor_bind(parameter: &str, ty: ColumnType, nullable: bool) -> AccessorBind {
    AccessorBind {
        parameter: parameter.to_owned(),
        postgres: sql::postgres_type(ty).to_owned(),
        nullable,
        native_rust: projected_rust_type(ty, Projection::Native, nullable),
        wamn_rust: projected_rust_type(ty, Projection::Wamn, nullable),
        statement_type: ty,
    }
}

pub(super) fn emit_projection(
    files: &mut BTreeMap<String, Vec<u8>>,
    sql_corpus: &BTreeMap<String, Vec<u8>>,
    model_name: &str,
    table: &Table,
    operation_sql: &BTreeMap<String, Vec<String>>,
    contents: ProjectionContents<'_>,
) -> Result<(), GenerateError> {
    let (projection, operation_rows, native_bind_fixtures, wamn_api) = match contents {
        ProjectionContents::Native {
            operation_rows,
            bind_fixtures,
        } => (Projection::Native, operation_rows, bind_fixtures, None),
        ProjectionContents::Wamn(api) => (
            Projection::Wamn,
            api.operation_rows.as_slice(),
            &[][..],
            Some(api),
        ),
    };
    let mut source = String::from("// @generated from migration IR; do not edit.\n\n");
    if matches!(projection, Projection::Wamn) {
        source.push_str("use wamn_postgres_statements::Connection;\n\n");
    }
    let model_row = RustRow {
        name: format!("{}Row", rust_type_identifier(model_name)),
        visibility: RustVisibility::Public,
        fields: table
            .columns()
            .iter()
            .map(|column| RustMember {
                name: rust_identifier(column.name()).expect("model fields were validated for Rust"),
                rust_type: rust_type(column, projection),
                statement_type: column.column_type(),
                nullable: column.nullable(),
            })
            .collect(),
    };
    emit_rust_row(&mut source, &model_row, projection);
    for row in operation_rows {
        emit_rust_row(&mut source, row, projection);
    }

    for (action, paths) in operation_sql {
        for (index, path) in paths.iter().enumerate() {
            match projection {
                Projection::Native => {
                    let include_path = if path.starts_with("generated/") {
                        path.strip_prefix("generated/")
                            .map(|path| format!("../{path}"))
                            .expect("prefix checked")
                    } else {
                        format!("../../{path}")
                    };
                    writeln!(
                        &mut source,
                        "{} const {}: &str = include_str!(\"{}\");",
                        RustVisibility::Crate.source(),
                        sql_constant_name(action, index, paths.len()),
                        include_path
                    )
                    .expect("writing to a String cannot fail");
                }
                Projection::Wamn => {
                    let digest = sha256(
                        sql_corpus
                            .get(path)
                            .expect("generated statement is present in the SQL corpus"),
                    );
                    writeln!(
                        &mut source,
                        "{} const {}: &str = {digest:?};",
                        wamn_api
                            .expect("Wamn projection carries its accessor API")
                            .statement_digest_visibility
                            .source(),
                        statement_digest_constant_name(action, index, paths.len()),
                    )
                    .expect("writing to a String cannot fail");
                }
            }
        }
    }
    source.push('\n');
    for fixture in native_bind_fixtures {
        emit_native_bind_fixture(&mut source, fixture);
    }
    if !native_bind_fixtures.is_empty() {
        source.push('\n');
    }
    if let Some(api) = wamn_api {
        if api
            .accessors
            .iter()
            .any(|accessor| accessor.operation == CrudAction::Create)
        {
            emit_claim_transaction(&mut source, &model_row.name);
        }
        for constraints in &api.mutation_constraints {
            emit_constraint_name_slice(&mut source, &constraints.unique);
            emit_constraint_name_slice(&mut source, &constraints.foreign_key);
            emit_constraint_name_slice(&mut source, &constraints.check);
            emit_constraint_name_slice(&mut source, &constraints.exclusion);
        }
        if !api.mutation_constraints.is_empty() {
            source.push('\n');
        }
        for accessor in &api.accessors {
            let row = if accessor.row == model_row.name {
                &model_row
            } else {
                operation_rows
                    .iter()
                    .find(|row| row.name == accessor.row)
                    .expect("operation accessor row was generated from the same operation")
            };
            emit_wamn_accessor(&mut source, accessor, row);
        }
    }
    while source.ends_with("\n\n") {
        source.pop();
    }
    let directory = match projection {
        Projection::Native => "native-verifier",
        Projection::Wamn => "wamn",
    };
    insert_bytes(
        files,
        &format!("generated/{directory}/{model_name}.rs"),
        source.into_bytes(),
    )
}

fn emit_native_bind_fixture(source: &mut String, fixture: &NativeBindFixture) {
    writeln!(
        source,
        "{} fn {}() -> {} {{\n    {}\n}}",
        fixture.visibility.source(),
        fixture.function,
        fixture.rust_type,
        fixture.value
    )
    .expect("writing to a String cannot fail");
}

fn emit_constraint_name_slice(source: &mut String, constraints: &ConstraintNameSlice) {
    let names = constraints
        .names
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        source,
        "{} const {}: &[&str] = &[{}];",
        constraints.visibility.source(),
        constraints.constant,
        names
    )
    .expect("writing to a String cannot fail");
}

fn emit_rust_row(source: &mut String, row: &RustRow, projection: Projection) {
    match projection {
        Projection::Native => source.push_str("#[derive(Debug, sqlx::FromRow)]\n"),
        Projection::Wamn => source.push_str("#[derive(Debug)]\n"),
    }
    writeln!(source, "{} struct {} {{", row.visibility.source(), row.name)
        .expect("writing to a String cannot fail");
    for field in &row.fields {
        writeln!(source, "    pub {}: {},", field.name, field.rust_type)
            .expect("writing to a String cannot fail");
    }
    source.push_str("}\n\n");
}

fn emit_wamn_accessor(source: &mut String, accessor: &WamnAccessor, row: &RustRow) {
    let owns_claim = accessor.operation == CrudAction::Create;
    let finalizes_claim = owns_claim && accessor.name == CREATE_STATEMENT;
    writeln!(
        source,
        "{} async fn {}(",
        accessor.visibility.source(),
        accessor.name
    )
    .expect("writing to a String cannot fail");
    if finalizes_claim {
        source.push_str("    mut claim: PendingClaim,\n");
    } else if owns_claim {
        source.push_str("    claim: &mut PendingClaim,\n");
    } else {
        source.push_str("    connection: &mut Connection,\n");
    }
    for bind in &accessor.binds {
        writeln!(source, "    {}: {},", bind.parameter, bind.wamn_rust)
            .expect("writing to a String cannot fail");
    }
    writeln!(
        source,
        ") -> Result<{}, wamn_postgres_statements::StatementError> {{",
        if finalizes_claim {
            "FinalizedClaim".to_owned()
        } else {
            accessor_result_type(accessor)
        }
    )
    .expect("writing to a String cannot fail");
    writeln!(
        source,
        "    let rows = {}.run({}, vec![",
        if owns_claim {
            "claim.transaction"
        } else {
            "connection"
        },
        accessor.statement_digest_constant
    )
    .expect("writing to a String cannot fail");
    for bind in &accessor.binds {
        writeln!(
            source,
            "        wamn_postgres_statements::into_sql_value({}),",
            bind.parameter
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str("    ]).await?;\n");
    let decode_function = match accessor.fetch {
        AccessorFetch::Optional => "decode_optional",
        AccessorFetch::All => "decode_all",
        AccessorFetch::One => "decode_one",
    };
    emit_decode_result(
        source,
        row,
        decode_function,
        &accessor.statement_digest_constant,
        finalizes_claim,
    );
}

fn accessor_result_type(accessor: &WamnAccessor) -> String {
    match accessor.fetch {
        AccessorFetch::Optional => format!("Option<{}>", accessor.row),
        AccessorFetch::All => format!("Vec<{}>", accessor.row),
        AccessorFetch::One => accessor.row.clone(),
    }
}

fn emit_decode_result(
    source: &mut String,
    row: &RustRow,
    decode_function: &str,
    statement_digest_constant: &str,
    finalizes_claim: bool,
) {
    source.push_str(if finalizes_claim {
        "    let row = "
    } else {
        "    "
    });
    writeln!(
        source,
        "wamn_postgres_statements::{decode_function}({statement_digest_constant}, rows, |row| {{"
    )
    .expect("writing to a String cannot fail");
    writeln!(source, "        Ok({} {{", row.name).expect("writing to a String cannot fail");
    for field in &row.fields {
        writeln!(
            source,
            "            {}: row.decode({:?})?,",
            field.name, field.name
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str("        })\n    })");
    if finalizes_claim {
        source.push_str("?;\n    Ok(FinalizedClaim { transaction: claim.transaction, row })");
    }
    source.push_str("\n}\n\n");
}

fn sql_constant_name(action: &str, index: usize, path_count: usize) -> String {
    let suffix = if path_count == 1 {
        String::new()
    } else {
        format!("_{index}")
    };
    format!("{}{}_SQL", action.to_ascii_uppercase(), suffix)
}

fn statement_digest_constant_name(action: &str, index: usize, path_count: usize) -> String {
    let suffix = if path_count == 1 {
        String::new()
    } else {
        format!("_{index}")
    };
    format!("{}{}_DIGEST", action.to_ascii_uppercase(), suffix)
}

fn rust_type(column: &Column, projection: Projection) -> String {
    projected_rust_type(column.column_type(), projection, column.nullable())
}

fn optional_rust_type(column: &Column, projection: Projection) -> String {
    projected_rust_type(column.column_type(), projection, true)
}

fn projected_rust_type(ty: ColumnType, projection: Projection, optional: bool) -> String {
    let base = match (projection, ty) {
        (_, ColumnType::Boolean) => "bool",
        (_, ColumnType::Int32) => "i32",
        (_, ColumnType::Int64) => "i64",
        (_, ColumnType::Float64) => "f64",
        (_, ColumnType::Text) => "String",
        (_, ColumnType::Bytes) => "Vec<u8>",
        (Projection::Native, ColumnType::Numeric) => "rust_decimal::Decimal",
        (Projection::Native, ColumnType::Timestamptz) => "chrono::DateTime<chrono::Utc>",
        (Projection::Native, ColumnType::Json) => "serde_json::Value",
        (Projection::Native, ColumnType::Uuid) => "uuid::Uuid",
        (Projection::Wamn, ColumnType::Numeric) => "wamn_postgres_statements::Numeric",
        (Projection::Wamn, ColumnType::Timestamptz) => "wamn_postgres_statements::TimestampTz",
        (Projection::Wamn, ColumnType::Json) => "wamn_postgres_statements::Json",
        (Projection::Wamn, ColumnType::Uuid) => "wamn_postgres_statements::Uuid",
    };
    if optional {
        format!("Option<{base}>")
    } else {
        base.to_owned()
    }
}
