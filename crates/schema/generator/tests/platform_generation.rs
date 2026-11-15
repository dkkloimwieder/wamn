use serde_json::{Value, json};
use wamn_compiler::ManifestErrorType as GenerateErrorType;
use wamn_compiler::manifest::{PackageManifest, validate_operation_vocabulary};

#[path = "support/platform_fixture.rs"]
mod fixture;
#[path = "support/platform_claim.rs"]
mod platform_claim;
#[path = "support/platform_descriptors.rs"]
mod platform_descriptors;

#[path = "support/platform_protocol.rs"]
mod platform_protocol;

#[path = "support/platform_errors.rs"]
mod platform_errors;

fn description_json(package: &wamn_compiler::GeneratedPackage, local: &str, part: &str) -> Value {
    serde_json::to_value(&package.operations()[local]).unwrap()[part].clone()
}

fn parsed(value: &Value) -> PackageManifest {
    PackageManifest::from_slice(&serde_json::to_vec(value).expect("serialize manifest"))
        .expect("parse manifest")
}

#[test]
#[ignore = "requires: input:fixture"]
fn business_error_details_are_closed() {
    let baseline = fixture::manifest();
    let mut platform_detail = baseline.clone();
    platform_detail["custom_operations"]["widget.archive"]["error_details"]["invalid_input"] =
        json!({"required": ["field"]});
    assert_eq!(
        validate_operation_vocabulary(&parsed(&platform_detail))
            .expect_err("platform error detail was authored")
            .error_type(),
        GenerateErrorType::InvalidOperation
    );

    let mut missing_business_detail = baseline;
    missing_business_detail["custom_operations"]["widget.archive"]["error_details"] = json!({});
    assert_eq!(
        validate_operation_vocabulary(&parsed(&missing_business_detail))
            .expect_err("business error detail was omitted")
            .error_type(),
        GenerateErrorType::InvalidOperation
    );
}

#[test]
#[ignore = "requires: input:fixture"]
fn state_idempotence_requires_its_exact_shape() {
    for guards in [
        json!({}),
        json!({"missing": "expected_edit_version"}),
        json!({"widget": "missing"}),
    ] {
        let mut manifest = fixture::manifest();
        manifest["custom_operations"]["widget.archive"]["idempotent_by"]["state"]["guards"] =
            guards;
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let parsed = PackageManifest::from_slice(&bytes).unwrap();
        assert_eq!(
            validate_operation_vocabulary(&parsed)
                .expect_err("invalid state guard was accepted")
                .error_type(),
            GenerateErrorType::InvalidOperation
        );
    }

    let mut absent = fixture::manifest();
    absent["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap()
        .remove("idempotent_by");
    let error = validate_operation_vocabulary(&parsed(&absent))
        .expect_err("command without idempotence was accepted");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
}

/// `stateless` is for a command with no SQL, whose replay guard is the
/// engine's per request element intent. A command with SQL still names claim or state
/// (owner ruling, `wamn-hxow`).
#[test]
#[ignore = "requires: input:fixture"]
fn stateless_idempotence_is_only_for_a_command_without_sql() {
    let mut manifest = fixture::manifest();
    manifest["custom_operations"]["widget.inspect"] = json!({
        "type": "command",
        "visibility": "public",
        "permission": "widget.inspect",
        "idempotent_by": "stateless",
        "input": {"fields": [
            {"path": "request_id", "type": "text", "nullable": false},
            {"path": "value.note", "type": "text", "nullable": false}
        ]},
        "result": {"class": "one", "fields": [
            {"path": "note", "type": "text", "nullable": false}
        ]},
        "errors": ["invalid_input", "permission_denied", "internal_error"],
        "error_details": {}
    });
    validate_operation_vocabulary(&parsed(&manifest))
        .expect("a stateless command without SQL is accepted");

    let mut with_sql = fixture::manifest();
    with_sql["custom_operations"]["widget.archive"]["idempotent_by"] = json!("stateless");
    let error = validate_operation_vocabulary(&parsed(&with_sql))
        .expect_err("a stateless command with SQL was accepted");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
}

/// The fixture manifest with [`fixture::UNREAD_TABLE`] as an internal relation.
fn with_internal_relation() -> serde_json::Value {
    let mut manifest = fixture::manifest();
    manifest["internal_relations"] = json!({
        (fixture::UNREAD_TABLE): {
            "schema": "inventory", "table": fixture::UNREAD_TABLE, "cdc": "excluded"
        }
    });
    manifest
}

/// Validation refuses the `claim` object: `idempotent_by: claim` is the whole
/// declaration. A claim command must carry the key it claims, and it commits
/// its claim and its work together.
#[test]
fn a_claim_command_declares_no_claim_object() {
    let manifest = platform_claim::manifest();
    fixture::generate_with(&fixture::catalog(), &manifest);

    let mut claim_object = manifest.clone();
    claim_object["custom_operations"]["widget.archive"]["claim"] = json!({
        "table": "widget_command", "identities": {"id": "widget_id"},
        "claim": "claim", "replay": "replay", "finalize": "finalize"
    });
    let error = fixture::try_generate_with(&fixture::catalog(), &claim_object)
        .expect_err("a claim object refuses");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidManifest);

    let mut keyless = manifest.clone();
    keyless["custom_operations"]["widget.archive"]["input"]["fields"][0]["path"] = json!("key");
    let error = validate_operation_vocabulary(&parsed(&keyless)).unwrap_err();
    assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);

    let mut no_transaction = manifest;
    let operation = no_transaction["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap();
    operation.remove("transaction");
    operation.remove("automatic_retry");
    let error = validate_operation_vocabulary(&parsed(&no_transaction)).unwrap_err();
    assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
}

/// A model can say how its rows leave without publishing a route that removes
/// them: only its command SQL deletes. A delete action still needs its mode.
#[test]
#[ignore = "requires: input:fixture"]
fn a_delete_mode_needs_no_delete_route() {
    let mut manifest = fixture::manifest();
    manifest["models"]["widget"]["operations"]
        .as_object_mut()
        .unwrap()
        .remove("delete");
    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    assert!(!package.operations().contains_key("widget.delete"));

    let mut action_without_mode = fixture::manifest();
    action_without_mode["models"]["widget"]
        .as_object_mut()
        .unwrap()
        .remove("delete_mode");
    let error = validate_operation_vocabulary(&parsed(&action_without_mode)).unwrap_err();
    assert_eq!(error.error_type(), GenerateErrorType::InvalidModel);
}

/// EXIT CONDITION for `wamn-c2y5.1`: authored text is optional everywhere, it is
/// carried where the author wrote it, and a misspelled key is refused.
///
/// The refusal matters more than the parse. A label that silently disappears
/// because its key is `labels` would reach an operator as a field path, and
/// nothing would say why.
#[test]
#[ignore = "requires: input:fixture"]
fn authored_text_is_optional_and_its_key_is_closed() {
    let authored = fixture::manifest();
    let declared = parsed(&fixture::manifest());
    let widget = &declared.models["widget"];
    assert_eq!(
        widget.field_text["code"].label.as_deref(),
        authored["models"]["widget"]["field_text"]["code"]["label"].as_str()
    );
    assert!(widget.field_text["code"].description.is_some());
    assert_eq!(
        widget.field_text["note"].label.as_deref(),
        authored["models"]["widget"]["field_text"]["note"]["label"].as_str()
    );
    assert_eq!(
        widget.field_text["note"].description, None,
        "a label alone is a complete declaration"
    );
    assert_eq!(
        widget.operations[&wamn_compiler::manifest::CrudAction::Query]
            .label
            .as_deref(),
        authored["models"]["widget"]["operations"]["query"]["label"].as_str()
    );
    let batch = &declared.custom_operations["widget.record_batch"];
    assert_eq!(
        batch.label.as_deref(),
        authored["custom_operations"]["widget.record_batch"]["label"].as_str()
    );
    assert!(batch.description.is_some());
    let line = batch
        .input
        .fields
        .iter()
        .find(|field| field.path == "value.line[].amount")
        .expect("the repeated line amount");
    assert_eq!(
        line.label.as_deref(),
        authored["custom_operations"]["widget.record_batch"]["input"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["path"] == "value.line[].amount")
            .unwrap()["label"]
            .as_str()
    );

    // Every text member removed: the same manifest still parses, and the
    // whole vocabulary reads as absent rather than as empty text.
    let mut silent = fixture::manifest();
    silent["models"]["widget"]
        .as_object_mut()
        .expect("the widget model")
        .remove("field_text");
    silent["models"]["widget"]["operations"]["query"]
        .as_object_mut()
        .expect("the query operation")
        .remove("label");
    for key in ["label", "description"] {
        silent["custom_operations"]["widget.record_batch"]
            .as_object_mut()
            .expect("the batch command")
            .remove(key);
        // The line bound carries the repeated group's own text.
        silent["custom_operations"]["widget.record_batch"]["input"]["line"]
            .as_object_mut()
            .expect("the line bound")
            .remove(key);
        for pointer in [
            "/custom_operations/widget.record_batch/input/fields",
            "/custom_operations/widget.list/result/fields",
        ] {
            for field in silent
                .pointer_mut(pointer)
                .expect("declared fields")
                .as_array_mut()
                .expect("declared fields")
            {
                field.as_object_mut().expect("a field object").remove(key);
            }
        }
    }
    let quiet = parsed(&silent);
    assert!(quiet.models["widget"].field_text.is_empty());
    assert_eq!(
        quiet.custom_operations["widget.record_batch"].label, None,
        "an absent label is absent, never an empty string"
    );

    // A misspelled key is refused by the closed vocabulary, at each carrier.
    let mut model_text = fixture::manifest();
    model_text["models"]["widget"]["field_text"]["code"]["labels"] = json!("Widget code");
    let error =
        PackageManifest::from_slice(&serde_json::to_vec(&model_text).expect("serialize manifest"))
            .expect_err("a misspelled model label key was accepted");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidManifest);

    let mut field_text = fixture::manifest();
    field_text["custom_operations"]["widget.record_batch"]["input"]["fields"][0]["labels"] =
        json!("Request");
    let error =
        PackageManifest::from_slice(&serde_json::to_vec(&field_text).expect("serialize manifest"))
            .expect_err("a misspelled field label key was accepted");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidManifest);

    for path in [
        "/models/widget/operations/query",
        "/custom_operations/widget.record_batch",
    ] {
        let mut spelling = fixture::manifest();
        *spelling.pointer_mut(path).expect("the operation") = {
            let mut operation = spelling.pointer(path).expect("the operation").clone();
            operation
                .as_object_mut()
                .expect("an operation object")
                .insert("labels".to_owned(), json!("Find widgets"));
            operation
        };
        let error = PackageManifest::from_slice(
            &serde_json::to_vec(&spelling).expect("serialize manifest"),
        )
        .expect_err("a misspelled operation label key was accepted");
        assert_eq!(error.error_type(), GenerateErrorType::InvalidManifest);
    }
}

#[test]
#[ignore = "requires: input:fixture"]
fn generated_query_display_fields_require_a_returned_field() {
    for field in ["missing", ""] {
        let mut manifest = fixture::manifest();
        manifest["models"]["widget"]["operations"]["query"]["display_field"] = json!(field);
        let error = fixture::try_generate_with(&fixture::catalog(), &manifest)
            .expect_err("a query label must name a returned field");
        assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
    }
    for action in ["get", "create", "update", "delete"] {
        let mut manifest = fixture::manifest();
        if manifest["models"]["widget"]["operations"]
            .get(action)
            .is_none()
        {
            continue;
        }
        manifest["models"]["widget"]["operations"][action]["display_field"] = json!("code");
        let error = fixture::try_generate_with(&fixture::catalog(), &manifest)
            .expect_err("only a query declares a list label");
        assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
    }
}

#[test]
#[ignore = "requires: input:fixture"]
fn authored_query_display_fields_require_a_declared_result_field() {
    for field in ["missing", ""] {
        let mut manifest = fixture::manifest();
        manifest["custom_operations"]["widget.list"]["lists"]["display_field"] = json!(field);
        let error = fixture::try_generate_with(&fixture::catalog(), &manifest)
            .expect_err("an authored list label must name a returned field");
        assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
    }
    let mut manifest = fixture::manifest();
    manifest["custom_operations"]["widget.list"]["lists"]["display_field"] = json!("note");
    let error = fixture::try_generate_with(&fixture::catalog(), &manifest)
        .expect_err("a model field omitted from the authored result cannot label its rows");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);

    let mut manifest = fixture::manifest();
    manifest["custom_operations"]["widget.archive"]["lists"] =
        json!({"key_field": "id", "display_field": "note"});
    let error = fixture::try_generate_with(&fixture::catalog(), &manifest)
        .expect_err("a command cannot declare a query label");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
}

/// EXIT CONDITION for `wamn-rm14.6`: the envelope bound carries no screen text.
///
/// The line bound owns the repeated group a form renders. The envelope is the
/// submission itself, so text on it would be a member nothing reads, and
/// generation refuses it instead of accepting it.
#[test]
#[ignore = "requires: input:fixture"]
fn screen_text_on_the_envelope_bound_refuses_and_names_the_line_bound() {
    let mut misplaced = fixture::manifest();
    misplaced["custom_operations"]["widget.record_batch"]["input"]["envelope"]["label"] =
        json!("Batch");
    let error = fixture::try_generate_with(&fixture::catalog(), &misplaced)
        .expect_err("screen text on the envelope bound was accepted");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
}

/// A route member that publish could not serve refuses, and names the member.
#[test]
#[ignore = "requires: input:fixture"]
fn a_route_member_publish_cannot_serve_refuses() {
    for (member, value) in [
        ("auth_modes", json!(["none"])),
        ("auth_modes", json!(["session", "pat"])),
        ("path_prefix", json!("acme")),
        ("path_prefix", json!("/acme/")),
        ("id_prefix", json!("Acme")),
        ("id_prefix", json!("acme-")),
    ] {
        let mut manifest = fixture::manifest();
        manifest["routes"][member] = value.clone();
        let error = fixture::try_generate_with(&fixture::catalog(), &manifest)
            .expect_err("the route member refuses");
        assert_eq!(
            error.error_type(),
            GenerateErrorType::InvalidManifest,
            "{value}"
        );
    }
}

/// A minimum length needs the CHECK that guards it, and generation writes no
/// migration: the refusal names the column and the expression the author
/// writes (wamn-iowb.4).
#[test]
#[ignore = "requires: input:fixture"]
fn a_minimum_length_without_its_check_refuses() {
    let mut manifest = fixture::manifest();
    manifest["models"]["widget"]["min_lengths"] = json!({"code": 1});
    let error = fixture::try_generate_with(&fixture::catalog(), &manifest)
        .expect_err("the fixture table has no CHECK on code");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidModel);

    for (field, minimum) in [("code", 0), ("edit_version", 1)] {
        let mut manifest = fixture::manifest();
        manifest["models"]["widget"]["min_lengths"] = json!({field: minimum});
        let _error = fixture::try_generate_with(&fixture::catalog(), &manifest)
            .expect_err("a minimum length is at least 1 on text");
    }
}

/// Authored declarations without the fixture build's derived members.
fn without_relations(mut manifest: Value) -> Value {
    manifest
        .as_object_mut()
        .unwrap()
        .remove("compiled_operations");
    for operation in manifest["custom_operations"]
        .as_object_mut()
        .expect("custom operations")
        .values_mut()
    {
        let operation = operation.as_object_mut().expect("an operation");
        operation.remove("relations");
        operation.remove("feed_source");
    }
    for model in manifest["models"]
        .as_object_mut()
        .expect("models")
        .values_mut()
    {
        model["server_owned_fields"]
            .as_array_mut()
            .expect("server-owned fields")
            .retain(|field| {
                !wamn_compiler::manifest::RecordHistoryColumn::ALL
                    .iter()
                    .any(|column| field == column.as_str())
            });
        model
            .as_object_mut()
            .expect("a model")
            .remove("enum_fields");
    }
    manifest
}

fn derive(manifest: &Value) -> Result<Value, wamn_compiler::ManifestError> {
    let sql = fixture::authored_sql();
    let sources = sql
        .iter()
        .map(|(path, bytes)| wamn_compiler::AuthoredSql::new(path, bytes))
        .collect::<Vec<_>>();
    let compiled = wamn_compiler::derive_manifest(
        &fixture::catalog(),
        &serde_json::to_vec(manifest).expect("serialize manifest"),
        &sources,
        &[parsed(&fixture::manifest())],
    )?;
    Ok(serde_json::from_slice(&compiled).expect("compiled JSON"))
}

/// The compiler accepts authored declarations and refuses computed members in input.
#[test]
#[ignore = "requires: input:fixture"]
fn the_compiled_manifest_carries_the_relations_the_statements_reach() {
    let committed = fixture::manifest();
    derive(&without_relations(committed)).expect("derive the authored manifest");
    let mut authored = without_relations(fixture::manifest());
    for (member, value) in [("relations", json!([])), ("feed_source", Value::Null)] {
        authored["custom_operations"]["widget.list"][member] = value;
        assert!(derive(&authored).is_err());
        authored["custom_operations"]["widget.list"]
            .as_object_mut()
            .unwrap()
            .remove(member);
    }
    assert!(derive(&fixture::manifest()).is_err());
}

/// A constraint that a command maps to an error belongs to the relation that
/// its SQL reaches and that defines it. One that no reached relation defines
/// refuses.
#[test]
#[ignore = "requires: input:fixture"]
fn a_mapped_constraint_lands_on_the_relation_that_defines_it() {
    let mut manifest = without_relations(fixture::manifest());
    let archive = &mut manifest["custom_operations"]["widget.archive"];
    archive["errors"]
        .as_array_mut()
        .expect("errors")
        .push(json!("code_taken"));
    archive["error_details"]["code_taken"] = json!({"required": ["constraint"]});
    archive["constraint_errors"] = json!({"widget_code_key": "code_taken"});
    let compiled = derive(&manifest).expect("derive the manifest");
    {
        let observed =
            &(compiled["custom_operations"]["widget.archive"]["relations"][0]["constraints"]);
        assert_eq!(observed.as_array().expect("array").len(), 1);
        assert_eq!(observed[0], serde_json::json!("widget_code_key"));
    };

    manifest["custom_operations"]["widget.archive"]["constraint_errors"] =
        json!({"widget_maker_id_pkey": "code_taken"});
    let _error = derive(&manifest).expect_err("widget.archive reaches no widget_maker");
}

/// The stamp columns are server-owned from the table, so an authored list
/// that names one refuses.
#[test]
#[ignore = "requires: input:fixture"]
fn a_stated_stamp_column_refuses() {
    let mut authored = without_relations(fixture::manifest());
    authored["models"]["widget"]["server_owned_fields"] = json!(["created_at"]);
    let _error = derive(&authored).expect_err("a stamp is not authored");
}

/// A model query that its table alone answers is generated: authored SQL for
/// it refuses. Authored SQL that joins another table stays.
#[test]
#[ignore = "requires: input:fixture"]
fn authored_sql_for_a_query_generation_writes_refuses() {
    let path = "query/widget_query.sql";
    let mut manifest = fixture::manifest();
    manifest["models"]["widget"]["operations"]["query"]["authored_sql"] = json!({
        "default": path,
        "variants": [
            {"field": "created_at", "direction": "ascending", "path": path},
            {"field": "created_at", "direction": "descending", "path": "query/widget_query_descending.sql"}
        ]
    });
    manifest["models"]["widget"]["operations"]["query"]["feed"] = json!(false);
    // The statement generation writes for this query, authored as is.
    let package = fixture::generate_fixture();
    let one_table = std::str::from_utf8(
        package
            .file("generated/sql/widget/query_created_at_ascending.sql")
            .expect("generated widget query")
            .bytes(),
    )
    .expect("generated SQL is UTF-8");
    let sources = |sql: &str| {
        fixture::authored_sql()
            .into_iter()
            .chain([
                (path.to_owned(), sql.as_bytes().to_vec()),
                (
                    "query/widget_query_descending.sql".to_owned(),
                    sql.as_bytes().to_vec(),
                ),
            ])
            .collect::<Vec<_>>()
    };
    let error = fixture::try_generate_with_sql(&fixture::catalog(), &manifest, &sources(one_table))
        .expect_err("generation writes this query");
    assert_eq!(error.path(), Some(path));
    let grouped = one_table.replace("ORDER BY", "GROUP BY model.id ORDER BY");
    fixture::try_generate_with_sql(&fixture::catalog(), &manifest, &sources(&grouped))
        .expect("a grouped query stays authored");
}

/// A screen follows its read through the sync store only when the read has a
/// feed. The contract carries the field, and the build names a read without one.
#[test]
#[ignore = "requires: input:fixture"]
fn a_sync_screen_needs_a_feed_on_its_read() {
    let mut manifest = fixture::manifest();
    manifest["custom_operations"]["feed_line.screen"]["sync"] = json!(true);
    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    assert!(package.operations()["feed_line.screen"].operation.sync);
    let silent = fixture::generate_fixture();
    assert!(!silent.operations()["feed_line.screen"].operation.sync);

    manifest["custom_operations"]["widget.list"]["sync"] = json!(true);
    let error = fixture::try_generate_with(&fixture::catalog(), &manifest)
        .expect_err("a read without a feed takes no sync screen");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
}

#[test]
#[ignore = "requires: input:fixture"]
fn custom_statement_paths_must_be_unique() {
    let mut document = fixture::manifest();
    let operation = document["custom_operations"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .find(|operation| {
            operation["statements"]
                .as_object()
                .is_some_and(|statements| !statements.is_empty())
        })
        .unwrap();
    let statements = operation["statements"].as_object_mut().unwrap();
    let duplicate = statements.values().next().unwrap().clone();
    statements.insert("duplicate".to_owned(), duplicate);
    assert!(fixture::try_generate_with(&fixture::catalog(), &document).is_err());
}

#[test]
#[ignore = "requires: input:fixture"]
fn internal_relations_are_not_models_and_their_vocabulary_is_closed() {
    let mut overlap = with_internal_relation();
    overlap["internal_relations"][fixture::UNREAD_TABLE]["table"] = json!("widget");
    let bytes = serde_json::to_vec(&overlap).unwrap();
    let error = wamn_compiler::generate(&wamn_compiler::GenerationInput::new(
        &fixture::catalog_with_unread_table(),
        &bytes,
        &[],
        &wamn_compiler::StatementTransactionality::from_paths(std::collections::BTreeMap::new()),
    ))
    .expect_err("model/internal relation overlap was accepted");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidManifest);

    let mut open = with_internal_relation();
    open["internal_relations"][fixture::UNREAD_TABLE]["cdc"] = json!("ignored");
    assert!(fixture::try_generate_with(&fixture::catalog_with_unread_table(), &open).is_err());
}
