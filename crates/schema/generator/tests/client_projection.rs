use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Value, json};
use wamn_schema_generator::client_ir::{
    ClientContractIr, OperationIr, ReplayIr, ResponseIr, RevisionBindingIr, RouteIr, leaf_fields,
};

const READ: &str = "example:entry/read-projection@1.0.0";
const UPDATE: &str = "example:entry/apply-title@1.0.0";

type Contracts = BTreeMap<String, Vec<u8>>;

fn insert_operation(
    contracts: &mut Contracts,
    name: &str,
    operation: Value,
    input: Value,
    result: Value,
) {
    for (part, value) in [
        ("operation", operation),
        ("input", input),
        ("result", result),
    ] {
        contracts.insert(
            format!("entry/{name}.{part}.json"),
            serde_json::to_vec(&value).unwrap(),
        );
    }
}

fn direct_route(operation: &str) -> RouteIr {
    RouteIr {
        method: "POST".into(),
        template: "/entry/action".into(),
        input_schema: None,
        terminal_operation: Some(operation.into()),
        direct: true,
        response: ResponseIr::default(),
        replay: None,
    }
}

fn records() -> (Contracts, BTreeMap<String, RouteIr>) {
    let mut contracts = BTreeMap::new();
    insert_operation(
        &mut contracts,
        "read_projection",
        json!({
            "operation": READ, "kind": "get", "grant": READ,
            "permission_token": "entry.read_projection", "result": "one",
            "record": {
                "relation": "inventory.stock", "key_field": "stock_id",
                "key_input": "lookup_key"
            }
        }),
        json!({"fields": [{"path": "lookup_key", "type": "uuid", "nullable": false}]}),
        json!({"class": "one", "fields": [
            {"path": "stock_id", "type": "uuid", "nullable": false},
            {"path": "edit_version", "type": "int64", "nullable": false}
        ]}),
    );
    insert_operation(
        &mut contracts,
        "apply_title",
        json!({
            "operation": UPDATE, "kind": "update", "grant": UPDATE,
            "permission_token": "entry.apply_title", "result": "one",
            "record": {
                "relation": "inventory.stock", "key_field": "stock_id",
                "key_input": "record_key", "revision_field": "edit_version",
                "revision_input": "version_seen"
            }
        }),
        json!({"fields": [
            {"path": "record_key", "type": "uuid", "nullable": false},
            {"path": "version_seen", "type": "int64", "nullable": false},
            {"path": "title", "type": "text", "nullable": false}
        ]}),
        json!({"class": "one", "fields": [
            {"path": "title", "type": "text", "nullable": false}
        ]}),
    );
    let routes = BTreeMap::from([
        (READ.into(), direct_route(READ)),
        (UPDATE.into(), direct_route(UPDATE)),
    ]);
    (contracts, routes)
}

fn project(contracts: &Contracts, routes: &BTreeMap<String, RouteIr>) -> ClientContractIr {
    ClientContractIr::from_release_contracts("example", contracts, routes)
        .expect("declared contract projection")
}

fn operation<'a>(ir: &'a ClientContractIr, name: &str) -> &'a OperationIr {
    ir.models
        .iter()
        .flat_map(|model| &model.operations)
        .find(|operation| operation.name == name)
        .unwrap_or_else(|| panic!("operation {name} is present"))
}

fn change(contracts: &mut Contracts, path: &str, pointer: &str, value: Value) {
    let mut document: Value = serde_json::from_slice(&contracts[path]).unwrap();
    *document.pointer_mut(pointer).expect("fixture member") = value;
    contracts.insert(path.into(), serde_json::to_vec(&document).unwrap());
}

#[test]
fn declared_kind_and_record_mappings_bind_operations_with_unrelated_names() {
    let (contracts, routes) = records();
    let ir = project(&contracts, &routes);
    assert_eq!(operation(&ir, "read_projection").kind, "get");
    let update = operation(&ir, "apply_title");
    assert_eq!(update.kind, "update");
    assert!(!update.requires_composition);
    assert_eq!(
        update.revision_binding,
        Some(RevisionBindingIr {
            read_operation: READ.into(),
            read_key_input: "lookup_key".into(),
            key_field: "stock_id".into(),
            revision_field: "edit_version".into(),
            command_key_input: "record_key".into(),
            command_revision_input: "version_seen".into(),
        })
    );
}

#[test]
fn declared_delete_needs_a_compatible_exposed_read_for_its_revision() {
    for exposed in [false, true] {
        let (mut contracts, mut routes) = records();
        change(
            &mut contracts,
            "entry/apply_title.operation.json",
            "/kind",
            json!("delete"),
        );
        if !exposed {
            routes.remove(READ);
        }
        let ir = project(&contracts, &routes);
        let delete = operation(&ir, "apply_title");
        assert_eq!(delete.kind, "delete");
        assert_eq!(delete.requires_composition, !exposed);
        if exposed {
            let binding = delete.revision_binding.as_ref().unwrap();
            assert_eq!(binding.read_operation, READ);
            assert_eq!(binding.revision_field, "edit_version");
            assert_eq!(binding.command_revision_input, "version_seen");
        } else {
            assert!(delete.revision_binding.is_none());
        }
    }
}

#[test]
fn missing_or_unexposed_record_read_requires_composition() {
    for remove_contract in [false, true] {
        let (mut contracts, mut routes) = records();
        routes.remove(READ);
        if remove_contract {
            contracts.retain(|path, _| !path.starts_with("entry/read_projection."));
        }
        let ir = project(&contracts, &routes);
        let update = operation(&ir, "apply_title");
        assert!(update.requires_composition);
        assert!(update.revision_binding.is_none());
    }
}

#[test]
fn composed_record_read_uses_the_served_terminal_response_for_revision_binding() {
    const TERMINAL: &str = "example:entry/terminal@1.0.0";
    for terminal in [None, Some(TERMINAL)] {
        let (mut contracts, mut routes) = records();
        insert_operation(
            &mut contracts,
            "terminal",
            json!({
                "operation": TERMINAL, "kind": "query", "grant": TERMINAL,
                "permission_token": "entry.terminal", "result": "one"
            }),
            json!({"fields": []}),
            json!({"class": "one", "fields": [
                {"path": "label", "type": "text", "nullable": false}
            ]}),
        );
        let route = routes.get_mut(READ).unwrap();
        route.direct = false;
        route.terminal_operation = terminal.map(str::to_owned);

        let ir = project(&contracts, &routes);
        let read = operation(&ir, "read_projection");
        assert!(
            leaf_fields(&read.result_fields)
                .iter()
                .any(|field| field.path == "edit_version")
        );
        let response = &read.route.as_ref().unwrap().response;
        assert!(
            leaf_fields(&response.fields)
                .iter()
                .all(|field| field.path != "edit_version")
        );
        if terminal.is_some() {
            assert!(
                leaf_fields(&response.fields)
                    .iter()
                    .any(|field| field.path == "label")
            );
        } else {
            assert!(response.fields.is_empty());
        }
        let update = operation(&ir, "apply_title");
        assert!(update.requires_composition, "terminal {terminal:?}");
        assert!(update.revision_binding.is_none(), "terminal {terminal:?}");
    }
}

#[test]
fn revision_binding_requires_one_served_record_even_when_terminal_fields_match() {
    const TERMINAL: &str = "example:entry/terminal@1.0.0";
    for class in ["one", "bounded_list", "page", "none"] {
        let (mut contracts, mut routes) = records();
        insert_operation(
            &mut contracts,
            "terminal",
            json!({
                "operation": TERMINAL, "kind": "projection", "grant": TERMINAL,
                "permission_token": "entry.terminal", "result": class
            }),
            json!({"fields": []}),
            json!({"class": class, "fields": [
                {"path": "stock_id", "type": "uuid", "nullable": false},
                {"path": "edit_version", "type": "int64", "nullable": false}
            ]}),
        );
        let route = routes.get_mut(READ).unwrap();
        route.direct = false;
        route.terminal_operation = Some(TERMINAL.into());

        let ir = project(&contracts, &routes);
        let read = operation(&ir, "read_projection");
        assert_eq!(read.result_class, "one");
        assert_eq!(
            read.route
                .as_ref()
                .unwrap()
                .response
                .result_class
                .as_deref(),
            Some(class)
        );
        let update = operation(&ir, "apply_title");
        assert_eq!(update.requires_composition, class != "one", "{class}");
        assert_eq!(update.revision_binding.is_some(), class == "one", "{class}");
    }
}

#[test]
fn incompatible_record_relation_key_or_revision_requires_composition() {
    for (path, pointer, value) in [
        (
            "operation",
            "/record/relation",
            json!("inventory.other_stock"),
        ),
        ("input", "/fields/0/type", json!("text")),
        ("result", "/fields/0/type", json!("text")),
        ("result", "/fields/1/type", json!("text")),
        ("result", "/fields/1/nullable", json!(true)),
    ] {
        let (mut contracts, routes) = records();
        change(
            &mut contracts,
            &format!("entry/read_projection.{path}.json"),
            pointer,
            value,
        );
        let ir = project(&contracts, &routes);
        let update = operation(&ir, "apply_title");
        assert!(update.requires_composition, "{path}{pointer}");
        assert!(update.revision_binding.is_none(), "{path}{pointer}");
    }
}

#[test]
fn a_record_read_keeps_its_other_required_inputs() {
    let (contracts, mut routes) = records();
    routes.get_mut(READ).unwrap().input_schema = Some(json!({
        "type": "array", "items": {
            "type": "object", "required": ["lookup_key", "warehouse"],
            "properties": {
                "lookup_key": {"type": "string", "format": "uuid"},
                "warehouse": {"type": "string"}
            }
        }
    }));
    let ir = project(&contracts, &routes);
    let read = operation(&ir, "read_projection");
    let fields = leaf_fields(&read.input_fields);
    let warehouse = fields
        .iter()
        .find(|field| field.path == "warehouse")
        .unwrap();
    assert!(warehouse.required);
    assert!(!warehouse.nullable);
    assert!(fields.iter().any(|field| field.path == "lookup_key"));
}

#[test]
fn custom_state_and_claim_declarations_do_not_invent_input_key_links() {
    let mut contracts = BTreeMap::new();
    for (name, idempotent_by) in [
        (
            "change_state",
            json!({"state": {"guards": {"stock": "value.expected_row_version"}}}),
        ),
        ("claim_stock", json!("claim")),
    ] {
        let identity = format!("example:entry/{name}@1.0.0");
        let mut declaration = json!({
            "operation": identity, "kind": "command", "grant": identity,
            "permission_token": format!("entry.{name}"), "result": "one",
            "idempotent_by": idempotent_by,
            "relations": [{"schema": "inventory", "table": "stock"}]
        });
        if name == "claim_stock" {
            declaration["claim"] = json!({
                "table": "stock_command", "identities": {"stock_id": "stock_id"},
                "claim": "claim", "replay": "read_claim", "finalize": "finish_claim"
            });
        }
        insert_operation(
            &mut contracts,
            name,
            declaration,
            json!({"fields": [
                {"path": "value.stock_id", "type": "uuid", "nullable": false},
                {"path": "value.expected_row_version", "type": "int64", "nullable": false}
            ]}),
            json!({"class": "one", "fields": []}),
        );
    }
    let ir = project(&contracts, &BTreeMap::new());
    for name in ["change_state", "claim_stock"] {
        let command = operation(&ir, name);
        assert!(command.record.is_none());
        assert!(command.revision_binding.is_none());
        assert!(command.requires_composition);
    }
}

fn release(package: &str) -> ClientContractIr {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    ClientContractIr::from_release(
        package,
        &root.join(format!("apps/{package}/generated/contracts")),
        &root.join(format!("apps/{package}/publication/attachments.json")),
    )
    .unwrap_or_else(|error| panic!("{package} projects: {error}"))
}

#[test]
fn receiving_update_preserves_writable_type_presence_and_null_refusal() {
    let receiving = release("wamn_receiving");
    let update = operation(&receiving, "update");
    let schema = update
        .route
        .as_ref()
        .unwrap()
        .input_schema
        .as_ref()
        .unwrap();
    let transport = schema
        .pointer("/items/properties/change/properties/supplier_id")
        .unwrap();
    assert_eq!(transport["type"], json!(["string", "null"]));
    assert_eq!(transport["x-wamn-explicit-null"], "invalid_input");

    let fields = leaf_fields(&update.input_fields);
    let supplier = fields
        .iter()
        .find(|field| field.path == "change.supplier_id")
        .expect("declared writable supplier");
    assert_eq!(supplier.type_name, "uuid");
    assert!(!supplier.required);
    assert!(!supplier.nullable);
}

#[test]
fn receiving_query_closed_string_domains_remain_typed_without_explicit_schema_types() {
    let receiving = release("wamn_receiving");
    let query = receiving
        .models
        .iter()
        .find(|model| model.name == "purchase_order")
        .unwrap()
        .operations
        .iter()
        .find(|operation| operation.name == "query")
        .unwrap();
    let schema = query.route.as_ref().unwrap().input_schema.as_ref().unwrap();
    let fields = leaf_fields(&query.input_fields);
    for (path, pointer, values) in [
        (
            "sort.field",
            "/items/properties/sort/properties/field",
            vec!["created_at", "purchase_order_number", "status"],
        ),
        (
            "sort.direction",
            "/items/properties/sort/properties/direction",
            vec!["ascending", "descending"],
        ),
        (
            "filter.status[]",
            "/items/properties/filter/properties/status/items",
            vec!["cancelled", "complete", "open"],
        ),
    ] {
        assert!(schema.pointer(pointer).unwrap().get("type").is_none());
        let field = fields.iter().find(|field| field.path == path).unwrap();
        assert_eq!(field.type_name, "text", "{path}");
        assert_eq!(field.values, values, "{path}");
    }
}

#[test]
fn receiving_direct_claim_grants_replay_but_wms_composition_does_not() {
    let receiving = release("wamn_receiving");
    let command = operation(&receiving, "record_receipt");
    let route = command.route.as_ref().expect("Receiving route");
    assert!(route.direct);
    assert_eq!(
        route.terminal_operation.as_deref(),
        Some(command.operation.as_str())
    );
    assert_eq!(route.replay, Some(ReplayIr::Claim));
    assert_eq!(command.transaction.as_deref(), Some("explicit_per_input"));
    for errors in [&command.errors, &route.response.errors] {
        let retry = errors
            .iter()
            .find(|error| error.literal == "retry")
            .expect("Receiving retry outcome");
        assert_eq!(
            retry.sources,
            ["connection_unavailable", "serialization_failure"]
        );
    }

    let wms = release("wamn_wms");
    let command = operation(&wms, "move");
    assert_eq!(command.idempotent_by, Some(json!("claim")));
    assert!(
        leaf_fields(&command.result_fields)
            .iter()
            .any(|field| field.path == "movement_id")
    );
    let route = command.route.as_ref().expect("WMS composed route");
    assert!(!route.direct);
    assert_eq!(
        route.terminal_operation.as_deref(),
        Some("wamn:node/async-handler@0.1.0")
    );
    assert_eq!(route.replay, None);
    assert!(route.response.schema.is_some());
    let partial = route
        .response
        .partial_schema
        .as_ref()
        .expect("declared partial schema");
    let committed = &partial["properties"]["committed_result"]["items"]["properties"]["value"];
    let command_fields = leaf_fields(&command.result_fields);
    assert_eq!(
        committed["properties"].as_object().unwrap().len(),
        command_fields.len()
    );
    for field in command_fields {
        assert!(
            committed["required"]
                .as_array()
                .unwrap()
                .contains(&json!(field.path))
        );
        let property = &committed["properties"][&field.path];
        let expected_type = match field.type_name.as_str() {
            "uuid" | "text" => "string",
            "int64" => "integer",
            other => panic!("the committed declaration needs the generated {other} type"),
        };
        assert_eq!(property["type"], expected_type);
        if !field.values.is_empty() {
            let actual = property["enum"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<BTreeSet<_>>();
            let expected = field.values.iter().map(String::as_str).collect();
            assert_eq!(actual, expected);
        }
    }
    assert_eq!(route.response.result_class.as_deref(), Some("one"));
    let fields = leaf_fields(&route.response.fields);
    for path in ["movement_id", "zpl", "stored.container", "stored.key"] {
        assert!(fields.iter().any(|field| field.path == path), "{path}");
    }
}

#[test]
fn query_columns_remain_specific_to_each_result_contract() {
    let mut contracts = BTreeMap::new();
    for (name, column) in [("by_code", "code"), ("by_title", "title")] {
        let identity = format!("example:entry/{name}@1.0.0");
        insert_operation(
            &mut contracts,
            name,
            json!({
                "operation": identity, "kind": "query", "grant": identity,
                "permission_token": format!("entry.{name}"), "result": "page"
            }),
            json!({"fields": []}),
            json!({"class": "page", "fields": [
                {"path": column, "type": "text", "nullable": false}
            ]}),
        );
    }
    let ir = project(&contracts, &BTreeMap::new());
    assert_eq!(ir.models[0].fields.len(), 2);
    for (name, column) in [("by_code", "code"), ("by_title", "title")] {
        let fields = leaf_fields(&operation(&ir, name).result_fields);
        assert_eq!(
            fields
                .iter()
                .map(|field| field.path.as_str())
                .collect::<Vec<_>>(),
            [column]
        );
    }
}

#[test]
fn revision_roles_use_declared_paths_and_platform_fields_only() {
    let (contracts, routes) = records();
    let ir = project(&contracts, &routes);
    let mut command = operation(&ir, "apply_title").clone();
    command.idempotent_by = Some(json!({"state": {"guards": {"stock": "another_revision"}}}));
    assert_eq!(
        wamn_schema_generator::client_ir::revision_inputs(&command),
        vec!["another_revision", "version_seen"]
    );
    command.record = None;
    command.idempotent_by = None;
    assert!(wamn_schema_generator::client_ir::revision_inputs(&command).is_empty());
}
