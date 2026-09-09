use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use wamn_schema_generator::GeneratedFile;
use wamn_schema_generator::client_ir::{ClientContractIr, ResponseIr, RouteIr};
use wamn_schema_generator::client_rust::emit_rust_client;
use wamn_schema_generator::client_tui::{ClientTuiErrorKind, emit_tui};

fn release(package: &str) -> ClientContractIr {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    ClientContractIr::from_release(
        package,
        &root.join(format!("packages/{package}/generated/contracts")),
        &root.join(format!("packages/{package}/publication/attachments.json")),
    )
    .unwrap_or_else(|error| panic!("{package} projects: {error}"))
}

fn source<'a>(files: &'a [GeneratedFile], path: &str) -> &'a str {
    std::str::from_utf8(
        files
            .iter()
            .find(|file| file.path() == path)
            .unwrap_or_else(|| panic!("emitted {path}"))
            .bytes(),
    )
    .unwrap()
}

fn declared_identifier<'a>(
    source: &'a str,
    prefix: &str,
    name: &str,
    terminator: char,
) -> Option<&'a str> {
    source.lines().find_map(|line| {
        let (identifier, _) = line.strip_prefix(prefix)?.split_once(terminator)?;
        (identifier.strip_prefix("r#").unwrap_or(identifier) == name).then_some(identifier)
    })
}

fn spec<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source
        .find(&format!("pub static {}_SPEC:", name.to_uppercase()))
        .unwrap();
    source[start..].split("\n#[must_use]").next().unwrap()
}

#[test]
fn shipped_operator_crates_are_deterministic_and_cover_each_callable_operation() {
    for package in ["receiving", "client_acme_receiving", "wms"] {
        let ir = release(package);
        let first = emit_tui(&ir, package).unwrap();
        let second = emit_tui(&ir, package).unwrap();
        assert_eq!(first, second, "{package}");
        assert_eq!(first.len(), 4 + ir.models.len());
        let library = source(&first, &format!("generated/{package}-tui/src/lib.rs"));
        let calls = library
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("screens::"))
            .collect::<Vec<_>>();
        let callable_count = ir
            .models
            .iter()
            .flat_map(|model| &model.operations)
            .filter(|operation| operation.kind != "event_handler")
            .count();
        assert_eq!(calls.len(), callable_count);
        if let Some((last, earlier)) = calls.split_last() {
            assert!(last.ends_with("(binding),"));
            assert!(
                earlier
                    .iter()
                    .all(|call| call.ends_with("(binding.clone()),"))
            );
        }
        for model in &ir.models {
            assert!(library.contains(&format!("#[path = \"../../client/{}.rs\"]", model.name)));
            let screens = source(
                &first,
                &format!("generated/{package}-tui/src/screens/{}.rs", model.name),
            );
            let module = declared_identifier(library, "pub mod ", &model.name, ';')
                .expect("the library declares its client module");
            for operation in &model.operations {
                let function = declared_identifier(screens, "pub fn ", &operation.name, '(');
                if operation.kind == "event_handler" {
                    assert!(function.is_none());
                } else {
                    let function = function.expect("a callable operation has a screen constructor");
                    assert!(
                        spec(screens, &operation.name)
                            .contains(&format!("operation: {:?}", operation.operation))
                    );
                    assert!(
                        library
                            .contains(&format!("screens::{module}::{function}(binding.clone())"))
                            || library.contains(&format!("screens::{module}::{function}(binding)"))
                    );
                }
            }
        }
    }
}

#[test]
fn workspace_package_and_binary_names_keep_the_reference_crate_distinct() {
    let ir = release("client_acme_receiving");
    let files = emit_tui(&ir, "client_acme_receiving").unwrap();
    let cargo = source(&files, "generated/client_acme_receiving-tui/Cargo.toml");
    assert!(cargo.contains("name = \"wamn-generated-client-acme-receiving-tui\""));
    assert!(cargo.contains("name = \"wamn-client-acme-receiving-tui\""));
    for inherited in ["version", "edition", "license"] {
        assert!(cargo.contains(&format!("{inherited}.workspace = true")));
    }
    assert!(cargo.contains("[lints]\nworkspace = true"));
    assert!(!cargo.contains("[workspace]"));
    for dependency in [
        "wamn-client",
        "wamn-client-tui",
        "wamn-client-terminal",
        "serde_json",
        "chrono",
        "rust_decimal",
        "uuid",
        "tokio",
    ] {
        assert!(cargo.contains(&format!("{dependency} = {{ workspace = true")));
    }
    let main = source(&files, "generated/client_acme_receiving-tui/src/main.rs");
    assert!(main.contains(
        "async fn main() -> Result<wamn_client_terminal::operator::ExitReason, Box<dyn std::error::Error>>"
    ));
    assert!(main.contains("#[tokio::main]"));
    assert!(main.contains("wamn_client_terminal::operator::run(\"client_acme_receiving\", wamn_generated_client_acme_receiving_tui::screens).await"));
}

#[test]
fn receiving_replay_and_wms_unknown_completion_use_the_served_contract() {
    let receiving = emit_tui(&release("receiving"), "receiving").unwrap();
    let command = spec(
        source(
            &receiving,
            "generated/receiving-tui/src/screens/receiving.rs",
        ),
        "record_receipt",
    );
    assert!(command.contains("replay: submission::Replay::Claim"));
    assert!(command.contains("transaction: Some(\"explicit_per_input\")"));
    assert!(command.contains("connection_unavailable"));
    assert!(command.contains("direct: true"));

    let ir = release("wms");
    let files = emit_tui(&ir, "wms").unwrap();
    let command = spec(
        source(&files, "generated/wms-tui/src/screens/inventory.rs"),
        "move",
    );
    assert!(command.contains("direct: false"));
    assert!(command.contains("replay: submission::Replay::Unknown"));
    assert!(command.contains("result_class: None"));
    assert!(command.contains("errors: &[\n        ]"));
    assert!(!command.contains("movement_id"));
    let bindings = emit_rust_client(&ir).unwrap();
    let module = source(&bindings, "generated/client/inventory.rs");
    assert!(module.contains(
        "pub const INVENTORY_MOVE_RESULT_SCHEMA: &[wamn_client::descriptor::FieldSchema] = &[\n];"
    ));
}

fn fixtures() -> ClientContractIr {
    let mut contracts = BTreeMap::new();
    let mut routes = BTreeMap::new();
    for (name, kind, result, exposed) in [
        ("query_alpha", "query", "code", true),
        ("query_beta", "query", "title", true),
        ("hidden", "update", "hidden_value", false),
        ("composed", "command", "inner_movement_id", true),
        ("unknown", "command", "opaque_value", true),
    ] {
        let identity = format!("example:entry/{name}@1.0.0");
        for (suffix, value) in [
            (
                "operation",
                json!({
                    "operation": identity, "kind": kind, "grant": identity,
                    "permission_token": format!("entry.{name}"), "result": "one"
                }),
            ),
            (
                "input",
                json!({"fields": [
                    {"path": "request_id", "type": "string", "nullable": false},
                    {"path": "value", "type": if name == "unknown" {"opaque"} else {"text"}, "nullable": false}
                ]}),
            ),
            (
                "result",
                json!({"class": "one", "fields": [
                    {"path": result, "type": "text", "nullable": false}
                ]}),
            ),
            (
                "errors",
                json!({"cases": [
                    {"literal": format!("{name}_refusal"), "from": "transaction_invariant", "detail": {"required": ["field"]}}
                ]}),
            ),
        ] {
            contracts.insert(
                format!("entry/{name}.{suffix}.json"),
                serde_json::to_vec(&value).unwrap(),
            );
        }
        if exposed {
            let direct = name != "composed";
            routes.insert(
                identity.clone(),
                RouteIr {
                    method: "POST".into(),
                    template: format!("/entry/{name}"),
                    input_schema: None,
                    terminal_operation: Some(if direct {
                        identity
                    } else {
                        "external:handler/respond@1.0.0".into()
                    }),
                    direct,
                    response: ResponseIr::default(),
                    replay: None,
                },
            );
        }
    }
    ClientContractIr::from_release_contracts("example", &contracts, &routes).unwrap()
}

#[test]
fn queries_reference_their_own_result_schemas_and_preserve_distinct_error_cases() {
    let ir = fixtures();
    let files = emit_tui(&ir, "example").unwrap();
    let screens = source(&files, "generated/example-tui/src/screens/entry.rs");
    for (name, column) in [("query_alpha", "code"), ("query_beta", "title")] {
        let screen = spec(screens, name);
        assert!(screen.contains(&format!(
            "fields: crate::entry::ENTRY_{}_RESULT_SCHEMA",
            name.to_uppercase()
        )));
        assert!(screen.contains(&format!("literal: \"{name}_refusal\"")));
        assert!(!screen.contains("ENTRY_FIELDS"));
        let bindings = emit_rust_client(&ir).unwrap();
        let module = source(&bindings, "generated/client/entry.rs");
        let marker = format!("pub const ENTRY_{}_RESULT_SCHEMA:", name.to_uppercase());
        let schema = module
            .split(&marker)
            .nth(1)
            .unwrap()
            .split("\n];")
            .next()
            .unwrap();
        assert!(schema.contains(&format!("path: \"{column}\"")));
        assert!(!schema.contains(if column == "code" {
            "path: \"title\""
        } else {
            "path: \"code\""
        }));
    }
}

#[test]
fn unexposed_unsupported_and_composed_operations_keep_distinct_screens() {
    let ir = fixtures();
    let files = emit_tui(&ir, "example").unwrap();
    let screens = source(&files, "generated/example-tui/src/screens/entry.rs");
    let hidden = spec(screens, "hidden");
    assert!(hidden.contains("route: None"));
    assert!(hidden.contains("hidden_refusal"));
    assert!(hidden.contains("result_class: Some(\"one\")"));
    assert!(hidden.contains("requires_composition: true"));
    let composed = spec(screens, "composed");
    assert!(composed.contains("route: Some(crate::entry::composed_route)"));
    assert!(!composed.contains("composed_refusal"));
    assert!(composed.contains("result_class: None"));
    let unknown = spec(screens, "unknown");
    assert!(unknown.contains("input: crate::entry::ENTRY_UNKNOWN_INPUT_SCHEMA"));
    assert!(unknown.contains("unknown_refusal"));
    assert!(screens.contains("pub fn unknown("));
}

#[test]
fn platform_and_revision_inputs_come_only_from_exact_declared_paths() {
    let mut ir = fixtures();
    let operation = &mut ir.models[0].operations[0];
    operation.name = "reserved".into();
    operation.route = None;
    operation.record = Some(wamn_schema_generator::client_ir::RecordIr {
        relation: "inventory.stock".into(),
        key_field: "id".into(),
        key_input: None,
        revision_field: Some("version".into()),
        revision_input: Some("record_revision".into()),
    });
    operation.idempotent_by = Some(json!({"state": {"guards": {"stock": "value.version_seen"}}}));
    let template = operation.input_fields[0].clone();
    operation.input_fields = [
        "request_id",
        "idempotency_key",
        "value.idempotency_key",
        "occurred_at",
        "value.occurred_at",
        "expected_row_version",
        "value.expected_row_version",
        "record_revision",
        "value.version_seen",
        "supplier_id",
        "value.domain_occurred_at",
        "value.transfer_id",
    ]
    .into_iter()
    .map(|path| wamn_schema_generator::client_ir::FieldIr {
        path: path.into(),
        children: Vec::new(),
        ..template.clone()
    })
    .collect();
    let files = emit_tui(&ir, "example").unwrap();
    let reserved = spec(
        source(&files, "generated/example-tui/src/screens/entry.rs"),
        "reserved",
    );
    assert!(reserved.contains("revision_inputs: &[\"expected_row_version\", \"record_revision\", \"value.expected_row_version\", \"value.version_seen\"]"));
    for path in [
        "request_id",
        "idempotency_key",
        "value.idempotency_key",
        "occurred_at",
        "value.occurred_at",
    ] {
        assert!(reserved.contains(&format!("SuppliedField {{ path: {path:?},")));
    }
    assert_eq!(reserved.matches("screen::SuppliedField").count(), 5);
    for path in [
        "supplier_id",
        "value.domain_occurred_at",
        "value.transfer_id",
    ] {
        assert!(!reserved.contains(&format!("SuppliedField {{ path: {path:?},")));
    }
}

#[test]
fn event_handlers_are_excluded_and_deployment_values_are_supplied_at_launch() {
    let mut ir = fixtures();
    let mut handler = ir.models[0].operations[0].clone();
    handler.name = "private_handler".into();
    handler.kind = "event_handler".into();
    handler.operation = "example:entry/private-handler@1.0.0".into();
    ir.models[0].operations.push(handler);
    let files = emit_tui(&ir, "example").unwrap();
    let combined: String = files
        .iter()
        .map(|file| std::str::from_utf8(file.bytes()).unwrap())
        .collect();
    assert!(!combined.contains("private_handler"));
    assert!(!combined.contains("private-handler"));
    assert!(combined.contains("SessionBinding"));
    assert!(combined.contains("route: Some(crate::entry::query_alpha_route)"));
    for value in [
        "localhost",
        "http://",
        "https://",
        "Bearer ",
        "Authorization",
        "target_instance:",
        "base_url:",
    ] {
        assert!(!combined.contains(value), "deployment fact {value}");
    }
}

#[test]
fn raw_schema_literals_preserve_quotes_without_becoming_rust_source() {
    let mut ir = fixtures();
    let operation = ir.models[0]
        .operations
        .iter_mut()
        .find(|operation| operation.name == "query_alpha")
        .unwrap();
    let input = json!({"type": "array", "description": "A quoted \"label\""});
    let response = json!({"type": "array", "maxItems": 1});
    let route = operation.route.as_mut().unwrap();
    route.input_schema = Some(input.clone());
    route.response.schema = Some(response.clone());
    let files = emit_tui(&ir, "example").unwrap();
    let screen = spec(
        source(&files, "generated/example-tui/src/screens/entry.rs"),
        "query_alpha",
    );
    assert!(screen.contains(&format!("input_schema: Some({:?})", input.to_string())));
    assert!(screen.contains(&format!("schema: Some({:?})", response.to_string())));
}

#[test]
fn invalid_paths_and_generated_module_collisions_are_refused() {
    let ir = fixtures();
    for directory in ["", "..", "../outside", "example/nested", "example\\nested"] {
        assert_eq!(
            emit_tui(&ir, directory).unwrap_err().kind(),
            ClientTuiErrorKind::InvalidName
        );
    }
    let mut collision = ir.clone();
    collision.models[0].name = "screens".into();
    assert_eq!(
        emit_tui(&collision, "example").unwrap_err().kind(),
        ClientTuiErrorKind::NameCollision
    );
    let mut invalid = ir;
    invalid.models[0].operations[0].name = "self".into();
    assert_eq!(
        emit_tui(&invalid, "example").unwrap_err().kind(),
        ClientTuiErrorKind::InvalidName
    );
}

#[test]
fn screens_reference_the_same_collision_free_helpers_as_the_client() {
    let mut ir = fixtures();
    let template = ir.models[0]
        .operations
        .iter()
        .find(|operation| operation.name == "query_alpha")
        .unwrap()
        .clone();
    ir.models[0].operations = [
        "get",
        "get_route",
        "get_route_route",
        "__wamn_route_get",
        "__wamn_route_get_1",
    ]
    .into_iter()
    .map(|name| {
        let mut operation = template.clone();
        operation.name = name.into();
        operation.operation = format!("example:entry/{name}@1.0.0");
        operation.route.as_mut().unwrap().template = format!("/entry/{name}");
        operation
    })
    .collect();
    let client = emit_rust_client(&ir).unwrap();
    let bindings = source(&client, "generated/client/entry.rs");
    let first = emit_tui(&ir, "example").unwrap();
    ir.models[0].operations.reverse();
    assert_eq!(first, emit_tui(&ir, "example").unwrap());
    let screens = source(&first, "generated/example-tui/src/screens/entry.rs");
    for (name, helper) in [
        ("get", "__wamn_route_get_2"),
        ("get_route", "__wamn_route_get_route_1"),
        ("get_route_route", "get_route_route_route"),
        ("__wamn_route_get", "__wamn_route_get_route"),
        ("__wamn_route_get_1", "__wamn_route_get_1_route"),
    ] {
        assert!(spec(screens, name).contains(&format!("route: Some(crate::entry::{helper})")));
        assert!(bindings.contains(&format!("pub fn {helper}() -> RouteMetadata")));
        assert!(bindings.contains(&format!(".invoke(&{helper}(),")));
    }
}
