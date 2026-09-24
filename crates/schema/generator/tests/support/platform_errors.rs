use serde_json::{Value, json};
use wamn_schema_generator::{
    DATA_ACCESS_OVERLAY_PATH, DataAccessOverlay, GenerateErrorKind, validate_operation_vocabulary,
};
use wamn_schema_introspection::ir::{CatalogIr, Constraint, Table};

use super::{artifact, fixture, parsed};

fn mapped_manifest() -> Value {
    let mut manifest = fixture::manifest();
    let operation = &mut manifest["custom_operations"]["widget.archive"];
    operation["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!("code_conflict"));
    operation["error_details"]["code_conflict"] = json!({"required": ["constraint"]});
    operation["constraint_errors"] = json!({"widget_code_key": "code_conflict"});
    operation["relations"][0]["constraints"] = json!(["widget_code_key"]);
    manifest
}

#[test]
fn custom_error_details_preserve_business_and_constraint_meanings() {
    let baseline = mapped_manifest();
    validate_operation_vocabulary(&parsed(&baseline)).unwrap();
    let mut authored = baseline.clone();
    authored["custom_operations"]["widget.archive"]["error_details"]["already_archived"] =
        json!({"required": ["id"]});
    validate_operation_vocabulary(&parsed(&authored)).unwrap();

    for (path, value) in [
        (
            "/error_details/already_archived/required",
            json!(["id", "id"]),
        ),
        ("/error_details/code_conflict/required", json!(["field"])),
        ("/constraint_errors/widget_code_key", json!("retry")),
    ] {
        let mut manifest = baseline.clone();
        *manifest["custom_operations"]["widget.archive"]
            .pointer_mut(path)
            .unwrap() = value;
        assert_eq!(
            validate_operation_vocabulary(&parsed(&manifest))
                .expect_err(path)
                .kind(),
            GenerateErrorKind::InvalidOperation
        );
    }
    let mut repeated = baseline.clone();
    repeated["custom_operations"]["widget.archive"]["constraint_errors"]["widget_id_pkey"] =
        json!("code_conflict");
    assert_eq!(
        validate_operation_vocabulary(&parsed(&repeated))
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
    let mut missing_permission = baseline;
    missing_permission["custom_operations"]["widget.archive"]["errors"]
        .as_array_mut()
        .unwrap()
        .retain(|error| error != "permission_denied");
    assert_eq!(
        validate_operation_vocabulary(&parsed(&missing_permission))
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn custom_constraints_follow_catalog_identity_and_kind() {
    let manifest = mapped_manifest();
    let catalog = fixture::catalog();
    let widget = catalog
        .tables()
        .iter()
        .find(|table| table.name() == "widget")
        .unwrap();
    for replacement in [
        None,
        Some(Constraint::check("widget_code_key", "code <> ''").unwrap()),
    ] {
        let mut constraints = widget
            .constraints()
            .iter()
            .filter(|constraint| constraint.name() != "widget_code_key")
            .cloned()
            .collect::<Vec<_>>();
        constraints.extend(replacement.clone());
        let changed = Table::new(
            widget.schema(),
            widget.name(),
            widget.columns().to_vec(),
            constraints,
            Vec::new(),
        );
        let changed_catalog = CatalogIr::new(
            catalog
                .tables()
                .iter()
                .map(|table| {
                    if table.name() == "widget" {
                        changed.clone()
                    } else {
                        table.clone()
                    }
                })
                .collect(),
        );
        let result = fixture::try_generate_with(&changed_catalog, &manifest);
        if replacement.is_none() {
            assert_eq!(
                result.unwrap_err().kind(),
                GenerateErrorKind::InvalidOperation
            );
        } else {
            let errors = artifact(
                &result.unwrap(),
                "generated/contracts/widget/archive.errors.json",
            );
            let mapped = errors["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["literal"] == "code_conflict")
                .unwrap();
            assert_eq!(mapped["from"], "check_violation");
        }
    }
    let mut missing_field = manifest;
    missing_field["custom_operations"]["widget.archive"]["relations"][0]["select_fields"] =
        json!(["missing"]);
    assert_eq!(
        fixture::try_generate_with(&catalog, &missing_field)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::UnknownColumn
    );
}

#[test]
fn custom_visibility_permissions_and_components_remain_closed() {
    for (field, value, kind) in [
        ("component", json!(""), GenerateErrorKind::InvalidComponent),
        (
            "permission",
            json!("widget.other"),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "registration",
            json!({"source_package": "platform_fixture", "entity": "widget", "ops": ["insert"]}),
            GenerateErrorKind::InvalidOperation,
        ),
        (
            "visibility",
            json!("private"),
            GenerateErrorKind::InvalidOperation,
        ),
    ] {
        let mut manifest = fixture::manifest();
        manifest["custom_operations"]["widget.archive"][field] = value;
        assert_eq!(
            validate_operation_vocabulary(&parsed(&manifest))
                .expect_err(field)
                .kind(),
            kind
        );
    }
    let mut projection = fixture::manifest();
    let operation = projection["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap();
    operation.insert("kind".into(), json!("projection"));
    for field in ["idempotent_by", "transaction", "automatic_retry"] {
        operation.remove(field);
    }
    operation["relations"][0]["lock"] = json!(false);
    operation["errors"]
        .as_array_mut()
        .unwrap()
        .retain(|error| error != "concurrency_conflict");
    validate_operation_vocabulary(&parsed(&projection)).expect("valid read-only projection");
    projection["custom_operations"]["widget.archive"]["relations"][0]["update_fields"] =
        json!(["id"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed(&projection))
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn pre_commit_generation_and_canonical_authority_use_platform_declarations() {
    let mut manifest = super::platform_claim::manifest();
    manifest["custom_operations"]["widget.archive"]["pre_commit"] = json!({"fields": [
        {"path": "widget_id", "type": "uuid", "nullable": false}
    ]});
    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    let operation = artifact(
        &package,
        "generated/contracts/widget/archive.operation.json",
    );
    assert_eq!(operation["transaction"], "explicit_per_input");
    assert_eq!(operation["automatic_retry"], false);
    assert_eq!(
        operation["pre_commit"],
        "platform-fixture:widget/archive-pre-commit@1.0.0"
    );
    for (path, expected) in [
        (
            "generated/wit/deps/platform-fixture-widget/package.wit",
            "run: async func(ctx: node-context, input: archive-pre-commit-request) -> result<archive-pre-commit-request, node-error>",
        ),
        (
            "generated/wit/widget_archive_codec.rs",
            "$handler(context.clone(), state, request).await",
        ),
        (
            "generated/wamn/widget_archive.rs",
            "async fn select_participant(",
        ),
    ] {
        assert!(
            std::str::from_utf8(package.file(path).unwrap().bytes())
                .unwrap()
                .contains(expected),
            "{path}"
        );
    }
    let bytes = package.file(DATA_ACCESS_OVERLAY_PATH).unwrap().bytes();
    DataAccessOverlay::from_slice(bytes).unwrap();
    let mut noncanonical = bytes.to_vec();
    noncanonical.push(b'\n');
    assert_eq!(
        DataAccessOverlay::from_slice(&noncanonical)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidManifest
    );
    let mut read_only = fixture::manifest();
    read_only["models"]["widget"]["operations"] = json!({});
    read_only["models"]["widget"]
        .as_object_mut()
        .unwrap()
        .remove("delete_mode");
    let package = fixture::generate_with(&fixture::catalog(), &read_only);
    let overlay = artifact(&package, DATA_ACCESS_OVERLAY_PATH);
    assert_eq!(overlay["role"], "wamn_app");
    assert_eq!(overlay["contract"], "platform_fixture_data_access");
    let relation = overlay["relations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|relation| relation["table"] == "widget")
        .unwrap();
    // `widget.list` reads every column: its attributes are the whole row.
    assert_eq!(
        relation["select_fields"],
        json!([
            "code",
            "created_at",
            "edit_version",
            "id",
            "maker_id",
            "note"
        ])
    );
    assert_eq!(relation["insert_fields"], json!([]));
    assert_eq!(relation["update_fields"], json!([]));
    assert_eq!(relation["lock"], true);
    // The carrier is the first select field when a relation writes none, so
    // it follows the union of what every operation reads.
    assert_eq!(relation["lock_update_field"], "code");
}

#[test]
fn an_optional_pre_commit_slot_generates_its_no_op_participant() {
    let mut manifest = super::platform_claim::manifest();
    manifest["custom_operations"]["widget.archive"]["pre_commit"] = json!({"fields": [
        {"path": "widget_id", "type": "uuid", "nullable": false}
    ]});
    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    let source = |path: &str| {
        std::str::from_utf8(package.file(path).expect(path).bytes())
            .unwrap()
            .to_owned()
    };
    let cargo = source("generated/widget_archive-no-op/Cargo.toml");
    assert!(
        cargo.contains("name = \"platform-fixture-widget-archive-no-op\""),
        "{cargo}"
    );
    let library = source("generated/widget_archive-no-op/src/lib.rs");
    for expected in [
        "export platform-fixture:widget/archive-pre-commit@1.0.0;",
        "\"../wit/deps/platform-fixture-widget\"",
        "input: ArchivePreCommitRequest,",
        "std::future::ready(Ok(input))",
    ] {
        assert!(library.contains(expected), "{expected}\n{library}");
    }
    assert!(
        artifact(
            &package,
            "generated/contracts/widget/archive.operation.json"
        )
        .get("pre_commit_required")
        .is_none()
    );

    manifest["custom_operations"]["widget.archive"]["pre_commit_required"] = json!(true);
    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    assert!(
        package
            .file("generated/widget_archive-no-op/src/lib.rs")
            .is_none(),
        "a required slot has no default participant"
    );
    assert_eq!(
        artifact(
            &package,
            "generated/contracts/widget/archive.operation.json"
        )["pre_commit_required"],
        true
    );

    manifest["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap()
        .remove("pre_commit");
    let error = wamn_schema_generator::validate_operation_vocabulary(
        &wamn_schema_generator::PackageManifest::from_slice(
            &serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap(),
    )
    .expect_err("a required slot without a pre_commit was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
}
