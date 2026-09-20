use serde_json::{Value, json};
use wamn_schema_generator::{
    GenerateErrorKind, PackageManifest, validate_operation_vocabulary, validate_parity_json,
};
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
fn state_idempotence_carries_its_contract_and_requires_its_exact_shape() {
    let package = fixture::generate_fixture();
    assert!(
        fixture::contracts(&package).contains_key("widget/archive.state-tests.json"),
        "the state contract is available to descriptor consumers"
    );
    assert_eq!(
        artifact(
            &package,
            "generated/contracts/widget/archive.state-tests.json"
        ),
        json!({
            "operation": "platform-fixture:widget/archive@1.0.0",
            "law": "command-idempotence-from-state",
            "guards": [{
                "relation": "inventory.widget",
                "expected_version": "expected_edit_version"
            }],
            "cases": [{
                "id": "a_repeat_is_a_no_op_or_a_typed_conflict",
                "given": "the same request sent again after the first call succeeded",
                "expect": {
                    "writes": "none",
                    "outcome": "unchanged_original_or_refusal",
                    "refusal": "concurrency_conflict",
                    "second_write": "never",
                    "identity_minted": "none"
                }
            }]
        })
    );

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
    assert!(
        package
            .file("generated/models/widget_command.json")
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
    let package = fixture::generate_with(&fixture::catalog(), &ownership_only);
    assert!(
        package
            .file("generated/models/command_state.json")
            .is_some()
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
            json!({"path": "line[].purchase_order_line_id", "type": "uuid", "nullable": false}),
            json!({"path": "line[].quantity", "type": "numeric", "nullable": false}),
        ]);
    lined["custom_operations"]["widget.archive"]["canonicalization"]["line_order"] =
        json!("purchase_order_line_id_ascending");
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

    let mut missing_quantity = lined;
    missing_quantity["custom_operations"]["widget.archive"]["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .retain(|field| field["path"] != "line[].quantity");
    assert_eq!(
        validate_operation_vocabulary(&parsed(&missing_quantity))
            .expect_err("canonical line profile omitted quantity")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn custom_statement_declarations_drive_parity_and_require_unique_paths() {
    let package = fixture::generate_fixture();
    let source_map = artifact(&package, "generated/source-map/widget_archive.json");
    let parity = artifact(&package, "generated/parity/widget_archive.json");
    validate_parity_json(
        package
            .file("generated/parity/widget_archive.json")
            .unwrap()
            .bytes(),
    )
    .unwrap();
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
    assert_eq!(parity["fields"].as_array().unwrap().len(), 2);
    assert_eq!(parity["accessor_binds"].as_array().unwrap().len(), 1);
    assert_eq!(contract_statement["path"], statement["path"]);
    assert_eq!(contract_statement["binds"], statement["parameters"]);
    assert_eq!(contract_statement["columns"], statement["row"]);
    assert!(
        contract_statement["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    for declared in statement["row"].as_array().unwrap() {
        let name = declared["name"].as_str().unwrap();
        let identity = format!("archive.{name}");
        let parity_field = parity["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["field"] == identity)
            .unwrap();
        for rows in [&source_map["native_rows"], &source_map["wamn_rows"]] {
            let field = rows[0]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .find(|field| field["name"] == name)
                .unwrap();
            let expected = if rows == &source_map["native_rows"] {
                &parity_field["native_rust"]
            } else {
                &parity_field["wamn_rust"]
            };
            assert_eq!(&field["type"], expected);
        }
        assert_eq!(parity_field["nullable"], declared["nullable"]);
    }
    for declared in statement["parameters"].as_array().unwrap() {
        let name = declared["name"].as_str().unwrap();
        let parity_bind = parity["accessor_binds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|bind| bind["accessor"] == "archive" && bind["parameter"] == name)
            .unwrap();
        let emitted = accessor["binds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|bind| bind["parameter"] == name)
            .unwrap();
        assert_eq!(emitted["native_rust"], parity_bind["native_rust"]);
        assert_eq!(emitted["wamn_rust"], parity_bind["wamn_rust"]);
        assert_eq!(emitted["nullable"], declared["nullable"]);
    }
    assert_eq!(
        source_map["native_bind_fixtures"][0]["type"],
        parity["accessor_binds"][0]["native_rust"]
    );

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
    let model = manifest["models"]["widget"].as_object_mut().unwrap();
    model.remove("audit_log");
    model.remove("delete_mode");
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
        "generated/parity/widget_archive.json",
    ] {
        assert!(
            composed.file(path).is_none(),
            "SQL-less composition emitted {path}"
        );
    }
    assert_eq!(
        artifact(&composed, "generated/source-map/widget_archive.json")["composition"]["alias"],
        "base"
    );

    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    let inherited = artifact(
        &package,
        "generated/contracts/widget/archive.inherited-tests.json",
    );
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
        inherited,
        json!({
            "operation": "decorator-fixture:widget/archive@1.0.0",
            "law": "command-identity-from-claim",
            "inherits": {
                "alias": "base",
                "package": "platform_fixture",
                "version": "1.0.0",
                "digest": format!("sha256:{}", "a".repeat(64)),
                "operation": "widget.archive",
            },
            "cases": [
                {
                    "id": "replay_returns_the_base_original",
                    "given": "the same idempotency_key with the same canonical_command",
                    "expect": {
                        "identity_source": "base_claim",
                        "base_result": "identical_to_the_base_first_call",
                        "result": "the_base_original_under_this_command_decoration",
                        "writes": "none",
                        "claim": "none_of_its_own",
                    },
                },
            ],
        })
    );
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
fn authored_claims_emit_the_law_and_require_exact_finalization() {
    let manifest = platform_claim::manifest();
    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    let contract = artifact(
        &package,
        "generated/contracts/widget/archive.claim-tests.json",
    );
    assert_eq!(
        contract,
        json!({
            "operation": "platform-fixture:widget/archive@1.0.0",
            "law": "command-identity-from-claim",
            "cases": [
                {
                    "id": "replay_returns_the_immutable_original",
                    "given": "the same idempotency_key with the same canonical_command",
                    "first_call": ["claim", "finalize"],
                    "second_call": ["claim", "replay"],
                    "expect": {
                        "claim": "no_row",
                        "canonical_command": "equal",
                        "result": "identical_to_the_first_call",
                        "writes": "none",
                        "identity_source": "claim",
                    },
                },
                {
                    "id": "changed_request_under_a_live_key_refuses",
                    "given": "the same idempotency_key with a changed canonical_command",
                    "first_call": ["claim", "finalize"],
                    "second_call": ["claim", "replay"],
                    "expect": {
                        "claim": "no_row",
                        "canonical_command": "differs",
                        "writes": "none",
                        "refusal": "idempotency_conflict",
                    },
                },
            ],
        })
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
