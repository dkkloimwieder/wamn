use wamn_schema_generator::GeneratedFile;
use wamn_schema_generator::client_ir::ClientContractIr;
use wamn_schema_generator::client_ts::{
    ClientTsErrorKind, emit_ts_client, to_camel, to_snake, ts_type,
};

#[path = "support/platform_fixture.rs"]
mod fixture;

fn release() -> ClientContractIr {
    fixture::client_release()
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

fn widget(files: &[GeneratedFile]) -> &str {
    source(files, "generated/client-ts/widget.ts")
}

fn combined(files: &[GeneratedFile]) -> String {
    files
        .iter()
        .map(|file| std::str::from_utf8(file.bytes()).unwrap())
        .collect()
}

#[test]
fn every_public_operation_gets_two_interfaces_and_one_function() {
    let ir = release();
    let files = emit_ts_client(&ir).unwrap();
    assert_eq!(
        files.iter().map(GeneratedFile::path).collect::<Vec<_>>(),
        [
            "generated/client-ts/index.ts",
            "generated/client-ts/widget.ts",
            "generated/client-ts/wire.ts",
        ]
    );
    let widget = widget(&files);
    for (operation, function) in [
        ("archive", "archive"),
        ("create", "create"),
        // `delete` is a reserved word, so the name keeps its spelling and
        // gains one underscore.
        ("delete", "delete_"),
        ("get", "get"),
        ("query", "query"),
        ("update", "update"),
    ] {
        let stem = format!("Widget{}", {
            let mut name = operation.to_owned();
            name[..1].make_ascii_uppercase();
            name
        });
        assert!(
            widget.contains(&format!("export interface {stem}Request {{")),
            "{operation}"
        );
        assert!(
            widget.contains(&format!("export interface {stem}Result {{")),
            "{operation}"
        );
        assert!(
            widget.contains(&format!("export async function {function}(")),
            "{operation}"
        );
        assert!(
            widget.contains(&format!(
                "operation: \"platform-fixture:widget/{operation}@1.0.0\","
            )),
            "{operation} carries its exact canonical identity"
        );
    }
    assert_eq!(widget.matches("export async function ").count(), 6);
    assert_eq!(
        source(&files, "generated/client-ts/index.ts"),
        concat!(
            "// @generated from the client-contract IR; do not edit.\n\n",
            "export * from \"./wire.js\";\n",
            "export * as widget from \"./widget.js\";\n",
        )
    );
}

#[test]
fn the_release_route_supplies_the_method_and_the_template() {
    let ir = release();
    let files = emit_ts_client(&ir).unwrap();
    let widget = widget(&files);
    assert!(widget.contains("export const WIDGET_QUERY_ROUTE: OperationRoute = {"));
    assert!(widget.contains("  method: \"POST\","));
    assert!(widget.contains("  template: \"/widget/query\","));
    assert!(
        widget.contains("    replay: \"state\","),
        "archive replays state"
    );
    assert!(
        widget.contains("    resultClass: \"page\","),
        "the query pages"
    );

    let combined = combined(&files);
    for absent in [
        "localhost",
        "http",
        "Bearer",
        "Authorization",
        "baseUrl",
        "targetInstance",
        "fetch(",
    ] {
        assert!(!combined.contains(absent), "deployment fact {absent}");
    }
    for template in ["/widget/archive", "/widget/create", "/widget/query"] {
        assert_eq!(
            combined.matches(template).count(),
            1,
            "{template} appears once, from the route"
        );
    }
}

#[test]
fn an_event_handler_emits_nothing_and_an_unexposed_operation_gets_no_function() {
    let mut ir = release();
    let mut handler = ir.models[0].operations[0].clone();
    handler.name = "widget_archived".to_owned();
    handler.kind = "event_handler".to_owned();
    handler.operation = "platform-fixture:widget/widget-archived@1.0.0".to_owned();
    ir.models[0].operations.push(handler);
    ir.models[0]
        .operations
        .iter_mut()
        .find(|operation| operation.name == "get")
        .unwrap()
        .route = None;

    let files = emit_ts_client(&ir).unwrap();
    let combined = combined(&files);
    assert!(!combined.contains("widget_archived"));
    assert!(!combined.contains("widgetArchived"));
    assert!(!combined.contains("widget-archived"));

    let widget = widget(&files);
    assert!(widget.contains("export interface WidgetGetRequest {"));
    assert!(widget.contains("export interface WidgetGetResult {"));
    assert!(!widget.contains("export async function get("));
    assert!(!widget.contains("WIDGET_GET_ROUTE"));
    assert!(widget.contains(
        "// `platform-fixture:widget/get@1.0.0` is not published over HTTP by this release"
    ));
}

#[test]
fn two_names_that_take_one_typescript_name_refuse_by_name() {
    let mut ir = release();
    let template = ir.models[0].operations[0].clone();
    for (index, name) in ["a_b", "aB"].into_iter().enumerate() {
        let mut operation = template.clone();
        operation.name = name.to_owned();
        operation.operation = format!("platform-fixture:widget/collide-{index}@1.0.0");
        ir.models[0].operations.push(operation);
    }
    let refusal = emit_ts_client(&ir).expect_err("a duplicate TypeScript name refuses");
    assert_eq!(refusal.kind(), ClientTsErrorKind::NameCollision);
    assert!(refusal.to_string().contains("\"aB\""), "{refusal}");

    let mut unnameable = release();
    unnameable.models[0].operations[0].name = "9lives".to_owned();
    let refusal = emit_ts_client(&unnameable).expect_err("a name with no spelling refuses");
    assert_eq!(refusal.kind(), ClientTsErrorKind::UnnameableIdentifier);
    assert!(refusal.to_string().contains("9lives"), "{refusal}");
}

#[test]
fn the_emitted_bindings_are_byte_stable() {
    let mut ir = release();
    let first = emit_ts_client(&ir).unwrap();
    assert_eq!(first, emit_ts_client(&ir).unwrap());
    ir.models[0].operations.reverse();
    assert_eq!(
        first,
        emit_ts_client(&ir).unwrap(),
        "the contract's incidental order does not reach the bytes"
    );
}

/// Every name the platform fixture declares must survive the mapping, because
/// the emitted `fromWire` reverses it at run time.
#[test]
fn every_contract_name_in_the_fixture_reverses_through_the_member_rule() {
    let ir = release();
    let mut checked = 0;
    for model in &ir.models {
        assert_eq!(to_snake(&to_camel(&model.name)), model.name);
        for operation in &model.operations {
            assert_eq!(to_snake(&to_camel(&operation.name)), operation.name);
            let fields = operation
                .input_fields
                .iter()
                .chain(&operation.result_fields)
                .chain(
                    operation
                        .route
                        .iter()
                        .flat_map(|route| &route.response.fields),
                );
            for field in
                wamn_schema_generator::client_ir::leaf_fields(&fields.cloned().collect::<Vec<_>>())
            {
                for part in field.path.split('.') {
                    let name = part.trim_end_matches("[]");
                    assert_eq!(to_snake(&to_camel(name)), name, "{}", field.path);
                    assert!(
                        ts_type(&field.type_name).is_ok() || field.type_name == "object",
                        "{} has type {}",
                        field.path,
                        field.type_name
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 20, "the fixture declares enough names: {checked}");
}
