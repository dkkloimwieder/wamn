use serde_json::{Value, json};
use wamn_schema_generator::{
    AuthoredSql, GeneratedPackage, GenerationInput, GenerationProvenance,
    StatementTransactionality, generate,
};
use wamn_schema_introspection::ir::{CatalogIr, Column, ColumnType, Constraint, Table};

fn catalog() -> CatalogIr {
    CatalogIr::new(vec![Table::new(
        "inventory",
        "stock",
        vec![
            Column::new("id", ColumnType::Uuid, false, None, None),
            Column::new("edit_version", ColumnType::Int64, false, None, None),
            Column::new("title", ColumnType::Text, false, None, None),
        ],
        vec![Constraint::primary_key("stock_pkey", ["id"]).unwrap()],
        Vec::new(),
    )])
}

fn manifest() -> Value {
    let mut operations = serde_json::Map::new();
    for action in ["get", "update"] {
        let mut operation = json!({
            "permission": format!("entry.{action}"),
            "error_details": {
                "invalid_input": {"required": ["field"]},
                "not_found": {"required": ["field", "id"]},
                "retry": {},
                "timeout": {},
                "permission_denied": {"required": ["operation"]},
                "internal_error": {}
            },
            "result": "one"
        });
        if action != "get" {
            operation["revision_field"] = json!("edit_version");
            operation["error_details"]["concurrency_conflict"] = json!({
                "required": ["expected_row_version", "observed_row_version"]
            });
        }
        if action == "update" {
            operation["writable_fields"] = json!(["title"]);
        }
        operations.insert(action.to_owned(), operation);
    }
    json!({
        "package": {"id": "example", "version": "1.0.0"},
        "required_platform_policy_contract": {"id": "example_data", "state": "unsatisfied"},
        "models": {
            "entry": {
                "schema": "inventory",
                "table": "stock",
                "owner": "example",
                "server_owned_fields": ["id", "edit_version"],
                "operations": operations
            }
        },
        "connections": {"postgres": {"interface": "wamn:postgres@0.1.0"}},
        "components": {"example": {"connections": ["postgres"]}}
    })
}

fn emit(manifest: &Value, sources: &[AuthoredSql<'_>]) -> GeneratedPackage {
    generate(&GenerationInput::new(
        &catalog(),
        &serde_json::to_vec(manifest).unwrap(),
        sources,
        GenerationProvenance::new("wamn-schema-generator/0.1.0", "test"),
        &StatementTransactionality::default(),
    ))
    .expect("declared client contract fixture generates")
}

fn artifact(package: &GeneratedPackage, path: &str) -> Value {
    serde_json::from_slice(package.file(path).expect("contract is emitted").bytes()).unwrap()
}

#[test]
fn crud_record_links_use_the_declared_relation_and_revision() {
    let package = emit(&manifest(), &[]);
    for action in ["get", "update"] {
        let contract = artifact(
            &package,
            &format!("generated/contracts/entry/{action}.operation.json"),
        );
        assert_eq!(contract["kind"], action);
        let mut record = json!({
            "relation": "inventory.stock",
            "key_field": "id",
            "key_input": "id"
        });
        if action != "get" {
            record["revision_field"] = json!("edit_version");
            record["revision_input"] = json!("expected_edit_version");
            let input = artifact(
                &package,
                &format!("generated/contracts/entry/{action}.input.json"),
            );
            assert_eq!(input["expected_edit_version"]["field"], "edit_version");
        }
        assert_eq!(contract["record"], record);
        assert!(contract.get("idempotent_by").is_none());
    }
}

#[test]
fn custom_state_metadata_preserves_the_guard_without_inventing_a_record_link() {
    let mut manifest = manifest();
    manifest["custom_operations"] = json!({
        "entry.inspect": {
            "kind": "command",
            "visibility": "public",
            "permission": "entry.inspect",
            "connection": "postgres",
            "transaction": "explicit_per_input",
            "automatic_retry": false,
            "idempotent_by": {"state": {"guards": {"stock": "version_seen"}}},
            "input": {"fields": [
                {"path": "target_id", "type": "uuid", "nullable": false},
                {"path": "version_seen", "type": "int64", "nullable": false}
            ]},
            "result": {"class": "one", "fields": [
                {"path": "id", "type": "uuid", "nullable": false},
                {"path": "edit_version", "type": "int64", "nullable": false}
            ]},
            "errors": ["permission_denied"],
            "error_details": {"permission_denied": {"required": ["operation"]}},
            "relations": [{
                "schema": "inventory", "table": "stock",
                "select_fields": ["id", "edit_version"],
                "insert_fields": [], "update_fields": [], "lock": false
            }],
            "statements": {"read": {
                "path": "command/inspect.sql", "fetch": "one",
                "parameters": [{"name": "target_id", "type": "uuid", "nullable": false}],
                "row": [
                    {"name": "id", "type": "uuid", "nullable": false},
                    {"name": "edit_version", "type": "int64", "nullable": false}
                ]
            }}
        }
    });
    let package = emit(
        &manifest,
        &[AuthoredSql::new(
            "command/inspect.sql",
            b"SELECT id, edit_version FROM stock WHERE id = $1;\n",
        )],
    );
    let contract = artifact(&package, "generated/contracts/entry/inspect.operation.json");
    assert_eq!(contract["kind"], "command");
    assert_eq!(
        contract["idempotent_by"],
        json!({"state": {"guards": {"stock": "version_seen"}}})
    );
    assert_eq!(
        contract["relations"],
        json!([{
            "schema": "inventory", "table": "stock",
            "select_fields": ["id", "edit_version"],
            "insert_fields": [], "update_fields": [], "lock": false,
            "constraints": []
        }])
    );
    assert!(contract.get("record").is_none());
    assert!(contract.get("claim").is_none());
}
