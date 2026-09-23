use serde_json::{Value, json};
use wamn_schema_generator::{GenerateErrorKind, PackageManifest, validate_operation_vocabulary};
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnType, Exclusion, ExclusionAccessMethod, ExclusionElement,
    ExclusionKey, Table,
};

#[path = "support/platform_fixture.rs"]
mod fixture;
#[path = "support/platform_claim.rs"]
mod platform_claim;
#[path = "support/platform_descriptors.rs"]
mod platform_descriptors;

#[path = "support/platform_wit.rs"]
mod platform_wit;

#[path = "support/platform_client_ir.rs"]
mod platform_client_ir;

#[path = "support/platform_protocol.rs"]
mod platform_protocol;

#[path = "support/platform_sqlx.rs"]
mod platform_sqlx;

#[path = "support/platform_errors.rs"]
mod platform_errors;

fn artifact(package: &wamn_schema_generator::GeneratedPackage, path: &str) -> Value {
    serde_json::from_slice(package.file(path).expect("generated artifact").bytes())
        .expect("generated JSON")
}

fn parsed(value: &Value) -> PackageManifest {
    PackageManifest::from_slice(&serde_json::to_vec(value).expect("serialize manifest"))
        .expect("parse manifest")
}

#[test]
fn public_freshness_and_business_error_details_are_closed() {
    let baseline = fixture::manifest();
    let baseline_package = fixture::generate_with(&fixture::catalog(), &baseline);
    let paths = [
        "generated/contracts/widget/get.operation.json",
        "generated/contracts/widget/archive.operation.json",
    ];

    for enabled in [false, true] {
        let mut manifest = baseline.clone();
        manifest["models"]["widget"]["operations"]["get"]["fresh_only"] = json!(enabled);
        manifest["custom_operations"]["widget.archive"]["fresh_only"] = json!(enabled);
        let package = fixture::generate_with(&fixture::catalog(), &manifest);
        for path in paths {
            let contract = artifact(&package, path);
            if enabled {
                assert_eq!(contract["fresh_only"], true, "{path}");
            } else {
                assert!(contract.get("fresh_only").is_none(), "{path}");
                assert_eq!(
                    package.file(path).unwrap().bytes(),
                    baseline_package.file(path).unwrap().bytes(),
                    "{path}"
                );
            }
        }
    }

    let mut platform_detail = baseline.clone();
    platform_detail["custom_operations"]["widget.archive"]["error_details"]["invalid_input"] =
        json!({"required": ["field"]});
    assert_eq!(
        validate_operation_vocabulary(&parsed(&platform_detail))
            .expect_err("platform error detail was authored")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut missing_business_detail = baseline;
    missing_business_detail["custom_operations"]["widget.archive"]["error_details"] = json!({});
    assert_eq!(
        validate_operation_vocabulary(&parsed(&missing_business_detail))
            .expect_err("business error detail was omitted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut private = fixture::manifest();
    let archive = private["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap();
    archive.insert("visibility".to_owned(), json!("private"));
    archive.remove("permission");
    archive["errors"]
        .as_array_mut()
        .unwrap()
        .retain(|error| error != "permission_denied");
    archive.insert("fresh_only".to_owned(), json!(true));
    assert_eq!(
        validate_operation_vocabulary(&parsed(&private))
            .expect_err("private operation required a fresh caller")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    for value in [json!(null), json!("true"), json!(1)] {
        let mut malformed = fixture::manifest();
        malformed["models"]["widget"]["operations"]["get"]["fresh_only"] = value;
        assert!(PackageManifest::from_slice(&serde_json::to_vec(&malformed).unwrap()).is_err());
    }
}

#[test]
fn authored_sql_access_must_match_declared_reads_and_row_locks() {
    for (field, value) in [
        ("select_fields", json!([])),
        ("lock", json!(false)),
        ("update_fields", json!(["edit_version"])),
    ] {
        let mut manifest = fixture::manifest();
        manifest["custom_operations"]["widget.archive"]["relations"][0][field] = value;
        let error = fixture::try_generate_with(&fixture::catalog(), &manifest)
            .expect_err("SQL authority mismatch was accepted");
        assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
        assert_eq!(error.object(), Some("inventory.widget"));
        let message = error.to_string();
        assert!(message.contains("privilege declaration does not match verified SQL"));
        assert!(message.contains("\"lock\":true"));
        assert!(message.contains("\"select_fields\":[\"edit_version\",\"id\"]"));
    }
}

#[test]
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
                .kind(),
            GenerateErrorKind::InvalidOperation
        );
    }

    let mut absent = fixture::manifest();
    absent["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap()
        .remove("idempotent_by");
    let error = validate_operation_vocabulary(&parsed(&absent))
        .expect_err("command without idempotence was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    for remedy in [
        "idempotent_by",
        "claim",
        "state",
        "guards",
        "inherited",
        "base",
    ] {
        assert!(
            error.to_string().contains(remedy),
            "missing {remedy}: {error}"
        );
    }
}

#[test]
fn internal_relations_are_not_models_and_their_vocabulary_is_closed() {
    let package = fixture::generate_fixture();
    assert!(package.file("generated/wamn/widget_command.rs").is_none());
    assert!(
        package
            .file("generated/native-verifier/widget_command.rs")
            .is_none()
    );

    let mut overlap = fixture::manifest();
    overlap["internal_relations"]["widget_command"]["table"] = json!("widget");
    let bytes = serde_json::to_vec(&overlap).unwrap();
    let error = wamn_schema_generator::generate(&wamn_schema_generator::GenerationInput::new(
        &fixture::catalog(),
        &bytes,
        &[],
        wamn_schema_generator::GenerationProvenance::new("test", "test"),
        &wamn_schema_generator::StatementTransactionality::default(),
    ))
    .expect_err("model/internal relation overlap was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);

    let mut open = fixture::manifest();
    open["internal_relations"]["widget_command"]["cdc"] = json!("ignored");
    assert!(PackageManifest::from_slice(&serde_json::to_vec(&open).unwrap()).is_err());
}

#[test]
fn an_unused_column_changes_verified_state_without_widening_the_contract() {
    let manifest = fixture::manifest();
    let catalog = fixture::catalog();
    let baseline = fixture::generate_with(&catalog, &manifest);
    let mut tables = catalog.tables().to_vec();
    let command = tables
        .iter_mut()
        .find(|table| table.name() == "widget_command")
        .expect("command relation");
    let mut columns = command.columns().to_vec();
    columns.push(Column::new(
        "unused_note",
        ColumnType::Text,
        true,
        None,
        None,
    ));
    *command = Table::new(
        command.schema(),
        command.name(),
        columns,
        command.constraints().to_vec(),
        command.indexes().to_vec(),
    );
    let additive = fixture::generate_with(&CatalogIr::new(tables), &manifest);
    let baseline = artifact(&baseline, "generated/package-weld.json");
    let additive = artifact(&additive, "generated/package-weld.json");
    assert_ne!(
        baseline["verified_schema_state_id"],
        additive["verified_schema_state_id"]
    );
    assert_eq!(
        baseline["required_schema_contract"],
        additive["required_schema_contract"]
    );
}

#[test]
fn ownership_only_models_and_exclusion_owners_are_exact() {
    let catalog = fixture::catalog();
    let widget = catalog
        .tables()
        .iter()
        .find(|table| table.name() == "widget")
        .unwrap();
    let excluded = Table::new(
        widget.schema(),
        widget.name(),
        widget.columns().to_vec(),
        widget.constraints().to_vec(),
        widget.indexes().to_vec(),
    )
    .with_exclusions(vec![
        Exclusion::new(
            "widget_code_excl",
            ExclusionAccessMethod::Gist,
            vec![ExclusionKey::new(ExclusionElement::column("code"), "=")],
            ["code"],
        )
        .unwrap(),
    ]);
    let catalog = CatalogIr::new(
        catalog
            .tables()
            .iter()
            .map(|table| {
                if table.name() == "widget" {
                    excluded.clone()
                } else {
                    table.clone()
                }
            })
            .collect(),
    );

    let mut owned = fixture::manifest();
    owned["models"]["widget"]["constraint_owners"] =
        json!({"widget_code_excl": "platform_fixture"});
    let package = fixture::try_generate_with(&catalog, &owned).expect("package owns its exclusion");
    let errors = artifact(&package, "generated/contracts/widget/update.errors.json");
    assert!(errors["cases"].as_array().unwrap().iter().any(|case| {
        case["literal"] == "exclusion_violation"
            && case["constraint"] == "widget_code_excl"
            && case["detail"]["required"] == json!(["constraint"])
    }));

    let mut dependency_owned = owned.clone();
    dependency_owned["base_dependencies"] = json!({"base": {
        "package": "base_fixture",
        "version": "1.0.0",
        "digest": format!("sha256:{}", "a".repeat(64)),
        "operations": ["widget.get"]
    }});
    dependency_owned["models"]["widget"]["constraint_owners"]["widget_code_excl"] =
        json!("base_fixture");
    fixture::try_generate_with(&catalog, &dependency_owned)
        .expect("declared base owns the exclusion");

    for (constraint, owner) in [
        ("missing_excl", "platform_fixture"),
        ("widget_code_excl", "undeclared_fixture"),
    ] {
        let mut invalid = owned.clone();
        invalid["models"]["widget"]["constraint_owners"] = json!({(constraint): owner});
        assert_eq!(
            fixture::try_generate_with(&catalog, &invalid)
                .expect_err("invalid exclusion ownership was accepted")
                .kind(),
            GenerateErrorKind::InvalidModel
        );
    }

    let mut ownership_only = fixture::manifest();
    ownership_only["models"]["command_state"] = json!({
        "schema": "inventory",
        "table": "widget_command",
        "owner": "platform_fixture",
        "server_owned_fields": ["widget_id"],
        "audit_log": {"columns": [], "retention": "none"},
        "operations": {}
    });
    ownership_only["internal_relations"] = json!({});
    ownership_only["models"]["widget"]["operations"]
        .as_object_mut()
        .unwrap()
        .remove("create");
    // The claim table is a model here, so no operation can claim it.
    ownership_only["custom_operations"]
        .as_object_mut()
        .unwrap()
        .remove("widget.record_batch");
    let package = fixture::generate_with(&fixture::catalog(), &ownership_only);
    assert!(package.file("generated/wamn/command_state.rs").is_none());
    assert!(
        package
            .file("generated/native-verifier/command_state.rs")
            .is_none()
    );
    let metadata = artifact(&package, "generated/package-weld.json");
    assert!(
        metadata["required_schema_contract"]["tables"]
            .as_array()
            .unwrap()
            .iter()
            .any(|table| table["table"] == "widget_command")
    );
    for action in ["get", "query", "create", "update", "delete"] {
        assert!(
            package
                .file(&format!(
                    "generated/contracts/command_state/{action}.operation.json"
                ))
                .is_none()
        );
    }
}

#[test]
fn event_registration_and_line_profiles_are_closed() {
    let mut handler = fixture::manifest();
    let operation = handler["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap();
    operation.insert("kind".to_owned(), json!("event_handler"));
    operation.insert("visibility".to_owned(), json!("private"));
    operation.remove("permission");
    operation.remove("result");
    operation.remove("transaction");
    operation.remove("automatic_retry");
    operation.remove("idempotent_by");
    operation.remove("fresh_only");
    operation["errors"]
        .as_array_mut()
        .unwrap()
        .retain(|error| error != "permission_denied" && error != "concurrency_conflict");
    operation.insert(
        "registration".to_owned(),
        json!({"source_package": "platform_fixture", "entity": "widget", "ops": ["insert"]}),
    );
    validate_operation_vocabulary(&parsed(&handler)).expect("valid event registration");

    for registration in [
        Value::Null,
        json!({
            "source_package": "platform_fixture", "entity": "widget", "ops": []
        }),
        json!({
            "source_package": "platform_fixture", "entity": "widget", "ops": ["insert", "insert"]
        }),
    ] {
        let mut invalid = handler.clone();
        if registration.is_null() {
            invalid["custom_operations"]["widget.archive"]
                .as_object_mut()
                .unwrap()
                .remove("registration");
        } else {
            invalid["custom_operations"]["widget.archive"]["registration"] = registration;
        }
        assert_eq!(
            validate_operation_vocabulary(&parsed(&invalid))
                .expect_err("invalid event registration was accepted")
                .kind(),
            GenerateErrorKind::InvalidOperation
        );
    }

    let mut lineless = fixture::manifest();
    lineless["custom_operations"]["widget.archive"]["canonicalization"] =
        json!({"excluded_fields": ["id"]});
    validate_operation_vocabulary(&parsed(&lineless)).expect("lineless canonicalization");

    let mut lined = lineless;
    lined["custom_operations"]["widget.archive"]["input"]["line"] =
        json!({"minimum": 1, "maximum": 10});
    lined["custom_operations"]["widget.archive"]["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({"path": "line[].entry_id", "type": "uuid", "nullable": false}),
            json!({"path": "line[].amount", "type": "numeric", "nullable": false}),
        ]);
    lined["custom_operations"]["widget.archive"]["canonicalization"]["line_order"] =
        json!({"ascending_by": "entry_id", "positive_member": "amount"});
    validate_operation_vocabulary(&parsed(&lined)).expect("valid line profile");
    for field in ["line", "line_order"] {
        let mut invalid = lined.clone();
        let object = if field == "line" {
            invalid["custom_operations"]["widget.archive"]["input"]
                .as_object_mut()
                .unwrap()
        } else {
            invalid["custom_operations"]["widget.archive"]["canonicalization"]
                .as_object_mut()
                .unwrap()
        };
        object.remove(field);
        assert_eq!(
            validate_operation_vocabulary(&parsed(&invalid))
                .expect_err("line input/profile mismatch was accepted")
                .kind(),
            GenerateErrorKind::InvalidOperation
        );
    }

    let mut missing_positive = lined.clone();
    missing_positive["custom_operations"]["widget.archive"]["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .retain(|field| field["path"] != "line[].amount");
    assert_eq!(
        validate_operation_vocabulary(&parsed(&missing_positive))
            .expect_err("canonical line profile omitted its positive member")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    // The declared member decides. A profile that orders by a member the
    // command does not declare is refused, whatever the member is called.
    let mut unknown_member = lined.clone();
    unknown_member["custom_operations"]["widget.archive"]["canonicalization"]["line_order"] =
        json!({"ascending_by": "gadget_line_id", "positive_member": "amount"});
    assert_eq!(
        validate_operation_vocabulary(&parsed(&unknown_member))
            .expect_err("canonical line profile ordered by an undeclared member")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    // A command with lines and no positive member is valid, and it refuses no
    // line for being zero.
    let mut no_positive = lined;
    no_positive["custom_operations"]["widget.archive"]["canonicalization"]["line_order"] =
        json!({"ascending_by": "entry_id"});
    validate_operation_vocabulary(&parsed(&no_positive)).expect("line profile with no quantity");
}

#[test]
fn custom_statement_declarations_drive_projections_and_require_unique_paths() {
    let package = fixture::generate_fixture();
    let source_map = artifact(&package, "generated/source-map/widget_archive.json");
    let statement = &source_map["statements"]["archive"];
    let accessor = &source_map["wamn_accessors"][0];
    let operation = artifact(
        &package,
        "generated/contracts/widget/archive.operation.json",
    );
    let contract_statement = operation["statements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["name"] == "archive")
        .unwrap();
    assert_eq!(accessor["name"], "archive");
    assert_eq!(
        accessor["binds"][0]["parameter"],
        statement["parameters"][0]["name"]
    );
    assert_eq!(
        accessor["binds"][0]["nullable"],
        statement["parameters"][0]["nullable"]
    );
    assert_eq!(
        source_map["native_rows"][0]["fields"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        source_map["wamn_rows"][0]["fields"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        source_map["native_rows"][0]["fields"],
        json!([
            {"name": "id", "type": "uuid::Uuid"},
            {"name": "edit_version", "type": "i64"}
        ])
    );
    assert_eq!(
        source_map["wamn_rows"][0]["fields"],
        json!([
            {"name": "id", "type": "wamn_postgres_statements::Uuid"},
            {"name": "edit_version", "type": "i64"}
        ])
    );
    assert_eq!(contract_statement["path"], statement["path"]);
    assert_eq!(contract_statement["binds"], statement["parameters"]);
    assert_eq!(contract_statement["columns"], statement["row"]);
    assert!(
        contract_statement["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    for declared in statement["parameters"].as_array().unwrap() {
        let name = declared["name"].as_str().unwrap();
        let emitted = accessor["binds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|bind| bind["parameter"] == name)
            .unwrap();
        let fixture = source_map["native_bind_fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .find(|fixture| fixture["accessor"] == "archive" && fixture["parameter"] == name)
            .unwrap();
        assert_eq!(fixture["type"], emitted["native_rust"]);
        assert_eq!(emitted["nullable"], declared["nullable"]);
    }

    let mut renamed = fixture::manifest();
    renamed["custom_operations"]["widget.archive"]["statements"]["archive"]["parameters"][0] =
        json!({"name": "lookup_id", "type": "uuid", "nullable": true});
    renamed["custom_operations"]["widget.archive"]["statements"]["archive"]["row"][0] =
        json!({"name": "archived_id", "type": "text", "nullable": true});
    let package = fixture::generate_with(&fixture::catalog(), &renamed);
    let source_map = artifact(&package, "generated/source-map/widget_archive.json");
    assert_eq!(
        source_map["wamn_accessors"][0]["binds"][0]["parameter"],
        "lookup_id"
    );
    assert_eq!(
        source_map["wamn_accessors"][0]["binds"][0]["nullable"],
        true
    );
    assert_eq!(
        source_map["wamn_rows"][0]["fields"][0]["name"],
        "archived_id"
    );
    assert_eq!(
        source_map["wamn_rows"][0]["fields"][0]["type"],
        "Option<String>"
    );

    let mut duplicate = fixture::manifest();
    let statement =
        duplicate["custom_operations"]["widget.archive"]["statements"]["archive"].clone();
    duplicate["custom_operations"]["widget.archive"]["statements"]["duplicate"] = statement;
    assert_eq!(
        fixture::try_generate_with(&fixture::catalog(), &duplicate)
            .expect_err("two statements consumed one SQL path")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn inherited_composition_is_exact_and_carries_its_contract() {
    let mut manifest = fixture::manifest();
    manifest["package"]["id"] = json!("decorator_fixture");
    // An overlay owns no relation of the base package, so neither model
    // declares a record history here.
    let maker = manifest["models"]["widget_maker"].as_object_mut().unwrap();
    maker.remove("audit_log");
    let model = manifest["models"]["widget"].as_object_mut().unwrap();
    model.remove("audit_log");
    model.remove("delete_mode");
    // Only the package that owns a relation declares that its client may
    // extend it, so an overlay of this model declares nothing.
    model.remove("client_field_extensible");
    model["operations"]
        .as_object_mut()
        .unwrap()
        .remove("create");
    model["operations"]
        .as_object_mut()
        .unwrap()
        .remove("delete");
    manifest["base_dependencies"] = json!({"base": {
        "package": "platform_fixture",
        "version": "1.0.0",
        "digest": format!("sha256:{}", "a".repeat(64)),
        "operations": ["widget.archive"]
    }});
    manifest["custom_operations"]["widget.archive"]["idempotent_by"] =
        json!({"inherited": {"base": "base", "operation": "widget.archive"}});
    let mut sql_less = manifest.clone();
    let operation = sql_less["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap();
    for field in [
        "connection",
        "transaction",
        "automatic_retry",
        "relations",
        "statements",
    ] {
        operation.remove(field);
    }
    let composed = fixture::generate_with(&fixture::catalog(), &sql_less);
    for path in [
        "generated/native-verifier/widget_archive.rs",
        "generated/wamn/widget_archive.rs",
        "generated/source-map/widget_archive.json",
    ] {
        assert!(
            composed.file(path).is_none(),
            "SQL-less composition emitted {path}"
        );
    }
    assert_eq!(
        artifact(
            &composed,
            "generated/contracts/widget/archive.operation.json"
        )["dependency"]["alias"],
        "base"
    );

    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    for (field, value, kind) in [
        (
            "field_owners",
            json!({"missing": "decorator_fixture"}),
            GenerateErrorKind::UnknownColumn,
        ),
        (
            "field_owners",
            json!({"note": "undeclared_fixture"}),
            GenerateErrorKind::InvalidModel,
        ),
        (
            "constraint_owners",
            json!({"missing": "decorator_fixture"}),
            GenerateErrorKind::InvalidModel,
        ),
        (
            "client_field_extensible",
            json!(true),
            GenerateErrorKind::InvalidModel,
        ),
    ] {
        let mut invalid = manifest.clone();
        invalid["models"]["widget"][field] = value;
        assert_eq!(
            fixture::try_generate_with(&fixture::catalog(), &invalid)
                .expect_err(field)
                .kind(),
            kind
        );
    }
    assert_eq!(
        artifact(
            &package,
            "generated/contracts/widget/archive.operation.json"
        )["dependency"]["alias"],
        "base"
    );
    assert!(package.file("generated/wamn/widget_archive.rs").is_some());

    for (field, value) in [("version", json!("^1.0")), ("digest", json!("latest"))] {
        let mut invalid = manifest.clone();
        invalid["base_dependencies"]["base"][field] = value;
        assert_eq!(
            validate_operation_vocabulary(&parsed(&invalid))
                .expect_err("inexact dependency identity was accepted")
                .kind(),
            GenerateErrorKind::InvalidIdentity
        );
    }

    let mut missing = manifest.clone();
    missing["base_dependencies"] = json!({});
    assert_eq!(
        validate_operation_vocabulary(&parsed(&missing))
            .expect_err("undeclared inherited base was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut ambiguous = sql_less;
    ambiguous["base_dependencies"]["second"] = json!({
        "package": "other_fixture",
        "version": "1.0.0",
        "digest": format!("sha256:{}", "b".repeat(64)),
        "operations": ["widget.archive"]
    });
    assert_eq!(
        validate_operation_vocabulary(&parsed(&ambiguous))
            .expect_err("ambiguous composition dependency was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut writing = manifest;
    writing["custom_operations"]["widget.archive"]["relations"][0]["insert_fields"] = json!(["id"]);
    let error = validate_operation_vocabulary(&parsed(&writing))
        .expect_err("inherited command minted local state");
    assert!(error.to_string().contains("idempotent_by claim"));
}

#[test]
fn authored_claims_require_exact_finalization() {
    let manifest = platform_claim::manifest();
    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    let operation = artifact(
        &package,
        "generated/contracts/widget/archive.operation.json",
    );
    assert_eq!(operation["idempotent_by"], "claim");
    assert_eq!(
        operation["claim"],
        manifest["custom_operations"]["widget.archive"]["claim"]
    );
    for fetch in ["optional_one", "bounded_list"] {
        let mut invalid = manifest.clone();
        invalid["custom_operations"]["widget.archive"]["statements"]["finalize"]["fetch"] =
            json!(fetch);
        let error = fixture::try_generate_with(&fixture::catalog(), &invalid)
            .expect_err("claim finalizer did not require one row");
        assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
        assert!(error.to_string().contains("finalize fetch to one"));
    }

    for (pointer, value) in [
        (
            "/custom_operations/widget.archive/claim/table",
            json!("missing_command"),
        ),
        (
            "/custom_operations/widget.archive/claim/identities",
            json!({"id": "canonical_command"}),
        ),
        (
            "/custom_operations/widget.archive/claim/identities",
            json!({"missing": "widget_id"}),
        ),
        (
            "/custom_operations/widget.archive/statements/claim/row/0/name",
            json!("canonical_command"),
        ),
        (
            "/custom_operations/widget.archive/statements/replay/row/0/name",
            json!("other_command"),
        ),
    ] {
        let mut invalid = manifest.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            fixture::try_generate_with(&fixture::catalog(), &invalid)
                .expect_err("malformed authored claim was accepted")
                .kind(),
            GenerateErrorKind::InvalidOperation
        );
    }
    for (idempotence, keep_claim) in [
        (json!("claim"), false),
        (
            json!({"state": {"guards": {"widget_command": "payload"}}}),
            true,
        ),
        (
            json!({"state": {"guards": {"widget_command": "payload"}}}),
            false,
        ),
    ] {
        let mut invalid = manifest.clone();
        let operation = invalid["custom_operations"]["widget.archive"]
            .as_object_mut()
            .unwrap();
        operation.insert("idempotent_by".into(), idempotence);
        if !keep_claim {
            operation.remove("claim");
        }
        assert_eq!(
            validate_operation_vocabulary(&parsed(&invalid))
                .unwrap_err()
                .kind(),
            GenerateErrorKind::InvalidOperation
        );
    }
    let mut no_transaction = manifest;
    let operation = no_transaction["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap();
    operation.remove("transaction");
    operation.remove("automatic_retry");
    let error = validate_operation_vocabulary(&parsed(&no_transaction)).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert!(
        error
            .to_string()
            .contains("transaction: explicit_per_input")
    );
    assert!(error.to_string().contains("automatic_retry: false"));
}

/// Whether a refusal names the member that caused it. The top line states the
/// closed vocabulary, and the parser's own message underneath names the key.
fn names_the_member(error: &wamn_schema_generator::GenerateError, member: &str) -> bool {
    std::error::Error::source(error).is_some_and(|source| source.to_string().contains(member))
}

/// EXIT GATE for `wamn-c2y5.1`: authored text is optional everywhere, it is
/// carried where the author wrote it, and a misspelled key is refused.
///
/// The refusal matters more than the parse. A label that silently disappears
/// because its key is `labels` would reach an operator as a field path, and
/// nothing would say why.
#[test]
fn authored_text_is_optional_and_its_key_is_closed() {
    let declared = parsed(&fixture::manifest());
    let widget = &declared.models["widget"];
    assert_eq!(
        widget.field_text["code"].label.as_deref(),
        Some("Widget code")
    );
    assert!(widget.field_text["code"].description.is_some());
    assert_eq!(
        widget.field_text["note"].label.as_deref(),
        Some("Operator note")
    );
    assert_eq!(
        widget.field_text["note"].description, None,
        "a label alone is a complete declaration"
    );
    assert_eq!(
        widget.operations[&wamn_schema_generator::CrudAction::Query]
            .label
            .as_deref(),
        Some("Find widgets")
    );
    let batch = &declared.custom_operations["widget.record_batch"];
    assert_eq!(batch.label.as_deref(), Some("Record a batch"));
    assert!(batch.description.is_some());
    let line = batch
        .input
        .fields
        .iter()
        .find(|field| field.path == "value.line[].amount")
        .expect("the repeated line amount");
    assert_eq!(line.label.as_deref(), Some("Quantity received"));

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
    assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);
    assert!(names_the_member(&error, "labels"), "{error}");

    let mut field_text = fixture::manifest();
    field_text["custom_operations"]["widget.record_batch"]["input"]["fields"][0]["labels"] =
        json!("Request");
    let error =
        PackageManifest::from_slice(&serde_json::to_vec(&field_text).expect("serialize manifest"))
            .expect_err("a misspelled field label key was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);
    assert!(names_the_member(&error, "labels"), "{error}");

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
        assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);
        assert!(names_the_member(&error, "labels"), "{error}");
    }
}

/// Every JSON object under `value`, including `value` itself.
fn objects(value: &Value) -> Vec<&serde_json::Map<String, Value>> {
    match value {
        Value::Object(members) => std::iter::once(members)
            .chain(members.values().flat_map(objects))
            .collect(),
        Value::Array(items) => items.iter().flat_map(objects).collect(),
        _ => Vec::new(),
    }
}

/// EXIT GATE for `wamn-c2y5.2`: the contract carries the authored text to the
/// only reader that matters, and it carries nothing when nobody authored any.
///
/// The silence half is the load-bearing one. A member written as `null` or as
/// an empty string would move the contract bytes of three applications that
/// author nothing, and with them every hash those bytes feed.
#[test]
fn a_contract_carries_the_authored_text_and_nothing_else() {
    let package = fixture::generate_fixture();

    // A generated action declares no field, so its result reads the column.
    let result = artifact(&package, "generated/contracts/widget/get.result.json");
    let code = result["fields"]
        .as_array()
        .expect("result fields")
        .iter()
        .find(|field| field["path"] == "code")
        .expect("the code column");
    assert_eq!(code["label"], "Widget code");
    assert!(code["description"].is_string());
    let identity = result["fields"]
        .as_array()
        .expect("result fields")
        .iter()
        .find(|field| field["path"] == "id")
        .expect("the id column");
    assert_eq!(
        identity.get("label"),
        None,
        "a column with no authored text carries none"
    );

    // An update states the same column at the path its input uses.
    let update = artifact(&package, "generated/contracts/widget/update.input.json");
    let writable = update["writable_fields"]
        .as_array()
        .expect("writable fields")
        .iter()
        .find(|field| field["path"] == "change.code")
        .expect("the code control");
    assert_eq!(writable["label"], "Widget code");

    // A filter names a column, so it reads that column's text.
    let query = artifact(&package, "generated/contracts/widget/query.input.json");
    assert_eq!(query["filters"][0]["field"], "code");
    assert_eq!(query["filters"][0]["label"], "Widget code");
    let query_operation = artifact(&package, "generated/contracts/widget/query.operation.json");
    assert_eq!(query_operation["label"], "Find widgets");

    // An authored operation states its own text, on itself and on its fields.
    let batch = artifact(
        &package,
        "generated/contracts/widget/record_batch.operation.json",
    );
    assert_eq!(batch["label"], "Record a batch");
    assert!(batch["description"].is_string());
    let input = artifact(
        &package,
        "generated/contracts/widget/record_batch.input.json",
    );
    let quantity = input["fields"]
        .as_array()
        .expect("input fields")
        .iter()
        .find(|field| field["path"] == "value.line[].amount")
        .expect("the line amount");
    assert_eq!(quantity["label"], "Quantity received");

    // Nobody authored any: no contract file carries either member anywhere.
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
    let quiet = fixture::generate_with(&fixture::catalog(), &silent);
    for file in quiet.files() {
        let path = file.path();
        if !path.starts_with("generated/contracts/") {
            continue;
        }
        let contract: Value = serde_json::from_slice(file.bytes()).expect("generated JSON");
        for members in objects(&contract) {
            for key in ["label", "description"] {
                assert!(
                    !members.contains_key(key),
                    "{path} carries {key} although nothing was authored"
                );
            }
        }
    }
}

/// EXIT GATE for `wamn-rm14.1`: a package whose inputs name no record writes
/// no reference anywhere, and every read that serves rows states what it
/// lists whoever wrote it.
///
/// The silence matters for the same reason the authored text's did: a member
/// written as null would move the contract bytes of three applications that
/// declare nothing. The second half is the uniformity rule: one member, one
/// meaning, whether an author states it or generation derives it.
#[test]
fn a_contract_carries_no_reference_and_no_list_when_nobody_states_one() {
    let mut silent = fixture::manifest();
    for pointer in [
        "/custom_operations/widget.record_batch/input/fields",
        "/custom_operations/widget.list/input/fields",
        "/custom_operations/widget.list/result/fields",
    ] {
        for field in silent
            .pointer_mut(pointer)
            .expect("declared fields")
            .as_array_mut()
            .expect("declared fields")
        {
            field
                .as_object_mut()
                .expect("a field object")
                .remove("references");
        }
    }
    for operation in ["widget.list", "widget_maker.list"] {
        silent["custom_operations"][operation]
            .as_object_mut()
            .expect("an authored operation")
            .remove("lists");
    }
    // The one derived reference leaves with its column.
    for action in ["create", "update"] {
        let writable = silent["models"]["widget"]["operations"][action]["writable_fields"]
            .as_array_mut()
            .expect("writable fields");
        writable.retain(|field| field != "maker_id");
    }
    let quiet = fixture::generate_with(&fixture::catalog(), &silent);
    for file in quiet.files() {
        let path = file.path();
        if !path.starts_with("generated/contracts/") {
            continue;
        }
        let contract: Value = serde_json::from_slice(file.bytes()).expect("generated JSON");
        for members in objects(&contract) {
            assert!(
                !members.contains_key("references"),
                "{path} carries a reference although no input names a record"
            );
        }
    }

    // One member, one meaning. A generated query states what it lists with
    // nothing authored, and it states no display field, which is the default.
    let generated = artifact(&quiet, "generated/contracts/widget/query.operation.json");
    assert_eq!(generated["lists"]["model"], "widget");
    assert_eq!(generated["lists"]["key_field"], "id");
    assert_eq!(generated["lists"].get("display_field"), None);
    let authored = artifact(
        &fixture::generate_fixture(),
        "generated/contracts/widget/list.operation.json",
    );
    assert_eq!(authored["lists"]["model"], "widget");
    assert_eq!(authored["lists"]["display_field"], "code");
}

/// EXIT GATE for `wamn-rm14.6`: the envelope bound carries no screen text.
///
/// The line bound owns the repeated group a form renders. The envelope is the
/// submission itself, so text on it would be a member nothing reads, and
/// generation refuses it instead of accepting it.
#[test]
fn screen_text_on_the_envelope_bound_refuses_and_names_the_line_bound() {
    let mut misplaced = fixture::manifest();
    misplaced["custom_operations"]["widget.record_batch"]["input"]["envelope"]["label"] =
        json!("Batch");
    let error = fixture::try_generate_with(&fixture::catalog(), &misplaced)
        .expect_err("screen text on the envelope bound was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    let message = error.to_string();
    assert!(message.contains("widget.record_batch"), "{message}");
    assert!(message.contains("line bound"), "{message}");
}
