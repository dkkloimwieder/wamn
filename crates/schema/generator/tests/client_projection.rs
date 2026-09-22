use std::collections::BTreeMap;

use serde_json::{Value, json};
use wamn_schema_generator::client_ir::{
    ClientContractIr, OperationIr, ReplayIr, ResponseIr, RevisionBindingIr, RouteIr, leaf_fields,
};

#[path = "support/platform_fixture.rs"]
mod fixture;
#[path = "support/platform_claim.rs"]
mod platform_claim;
#[path = "support/platform_claim_release.rs"]
mod platform_claim_release;

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
            {"path": "version_seen", "type": "int64", "nullable": false, "revision": true, "json": "string"},
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
                {"path": "edit_version", "type": "int64", "nullable": false, "revision": true, "json": "string"}
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
                {"path": "value.expected_row_version", "type": "int64", "nullable": false, "revision": true, "json": "string"}
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

fn release() -> ClientContractIr {
    fixture::client_release()
}

#[test]
fn platform_update_preserves_writable_type_presence_and_nullability() {
    let platform = release();
    let update = operation(&platform, "update");
    let schema = update
        .route
        .as_ref()
        .unwrap()
        .input_schema
        .as_ref()
        .unwrap();
    let transport = schema
        .pointer("/items/properties/change/properties/note")
        .unwrap();
    assert_eq!(transport["type"], json!(["string", "null"]));
    assert_eq!(transport["x-wamn-explicit-null"], "accepted");

    let fields = leaf_fields(&update.input_fields);
    let note = fields
        .iter()
        .find(|field| field.path == "change.note")
        .expect("declared writable note");
    assert_eq!(note.type_name, "text");
    assert!(!note.required);
    assert!(note.nullable);
    let code_transport = schema
        .pointer("/items/properties/change/properties/code")
        .unwrap();
    assert_eq!(code_transport["type"], json!(["string", "null"]));
    assert_eq!(code_transport["x-wamn-explicit-null"], "invalid_input");
    let code = fields
        .iter()
        .find(|field| field.path == "change.code")
        .expect("declared writable code");
    assert!(!code.required);
    assert!(!code.nullable);
}

#[test]
fn platform_query_closed_sort_domains_remain_typed() {
    let mut contracts = BTreeMap::new();
    insert_operation(
        &mut contracts,
        "closed_query",
        json!({
            "operation": "example:entry/closed-query@1.0.0", "kind": "query",
            "grant": "example:entry/closed-query@1.0.0",
            "permission_token": "entry.closed_query", "result": "page"
        }),
        json!({"fields": [{"path": "filter.code[]", "type": "text", "nullable": false,
            "values": ["priority", "standard"]}]}),
        json!({"class": "page", "fields": []}),
    );
    let platform = project(&contracts, &BTreeMap::new());
    let query = operation(&platform, "closed_query");
    let fields = leaf_fields(&query.input_fields);
    let code = fields
        .iter()
        .find(|field| field.path == "filter.code[]")
        .expect("closed code filter descriptor");
    assert_eq!(code.type_name, "text");
    assert_eq!(code.values, ["priority", "standard"]);
}

#[test]
fn direct_state_replay_and_composed_routes_remain_distinct() {
    let platform = release();
    let command = operation(&platform, "archive");
    let route = command.route.as_ref().expect("platform route");
    assert!(route.direct);
    assert_eq!(
        route.terminal_operation.as_deref(),
        Some(command.operation.as_str())
    );
    assert_eq!(route.replay, Some(ReplayIr::State));
    assert_eq!(command.transaction.as_deref(), Some("explicit_per_input"));
    for errors in [&command.errors, &route.response.errors] {
        let retry = errors
            .iter()
            .find(|error| error.literal == "retry")
            .expect("retry outcome");
        assert_eq!(
            retry.sources,
            ["connection_unavailable", "serialization_failure"]
        );
    }

    let package = fixture::generate_fixture();
    let contracts = fixture::contracts(&package);
    let identity = command.operation.clone();
    let routes = BTreeMap::from([(
        identity.clone(),
        RouteIr {
            method: "POST".into(),
            template: "/widget/archive".into(),
            input_schema: None,
            terminal_operation: Some("wamn:node/async-handler@0.1.0".into()),
            direct: false,
            response: ResponseIr::default(),
            replay: None,
        },
    )]);
    let projected =
        ClientContractIr::from_release_contracts("platform_fixture", &contracts, &routes)
            .expect("composed route projects");
    let route = operation(&projected, "archive").route.as_ref().unwrap();
    assert!(!route.direct);
    assert_eq!(
        route.terminal_operation.as_deref(),
        Some("wamn:node/async-handler@0.1.0")
    );
    assert_eq!(route.replay, None);
}

#[test]
fn direct_claim_and_composed_completion_project_from_route_evidence() {
    let direct = platform_claim_release::release(false);
    let command = operation(&direct, "archive");
    let route = command.route.as_ref().unwrap();
    assert!(route.direct);
    assert_eq!(route.replay, Some(ReplayIr::Claim));
    assert_eq!(
        route.terminal_operation.as_deref(),
        Some(command.operation.as_str())
    );

    let composed = platform_claim_release::release(true);
    let command = operation(&composed, "archive");
    let route = command.route.as_ref().unwrap();
    assert!(!route.direct);
    assert_eq!(route.replay, None);
    assert_eq!(
        route.terminal_operation.as_deref(),
        Some("wamn:node/async-handler@0.1.0")
    );
    assert!(route.response.schema.is_some());
    let committed = &route.response.partial_schema.as_ref().unwrap()["properties"]["committed_result"]
        ["items"]["properties"]["value"];
    let result = leaf_fields(&command.result_fields);
    assert_eq!(
        committed["properties"].as_object().unwrap().len(),
        result.len()
    );
    for field in result {
        assert!(
            committed["required"]
                .as_array()
                .unwrap()
                .contains(&json!(field.path))
        );
        assert_eq!(committed["properties"][&field.path]["type"], "string");
    }
    let served = leaf_fields(&route.response.fields);
    assert!(served.iter().any(|field| field.path == "stored.key"));
    assert!(served.iter().any(|field| field.path == "stored.container"));
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
    assert_eq!(
        wamn_schema_generator::client_ir::revision_inputs(&command),
        vec!["version_seen"]
    );
}

/// One leaf of `fields` by its exact path.
fn leaf<'a>(
    fields: &'a [wamn_schema_generator::client_ir::FieldIr],
    path: &str,
) -> &'a wamn_schema_generator::client_ir::FieldIr {
    leaf_fields(fields)
        .into_iter()
        .find(|field| field.path == path)
        .unwrap_or_else(|| panic!("the contract declares {path}"))
}

/// EXIT GATE for `wamn-c2y5.3`: the authored text reaches every field a client
/// reads, on each of the three paths a field can travel.
///
/// The three differ, and a label that arrives on one path alone is a screen
/// that reads a database in the other two.
#[test]
fn authored_text_reaches_every_field_the_client_reads() {
    let ir = fixture::client_release();
    let operation = |name: &str| {
        ir.models[0]
            .operations
            .iter()
            .find(|operation| operation.name == name)
            .unwrap_or_else(|| panic!("the fixture declares {name}"))
    };

    // A served input: the published schema states the shape, and the declared
    // contract carries the text through the hint.
    let update = operation("update");
    assert!(
        update
            .route
            .as_ref()
            .is_some_and(|route| route.input_schema.is_some())
    );
    assert_eq!(
        leaf(&update.input_fields, "change.code").label.as_deref(),
        Some("Widget code")
    );
    assert_eq!(
        leaf(&update.input_fields, "change.note").label.as_deref(),
        Some("Operator note")
    );
    assert_eq!(
        leaf(&update.input_fields, "id").label,
        None,
        "a column with no authored text carries none"
    );

    // A declared input with no published schema, including a leaf inside a
    // repeated group. The group itself keeps the derived text: `wamn-j3yr`.
    let batch = operation("record_batch");
    assert_eq!(
        leaf(&batch.input_fields, "value.line[].quantity")
            .label
            .as_deref(),
        Some("Quantity received")
    );
    assert_eq!(
        leaf(&batch.input_fields, "value.note")
            .description
            .as_deref(),
        Some("What the operator recorded about this batch.")
    );

    // A declared result of an authored operation, carried whole because the
    // published response states no class.
    let list = operation("list");
    assert_eq!(
        leaf(&list.result_fields, "attributes").label.as_deref(),
        Some("Attributes")
    );
    assert_eq!(
        leaf(
            &list.route.as_ref().expect("the list route").response.fields,
            "attributes"
        )
        .label
        .as_deref(),
        Some("Attributes"),
        "the served response is the one a table reads"
    );

    // The operation itself, at both carriers.
    assert_eq!(operation("query").label.as_deref(), Some("Find widgets"));
    assert_eq!(batch.label.as_deref(), Some("Record a batch"));
    assert!(batch.description.is_some());
    assert_eq!(operation("get").label, None);
}

/// EXIT GATE for `wamn-c2y5.3`: a response whose published schema states its
/// own class still carries the text of the terminal that answers it.
///
/// This is the detail screen's path. A published schema carries no authored
/// text by decision, so without the join a detail reads field paths while the
/// table beside it reads labels.
#[test]
fn a_published_response_takes_its_text_from_the_terminal() {
    let package = fixture::generate_fixture();
    let contracts = fixture::contracts(&package);
    let identity = "platform-fixture:widget/get@1.0.0";
    let served = |path: &str| wamn_schema_generator::client_ir::FieldIr {
        path: path.to_owned(),
        type_name: "text".to_owned(),
        nullable: false,
        required: true,
        revision: false,
        children: Vec::new(),
        minimum: None,
        maximum: None,
        values: Vec::new(),
        label: None,
        description: None,
    };
    let routes = BTreeMap::from([(
        identity.to_owned(),
        RouteIr {
            method: "POST".to_owned(),
            template: "/widget/get".to_owned(),
            input_schema: None,
            terminal_operation: Some(identity.to_owned()),
            direct: true,
            response: ResponseIr {
                result_class: Some("one".to_owned()),
                fields: vec![served("id"), served("code")],
                ..ResponseIr::default()
            },
            replay: None,
        },
    )]);
    let ir = ClientContractIr::from_release_contracts("platform_fixture", &contracts, &routes)
        .expect("the fixture projects with a published response");
    let get = ir.models[0]
        .operations
        .iter()
        .find(|operation| operation.name == "get")
        .expect("the fixture declares get");
    let response = &get.route.as_ref().expect("the get route").response;
    assert_eq!(
        leaf(&response.fields, "code").label.as_deref(),
        Some("Widget code"),
        "the terminal's declared text joins the published shape"
    );
    assert_eq!(leaf(&response.fields, "id").label, None);
}
