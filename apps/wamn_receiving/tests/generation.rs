use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use wamn_schema_generator::{
    AuthoredSql, DATA_ACCESS_OVERLAY_PATH, DataAccessOverlay, GenerateErrorKind, GeneratedPackage,
    GenerationInput, GenerationProvenance, OperationVisibility, PackageManifest,
    StatementTransactionality, generate, validate_operation_vocabulary, validate_parity_json,
};
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnType, Constraint, Exclusion, ExclusionAccessMethod,
    ExclusionElement, ExclusionKey, ForeignKeyAction, ForeignKeyColumn, Table,
};

#[path = "../../../crates/schema/generator/tests/support/generation.rs"]
mod support;
use support::{
    QUERY_SOURCES, accessor_bind, artifact_json, assert_native_fixtures_match_parity, catalog,
    manifest, object_named, parsed_manifest, projection_operation, rebuilt_table, replacing_table,
    run, statement_digest, table,
};

const RECEIVING_MANIFEST: &[u8] = include_bytes!("../wamn.json");
const RECEIVING_SOURCES: [AuthoredSql<'static>; 17] = [
    AuthoredSql::new(
        "command/record_receipt/claim_command.sql",
        include_bytes!("../command/record_receipt/claim_command.sql"),
    ),
    AuthoredSql::new(
        "command/record_receipt/finalize_command.sql",
        include_bytes!(
            "../command/record_receipt/finalize_command.sql"
        ),
    ),
    AuthoredSql::new(
        "command/record_receipt/find_replay.sql",
        include_bytes!("../command/record_receipt/find_replay.sql"),
    ),
    AuthoredSql::new(
        "command/record_receipt/finish_purchase_order.sql",
        include_bytes!(
            "../command/record_receipt/finish_purchase_order.sql"
        ),
    ),
    AuthoredSql::new(
        "command/record_receipt/insert_receipt.sql",
        include_bytes!("../command/record_receipt/insert_receipt.sql"),
    ),
    AuthoredSql::new(
        "command/record_receipt/insert_receipt_line.sql",
        include_bytes!(
            "../command/record_receipt/insert_receipt_line.sql"
        ),
    ),
    AuthoredSql::new(
        "command/record_receipt/lock_purchase_order.sql",
        include_bytes!(
            "../command/record_receipt/lock_purchase_order.sql"
        ),
    ),
    AuthoredSql::new(
        "command/record_receipt/update_purchase_order_line.sql",
        include_bytes!(
            "../command/record_receipt/update_purchase_order_line.sql"
        ),
    ),
    AuthoredSql::new(
        "command/record_receipt/validate_receipt_line.sql",
        include_bytes!(
            "../command/record_receipt/validate_receipt_line.sql"
        ),
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_purchase_order_number_ascending.sql",
        include_bytes!(
            "../query/open_purchase_order_by_purchase_order_number_ascending.sql"
        ),
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_purchase_order_number_descending.sql",
        include_bytes!(
            "../query/open_purchase_order_by_purchase_order_number_descending.sql"
        ),
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_status_ascending.sql",
        include_bytes!(
            "../query/open_purchase_order_by_status_ascending.sql"
        ),
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_status_descending.sql",
        include_bytes!(
            "../query/open_purchase_order_by_status_descending.sql"
        ),
    ),
    AuthoredSql::new(
        "query/open_purchase_order.sql",
        include_bytes!("../query/open_purchase_order.sql"),
    ),
    AuthoredSql::new(
        "query/open_purchase_order_by_created_at_descending.sql",
        include_bytes!(
            "../query/open_purchase_order_by_created_at_descending.sql"
        ),
    ),
    AuthoredSql::new(
        "query/load_receipt_screen.sql",
        include_bytes!("../query/load_receipt_screen.sql"),
    ),
    AuthoredSql::new(
        "query/location.sql",
        include_bytes!("../query/location.sql"),
    ),
];

fn receiving_catalog() -> CatalogIr {
    let item = Table::new(
        "receiving",
        "item",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("item_number", ColumnType::Text, false, None, None),
        ],
        vec![
            Constraint::primary_key("item_id_pkey", ["id"]).unwrap(),
            Constraint::unique("item_item_number_key", ["item_number"]).unwrap(),
        ],
        Vec::new(),
    );
    let location = Table::new(
        "receiving",
        "location",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("location_code", ColumnType::Text, false, None, None),
        ],
        vec![Constraint::primary_key("location_id_pkey", ["id"]).unwrap()],
        Vec::new(),
    );
    let purchase_order = Table::new(
        "receiving",
        "purchase_order",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("purchase_order_number", ColumnType::Text, false, None, None),
            Column::new("supplier_id", ColumnType::Uuid, false, None, None),
            Column::new(
                "status",
                ColumnType::Text,
                false,
                Some(ColumnDefault::text("open")),
                None,
            ),
            Column::new(
                "row_version",
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
            Column::new(
                "updated_at",
                ColumnType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
        ],
        vec![
            Constraint::primary_key("purchase_order_id_pkey", ["id"]).unwrap(),
            Constraint::unique(
                "purchase_order_purchase_order_number_key",
                ["purchase_order_number"],
            )
            .unwrap(),
            Constraint::check(
                "purchase_order_status_check",
                "status = ANY (ARRAY['open'::text, 'complete'::text, 'cancelled'::text])",
            )
            .unwrap(),
        ],
        Vec::new(),
    );
    let purchase_order_line = Table::new(
        "receiving",
        "purchase_order_line",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("purchase_order_id", ColumnType::Uuid, false, None, None),
            Column::new("line_number", ColumnType::Int32, false, None, None),
            Column::new("item_id", ColumnType::Uuid, false, None, None),
            Column::new("ordered_quantity", ColumnType::Numeric, false, None, None),
            Column::new(
                "received_quantity",
                ColumnType::Numeric,
                false,
                Some(ColumnDefault::numeric("0")),
                None,
            ),
        ],
        vec![Constraint::primary_key("purchase_order_line_id_pkey", ["id"]).unwrap()],
        Vec::new(),
    );
    let record_receipt_command = Table::new(
        "receiving",
        "record_receipt_command",
        vec![
            Column::new("idempotency_key", ColumnType::Text, false, None, None),
            Column::new("canonical_command", ColumnType::Bytes, false, None, None),
            Column::new(
                "receipt_id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("purchase_order_id", ColumnType::Uuid, false, None, None),
            Column::new("purchase_order_status", ColumnType::Text, true, None, None),
            Column::new("row_version", ColumnType::Int64, true, None, None),
        ],
        vec![
            Constraint::primary_key(
                "record_receipt_command_idempotency_key_pkey",
                ["idempotency_key"],
            )
            .unwrap(),
            Constraint::unique("record_receipt_command_receipt_id_key", ["receipt_id"]).unwrap(),
        ],
        Vec::new(),
    );
    let receipt = Table::new(
        "receiving",
        "receipt",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("idempotency_key", ColumnType::Text, false, None, None),
            Column::new("purchase_order_id", ColumnType::Uuid, false, None, None),
            Column::new("receipt_reference", ColumnType::Text, false, None, None),
            Column::new("occurred_at", ColumnType::Timestamptz, false, None, None),
            Column::new(
                "created_at",
                ColumnType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
        ],
        vec![
            Constraint::primary_key("receipt_id_pkey", ["id"]).unwrap(),
            Constraint::unique("receipt_idempotency_key_key", ["idempotency_key"]).unwrap(),
            Constraint::foreign_key(
                "receipt_purchase_order_id_fkey",
                vec![ForeignKeyColumn::new("purchase_order_id", "id")],
                "receiving",
                "purchase_order",
                ForeignKeyAction::NoAction,
                ForeignKeyAction::NoAction,
            )
            .unwrap(),
            Constraint::unique(
                "receipt_purchase_order_id_receipt_reference_key",
                ["purchase_order_id", "receipt_reference"],
            )
            .unwrap(),
        ],
        Vec::new(),
    );
    let receipt_line = Table::new(
        "receiving",
        "receipt_line",
        vec![
            Column::new(
                "id",
                ColumnType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("receipt_id", ColumnType::Uuid, false, None, None),
            Column::new(
                "purchase_order_line_id",
                ColumnType::Uuid,
                false,
                None,
                None,
            ),
            Column::new("quantity", ColumnType::Numeric, false, None, None),
            Column::new("location_id", ColumnType::Uuid, false, None, None),
        ],
        vec![Constraint::primary_key("receipt_line_id_pkey", ["id"]).unwrap()],
        Vec::new(),
    );
    CatalogIr::new(vec![
        item,
        location,
        purchase_order,
        purchase_order_line,
        record_receipt_command,
        receipt,
        receipt_line,
    ])
}

fn shipped_manifest() -> Value {
    serde_json::from_slice(RECEIVING_MANIFEST).unwrap()
}

fn event_handler_operation() -> Value {
    json!({
        "kind": "event_handler",
        "visibility": "private",
        "connection": "postgres",
        "input": {
            "fields": [
                {"path": "event", "type": "text", "nullable": false, "values": ["insert"]},
                {"path": "new.id", "type": "uuid", "nullable": false}
            ]
        },
        "errors": ["invalid_input", "retry", "timeout", "internal_error"],
        "error_details": {
            "invalid_input": {"required": ["field"]},
            "retry": {},
            "timeout": {},
            "internal_error": {}
        },
        "relations": [{
            "schema": "receiving",
            "table": "location",
            "select_fields": ["id"],
            "insert_fields": [],
            "update_fields": [],
            "lock": false,
            "constraints": []
        }],
        "statements": {
            "load_location": {
                "path": "command/create_inspection/load_location.sql",
                "fetch": "optional_one",
                "parameters": [{"name": "id", "type": "uuid", "nullable": false}],
                "row": [{"name": "id", "type": "uuid", "nullable": false}]
            }
        },
        "registration": {
            "source_package": "wamn_receiving",
            "entity": "receipt",
            "ops": ["insert"]
        }
    })
}

fn generic_operation_sources() -> Vec<AuthoredSql<'static>> {
    let mut sources = RECEIVING_SOURCES
        .iter()
        .copied()
        .filter(|source| source.path().starts_with("query/open_purchase_order"))
        .collect::<Vec<_>>();
    sources.push(AuthoredSql::new(
        "query/quality_purchase_order_detail.sql",
        b"SELECT id FROM purchase_order WHERE id = $1;\n",
    ));
    sources.push(AuthoredSql::new(
        "command/create_inspection/load_location.sql",
        b"SELECT id FROM location WHERE id = $1;\n",
    ));
    sources
}

fn generic_custom_operation_manifest() -> Value {
    let mut manifest = shipped_manifest();
    manifest["custom_operations"] = json!({
        "quality.load_purchase_order_detail": projection_operation(),
        "quality.create_inspection": event_handler_operation(),
    });
    manifest
}

fn shipped_generation(
    catalog: &CatalogIr,
    manifest: &Value,
) -> Result<GeneratedPackage, wamn_schema_generator::GenerateError> {
    run(catalog, manifest, &RECEIVING_SOURCES)
}

fn overlay_manifest() -> Value {
    let mut overlay = manifest();
    let mut composition =
        shipped_manifest()["custom_operations"]["receiving.record_receipt"].clone();
    let composition_object = composition.as_object_mut().unwrap();
    for field in [
        "connection",
        "relations",
        "statements",
        "transaction",
        "automatic_retry",
        "claim",
        "canonicalization",
        "constraint_errors",
    ] {
        composition_object.remove(field);
    }
    // The composed command holds no claim of its own. It rides the base claim,
    // and it says so rather than leaving generation to infer it.
    composition_object.insert(
        "idempotent_by".to_owned(),
        json!({"inherited": {"base": "base_receiving", "operation": "receiving.record_receipt"}}),
    );

    overlay["package"] = json!({"id": "client_acme_receiving", "version": "3.0.0"});
    overlay["base_dependencies"] = json!({
        "base_receiving": {
            "package": "wamn_receiving",
            "version": "1.0.0",
            "digest": format!("sha256:{}", "a".repeat(64)),
            "operations": ["receiving.record_receipt"]
        }
    });
    overlay["models"]["purchase_order"]["owner"] = json!("wamn_receiving");
    overlay["models"]["purchase_order"]["field_owners"] = json!({
        "supplier_id": "client_acme_receiving"
    });
    overlay["models"]["purchase_order"]["constraint_owners"] = json!({
        "purchase_order_status_check": "wamn_receiving"
    });
    overlay["custom_operations"] = json!({
        "receiving.record_receipt": composition,
        "quality.load_purchase_order_detail": projection_operation(),
        "quality.create_inspection": event_handler_operation(),
    });
    overlay["components"] = json!({
        "client_acme_receiving": {
            "connections": ["postgres"]
        }
    });
    overlay
}

#[test]
fn overlay_vocabulary_is_exact_at_dependency_definition_and_operation_grain() {
    let mut base = manifest();
    base["models"]["purchase_order"]["client_field_extensible"] = json!(true);
    let base = parsed_manifest(&base);
    assert!(base.models["purchase_order"].client_field_extensible);

    let overlay = overlay_manifest();
    let parsed = parsed_manifest(&overlay);
    let dependency = &parsed.base_dependencies["base_receiving"];
    assert_eq!(dependency.package, "wamn_receiving");
    assert_eq!(dependency.version, "1.0.0");
    assert_eq!(
        dependency.operations,
        vec!["receiving.record_receipt".to_owned()]
    );
    let model = &parsed.models["purchase_order"];
    assert_eq!(model.owner, "wamn_receiving");
    assert_eq!(model.field_owner("supplier_id"), "client_acme_receiving");
    assert_eq!(model.field_owner("status"), "wamn_receiving");
    assert_eq!(
        model.constraint_owner("purchase_order_status_check"),
        "wamn_receiving"
    );
    let projection = &parsed.custom_operations["quality.load_purchase_order_detail"];
    assert_eq!(projection.visibility(), OperationVisibility::Public);
    assert_eq!(projection.component(), None);
    assert_eq!(
        projection.permission(),
        Some("quality.load_purchase_order_detail")
    );
    let handler = &parsed.custom_operations["quality.create_inspection"];
    assert_eq!(handler.visibility(), OperationVisibility::Private);
    assert_eq!(handler.permission(), None);
    let registration = handler.registration().expect("event registration");
    assert_eq!(registration.source_package, "wamn_receiving");
    assert_eq!(registration.entity, "receipt");
    assert_eq!(registration.ops, [wamn_event_wire::Op::Insert]);

    let generated = run(&receiving_catalog(), &overlay, &generic_operation_sources())
        .expect("generic overlay custom operations must generate from one strict declaration");
    for suffix in ["operation", "input", "result", "errors"] {
        generated
            .file(&format!(
                "generated/contracts/receiving/record_receipt.{suffix}.json"
            ))
            .unwrap();
    }
    for path in [
        "generated/native-verifier/receiving_record_receipt.rs",
        "generated/wamn/receiving_record_receipt.rs",
        "generated/parity/receiving_record_receipt.json",
    ] {
        assert!(
            generated.file(path).is_none(),
            "composition-only command emitted a local SQL artifact: {path}"
        );
    }
    let composition = artifact_json(
        &generated,
        "generated/source-map/receiving_record_receipt.json",
    );
    assert_eq!(composition["composition"]["alias"], "base_receiving");
    assert_eq!(composition["composition"]["package"], "wamn_receiving");
    assert_eq!(composition["composition"]["version"], "1.0.0");
    assert_eq!(
        composition["composition"]["digest"],
        format!("sha256:{}", "a".repeat(64))
    );
    assert!(
        generated
            .file("generated/contracts/quality/create_inspection.result.json")
            .is_none()
    );

    let mut unbacked_composition = overlay;
    unbacked_composition["base_dependencies"] = json!({});
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&unbacked_composition))
            .expect_err("an unbacked SQL-less command was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut ambiguous_composition = overlay_manifest();
    ambiguous_composition["base_dependencies"]["second_base"] = json!({
        "package": "other_receiving",
        "version": "1.0.0",
        "digest": format!("sha256:{}", "b".repeat(64)),
        "operations": ["receiving.record_receipt"]
    });
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&ambiguous_composition))
            .expect_err("an ambiguous composition dependency was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn dependency_composition_may_add_one_declared_post_call_projection() {
    let mut overlay = overlay_manifest();
    let projection = projection_operation();
    let operation = overlay["custom_operations"]["receiving.record_receipt"]
        .as_object_mut()
        .unwrap();
    operation.insert("connection".to_owned(), json!("postgres"));
    operation.insert("transaction".to_owned(), json!("explicit_per_input"));
    operation.insert("automatic_retry".to_owned(), json!(false));
    operation.insert("relations".to_owned(), projection["relations"].clone());
    operation.insert("statements".to_owned(), projection["statements"].clone());

    let generated = run(&receiving_catalog(), &overlay, &generic_operation_sources())
        .expect("an exact dependency may be followed by declared local projection SQL");
    let contract = artifact_json(
        &generated,
        "generated/contracts/receiving/record_receipt.operation.json",
    );
    let statements = contract["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 1);
    assert_eq!(
        statements[0]["path"],
        "query/quality_purchase_order_detail.sql"
    );
    assert_eq!(statements[0]["name"], "load_purchase_order_detail");
    let source = generic_operation_sources()
        .into_iter()
        .find(|source| source.path() == "query/quality_purchase_order_detail.sql")
        .unwrap();
    assert_eq!(statements[0]["digest"], statement_digest(source.bytes()));
    assert_eq!(contract["dependency"]["alias"], "base_receiving");
    let source_map = artifact_json(
        &generated,
        "generated/source-map/receiving_record_receipt.json",
    );
    assert_eq!(source_map["composition"]["package"], "wamn_receiving");
    assert_eq!(source_map["command"], "receiving.record_receipt");
    generated
        .file("generated/wamn/receiving_record_receipt.rs")
        .expect("declared post-call SQL generates its Wamn accessor");
}

#[test]
fn overlay_vocabulary_refuses_ranges_opaque_digests_and_unknown_definitions() {
    let mut range = overlay_manifest();
    range["base_dependencies"]["base_receiving"]["version"] = json!("^1.0");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&range))
            .expect_err("a base version range was accepted")
            .kind(),
        GenerateErrorKind::InvalidIdentity
    );

    let mut digest = overlay_manifest();
    digest["base_dependencies"]["base_receiving"]["digest"] = json!("latest");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&digest))
            .expect_err("an opaque base artifact identity was accepted")
            .kind(),
        GenerateErrorKind::InvalidIdentity
    );

    let mut field = overlay_manifest();
    field["models"]["purchase_order"]["field_owners"] = json!({
        "unknown_field": "client_acme_receiving"
    });
    assert_eq!(
        run(&catalog(false), &field, &QUERY_SOURCES)
            .expect_err("ownership of an unknown field was accepted")
            .kind(),
        GenerateErrorKind::UnknownColumn
    );

    let mut constraint = overlay_manifest();
    constraint["models"]["purchase_order"]["constraint_owners"] = json!({
        "unknown_constraint": "client_acme_receiving"
    });
    assert_eq!(
        run(&catalog(false), &constraint, &QUERY_SOURCES)
            .expect_err("ownership of an unknown constraint was accepted")
            .kind(),
        GenerateErrorKind::InvalidModel
    );

    let mut owner = overlay_manifest();
    owner["models"]["purchase_order"]["field_owners"] = json!({
        "supplier_id": "undeclared_package"
    });
    assert_eq!(
        run(&catalog(false), &owner, &QUERY_SOURCES)
            .expect_err("an undeclared definition owner was accepted")
            .kind(),
        GenerateErrorKind::InvalidModel
    );

    let mut restated_extensibility = overlay_manifest();
    restated_extensibility["models"]["purchase_order"]["client_field_extensible"] = json!(true);
    assert_eq!(
        run(&catalog(false), &restated_extensibility, &QUERY_SOURCES)
            .expect_err("an overlay restated base extensibility")
            .kind(),
        GenerateErrorKind::InvalidModel
    );
}

fn exclusion_owner_fixture() -> (CatalogIr, Value) {
    let catalog = receiving_catalog();
    let purchase_order = table(&catalog, "purchase_order");
    let purchase_order = rebuilt_table(
        purchase_order,
        purchase_order.columns().to_vec(),
        purchase_order.constraints().to_vec(),
    )
    .with_exclusions(vec![
        Exclusion::new(
            "purchase_order_supplier_excl",
            ExclusionAccessMethod::Gist,
            vec![ExclusionKey::new(
                ExclusionElement::column("supplier_id"),
                "=",
            )],
            ["supplier_id"],
        )
        .unwrap(),
    ]);
    let mut manifest = overlay_manifest();
    manifest["models"]["purchase_order"]["operations"]["update"]["error_details"]["exclusion_violation"] =
        json!({"required": ["constraint"]});
    (replacing_table(&catalog, purchase_order), manifest)
}

#[test]
fn exclusion_owners_accept_the_package_and_a_declared_base() {
    let (catalog, mut manifest) = exclusion_owner_fixture();
    for owner in ["client_acme_receiving", "wamn_receiving"] {
        manifest["models"]["purchase_order"]["constraint_owners"] = json!({
            "purchase_order_supplier_excl": owner
        });
        let package = run(&catalog, &manifest, &generic_operation_sources())
            .expect("an actual exclusion may belong to the package or a declared base");
        let errors = artifact_json(
            &package,
            "generated/contracts/purchase_order/update.errors.json",
        );
        assert_eq!(errors["closed"], json!(true));
        let exclusions = errors["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["literal"] == "exclusion_violation")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            exclusions,
            vec![json!({
                "literal": "exclusion_violation",
                "from": "exclusion_violation",
                "constraint": "purchase_order_supplier_excl",
                "detail": {"required": ["constraint"]}
            })],
            "the declared owner {owner} preserves the exact reachable refusal"
        );
    }
}

#[test]
fn exclusion_owners_reject_unknown_constraints() {
    let (catalog, mut manifest) = exclusion_owner_fixture();
    manifest["models"]["purchase_order"]["constraint_owners"] = json!({
        "purchase_order_unknown_excl": "client_acme_receiving"
    });
    let error = run(&catalog, &manifest, &generic_operation_sources())
        .expect_err("an existing exclusion does not admit an unknown constraint name");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidModel);
    assert_eq!(
        error.object(),
        Some("receiving.purchase_order.purchase_order_unknown_excl")
    );
    assert_eq!(
        error.to_string(),
        "InvalidModel: purchase_order owns unknown constraint purchase_order_unknown_excl"
    );
}

#[test]
fn exclusion_owners_reject_undeclared_owners() {
    let (catalog, mut manifest) = exclusion_owner_fixture();
    manifest["models"]["purchase_order"]["constraint_owners"] = json!({
        "purchase_order_supplier_excl": "undeclared_package"
    });
    let error = run(&catalog, &manifest, &generic_operation_sources())
        .expect_err("an existing exclusion does not admit an undeclared owner");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidModel);
    assert_eq!(error.object(), None);
    assert_eq!(
        error.to_string(),
        "InvalidModel: purchase_order.purchase_order_supplier_excl owner undeclared_package is not the package or a declared base"
    );
}

#[test]
fn custom_operation_kinds_visibility_permissions_and_registration_are_closed() {
    let mut empty_component = overlay_manifest();
    empty_component["custom_operations"]["quality.load_purchase_order_detail"]["component"] =
        json!("");
    let error = validate_operation_vocabulary(&parsed_manifest(&empty_component))
        .expect_err("an empty custom-operation component was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidComponent);
    assert_eq!(
        error.context(),
        "operation quality.load_purchase_order_detail component must not be empty"
    );

    let mut private_permission = overlay_manifest();
    private_permission["custom_operations"]["quality.create_inspection"]["permission"] =
        json!("quality.create_inspection");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&private_permission))
            .expect_err("a private operation permission was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut public_permission = overlay_manifest();
    public_permission["custom_operations"]["quality.load_purchase_order_detail"]["permission"] =
        json!("quality.other_projection");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&public_permission))
            .expect_err("an inexact public operation permission was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut write_projection = overlay_manifest();
    write_projection["custom_operations"]["quality.load_purchase_order_detail"]["relations"][0]["update_fields"] =
        json!(["id"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&write_projection))
            .expect_err("a write-capable projection was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut missing_registration = overlay_manifest();
    missing_registration["custom_operations"]["quality.create_inspection"]
        .as_object_mut()
        .expect("event handler object")
        .remove("registration");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&missing_registration))
            .expect_err("an event handler without a registration was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut projection_registration = overlay_manifest();
    projection_registration["custom_operations"]["quality.load_purchase_order_detail"]["registration"] =
        json!({"source_package": "wamn_receiving", "entity": "receipt", "ops": ["insert"]});
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&projection_registration))
            .expect_err("a projection registration was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    for ops in [json!([]), json!(["insert", "insert"])] {
        let mut invalid_ops = overlay_manifest();
        invalid_ops["custom_operations"]["quality.create_inspection"]["registration"]["ops"] = ops;
        assert_eq!(
            validate_operation_vocabulary(&parsed_manifest(&invalid_ops))
                .expect_err("an empty or repeated registration op set was accepted")
                .kind(),
            GenerateErrorKind::InvalidOperation
        );
    }

    let mut handler_permission_error = overlay_manifest();
    handler_permission_error["custom_operations"]["quality.create_inspection"]["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!("permission_denied"));
    handler_permission_error["custom_operations"]["quality.create_inspection"]["error_details"]["permission_denied"] =
        json!({"required": ["operation"]});
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&handler_permission_error))
            .expect_err("a private handler permission error was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut unknown_kind = overlay_manifest();
    unknown_kind["custom_operations"]["quality.load_purchase_order_detail"]["kind"] =
        json!("workflow");
    assert!(
        PackageManifest::from_slice(
            &serde_json::to_vec(&unknown_kind).expect("serialize manifest")
        )
        .is_err()
    );

    let mut old_grammar = overlay_manifest();
    old_grammar["commands"] = json!({});
    assert!(
        PackageManifest::from_slice(&serde_json::to_vec(&old_grammar).expect("serialize manifest"))
            .is_err()
    );

    let mut model_collision = manifest();
    let mut operation = projection_operation();
    operation["permission"] = json!("purchase.order");
    model_collision["custom_operations"]["purchase.order"] = operation;
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&model_collision))
            .expect_err("a custom artifact collided with a model artifact")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut custom_collision = manifest();
    for operation_name in ["a_b.c", "a.b_c"] {
        let mut operation = projection_operation();
        operation["permission"] = json!(operation_name);
        custom_collision["custom_operations"][operation_name] = operation;
    }
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&custom_collision))
            .expect_err("two custom operations shared one flattened artifact identity")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn operation_error_details_are_required_closed_and_exact() {
    let mut missing_declaration = manifest();
    missing_declaration["models"]["purchase_order"]["operations"]["get"]
        .as_object_mut()
        .unwrap()
        .remove("error_details");
    assert!(
        PackageManifest::from_slice(&serde_json::to_vec(&missing_declaration).unwrap()).is_err(),
        "an operation without its error-detail declaration was accepted"
    );

    let mut unknown_code = manifest();
    unknown_code["models"]["purchase_order"]["operations"]["get"]["error_details"]["database_error"] =
        json!({});
    assert!(
        PackageManifest::from_slice(&serde_json::to_vec(&unknown_code).unwrap()).is_err(),
        "an undeclared error code was accepted"
    );

    let mut unknown_schema_key = manifest();
    unknown_schema_key["models"]["purchase_order"]["operations"]["get"]["error_details"]["invalid_input"]
        ["sqlstate"] = json!(true);
    assert!(
        PackageManifest::from_slice(&serde_json::to_vec(&unknown_schema_key).unwrap()).is_err(),
        "an open-ended detail declaration was accepted"
    );

    let mut missing_code = manifest();
    missing_code["models"]["purchase_order"]["operations"]["get"]["error_details"]
        .as_object_mut()
        .unwrap()
        .remove("not_found");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&missing_code))
            .expect_err("an incomplete error-code set was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut wrong_detail = manifest();
    wrong_detail["models"]["purchase_order"]["operations"]["update"]["error_details"]["concurrency_conflict"]
        ["required"] = json!(["expected_row_version", "id"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&wrong_detail))
            .expect_err("incorrect concurrency detail keys were accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut repeated_detail = manifest();
    repeated_detail["models"]["purchase_order"]["operations"]["query"]["error_details"]["invalid_input"]
        ["optional"] = json!(["minimum", "maximum", "observed", "observed"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&repeated_detail))
            .expect_err("a repeated detail key was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut command_detail = shipped_manifest();
    command_detail["custom_operations"]["receiving.record_receipt"]["error_details"]["purchase_order_not_open"]
        ["required"] = json!(["id"]);
    validate_operation_vocabulary(&parsed_manifest(&command_detail))
        .expect("package-local error detail keys are authored contract facts");

    command_detail["custom_operations"]["receiving.record_receipt"]["error_details"]["purchase_order_not_open"]
        ["required"] = json!(["id", "id"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&command_detail))
            .expect_err("repeated package-local detail keys were accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut constraint_detail = shipped_manifest();
    constraint_detail["custom_operations"]["receiving.record_receipt"]["error_details"]["receipt_reference_conflict"]
        ["required"] = json!(["field"]);
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&constraint_detail))
            .expect_err("a constraint error without constraint identity was accepted")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut repeated_constraint_target = shipped_manifest();
    repeated_constraint_target["custom_operations"]["receiving.record_receipt"]["constraint_errors"]
        ["receipt_idempotency_key_key"] = json!("receipt_reference_conflict");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&repeated_constraint_target))
            .expect_err("multiple constraints silently collapsed to one error case")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut reserved_constraint_target = shipped_manifest();
    reserved_constraint_target["custom_operations"]["receiving.record_receipt"]["constraint_errors"]
        ["receipt_purchase_order_id_receipt_reference_key"] = json!("retry");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&reserved_constraint_target))
            .expect_err("a constraint redefined a reserved error meaning")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut public_without_permission_refusal = shipped_manifest();
    public_without_permission_refusal["custom_operations"]["receiving.record_receipt"]["errors"]
        .as_array_mut()
        .unwrap()
        .retain(|error| error.as_str() != Some("permission_denied"));
    public_without_permission_refusal["custom_operations"]["receiving.record_receipt"]
        ["error_details"]
        .as_object_mut()
        .unwrap()
        .remove("permission_denied");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&public_without_permission_refusal))
            .expect_err("a public operation omitted its permission refusal")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mut transactionless_sql = shipped_manifest();
    let command = transactionless_sql["custom_operations"]["receiving.record_receipt"]
        .as_object_mut()
        .unwrap();
    command.remove("transaction");
    command.remove("automatic_retry");
    let error = validate_operation_vocabulary(&parsed_manifest(&transactionless_sql))
        .expect_err("a claim-bearing command without an explicit transaction was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert!(error.to_string().contains("receiving.record_receipt"));
    assert!(
        error
            .to_string()
            .contains("transaction: explicit_per_input")
    );
    assert!(error.to_string().contains("automatic_retry: false"));

    let mut missing_canonical_quantity = shipped_manifest();
    missing_canonical_quantity["custom_operations"]["receiving.record_receipt"]["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .retain(|field| field["path"].as_str() != Some("value.line[].quantity"));
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&missing_canonical_quantity))
            .expect_err("canonical line semantics omitted their positive numeric quantity")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn shipped_receiving_manifest_and_authored_corpus_generate_without_drift() {
    let ir = receiving_catalog();
    let package = generate(&GenerationInput::new(
        &ir,
        RECEIVING_MANIFEST,
        &RECEIVING_SOURCES,
        GenerationProvenance::new("wamn-schema-generator/0.1.0", "rust-1.89"),
        &StatementTransactionality::default(),
    ))
    .unwrap();

    let purchase_query = artifact_json(
        &package,
        "generated/contracts/purchase_order/query.operation.json",
    );
    assert_eq!(purchase_query["statements"].as_array().unwrap().len(), 6);
    let receipt_query = artifact_json(&package, "generated/contracts/receipt/query.operation.json");
    assert_eq!(
        receipt_query["statements"][0]["path"],
        "generated/sql/receipt/query_created_at_ascending.sql"
    );
    package
        .file("generated/sql/receipt/query_created_at_ascending.sql")
        .unwrap();
    package
        .file("generated/native-verifier/receiving_record_receipt.rs")
        .unwrap();
    package
        .file("generated/wamn/receiving_record_receipt.rs")
        .unwrap();
    let command = artifact_json(
        &package,
        "generated/contracts/receiving/record_receipt.operation.json",
    );
    assert_eq!(command["transaction"], "explicit_per_input");
    assert_eq!(command["automatic_retry"], false);
    assert_eq!(command["statements"].as_array().unwrap().len(), 9);
    let errors = artifact_json(
        &package,
        "generated/contracts/receiving/record_receipt.errors.json",
    );
    assert!(errors["cases"].as_array().unwrap().iter().any(|case| {
        case["literal"] == "receipt_reference_conflict"
            && case["constraint"] == "receipt_purchase_order_id_receipt_reference_key"
    }));
    validate_parity_json(
        package
            .file("generated/parity/receiving_record_receipt.json")
            .unwrap()
            .bytes(),
    )
    .unwrap();

    let overlay_file = package.file(DATA_ACCESS_OVERLAY_PATH).unwrap();
    DataAccessOverlay::from_slice(overlay_file.bytes())
        .expect("the generator must emit exact canonical data-access evidence");
    let mut noncanonical_overlay = overlay_file.bytes().to_vec();
    noncanonical_overlay.push(b'\n');
    assert_eq!(
        DataAccessOverlay::from_slice(&noncanonical_overlay)
            .expect_err("a second byte spelling must refuse")
            .kind(),
        GenerateErrorKind::InvalidManifest
    );
    let overlay: Value = serde_json::from_slice(overlay_file.bytes()).unwrap();
    assert_eq!(overlay["role"], "wamn_app");
    assert_eq!(overlay["contract"], "receiving_data_access");
    let location = overlay["relations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|relation| relation["table"] == "location")
        .unwrap();
    // `location.list` reads the code, so the derived ACL grants SELECT on it.
    // The invariant this pins is that location is never WRITTEN: a read
    // operation may widen the select set and must not touch the rest.
    assert_eq!(location["select_fields"], json!(["id", "location_code"]));
    assert_eq!(location["insert_fields"], json!([]));
    assert_eq!(location["update_fields"], json!([]));
    assert_eq!(location["lock"], true);
    assert_eq!(location["lock_update_field"], "id");

    let receipt_source_map = artifact_json(&package, "generated/source-map/receipt.json");
    assert_eq!(
        receipt_source_map["wamn_api"],
        json!({
            "statement_digest_visibility": "crate",
            "mutation_constraints": [],
            "operation_rows": [],
            "accessors": [
                {
                    "name": "get",
                    "visibility": "crate",
                    "operation": "get",
                    "statement_digest_constant": "GET_DIGEST",
                    "row": "ReceiptRow",
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
                },
                {
                    "name": "query_created_at_ascending",
                    "visibility": "crate",
                    "operation": "query",
                    "statement_digest_constant": "QUERY_DIGEST",
                    "row": "ReceiptRow",
                    "fetch": "all",
                    "binds": [
                        accessor_bind(
                            "cursor_key",
                            "timestamptz",
                            true,
                            "Option<chrono::DateTime<chrono::Utc>>",
                            "Option<wamn_postgres_statements::TimestampTz>"
                        ),
                        accessor_bind(
                            "cursor_id",
                            "uuid",
                            true,
                            "Option<uuid::Uuid>",
                            "Option<wamn_postgres_statements::Uuid>"
                        ),
                        accessor_bind("limit", "int8", false, "i64", "i64")
                    ]
                }
            ]
        })
    );
    assert_native_fixtures_match_parity(&package, "receipt");
}

#[test]
fn generic_custom_operation_path_preserves_shipped_receiving_bytes() {
    let package = generate(&GenerationInput::new(
        &receiving_catalog(),
        RECEIVING_MANIFEST,
        &RECEIVING_SOURCES,
        GenerationProvenance::new("wamn-schema-generator/0.1.0", "rust-1.98.0"),
        // THE SHIPPED BYTES CARRY POSTGRESQL'S VERDICTS, so the generation this
        // compares against must carry the same ones. 3c added the classification
        // and regenerated the packages but left this call unclassified, which
        // made every write statement differ and the test red on main. Frozen
        // here rather than read back out of the artifact: reading it from the
        // file under test would compare the file with itself.
        &StatementTransactionality::from_paths(BTreeMap::from([
            ("command/record_receipt/claim_command.sql".to_owned(), true),
            (
                "command/record_receipt/finalize_command.sql".to_owned(),
                true,
            ),
            ("command/record_receipt/find_replay.sql".to_owned(), false),
            (
                "command/record_receipt/finish_purchase_order.sql".to_owned(),
                true,
            ),
            ("command/record_receipt/insert_receipt.sql".to_owned(), true),
            (
                "command/record_receipt/insert_receipt_line.sql".to_owned(),
                true,
            ),
            (
                "command/record_receipt/lock_purchase_order.sql".to_owned(),
                true,
            ),
            (
                "command/record_receipt/update_purchase_order_line.sql".to_owned(),
                true,
            ),
            (
                "command/record_receipt/validate_receipt_line.sql".to_owned(),
                true,
            ),
        ])),
    ))
    .unwrap();
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let generated_root = package_root.join("generated");
    for relative in [
        "contracts/receiving/record_receipt.operation.json",
        "contracts/receiving/record_receipt.input.json",
        "contracts/receiving/record_receipt.result.json",
        "contracts/receiving/record_receipt.errors.json",
        "native-verifier/receiving_record_receipt.rs",
        "wamn/receiving_record_receipt.rs",
        "parity/receiving_record_receipt.json",
        "source-map/receiving_record_receipt.json",
    ] {
        let file = package.file(&format!("generated/{relative}")).unwrap();
        assert_eq!(
            file.bytes(),
            fs::read(generated_root.join(relative)).unwrap(),
            "generated byte drift in {relative}"
        );
    }
}

#[test]
fn fresh_only_public_operations_emit_policy_and_false_preserves_contract_bytes() {
    let baseline = generic_custom_operation_manifest();
    let paths = [
        "generated/contracts/purchase_order/get.operation.json",
        "generated/contracts/quality/load_purchase_order_detail.operation.json",
    ];
    let initial = run(
        &receiving_catalog(),
        &baseline,
        &generic_operation_sources(),
    )
    .unwrap();
    for enabled in [false, true] {
        let mut manifest = baseline.clone();
        manifest["models"]["purchase_order"]["operations"]["get"]["fresh_only"] = json!(enabled);
        manifest["custom_operations"]["quality.load_purchase_order_detail"]["fresh_only"] =
            json!(enabled);
        let generated = run(
            &receiving_catalog(),
            &manifest,
            &generic_operation_sources(),
        )
        .unwrap();
        for path in paths {
            let contract = artifact_json(&generated, path);
            if enabled {
                assert_eq!(contract["fresh_only"], true, "{path}");
            } else {
                assert!(contract.get("fresh_only").is_none(), "{path}");
                assert_eq!(
                    generated.file(path).unwrap().bytes(),
                    initial.file(path).unwrap().bytes(),
                    "{path}"
                );
            }
        }
    }
}

#[test]
fn fresh_only_refuses_private_operations_and_non_boolean_flags() {
    let mut private = generic_custom_operation_manifest();
    private["custom_operations"]["quality.create_inspection"]["fresh_only"] = json!(true);
    let error = run(&receiving_catalog(), &private, &generic_operation_sources())
        .expect_err("private operation cannot require a fresh caller");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert!(
        error
            .to_string()
            .contains("private operation quality.create_inspection")
    );

    for value in [json!(null), json!("true"), json!(1)] {
        for pointer in [
            "/models/purchase_order/operations/get",
            "/custom_operations/quality.load_purchase_order_detail",
        ] {
            let mut malformed = generic_custom_operation_manifest();
            malformed.pointer_mut(pointer).unwrap()["fresh_only"] = value.clone();
            let error = run(
                &receiving_catalog(),
                &malformed,
                &generic_operation_sources(),
            )
            .expect_err("fresh_only must be a boolean");
            assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);
        }
    }
}

#[test]
fn generic_custom_operation_kinds_emit_typed_contracts_and_sql_siblings() {
    let manifest = generic_custom_operation_manifest();
    let package = run(
        &receiving_catalog(),
        &manifest,
        &generic_operation_sources(),
    )
    .unwrap();

    for (operation, module, kind) in [
        (
            "quality/load_purchase_order_detail",
            "quality_load_purchase_order_detail",
            "projection",
        ),
        (
            "quality/create_inspection",
            "quality_create_inspection",
            "event_handler",
        ),
    ] {
        let contract = artifact_json(
            &package,
            &format!("generated/contracts/{operation}.operation.json"),
        );
        assert_eq!(contract["kind"], kind);
        package
            .file(&format!("generated/contracts/{operation}.input.json"))
            .unwrap();
        package
            .file(&format!("generated/contracts/{operation}.errors.json"))
            .unwrap();
        package
            .file(&format!("generated/native-verifier/{module}.rs"))
            .unwrap();
        package
            .file(&format!("generated/wamn/{module}.rs"))
            .unwrap();
        let parity_path = format!("generated/parity/{module}.json");
        validate_parity_json(package.file(&parity_path).unwrap().bytes()).unwrap();
        let source_map = artifact_json(&package, &format!("generated/source-map/{module}.json"));
        assert_eq!(source_map["operation"], operation.replace('/', "."));
        assert_eq!(source_map["kind"], kind);
        assert_eq!(
            source_map["statements"].as_object().unwrap().len(),
            source_map["wamn_accessors"].as_array().unwrap().len()
        );
    }

    assert!(
        package
            .file("generated/contracts/quality/create_inspection.result.json")
            .is_none()
    );
    package
        .file("generated/contracts/quality/load_purchase_order_detail.result.json")
        .unwrap();
    let handler = artifact_json(
        &package,
        "generated/contracts/quality/create_inspection.operation.json",
    );
    assert_eq!(handler["registration"]["source_package"], "wamn_receiving");
    assert_eq!(handler["registration"]["ops"], json!(["insert"]));
    let data_access = artifact_json(&package, "generated/platform-policy/data-access.json");
    let location = data_access["relations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|relation| relation["table"] == "location")
        .unwrap();
    assert_eq!(location["select_fields"], json!(["id"]));
}

#[test]
fn ownership_only_model_generates_without_fabricated_crud() {
    let manifest = shipped_manifest();
    let package = shipped_generation(&receiving_catalog(), &manifest).unwrap();
    for model in ["item", "location", "purchase_order_line", "receipt_line"] {
        package
            .file(&format!("generated/models/{model}.json"))
            .unwrap();
        for operation in ["get", "query", "create", "update", "delete"] {
            assert!(
                package
                    .file(&format!(
                        "generated/contracts/{model}/{operation}.operation.json"
                    ))
                    .is_none()
            );
        }
    }
}

#[test]
fn internal_relation_cdc_exclusion_is_closed_and_not_a_model() {
    let manifest = shipped_manifest();
    let package = shipped_generation(&receiving_catalog(), &manifest).unwrap();
    assert!(
        package
            .file("generated/models/record_receipt_command.json")
            .is_none(),
        "the command table is mechanism state, not a fabricated model"
    );

    let mut overlap = manifest.clone();
    overlap["internal_relations"]["record_receipt_command"]["table"] = json!("receipt");
    let error = shipped_generation(&receiving_catalog(), &overlap)
        .expect_err("one physical relation received two CDC classifications");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);

    let mut duplicate_model = manifest.clone();
    let receipt_model = duplicate_model["models"]["receipt"].clone();
    duplicate_model["models"]["receipt_alias"] = receipt_model;
    let error = shipped_generation(&receiving_catalog(), &duplicate_model)
        .expect_err("one physical relation received two model identities");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);

    for (class, pointer, table) in [
        ("model", "/models/item/table", "wamn_entities"),
        (
            "internal relation",
            "/internal_relations/record_receipt_command/table",
            "wamn_cdc_exclusions",
        ),
    ] {
        let mut reserved = manifest.clone();
        *reserved.pointer_mut(pointer).unwrap() = json!(table);
        let error = shipped_generation(&receiving_catalog(), &reserved)
            .expect_err("a package claimed a control-owned relation");
        assert_eq!(error.kind(), GenerateErrorKind::InvalidManifest);
        assert!(error.to_string().contains(class));
        assert!(error.to_string().contains(table));
    }

    let mut reserved_operation = manifest.clone();
    reserved_operation["custom_operations"]["receiving.record_receipt"]["relations"][0]["table"] =
        json!("wamn_cdc_exclusions");
    let error = shipped_generation(&receiving_catalog(), &reserved_operation)
        .expect_err("a package operation referenced a control-owned relation");
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert!(error.to_string().contains("wamn_cdc_exclusions"));

    let mut unknown = manifest.clone();
    unknown["internal_relations"]["record_receipt_command"]["table"] = json!("missing");
    let error = shipped_generation(&receiving_catalog(), &unknown)
        .expect_err("an exclusion for an absent relation was accepted");
    assert_eq!(error.kind(), GenerateErrorKind::UnknownRelation);

    let mut open_vocabulary = manifest;
    open_vocabulary["internal_relations"]["record_receipt_command"]["cdc"] = json!("ignored");
    assert!(
        PackageManifest::from_slice(&serde_json::to_vec(&open_vocabulary).unwrap()).is_err(),
        "the CDC disposition vocabulary became open-ended"
    );
}

#[test]
fn command_privilege_declarations_match_sql_effects_and_row_locks() {
    let catalog = receiving_catalog();
    let mut wrong_verb = shipped_manifest();
    wrong_verb["custom_operations"]["receiving.record_receipt"]["relations"][0]["select_fields"] =
        json!([]);
    wrong_verb["custom_operations"]["receiving.record_receipt"]["relations"][0]["update_fields"] =
        json!(["id"]);
    let error = shipped_generation(&catalog, &wrong_verb).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert_eq!(error.object(), Some("receiving.location"));

    let mut undeclared_lock = shipped_manifest();
    undeclared_lock["custom_operations"]["receiving.record_receipt"]["relations"][0]["lock"] =
        json!(false);
    let error = shipped_generation(&catalog, &undeclared_lock).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert_eq!(error.object(), Some("receiving.location"));

    let mut unused_lock = shipped_manifest();
    let receipt = unused_lock["custom_operations"]["receiving.record_receipt"]["relations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|relation| relation["table"] == "receipt")
        .unwrap();
    receipt["lock"] = json!(true);
    let error = shipped_generation(&catalog, &unused_lock).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert_eq!(error.object(), Some("receiving.receipt"));
}

#[test]
fn command_privilege_mismatch_reports_returning_reads_and_declared_writes() {
    let mut manifest = shipped_manifest();
    let receipt = manifest["custom_operations"]["receiving.record_receipt"]["relations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|relation| relation["table"] == "receipt")
        .unwrap();
    receipt["select_fields"] = json!([]);
    receipt["insert_fields"] = json!(["id"]);
    receipt["update_fields"] = json!(["receipt_reference", "id"]);
    let error = shipped_generation(&receiving_catalog(), &manifest).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert_eq!(error.object(), Some("receiving.receipt"));
    assert_eq!(
        error.to_string(),
        concat!(
            "InvalidOperation: receiving.record_receipt receiving.receipt privilege declaration ",
            "does not match verified SQL reads, writes, and row locks.\n",
            "Verified SQL: {\"insert_fields\":[\"id\",\"idempotency_key\",\"occurred_at\",",
            "\"purchase_order_id\",\"receipt_reference\"],\"lock\":false,",
            "\"select_fields\":[\"id\"],\"update_fields\":[]}\n",
            "Declared: {\"insert_fields\":[\"id\"],\"lock\":false,\"select_fields\":[],",
            "\"update_fields\":[\"id\",\"receipt_reference\"]}\n",
            "RETURNING columns require select_fields. ",
            "Row-lock clauses such as FOR UPDATE require lock=true.",
        )
    );
}

#[test]
fn command_privilege_mismatch_reports_for_update_lock_and_declared_value() {
    let mut manifest = shipped_manifest();
    let purchase_order = manifest["custom_operations"]["receiving.record_receipt"]["relations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|relation| relation["table"] == "purchase_order")
        .unwrap();
    purchase_order["lock"] = json!(false);
    let error = shipped_generation(&receiving_catalog(), &manifest).unwrap_err();
    assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
    assert_eq!(error.object(), Some("receiving.purchase_order"));
    assert_eq!(
        error.to_string(),
        concat!(
            "InvalidOperation: receiving.record_receipt receiving.purchase_order privilege declaration ",
            "does not match verified SQL reads, writes, and row locks.\n",
            "Verified SQL: {\"insert_fields\":[],\"lock\":true,",
            "\"select_fields\":[\"id\",\"row_version\",\"status\"],",
            "\"update_fields\":[\"row_version\",\"status\",\"updated_at\"]}\n",
            "Declared: {\"insert_fields\":[],\"lock\":false,",
            "\"select_fields\":[\"id\",\"row_version\",\"status\"],",
            "\"update_fields\":[\"row_version\",\"status\",\"updated_at\"]}\n",
            "RETURNING columns require select_fields. ",
            "Row-lock clauses such as FOR UPDATE require lock=true.",
        )
    );
}

#[test]
fn shipped_command_source_map_parity_and_bind_fixtures_align_structurally() {
    let manifest = shipped_manifest();
    let package = shipped_generation(&receiving_catalog(), &manifest).unwrap();
    let operation = artifact_json(
        &package,
        "generated/contracts/receiving/record_receipt.operation.json",
    );
    let input = artifact_json(
        &package,
        "generated/contracts/receiving/record_receipt.input.json",
    );
    let source_map = artifact_json(
        &package,
        "generated/source-map/receiving_record_receipt.json",
    );
    let parity = artifact_json(&package, "generated/parity/receiving_record_receipt.json");
    validate_parity_json(
        package
            .file("generated/parity/receiving_record_receipt.json")
            .unwrap()
            .bytes(),
    )
    .unwrap();

    let request_id = input["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["path"] == "request_id")
        .unwrap();
    assert_eq!(request_id["type"], "text");
    assert_eq!(source_map["command"], "receiving.record_receipt");
    assert_eq!(
        source_map["manifest"],
        "wamn.json#/custom_operations/receiving.record_receipt"
    );
    assert_eq!(
        source_map["relations"],
        manifest["custom_operations"]["receiving.record_receipt"]["relations"]
    );

    let statements = source_map["statements"].as_object().unwrap();
    let accessors = source_map["wamn_accessors"].as_array().unwrap();
    let native_rows = source_map["native_rows"].as_array().unwrap();
    let wamn_rows = source_map["wamn_rows"].as_array().unwrap();
    let parity_fields = parity["fields"].as_array().unwrap();
    let parity_binds = parity["accessor_binds"].as_array().unwrap();
    assert_eq!(statements.len(), 9);
    assert_eq!(accessors.len(), statements.len());
    assert_eq!(native_rows.len(), statements.len());
    assert_eq!(wamn_rows.len(), statements.len());

    let sql_files = statements
        .values()
        .map(|statement| statement["path"].clone())
        .collect::<Vec<_>>();
    let operation_statements = operation["statements"].as_array().unwrap();
    assert_eq!(operation_statements.len(), sql_files.len());
    assert_eq!(
        operation_statements
            .iter()
            .map(|statement| statement["path"].clone())
            .collect::<Vec<_>>(),
        sql_files
    );
    assert_eq!(
        sql_files
            .iter()
            .map(Value::as_str)
            .collect::<Option<BTreeSet<_>>>()
            .unwrap()
            .len(),
        statements.len()
    );

    let mut observed_fields = 0;
    let mut observed_binds = 0;
    for (index, (statement_name, statement)) in statements.iter().enumerate() {
        let accessor = &accessors[index];
        let native_row = &native_rows[index];
        let wamn_row = &wamn_rows[index];
        assert_eq!(accessor["name"], statement_name.as_str());
        assert_eq!(accessor["fetch"], statement["fetch"]);
        assert_eq!(
            accessor["statement_digest_constant"],
            format!("{}_DIGEST", statement_name.to_ascii_uppercase())
        );
        let contract = operation_statements
            .iter()
            .find(|contract| contract["name"] == statement_name.as_str())
            .unwrap();
        assert_eq!(contract["path"], statement["path"]);
        let path = statement["path"].as_str().unwrap();
        let source = RECEIVING_SOURCES
            .iter()
            .find(|source| source.path() == path)
            .unwrap();
        assert_eq!(contract["digest"], statement_digest(source.bytes()));
        assert_eq!(contract["binds"], statement["parameters"]);
        assert_eq!(contract["columns"], statement["row"]);
        assert_eq!(native_row["name"], accessor["row"]);
        assert_eq!(wamn_row["name"], accessor["row"]);
        assert_eq!(native_row["visibility"], "crate");
        assert_eq!(wamn_row["visibility"], "crate");

        let declared_fields = statement["row"].as_array().unwrap();
        let native_fields = native_row["fields"].as_array().unwrap();
        let wamn_fields = wamn_row["fields"].as_array().unwrap();
        assert_eq!(native_fields.len(), declared_fields.len());
        assert_eq!(wamn_fields.len(), declared_fields.len());
        observed_fields += declared_fields.len();
        for declared in declared_fields {
            let name = declared["name"].as_str().unwrap();
            let identity = format!("{statement_name}.{name}");
            let parity_field = object_named(parity_fields, "field", &identity);
            let native_field = object_named(native_fields, "name", name);
            let wamn_field = object_named(wamn_fields, "name", name);
            assert_eq!(parity_field["nullable"], declared["nullable"]);
            assert_eq!(parity_field["wamn_sql_value"], declared["type"]);
            assert_eq!(native_field["type"], parity_field["native_rust"]);
            assert_eq!(wamn_field["type"], parity_field["wamn_rust"]);
        }

        let declared_parameters = statement["parameters"].as_array().unwrap();
        let accessor_binds = accessor["binds"].as_array().unwrap();
        assert_eq!(accessor_binds.len(), declared_parameters.len());
        observed_binds += declared_parameters.len();
        for declared in declared_parameters {
            let name = declared["name"].as_str().unwrap();
            let accessor_bind = object_named(accessor_binds, "parameter", name);
            let parity_bind = parity_binds
                .iter()
                .find(|bind| {
                    bind["accessor"] == statement_name.as_str() && bind["parameter"] == name
                })
                .unwrap();
            assert_eq!(accessor_bind["nullable"], declared["nullable"]);
            assert_eq!(accessor_bind["postgres"], parity_bind["postgres"]);
            assert_eq!(accessor_bind["native_rust"], parity_bind["native_rust"]);
            assert_eq!(accessor_bind["wamn_rust"], parity_bind["wamn_rust"]);
        }
    }
    assert_eq!(observed_fields, parity_fields.len());
    assert_eq!(observed_binds, parity_binds.len());
    assert_native_fixtures_match_parity(&package, "receiving_record_receipt");
}

#[test]
fn custom_statement_declarations_drive_both_siblings_without_domain_tables() {
    let catalog = receiving_catalog();
    let mut declaration = shipped_manifest();
    // Not the claim's own statements. The claim binds those to the claim
    // relation, so a renamed row member there is a refusal and not a rename.
    declaration["custom_operations"]["receiving.record_receipt"]["statements"]["validate_receipt_line"]
        ["parameters"][1] = json!({
        "name": "canonical_payload",
        "type": "text",
        "nullable": true
    });
    declaration["custom_operations"]["receiving.record_receipt"]["statements"]["validate_receipt_line"]
        ["row"][0] = json!({
        "name": "claimed_receipt_id",
        "type": "text",
        "nullable": true
    });
    let package = shipped_generation(&catalog, &declaration).unwrap();
    let source_map = artifact_json(
        &package,
        "generated/source-map/receiving_record_receipt.json",
    );
    let accessor = object_named(
        source_map["wamn_accessors"].as_array().unwrap(),
        "name",
        "validate_receipt_line",
    );
    let bind = object_named(
        accessor["binds"].as_array().unwrap(),
        "parameter",
        "canonical_payload",
    );
    assert_eq!(bind["postgres"], "text");
    assert_eq!(bind["nullable"], true);
    let row = object_named(
        source_map["wamn_rows"].as_array().unwrap(),
        "name",
        "ValidateReceiptLineRow",
    );
    let field = object_named(
        row["fields"].as_array().unwrap(),
        "name",
        "claimed_receipt_id",
    );
    assert_eq!(field["type"], "Option<String>");

    let mut duplicate_path = shipped_manifest();
    duplicate_path["custom_operations"]["receiving.record_receipt"]["statements"]["claim_command"]
        ["path"] = json!("command/record_receipt/find_replay.sql");
    assert_eq!(
        shipped_generation(&catalog, &duplicate_path)
            .expect_err("two statements consumed one authored SQL path")
            .kind(),
        GenerateErrorKind::InvalidOperation
    );
}

#[test]
fn custom_operation_ir_references_require_declared_fields_and_named_constraints() {
    let manifest = shipped_manifest();
    let catalog = receiving_catalog();
    let command_table = table(&catalog, "record_receipt_command");

    let missing_field = replacing_table(
        &catalog,
        rebuilt_table(
            command_table,
            command_table
                .columns()
                .iter()
                .filter(|column| column.name() != "canonical_command")
                .cloned()
                .collect(),
            command_table.constraints().to_vec(),
        ),
    );
    assert_eq!(
        shipped_generation(&missing_field, &manifest)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::UnknownColumn
    );

    let missing_command_constraint = replacing_table(
        &catalog,
        rebuilt_table(
            command_table,
            command_table.columns().to_vec(),
            command_table
                .constraints()
                .iter()
                .filter(|constraint| {
                    constraint.name() != "record_receipt_command_idempotency_key_pkey"
                })
                .cloned()
                .collect(),
        ),
    );
    assert_eq!(
        shipped_generation(&missing_command_constraint, &manifest)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let receipt = table(&catalog, "receipt");
    let missing_constraint = replacing_table(
        &catalog,
        rebuilt_table(
            receipt,
            receipt.columns().to_vec(),
            receipt
                .constraints()
                .iter()
                .filter(|constraint| {
                    constraint.name() != "receipt_purchase_order_id_receipt_reference_key"
                })
                .cloned()
                .collect(),
        ),
    );
    assert_eq!(
        shipped_generation(&missing_constraint, &manifest)
            .unwrap_err()
            .kind(),
        GenerateErrorKind::InvalidOperation
    );

    let mapped_check_constraint = replacing_table(
        &catalog,
        rebuilt_table(
            receipt,
            receipt.columns().to_vec(),
            receipt
                .constraints()
                .iter()
                .map(|constraint| {
                    if constraint.name() == "receipt_purchase_order_id_receipt_reference_key" {
                        Constraint::check(constraint.name(), "receipt_reference <> ''").unwrap()
                    } else {
                        constraint.clone()
                    }
                })
                .collect(),
        ),
    );
    let package = shipped_generation(&mapped_check_constraint, &manifest).unwrap();
    let errors = artifact_json(
        &package,
        "generated/contracts/receiving/record_receipt.errors.json",
    );
    let mapped = errors["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["literal"] == "receipt_reference_conflict")
        .unwrap();
    assert_eq!(mapped["from"], "check_violation");
}

#[test]
fn additive_unused_column_on_consumed_relation_preserves_required_contract() {
    let manifest = shipped_manifest();
    let catalog = receiving_catalog();
    let base = shipped_generation(&catalog, &manifest).unwrap();
    let command_table = table(&catalog, "record_receipt_command");
    let mut columns = command_table.columns().to_vec();
    columns.push(Column::new(
        "unused_receiving_note",
        ColumnType::Text,
        true,
        None,
        None,
    ));
    let additive_catalog = replacing_table(
        &catalog,
        rebuilt_table(
            command_table,
            columns,
            command_table.constraints().to_vec(),
        ),
    );
    let additive = shipped_generation(&additive_catalog, &manifest).unwrap();
    let base_metadata = artifact_json(&base, "generated/package-weld.json");
    let additive_metadata = artifact_json(&additive, "generated/package-weld.json");

    assert_ne!(
        base_metadata["verified_schema_state_id"],
        additive_metadata["verified_schema_state_id"]
    );
    assert_eq!(
        base_metadata["required_schema_contract"],
        additive_metadata["required_schema_contract"]
    );
}

/// A lineless command declares canonicalization without a line profile.
///
/// `line_order` names how a LINE SET is ordered before hashing, and its one
/// spelling is `purchase_order_line_id_ascending` — another package's table.
/// Requiring it of every command made the vocabulary un-nameable by the second
/// application to arrive, which is the shape a two-package test exists to
/// find. It is optional now, and absent means the command carries no lines.
#[test]
fn a_lineless_command_canonicalizes_without_a_line_profile() {
    let mut manifest = shipped_manifest();
    let command = manifest["custom_operations"]["receiving.record_receipt"]
        .as_object_mut()
        .expect("the shipped command is an object");

    // Strip the line set and everything that describes one, leaving a command
    // shaped like WMS's `inventory.move`.
    command["input"]
        .as_object_mut()
        .expect("input")
        .remove("line");
    let fields = command["input"]["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .filter(|field| {
            !field["path"]
                .as_str()
                .is_some_and(|path| path.contains("line[]"))
        })
        .cloned()
        .collect::<Vec<Value>>();
    command["input"]["fields"] = Value::Array(fields);
    let canonicalization = command["canonicalization"]
        .as_object_mut()
        .expect("canonicalization");
    canonicalization.remove("line_order");
    canonicalization.remove("duplicate_line");

    validate_operation_vocabulary(&parsed_manifest(&manifest))
        .expect("a lineless command canonicalizes its top-level fields alone");
}

/// The INPUT decides. A line profile without a line set, or a line set without
/// one, is a manifest that cannot mean what it says — so both refuse rather
/// than one silently governing nothing.
#[test]
fn a_line_profile_and_a_line_input_must_agree() {
    let strip_line_input = |manifest: &mut Value| {
        manifest["custom_operations"]["receiving.record_receipt"]["input"]
            .as_object_mut()
            .expect("input")
            .remove("line");
    };
    let strip_line_profile = |manifest: &mut Value| {
        let canonicalization =
            manifest["custom_operations"]["receiving.record_receipt"]["canonicalization"]
                .as_object_mut()
                .expect("canonicalization");
        canonicalization.remove("line_order");
        canonicalization.remove("duplicate_line");
    };

    for (label, mutate) in [
        (
            "a profile with no line input",
            Box::new(strip_line_input) as Box<dyn Fn(&mut Value)>,
        ),
        ("a line input with no profile", Box::new(strip_line_profile)),
    ] {
        let mut manifest = shipped_manifest();
        mutate(&mut manifest);
        let refusal = validate_operation_vocabulary(&parsed_manifest(&manifest)).expect_err(label);
        assert_eq!(
            refusal.kind(),
            GenerateErrorKind::InvalidOperation,
            "{label}"
        );
    }
}

/// `line_order` and `duplicate_line` are declared together: a command that
/// said how to order lines but not what a repeat costs would leave half a rule.
#[test]
fn the_two_line_members_are_declared_together() {
    for member in ["line_order", "duplicate_line"] {
        let mut manifest = shipped_manifest();
        manifest["custom_operations"]["receiving.record_receipt"]["canonicalization"]
            .as_object_mut()
            .expect("canonicalization")
            .remove(member);
        let refusal = validate_operation_vocabulary(&parsed_manifest(&manifest))
            .expect_err("half a line profile refuses");
        assert_eq!(
            refusal.kind(),
            GenerateErrorKind::InvalidOperation,
            "{member}"
        );
    }
}

// ---------------------------------------------------------------------------
// Authored command: command-identity-from-claim.
//
// The law was ratified on an authored command, so the generator carries it for
// an authored command and not for a generated create alone (wamn-10yt.26). The
// shipped receiving package is the fixture, so these run against the same
// declaration the tree ships.
// ---------------------------------------------------------------------------

/// EXIT GATE: an authored claim-bearing command CARRIES the two claim contract
/// tests, exactly as a generated create does.
///
/// The whole artifact is frozen. An added, removed or renamed field fails here,
/// because a runner reads this file and a silent rename would make it skip a
/// case rather than refuse.
#[test]
fn an_authored_command_carries_the_two_claim_contract_tests() {
    let package = shipped_generation(&receiving_catalog(), &shipped_manifest()).unwrap();

    assert_eq!(
        artifact_json(
            &package,
            "generated/contracts/receiving/record_receipt.claim-tests.json"
        ),
        json!({
            "operation": "wamn-receiving:receiving/record-receipt@1.0.0",
            "law": "command-identity-from-claim",
            "cases": [
                {
                    "id": "replay_returns_the_immutable_original",
                    "given": "the same idempotency_key with the same canonical_command",
                    "first_call": ["claim_command", "finalize_command"],
                    "second_call": ["claim_command", "find_replay"],
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
                    "first_call": ["claim_command", "finalize_command"],
                    "second_call": ["claim_command", "find_replay"],
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

#[test]
fn claim_finalization_requires_one_returned_row() {
    for fetch in ["optional_one", "bounded_list"] {
        let mut declaration = shipped_manifest();
        declaration["custom_operations"]["receiving.record_receipt"]["statements"]["finalize_command"]
            ["fetch"] = json!(fetch);
        let error = shipped_generation(&receiving_catalog(), &declaration)
            .expect_err("a finalizer that can return no row must not permit commit");
        assert_eq!(error.kind(), GenerateErrorKind::InvalidOperation);
        assert!(error.to_string().contains("finalize_command fetch to one"));
    }
}

/// EXIT GATE: a command that declares no idempotence refuses generation, and
/// the refusal names all three remedies.
///
/// An author who writes a command that says nothing reads this text and nothing
/// else. It names every value, the relation and columns the claim value needs,
/// the guard the state value needs, and the two keys the inherited value takes.
#[test]
fn a_command_that_declares_no_idempotence_refuses_and_names_all_three_remedies() {
    let mut declaration = shipped_manifest();
    let operation = declaration["custom_operations"]["receiving.record_receipt"]
        .as_object_mut()
        .unwrap();
    operation.remove("idempotent_by");
    operation.remove("claim");

    let refusal = shipped_generation(&receiving_catalog(), &declaration)
        .expect_err("a command without declared idempotence was accepted");

    assert_eq!(refusal.kind(), GenerateErrorKind::InvalidOperation);
    let message = refusal.to_string();
    for remedy in [
        "receiving.record_receipt",
        "idempotent_by",
        "\"claim\"",
        "idempotency_key",
        "canonical_command",
        "gen_random_uuid()",
        "identities",
        "finalize",
        "\"state\"",
        "\"guards\"",
        "expected_row_version",
        "\"inherited\"",
        "\"base\"",
    ] {
        assert!(
            message.contains(remedy),
            "the refusal does not name {remedy}: {message}"
        );
    }
}

/// EXIT GATE: a state-idempotent command CARRIES its one contract test, and its
/// shape is checked rather than trusted.
///
/// The whole artifact is frozen. This command mints nothing, so the case it
/// carries is about writes and not about ids: a repeat is the unchanged
/// original or a typed conflict, and never a silent second write.
///
/// `guards` names the relation, schema qualified, beside the version field. A
/// reader of the artifact alone can say WHICH row the guard protects.
#[test]
fn a_state_idempotent_command_carries_the_relation_its_version_guards() {
    let mut overlay = overlay_manifest();
    let projection = projection_operation();
    let operation = overlay["custom_operations"]["receiving.record_receipt"]
        .as_object_mut()
        .unwrap();
    operation.insert("connection".to_owned(), json!("postgres"));
    operation.insert("transaction".to_owned(), json!("explicit_per_input"));
    operation.insert("automatic_retry".to_owned(), json!(false));
    operation.insert("relations".to_owned(), projection["relations"].clone());
    operation.insert("statements".to_owned(), projection["statements"].clone());
    operation.insert(
        "idempotent_by".to_owned(),
        json!({"state": {"guards": {"purchase_order": "value.expected_row_version"}}}),
    );
    operation["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "path": "value.expected_row_version",
            "type": "int64",
            "nullable": false
        }));

    let package = run(&receiving_catalog(), &overlay, &generic_operation_sources()).unwrap();

    assert!(
        package
            .file("generated/contracts/receiving/record_receipt.claim-tests.json")
            .is_none()
    );
    assert_eq!(
        artifact_json(
            &package,
            "generated/contracts/receiving/record_receipt.state-tests.json"
        ),
        json!({
            "operation": "client-acme-receiving:receiving/record-receipt@3.0.0",
            "law": "command-idempotence-from-state",
            "guards": [
                {
                    "relation": "receiving.purchase_order",
                    "expected_version": "value.expected_row_version",
                },
            ],
            "cases": [
                {
                    "id": "a_repeat_is_a_no_op_or_a_typed_conflict",
                    "given": "the same request sent again after the first call succeeded",
                    "expect": {
                        "writes": "none",
                        "outcome": "unchanged_original_or_refusal",
                        "refusal": "concurrency_conflict",
                        "second_write": "never",
                        "identity_minted": "none",
                    },
                },
            ],
        })
    );
}

/// EXIT GATE: a command riding a base claim CARRIES its one contract test, and
/// names the base it rides.
///
/// The whole artifact is frozen. The composed command holds no claim of its
/// own, and the case asserts its replay hands back the BASE's original result.
#[test]
fn a_command_riding_a_base_claim_carries_its_one_contract_test() {
    let overlay = overlay_manifest();

    let package = run(&receiving_catalog(), &overlay, &generic_operation_sources()).unwrap();

    assert_eq!(
        artifact_json(
            &package,
            "generated/contracts/receiving/record_receipt.inherited-tests.json"
        ),
        json!({
            "operation": "client-acme-receiving:receiving/record-receipt@3.0.0",
            "law": "command-identity-from-claim",
            "inherits": {
                "alias": "base_receiving",
                "package": "wamn_receiving",
                "version": "1.0.0",
                "digest": format!("sha256:{}", "a".repeat(64)),
                "operation": "receiving.record_receipt",
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
}

/// EXIT GATE: an inherited command that writes its own row is refused, and the
/// refusal names the shape that owns a write.
///
/// The rule is not narrowed to an id column. A command riding a base claim
/// decorates a base result, so the moment it writes it writes under an identity
/// it does not own. That command is a command with a claim that also composes,
/// and the refusal says exactly that so the author does not narrow the rule.
#[test]
fn an_inherited_command_that_writes_its_own_row_is_told_to_declare_a_claim() {
    let mut overlay = overlay_manifest();
    let projection = projection_operation();
    let operation = overlay["custom_operations"]["receiving.record_receipt"]
        .as_object_mut()
        .unwrap();
    operation.insert("connection".to_owned(), json!("postgres"));
    operation.insert("transaction".to_owned(), json!("explicit_per_input"));
    operation.insert("automatic_retry".to_owned(), json!(false));
    operation.insert("relations".to_owned(), projection["relations"].clone());
    operation.insert("statements".to_owned(), projection["statements"].clone());
    operation["relations"][0]["insert_fields"] = json!(["id"]);

    let refusal = run(&receiving_catalog(), &overlay, &generic_operation_sources())
        .expect_err("an inherited command that writes its own row was accepted");

    assert_eq!(refusal.kind(), GenerateErrorKind::InvalidOperation);
    let message = refusal.to_string();
    for remedy in [
        "inherited",
        "receiving.purchase_order",
        "idempotent_by claim",
        "claim relation",
    ] {
        assert!(
            message.contains(remedy),
            "the refusal does not name {remedy}: {message}"
        );
    }
}

/// Turn the shipped command into a state-idempotent one with the given guards.
fn state_command(declaration: &mut Value, guards: Value) {
    let operation = declaration["custom_operations"]["receiving.record_receipt"]
        .as_object_mut()
        .unwrap();
    operation.insert(
        "idempotent_by".to_owned(),
        json!({"state": {"guards": guards}}),
    );
    operation.remove("claim");
    for relation in operation["relations"].as_array_mut().unwrap() {
        relation["insert_fields"] = json!([]);
    }
}

/// EXIT GATE: each declared value admits only its own shape, so no command
/// borrows another value's guarantee.
///
/// A claim outside `claim` is a second identity source. A minted row under
/// `state` or `inherited` is an identity with no claim behind it. A `state`
/// command must name the relation each version guards, and that relation and
/// field must be ones it already declares. An `inherited` command must name a
/// base operation this package actually depends on.
#[test]
fn each_declared_idempotence_value_admits_only_its_own_shape() {
    let catalog = receiving_catalog();
    let cases: [(&str, fn(&mut Value)); 7] = [
        (
            "idempotent_by claim with no claim declared",
            |declaration: &mut Value| {
                declaration["custom_operations"]["receiving.record_receipt"]
                    .as_object_mut()
                    .unwrap()
                    .remove("claim");
            },
        ),
        (
            "idempotent_by state that still declares a claim",
            |declaration: &mut Value| {
                declaration["custom_operations"]["receiving.record_receipt"]["idempotent_by"] =
                    json!({"state": {"guards": {"purchase_order": "value.purchase_order_id"}}});
            },
        ),
        (
            "idempotent_by state that mints a row of its own",
            |declaration: &mut Value| {
                let operation = declaration["custom_operations"]["receiving.record_receipt"]
                    .as_object_mut()
                    .unwrap();
                operation.insert(
                    "idempotent_by".to_owned(),
                    json!({"state": {"guards": {"purchase_order": "value.purchase_order_id"}}}),
                );
                operation.remove("claim");
            },
        ),
        (
            "idempotent_by state that guards no relation at all",
            |declaration: &mut Value| {
                state_command(declaration, json!({}));
            },
        ),
        (
            "idempotent_by state guarding a relation it never declared",
            |declaration: &mut Value| {
                state_command(
                    declaration,
                    json!({"absent_relation": "value.purchase_order_id"}),
                );
            },
        ),
        (
            "idempotent_by state guarding with a field it never takes",
            |declaration: &mut Value| {
                state_command(declaration, json!({"purchase_order": "absent_field"}));
            },
        ),
        (
            "idempotent_by inherited naming an undeclared base",
            |declaration: &mut Value| {
                let operation = declaration["custom_operations"]["receiving.record_receipt"]
                    .as_object_mut()
                    .unwrap();
                operation.insert(
                    "idempotent_by".to_owned(),
                    json!({"inherited": {"base": "absent_base", "operation": "receiving.record_receipt"}}),
                );
                operation.remove("claim");
                for relation in operation["relations"].as_array_mut().unwrap() {
                    relation["insert_fields"] = json!([]);
                }
            },
        ),
    ];
    for (label, mutate) in cases {
        let mut declaration = shipped_manifest();
        mutate(&mut declaration);
        let refusal = shipped_generation(&catalog, &declaration).expect_err(label);
        assert_eq!(
            refusal.kind(),
            GenerateErrorKind::InvalidOperation,
            "{label}"
        );
    }
}

/// EXIT GATE: an authored claim is checked structurally, so a declaration that
/// only looks like the law is refused.
///
/// Each mutant breaks one link in the chain that makes a replay return the same
/// value BY CONSTRUCTION. The claim relation must be CDC-excluded, its identity
/// columns must be minted once, the claim statement must hand back exactly
/// those columns, and the replay statement must read them all back.
#[test]
fn an_authored_claim_is_refused_unless_every_identity_comes_from_it() {
    let catalog = receiving_catalog();
    let cases: [(&str, fn(&mut Value)); 5] = [
        (
            "a claim relation the operation never declared",
            |declaration: &mut Value| {
                declaration["custom_operations"]["receiving.record_receipt"]["claim"]["table"] =
                    json!("absent_command");
            },
        ),
        (
            "an identity that is not a claim column",
            |declaration: &mut Value| {
                declaration["custom_operations"]["receiving.record_receipt"]["claim"]["identities"] =
                    json!({"receipt_id": "purchase_order_id"});
            },
        ),
        (
            "an identity the command never returns",
            |declaration: &mut Value| {
                declaration["custom_operations"]["receiving.record_receipt"]["claim"]["identities"] =
                    json!({"claimed_id": "receipt_id"});
            },
        ),
        (
            "a claim statement that hands back something other than the identity",
            |declaration: &mut Value| {
                declaration["custom_operations"]["receiving.record_receipt"]["statements"]["claim_command"]
                    ["row"][0] =
                    json!({"name": "purchase_order_id", "type": "uuid", "nullable": false});
            },
        ),
        (
            "a replay statement that drops the canonical command",
            |declaration: &mut Value| {
                declaration["custom_operations"]["receiving.record_receipt"]["statements"]["find_replay"]
                    ["row"][0] =
                    json!({"name": "claimed_command", "type": "bytes", "nullable": false});
            },
        ),
    ];
    for (label, mutate) in cases {
        let mut declaration = shipped_manifest();
        mutate(&mut declaration);
        let refusal = shipped_generation(&catalog, &declaration).expect_err(label);
        assert_eq!(
            refusal.kind(),
            GenerateErrorKind::InvalidOperation,
            "{label}"
        );
    }
}
