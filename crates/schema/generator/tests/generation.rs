use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use wamn_execution_contract::canonical_json_bytes;
use wamn_schema_generator::{
    AuthoredSql, CrudAction, DATA_ACCESS_OVERLAY_PATH, GenerateErrorKind, GeneratedPackage,
    GeneratedPackageMetadata, PackageManifest, canonical_operation_identity,
    canonical_operation_prefix, corpus_sha256, validate_operation_vocabulary,
};
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnType, Constraint, Exclusion, ExclusionAccessMethod,
    ExclusionElement, ExclusionKey, Table,
};

#[path = "support/generation.rs"]
mod support;
use support::{
    QUERY_SOURCES, accessor_bind, artifact_json, assert_native_fixtures_match_wamn_api, catalog,
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
    mismatch["models"]["gadget"]["table"] = json!("missing");
    let error = run(&ir, &mismatch, &QUERY_SOURCES).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::UnknownRelation);
    assert_eq!(error.object(), Some("inventory.missing"));

    let mut unknown_action = manifest();
    unknown_action["models"]["gadget"]["operations"]["merge"] =
        json!({"permission": "gadget.merge", "result": "one"});
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
    let statement = operation["statements"]["load_gadget_part"].clone();
    operation["statements"] = json!({
        "foo1": statement,
        "foo_1": {
            "path": "query/other_gadget_part.sql",
            "fetch": "optional_one",
            "parameters": [{"name": "id", "type": "uuid", "nullable": false}],
            "row": [{"name": "id", "type": "uuid", "nullable": false}]
        }
    });
    row_collision["custom_operations"]["quality.load_gadget_part"] = operation;
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
    fixture_collision["custom_operations"]["quality.load_gadget_part"] = operation;
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&fixture_collision))
            .expect_err("two statement parameters collapsed to one Rust fixture symbol")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

/// A custom projection or command cannot declare the page result class. No
/// paging contract exists for it, so page belongs to the generated query.
#[test]
fn a_custom_operation_refuses_the_page_result_class() {
    let projection = projection_operation();
    let mut command = projection_operation();
    command["kind"] = json!("command");
    command["transaction"] = json!("explicit_per_input");
    command["automatic_retry"] = json!(false);
    command["idempotent_by"] = json!({"state": {"guards": {"gadget": "gadget_id"}}});
    for (kind, mut operation) in [("projection", projection), ("command", command)] {
        let mut admitted = manifest();
        admitted["custom_operations"]["quality.load_gadget_part"] = operation.clone();
        validate_operation_vocabulary(&parsed_manifest(&admitted))
            .unwrap_or_else(|error| panic!("the {kind} fixture is valid with class one: {error}"));

        operation["result"]["class"] = json!("page");
        let mut paged = manifest();
        paged["custom_operations"]["quality.load_gadget_part"] = operation;
        let refusal = run(&catalog(false), &paged, &QUERY_SOURCES)
            .expect_err("a custom operation declaring page generated");
        assert_eq!(
            refusal.kind(),
            GenerateErrorKind::InvalidOperation,
            "{kind}"
        );
        assert_eq!(
            refusal.context(),
            format!(
                "{kind} quality.load_gadget_part must not declare result class page; \
                 page belongs to the generated query"
            ),
        );
    }
}

#[test]
fn component_grouping_defaults_one_group_and_refuses_invalid_splits() {
    let single = parsed_manifest(&manifest());
    assert_eq!(
        validate_operation_vocabulary(&single).unwrap(),
        BTreeSet::from([
            "gadget.get".to_owned(),
            "gadget.query".to_owned(),
            "gadget.update".to_owned(),
        ])
    );
    assert!(
        single.models["gadget"].operations[&CrudAction::Get]
            .component
            .is_none()
    );

    let mut empty = manifest();
    empty["models"]["gadget"]["operations"]["get"]["component"] = json!("");
    let error = validate_operation_vocabulary(&parsed_manifest(&empty))
        .expect_err("an empty component name was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation gadget.get component must not be empty"
    );

    let mut unknown = manifest();
    unknown["models"]["gadget"]["operations"]["get"]["component"] = json!("missing");
    let error = validate_operation_vocabulary(&parsed_manifest(&unknown))
        .expect_err("an unknown component was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation gadget.get references unknown component missing"
    );

    let mut identical = manifest();
    identical["components"]["duplicate"] = json!({"connections": ["postgres"]});
    for operation in ["get", "query", "update"] {
        identical["models"]["gadget"]["operations"][operation]["component"] =
            json!(if operation == "query" {
                "duplicate"
            } else {
                "inventory"
            });
    }
    let error = validate_operation_vocabulary(&parsed_manifest(&identical))
        .expect_err("an identical-requirement split was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "components duplicate and inventory have identical requirement sets"
    );

    let mut distinct = manifest();
    distinct["connections"] = json!(["postgres", "reporting"]);
    distinct["components"]["reporting"] = json!({"connections": ["reporting"]});
    distinct["models"]["gadget"]["operations"]["query"]["component"] = json!("reporting");
    let error = validate_operation_vocabulary(&parsed_manifest(&distinct))
        .expect_err("a multi-component manifest omitted explicit operation grouping");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation gadget.get must name a component when the manifest declares multiple components"
    );

    for operation in ["get", "update"] {
        distinct["models"]["gadget"]["operations"][operation]["component"] = json!("inventory");
    }
    validate_operation_vocabulary(&parsed_manifest(&distinct))
        .expect("distinct requirement groups with explicit operation membership");

    let mut old_grouping = manifest();
    old_grouping["components"]["inventory"]["operations"] = json!(["gadget.get"]);
    assert!(PackageManifest::from_slice(&serde_json::to_vec(&old_grouping).unwrap()).is_err());
}

#[test]
fn shared_operation_vocabulary_refuses_permission_identity_drift() {
    let mut mismatch = manifest();
    mismatch["models"]["gadget"]["operations"]["get"]["permission"] = json!("gadget.query");
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
    package["package"]["id"] = json!("platform-Gadget");
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
    server_owned["models"]["gadget"]["operations"]["update"]["writable_fields"] = json!(["status"]);
    assert_eq!(
        run(&ir, &server_owned, &QUERY_SOURCES).unwrap_err().kind(),
        GenerateErrorKind::InvalidOperation
    );

    let package = run(&ir, &manifest(), &QUERY_SOURCES).unwrap();
    let input = artifact_json(&package, "generated/contracts/gadget/update.input.json");
    assert_eq!(
        input["expected_row_version"],
        json!({"field": "row_version", "type": "int64", "required": true, "revision": true})
    );
    assert_eq!(input["writable_fields"][0]["field"], "stock_id");
    assert_eq!(
        input["writable_fields"][0]["explicit_null"],
        "invalid_input"
    );
    assert_eq!(input["server_owned_fields"]["if_supplied"], "invalid_input");

    let wit = std::str::from_utf8(
        package
            .file("generated/wit/deps/platform-gadget-gadget/package.wit")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    assert!(wit.contains("interface get"));
    assert!(wit.contains("interface query"));
    assert!(wit.contains("interface update"));
    assert!(wit.contains("stock-id: option<option<string>>"));
    assert!(wit.contains("expected-row-version: s64"));
    assert!(wit.contains("observed-row-version: s64"));
    assert!(wit.contains("input: result<update-request, invalid-input-detail>"));
    for field in [
        "created-at",
        "id",
        "part-code",
        "row-version",
        "status",
        "stock-id",
    ] {
        assert!(wit.contains(&format!("    {field}:")));
    }
    let codec = std::str::from_utf8(
        package
            .file("generated/wit/gadget_update_codec.rs")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    assert!(codec.contains("json!(JsonInt64(value.expected_row_version))"));
    assert!(codec.contains("value.parse::<i64>().ok()"));
}

/// An application integer is int32 by default, and int64 is opt-in, so a
/// revision carries the width of its own column. The request member, the JSON
/// codec and the refusal detail all follow that column.
#[test]
fn a_revision_carries_the_width_its_column_declares() {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let columns = gadget
        .columns()
        .iter()
        .map(|column| {
            if column.name() == "row_version" {
                Column::new(
                    "row_version",
                    ColumnType::Int32,
                    false,
                    Some(ColumnDefault::int32(1)),
                    None,
                )
            } else {
                column.clone()
            }
        })
        .collect::<Vec<_>>();
    let narrow = replacing_table(
        &base,
        rebuilt_table(gadget, columns, gadget.constraints().to_vec()),
    );
    let package = run(&narrow, &manifest(), &QUERY_SOURCES).expect("an int32 revision generates");

    let input = artifact_json(&package, "generated/contracts/gadget/update.input.json");
    assert_eq!(input["expected_row_version"]["type"], "int32");

    let wit = std::str::from_utf8(
        package
            .file("generated/wit/deps/platform-gadget-gadget/package.wit")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    assert!(wit.contains("expected-row-version: s32"));
    assert!(wit.contains("observed-row-version: s32"));

    let codec = std::str::from_utf8(
        package
            .file("generated/wit/gadget_update_codec.rs")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    // The narrow revision is a JSON number in both directions.
    assert!(codec.contains("i32::try_from(request.expected_row_version)"));
    assert!(codec.contains("json!(value.expected_row_version)"));
    assert!(!codec.contains("JsonInt64(value.expected_row_version)"));

    // A revision of any other type is still refused, and the message names the
    // two widths an application can declare.
    let columns = gadget
        .columns()
        .iter()
        .map(|column| {
            if column.name() == "row_version" {
                Column::new("row_version", ColumnType::Text, false, None, None)
            } else {
                column.clone()
            }
        })
        .collect::<Vec<_>>();
    let wrong = replacing_table(
        &base,
        rebuilt_table(gadget, columns, gadget.constraints().to_vec()),
    );
    let refusal = run(&wrong, &manifest(), &QUERY_SOURCES).expect_err("a text revision generated");
    assert_eq!(refusal.kind(), GenerateErrorKind::InvalidOperation);
    assert!(
        refusal
            .context()
            .contains("revision field must be a non-null int32 or int64"),
        "{}",
        refusal.context()
    );
}

#[test]
fn operation_identity_errors_and_constraint_names_are_closed() {
    let package_manifest = parsed_manifest(&manifest());
    assert_eq!(
        canonical_operation_prefix(&package_manifest.package).unwrap(),
        "platform-gadget:"
    );
    assert_eq!(
        canonical_operation_identity(&package_manifest.package, "gadget.assemble").unwrap(),
        "platform-gadget:gadget/assemble@1.0.0"
    );
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let operation = artifact_json(&package, "generated/contracts/gadget/update.operation.json");
    assert_eq!(operation["permission_token"], "gadget.update");
    assert_eq!(operation["grant"], "platform-gadget:gadget/update@1.0.0");
    assert_eq!(operation["automatic_retry"], false);

    let errors = artifact_json(&package, "generated/contracts/gadget/update.errors.json");
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

    first.file("generated/native-verifier/gadget.rs").unwrap();
    first.file("generated/wamn/gadget.rs").unwrap();
    assert!(first.file("query/open_gadget.sql").is_none());

    let source_map = artifact_json(&first, "generated/source-map/gadget.json");
    assert_eq!(source_map["relation"], "catalog-ir://inventory.gadget");
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
            &format!("generated/contracts/gadget/{action}.operation.json"),
        );
        let result = artifact_json(
            &package,
            &format!("generated/contracts/gadget/{action}.result.json"),
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
        artifact_json(&package, "generated/contracts/gadget/get.result.json"),
        json!({
            "class": "one",
            "fields": [
                {"path": "created_at", "type": "timestamptz", "nullable": false, "values": []},
                {"path": "id", "type": "uuid", "nullable": false, "values": []},
                {
                    "path": "part_code",
                    "type": "text",
                    "nullable": false,
                    "values": []
                },
                {"path": "row_version", "type": "int64", "nullable": false, "values": [], "revision": true},
                {
                    "path": "status",
                    "type": "text",
                    "nullable": false,
                    "values": ["open", "complete", "cancelled"]
                },
                {"path": "stock_id", "type": "uuid", "nullable": false, "values": []}
            ]
        })
    );

    let update = artifact_json(&package, "generated/contracts/gadget/update.result.json");
    assert_eq!(
        update,
        artifact_json(&package, "generated/contracts/gadget/get.result.json"),
        "a successful update returns the public model row"
    );
    let operation = artifact_json(&package, "generated/contracts/gadget/update.operation.json");
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
            .file("generated/sql/gadget/update.sql")
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
            "    model.created_at,\n    model.id,\n    model.part_code,\n",
            "    model.row_version,\n    model.status,\n    model.stock_id",
        )
    );
}

#[test]
fn wamn_accessors_are_structurally_derived_from_operations_and_ir() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let get_contract = artifact_json(&package, "generated/contracts/gadget/get.operation.json");
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
            {"name": "part_code", "type": "text", "nullable": false},
            {"name": "row_version", "type": "int64", "nullable": false},
            {"name": "status", "type": "text", "nullable": false},
            {"name": "stock_id", "type": "uuid", "nullable": false}
        ])
    );
    let source_map = artifact_json(&package, "generated/source-map/gadget.json");
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
            "row": "GadgetRow",
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
            "query_part_code_ascending",
            "QUERY_0_DIGEST",
            "text",
            "Option<String>",
            "Option<String>",
        ),
        (
            "query_part_code_descending",
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
                "row": "GadgetRow",
                "fetch": "all",
                "binds": [
                    accessor_bind(
                        "stock_id_filter",
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
            "row": "GadgetUpdateRow",
            "fetch": "one",
            "binds": [
                accessor_bind("id", "uuid", false, "uuid::Uuid", "wamn_postgres_statements::Uuid"),
                accessor_bind("expected_row_version", "int8", false, "i64", "i64"),
                accessor_bind("stock_id_present", "boolean", false, "bool", "bool"),
                accessor_bind(
                    "stock_id_value",
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
            "name": "GadgetUpdateRow",
            "visibility": "public",
            "fields": [
                {"name": "outcome", "type": "Option<String>"},
                {"name": "observed_row_version", "type": "Option<i64>"},
                {"name": "created_at", "type": "Option<wamn_postgres_statements::TimestampTz>"},
                {"name": "id", "type": "Option<wamn_postgres_statements::Uuid>"},
                {"name": "part_code", "type": "Option<String>"},
                {"name": "row_version", "type": "Option<i64>"},
                {"name": "status", "type": "Option<String>"},
                {"name": "stock_id", "type": "Option<wamn_postgres_statements::Uuid>"}
            ]
        }])
    );
    assert_eq!(
        source_map["native_operation_rows"],
        json!([{
            "name": "GadgetUpdateRow",
            "visibility": "public",
            "fields": [
                {"name": "outcome", "type": "Option<String>"},
                {"name": "observed_row_version", "type": "Option<i64>"},
                {"name": "created_at", "type": "Option<chrono::DateTime<chrono::Utc>>"},
                {"name": "id", "type": "Option<uuid::Uuid>"},
                {"name": "part_code", "type": "Option<String>"},
                {"name": "row_version", "type": "Option<i64>"},
                {"name": "status", "type": "Option<String>"},
                {"name": "stock_id", "type": "Option<uuid::Uuid>"}
            ]
        }])
    );

    assert_native_fixtures_match_wamn_api(&package, "gadget");
}

#[test]
fn metadata_hashes_exact_ir_and_sql_but_contract_ignores_unused_tables() {
    let base_ir = catalog(false);
    let additive_ir = catalog(true);
    let base = run(&base_ir, &manifest(), &QUERY_SOURCES).unwrap();
    let additive = run(&additive_ir, &manifest(), &QUERY_SOURCES).unwrap();
    let base_metadata = artifact_json(&base, "generated/package-weld.json");
    let additive_metadata = artifact_json(&additive, "generated/package-weld.json");

    let expected_schema = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(canonical_json_bytes(
            &serde_json::to_value(&base_ir).unwrap()
        )))
    );
    assert_eq!(base_metadata["verified_schema_state_id"], expected_schema);
    assert_ne!(
        base_metadata["verified_schema_state_id"],
        additive_metadata["verified_schema_state_id"]
    );
    assert_eq!(
        base_metadata["required_schema_contract"],
        additive_metadata["required_schema_contract"]
    );
    assert_eq!(
        base_metadata["required_platform_policy_contract"],
        json!({"id": "gadget_data_access", "state": "unsatisfied"})
    );
    assert_eq!(
        base_metadata["promotion_state"],
        "blocked_unsatisfied_policy_contract"
    );
    assert!(!base.metadata().promotion_eligible());
}

#[test]
fn explicit_cdc_exclusion_is_a_required_relation_without_fabricated_fields() {
    let mut package_manifest = manifest();
    package_manifest["internal_relations"] = json!({
        "unused": {
            "schema": "inventory",
            "table": "unused",
            "cdc": "excluded"
        }
    });
    let package = run(&catalog(true), &package_manifest, &QUERY_SOURCES).unwrap();
    let metadata = artifact_json(&package, "generated/package-weld.json");
    let required = metadata["required_schema_contract"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|table| table["table"] == "unused")
        .expect("the explicit exclusion is part of the schema contract");
    assert_eq!(required["schema"], "inventory");
    assert_eq!(required["fields"], json!([]));
    assert_eq!(required["constraints"], json!([]));
}

#[test]
fn generated_metadata_has_one_strict_canonical_reader() {
    let package = run(&catalog(false), &manifest(), &QUERY_SOURCES).unwrap();
    let bytes = package
        .file("generated/package-weld.json")
        .expect("the generated package carries its metadata")
        .bytes();
    assert_eq!(
        &GeneratedPackageMetadata::from_slice(bytes).unwrap(),
        package.metadata()
    );

    let wrong_type = GeneratedPackageMetadata::from_slice(b"null").unwrap_err();
    assert_eq!(wrong_type.kind(), GenerateErrorKind::InvalidManifest);
    assert_eq!(
        std::error::Error::source(&wrong_type).unwrap().to_string(),
        "invalid type: null, expected struct PackageWeld at line 1 column 4"
    );

    let mut alternate = bytes.to_vec();
    alternate.push(b'\n');
    assert_eq!(
        GeneratedPackageMetadata::from_slice(&alternate)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidManifest
    );

    let mut unknown: Value = serde_json::from_slice(bytes).unwrap();
    unknown["extra"] = json!(true);
    assert_eq!(
        GeneratedPackageMetadata::from_slice(&canonical_json_bytes(&unknown))
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidManifest
    );

    let mut contradictory: Value = serde_json::from_slice(bytes).unwrap();
    contradictory["promotion_state"] = json!("eligible");
    assert_eq!(
        GeneratedPackageMetadata::from_slice(&canonical_json_bytes(&contradictory))
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
    generated_manifest["models"]["gadget"]["operations"]["query"]
        .as_object_mut()
        .unwrap()
        .remove("authored_sql");
    let package = run(&ir, &generated_manifest, &[]).unwrap();
    package
        .file("generated/sql/gadget/query_part_code_ascending.sql")
        .unwrap();
    package
        .file("generated/sql/gadget/query_created_at_descending.sql")
        .unwrap();
    let input = artifact_json(&package, "generated/contracts/gadget/query.input.json");
    assert_eq!(
        input["filters"],
        json!([
            {"field": "stock_id", "binding": "json_array", "type": "uuid"},
            {"field": "status", "binding": "json_array", "type": "text",
                "values": ["open", "complete", "cancelled"]}
        ])
    );

    let mut default_only = generated_manifest;
    default_only["models"]["gadget"]["operations"]["query"]
        .as_object_mut()
        .unwrap()
        .remove("sort");
    let package = run(&ir, &default_only, &[]).unwrap();
    let query_paths = package
        .files()
        .iter()
        .filter(|file| file.path().starts_with("generated/sql/gadget/query_"))
        .collect::<Vec<_>>();
    assert_eq!(query_paths.len(), 1);
    assert_eq!(
        query_paths[0].path(),
        "generated/sql/gadget/query_created_at_ascending.sql"
    );
}

#[test]
fn duplicate_filters_and_schema_qualified_authored_sql_refuse() {
    let ir = catalog(false);
    let mut duplicate = manifest();
    duplicate["models"]["gadget"]["operations"]["query"]["filters"] = json!([
        {"field": "status"},
        {"field": "status"}
    ]);
    assert_eq!(
        run(&ir, &duplicate, &QUERY_SOURCES).unwrap_err().kind(),
        GenerateErrorKind::InvalidOperation
    );

    let qualified = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_gadget.sql" {
            AuthoredSql::new(source.path(), b"SELECT * FROM inventory.gadget;\n")
        } else {
            source
        }
    });
    let error = run(&ir, &manifest(), &qualified).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::SchemaQualifiedSql);
    assert_eq!(error.path(), Some("query/open_gadget.sql"));

    let quoted = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_gadget.sql" {
            AuthoredSql::new(source.path(), b"SELECT * FROM \"inventory\".\"gadget\";\n")
        } else {
            source
        }
    });
    assert_eq!(
        run(&ir, &manifest(), &quoted).unwrap_err().kind(),
        GenerateErrorKind::SchemaQualifiedSql
    );

    let inert = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_gadget.sql" {
            AuthoredSql::new(
                source.path(),
                b"-- inventory.gadget\nSELECT 'inventory.gadget', $$\"inventory\".\"gadget\"$$, $1, $2, $3, $4, $5 /* inventory.gadget */;\n",
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
// These tests check the generator. No package declares a create today, so
// the emitted statements have no in-cluster consumer yet; the executing test
// is due on the first one.
// ---------------------------------------------------------------------------

const CLAIM_TABLE: &str = "gadget_command";

fn claim_columns() -> Vec<Column> {
    vec![
        Column::new("canonical_command", ColumnType::Bytes, false, None, None),
        Column::new("idempotency_key", ColumnType::Text, false, None, None),
        Column::new(
            "gadget_id",
            ColumnType::Uuid,
            false,
            Some(ColumnDefault::GenRandomUuid),
            None,
        ),
    ]
}

fn claim_constraints() -> Vec<Constraint> {
    vec![
        Constraint::primary_key("gadget_command_idempotency_key_pkey", ["idempotency_key"])
            .unwrap(),
        Constraint::unique("gadget_command_gadget_id_key", ["gadget_id"]).unwrap(),
    ]
}

fn claim_catalog_with(model: Table, claim: Table) -> CatalogIr {
    CatalogIr::new(vec![model, claim])
}

fn claim_catalog() -> CatalogIr {
    let model = table(&catalog(false), "gadget").clone();
    claim_catalog_with(
        model,
        Table::new(
            "inventory",
            CLAIM_TABLE,
            claim_columns(),
            claim_constraints(),
            Vec::new(),
        ),
    )
}

fn claim_manifest() -> Value {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["operations"]["create"] = json!({
        "permission": "gadget.create",
        "writable_fields": ["stock_id"],
        "claim": {
            "table": CLAIM_TABLE,
            "identities": {"id": "gadget_id"}
        },
        "result": "one"
    });
    manifest["internal_relations"] = json!({
        CLAIM_TABLE: {"schema": "inventory", "table": CLAIM_TABLE, "cdc": "excluded"}
    });
    manifest
}

/// A second model with create fields in each admitted presence state
/// and a revision name that is not `row_version`.
fn inventory_item_fixture() -> (CatalogIr, Value) {
    let model = Table::new(
        "inventory",
        "inventory_item",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("sku", ColumnType::Text, false, None, None),
            Column::new("note", ColumnType::Text, true, None, None),
            Column::new(
                "priority",
                ColumnType::Int64,
                false,
                Some(ColumnDefault::int64(0)),
                None,
            ),
            Column::new(
                "sequence_number",
                ColumnType::Int64,
                false,
                Some(ColumnDefault::int64(1)),
                None,
            ),
            Column::new(
                "created_at",
                ColumnType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
        ],
        vec![Constraint::primary_key("inventory_item_id_pkey", ["id"]).unwrap()],
        Vec::new(),
    );
    let claim = Table::new(
        "inventory",
        CLAIM_TABLE,
        claim_columns(),
        claim_constraints(),
        Vec::new(),
    );
    let manifest = json!({
        "package": {"id": "wamn_inventory", "version": "1.0.0"},
        "required_platform_policy_contract": {
            "id": "inventory_data_access",
            "state": "unsatisfied"
        },
        "models": {
            "inventory_item": {
                "schema": "inventory",
                "table": "inventory_item",
                "owner": "wamn_inventory",
                "server_owned_fields": ["id", "sequence_number", "created_at"],
                "audit_log": {"columns": ["created_at"], "retention": "none"},
                "delete_mode": "hard",
                "operations": {
                    "create": {
                        "permission": "inventory_item.create",
                        "writable_fields": ["sku", "note", "priority"],
                        "claim": {
                            "table": CLAIM_TABLE,
                            "identities": {"id": "gadget_id"}
                        },
                        "result": "one"
                    },
                    "update": {
                        "permission": "inventory_item.update",
                        "writable_fields": ["priority"],
                        "revision_field": "sequence_number",
                        "result": "one"
                    },
                    "delete": {
                        "permission": "inventory_item.delete",
                        "revision_field": "sequence_number",
                        "result": "one"
                    }
                }
            }
        },
        "internal_relations": {
            CLAIM_TABLE: {"schema": "inventory", "table": CLAIM_TABLE, "cdc": "excluded"}
        },
        "connections": ["postgres"],
        "components": {"inventory": {"connections": ["postgres"]}}
    });
    (CatalogIr::new(vec![model, claim]), manifest)
}

/// Build the generated typed CRUD component and run its codec tests.
///
/// The codec is generated source, so the test that states its behavior runs
/// inside the scratch crate that compiles it.
fn compile_inventory_item_component(package: &GeneratedPackage) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("the generator sits three levels below the repository root");
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let scratch = target.join("generator-fixtures/typed-crud-contracts");
    let fixture_target = target.join("generator-fixture-build");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(scratch.join("src")).expect("create fixture source directory");
    std::fs::create_dir_all(scratch.join("wit")).expect("create fixture WIT directory");

    for (source, target) in [
        (
            "generated/wit/deps/wamn-inventory-inventory-item/package.wit",
            "wit/package.wit",
        ),
        (
            "generated/wit/inventory_item_create_codec.rs",
            "src/create_codec.rs",
        ),
        (
            "generated/wit/inventory_item_update_codec.rs",
            "src/update_codec.rs",
        ),
        (
            "generated/wit/inventory_item_delete_codec.rs",
            "src/delete_codec.rs",
        ),
        ("generated/wit/operation_codec.rs", "src/operation_codec.rs"),
    ] {
        std::fs::write(
            scratch.join(target),
            package
                .file(source)
                .unwrap_or_else(|| panic!("missing {source}"))
                .bytes(),
        )
        .unwrap_or_else(|error| panic!("write {target}: {error}"));
    }

    std::fs::write(
        scratch.join("Cargo.toml"),
        "[package]\nname = \"typed-crud-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         \n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\nserde_json = \"1\"\n\
         uuid = \"1\"\nwit-bindgen = { version = \"0.61\", default-features = false, features = [\"async\", \"macros\", \"realloc\"] }\n\
         \n[lib]\ncrate-type = [\"cdylib\"]\n\n[workspace]\n",
    )
    .expect("write fixture manifest");
    let node = root.join("crates/execution/workflow/router/wit");
    let lib = format!(
        r##"wit_bindgen::generate!({{
    world: "wamn:inventory-fixture/component@1.0.0",
    inline: r#"
        package wamn:inventory-fixture@1.0.0;
        world component {{
          export wamn-inventory:inventory-item/create@1.0.0;
          export wamn-inventory:inventory-item/update@1.0.0;
          export wamn-inventory:inventory-item/delete@1.0.0;
        }}
    "#,
    path: ["{}", "{}"],
    generate_all,
    async: true,
}});

struct Component;

macro_rules! operation {{
    ($module:ident, $contract:ident, $handler:ident, $request:ident, $result:ident, $error:ident) => {{
        mod $module {{
            use crate::exports::wamn_inventory::inventory_item::$contract as contract;
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/", stringify!($module), ".rs"));
        }}
        async fn $handler(
            _state: &mut (),
            _request: exports::wamn_inventory::inventory_item::$contract::$request,
        ) -> Result<
            exports::wamn_inventory::inventory_item::$contract::$result,
            exports::wamn_inventory::inventory_item::$contract::$error,
        > {{
            unreachable!()
        }}
    }};
}}
"##,
        node.display(),
        scratch.join("wit").display()
    );
    let lib = format!(
        "{lib}\noperation!(create_codec, create, create_handler, CreateRequest, CreateResult, CreateError);\n\
         operation!(update_codec, update, update_handler, UpdateRequest, UpdateResult, UpdateError);\n\
         operation!(delete_codec, delete, delete_handler, DeleteRequest, DeleteResult, DeleteError);\n\n\
         create_codec::export_operation!(Component, crate::exports::wamn_inventory::inventory_item::create, crate::wamn::node::types, (), create_handler, create_codec);\n\
         update_codec::export_operation!(Component, crate::exports::wamn_inventory::inventory_item::update, crate::wamn::node::types, (), update_handler, update_codec);\n\
         delete_codec::export_operation!(Component, crate::exports::wamn_inventory::inventory_item::delete, crate::wamn::node::types, (), delete_handler, delete_codec);\n\n\
         export!(Component);\n\n{CODEC_TESTS}"
    );
    std::fs::write(scratch.join("src/lib.rs"), lib).expect("write fixture component");

    let output = Command::new(env!("CARGO"))
        .args(["test", "--offline", "--quiet"])
        .env("CARGO_TARGET_DIR", fixture_target)
        .current_dir(&scratch)
        .output()
        .expect("cargo test runs for the typed CRUD fixture");
    assert!(
        output.status.success(),
        "the generated typed CRUD component does not compile or its codec test fails:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

/// The create codec refuses what the create contract refuses (wamn-59wi).
///
/// `sku` is NOT NULL with no default, so its contract states
/// `omitted: invalid_input`, and the codec refuses an omitted or null `sku` on
/// that path before the handler runs. `note` is nullable and `priority` has a
/// default, so a create may omit both.
const CODEC_TESTS: &str = r##"
#[cfg(test)]
mod codec_tests {
    use crate::exports::wamn_inventory::inventory_item::create as contract;

    /// The field each item refuses on, and how many items reached the handler.
    fn create(body: &str) -> (Vec<String>, usize) {
        let items = super::create_codec::decode(body).expect("the body decodes");
        let mut calls = 0_usize;
        let outcomes = {
            let run = super::create_codec::run(items, &mut calls, async |calls: &mut usize, _| {
                *calls += 1;
                Err(contract::CreateError::InvalidInput(contract::InvalidInputDetail {
                    field: "handler".to_owned(),
                }))
            });
            let mut run = std::pin::pin!(run);
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            let std::task::Poll::Ready(outcomes) = run.as_mut().poll(&mut context) else {
                panic!("the codec awaits only the handler, which is ready");
            };
            outcomes
        };
        let fields = outcomes
            .into_iter()
            .map(|outcome| match outcome.outcome {
                Err(contract::CreateError::InvalidInput(detail)) => detail.field,
                _ => panic!("every item refuses as invalid input"),
            })
            .collect();
        (fields, calls)
    }

    #[test]
    fn a_create_refuses_an_omitted_field_that_nothing_would_fill() {
        let omitted = r#"[{"request_id":"r","idempotency_key":"k"}]"#;
        assert_eq!(create(omitted), (vec!["sku".to_owned()], 0));
        let null = r#"[{"request_id":"r","idempotency_key":"k","sku":null}]"#;
        assert_eq!(create(null), (vec!["sku".to_owned()], 0));
        let present = r#"[{"request_id":"r","idempotency_key":"k","sku":"A-1"}]"#;
        assert_eq!(create(present), (vec!["handler".to_owned()], 1));
    }
}
"##;

#[test]
fn typed_crud_contracts_follow_a_second_model_declaration() {
    let (catalog, manifest) = inventory_item_fixture();
    let package = run(&catalog, &manifest, &[]).expect("the inventory CRUD fixture generates");
    let input = artifact_json(
        &package,
        "generated/contracts/inventory_item/create.input.json",
    );
    let fields = input["writable_fields"].as_array().unwrap();
    let field = |name| {
        fields
            .iter()
            .find(|field| field["field"] == name)
            .unwrap_or_else(|| panic!("create contract omitted {name}"))
    };
    // NOT NULL with no default: an omitted sku cannot take a default.
    assert_eq!(field("sku")["omitted"], "invalid_input");
    assert_eq!(field("sku")["explicit_null"], "invalid_input");
    assert_eq!(field("note")["omitted"], "postgres_default");
    assert_eq!(field("note")["explicit_null"], "accepted");
    assert_eq!(field("priority")["omitted"], "postgres_default");
    assert_eq!(field("priority")["explicit_null"], "invalid_input");

    let wit = std::str::from_utf8(
        package
            .file("generated/wit/deps/wamn-inventory-inventory-item/package.wit")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    assert!(wit.contains("sku: option<option<string>>"), "{wit}");
    assert!(wit.contains("note: option<option<string>>"), "{wit}");
    assert!(wit.contains("priority: option<option<s64>>"), "{wit}");
    assert!(wit.contains("expected-sequence-number: s64"), "{wit}");
    assert!(
        wit.contains("record delete-row {\n    outcome: option<string>,"),
        "{wit}"
    );

    let create_codec = std::str::from_utf8(
        package
            .file("generated/wit/inventory_item_create_codec.rs")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    assert!(
        create_codec.contains(
            "if matches!(request.sku, Some(None)) {\n        return Err(invalid(\"sku\"));\n    }"
        ),
        "{create_codec}"
    );

    let update_codec = std::str::from_utf8(
        package
            .file("generated/wit/inventory_item_update_codec.rs")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    assert!(
        update_codec.contains("expected_sequence_number: String"),
        "{update_codec}"
    );
    assert!(
        update_codec.contains("request.expected_sequence_number.parse::<i64>()"),
        "{update_codec}"
    );

    let delete = artifact_json(
        &package,
        "generated/contracts/inventory_item/delete.input.json",
    );
    assert_eq!(
        delete["expected_sequence_number"],
        json!({"field": "sequence_number", "type": "int64", "required": true, "revision": true})
    );
    compile_inventory_item_component(&package);
}

fn generated_create_sql(package: &GeneratedPackage, statement: &str) -> String {
    String::from_utf8(
        package
            .file(&format!("generated/sql/gadget/{statement}.sql"))
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
/// `create_replay` reads the created row back through the claim. That is
/// why a replay returns the same id BY CONSTRUCTION and not by an early return.
#[test]
fn generated_create_takes_every_identity_from_the_claim() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();

    assert_eq!(
        generated_create_sql(&package, "create_claim"),
        "INSERT INTO gadget_command (idempotency_key, canonical_command)\n\
         VALUES ($1::text, $2::bytea)\n\
         ON CONFLICT ON CONSTRAINT gadget_command_idempotency_key_pkey DO NOTHING\n\
         RETURNING\n    gadget_id;\n",
    );
    assert_eq!(
        generated_create_sql(&package, "create"),
        "INSERT INTO gadget (id, stock_id)\n\
         VALUES ($1::uuid, $2::uuid)\n\
         RETURNING\n    \
         created_at,\n    id,\n    part_code,\n    row_version,\n    status,\n    stock_id;\n",
    );
    assert_eq!(
        generated_create_sql(&package, "create_replay"),
        "SELECT\n    claim.canonical_command,\n    \
         model.created_at,\n    model.id,\n    model.part_code,\n    \
         model.row_version,\n    model.status,\n    model.stock_id\n\
         FROM gadget_command AS claim\n\
         JOIN gadget AS model\n    ON model.id = claim.gadget_id\n\
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

    let operation = artifact_json(&package, "generated/contracts/gadget/create.operation.json");
    assert_eq!(
        operation["idempotency"],
        json!({
            "key": "idempotency_key",
            "canonical_command": "canonical_command",
            "claim": {
                "schema": "inventory",
                "table": CLAIM_TABLE,
                "constraint": "gadget_command_idempotency_key_pkey",
                "identities": {"id": "gadget_id"},
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
        json!([{"name": "gadget_id", "type": "uuid", "nullable": false}])
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
            {"name": "stock_id", "type": "uuid", "nullable": false},
        ])
    );

    let errors = artifact_json(&package, "generated/contracts/gadget/create.errors.json");
    let conflict = errors["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["literal"] == "idempotency_conflict")
        .expect("a changed request for a live key is typed-refused");
    assert_eq!(conflict["from"], json!("changed_canonical_command"));
    assert_eq!(conflict["detail"]["required"], json!(["field"]));

    let input = artifact_json(&package, "generated/contracts/gadget/create.input.json");
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

/// EXIT GATE: the claim's shape reaches the required-schema contract and the
/// data-access overlay, so nothing the emitted SQL names is left unpinned.
#[test]
fn generated_create_pins_and_grants_its_claim_relation() {
    let package = run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES).unwrap();

    let metadata = artifact_json(&package, "generated/package-weld.json");
    let claim = object_named(
        metadata["required_schema_contract"]["tables"]
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
        ["canonical_command", "gadget_id", "idempotency_key"]
    );
    assert_eq!(
        claim["constraints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|constraint| constraint["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "gadget_command_gadget_id_key",
            "gadget_command_idempotency_key_pkey",
        ]
    );

    let overlay = artifact_json(&package, DATA_ACCESS_OVERLAY_PATH);
    let relations = overlay["relations"].as_array().unwrap();
    let granted = object_named(relations, "table", CLAIM_TABLE);
    assert_eq!(
        granted["select_fields"],
        json!(["canonical_command", "gadget_id", "idempotency_key"])
    );
    assert_eq!(
        granted["insert_fields"],
        json!(["canonical_command", "idempotency_key"])
    );
    // A claim is written once: nothing may update it.
    assert_eq!(granted["update_fields"], json!([]));
    assert_eq!(granted["lock"], json!(false));
    // The model insert writes the claim-minted id, so the grant must allow it.
    let model = object_named(relations, "table", "gadget");
    assert_eq!(model["insert_fields"], json!(["id", "stock_id"]));
}

/// EXIT GATE: every way the claim could stop being the identity source refuses.
///
/// The unmutated manifest and catalog run FIRST as the negative control: if
/// they did not generate, a refusal below would show nothing.
#[test]
fn generated_create_refuses_a_claim_that_does_not_pre_generate_identity() {
    run(&claim_catalog(), &claim_manifest(), &QUERY_SOURCES)
        .expect("the unmutated claim generates");

    let model = || table(&catalog(false), "gadget").clone();
    let claim_table = |columns, constraints| {
        Table::new("inventory", CLAIM_TABLE, columns, constraints, Vec::new())
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
                manifest["models"]["gadget"]["operations"]["create"]
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
                manifest["models"]["gadget"]["operations"]["create"]["claim"]["table"] =
                    json!("absent_command");
                manifest["internal_relations"] = json!({
                    "absent_command": {
                        "schema": "inventory", "table": "absent_command", "cdc": "excluded"
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
                        "gadget_id",
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
                        "gadget_id",
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
                            "gadget_command_idempotency_key_pkey",
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
                        Constraint::unique("gadget_command_gadget_id_key", ["gadget_id"]).unwrap(),
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
                manifest["models"]["gadget"]["operations"]["create"]["claim"]["identities"] =
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
                manifest["models"]["gadget"]["operations"]["update"]["claim"] = json!({
                    "table": CLAIM_TABLE,
                    "identities": {"id": "gadget_id"}
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
        table(&catalog(false), "gadget"),
        table(&catalog(false), "gadget")
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
        table(&catalog(false), "gadget").constraints().to_vec(),
    );
    let claim = Table::new(
        "inventory",
        CLAIM_TABLE,
        claim_columns(),
        claim_constraints(),
        Vec::new(),
    );
    let unmapped = claim_catalog_with(with_second_identity.clone(), claim);
    let mut manifest = claim_manifest();
    manifest["models"]["gadget"]["server_owned_fields"] = json!([
        "id",
        "external_id",
        "part_code",
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
            "inventory",
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
                .chain([
                    Constraint::unique("gadget_command_external_id_key", ["external_id"]).unwrap(),
                ])
                .collect(),
            Vec::new(),
        ),
    );
    manifest["models"]["gadget"]["operations"]["create"]["claim"]["identities"] =
        json!({"external_id": "external_id", "id": "gadget_id"});
    let package = run(&mapped, &manifest, &QUERY_SOURCES).expect("both identities are claimed");
    assert_eq!(
        generated_create_sql(&package, "create"),
        "INSERT INTO gadget (external_id, id, stock_id)\n\
         VALUES ($1::uuid, $2::uuid, $3::uuid)\n\
         RETURNING\n    \
         created_at,\n    external_id,\n    id,\n    part_code,\n    \
         row_version,\n    status,\n    stock_id;\n",
    );
}

/// wamn-10yt.54. An exclusion violation is SQLSTATE 23P01. The generated
/// operation names it exactly as it names a unique or foreign-key violation,
/// carrying the constraint name in the `{constraint}` detail the matrix already
/// defines.
///
/// The reachable constraint is reachable ONLY through its dependency columns:
/// `stock_id` is the single field `update` writes, and it appears in no key
/// -- it sits inside the expression key. PostgreSQL records it as a dependency
/// of the constraint's index, which is what [`Exclusion::columns`] carries, so
/// intersecting written fields with that set is what names the refusal. The
/// second constraint depends on nothing this operation writes and is therefore
/// not named, so the contract does not over-declare.
#[test]
fn a_generated_operation_names_an_exclusion_violation_with_its_constraint() {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let with_exclusions = rebuilt_table(
        gadget,
        gadget.columns().to_vec(),
        gadget.constraints().to_vec(),
    )
    .with_exclusions(vec![
        Exclusion::new(
            "gadget_stock_window",
            ExclusionAccessMethod::Gist,
            vec![
                ExclusionKey::new(ExclusionElement::column("status"), "="),
                ExclusionKey::new(
                    ExclusionElement::expression("tstzrange(created_at, created_at)"),
                    "&&",
                ),
            ],
            ["status", "created_at", "stock_id"],
        )
        .unwrap(),
        Exclusion::new(
            "gadget_untouched_window",
            ExclusionAccessMethod::Gist,
            vec![ExclusionKey::new(ExclusionElement::column("status"), "=")],
            ["status", "created_at"],
        )
        .unwrap(),
    ]);
    let catalog = replacing_table(&base, with_exclusions);

    let manifest = manifest();
    let package = run(&catalog, &manifest, &QUERY_SOURCES).unwrap();
    let errors = artifact_json(&package, "generated/contracts/gadget/update.errors.json");
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
            "constraint": "gadget_stock_window",
            "detail": {"required": ["constraint"]}
        })],
        "the reachable exclusion is named once, in the same case shape a unique \
         violation uses"
    );
}

// ---------------------------------------------------------------------------
// Record history, level 1: the audit_log declaration (spec section 2, test 6).
// ---------------------------------------------------------------------------

/// The support catalog, with each named column added to `gadget`.
fn catalog_with_columns(columns: &[(&str, ColumnType, bool)]) -> CatalogIr {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let stamped = rebuilt_table(
        gadget,
        gadget
            .columns()
            .iter()
            .cloned()
            .chain(
                columns
                    .iter()
                    .map(|(name, ty, nullable)| Column::new(*name, *ty, *nullable, None, None)),
            )
            .collect(),
        gadget.constraints().to_vec(),
    );
    replacing_table(&base, stamped)
}

fn all_stamps_catalog() -> CatalogIr {
    catalog_with_columns(&[
        ("created_by", ColumnType::Uuid, false),
        ("updated_at", ColumnType::Timestamptz, false),
        ("updated_by", ColumnType::Uuid, false),
    ])
}

fn with_audit_log(columns: &Value, retention: &str) -> Value {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["audit_log"] = json!({"columns": columns, "retention": retention});
    manifest
}

fn all_stamps_manifest() -> Value {
    with_audit_log(
        &json!(["created_at", "created_by", "updated_at", "updated_by"]),
        "none",
    )
}

/// A package that overlays the `platform_gadget` purchase order.
fn overlay_manifest() -> Value {
    let mut manifest = manifest();
    manifest["package"]["id"] = json!("acme_inventory");
    manifest["base_dependencies"] = json!({"base_inventory": {
        "package": "platform_gadget",
        "version": "1.0.0",
        "digest": format!("sha256:{}", "a".repeat(64)),
        "operations": ["gadget.get"]
    }});
    manifest["models"]["gadget"]
        .as_object_mut()
        .unwrap()
        .remove("audit_log");
    manifest
}

/// One declared delete, in the shape the closed error-detail set requires.
///
/// The fixture relation carries no inbound foreign key, so no constraint code
/// belongs in the set.
fn delete_operation() -> Value {
    json!({
        "permission": "gadget.delete",

        "revision_field": "row_version",
        "result": "one"
    })
}

fn deleting_manifest(mode: &str) -> Value {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["delete_mode"] = json!(mode);
    manifest["models"]["gadget"]["operations"]["delete"] = delete_operation();
    manifest
}

fn tombstone_catalog() -> CatalogIr {
    catalog_with_columns(&[
        ("deleted_at", ColumnType::Timestamptz, true),
        ("deleted_by", ColumnType::Uuid, true),
    ])
}

fn statement(package: &wamn_schema_generator::GeneratedPackage, path: &str) -> String {
    std::str::from_utf8(package.file(path).unwrap().bytes())
        .unwrap()
        .to_owned()
}

#[test]
fn generated_delete_result_declares_only_the_successful_sql_outcome() {
    for (mode, catalog) in [("hard", catalog(false)), ("tombstone", tombstone_catalog())] {
        let package = run(&catalog, &deleting_manifest(mode), &QUERY_SOURCES).unwrap();
        let result = artifact_json(&package, "generated/contracts/gadget/delete.result.json");
        let outcome = result["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["path"] == "outcome")
            .expect("delete outcome field");
        assert_eq!(outcome["values"], json!(["deleted"]));
        let sql = statement(&package, "generated/sql/gadget/delete.sql");
        assert!(sql.contains("ELSE 'deleted'"), "{mode}: {sql}");
        assert!(sql.contains("THEN 'not_found'"), "{mode}: {sql}");
        assert!(sql.contains("THEN 'concurrency_conflict'"), "{mode}: {sql}");
    }
}

/// Owner rulings 1 and 3: the declared mode decides the statement, and only a
/// hard delete can meet an inbound key.
#[test]
fn a_hard_delete_removes_the_row_and_a_tombstone_marks_it() {
    let hard = run(&catalog(false), &deleting_manifest("hard"), &QUERY_SOURCES)
        .expect("a hard delete generates");
    let removal = statement(&hard, "generated/sql/gadget/delete.sql");
    assert!(removal.contains("DELETE FROM gadget AS model"), "{removal}");
    assert!(!removal.contains("deleted_at"), "{removal}");
    assert!(removal.contains("ELSE 'deleted'"), "{removal}");
    assert!(
        !statement(&hard, "generated/sql/gadget/get.sql").contains("deleted_at"),
        "a hard delete adds no predicate to a read"
    );

    let soft = run(
        &tombstone_catalog(),
        &deleting_manifest("tombstone"),
        &QUERY_SOURCES,
    )
    .expect("a tombstone delete generates");
    let marking = statement(&soft, "generated/sql/gadget/delete.sql");
    assert!(marking.contains("UPDATE gadget AS model"), "{marking}");
    assert!(
        marking.contains("deleted_at = transaction_timestamp()"),
        "{marking}"
    );
    assert!(
        marking.contains("deleted_by = NULLIF(current_setting('app.user_id', true), '')::uuid"),
        "{marking}"
    );
    assert!(!marking.contains("DELETE FROM"), "{marking}");
    for path in [
        "generated/sql/gadget/get.sql",
        "generated/sql/gadget/update.sql",
    ] {
        assert!(
            statement(&soft, path).contains("deleted_at IS NULL"),
            "{path} must hide a tombstoned row"
        );
    }
}

/// Authored commands declare their delete and every column that the SQL reads.
#[test]
fn authored_sql_deletes_only_from_a_hard_delete_model() {
    let authored = AuthoredSql::new(
        "query/quality_gadget_part.sql",
        b"WITH removed AS (DELETE FROM gadget WHERE id = $1 AND row_version = $2 RETURNING id) SELECT removed.id FROM removed;",
    );
    let mut operation = projection_operation();
    operation["kind"] = json!("command");
    operation["transaction"] = json!("explicit_per_input");
    operation["automatic_retry"] = json!(false);
    operation["idempotent_by"] = json!({"state": {"guards": {"gadget": "expected_row_version"}}});
    let revision = json!({"path": "expected_row_version", "type": "int64", "nullable": false});
    operation["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(revision);
    operation["statements"]["load_gadget_part"]["parameters"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "expected_row_version", "type": "int64", "nullable": false}));
    operation["relations"][0]["select_fields"] = json!(["id", "row_version"]);
    operation["relations"][0]["delete"] = json!(true);
    let mut sources = QUERY_SOURCES.to_vec();
    sources.push(authored);

    let generate = |manifest: &Value, catalog: &CatalogIr, operation: &Value| {
        let mut with_operation = manifest.clone();
        with_operation["custom_operations"]["quality.load_gadget_part"] = operation.clone();
        run(catalog, &with_operation, &sources)
    };
    generate(&deleting_manifest("hard"), &catalog(false), &operation)
        .expect("an authored hard delete generates with its declared access");

    for (field, value) in [
        ("delete", json!(false)),
        ("select_fields", json!(["id"])),
        ("select_fields", json!(["row_version"])),
    ] {
        let mut missing_access = operation.clone();
        missing_access["relations"][0][field] = value;
        let refusal = generate(&deleting_manifest("hard"), &catalog(false), &missing_access)
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("privilege declaration does not match verified SQL"),
            "{refusal}"
        );
    }

    let marked = generate(
        &deleting_manifest("tombstone"),
        &tombstone_catalog(),
        &operation,
    )
    .unwrap_err()
    .to_string();
    assert!(marked.contains("delete_mode: tombstone"), "{marked}");
    let undeclared = generate(&manifest(), &catalog(false), &operation)
        .unwrap_err()
        .to_string();
    assert!(
        undeclared.contains("declares no delete_mode"),
        "{undeclared}"
    );

    let mut projection = projection_operation();
    projection["relations"][0]["delete"] = json!(true);
    let refusal = generate(&deleting_manifest("hard"), &catalog(false), &projection)
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("must be read-only"), "{refusal}");

    // A delete with no column reads still declares relation access.
    operation["relations"][0]["select_fields"] = json!([]);
    let mut delete_only = deleting_manifest("hard");
    delete_only["custom_operations"]["quality.load_gadget_part"] = operation;
    sources.pop();
    sources.push(AuthoredSql::new(
        "query/quality_gadget_part.sql",
        b"WITH removed AS (DELETE FROM gadget) SELECT $1::uuid AS id WHERE $2::int8 IS NOT NULL;",
    ));
    run(&catalog(false), &delete_only, &sources).expect("delete alone is nonempty relation access");
}

#[test]
fn a_delete_mode_travels_with_its_delete_and_its_marker_columns() {
    let mut mode_without_delete = manifest();
    mode_without_delete["models"]["gadget"]["delete_mode"] = json!("hard");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&mode_without_delete))
            .expect_err("a delete_mode with no delete action")
            .kind(),
        GenerateErrorKind::InvalidModel,
    );

    let mut overlay = overlay_manifest();
    overlay["models"]["gadget"]["delete_mode"] = json!("tombstone");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&overlay))
            .expect_err("an overlay that declares a delete_mode")
            .kind(),
        GenerateErrorKind::InvalidModel,
    );

    for marker in ["deleted_at", "deleted_by"] {
        let ty = if marker == "deleted_at" {
            ColumnType::Timestamptz
        } else {
            ColumnType::Uuid
        };
        assert_eq!(
            run(
                &catalog_with_columns(&[(marker, ty, true)]),
                &manifest(),
                &QUERY_SOURCES,
            )
            .expect_err(marker)
            .to_string(),
            format!(
                "InvalidModel: gadget carries reserved column {marker} without a tombstone delete"
            ),
            "{marker}"
        );
    }
}

#[test]
fn selected_stamp_columns_become_server_owned() {
    let package = run(
        &all_stamps_catalog(),
        &all_stamps_manifest(),
        &QUERY_SOURCES,
    )
    .expect("a declaration that selects four valid columns generates");
    let input = artifact_json(&package, "generated/contracts/gadget/update.input.json");
    assert_eq!(
        input["server_owned_fields"]["fields"],
        json!([
            "id",
            "part_code",
            "status",
            "row_version",
            "created_at",
            "created_by",
            "updated_at",
            "updated_by"
        ])
    );
    // An overlay declares nothing, and the base stamp columns are server-owned.
    let overlay = run(&all_stamps_catalog(), &overlay_manifest(), &QUERY_SOURCES)
        .expect("an overlay inherits the owner's declaration");
    let input = artifact_json(&overlay, "generated/contracts/gadget/update.input.json");
    assert_eq!(
        input["server_owned_fields"]["fields"],
        json!([
            "id",
            "part_code",
            "status",
            "row_version",
            "created_at",
            "created_by",
            "updated_at",
            "updated_by"
        ])
    );
}

/// A catalog-free defect refuses in the shared vocabulary as well as in generation.
#[test]
fn audit_log_shape_refuses_without_a_catalog() {
    let mut missing = manifest();
    missing["models"]["gadget"]
        .as_object_mut()
        .unwrap()
        .remove("audit_log");
    let mut on_overlay = overlay_manifest();
    on_overlay["models"]["gadget"]["audit_log"] = json!({"columns": [], "retention": "none"});
    let cases = [
        ("a relation-owning model without the key", missing),
        ("an overlay model with the key", on_overlay),
        (
            "a repeated name",
            with_audit_log(
                &json!([
                    "created_at",
                    "created_at",
                    "created_by",
                    "updated_at",
                    "updated_by"
                ]),
                "none",
            ),
        ),
        (
            "created_by without created_at",
            with_audit_log(&json!(["created_by", "updated_at", "updated_by"]), "none"),
        ),
        (
            "updated_by without updated_at",
            with_audit_log(&json!(["created_at", "created_by", "updated_by"]), "none"),
        ),
    ];
    for (label, manifest) in cases {
        assert_eq!(
            validate_operation_vocabulary(&parsed_manifest(&manifest))
                .expect_err(label)
                .kind(),
            GenerateErrorKind::InvalidModel,
            "{label}"
        );
        assert_eq!(
            run(&all_stamps_catalog(), &manifest, &QUERY_SOURCES)
                .expect_err(label)
                .kind(),
            GenerateErrorKind::InvalidModel,
            "{label}"
        );
    }

    let outside = with_audit_log(&json!(["created_at", "deleted_at"]), "none");
    assert_eq!(
        run(&all_stamps_catalog(), &outside, &QUERY_SOURCES)
            .expect_err("a name outside the four refuses")
            .kind(),
        GenerateErrorKind::InvalidManifest
    );
}

/// A catalog defect names the column that the trigger cannot stamp.
#[test]
fn audit_log_columns_refuse_against_the_catalog() {
    let timestamps_only = with_audit_log(&json!(["created_at", "updated_at"]), "none");
    let mut overlay_added = overlay_manifest();
    overlay_added["models"]["gadget"]["field_owners"] = json!({"updated_by": "acme_inventory"});
    let cases = [
        (
            "an absent selected column",
            catalog(false),
            timestamps_only.clone(),
            "updated_at",
        ),
        (
            "a wrongly typed time",
            catalog_with_columns(&[("updated_at", ColumnType::Text, false)]),
            timestamps_only.clone(),
            "updated_at",
        ),
        (
            "a wrongly typed actor",
            catalog_with_columns(&[("created_by", ColumnType::Text, false)]),
            with_audit_log(&json!(["created_at", "created_by"]), "none"),
            "created_by",
        ),
        (
            "a nullable selected column",
            catalog_with_columns(&[("updated_at", ColumnType::Timestamptz, true)]),
            timestamps_only.clone(),
            "updated_at",
        ),
        (
            "an unselected reserved-name column",
            all_stamps_catalog(),
            timestamps_only,
            "created_by",
        ),
        (
            "an off declaration over a reserved-name column",
            catalog(false),
            with_audit_log(&json!([]), "none"),
            "created_at",
        ),
        (
            "an overlay-added reserved-name column",
            catalog_with_columns(&[("updated_by", ColumnType::Uuid, false)]),
            overlay_added,
            "updated_by",
        ),
    ];
    for (label, catalog, manifest, column) in cases {
        validate_operation_vocabulary(&parsed_manifest(&manifest))
            .unwrap_or_else(|error| panic!("{label} is a catalog defect: {error:?}"));
        let refusal = run(&catalog, &manifest, &QUERY_SOURCES).expect_err(label);
        assert_eq!(refusal.kind(), GenerateErrorKind::InvalidModel, "{label}");
        let object = format!("inventory.gadget.{column}");
        assert_eq!(refusal.object(), Some(object.as_str()), "{label}");
    }

    let mut writable = all_stamps_manifest();
    writable["models"]["gadget"]["operations"]["update"]["writable_fields"] =
        json!(["stock_id", "updated_by"]);
    assert_eq!(
        run(&all_stamps_catalog(), &writable, &QUERY_SOURCES)
            .expect_err("a selected column declared writable refuses")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

// ---------------------------------------------------------------------------
// Record history, level 2: retention, history tables, and the log trigger.
// ---------------------------------------------------------------------------

const ALL_STAMP_COLUMNS: [&str; 4] = ["created_at", "created_by", "updated_at", "updated_by"];

/// Spec test 12: a retention is none, unlimited, or whole days, with or without stamp columns.
#[test]
fn audit_log_retention_is_none_unlimited_or_whole_days() {
    for retention in ["none", "unlimited", "P1D", "P30D", "P365D"] {
        for columns in [json!([]), json!(ALL_STAMP_COLUMNS)] {
            validate_operation_vocabulary(&parsed_manifest(&with_audit_log(&columns, retention)))
                .unwrap_or_else(|error| panic!("{retention} with {columns} refused: {error:?}"));
        }
        run(
            &all_stamps_catalog(),
            &with_audit_log(&json!(ALL_STAMP_COLUMNS), retention),
            &QUERY_SOURCES,
        )
        .unwrap_or_else(|error| panic!("{retention} did not generate: {error:?}"));
    }
    for retention in [
        "",
        "forever",
        "P0D",
        "P01D",
        "P-1D",
        "P+1D",
        "P1.5D",
        "P1W",
        "P1M",
        "P1Y",
        "PT24H",
        "P1DT1H",
        "p30d",
        "UNLIMITED",
    ] {
        let manifest = with_audit_log(&json!(ALL_STAMP_COLUMNS), retention);
        assert_eq!(
            validate_operation_vocabulary(&parsed_manifest(&manifest))
                .expect_err(retention)
                .kind(),
            GenerateErrorKind::InvalidModel,
            "{retention}"
        );
        assert_eq!(
            run(&all_stamps_catalog(), &manifest, &QUERY_SOURCES)
                .expect_err(retention)
                .kind(),
            GenerateErrorKind::InvalidModel,
            "{retention}"
        );
    }
}

/// A logged relation keeps the schema description and grants the insert of each entry.
#[test]
fn a_logged_relation_keeps_the_schema_description_and_grants_its_history_insert() {
    let unlogged = run(
        &all_stamps_catalog(),
        &all_stamps_manifest(),
        &QUERY_SOURCES,
    )
    .unwrap();
    let logged = run(
        &all_stamps_catalog(),
        &with_audit_log(&json!(ALL_STAMP_COLUMNS), "P90D"),
        &QUERY_SOURCES,
    )
    .unwrap();
    assert_eq!(
        artifact_json(&logged, "generated/package-weld.json"),
        artifact_json(&unlogged, "generated/package-weld.json"),
        "the verified schema state id and the required schema contract stay"
    );
    let other_files = |package: &GeneratedPackage| {
        package
            .files()
            .iter()
            .filter(|file| file.path() != DATA_ACCESS_OVERLAY_PATH)
            .map(|file| (file.path().to_owned(), file.bytes().to_vec()))
            .collect::<Vec<_>>()
    };
    assert_eq!(other_files(&logged), other_files(&unlogged));

    let unlogged_overlay = artifact_json(&unlogged, DATA_ACCESS_OVERLAY_PATH);
    assert!(
        unlogged_overlay["relations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|relation| relation["table"] != "gadget_history")
    );
    let overlay = artifact_json(&logged, DATA_ACCESS_OVERLAY_PATH);
    assert_eq!(
        object_named(
            overlay["relations"].as_array().unwrap(),
            "table",
            "gadget_history"
        ),
        &json!({
            "schema": "inventory",
            "table": "gadget_history",
            "all_fields": [
                "after", "before", "changed_at", "changed_by", "kind", "operation", "position",
                "row_key", "transaction_id"
            ],
            "select_fields": [],
            "insert_fields": [
                "after", "before", "changed_at", "changed_by", "kind", "operation", "row_key",
                "transaction_id"
            ],
            "update_fields": [],
            "lock": false
        })
    );
}

/// A custom operation reads a history table, and the read adds only its select grant.
#[test]
fn a_custom_operation_reads_a_history_table() {
    let mut manifest = with_audit_log(&json!(ALL_STAMP_COLUMNS), "unlimited");
    let mut operation = projection_operation();
    operation["relations"][0]["table"] = json!("gadget_history");
    operation["relations"][0]["select_fields"] = json!(["changed_by", "kind"]);
    operation["statements"]["load_gadget_part"]["row"] =
        json!([{"name": "changed_by", "type": "uuid", "nullable": false}]);
    operation["result"]["fields"] =
        json!([{"path": "changed_by", "type": "uuid", "nullable": false}]);
    manifest["custom_operations"]["quality.load_gadget_part"] = operation;
    let mut sources = QUERY_SOURCES.to_vec();
    sources.push(AuthoredSql::new(
        "query/quality_gadget_part.sql",
        b"SELECT changed_by FROM gadget_history WHERE kind = $1;\n",
    ));

    let package = run(&all_stamps_catalog(), &manifest, &sources)
        .expect("a declared read of a history table generates");
    let overlay = artifact_json(&package, DATA_ACCESS_OVERLAY_PATH);
    let history = object_named(
        overlay["relations"].as_array().unwrap(),
        "table",
        "gadget_history",
    );
    assert_eq!(history["select_fields"], json!(["changed_by", "kind"]));
    assert_eq!(history["update_fields"], json!([]));
    assert_eq!(history["lock"], json!(false));

    let unlogged = with_audit_log(&json!(ALL_STAMP_COLUMNS), "none");
    let mut reads_unlogged = unlogged.clone();
    reads_unlogged["custom_operations"] = manifest["custom_operations"].clone();
    assert_eq!(
        run(&all_stamps_catalog(), &reads_unlogged, &sources)
            .expect_err("a relation that keeps no log has no history table to read")
            .kind(),
        GenerateErrorKind::UnknownRelation
    );
}

/// Spec test 17: generation refuses an authored history name and an overlong relation.
#[test]
fn history_names_are_reserved_and_fit_in_a_postgres_name() {
    let mut model_table = all_stamps_manifest();
    model_table["models"]["gadget"]["table"] = json!("gadget_history");
    let mut internal_table = manifest();
    internal_table["internal_relations"] =
        json!({"command": {"schema": "inventory", "table": "command_history", "cdc": "excluded"}});
    let mut internal_id = manifest();
    internal_id["internal_relations"] =
        json!({"command_history": {"schema": "inventory", "table": "command", "cdc": "excluded"}});
    let mut cases = vec![
        (
            "a model table",
            model_table,
            GenerateErrorKind::InvalidManifest,
            "inventory.gadget_history",
        ),
        (
            "an internal relation table",
            internal_table,
            GenerateErrorKind::InvalidManifest,
            "inventory.command_history",
        ),
        (
            "an internal relation id",
            internal_id,
            GenerateErrorKind::InvalidManifest,
            "inventory.command",
        ),
    ];
    for (label, insert, update, lock, delete) in [
        (
            "a declared insert",
            json!(["kind"]),
            json!([]),
            false,
            false,
        ),
        (
            "a declared update",
            json!([]),
            json!(["kind"]),
            false,
            false,
        ),
        ("a declared row lock", json!([]), json!([]), true, false),
        ("a declared delete", json!([]), json!([]), false, true),
    ] {
        let mut writes = with_audit_log(&json!(ALL_STAMP_COLUMNS), "unlimited");
        // An event handler can write, so the refusal is the history reservation.
        writes["custom_operations"]["gadget.record_history"] = json!({
            "kind": "event_handler",
            "visibility": "private",
            "connection": "postgres",
            "input": {"fields": [{"path": "new.id", "type": "uuid", "nullable": false}]},
            "errors": ["invalid_input", "retry", "timeout", "internal_error"],

            "relations": [{
                "schema": "inventory",
                "table": "gadget_history",
                "select_fields": [],
                "insert_fields": insert,
                "update_fields": update,
                "lock": lock,
                "delete": delete,
                "constraints": []
            }],
            "statements": {
                "write_history": {
                    "path": "command/record_history/write_history.sql",
                    "fetch": "optional_one",
                    "parameters": [{"name": "id", "type": "uuid", "nullable": false}],
                    "row": [{"name": "kind", "type": "text", "nullable": false}]
                }
            },
            "registration": {
                "source_package": "platform_gadget",
                "entity": "gadget",
                "ops": ["insert"]
            }
        });
        cases.push((
            label,
            writes,
            GenerateErrorKind::InvalidOperation,
            "inventory.gadget_history",
        ));
    }
    let long_relation = "r".repeat(32);
    let mut overlong = with_audit_log(&json!([]), "P30D");
    overlong["models"]["gadget"]["table"] = json!(long_relation);
    let long_object = format!("inventory.{long_relation}");
    cases.push((
        "a logged relation of 32 bytes",
        overlong,
        GenerateErrorKind::InvalidModel,
        &long_object,
    ));
    for (label, manifest, kind, object) in cases {
        let refusal = validate_operation_vocabulary(&parsed_manifest(&manifest)).expect_err(label);
        assert_eq!(refusal.kind(), kind, "{label}");
        assert_eq!(refusal.object(), Some(object), "{label}");
        assert_eq!(
            run(&all_stamps_catalog(), &manifest, &QUERY_SOURCES)
                .expect_err(label)
                .kind(),
            kind,
            "{label}"
        );
    }

    let mut fits = with_audit_log(&json!([]), "P30D");
    fits["models"]["gadget"]["table"] = json!("r".repeat(31));
    validate_operation_vocabulary(&parsed_manifest(&fits))
        .expect("a logged relation of 31 bytes fits its history names");
    let mut unlogged_long = with_audit_log(&json!([]), "none");
    unlogged_long["models"]["gadget"]["table"] = json!("r".repeat(32));
    validate_operation_vocabulary(&parsed_manifest(&unlogged_long))
        .expect("a relation that keeps no log derives no history name");
}

/// A logged relation needs a primary key.
#[test]
fn a_logged_relation_needs_a_primary_key() {
    let logged = with_audit_log(&json!(ALL_STAMP_COLUMNS), "P30D");
    let stamped = all_stamps_catalog();
    let gadget = table(&stamped, "gadget");
    let keyless = replacing_table(
        &stamped,
        rebuilt_table(
            gadget,
            gadget.columns().to_vec(),
            gadget
                .constraints()
                .iter()
                .filter(|constraint| constraint.name() != "gadget_id_pkey")
                .cloned()
                .collect(),
        ),
    );
    let refusal = run(&keyless, &logged, &QUERY_SOURCES).expect_err("a keyless logged relation");
    assert_eq!(refusal.kind(), GenerateErrorKind::InvalidModel);
    assert_eq!(refusal.object(), Some("inventory.gadget"));
}

#[test]
fn package_id_wamn_is_reserved_for_the_platform() {
    let mut reserved = manifest();
    reserved["package"]["id"] = json!("wamn");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&reserved))
            .expect_err("package id wamn was accepted")
            .kind(),
        GenerateErrorKind::InvalidIdentity
    );
    assert_eq!(
        run(&catalog(false), &reserved, &QUERY_SOURCES)
            .expect_err("generation accepted package id wamn")
            .kind(),
        GenerateErrorKind::InvalidIdentity
    );
}

/// A generated update moves the revision only when a supplied field changes.
#[test]
fn generated_update_keeps_the_revision_on_a_true_no_op() {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["operations"]["update"]["writable_fields"] =
        json!(["stock_id", "note"]);
    let catalog = catalog_with_columns(&[("note", ColumnType::Text, true)]);
    let package = run(&catalog, &manifest, &QUERY_SOURCES).unwrap();
    let sql = std::str::from_utf8(
        package
            .file("generated/sql/gadget/update.sql")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    let set = sql
        .split_once("    SET\n")
        .unwrap()
        .1
        .split_once("\n    FROM target\n")
        .unwrap()
        .0;
    assert_eq!(
        set,
        concat!(
            "        stock_id = CASE WHEN $3::boolean THEN $4::uuid ELSE model.stock_id END,\n",
            "        note = CASE WHEN $5::boolean THEN $6::text ELSE model.note END,\n",
            "        row_version = CASE\n",
            "            WHEN ($3::boolean AND $4::uuid::text IS DISTINCT FROM model.stock_id::text)\n",
            "            OR ($5::boolean AND $6::text::text IS DISTINCT FROM model.note::text)\n",
            "            THEN model.row_version + 1\n",
            "            ELSE model.row_version\n",
            "        END",
        )
    );
}

/// A generated update compares by text, so a numeric scale-only change moves the revision.
#[test]
fn generated_update_counts_a_numeric_scale_only_change() {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["operations"]["update"]["writable_fields"] =
        json!(["received_quantity"]);
    let catalog = catalog_with_columns(&[("received_quantity", ColumnType::Numeric, false)]);
    let package = run(&catalog, &manifest, &QUERY_SOURCES).unwrap();
    assert_eq!(
        std::str::from_utf8(
            package
                .file("generated/sql/gadget/update.sql")
                .unwrap()
                .bytes(),
        )
        .unwrap(),
        concat!(
            "WITH target AS MATERIALIZED (\n",
            "    SELECT id, row_version\n",
            "    FROM gadget\n",
            "    WHERE id = $1::uuid\n",
            "    FOR UPDATE\n",
            "),\n",
            "updated AS (\n",
            "    UPDATE gadget AS model\n",
            "    SET\n",
            "        received_quantity = CASE WHEN $3::boolean THEN $4::numeric ELSE model.received_quantity END,\n",
            "        row_version = CASE\n",
            "            WHEN ($3::boolean AND $4::numeric::text IS DISTINCT FROM model.received_quantity::text)\n",
            "            THEN model.row_version + 1\n",
            "            ELSE model.row_version\n",
            "        END\n",
            "    FROM target\n",
            "    WHERE model.id = target.id\n",
            "      AND target.row_version = $2::int8\n",
            "    RETURNING\n",
            "    model.created_at,\n",
            "    model.id,\n",
            "    model.part_code,\n",
            "    model.received_quantity,\n",
            "    model.row_version,\n",
            "    model.status,\n",
            "    model.stock_id\n",
            ")\n",
            "SELECT\n",
            "    CASE\n",
            "        WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'\n",
            "        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN 'concurrency_conflict'\n",
            "        ELSE 'updated'\n",
            "    END AS outcome,\n",
            "    (SELECT target.row_version FROM target) AS observed_row_version,\n",
            "    updated.created_at,\n",
            "    updated.id,\n",
            "    updated.part_code,\n",
            "    updated.received_quantity,\n",
            "    updated.row_version,\n",
            "    updated.status,\n",
            "    updated.stock_id\n",
            "FROM (SELECT 1) AS singleton\n",
            "LEFT JOIN updated ON TRUE;\n",
        )
    );
}
