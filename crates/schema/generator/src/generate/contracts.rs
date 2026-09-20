//! Operation contracts, schema contracts, and their SQL output.

use super::rust::{
    emit_projection, emit_static_sql_projection, native_bind_fixtures, operation_result_rows,
    static_sql_accessors, static_sql_native_bind_fixtures, static_sql_rows, wamn_api,
};
use super::validation::resolve_claim;
use super::wit::{emit_custom_operation_wit, emit_model_wit};
use super::{
    AccessOperationErrorLiteral, BTreeMap, BTreeSet, CLAIM_COMMAND_COLUMN, CLAIM_KEY_COLUMN,
    CREATE_CLAIM_STATEMENT, CREATE_REPLAY_STATEMENT, CREATE_STATEMENT, CREATE_STATEMENTS,
    CURSOR_VERSION, CatalogIr, ColumnType, ConstraintKind, ContractFieldDeclaration, CrudAction,
    CustomOperationDeclaration, CustomOperationKind, CustomOperationResultDeclaration, DeleteMode,
    GenerateError, GenerateErrorKind, ModelDeclaration, OperationDeclaration,
    OperationErrorDetailDeclaration, PackageManifest, Projection, ProjectionContents,
    RequiredConstraint, RequiredField, RequiredSchemaContract, RequiredTable, ResultClass,
    StatementContract, StatementTransactionality, StatementValueContract, Table, Value, WamnApi,
    canonical_operation_identity, column, constraint_error, constraint_error_code,
    custom_artifact_stem, custom_operation_constraint_origin, insert_bytes, insert_json,
    insert_json_line, json, operation_constraints, operation_exclusions, query_variants, relation,
    rust_type_identifier, server_owned_fields, sha256, sql,
};
use wamn_record_history::HISTORY_COLUMNS;

#[expect(
    clippy::too_many_arguments,
    reason = "model emission owns the complete validated model and catalog context"
)]
pub(super) fn emit_model(
    files: &mut BTreeMap<String, Vec<u8>>,
    sql_corpus: &mut BTreeMap<String, Vec<u8>>,
    transactional: &StatementTransactionality,
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
) -> Result<(), GenerateError> {
    if model.operations.is_empty() {
        return Ok(());
    }

    let claim = model.operations.get(&CrudAction::Create).map(|operation| {
        resolve_claim(
            catalog,
            manifest,
            &format!("{model_name}.create"),
            model,
            table,
            operation,
        )
        .expect("create validation resolved the command claim")
    });

    let mut operation_sql = BTreeMap::<String, Vec<String>>::new();
    for (action, operation) in &model.operations {
        let paths = emit_operation_sql(
            files,
            sql_corpus,
            model_name,
            table,
            claim.as_ref(),
            *action,
            operation,
            model.delete_mode,
        )?;
        operation_sql.insert(action.as_str().to_owned(), paths);
    }

    let native_operation_rows =
        operation_result_rows(model_name, model, table, claim.as_ref(), Projection::Native);
    let wamn_api = wamn_api(
        catalog,
        model_name,
        model,
        table,
        claim.as_ref(),
        &operation_sql,
    );
    for (action, operation) in &model.operations {
        emit_operation_contracts(
            catalog,
            files,
            sql_corpus,
            transactional,
            manifest,
            model_name,
            model,
            table,
            claim.as_ref(),
            *action,
            operation,
            &wamn_api,
        )?;
    }
    emit_model_wit(files, catalog, manifest, model_name, model, table)?;
    let native_bind_fixtures = native_bind_fixtures(&wamn_api);
    emit_projection(
        files,
        sql_corpus,
        model_name,
        table,
        &operation_sql,
        ProjectionContents::Native {
            operation_rows: &native_operation_rows,
            bind_fixtures: &native_bind_fixtures,
        },
    )?;
    emit_projection(
        files,
        sql_corpus,
        model_name,
        table,
        &operation_sql,
        ProjectionContents::Wamn(&wamn_api),
    )?;
    insert_json(
        files,
        &format!("generated/source-map/{model_name}.json"),
        &json!({
            "model": model_name,
            "relation": format!("catalog-ir://{}.{}", model.schema, model.table),
            "manifest": format!("wamn.json#/models/{model_name}"),
            "operations": operation_sql,
            "native_operation_rows": native_operation_rows,
            "native_bind_fixtures": native_bind_fixtures,
            "wamn_api": wamn_api,
        }),
    )
}

pub(super) fn emit_custom_operation(
    files: &mut BTreeMap<String, Vec<u8>>,
    sql_corpus: &BTreeMap<String, Vec<u8>>,
    transactional: &StatementTransactionality,
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    emit_custom_operation_contracts(
        files,
        sql_corpus,
        transactional,
        catalog,
        manifest,
        operation_name,
        operation,
    )?;
    if operation.statements.is_empty() {
        return Ok(());
    }

    let module_name = custom_artifact_stem(operation_name);
    let native_rows = static_sql_rows(operation, Projection::Native);
    let wamn_rows = static_sql_rows(operation, Projection::Wamn);
    let accessors = static_sql_accessors(operation);
    let bind_fixtures = static_sql_native_bind_fixtures(&accessors);

    emit_static_sql_projection(
        files,
        sql_corpus,
        &module_name,
        operation,
        &native_rows,
        &bind_fixtures,
        Projection::Native,
    )?;
    emit_static_sql_projection(
        files,
        sql_corpus,
        &module_name,
        operation,
        &wamn_rows,
        &[],
        Projection::Wamn,
    )?;
    let mut source_map = serde_json::Map::from_iter([
        (
            "manifest".to_owned(),
            json!(format!("wamn.json#/custom_operations/{operation_name}")),
        ),
        ("relations".to_owned(), json!(operation.relations)),
        ("statements".to_owned(), json!(operation.statements)),
        ("native_rows".to_owned(), json!(native_rows)),
        ("native_bind_fixtures".to_owned(), json!(bind_fixtures)),
        ("wamn_rows".to_owned(), json!(wamn_rows)),
        ("wamn_accessors".to_owned(), json!(accessors)),
    ]);
    if let Some((alias, dependency)) = operation_dependency(manifest, operation_name) {
        source_map.insert(
            "composition".to_owned(),
            json!({
                "alias": alias,
                "package": dependency.package,
                "version": dependency.version,
                "digest": dependency.digest,
                "operation": operation_name,
            }),
        );
    }
    match operation.kind {
        CustomOperationKind::Command => {
            source_map.insert("command".to_owned(), json!(operation_name));
        }
        CustomOperationKind::Projection | CustomOperationKind::EventHandler => {
            source_map.insert("operation".to_owned(), json!(operation_name));
            source_map.insert("kind".to_owned(), json!(operation.kind()));
            if let Some(registration) = &operation.registration {
                source_map.insert("registration".to_owned(), json!(registration));
            }
        }
    }
    insert_json(
        files,
        &format!("generated/source-map/{module_name}.json"),
        &Value::Object(source_map),
    )
}

fn emit_custom_operation_contracts(
    files: &mut BTreeMap<String, Vec<u8>>,
    sql_corpus: &BTreeMap<String, Vec<u8>>,
    transactional: &StatementTransactionality,
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    let (module, local_name) = operation_name
        .split_once('.')
        .expect("custom-operation validation requires module.operation");
    let root = format!("generated/contracts/{module}/{local_name}");
    let operation_id = canonical_operation_identity(&manifest.package, operation_name)?;
    let grant = (operation.visibility == crate::manifest::OperationVisibility::Public)
        .then(|| operation_id.clone());
    let statements =
        operation
            .statements
            .iter()
            .map(|(name, statement)| {
                statement_contract(
                    name,
                    &statement.path,
                    sql_corpus,
                    transactional,
                    statement.parameters.iter().map(|value| {
                        statement_value_contract(&value.name, value.ty, value.nullable)
                    }),
                    statement.row.iter().map(|value| {
                        statement_value_contract(&value.name, value.ty, value.nullable)
                    }),
                )
            })
            .collect::<Vec<_>>();
    let mut operation_contract = serde_json::Map::from_iter([
        ("operation".to_owned(), json!(operation_id)),
        ("kind".to_owned(), json!(operation.kind())),
        ("visibility".to_owned(), json!(operation.visibility)),
        ("permission_token".to_owned(), json!(operation.permission)),
        ("grant".to_owned(), json!(grant)),
        ("statements".to_owned(), json!(statements)),
    ]);
    if operation.fresh_only {
        operation_contract.insert("fresh_only".to_owned(), json!(true));
    }
    if let Some((alias, dependency)) = operation_dependency(manifest, operation_name) {
        let mut dependency_contract = serde_json::Map::from_iter([
            ("alias".to_owned(), json!(alias)),
            ("package".to_owned(), json!(dependency.package)),
            ("version".to_owned(), json!(dependency.version)),
            ("digest".to_owned(), json!(dependency.digest)),
            ("operation".to_owned(), json!(operation_name)),
        ]);
        if let Some(participant) = &operation.participant {
            dependency_contract.insert(
                "participant".to_owned(),
                json!(canonical_operation_identity(
                    &manifest.package,
                    participant
                )?),
            );
        }
        operation_contract.insert("dependency".to_owned(), Value::Object(dependency_contract));
    }
    if operation.pre_commit.is_some() {
        let (prefix, version) = operation_id
            .rsplit_once('@')
            .expect("canonical operation identity has a version");
        operation_contract.insert(
            "pre_commit".to_owned(),
            json!(format!("{prefix}-pre-commit@{version}")),
        );
    }
    if let Some(connection) = &operation.connection {
        operation_contract.insert("connection".to_owned(), json!(connection));
    }
    if let Some(result) = &operation.result {
        operation_contract.insert("result".to_owned(), json!(result.class));
    }
    if let Some(transaction) = operation.transaction {
        operation_contract.insert("transaction".to_owned(), json!(transaction));
    }
    if let Some(automatic_retry) = operation.automatic_retry {
        operation_contract.insert("automatic_retry".to_owned(), json!(automatic_retry));
    }
    if let Some(registration) = &operation.registration {
        operation_contract.insert("registration".to_owned(), json!(registration));
    }
    if let Some(idempotent_by) = &operation.idempotent_by {
        operation_contract.insert("idempotent_by".to_owned(), json!(idempotent_by));
    }
    if let Some(claim) = &operation.claim {
        operation_contract.insert("claim".to_owned(), json!(claim));
    }
    if !operation.relations.is_empty() {
        operation_contract.insert("relations".to_owned(), json!(operation.relations));
    }
    insert_json_line(
        files,
        &format!("{root}.operation.json"),
        &Value::Object(operation_contract),
    )?;
    let mut input_contract = serde_json::Map::new();
    if let Some(raw_body_maximum) = operation.input.raw_body_maximum {
        input_contract.insert(
            "raw_body_bytes".to_owned(),
            json!({
                "maximum": raw_body_maximum,
                "owner": "ingress_pre_parse",
                "refusal": "http_413",
            }),
        );
    }
    if let Some(envelope) = &operation.input.envelope {
        input_contract.insert("envelope".to_owned(), count_contract(envelope));
        input_contract.insert("item_semantics".to_owned(), json!("per_input"));
    }
    if let Some(line) = &operation.input.line {
        input_contract.insert("line".to_owned(), count_contract(line));
    }
    input_contract.insert("fields".to_owned(), json!(operation.input.fields));
    if let Some(canonicalization) = &operation.canonicalization {
        let mut contract = json!({
            "payload": "canonical_compact_json",
            "excluded_fields": canonicalization.excluded_fields,
            "uuid": "lowercase_hyphenated",
            "timestamptz": "utc_rfc3339_six_fractional_digits",
            "numeric": "postgresql_lexical_scale_preserved",
        });
        if let Some(order) = canonicalization.line_order {
            contract["line_order"] = json!(order);
            contract["duplicate_line"] = json!("invalid_input");
        }
        input_contract.insert("canonicalization".to_owned(), contract);
    }
    insert_json(
        files,
        &format!("{root}.input.json"),
        &Value::Object(input_contract),
    )?;
    if let Some(result) = &operation.result {
        insert_json(files, &format!("{root}.result.json"), result)?;
    }
    insert_json(
        files,
        &format!("{root}.errors.json"),
        &custom_operation_error_contract(catalog, operation),
    )?;
    emit_custom_operation_wit(files, manifest, operation_name, operation)?;
    Ok(())
}

fn statement_contract(
    name: &str,
    path: &str,
    sql_corpus: &BTreeMap<String, Vec<u8>>,
    transactional: &StatementTransactionality,
    binds: impl IntoIterator<Item = StatementValueContract>,
    columns: impl IntoIterator<Item = StatementValueContract>,
) -> StatementContract {
    let bytes = sql_corpus
        .get(path)
        .expect("statement path was emitted or supplied by the validated corpus");
    StatementContract {
        name: name.to_owned(),
        path: path.to_owned(),
        digest: sha256(bytes),
        binds: binds.into_iter().collect(),
        columns: columns.into_iter().collect(),
        transactional: transactional.needs_transaction(path),
    }
}

fn operation_row_columns(wamn_api: &WamnApi, row: &str) -> Vec<StatementValueContract> {
    wamn_api
        .operation_rows
        .iter()
        .find(|candidate| candidate.name == row)
        .expect("operation accessor row was emitted from the same operation")
        .fields
        .iter()
        .map(|field| statement_value_contract(&field.name, field.statement_type, field.nullable))
        .collect()
}

/// What a consumer must know to replay one generated create.
///
/// The `identities` map is the law made checkable downstream: it names the
/// claim column that pre-generated each model identity, so a reader can verify
/// that a replay returns the same ids WITHOUT reading the orchestration.
fn idempotency_contract(claim: &sql::Claim<'_>) -> Value {
    json!({
        "key": CLAIM_KEY_COLUMN,
        "canonical_command": CLAIM_COMMAND_COLUMN,
        "claim": {
            "schema": claim.table.schema(),
            "table": claim.table.name(),
            "constraint": claim.primary_key,
            "identities": claim
                .identities
                .iter()
                .map(|(field, claim_column)| ((*field).to_owned(), *claim_column))
                .collect::<BTreeMap<_, _>>(),
        },
        "statements": {
            "claim": CREATE_CLAIM_STATEMENT,
            "replay": CREATE_REPLAY_STATEMENT,
            "insert": CREATE_STATEMENT,
        },
        "replay": {"writes": "none", "identity_source": "claim"},
        "conflict": {
            "on": "changed_canonical_command",
            "refusal": AccessOperationErrorLiteral::IdempotencyConflict,
        },
        "atomicity": "claim_and_insert_commit_together",
    })
}

fn statement_value_contract(name: &str, ty: ColumnType, nullable: bool) -> StatementValueContract {
    StatementValueContract {
        name: name.to_owned(),
        ty,
        nullable,
    }
}

fn operation_dependency<'a>(
    manifest: &'a PackageManifest,
    operation_name: &str,
) -> Option<(&'a str, &'a crate::manifest::BaseDependencyRequirement)> {
    manifest
        .base_dependencies
        .iter()
        .find_map(|(alias, dependency)| {
            dependency
                .operations
                .iter()
                .any(|candidate| candidate == operation_name)
                .then_some((alias.as_str(), dependency))
        })
}

fn custom_operation_error_contract(
    catalog: &CatalogIr,
    operation: &CustomOperationDeclaration,
) -> Value {
    let cases = operation
        .errors
        .iter()
        .map(|literal| {
            let constraint = operation
                .constraint_errors
                .iter()
                .find_map(|(constraint, mapped)| (mapped == literal).then_some(constraint));
            let mut case = if let Some(constraint) = constraint {
                let origin = custom_operation_constraint_origin(catalog, operation, constraint)
                    .expect("custom constraint mapping was validated");
                json!({
                    "literal": literal,
                    "from": origin,
                    "constraint": constraint,
                })
            } else {
                custom_operation_error_origin(operation, literal)
            };
            case.as_object_mut()
                .expect("error contract case is an object")
                .insert(
                    "detail".to_owned(),
                    error_detail_contract(&crate::manifest::custom_operation_error_detail(
                        operation, literal,
                    )),
                );
            case
        })
        .collect::<Vec<_>>();
    json!({"closed": true, "cases": cases})
}

fn custom_operation_error_origin(operation: &CustomOperationDeclaration, literal: &str) -> Value {
    match literal {
        "invalid_input" => {
            let mut sources = vec!["malformed_input"];
            if operation.input.envelope.is_some() {
                sources.push("envelope_count");
            }
            if operation.input.line.is_some() {
                sources.push("line_count");
            }
            // Only a command WITH lines can refuse a duplicate one.
            if operation
                .canonicalization
                .as_ref()
                .is_some_and(|canonical| canonical.line_order.is_some())
            {
                sources.push("duplicate_line");
            }
            if operation.input.line.is_some() && operation.canonicalization.is_some() {
                sources.push("nonpositive_quantity");
            }
            json!({"literal": literal, "from": sources})
        }
        "retry" => json!({
            "literal": literal,
            "from": ["serialization_failure", "connection_unavailable"],
            "automatic": operation.automatic_retry.unwrap_or(false),
        }),
        "timeout" => json!({
            "literal": literal,
            "from": "statement_timeout",
        }),
        "permission_denied" => json!({
            "literal": literal,
            "from": "permission_denied",
        }),
        "internal_error" => json!({
            "literal": literal,
            "from": ["query_error", "row_limit_exceeded", "undeclared_constraint"],
        }),
        "idempotency_conflict" if operation.canonicalization.is_some() => json!({
            "literal": literal,
            "from": "same_key_different_canonical_command",
        }),
        _ => json!({
        "literal": literal,
        "from": "transaction_invariant",
        }),
    }
}

fn error_detail_contract(detail: &OperationErrorDetailDeclaration) -> Value {
    let mut contract = serde_json::Map::new();
    if !detail.required.is_empty() {
        contract.insert("required".to_owned(), json!(detail.required));
    }
    if !detail.optional.is_empty() {
        contract.insert("optional".to_owned(), json!(detail.optional));
    }
    Value::Object(contract)
}

pub(super) fn emit_cursor_contract(
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), GenerateError> {
    insert_json(
        files,
        "generated/contracts/cursor-v1.json",
        &json!({
            "version": CURSOR_VERSION,
            "payload": "canonical_compact_json",
            "member_order": ["direction", "field", "id", "key", "v"],
            "key": "bare_value_typed_by_manifest_ir",
            "timestamptz": "utc_rfc3339_six_fractional_digits",
            "numeric": "postgresql_lexical_scale_preserved",
            "encoding": "base64url_unpadded",
            "invalid": [
                "decode_failure",
                "unknown_version",
                "field_mismatch",
                "direction_mismatch",
                "key_parse_failure",
                "noncanonical_payload",
            ],
            "refusal": "invalid_input",
            "fallback_to_first_page": false,
        }),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "statement generation owns this complete validated context"
)]
fn emit_operation_sql(
    files: &mut BTreeMap<String, Vec<u8>>,
    sql_corpus: &mut BTreeMap<String, Vec<u8>>,
    model_name: &str,
    table: &Table,
    claim: Option<&sql::Claim<'_>>,
    action: CrudAction,
    operation: &OperationDeclaration,
    delete_mode: Option<DeleteMode>,
) -> Result<Vec<String>, GenerateError> {
    let tombstoned = delete_mode == Some(DeleteMode::Tombstone);
    if action == CrudAction::Create {
        let claim = claim.expect("create validation resolved the command claim");
        let mut paths = Vec::with_capacity(CREATE_STATEMENTS.len());
        for (name, sql) in [
            (CREATE_CLAIM_STATEMENT, sql::create_claim(claim)),
            (CREATE_REPLAY_STATEMENT, sql::create_replay(table, claim)),
            (CREATE_STATEMENT, sql::create(table, claim, operation)),
        ] {
            let path = format!("generated/sql/{model_name}/{name}.sql");
            let bytes = sql.into_bytes();
            insert_bytes(files, &path, bytes.clone())?;
            if sql_corpus.insert(path.clone(), bytes).is_some() {
                return Err(GenerateError::for_path(
                    GenerateErrorKind::DuplicatePath,
                    "generated SQL collides with the corpus",
                    path,
                ));
            }
            paths.push(path);
        }
        return Ok(paths);
    }
    if action == CrudAction::Query {
        if let Some(authored) = &operation.authored_sql {
            return Ok(authored
                .variants
                .iter()
                .map(|variant| variant.path.clone())
                .collect());
        }
        let variants = query_variants(operation);
        let mut paths = Vec::with_capacity(variants.len());
        for (field, direction) in variants {
            let path = format!(
                "generated/sql/{model_name}/query_{field}_{}.sql",
                sql::direction_name(direction)
            );
            let bytes = sql::query(table, operation, field, direction, tombstoned).into_bytes();
            insert_bytes(files, &path, bytes.clone())?;
            if sql_corpus.insert(path.clone(), bytes).is_some() {
                return Err(GenerateError::for_path(
                    GenerateErrorKind::DuplicatePath,
                    "generated SQL collides with the corpus",
                    path,
                ));
            }
            paths.push(path);
        }
        return Ok(paths);
    }

    let sql = match action {
        CrudAction::Get => sql::get(table, tombstoned),
        CrudAction::Update => sql::update(table, operation, tombstoned),
        CrudAction::Delete => sql::delete(
            table,
            operation,
            delete_mode.expect("delete validation requires a declared mode"),
        ),
        CrudAction::Create | CrudAction::Query => {
            unreachable!("create and query returned above")
        }
    };
    let path = format!("generated/sql/{model_name}/{}.sql", action.as_str());
    let bytes = sql.into_bytes();
    insert_bytes(files, &path, bytes.clone())?;
    if sql_corpus.insert(path.clone(), bytes).is_some() {
        return Err(GenerateError::for_path(
            GenerateErrorKind::DuplicatePath,
            "generated SQL collides with the corpus",
            path,
        ));
    }
    Ok(vec![path])
}

#[expect(
    clippy::too_many_arguments,
    reason = "operation contract generation owns this complete validated context"
)]
fn emit_operation_contracts(
    catalog: &CatalogIr,
    files: &mut BTreeMap<String, Vec<u8>>,
    sql_corpus: &BTreeMap<String, Vec<u8>>,
    transactional: &StatementTransactionality,
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    claim: Option<&sql::Claim<'_>>,
    action: CrudAction,
    operation: &OperationDeclaration,
    wamn_api: &WamnApi,
) -> Result<(), GenerateError> {
    let operation_id = canonical_operation_identity(
        &manifest.package,
        &format!("{model_name}.{}", action.as_str()),
    )?;
    let root = format!("generated/contracts/{model_name}/{}", action.as_str());
    let accessors = wamn_api
        .accessors
        .iter()
        .filter(|accessor| accessor.operation == action)
        .collect::<Vec<_>>();
    let model_columns = table
        .columns()
        .iter()
        .map(|column| {
            statement_value_contract(column.name(), column.column_type(), column.nullable())
        })
        .collect::<Vec<_>>();
    // A public create or update succeeds with the model row. The update
    // statement's outcome and observed revision remain SQL accessor columns.
    let columns = if action == CrudAction::Delete {
        accessors.first().map_or_else(Vec::new, |accessor| {
            operation_row_columns(wamn_api, &accessor.row)
        })
    } else {
        model_columns.clone()
    };
    let statements = accessors
        .iter()
        .map(|accessor| {
            let statement_columns =
                if accessor.row == format!("{}Row", rust_type_identifier(model_name)) {
                    model_columns.clone()
                } else {
                    operation_row_columns(wamn_api, &accessor.row)
                };
            statement_contract(
                &accessor.name,
                &accessor.sql_path,
                sql_corpus,
                transactional,
                accessor.binds.iter().map(|bind| {
                    statement_value_contract(&bind.parameter, bind.statement_type, bind.nullable)
                }),
                statement_columns,
            )
        })
        .collect::<Vec<_>>();
    let mut record = serde_json::Map::from_iter([
        (
            "relation".to_owned(),
            json!(format!("{}.{}", model.schema, model.table)),
        ),
        ("key_field".to_owned(), json!("id")),
    ]);
    if matches!(
        action,
        CrudAction::Get | CrudAction::Update | CrudAction::Delete
    ) {
        record.insert("key_input".to_owned(), json!("id"));
    }
    if let Some(revision) = operation.revision_field.as_deref() {
        record.insert("revision_field".to_owned(), json!(revision));
    }
    if matches!(action, CrudAction::Update | CrudAction::Delete) {
        let revision = operation
            .revision_field
            .as_deref()
            .expect("mutation validation requires a revision field");
        record.insert(
            "revision_input".to_owned(),
            json!(format!("expected_{revision}")),
        );
    }
    let mut operation_contract = serde_json::Map::from_iter([
        ("operation".to_owned(), json!(operation_id)),
        ("kind".to_owned(), json!(action.as_str())),
        ("record".to_owned(), Value::Object(record)),
        ("permission_token".to_owned(), json!(operation.permission)),
        ("grant".to_owned(), json!(operation_id)),
        ("result".to_owned(), json!(operation.result)),
        ("statements".to_owned(), json!(statements)),
        (
            "transaction".to_owned(),
            json!(if action == CrudAction::Create {
                "explicit_per_input"
            } else {
                "implicit"
            }),
        ),
        ("automatic_retry".to_owned(), json!(false)),
    ]);
    if operation.fresh_only {
        operation_contract.insert("fresh_only".to_owned(), json!(true));
    }
    if let Some(claim) = claim.filter(|_| action == CrudAction::Create) {
        operation_contract.insert("idempotent_by".to_owned(), json!("claim"));
        operation_contract.insert("idempotency".to_owned(), idempotency_contract(claim));
    }
    insert_json_line(
        files,
        &format!("{root}.operation.json"),
        &Value::Object(operation_contract),
    )?;
    insert_json(
        files,
        &format!("{root}.input.json"),
        &input_contract(model, table, action, operation),
    )?;
    insert_json(
        files,
        &format!("{root}.result.json"),
        &crud_result_contract(model, action, operation.result, &columns),
    )?;
    insert_json(
        files,
        &format!("{root}.errors.json"),
        &error_contract(catalog, table, action, operation, model.delete_mode),
    )?;
    Ok(())
}

/// The generated CRUD result contract, in the shape a custom operation
/// declares.
///
/// A result CLASS in the operation contract says how many rows come back, not
/// what is in them. Without this the only surviving description of a generated
/// operation's result is its statement columns — name, type, nullable and
/// nothing else — so a closed value domain the model declares never reaches a
/// client, and a control renders a free-text box where a choice belongs.
///
/// Model fields use their declared domains. A successful delete exposes only
/// the SQL-owned deleted outcome; other outcomes become typed refusals.
fn crud_result_contract(
    model: &ModelDeclaration,
    action: CrudAction,
    class: ResultClass,
    columns: &[StatementValueContract],
) -> CustomOperationResultDeclaration {
    CustomOperationResultDeclaration {
        class,
        fields: columns
            .iter()
            .map(|column| ContractFieldDeclaration {
                path: column.name.clone(),
                ty: column.ty,
                nullable: column.nullable,
                revision: model.operations.values().any(|operation| {
                    operation.revision_field.as_deref() == Some(column.name.as_str())
                }),
                values: if action == CrudAction::Delete && column.name == "outcome" {
                    vec![sql::OUTCOME_DELETED.to_owned()]
                } else {
                    model
                        .enum_fields
                        .get(&column.name)
                        .cloned()
                        .unwrap_or_default()
                },
            })
            .collect(),
    }
}

fn input_contract(
    model: &ModelDeclaration,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Value {
    let writable = operation
        .writable_fields
        .iter()
        .map(|field| {
            let column = column(table, field).expect("validation resolved writable fields");
            json!({
                "field": field,
                "path": if action == CrudAction::Update { format!("change.{field}") } else { field.clone() },
                "type": column.column_type().as_str(),
                "omitted": if action == CrudAction::Update { "unchanged" } else { "postgres_default" },
                "explicit_null": if column.nullable() { "accepted" } else { "invalid_input" },
            })
        })
        .collect::<Vec<_>>();
    let common = json!({
        "request_id": {"type": "string", "required": true},
        "server_owned_fields": {
            "fields": server_owned_fields(model, table),
            "if_supplied": "invalid_input",
        },
        "writable_fields": writable,
    });
    match action {
        CrudAction::Get => merge_json(common, &json!({"id": {"type": "uuid", "required": true}})),
        CrudAction::Query => merge_json(
            common,
            &json!({
                "filters": operation.filters.iter().map(|filter| {
                    let column = column(table, &filter.field).expect("validated filter column");
                    json!({
                        "field": filter.field,
                        "binding": "json_array",
                        "type": column.column_type().as_str(),
                    })
                }).collect::<Vec<_>>(),
                "sort": operation.sort.as_ref().map(|sort| json!({
                    "fields": sort.fields,
                    "directions": sort.directions,
                    "max_fields": 1,
                })),
                "pagination": operation.pagination.as_ref().map(|pagination| json!({
                    "kind": "keyset",
                    "cursor": {
                        "version": CURSOR_VERSION,
                        "payload": "canonical_compact_json",
                        "encoding": "base64url_unpadded",
                        "opaque": true,
                        "invalid": "invalid_input",
                    },
                    "default_sort": pagination.default_sort,
                    "tie_breaker": pagination.tie_breaker,
                })),
                "limit": operation.limit.as_ref().map(|limit| json!({
                    "default": limit.default,
                    "minimum": limit.minimum,
                    "maximum": limit.maximum,
                    "invalid": "invalid_input",
                })),
                "validation_order": ["cursor", "limit", "sql"],
                "invalid_cursor": "invalid_input",
                "invalid_limit": "invalid_input",
            }),
        ),
        CrudAction::Create => merge_json(
            common,
            &json!({
                CLAIM_KEY_COLUMN: {"type": "text", "required": true},
                CLAIM_COMMAND_COLUMN: {
                    "over": "writable_fields",
                    "payload": "canonical_compact_json",
                    "changed": "idempotency_conflict",
                },
            }),
        ),
        CrudAction::Update | CrudAction::Delete => {
            let revision = operation
                .revision_field
                .as_deref()
                .expect("mutation validation requires a revision field");
            let mut mutation = json!({
                "id": {"type": "uuid", "required": true},
            });
            mutation
                .as_object_mut()
                .expect("mutation input contract is an object")
                .insert(
                    format!("expected_{revision}"),
                    json!({
                        "field": revision,
                        "type": "int64",
                        "required": true,
                        "revision": true,
                    }),
                );
            merge_json(common, &mutation)
        }
    }
}

fn error_contract(
    catalog: &CatalogIr,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
    delete_mode: Option<DeleteMode>,
) -> Value {
    use AccessOperationErrorLiteral as Code;

    let mut cases = vec![
        (Code::InvalidInput, json!({"literal": "invalid_input"})),
        (
            Code::PermissionDenied,
            json!({"literal": "permission_denied", "from": "permission_denied"}),
        ),
        (
            Code::Retry,
            json!({
                "literal": "retry",
                "from": ["serialization_failure", "connection_unavailable"],
                "automatic": false,
            }),
        ),
        (
            Code::Timeout,
            json!({"literal": "timeout", "from": "statement_timeout"}),
        ),
        (
            Code::InternalError,
            json!({
                "literal": "internal_error",
                "from": ["query_error", "row_limit_exceeded"],
            }),
        ),
    ];
    if matches!(
        action,
        CrudAction::Get | CrudAction::Update | CrudAction::Delete
    ) {
        cases.push((Code::NotFound, json!({"literal": "not_found"})));
    }
    if matches!(action, CrudAction::Update | CrudAction::Delete) {
        cases.push((
            Code::ConcurrencyConflict,
            json!({"literal": "concurrency_conflict"}),
        ));
    }
    if action == CrudAction::Create {
        cases.push((
            Code::IdempotencyConflict,
            json!({
                "literal": "idempotency_conflict",
                "from": "changed_canonical_command",
            }),
        ));
    }
    for constraint in operation_constraints(catalog, table, action, operation, delete_mode) {
        let code = constraint_error_code(constraint.kind());
        cases.push((
            code,
            json!({
                "literal": constraint_error(constraint.kind()),
                "from": constraint_error(constraint.kind()),
                "constraint": constraint.name(),
            }),
        ));
    }
    for exclusion in operation_exclusions(table, action, operation) {
        cases.push((
            Code::ExclusionViolation,
            json!({
                "literal": "exclusion_violation",
                "from": "exclusion_violation",
                "constraint": exclusion.name(),
            }),
        ));
    }
    let cases = cases
        .into_iter()
        .map(|(code, mut case)| {
            case.as_object_mut()
                .expect("error contract case is an object")
                .insert(
                    "detail".to_owned(),
                    error_detail_contract(&crate::manifest::access_operation_error_detail(
                        action, code,
                    )),
                );
            case
        })
        .collect::<Vec<_>>();
    json!({"closed": true, "cases": cases})
}

pub(super) fn required_schema_contract(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
) -> RequiredSchemaContract {
    // A history table stays out of the catalog, so its entry carries no table.
    let mut consumed =
        BTreeMap::<(String, String), (Option<&Table>, BTreeSet<String>, BTreeSet<String>)>::new();
    for model in manifest.models.values() {
        let table = relation(catalog, model).expect("validation resolved relation");
        let entry = consumed
            .entry((model.schema.clone(), model.table.clone()))
            .or_insert_with(|| (Some(table), BTreeSet::new(), BTreeSet::new()));
        entry.1.extend(
            table
                .columns()
                .iter()
                .map(|column| column.name().to_owned()),
        );
        entry.2.extend(
            table
                .constraints()
                .iter()
                .map(|constraint| constraint.name().to_owned()),
        );
    }
    // A create's generated SQL names the claim's primary-key constraint and
    // reads back the columns that minted its identity, so the required-schema
    // contract has to pin them. The internal-relation pass below registers the
    // claim relation with no fields at all.
    for model in manifest.models.values() {
        let Some(claim) = model
            .operations
            .get(&CrudAction::Create)
            .and_then(|operation| operation.claim.as_ref())
        else {
            continue;
        };
        let table = catalog
            .tables()
            .iter()
            .find(|table| table.schema() == model.schema && table.name() == claim.table)
            .expect("create validation resolved the claim relation");
        let entry = consumed
            .entry((model.schema.clone(), claim.table.clone()))
            .or_insert_with(|| (Some(table), BTreeSet::new(), BTreeSet::new()));
        entry.1.insert(CLAIM_KEY_COLUMN.to_owned());
        entry.1.insert(CLAIM_COMMAND_COLUMN.to_owned());
        entry.1.extend(claim.identities.values().cloned());
        entry.2.extend(
            table
                .constraints()
                .iter()
                .filter(|constraint| match constraint.kind() {
                    ConstraintKind::PrimaryKey { columns } => columns
                        .iter()
                        .any(|column| column.as_ref() == CLAIM_KEY_COLUMN),
                    ConstraintKind::Unique { columns } => columns.iter().any(|column| {
                        claim
                            .identities
                            .values()
                            .any(|value| value == column.as_ref())
                    }),
                    ConstraintKind::ForeignKey { .. } | ConstraintKind::Check { .. } => false,
                })
                .map(|constraint| constraint.name().to_owned()),
        );
    }
    for relation in manifest.internal_relations.values() {
        let table = catalog
            .tables()
            .iter()
            .find(|table| table.schema() == relation.schema && table.name() == relation.table)
            .expect("internal-relation validation resolved relation");
        consumed
            .entry((relation.schema.clone(), relation.table.clone()))
            .or_insert_with(|| (Some(table), BTreeSet::new(), BTreeSet::new()));
    }
    for operation in manifest.custom_operations.values() {
        for relation in &operation.relations {
            let table = catalog
                .tables()
                .iter()
                .find(|table| table.schema() == relation.schema && table.name() == relation.table);
            let entry = consumed
                .entry((relation.schema.clone(), relation.table.clone()))
                .or_insert_with(|| (table, BTreeSet::new(), BTreeSet::new()));
            entry.1.extend(
                relation
                    .select_fields
                    .iter()
                    .chain(&relation.insert_fields)
                    .chain(&relation.update_fields)
                    .cloned(),
            );
            entry.2.extend(relation.constraints.iter().cloned());
        }
    }
    let tables = consumed
        .into_iter()
        .map(|((schema, name), (table, fields, constraints))| {
            let Some(table) = table else {
                // The fixed history columns in name order, as the catalog orders
                // columns. Every history column is NOT NULL.
                return RequiredTable {
                    schema: schema.into(),
                    table: name.into(),
                    fields: BTreeMap::from(HISTORY_COLUMNS)
                        .into_iter()
                        .filter(|(column, _)| fields.contains(*column))
                        .map(|(column, ty)| RequiredField {
                            name: column.into(),
                            ty: ty.into(),
                            nullable: false,
                        })
                        .collect(),
                    constraints: Box::default(),
                };
            };
            RequiredTable {
                schema: table.schema().into(),
                table: table.name().into(),
                fields: table
                    .columns()
                    .iter()
                    .filter(|column| fields.contains(column.name()))
                    .map(|column| RequiredField {
                        name: column.name().into(),
                        ty: column.column_type().as_str().into(),
                        nullable: column.nullable(),
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                constraints: table
                    .constraints()
                    .iter()
                    .filter(|constraint| constraints.contains(constraint.name()))
                    .map(|constraint| RequiredConstraint {
                        name: constraint.name().into(),
                        definition: serde_json::to_value(constraint.kind())
                            .expect("constraint IR always serializes"),
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            }
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    RequiredSchemaContract { tables }
}

fn merge_json(mut left: Value, right: &Value) -> Value {
    let left = left
        .as_object_mut()
        .expect("input contracts are JSON objects");
    left.extend(
        right
            .as_object()
            .expect("input contract additions are JSON objects")
            .clone(),
    );
    Value::Object(left.clone())
}

fn count_contract(limit: &crate::CountLimitDeclaration) -> Value {
    json!({
        "minimum": limit.minimum,
        "maximum": limit.maximum,
        "invalid": "invalid_input",
    })
}
