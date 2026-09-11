use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use wamn_execution_contract::canonical_json_bytes;
use wamn_schema_generator::{
    AuthoredSql, CrudAction, DATA_ACCESS_OVERLAY_PATH, GenerateErrorKind, GeneratedPackage,
    PackageManifest, PackageWeld, canonical_operation_identity, canonical_operation_prefix,
    corpus_sha256, validate_operation_vocabulary, validate_parity_json,
};
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnType, Constraint, Exclusion, ExclusionAccessMethod,
    ExclusionElement, ExclusionKey, Table,
};

#[path = "support/generation.rs"]
mod support;
use support::{
    QUERY_SOURCES, accessor_bind, artifact_json, assert_native_fixtures_match_parity, catalog,
    manifest, object_named, parsed_manifest, projection_operation, rebuilt_table, replacing_table,
    run, statement_digest, table,
};

fn wamn_accessor<'a>(source_map: &'a Value, name: &str) -> &'a Value {
    source_map["wamn_api"]["accessors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|accessor| accessor["name"] == name)
        .unwrap()
}

#[test]
fn strict_manifest_and_ir_references_fail_loudly() {
    let ir = catalog(false);
    let mut predecessor = manifest();
    predecessor["package"]["predecessor_version"] = json!("0.9.0");
    run(&ir, &predecessor, &QUERY_SOURCES)
        .expect("the optional predecessor version is in the closed manifest vocabulary");

    predecessor["package"]["predecessor_version"] = json!("1.0.0");
    assert_eq!(
        run(&ir, &predecessor, &QUERY_SOURCES)
            .expect_err("a package version cannot name itself as predecessor")
            .kind(),
        GenerateErrorKind::InvalidManifest
    );

    let mut unknown = manifest();
    unknown["future"] = json!(true);
    assert_eq!(
        run(&ir, &unknown, &QUERY_SOURCES).unwrap_err().kind(),
        GenerateErrorKind::InvalidManifest
    );

    let mut mismatch = manifest();
    mismatch["models"]["purchase_order"]["table"] = json!("missing");
    let error = run(&ir, &mismatch, &QUERY_SOURCES).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::UnknownRelation);
    assert_eq!(error.object(), Some("receiving.missing"));

    let mut unknown_action = manifest();
    unknown_action["models"]["purchase_order"]["operations"]["merge"] =
        json!({"permission": "purchase_order.merge", "result": "one"});
    assert_eq!(
        run(&ir, &unknown_action, &QUERY_SOURCES)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidManifest
    );
}

#[test]
fn custom_sql_generated_rust_symbols_are_unique_before_emission() {
    let mut row_collision = manifest();
    let mut operation = projection_operation();
    let statement = operation["statements"]["load_purchase_order_detail"].clone();
    operation["statements"] = json!({
        "foo1": statement,
        "foo_1": {
            "path": "query/other_purchase_order_detail.sql",
            "fetch": "optional_one",
            "parameters": [{"name": "id", "type": "uuid", "nullable": false}],
            "row": [{"name": "id", "type": "uuid", "nullable": false}]
        }
    });
    row_collision["custom_operations"]["quality.load_purchase_order_detail"] = operation;
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&row_collision))
            .expect_err("two statements collapsed to one Rust row symbol")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut fixture_collision = manifest();
    let mut operation = projection_operation();
    operation["statements"] = json!({
        "foo": {
            "path": "query/foo.sql",
            "fetch": "optional_one",
            "parameters": [{"name": "bar_baz", "type": "uuid", "nullable": false}],
            "row": [{"name": "id", "type": "uuid", "nullable": false}]
        },
        "foo_bar": {
            "path": "query/foo_bar.sql",
            "fetch": "optional_one",
            "parameters": [{"name": "baz", "type": "uuid", "nullable": false}],
            "row": [{"name": "id", "type": "uuid", "nullable": false}]
        }
    });
    fixture_collision["custom_operations"]["quality.load_purchase_order_detail"] = operation;
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&fixture_collision))
            .expect_err("two statement parameters collapsed to one Rust fixture symbol")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn component_grouping_defaults_one_group_and_refuses_invalid_splits() {
    let single = parsed_manifest(&manifest());
    assert_eq!(
        validate_operation_vocabulary(&single).unwrap(),
        BTreeSet::from([
            "purchase_order.get".to_owned(),
            "purchase_order.query".to_owned(),
            "purchase_order.update".to_owned(),
        ])
    );
    assert!(
        single.models["purchase_order"].operations[&CrudAction::Get]
            .component
            .is_none()
    );

    let mut empty = manifest();
    empty["models"]["purchase_order"]["operations"]["get"]["component"] = json!("");
    let error = validate_operation_vocabulary(&parsed_manifest(&empty))
        .expect_err("an empty component name was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation purchase_order.get component must not be empty"
    );

    let mut unknown = manifest();
    unknown["models"]["purchase_order"]["operations"]["get"]["component"] = json!("missing");
    let error = validate_operation_vocabulary(&parsed_manifest(&unknown))
        .expect_err("an unknown component was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation purchase_order.get references unknown component missing"
    );

    let mut identical = manifest();
    identical["components"]["duplicate"] = json!({"connections": ["postgres"]});
    for operation in ["get", "query", "update"] {
        identical["models"]["purchase_order"]["operations"][operation]["component"] =
            json!(if operation == "query" {
                "duplicate"
            } else {
                "receiving"
            });
    }
    let error = validate_operation_vocabulary(&parsed_manifest(&identical))
        .expect_err("an identical-requirement split was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "components duplicate and receiving have identical requirement sets"
    );

    let mut distinct = manifest();
    distinct["connections"]["reporting"] = json!({"interface": "wamn:postgres@0.1.0"});
    distinct["components"]["reporting"] = json!({"connections": ["reporting"]});
    distinct["models"]["purchase_order"]["operations"]["query"]["component"] = json!("reporting");
    let error = validate_operation_vocabulary(&parsed_manifest(&distinct))
        .expect_err("a multi-component manifest omitted explicit operation grouping");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation purchase_order.get must name a component when the manifest declares multiple components"
    );

    for operation in ["get", "update"] {
        distinct["models"]["purchase_order"]["operations"][operation]["component"] =
            json!("receiving");
    }
    validate_operation_vocabulary(&parsed_manifest(&distinct))
        .expect("distinct requirement groups with explicit operation membership");

    let mut old_grouping = manifest();
    old_grouping["components"]["receiving"]["operations"] = json!(["purchase_order.get"]);
    assert!(PackageManifest::from_slice(&serde_json::to_vec(&old_grouping).unwrap()).is_err());
}

#[test]
fn shared_operation_vocabulary_refuses_permission_identity_drift() {
    let mut mismatch = manifest();
    mismatch["models"]["purchase_order"]["operations"]["get"]["permission"] =
        json!("purchase_order.query");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&mismatch))
            .expect_err("permission identity drift was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
    assert_eq!(
        run(&catalog(false), &mismatch, &QUERY_SOURCES)
            .expect_err("generation accepted permission identity drift")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn shared_operation_vocabulary_refuses_noncanonical_coordinates() {
    let mut package = manifest();
    package["package"]["id"] = json!("wamn-Receiving");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&package))
            .expect_err("noncanonical package id was accepted")
            .kind(),
        GenerateErrorKind::InvalidIdentity
    );

    for ambiguous in ["1.0.0:shadow", "1.0.0/shadow", "1.0.0@shadow"] {
        let mut version = manifest();
        version["package"]["version"] = json!(ambiguous);
        assert_eq!(
            validate_operation_vocabulary(&parsed_manifest(&version))
                .expect_err("ambiguous package version was accepted")
                .kind(),
            GenerateErrorKind::InvalidIdentity
        );
    }
}

#[test]
fn mutation_contract_refuses_server_owned_and_nonnullable_null() {
    let ir = catalog(false);
    let mut server_owned = manifest();
    server_owned["models"]["purchase_order"]["operations"]["update"]["writable_fields"] =
        json!(["status"]);
    assert_eq!(
        run(&ir, &server_owned, &QUERY_SOURCES).unwrap_err().kind(),
        GenerateErrorKind::InvalidOperation
    );

    let package = run(&ir, &manifest(), &QUERY_SOURCES).unwrap();
    let input = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.input.json",
    );
    assert_eq!(
        input["expected_row_version"],
        json!({"field": "row_version", "type": "int64", "required": true})
    );
    assert_eq!(input["writable_fields"][0]["field"], "supplier_id");
    assert_eq!(
        input["writable_fields"][0]["explicit_null"],
        "invalid_input"
    );
    assert_eq!(input["server_owned_fields"]["if_supplied"], "invalid_input");
}

#[test]
fn operation_identity_errors_and_constraint_names_are_closed() {
    let package_manifest = parsed_manifest(&manifest());
    assert_eq!(
        canonical_operation_prefix(&package_manifest.package).unwrap(),
        "wamn-receiving:"
    );
    assert_eq!(
        canonical_operation_identity(&package_manifest.package, "receiving.record_receipt")
            .unwrap(),
        "wamn-receiving:receiving/record-receipt@1.0.0"
    );
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let operation = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.operation.json",
    );
    assert_eq!(operation["permission_token"], "purchase_order.update");
    assert_eq!(
        operation["grant"],
        "wamn-receiving:purchase-order/update@1.0.0"
    );
    assert_eq!(operation["automatic_retry"], false);

    let errors = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.errors.json",
    );
    assert_eq!(errors["closed"], true);
    let cases = errors["cases"].as_array().unwrap();
    assert!(cases.iter().any(|case| case["literal"] == "invalid_input"));
    assert!(
        cases
            .iter()
            .any(|case| case["literal"] == "concurrency_conflict")
    );
    assert!(cases.iter().any(|case| {
        case["literal"] == "retry"
            && case["from"] == json!(["serialization_failure", "connection_unavailable"])
            && case["automatic"] == false
    }));
    assert!(
        cases
            .iter()
            .any(|case| { case["literal"] == "timeout" && case["from"] == "statement_timeout" })
    );
    assert!(cases.iter().any(|case| {
        case["literal"] == "permission_denied" && case["from"] == "permission_denied"
    }));
    assert!(cases.iter().any(|case| {
        case["literal"] == "internal_error"
            && case["from"] == json!(["query_error", "row_limit_exceeded"])
            && case["detail"] == json!({})
    }));
    let named_constraint_cases = cases
        .iter()
        .filter(|case| case.get("constraint").is_some())
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(named_constraint_cases, Vec::<Value>::new());
}

#[test]
fn generation_is_byte_stable_and_emits_both_projection_siblings() {
    let ir = catalog(false);
    let first = run(&ir, &manifest(), &QUERY_SOURCES).unwrap();
    let second = run(&ir, &manifest(), &QUERY_SOURCES).unwrap();
    assert_eq!(first, second);
    assert!(
        first
            .files()
            .windows(2)
            .all(|pair| pair[0].path() < pair[1].path())
    );

    first
        .file("generated/native-verifier/purchase_order.rs")
        .unwrap();
    first.file("generated/wamn/purchase_order.rs").unwrap();
    assert!(first.file("query/open_purchase_order.sql").is_none());

    let parity_file = first.file("generated/parity/purchase_order.json").unwrap();
    validate_parity_json(parity_file.bytes()).unwrap();
    let parity: Value = serde_json::from_slice(parity_file.bytes()).unwrap();
    assert_eq!(parity["rule"], "same_sql_file_two_projection_structs");
    let source_map = artifact_json(&first, "generated/source-map/purchase_order.json");
    assert_eq!(
        source_map["relation"],
        "catalog-ir://receiving.purchase_order"
    );
}

/// A result CLASS says how many rows come back, not what is in them. Every
/// generated operation therefore ships a result CONTRACT beside its class, and
/// that contract carries the model's closed value domains — without it the
/// result survives only as statement columns, and a control rendering `status`
/// gets a free-text box where a choice belongs.
#[test]
fn generated_operations_ship_a_result_contract_with_closed_domains() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    for action in ["get", "query", "update"] {
        let operation = artifact_json(
            &package,
            &format!("generated/contracts/purchase_order/{action}.operation.json"),
        );
        let result = artifact_json(
            &package,
            &format!("generated/contracts/purchase_order/{action}.result.json"),
        );
        assert_eq!(
            result["class"], operation["result"],
            "{action} result contract disagrees with its declared class"
        );
        let status = result["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["path"] == "status")
            .unwrap_or_else(|| panic!("{action} projects status"));
        assert_eq!(
            status["values"],
            json!(["open", "complete", "cancelled"]),
            "{action} dropped the closed domain the model declares"
        );
    }

    assert_eq!(
        artifact_json(
            &package,
            "generated/contracts/purchase_order/get.result.json"
        ),
        json!({
            "class": "one",
            "fields": [
                {"path": "created_at", "type": "timestamptz", "nullable": false, "values": []},
                {"path": "id", "type": "uuid", "nullable": false, "values": []},
                {
                    "path": "purchase_order_number",
                    "type": "text",
                    "nullable": false,
                    "values": []
                },
                {"path": "row_version", "type": "int64", "nullable": false, "values": []},
                {
                    "path": "status",
                    "type": "text",
                    "nullable": false,
                    "values": ["open", "complete", "cancelled"]
                },
                {"path": "supplier_id", "type": "uuid", "nullable": false, "values": []}
            ]
        })
    );

    let update = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.result.json",
    );
    assert_eq!(
        update,
        artifact_json(
            &package,
            "generated/contracts/purchase_order/get.result.json"
        ),
        "a successful update returns the public model row"
    );
    let operation = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.operation.json",
    );
    let columns = operation["statements"][0]["columns"].as_array().unwrap();
    for name in ["outcome", "observed_row_version"] {
        assert!(
            columns.iter().any(|column| column["name"] == name),
            "the SQL accessor still needs {name} to resolve success or refusal"
        );
    }
}

#[test]
fn update_returning_names_only_the_declared_model_columns() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let sql = std::str::from_utf8(
        package
            .file("generated/sql/purchase_order/update.sql")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    let returning = sql
        .split_once("    RETURNING\n")
        .unwrap()
        .1
        .split_once("\n)\nSELECT")
        .unwrap()
        .0;
    assert_eq!(
        returning,
        concat!(
            "    model.created_at,\n    model.id,\n    model.purchase_order_number,\n",
            "    model.row_version,\n    model.status,\n    model.supplier_id",
        )
    );
}

#[test]
fn wamn_accessors_are_structurally_derived_from_operations_and_ir() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let get_contract = artifact_json(
        &package,
        "generated/contracts/purchase_order/get.operation.json",
    );
    assert!(get_contract.get("sql_files").is_none());
    let get_statement = &get_contract["statements"][0];
    assert_eq!(get_statement["name"], "get");
    let get_path = get_statement["path"].as_str().unwrap();
    assert_eq!(
        get_statement["digest"],
        statement_digest(package.file(get_path).unwrap().bytes())
    );
    assert_eq!(
        get_statement["binds"],
        json!([{"name": "id", "type": "uuid", "nullable": false}])
    );
    assert_eq!(
        get_statement["columns"],
        json!([
            {"name": "created_at", "type": "timestamptz", "nullable": false},
            {"name": "id", "type": "uuid", "nullable": false},
            {"name": "purchase_order_number", "type": "text", "nullable": false},
            {"name": "row_version", "type": "int64", "nullable": false},
            {"name": "status", "type": "text", "nullable": false},
            {"name": "supplier_id", "type": "uuid", "nullable": false}
        ])
    );
    let source_map = artifact_json(&package, "generated/source-map/purchase_order.json");
    let api = &source_map["wamn_api"];
    assert_eq!(api["statement_digest_visibility"], "crate");
    assert_eq!(
        api["mutation_constraints"],
        json!([{
            "operation": "update",
            "unique": {
                "constant": "UPDATE_UNIQUE_CONSTRAINTS",
                "visibility": "crate",
                "names": []
            },
            "foreign_key": {
                "constant": "UPDATE_FOREIGN_KEY_CONSTRAINTS",
                "visibility": "crate",
                "names": []
            },
            "check": {
                "constant": "UPDATE_CHECK_CONSTRAINTS",
                "visibility": "crate",
                "names": []
            },
            "exclusion": {
                "constant": "UPDATE_EXCLUSION_CONSTRAINTS",
                "visibility": "crate",
                "names": []
            }
        }])
    );
    assert_eq!(api["accessors"].as_array().unwrap().len(), 8);
    assert_eq!(
        wamn_accessor(&source_map, "get"),
        &json!({
            "name": "get",
            "visibility": "crate",
            "operation": "get",
            "statement_digest_constant": "GET_DIGEST",
            "row": "PurchaseOrderRow",
            "fetch": "optional",
            "binds": [
                accessor_bind(
                    "id",
                    "uuid",
                    false,
                    "uuid::Uuid",
                    "wamn_postgres_statements::Uuid"
                )
            ]
        })
    );

    for (name, digest_constant, cursor_postgres, native_cursor, wamn_cursor) in [
        (
            "query_purchase_order_number_ascending",
            "QUERY_0_DIGEST",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_purchase_order_number_descending",
            "QUERY_1_DIGEST",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_status_ascending",
            "QUERY_2_DIGEST",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_status_descending",
            "QUERY_3_DIGEST",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_created_at_ascending",
            "QUERY_4_DIGEST",
            "timestamptz",
            "Option<chrono::DateTime<chrono::Utc>>",
            "Option<wamn_postgres_statements::TimestampTz>",
        ),
        (
            "query_created_at_descending",
            "QUERY_5_DIGEST",
            "timestamptz",
            "Option<chrono::DateTime<chrono::Utc>>",
            "Option<wamn_postgres_statements::TimestampTz>",
        ),
    ] {
        assert_eq!(
            wamn_accessor(&source_map, name),
            &json!({
                "name": name,
                "visibility": "crate",
                "operation": "query",
                "statement_digest_constant": digest_constant,
                "row": "PurchaseOrderRow",
                "fetch": "all",
                "binds": [
                    accessor_bind(
                        "supplier_id_filter",
                        "jsonb",
                        true,
                        "Option<serde_json::Value>",
                        "Option<wamn_postgres_statements::Json>"
                    ),
                    accessor_bind(
                        "status_filter",
                        "jsonb",
                        true,
                        "Option<serde_json::Value>",
                        "Option<wamn_postgres_statements::Json>"
                    ),
                    accessor_bind("cursor_key", cursor_postgres, true, native_cursor, wamn_cursor),
                    accessor_bind(
                        "cursor_id",
                        "uuid",
                        true,
                        "Option<uuid::Uuid>",
                        "Option<wamn_postgres_statements::Uuid>"
                    ),
                    accessor_bind("limit", "int8", false, "i64", "i64")
                ]
            })
        );
    }

    assert_eq!(
        wamn_accessor(&source_map, "update"),
        &json!({
            "name": "update",
            "visibility": "crate",
            "operation": "update",
            "statement_digest_constant": "UPDATE_DIGEST",
            "row": "PurchaseOrderUpdateRow",
            "fetch": "one",
            "binds": [
                accessor_bind("id", "uuid", false, "uuid::Uuid", "wamn_postgres_statements::Uuid"),
                accessor_bind("expected_row_version", "int8", false, "i64", "i64"),
                accessor_bind("supplier_id_present", "boolean", false, "bool", "bool"),
                accessor_bind(
                    "supplier_id_value",
                    "uuid",
                    true,
                    "Option<uuid::Uuid>",
                    "Option<wamn_postgres_statements::Uuid>"
                )
            ]
        })
    );
    assert_eq!(
        api["operation_rows"],
        json!([{
            "name": "PurchaseOrderUpdateRow",
            "visibility": "public",
            "fields": [
                {"name": "outcome", "type": "Option<String>"},
                {"name": "observed_row_version", "type": "Option<i64>"},
                {"name": "created_at", "type": "Option<wamn_postgres_statements::TimestampTz>"},
                {"name": "id", "type": "Option<wamn_postgres_statements::Uuid>"},
                {"name": "purchase_order_number", "type": "Option<String>"},
                {"name": "row_version", "type": "Option<i64>"},
                {"name": "status", "type": "Option<String>"},
                {"name": "supplier_id", "type": "Option<wamn_postgres_statements::Uuid>"}
            ]
        }])
    );
    assert_eq!(
        source_map["native_operation_rows"],
        json!([{
            "name": "PurchaseOrderUpdateRow",
            "visibility": "public",
            "fields": [
                {"name": "outcome", "type": "Option<String>"},
                {"name": "observed_row_version", "type": "Option<i64>"},
                {"name": "created_at", "type": "Option<chrono::DateTime<chrono::Utc>>"},
                {"name": "id", "type": "Option<uuid::Uuid>"},
                {"name": "purchase_order_number", "type": "Option<String>"},
                {"name": "row_version", "type": "Option<i64>"},
                {"name": "status", "type": "Option<String>"},
                {"name": "supplier_id", "type": "Option<uuid::Uuid>"}
            ]
        }])
    );

    assert_native_fixtures_match_parity(&package, "purchase_order");
}

#[test]
fn weld_hashes_exact_ir_and_sql_but_contract_ignores_unused_tables() {
    let base_ir = catalog(false);
    let additive_ir = catalog(true);
    let base = run(&base_ir, &manifest(), &QUERY_SOURCES).unwrap();
    let additive = run(&additive_ir, &manifest(), &QUERY_SOURCES).unwrap();
    let base_weld = artifact_json(&base, "generated/package-weld.json");
    let additive_weld = artifact_json(&additive, "generated/package-weld.json");

    let expected_schema = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(canonical_json_bytes(
            &serde_json::to_value(&base_ir).unwrap()
        )))
    );
    assert_eq!(base_weld["verified_schema_state_id"], expected_schema);
    assert_ne!(
        base_weld["verified_schema_state_id"],
        additive_weld["verified_schema_state_id"]
    );
    assert_eq!(
        base_weld["required_schema_contract"],
        additive_weld["required_schema_contract"]
    );
    assert_eq!(
        base_weld["required_platform_policy_contract"],
        json!({"id": "receiving_data_access", "state": "unsatisfied"})
    );
    assert_eq!(
        base_weld["promotion_state"],
        "blocked_unsatisfied_policy_contract"
    );
    assert!(!base.weld().promotion_eligible());
}

#[test]
fn explicit_cdc_exclusion_is_a_required_relation_without_fabricated_fields() {
    let mut package_manifest = manifest();
    package_manifest["internal_relations"] = json!({
        "unused": {
            "schema": "receiving",
            "table": "unused",
            "cdc": "excluded"
        }
    });
    let package = run(&catalog(true), &package_manifest, &QUERY_SOURCES).unwrap();
    let weld = artifact_json(&package, "generated/package-weld.json");
    let required = weld["required_schema_contract"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|table| table["table"] == "unused")
        .expect("the explicit exclusion is part of the schema contract");
    assert_eq!(required["schema"], "receiving");
    assert_eq!(required["fields"], json!([]));
    assert_eq!(required["constraints"], json!([]));
}

#[test]
fn generated_weld_has_one_strict_canonical_reader() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let bytes = package
        .file("generated/package-weld.json")
        .expect("the generated package carries its weld")
        .bytes();
    assert_eq!(&PackageWeld::from_slice(bytes).unwrap(), package.weld());

    let mut alternate = bytes.to_vec();
    alternate.push(b'\n');
    assert_eq!(
        PackageWeld::from_slice(&alternate).unwrap_err().kind(),
        GenerateErrorKind::InvalidManifest
    );

    let mut unknown: Value = serde_json::from_slice(bytes).unwrap();
    unknown["extra"] = json!(true);
    assert_eq!(
        PackageWeld::from_slice(&canonical_json_bytes(&unknown))
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidManifest
    );

    let mut contradictory: Value = serde_json::from_slice(bytes).unwrap();
    contradictory["promotion_state"] = json!("eligible");
    assert_eq!(
        PackageWeld::from_slice(&canonical_json_bytes(&contradictory))
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidManifest
    );
}

#[test]
fn corpus_hash_uses_sorted_unambiguous_framing() {
    let first = corpus_sha256([("a", b"bc".as_slice()), ("ab", b"c".as_slice())]);
    let reversed = corpus_sha256([("ab", b"c".as_slice()), ("a", b"bc".as_slice())]);
    let ambiguous_without_lengths = corpus_sha256([("abc", b"".as_slice())]);
    assert_eq!(first, reversed);
    assert_ne!(first, ambiguous_without_lengths);
    assert_eq!(
        first,
        "sha256:c29ceb2bae87e8e60215cf078b051f981c665f88aa75ca510eaa99dbc0b3d00f"
    );
}

#[test]
fn ordered_filters_and_query_variants_remain_structural_and_finite() {
    let ir = catalog(false);
    let mut generated_manifest = manifest();
    generated_manifest["models"]["purchase_order"]["operations"]["query"]
        .as_object_mut()
        .unwrap()
        .remove("authored_sql");
    let package = run(&ir, &generated_manifest, &[]).unwrap();
    package
        .file("generated/sql/purchase_order/query_purchase_order_number_ascending.sql")
        .unwrap();
    package
        .file("generated/sql/purchase_order/query_created_at_descending.sql")
        .unwrap();
    let input = artifact_json(
        &package,
        "generated/contracts/purchase_order/query.input.json",
    );
    assert_eq!(
        input["filters"],
        json!([
            {"field": "supplier_id", "binding": "json_array", "type": "uuid"},
            {"field": "status", "binding": "json_array", "type": "text"}
        ])
    );

    let mut default_only = generated_manifest;
    default_only["models"]["purchase_order"]["operations"]["query"]
        .as_object_mut()
        .unwrap()
        .remove("sort");
    let package = run(&ir, &default_only, &[]).unwrap();
    let query_paths = package
        .files()
        .iter()
        .filter(|file| {
            file.path()
                .starts_with("generated/sql/purchase_order/query_")
        })
        .collect::<Vec<_>>();
    assert_eq!(query_paths.len(), 1);
    assert_eq!(
        query_paths[0].path(),
        "generated/sql/purchase_order/query_created_at_ascending.sql"
    );
}

#[test]
fn duplicate_filters_and_schema_qualified_authored_sql_refuse() {
    let ir = catalog(false);
    let mut duplicate = manifest();
    duplicate["models"]["purchase_order"]["operations"]["query"]["filters"] = json!([
        {"field": "status", "binding": "json_array"},
        {"field": "status", "binding": "json_array"}
    ]);
    assert_eq!(
        run(&ir, &duplicate, &QUERY_SOURCES).unwrap_err().kind(),
        GenerateErrorKind::InvalidOperation
    );

    let qualified = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_purchase_order.sql" {
            AuthoredSql::new(source.path(), b"SELECT * FROM receiving.purchase_order;\n")
        } else {
            source
        }
    });
    let error = run(&ir, &manifest(), &qualified).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::SchemaQualifiedSql);
    assert_eq!(error.path(), Some("query/open_purchase_order.sql"));

    let quoted = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_purchase_order.sql" {
            AuthoredSql::new(
                source.path(),
                b"SELECT * FROM \"receiving\".\"purchase_order\";\n",
            )
        } else {
            source
        }
    });
    assert_eq!(
        run(&ir, &manifest(), &quoted).unwrap_err().kind(),
        GenerateErrorKind::SchemaQualifiedSql
    );

    let inert = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_purchase_order.sql" {
            AuthoredSql::new(
                source.path(),
                b"-- receiving.purchase_order\nSELECT 'receiving.purchase_order', $$\"receiving\".\"purchase_order\"$$ /* receiving.purchase_order */;\n",
            )
        } else {
            source
        }
    });
    run(&ir, &manifest(), &inert).unwrap();
}

// ---------------------------------------------------------------------------
// Generated create: command-identity-from-claim.
//
// The law, ratified 2026-09-03: any identity a command creates comes from the
// CLAIM, not from the work. A create that let PostgreSQL default its row id
// would mint a SECOND id on replay -- a duplicate IDENTITY, which is real stock
// on a row nothing points at, not merely a duplicate row.
//
// These tests are the generator's proof. No package declares a create today, so
// the emitted statements have no in-cluster consumer yet; the executing proof
// is due on the first one.
// ---------------------------------------------------------------------------

const CLAIM_TABLE: &str = "purchase_order_command";

/// One labelled way to break the create's closed error vocabulary.
type ErrorVocabularyMutant = (&'static str, Box<dyn Fn(&mut Value)>);

fn claim_columns() -> Vec<Column> {
    vec![
        Column::new("canonical_command", ColumnType::Bytes, false, None, None),
        Column::new("idempotency_key", ColumnType::Text, false, None, None),
        Column::new(
            "purchase_order_id",
            ColumnType::Uuid,
            false,
            Some(ColumnDefault::GenRandomUuid),
            None,
        ),
    ]
}

fn claim_constraints() -> Vec<Constraint> {
    vec![
        Constraint::primary_key(
            "purchase_order_command_idempotency_key_pkey",
            ["idempotency_key"],
        )
        .unwrap(),
        Constraint::unique(
            "purchase_order_command_purchase_order_id_key",
            ["purchase_order_id"],
        )
        .unwrap(),
    ]
}

fn claim_catalog_with(model: Table, claim: Table) -> CatalogIr {
    CatalogIr::new(vec![model, claim])
}

fn claim_catalog() -> CatalogIr {
    let model = table(&catalog(false), "purchase_order").clone();
    claim_catalog_with(
        model,
        Table::new(
            "receiving",
            CLAIM_TABLE,
            claim_columns(),
            claim_constraints(),
            Vec::new(),
        ),
    )
}

fn claim_manifest() -> Value {
    let mut manifest = manifest();
    manifest["models"]["purchase_order"]["operations"]["create"] = json!({
        "permission": "purchase_order.create",
        "error_details": {
            "invalid_input": {"required": ["field"]},
            "idempotency_conflict": {"required": ["field"]},
            "unique_violation": {"required": ["constraint"]},
            "check_violation": {"required": ["constraint"]},
            "retry": {},
            "timeout": {},
            "permission_denied": {"required": ["operation"]},
            "internal_error": {}
        },
        "writable_fields": ["supplier_id"],
        "claim": {
            "table": CLAIM_TABLE,
            "identities": {"id": "purchase_order_id"}
        },
        "result": "one"
    });
    manifest["internal_relations"] = json!({
        CLAIM_TABLE: {"schema": "receiving", "table": CLAIM_TABLE, "cdc": "excluded"}
    });
    manifest
}

fn generated_create_sql(package: &GeneratedPackage, statement: &str) -> String {
    String::from_utf8(
        package
            .file(&format!("generated/sql/purchase_order/{statement}.sql"))
            .unwrap_or_else(|| panic!("{statement} was emitted"))
            .bytes()
            .to_vec(),
    )
    .unwrap()
}

/// EXIT GATE: every identity the create hands out is written once, under the
/// claim's primary key, and bound into the insert rather than defaulted.
///
/// Read the three statements together. `create_claim` mints the ids under
/// `idempotency_key PRIMARY KEY`, so a second call with that key mints nothing.
/// `create` BINDS them (`$1::uuid`), so it cannot invent a different one.
/// `create_replay` reads the durable original back through the claim. That is
/// why a replay returns the same id BY CONSTRUCTION and not by an early return.
#[test]
fn generated_create_takes_every_identity_from_the_claim() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();

    assert_eq!(
        generated_create_sql(&package, "create_claim"),
        "INSERT INTO purchase_order_command (idempotency_key, canonical_command)\n\
         VALUES ($1::text, $2::bytea)\n\
         ON CONFLICT ON CONSTRAINT purchase_order_command_idempotency_key_pkey DO NOTHING\n\
         RETURNING\n    purchase_order_id;\n",
    );
    assert_eq!(
        generated_create_sql(&package, "create"),
        "INSERT INTO purchase_order (id, supplier_id)\n\
         VALUES ($1::uuid, $2::uuid)\n\
         RETURNING\n    \
         created_at,\n    id,\n    purchase_order_number,\n    row_version,\n    status,\n    supplier_id;\n",
    );
    assert_eq!(
        generated_create_sql(&package, "create_replay"),
        "SELECT\n    claim.canonical_command,\n    \
         model.created_at,\n    model.id,\n    model.purchase_order_number,\n    \
         model.row_version,\n    model.status,\n    model.supplier_id\n\
         FROM purchase_order_command AS claim\n\
         JOIN purchase_order AS model\n    ON model.id = claim.purchase_order_id\n\
         WHERE claim.idempotency_key = $1::text;\n",
    );
}

/// EXIT GATE: the replay path performs ZERO writes.
///
/// A replay that re-ran the insert would be the duplicate the key exists to
/// prevent, so this reads the emitted text rather than trusting the caller.
#[test]
fn generated_create_replay_writes_nothing() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();
    let replay = generated_create_sql(&package, "create_replay");

    assert!(replay.starts_with("SELECT\n"), "{replay}");
    for write in [
        "INSERT",
        "UPDATE",
        "DELETE",
        "MERGE",
        "FOR UPDATE",
        "nextval",
    ] {
        assert!(!replay.contains(write), "replay must not {write}: {replay}");
    }
    // The insert never defaults an identity: every id is a bind.
    let create = generated_create_sql(&package, "create");
    assert!(!create.contains("gen_random_uuid"), "{create}");
    assert!(!create.contains("DEFAULT"), "{create}");
}

/// EXIT GATE: the create's contracts carry the claim, the replay rule and the
/// typed refusal for a key rebound to a different request.
#[test]
fn generated_create_contracts_publish_the_claim_and_its_refusal() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();

    let operation = artifact_json(
        &package,
        "generated/contracts/purchase_order/create.operation.json",
    );
    assert_eq!(
        operation["idempotency"],
        json!({
            "key": "idempotency_key",
            "canonical_command": "canonical_command",
            "claim": {
                "schema": "receiving",
                "table": CLAIM_TABLE,
                "constraint": "purchase_order_command_idempotency_key_pkey",
                "identities": {"id": "purchase_order_id"},
            },
            "statements": {
                "claim": "create_claim",
                "replay": "create_replay",
                "insert": "create",
            },
            "replay": {"writes": "none", "identity_source": "claim"},
            "conflict": {
                "on": "changed_canonical_command",
                "refusal": "idempotency_conflict",
            },
            "atomicity": "claim_and_insert_commit_together",
        })
    );
    assert_eq!(operation["transaction"], "explicit_per_input");
    let statements = operation["statements"].as_array().unwrap();
    assert_eq!(
        statements
            .iter()
            .map(|statement| statement["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["create_claim", "create_replay", "create"]
    );
    // The claim statement returns the minted identity, not the model row.
    assert_eq!(
        statements[0]["columns"],
        json!([{"name": "purchase_order_id", "type": "uuid", "nullable": false}])
    );
    assert_eq!(
        statements[0]["binds"],
        json!([
            {"name": "idempotency_key", "type": "text", "nullable": false},
            {"name": "canonical_command", "type": "bytes", "nullable": false},
        ])
    );
    // The insert binds the claim-minted id ahead of the writable fields.
    assert_eq!(
        statements[2]["binds"],
        json!([
            {"name": "id", "type": "uuid", "nullable": false},
            {"name": "supplier_id", "type": "uuid", "nullable": false},
        ])
    );

    let errors = artifact_json(
        &package,
        "generated/contracts/purchase_order/create.errors.json",
    );
    let conflict = errors["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["literal"] == "idempotency_conflict")
        .expect("a changed request for a live key is typed-refused");
    assert_eq!(conflict["from"], json!("changed_canonical_command"));
    assert_eq!(conflict["detail"]["required"], json!(["field"]));

    let input = artifact_json(
        &package,
        "generated/contracts/purchase_order/create.input.json",
    );
    assert_eq!(
        input["idempotency_key"],
        json!({"type": "text", "required": true})
    );
    assert_eq!(
        input["canonical_command"],
        json!({
            "over": "writable_fields",
            "payload": "canonical_compact_json",
            "changed": "idempotency_conflict",
        })
    );
}

/// EXIT GATE: every create-shaped command CARRIES the two claim contract
/// tests, so no package author writes them and none can omit them.
///
/// The whole artifact is frozen. An added, removed or renamed field fails here,
/// because a runner reads this file and a silent rename would make it skip a
/// case rather than refuse.
///
/// D7, wamn-10yt.19. The cases are emitted, not executed. Executing them needs
/// a live database, which is wamn-f89v.
#[test]
fn every_generated_create_carries_the_two_claim_contract_tests() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();

    assert_eq!(
        artifact_json(
            &package,
            "generated/contracts/purchase_order/create.claim-tests.json"
        ),
        json!({
            "operation": "wamn-receiving:purchase-order/create@1.0.0",
            "law": "command-identity-from-claim",
            "cases": [
                {
                    "id": "replay_returns_the_immutable_original",
                    "given": "the same idempotency_key with the same canonical_command",
                    "first_call": ["create_claim", "create"],
                    "second_call": ["create_claim", "create_replay"],
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
                    "first_call": ["create_claim", "create"],
                    "second_call": ["create_claim", "create_replay"],
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
}

/// EXIT GATE: a command that is not a create carries no claim tests, so the
/// artifact never appears where the law does not apply.
#[test]
fn only_a_create_carries_claim_contract_tests() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();

    for action in ["get", "query", "update", "delete"] {
        assert!(
            package
                .file(&format!(
                    "generated/contracts/purchase_order/{action}.claim-tests.json"
                ))
                .is_none(),
            "{action} carries claim tests"
        );
    }
}

/// EXIT GATE: the claim's shape reaches the required-schema contract and the
/// data-access overlay, so nothing the emitted SQL names is left unpinned.
#[test]
fn generated_create_pins_and_grants_its_claim_relation() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();

    let weld = artifact_json(&package, "generated/package-weld.json");
    let claim = object_named(
        weld["required_schema_contract"]["tables"]
            .as_array()
            .unwrap(),
        "table",
        CLAIM_TABLE,
    );
    assert_eq!(
        claim["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["canonical_command", "idempotency_key", "purchase_order_id"]
    );
    assert_eq!(
        claim["constraints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|constraint| constraint["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "purchase_order_command_idempotency_key_pkey",
            "purchase_order_command_purchase_order_id_key",
        ]
    );

    let overlay = artifact_json(&package, DATA_ACCESS_OVERLAY_PATH);
    let relations = overlay["relations"].as_array().unwrap();
    let granted = object_named(relations, "table", CLAIM_TABLE);
    assert_eq!(
        granted["select_fields"],
        json!(["canonical_command", "idempotency_key", "purchase_order_id"])
    );
    assert_eq!(
        granted["insert_fields"],
        json!(["canonical_command", "idempotency_key"])
    );
    // A claim is written once: nothing may update it.
    assert_eq!(granted["update_fields"], json!([]));
    assert_eq!(granted["lock"], json!(false));
    // The model insert writes the claim-minted id, so the grant must allow it.
    let model = object_named(relations, "table", "purchase_order");
    assert_eq!(model["insert_fields"], json!(["id", "supplier_id"]));
}

/// EXIT GATE: every way the claim could stop being the identity source refuses.
///
/// The unmutated manifest and catalog run FIRST as the negative control: if
/// they did not generate, a refusal below would prove nothing.
#[test]
fn generated_create_refuses_a_claim_that_does_not_pre_generate_identity() {
    run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES)
        .expect("the unmutated claim generates");

    let model = || table(&catalog(false), "purchase_order").clone();
    let claim_table = |columns, constraints| {
        Table::new("receiving", CLAIM_TABLE, columns, constraints, Vec::new())
    };
    let without = |name: &str| {
        claim_columns()
            .into_iter()
            .filter(|column| column.name() != name)
            .collect::<Vec<_>>()
    };
    let replacing = |replacement: Column| {
        claim_columns()
            .into_iter()
            .map(|column| {
                if column.name() == replacement.name() {
                    replacement.clone()
                } else {
                    column
                }
            })
            .collect::<Vec<_>>()
    };

    let cases: Vec<(&str, CatalogIr, Value, GenerateErrorKind)> = vec![
        (
            "no claim declared at all",
            claim_catalog(),
            {
                let mut manifest = claim_manifest();
                manifest["models"]["purchase_order"]["operations"]["create"]
                    .as_object_mut()
                    .unwrap()
                    .remove("claim");
                manifest
            },
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the claim relation does not exist",
            claim_catalog(),
            {
                let mut manifest = claim_manifest();
                manifest["models"]["purchase_order"]["operations"]["create"]["claim"]["table"] =
                    json!("absent_command");
                manifest["internal_relations"] = json!({
                    "absent_command": {
                        "schema": "receiving", "table": "absent_command", "cdc": "excluded"
                    }
                });
                manifest
            },
            GenerateErrorKind::UnknownRelation,
        ),
        (
            "the claim is not CDC-excluded, so mechanism state would ship as events",
            claim_catalog(),
            {
                let mut manifest = claim_manifest();
                manifest["internal_relations"] = json!({});
                manifest
            },
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the identity column carries no gen_random_uuid default",
            claim_catalog_with(
                model(),
                claim_table(
                    replacing(Column::new(
                        "purchase_order_id",
                        ColumnType::Uuid,
                        false,
                        None,
                        None,
                    )),
                    claim_constraints(),
                ),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the identity column is nullable, so the claim may mint nothing",
            claim_catalog_with(
                model(),
                claim_table(
                    replacing(Column::new(
                        "purchase_order_id",
                        ColumnType::Uuid,
                        true,
                        Some(ColumnDefault::GenRandomUuid),
                        None,
                    )),
                    claim_constraints(),
                ),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the identity column is not UNIQUE, so two claims could mint one id",
            claim_catalog_with(
                model(),
                claim_table(
                    claim_columns(),
                    vec![
                        Constraint::primary_key(
                            "purchase_order_command_idempotency_key_pkey",
                            ["idempotency_key"],
                        )
                        .unwrap(),
                    ],
                ),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the key is not a primary key, so a second call could claim it again",
            claim_catalog_with(
                model(),
                claim_table(
                    claim_columns(),
                    vec![
                        Constraint::unique(
                            "purchase_order_command_purchase_order_id_key",
                            ["purchase_order_id"],
                        )
                        .unwrap(),
                    ],
                ),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the claim carries no canonical command to compare a replay against",
            claim_catalog_with(
                model(),
                claim_table(without("canonical_command"), claim_constraints()),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "a claim column has no value the generated claim insert can supply",
            claim_catalog_with(
                model(),
                claim_table(
                    claim_columns()
                        .into_iter()
                        .chain([Column::new("actor_id", ColumnType::Uuid, false, None, None)])
                        .collect(),
                    claim_constraints(),
                ),
            ),
            claim_manifest(),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "the declared identity is not a column of the claim",
            claim_catalog(),
            {
                let mut manifest = claim_manifest();
                manifest["models"]["purchase_order"]["operations"]["create"]["claim"]["identities"] =
                    json!({"id": "absent_id"});
                manifest
            },
            GenerateErrorKind::UnknownColumn,
        ),
        (
            "only a create may carry a claim",
            claim_catalog(),
            {
                let mut manifest = claim_manifest();
                manifest["models"]["purchase_order"]["operations"]["update"]["claim"] = json!({
                    "table": CLAIM_TABLE,
                    "identities": {"id": "purchase_order_id"}
                });
                manifest
            },
            GenerateErrorKind::InvalidOperation,
        ),
    ];

    for (label, catalog, manifest, kind) in cases {
        let refusal = run(&catalog, &manifest, &QUERY_SOURCES).expect_err(label);
        assert_eq!(refusal.kind(), kind, "{label}");
    }
}

/// EXIT GATE: a second identity added to the model breaks the build unless the
/// claim pre-generates it too.
///
/// This is the enumeration the law demands, made mechanical: the generator
/// derives the minted set from the catalog, so a new `gen_random_uuid()` column
/// cannot slip through and be re-minted on replay.
#[test]
fn a_model_identity_the_claim_does_not_mint_refuses() {
    let with_second_identity = rebuilt_table(
        table(&catalog(false), "purchase_order"),
        table(&catalog(false), "purchase_order")
            .columns()
            .to_vec()
            .into_iter()
            .chain([Column::new(
                "external_id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            )])
            .collect(),
        table(&catalog(false), "purchase_order")
            .constraints()
            .to_vec(),
    );
    let claim = Table::new(
        "receiving",
        CLAIM_TABLE,
        claim_columns(),
        claim_constraints(),
        Vec::new(),
    );
    let unmapped = claim_catalog_with(with_second_identity.clone(), claim);
    let mut manifest = claim_manifest();
    manifest["models"]["purchase_order"]["server_owned_fields"] = json!([
        "id",
        "external_id",
        "purchase_order_number",
        "status",
        "row_version",
        "created_at"
    ]);

    let refusal =
        run(&unmapped, &manifest, &QUERY_SOURCES).expect_err("an unminted identity refuses");
    assert_eq!(refusal.kind(), GenerateErrorKind::InvalidOperation);

    // Mapping it to its own pre-generated claim column restores generation.
    let mapped = claim_catalog_with(
        with_second_identity,
        Table::new(
            "receiving",
            CLAIM_TABLE,
            claim_columns()
                .into_iter()
                .chain([Column::new(
                    "external_id",
                    ColumnType::Uuid,
                    false,
                    Some(ColumnDefault::GenRandomUuid),
                    None,
                )])
                .collect(),
            claim_constraints()
                .into_iter()
                .chain([Constraint::unique(
                    "purchase_order_command_external_id_key",
                    ["external_id"],
                )
                .unwrap()])
                .collect(),
            Vec::new(),
        ),
    );
    manifest["models"]["purchase_order"]["operations"]["create"]["claim"]["identities"] =
        json!({"external_id": "external_id", "id": "purchase_order_id"});
    let package = run(&mapped, &manifest, &QUERY_SOURCES).expect("both identities are claimed");
    assert_eq!(
        generated_create_sql(&package, "create"),
        "INSERT INTO purchase_order (external_id, id, supplier_id)\n\
         VALUES ($1::uuid, $2::uuid, $3::uuid)\n\
         RETURNING\n    \
         created_at,\n    external_id,\n    id,\n    purchase_order_number,\n    \
         row_version,\n    status,\n    supplier_id;\n",
    );
}

/// EXIT GATE: `idempotency_conflict` is a required, exactly-shaped member of
/// the create's closed error vocabulary, and belongs to no other action.
#[test]
fn idempotency_conflict_is_closed_to_the_create() {
    let cases: [ErrorVocabularyMutant; 3] = [
        (
            "a create that does not declare the refusal",
            Box::new(|manifest: &mut Value| {
                manifest["models"]["purchase_order"]["operations"]["create"]["error_details"]
                    .as_object_mut()
                    .unwrap()
                    .remove("idempotency_conflict");
            }),
        ),
        (
            "a create whose refusal carries the wrong structured detail",
            Box::new(|manifest: &mut Value| {
                manifest["models"]["purchase_order"]["operations"]["create"]["error_details"]["idempotency_conflict"] =
                    json!({"required": ["constraint"]});
            }),
        ),
        (
            "an update that claims the refusal",
            Box::new(|manifest: &mut Value| {
                manifest["models"]["purchase_order"]["operations"]["update"]["error_details"]["idempotency_conflict"] =
                    json!({"required": ["field"]});
            }),
        ),
    ];
    for (label, mutate) in cases {
        let mut manifest = claim_manifest();
        mutate(&mut manifest);
        let refusal = run(&claim_catalog(), &manifest, &QUERY_SOURCES).expect_err(label);
        assert_eq!(
            refusal.kind(),
            GenerateErrorKind::InvalidOperation,
            "{label}"
        );
    }
}

/// wamn-10yt.54. An exclusion violation is SQLSTATE 23P01. The generated
/// operation names it exactly as it names a unique or foreign-key violation,
/// carrying the constraint name in the `{constraint}` detail the matrix already
/// defines.
///
/// The reachable constraint is reachable ONLY through its dependency columns:
/// `supplier_id` is the single field `update` writes, and it appears in no key
/// -- it sits inside the expression key. PostgreSQL records it as a dependency
/// of the constraint's index, which is what [`Exclusion::columns`] carries, so
/// intersecting written fields with that set is what names the refusal. The
/// second constraint depends on nothing this operation writes and is therefore
/// not named, so the contract does not over-declare.
#[test]
fn a_generated_operation_names_an_exclusion_violation_with_its_constraint() {
    let base = catalog(false);
    let purchase_order = table(&base, "purchase_order");
    let with_exclusions = rebuilt_table(
        purchase_order,
        purchase_order.columns().to_vec(),
        purchase_order.constraints().to_vec(),
    )
    .with_exclusions(vec![
        Exclusion::new(
            "purchase_order_supplier_window",
            ExclusionAccessMethod::Gist,
            vec![
                ExclusionKey::new(ExclusionElement::column("status"), "="),
                ExclusionKey::new(
                    ExclusionElement::expression("tstzrange(created_at, created_at)"),
                    "&&",
                ),
            ],
            ["status", "created_at", "supplier_id"],
        )
        .unwrap(),
        Exclusion::new(
            "purchase_order_untouched_window",
            ExclusionAccessMethod::Gist,
            vec![ExclusionKey::new(ExclusionElement::column("status"), "=")],
            ["status", "created_at"],
        )
        .unwrap(),
    ]);
    let catalog = replacing_table(&base, with_exclusions);

    let mut manifest = manifest();
    manifest["models"]["purchase_order"]["operations"]["update"]["error_details"]["exclusion_violation"] =
        json!({"required": ["constraint"]});

    let package = run(&catalog, &manifest, &QUERY_SOURCES).unwrap();
    let errors = artifact_json(
        &package,
        "generated/contracts/purchase_order/update.errors.json",
    );
    assert_eq!(errors["closed"], json!(true));
    let named = errors["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["literal"] == "exclusion_violation")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        named,
        vec![json!({
            "literal": "exclusion_violation",
            "from": "exclusion_violation",
            "constraint": "purchase_order_supplier_window",
            "detail": {"required": ["constraint"]}
        })],
        "the reachable exclusion is named once, in the same case shape a unique \
         violation uses"
    );

    // The refusal vocabulary stays closed: an operation that can violate an
    // exclusion must declare it, exactly as it must for the other three.
    let mut undeclared = manifest.clone();
    undeclared["models"]["purchase_order"]["operations"]["update"]["error_details"]
        .as_object_mut()
        .unwrap()
        .remove("exclusion_violation");
    assert_eq!(
        run(&catalog, &undeclared, &QUERY_SOURCES)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}
