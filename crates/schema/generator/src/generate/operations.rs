//! Operation contracts, schema contracts, and their SQL output.

use super::rust::{emit_projection, emit_static_sql_projection, static_sql_rows, wamn_api};
use super::wit::{emit_custom_operation_wit, emit_model_wit};
use super::{
    BTreeMap, BTreeSet, CHANGES_STATEMENT, CREATE_KEY_FIELD, CREATE_STATEMENT, CURSOR_VERSION,
    CatalogIr, ConstraintType, RequiredConstraint, RequiredField, RequiredSchemaContract,
    RequiredTable, StatementContract, StatementTransactionality, StatementValueContract, Table,
    Value, WamnApi, column, constraint_error_code, constraint_field,
    custom_operation_constraint_origin, json, operation_constraints, operation_exclusions,
    query_variants, relation, server_owned_fields, sql, writable_path,
};
use crate::manifest::{
    ContractFieldDeclaration, CrudAction, CustomOperationDeclaration,
    CustomOperationResultDeclaration, DeleteMode, FieldText, ModelDeclaration,
    OperationDeclaration, OperationErrorDetailDeclaration, PackageManifest, ResultClass,
    canonical_operation_identity, custom_artifact_stem, rust_type_identifier,
};
use crate::manifest::{
    FieldReference, OperationErrorDetailKey, StaticSqlRelationDeclaration, TombstoneColumn,
    authored_name,
};
use crate::{ManifestError as GenerateError, ManifestErrorType as GenerateErrorType};
use wamn_record_history::HISTORY_COLUMNS;
use wamn_scalar::SqlValueType;

/// Request, result and refusal facts compiled once for a model action.
#[derive(Debug)]
pub(super) struct ModelOperation<'a> {
    pub request: &'a OperationDeclaration,
    pub result: CustomOperationResultDeclaration,
    pub refusals:
        BTreeMap<crate::manifest::AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
    pub revision: Option<SqlValueType>,
    pub guarded_fields: BTreeMap<String, String>,
}

/// Physical request field facts shared by the guest renderers.
#[derive(Debug)]
pub(super) struct ModelShape<'a> {
    pub columns: &'a [super::Column],
    pub key: crate::key::KeyType,
    pub generated_id: bool,
}

impl ModelShape<'_> {
    pub fn column(&self, name: &str) -> Option<&super::Column> {
        self.columns.iter().find(|column| column.name() == name)
    }
}

/// The shared input of every model guest emitter.
#[derive(Debug)]
pub(super) struct ModelDescription<'a> {
    pub package: &'a crate::manifest::PackageIdentity,
    pub name: &'a str,
    pub min_lengths: &'a BTreeMap<String, u32>,
    pub shape: ModelShape<'a>,
    pub operations: BTreeMap<CrudAction, ModelOperation<'a>>,
    pub api: WamnApi,
    pub paths: &'a BTreeMap<String, Vec<String>>,
    pub feed: bool,
    pub number: Option<(&'a str, &'a str, wamn_number_pattern::Pattern)>,
}

/// The shared input of every authored-operation guest emitter.
#[derive(Debug)]
pub(super) struct CustomDescription<'a> {
    pub declaration: &'a CustomOperationDeclaration,
    pub statements: &'a BTreeMap<String, crate::manifest::StaticSqlStatementDeclaration>,
    pub request: Vec<crate::client_ir::FieldIr>,
    pub request_root: &'static str,
    pub feed_key: Option<SqlValueType>,
    pub result: Option<std::borrow::Cow<'a, CustomOperationResultDeclaration>>,
    pub refusals: BTreeMap<String, OperationErrorDetailDeclaration>,
    pub revision: Option<SqlValueType>,
    pub rows: Vec<super::RustRow>,
    pub accessors: Vec<super::StaticSqlAccessor>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "model emission owns the complete validated model and catalog context"
)]
pub(super) fn emit_model(
    files: &mut BTreeMap<String, Vec<u8>>,
    operations: &mut BTreeMap<String, super::OperationDescription>,
    sql_corpus: &super::SqlCorpus,
    transactional: &StatementTransactionality,
    operation_sql: &BTreeMap<String, Vec<String>>,
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
) -> Result<(), GenerateError> {
    if model.operations.is_empty() {
        return Ok(());
    }

    let api = wamn_api(catalog, model_name, model, table, operation_sql);
    let mut compiled = BTreeMap::new();
    for (action, operation) in &model.operations {
        let (description, facts) = describe_model_operation(
            catalog,
            sql_corpus,
            transactional,
            manifest,
            model_name,
            model,
            table,
            *action,
            operation,
            &api,
        )?;
        operations.insert(format!("{model_name}.{}", action.as_str()), description);
        compiled.insert(*action, facts);
    }
    let number = model.number_field().map(|(field, sequence)| {
        (
            field,
            sequence,
            wamn_number_pattern::parse(&manifest.sequences[sequence].pattern)
                .expect("validated number pattern"),
        )
    });
    let compiled = ModelDescription {
        package: &manifest.package,
        name: model_name,
        min_lengths: &model.min_lengths,
        shape: ModelShape {
            columns: table.columns(),
            key: crate::key::key_type(table)?,
            generated_id: crate::key::database_generated(table),
        },
        operations: compiled,
        api,
        paths: operation_sql,
        feed: model.feed_enabled(),
        number,
    };
    emit_model_wit(files, &compiled)?;
    super::data::emit_model_data(files, &compiled)?;
    super::component::emit_model_component(files, &compiled)?;
    emit_projection(files, &compiled)
}

pub(super) fn compile_custom<'a>(
    operation: &'a CustomOperationDeclaration,
    sql: &'a CustomOperationDeclaration,
) -> CustomDescription<'a> {
    let mut result = operation.result.as_ref().map(std::borrow::Cow::Borrowed);
    let mut refusals: BTreeMap<_, _> = operation
        .errors
        .iter()
        .map(|literal| {
            (
                literal.clone(),
                crate::manifest::custom_operation_error_detail(operation, literal),
            )
        })
        .collect();
    let feed_key = if operation.feed == Some(true) {
        let result = result.as_mut().expect("validated feed result").to_mut();
        result.class = ResultClass::Page;
        if !result
            .fields
            .iter()
            .any(|field| field.path == "row_version")
        {
            result.fields.push(ContractFieldDeclaration {
                path: "row_version".to_owned(),
                ty: SqlValueType::Int64,
                nullable: false,
                revision: crate::manifest::Revision::Flag(false),
                values: Vec::new(),
                label: None,
                visible: None,
                description: None,
                references: None,
            });
        }
        refusals
            .entry("cursor_expired".to_owned())
            .or_insert_with(|| {
                crate::manifest::custom_operation_error_detail(operation, "cursor_expired")
            });
        let key = &operation
            .lists
            .as_ref()
            .expect("validated feed list")
            .key_field[0];
        Some(
            operation
                .statements
                .values()
                .next()
                .expect("validated statement")
                .row
                .iter()
                .find(|field| &field.name == key)
                .expect("validated key")
                .ty,
        )
    } else {
        None
    };
    let revision = result
        .as_ref()
        .and_then(|result| {
            result
                .fields
                .iter()
                .find(|field| field.revision.is_revision())
        })
        .map(|field| field.ty);
    let mut request = crate::client_fields::input_fields_of(
        &serde_json::to_value(&operation.input).expect("validated request serializes"),
    );
    request.retain(|field| field.path != "request_id");
    let request_root = if operation.result.is_some()
        && let Some(value) = request.iter().position(|field| field.path == "value")
    {
        request = request.remove(value).children;
        "value."
    } else {
        ""
    };
    CustomDescription {
        declaration: operation,
        statements: &sql.statements,
        feed_key,
        request,
        request_root,
        result,
        revision,
        refusals,
        rows: static_sql_rows(sql),
        accessors: super::rust::static_sql_accessors(sql),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "emission shares the compiled SQL corpus and operation descriptions"
)]
pub(super) fn emit_custom_operation(
    files: &mut BTreeMap<String, Vec<u8>>,
    operations: &mut BTreeMap<String, super::OperationDescription>,
    sql_corpus: &super::SqlCorpus,
    transactional: &StatementTransactionality,
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    operation_name: &str,
    compiled: &BTreeMap<String, CustomDescription<'_>>,
) -> Result<(), GenerateError> {
    let operation = &compiled[operation_name];
    operations.insert(
        operation_name.to_owned(),
        describe_custom_operation(
            sql_corpus,
            transactional,
            catalog,
            manifest,
            operation_name,
            operation,
        )?,
    );
    emit_custom_operation_wit(files, manifest, operation_name, compiled)?;
    super::authored_feed::emit(files, manifest, operation_name, operation)?;
    if operation.statements.is_empty() {
        return Ok(());
    }
    emit_static_sql_projection(files, &custom_artifact_stem(operation_name), operation)
}

fn describe_custom_operation(
    sql_corpus: &super::SqlCorpus,
    transactional: &StatementTransactionality,
    catalog: &CatalogIr,
    manifest: &PackageManifest,
    operation_name: &str,
    compiled: &CustomDescription<'_>,
) -> Result<super::OperationDescription, GenerateError> {
    let operation = compiled.declaration;
    let operation_id = canonical_operation_identity(&manifest.package, operation_name)?;
    let grant = (operation.visibility == crate::manifest::OperationVisibility::Public)
        .then(|| operation_id.clone());
    let statements =
        compiled
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
            .collect::<Result<Vec<_>, _>>()?;
    let operation_contract = wamn_catalog::OperationContract {
        type_: match operation.type_ {
            crate::manifest::CustomOperationType::Command => wamn_catalog::OperationType::Command,
            crate::manifest::CustomOperationType::Projection => {
                wamn_catalog::OperationType::Projection
            }
            crate::manifest::CustomOperationType::EventHandler => {
                wamn_catalog::OperationType::EventHandler
            }
        },
        visibility: Some(
            match operation.visibility {
                crate::manifest::OperationVisibility::Public => "public",
                crate::manifest::OperationVisibility::Private => "private",
            }
            .to_owned(),
        ),
        permission_token: operation.permission.clone(),
        grant,
        statements,
        label: operation.label.clone(),
        description: operation.description.clone(),
        lists: operation.lists.as_ref().map(|lists| json!(lists)),
        sync: operation.sync == Some(true),
        result: compiled
            .result
            .as_ref()
            .map(|result| result_class(result.class).to_owned()),
        transaction: operation
            .transaction
            .map(|_| "explicit_per_input".to_owned()),
        idempotent_by: operation.idempotent_by.as_ref().map(|value| json!(value)),
        relations: operation
            .relations
            .iter()
            .map(|relation| json!(relation))
            .collect(),
        record: None,
        operation: operation_id,
    };
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
    if operation.feed == Some(true) {
        input_contract.insert("envelope".to_owned(), json!({"minimum": 1, "maximum": 1}));
        input_contract.insert("feed".to_owned(), json!({
            "cursor": {"encoding": "base64url_unpadded", "opaque": true, "invalid": "invalid_input", "expired": "cursor_expired"},
            "limit": {"default": 100, "minimum": 1, "maximum": 100, "invalid": "invalid_input"}
        }));
    }

    Ok(super::OperationDescription {
        operation: operation_contract,
        input: Value::Object(input_contract),
        result: serde_json::to_value(&compiled.result).expect("result description serializes"),
        errors: custom_operation_error_contract(catalog, compiled),
    })
}

/// The contract of one statement, refusing a statement that takes a different
/// number of parameters than its generated accessor binds.
fn statement_contract(
    name: &str,
    path: &str,
    sql_corpus: &super::SqlCorpus,
    transactional: &StatementTransactionality,
    binds: impl IntoIterator<Item = StatementValueContract>,
    columns: impl IntoIterator<Item = StatementValueContract>,
) -> Result<StatementContract, GenerateError> {
    let binds = binds.into_iter().collect::<Vec<_>>();
    let columns = columns.into_iter().collect::<Vec<_>>();
    if !sql_corpus.statements.contains_key(path) {
        return Err(GenerateError::for_path(
            GenerateErrorType::InvalidOperation,
            "statement is absent from the SQL corpus",
            path,
        ));
    }
    if let Some(description) = transactional.described.get(path) {
        crate::describe::validate_contract(path, description, &binds, &columns)?;
    }
    Ok(StatementContract {
        name: name.to_owned(),
        path: path.to_owned(),
        binds,
        columns,
        relations: sql_corpus.relations[path].clone(),
        transactional: transactional.needs_transaction(path),
    })
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

fn statement_value_contract(
    name: &str,
    ty: SqlValueType,
    nullable: bool,
) -> StatementValueContract {
    StatementValueContract {
        name: name.to_owned(),
        ty,
        nullable,
    }
}

fn custom_operation_error_contract(catalog: &CatalogIr, compiled: &CustomDescription<'_>) -> Value {
    let operation = compiled.declaration;
    let cases = compiled
        .refusals
        .keys()
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
            let detail = compiled.refusals[literal].clone();
            let case_members = case
                .as_object_mut()
                .expect("error contract case is an object");
            case_members.insert(
                "detail".to_owned(),
                error_detail_contract(&detail, compiled.revision),
            );
            // Authored text rides on the case, as a label rides on its field.
            if let Some(text) = detail.text {
                case_members.insert("text".to_owned(), json!(text));
            }
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
        "idempotency_conflict" if operation.claims() => json!({
            "literal": literal,
            "from": "changed_request",
        }),
        _ => json!({
        "literal": literal,
        "from": "transaction_invariant",
        }),
    }
}

fn error_detail_contract(
    detail: &OperationErrorDetailDeclaration,
    revision: Option<SqlValueType>,
) -> Value {
    let mut contract = serde_json::Map::new();
    if !detail.required.is_empty() {
        contract.insert("required".to_owned(), json!(detail.required));
    }
    if !detail.optional.is_empty() {
        contract.insert("optional".to_owned(), json!(detail.optional));
    }
    contract.insert(
        "types".to_owned(),
        Value::Object(
            detail
                .required
                .iter()
                .chain(&detail.optional)
                .map(|key| {
                    let ty = match key {
                        OperationErrorDetailKey::ExpectedRowVersion
                        | OperationErrorDetailKey::ObservedRowVersion => {
                            revision.unwrap_or(SqlValueType::Int64)
                        }
                        _ => SqlValueType::Text,
                    };
                    (
                        serde_json::to_value(key)
                            .expect("detail key serializes")
                            .as_str()
                            .expect("detail name")
                            .to_owned(),
                        json!(ty),
                    )
                })
                .collect(),
        ),
    );
    Value::Object(contract)
}

pub(super) fn emit_operation_sql(
    sql_corpus: &mut BTreeMap<String, Vec<u8>>,
    model_name: &str,
    table: &Table,
    action: CrudAction,
    operation: &OperationDeclaration,
    model: &ModelDeclaration,
) -> Result<Vec<String>, GenerateError> {
    let delete_mode = model.delete_mode;
    let tombstoned = delete_mode == Some(DeleteMode::Tombstone);
    if action == CrudAction::Query {
        if let Some(authored) = &operation.authored_sql {
            return Ok(authored
                .variants
                .iter()
                .map(|variant| variant.path.clone())
                .collect());
        }
        let variants = query_variants(operation);
        let feed = model.feed_enabled();
        let mut statements = Vec::with_capacity(variants.len() + 1);
        for (field, direction) in variants {
            statements.push((
                format!(
                    "generated/sql/{model_name}/query_{field}_{}.sql",
                    sql::direction_name(direction)
                ),
                sql::query(table, operation, field, direction, tombstoned, feed),
            ));
        }
        // The change feed is the last statement of the list read.
        if feed {
            statements.push((
                format!("generated/sql/{model_name}/{CHANGES_STATEMENT}.sql"),
                sql::changes(table, operation, tombstoned),
            ));
        }
        let mut paths = Vec::with_capacity(statements.len());
        for (path, sql) in statements {
            let bytes = sql.into_bytes();
            if sql_corpus.insert(path.clone(), bytes).is_some() {
                return Err(GenerateError::for_path(
                    GenerateErrorType::DuplicatePath,
                    "generated SQL collides with the corpus",
                    path,
                ));
            }
            paths.push(path);
        }
        return Ok(paths);
    }

    let sql = match action {
        CrudAction::Create => sql::create(table, model, operation),
        CrudAction::Get => sql::get(table, tombstoned),
        CrudAction::Update => sql::update(table, model, operation, tombstoned),
        CrudAction::Delete => sql::delete(
            table,
            operation,
            delete_mode.expect("delete validation requires a declared mode"),
        ),
        CrudAction::Query => unreachable!("query returned above"),
    };
    let name = if action == CrudAction::Create {
        CREATE_STATEMENT
    } else {
        action.as_str()
    };
    let path = format!("generated/sql/{model_name}/{name}.sql");
    let bytes = sql.into_bytes();
    if sql_corpus.insert(path.clone(), bytes).is_some() {
        return Err(GenerateError::for_path(
            GenerateErrorType::DuplicatePath,
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
fn describe_model_operation<'a>(
    catalog: &CatalogIr,
    sql_corpus: &super::SqlCorpus,
    transactional: &StatementTransactionality,
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    action: CrudAction,
    operation: &'a OperationDeclaration,
    wamn_api: &WamnApi,
) -> Result<(super::OperationDescription, ModelOperation<'a>), GenerateError> {
    let operation_id = canonical_operation_identity(
        &manifest.package,
        &format!("{model_name}.{}", action.as_str()),
    )?;
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
    // A public create or update succeeds with the model row, and a delete
    // with its outcome. The observed revision of an update or a delete
    // remains an SQL accessor column.
    let mut columns = if action == CrudAction::Delete {
        accessors.first().map_or_else(Vec::new, |accessor| {
            operation_row_columns(wamn_api, &accessor.row)
                .into_iter()
                .filter(|column| column.name == "outcome")
                .collect()
        })
    } else {
        model_columns.clone()
    };
    if action == CrudAction::Query && model.feed_enabled() {
        columns.retain(|column| column.name != "row_version");
        columns.push(statement_value_contract(
            "row_version",
            SqlValueType::Int64,
            false,
        ));
    }
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
                    statement_value_contract(
                        authored_name(&bind.parameter),
                        bind.statement_type,
                        bind.nullable,
                    )
                }),
                statement_columns,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
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
    // A get carries its model's revision column, which gives it a strong ETag.
    let revision_field = if action == CrudAction::Get {
        model.revision_field()
    } else {
        operation.revision_field.as_deref()
    };
    if let Some(revision) = revision_field {
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
    let fields = table
        .columns()
        .iter()
        .map(|column| column.name().to_owned())
        .collect::<Vec<_>>();
    let lists = if action == CrudAction::Query {
        let mut lists = json!({"model": model_name, "key_field": "id"});
        if let Some(display) = &operation.display_field {
            lists["display_field"] = json!(display);
        }
        Some(lists)
    } else {
        None
    };
    let operation_contract = wamn_catalog::OperationContract {
        type_: match action {
            CrudAction::Get => wamn_catalog::OperationType::Get,
            CrudAction::Query => wamn_catalog::OperationType::Query,
            CrudAction::Create => wamn_catalog::OperationType::Create,
            CrudAction::Update => wamn_catalog::OperationType::Update,
            CrudAction::Delete => wamn_catalog::OperationType::Delete,
        },
        record: Some(Value::Object(record)),
        visibility: Some("public".to_owned()),
        permission_token: Some(operation.permission.clone()),
        grant: Some(operation_id.clone()),
        result: Some(result_class(operation.result).to_owned()),
        statements,
        transaction: Some(
            if action == CrudAction::Create {
                "explicit_per_input"
            } else {
                "implicit"
            }
            .to_owned(),
        ),
        label: operation.label.clone(),
        description: operation.description.clone(),
        lists,
        sync: false,
        relations: vec![json!(generated_relation_access(
            model, action, operation, &fields
        )?)],
        idempotent_by: (action == CrudAction::Create).then(|| json!("claim")),
        operation: operation_id,
    };
    let mut input = input_contract(manifest, model, table, action, operation);
    // A list read with a change feed takes the feed part in place of a page
    // request: the cursor a page or a previous feed read returned, and the
    // limit of the page (docs/architecture/execution.md#change-feed).
    if action == CrudAction::Query
        && model.feed_enabled()
        && let Some(limit) = &operation.limit
    {
        input["feed"] = json!({
            "cursor": {
                "encoding": "base64url_unpadded",
                "opaque": true,
                "invalid": "invalid_input",
                "expired": "cursor_expired",
            },
            "limit": {
                "default": limit.default,
                "minimum": limit.minimum,
                "maximum": limit.maximum,
                "invalid": "invalid_input",
            },
        });
    }
    let result = crud_result_contract(manifest, model, table, action, operation.result, &columns);
    let mut refusals = super::operation_error_details(catalog, table, action, operation, model);
    let guarded_fields = operation_constraints(catalog, table, action, operation, model)
        .into_iter()
        .filter_map(|constraint| {
            let field = constraint_field(constraint, action, operation)?;
            let detail = refusals
                .get_mut(&constraint_error_code(constraint.constraint_type()))
                .expect("constraint refusal is declared");
            if !detail.required.contains(&OperationErrorDetailKey::Field)
                && !detail.optional.contains(&OperationErrorDetailKey::Field)
            {
                detail.optional.push(OperationErrorDetailKey::Field);
            }
            Some((constraint.name().to_owned(), field))
        })
        .collect();
    let revision = operation
        .revision_field
        .as_deref()
        .and_then(|name| column(table, name))
        .map(super::Column::column_type);
    let facts = ModelOperation {
        request: operation,
        result,
        refusals,
        revision,
        guarded_fields,
    };
    let description = super::OperationDescription {
        operation: operation_contract,
        input,
        result: serde_json::to_value(&facts.result).expect("compiled result serializes"),
        errors: error_contract(catalog, table, action, model, &facts),
    };
    Ok((description, facts))
}

fn result_class(class: ResultClass) -> &'static str {
    match class {
        ResultClass::One => "one",
        ResultClass::OptionalOne => "optional_one",
        ResultClass::Page => "page",
        ResultClass::BoundedList => "bounded_list",
    }
}

fn model_text(model: &ModelDeclaration, column: &str) -> FieldText {
    model.field_text.get(column).cloned().unwrap_or_default()
}

/// The two text members of a model column, as contract JSON members.
/// The model whose record one column names, from its own foreign key.
///
/// A generated action reads this instead of an authored declaration, because
/// its writable field IS a model column and the catalog already states where
/// that column points. The referenced table maps back to the model that
/// declares it, so a relation with no model states nothing.
fn column_reference(
    manifest: &PackageManifest,
    table: &Table,
    column: &str,
) -> Option<(String, String)> {
    let (schema, referenced, key) = table.constraints().iter().find_map(|constraint| {
        let ConstraintType::ForeignKey {
            columns,
            referenced_schema,
            referenced_table,
            ..
        } = constraint.constraint_type()
        else {
            return None;
        };
        // One column only: a composite key has no single value a selector
        // could hand back, and no model declares one today.
        let [pair] = &columns[..] else { return None };
        (pair.column() == column).then(|| {
            (
                referenced_schema.to_string(),
                referenced_table.to_string(),
                pair.referenced_column().to_owned(),
            )
        })
    })?;
    let (model_name, _) = manifest
        .models
        .iter()
        .find(|(_, model)| model.schema == schema && model.table == referenced)?;
    Some((model_name.clone(), key))
}

fn model_text_members(model: &ModelDeclaration, column: &str) -> Vec<(String, Value)> {
    let text = model_text(model, column);
    text.label
        .map(|label| ("label".to_owned(), json!(label)))
        .into_iter()
        .chain(
            text.visible
                .map(|visible| ("visible".to_owned(), json!(visible))),
        )
        .chain(
            text.description
                .map(|description| ("description".to_owned(), json!(description))),
        )
        .collect()
}

fn crud_result_contract(
    manifest: &PackageManifest,
    model: &ModelDeclaration,
    table: &Table,
    action: CrudAction,
    class: ResultClass,
    columns: &[StatementValueContract],
) -> CustomOperationResultDeclaration {
    CustomOperationResultDeclaration {
        class,
        signed_reads: BTreeMap::new(),
        fields: columns
            .iter()
            .map(|column| ContractFieldDeclaration {
                path: column.name.clone(),
                ty: column.ty,
                nullable: column.nullable,
                revision: crate::manifest::Revision::Flag(
                    model.revision_field() == Some(column.name.as_str())
                        && !(action == CrudAction::Query
                            && model.feed_enabled()
                            && column.name == "row_version"),
                ),
                values: if action == CrudAction::Delete && column.name == "outcome" {
                    vec![sql::OUTCOME_DELETED.to_owned()]
                } else {
                    model
                        .enum_fields
                        .get(&column.name)
                        .cloned()
                        .unwrap_or_default()
                },
                // A generated action declares no result field, so the column's
                // own text is the only text this result can carry.
                label: model_text(model, &column.name).label,
                visible: model_text(model, &column.name).visible,
                description: model_text(model, &column.name).description,
                // The column's own foreign key states the record it names,
                // so a table can show that record's text instead of its key.
                references: column_reference(manifest, table, &column.name).map(|(model, _)| {
                    FieldReference {
                        model,
                        narrowed_by: None,
                    }
                }),
            })
            .collect(),
    }
}

fn input_contract(
    manifest: &PackageManifest,
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
            // A create that omits a NOT NULL column with no default cannot
            // take a default, so the omission is refused, not defaulted.
            let omitted = if action == CrudAction::Update {
                "unchanged"
            } else if super::omission_refused(column) {
                "invalid_input"
            } else {
                "postgres_default"
            };
            let mut declared = json!({
                "field": field,
                "path": writable_path(action, field),
                "type": column.column_type().as_str(),
                "omitted": omitted,
                "explicit_null": if column.nullable() { "accepted" } else { "invalid_input" },
            });
            // A control the operator fills reads the column's own text, at the
            // path the input states, which is `change.<field>` on an update.
            let members = declared.as_object_mut().expect("a writable field object");
            members.extend(model_text_members(model, field));
            // The column's own foreign key states the record it names, so a
            // generated action derives what an authored operation declares.
            if let Some((referenced, _)) = column_reference(manifest, table, field) {
                members.insert("references".to_owned(), json!({"model": referenced}));
            }
            // A field states the values it accepts: the model's, or the fewer
            // a create declares, so a client and the route offer only those.
            if let Some(values) = operation
                .values
                .get(field)
                .or_else(|| model.enum_fields.get(field))
            {
                members.insert("values".to_owned(), json!(values));
            }
            declared
        })
        .collect::<Vec<_>>();
    let mut server_owned = server_owned_fields(model, table);
    if action != CrudAction::Create {
        // A row locator is an input even when the database generated it.
        server_owned.retain(|field| *field != "id");
    }
    let mut common = json!({
        "server_owned_fields": {
            "fields": server_owned,
            "if_supplied": "invalid_input",
        },
        "writable_fields": writable,
    });
    // A read carries no request identity: its one request element travels in a GET query
    // string, and its outcomes match its request elements by list position.
    if !matches!(action, CrudAction::Get | CrudAction::Query) {
        common["request_id"] = json!({"type": "string", "required": true});
    }
    // The key input and a filter both name a model column, so both read that
    // column's text. The revision input names one too, and it is deliberately
    // left silent: no screen renders it, because the plan reserves it.
    let mut key = json!({
        "type": column(table, "id").expect("validated id column").column_type().as_str(),
        "required": true,
    });
    key.as_object_mut()
        .expect("the key input object")
        .extend(model_text_members(model, "id"));
    match action {
        CrudAction::Get => merge_json(common, &json!({"id": key})),
        CrudAction::Query => query_search(
            operation,
            merge_json(
                common,
                &json!({
                    "filters": operation.filters.iter().map(|filter| {
                        let column = column(table, &filter.field).expect("validated filter column");
                        let mut declared = json!({
                            "field": filter.field,
                            "binding": filter.match_mode.binding(),
                            "type": column.column_type().as_str(),
                        });
                        let members = declared.as_object_mut().expect("a filter object");
                        if !filter.match_mode.is_exact() {
                            members.insert("match".to_owned(), json!(filter.match_mode.as_str()));
                        }
                        if filter.required {
                            members.insert("required".to_owned(), json!(true));
                        }
                        if let Some(default) = filter.default {
                            members.insert(
                                "default".to_owned(),
                                json!({"last_days": default.last_days}),
                            );
                        }
                        members.extend(model_text_members(model, &filter.field));
                        if let Some(values) = model.enum_fields.get(&filter.field) {
                            members.insert("values".to_owned(), json!(values));
                        }
                        declared
                    }).collect::<Vec<_>>(),
                    "sort": operation.sort.as_ref().map(|sort| json!({
                        "fields": sort.fields,
                        "directions": sort.directions,
                        "max_fields": 1,
                    })),
                    "pagination": operation.pagination.as_ref().map(|pagination| json!({
                        "type": "keyset",
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
        ),
        CrudAction::Create => {
            let generated = crate::key::database_generated(table);
            let mut create = json!({
                CREATE_KEY_FIELD: {"type": "text", "required": true},
                "request": {
                    "over": if generated { "writable_fields" } else { "id_and_writable_fields" },
                    "payload": "canonical_compact_json",
                    "changed": "idempotency_conflict",
                },
            });
            if !generated {
                create
                    .as_object_mut()
                    .expect("create input is an object")
                    .insert("id".to_owned(), key);
            }
            merge_json(common, &create)
        }
        CrudAction::Update | CrudAction::Delete => {
            let revision = operation
                .revision_field
                .as_deref()
                .expect("mutation validation requires a revision field");
            let mut mutation = json!({ "id": key });
            mutation
                .as_object_mut()
                .expect("mutation input contract is an object")
                .insert(
                    format!("expected_{revision}"),
                    json!({
                        "field": revision,
                        // The revision carries the width of its own column.
                        "type": column(table, revision)
                            .expect("validated revision column")
                            .column_type()
                            .as_str(),
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
    model: &ModelDeclaration,
    facts: &ModelOperation<'_>,
) -> Value {
    use crate::manifest::AccessOperationErrorLiteral as Code;
    let operation = facts.request;
    let refusals = &facts.refusals;
    let revision = facts.revision;

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
    if action == CrudAction::Query && model.feed_enabled() {
        cases.push((Code::CursorExpired, json!({"literal": "cursor_expired"})));
    }
    if action == CrudAction::Create {
        cases.push((
            Code::IdempotencyConflict,
            json!({
                "literal": "idempotency_conflict",
                "from": "changed_request",
            }),
        ));
    }
    for constraint in operation_constraints(catalog, table, action, operation, model) {
        let code = constraint_error_code(constraint.constraint_type());
        cases.push((
            code,
            json!({
                "literal": constraint_error_code(constraint.constraint_type()),
                "from": constraint_error_code(constraint.constraint_type()),
                "constraint": constraint.name(),
            }),
        ));
    }
    for exclusion in operation_exclusions(table, action, operation) {
        cases.push((
            Code::ExclusionViolation,
            json!({
                "literal": Code::ExclusionViolation,
                "from": Code::ExclusionViolation,
                "constraint": exclusion.name(),
            }),
        ));
    }
    let cases = cases
        .into_iter()
        .map(|(code, mut case)| {
            let mut detail = refusals[&code].clone();
            if case["constraint"]
                .as_str()
                .is_some_and(|name| facts.guarded_fields.contains_key(name))
            {
                detail
                    .optional
                    .retain(|key| *key != OperationErrorDetailKey::Field);
                if !detail.required.contains(&OperationErrorDetailKey::Field) {
                    detail.required.insert(0, OperationErrorDetailKey::Field);
                }
            }
            case.as_object_mut()
                .expect("error contract case is an object")
                .insert(
                    "detail".to_owned(),
                    error_detail_contract(&detail, revision),
                );
            case
        })
        .collect::<Vec<_>>();
    json!({"closed": true, "cases": cases})
}

/// The schema an installed package needs: each relation it reads or writes,
/// with the fields and named constraints it consumes, written to
/// [`super::REQUIRED_SCHEMA_CONTRACT_PATH`]. Overlay compatibility evidence
/// tests a fresh base install against it.
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
                        definition: serde_json::to_value(constraint.constraint_type())
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

/// A query's contract with its search, when the query declares one. A query
/// without one keeps its bytes.
fn query_search(operation: &OperationDeclaration, mut contract: Value) -> Value {
    if let Some(search) = &operation.search {
        contract["search"] = json!({"fields": search.fields, "binding": "text"});
    }
    contract
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

fn count_contract(limit: &crate::manifest::CountLimitDeclaration) -> Value {
    let mut contract = json!({
        "minimum": limit.minimum,
        "maximum": limit.maximum,
        "invalid": "invalid_input",
    });
    let members = contract
        .as_object_mut()
        .expect("a count bound is an object");
    if let Some(label) = &limit.label {
        members.insert("label".to_owned(), json!(label));
    }
    if let Some(description) = &limit.description {
        members.insert("description".to_owned(), json!(description));
    }
    contract
}

pub(crate) fn generated_relation_access(
    model: &ModelDeclaration,
    action: CrudAction,
    operation: &OperationDeclaration,
    fields: &[String],
) -> Result<StaticSqlRelationDeclaration, GenerateError> {
    let mut access = StaticSqlRelationDeclaration {
        schema: model.schema.clone(),
        table: model.table.clone(),
        select_fields: fields.to_vec(),
        insert_fields: Vec::new(),
        update_fields: Vec::new(),
        delete: false,
        lock: false,
        constraints: Vec::new(),
    };
    match action {
        CrudAction::Get | CrudAction::Query => {}
        CrudAction::Create => {
            access.insert_fields.clone_from(&operation.writable_fields);
            access.insert_fields.push("id".to_owned());
            // The generated insert also writes the numbered and actor fields.
            access.insert_fields.extend(
                model
                    .number_field()
                    .map(|(field, _)| field)
                    .into_iter()
                    .chain(model.actor_fields())
                    .map(str::to_owned),
            );
        }
        CrudAction::Update => {
            access.update_fields.clone_from(&operation.writable_fields);
            access
                .update_fields
                .extend(operation.revision_field.iter().cloned());
            // The generated update also fills the updated_by actor field.
            access
                .update_fields
                .extend(model.update_actor_field().map(str::to_owned));
            access.lock = true;
        }
        // A hard delete removes the row, so the relation carries
        // table-level DELETE. A tombstone delete is an UPDATE that sets
        // the two reserved marker columns, which no caller writes, so
        // the shape is column UPDATE on that pair and no DELETE.
        CrudAction::Delete => match model.delete_mode {
            // Both delete statements open with the same locking read of
            // the id and the revision, so both need the read and the
            // lock that an update needs. `FOR UPDATE` is validated as the
            // UPDATE privilege, which the revision column supplies for a
            // hard delete and the marker pair supplies for a tombstone.
            Some(DeleteMode::Hard) => {
                access
                    .update_fields
                    .extend(operation.revision_field.iter().cloned());
                access.lock = true;
                access.delete = true;
            }
            Some(DeleteMode::Tombstone) => {
                access.update_fields.extend(
                    TombstoneColumn::ALL
                        .into_iter()
                        .map(|column| column.as_str().to_owned()),
                );
                access.lock = true;
            }
            None => {
                return Err(GenerateError::new(
                    GenerateErrorType::InvalidOperation,
                    "generated data-access overlay has a delete without a delete mode",
                ));
            }
        },
    }
    for list in [
        &mut access.select_fields,
        &mut access.insert_fields,
        &mut access.update_fields,
    ] {
        list.sort();
        list.dedup();
    }
    Ok(access)
}
