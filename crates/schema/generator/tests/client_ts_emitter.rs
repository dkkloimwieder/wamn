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
fn every_public_operation_gets_its_interfaces_and_one_function() {
    let ir = release();
    let files = emit_ts_client(&ir).unwrap();
    assert_eq!(
        files.iter().map(GeneratedFile::path).collect::<Vec<_>>(),
        [
            "generated/client-ts/index.ts",
            "generated/client-ts/widget.ts",
            "generated/client-ts/widget_maker.ts",
            "generated/client-ts/widget_tag.ts",
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
        ("list", "list"),
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
    assert_eq!(widget.matches("export async function ").count(), 8);
    assert_eq!(
        source(&files, "generated/client-ts/index.ts"),
        concat!(
            "// @generated from the client-contract IR; do not edit.\n//\n",
            "// The wire contract lives in `@wamn/web-runtime`, which this package",
            " depends on.\n\n",
            "export * as widget from \"./widget.js\";\n",
            "export * as widgetMaker from \"./widget_maker.js\";\n",
            "export * as widgetTag from \"./widget_tag.js\";\n",
        )
    );
}

/// Defect 1 of the Epic 2 review. A bounded list and a page return an
/// envelope, and the row sits inside it. `validate_value` in
/// `crates/client/tui/src/submission.rs` owns that rule.
#[test]
fn a_collection_result_is_typed_as_the_envelope_the_release_serves() {
    let ir = release();
    let files = emit_ts_client(&ir).unwrap();
    let widget = widget(&files);
    assert!(
        widget.contains(concat!(
            "/** Result of `platform-fixture:widget/list@1.0.0`. */\n",
            "export interface WidgetListResult {\n",
            "  /** Every row the release served. */\n",
            "  readonly rows: readonly WidgetListRow[];\n",
            "}\n",
        )),
        "a bounded list carries its array under `rows`"
    );
    assert!(
        widget.contains(concat!(
            "/** Result of `platform-fixture:widget/query@1.0.0`. */\n",
            "export interface WidgetQueryResult {\n",
            "  /** The rows this page carries. */\n",
            "  readonly item: readonly WidgetQueryRow[];\n",
            "  /** The next page's cursor, or null at the last page. */\n",
            "  readonly nextCursor: string | null;\n",
            "}\n",
        )),
        "a page carries its array under `item`, beside the cursor"
    );
    assert!(
        widget.contains(concat!(
            "/** One row of `platform-fixture:widget/list@1.0.0`. */\n",
            "export interface WidgetListRow {\n",
        )),
        "the row keeps the result leaf fields"
    );
    assert!(
        widget.contains("): Promise<Outcome<WidgetQueryResult>> {"),
        "the function returns the envelope, not one row"
    );
    for record in ["WidgetGet", "WidgetArchive", "WidgetCreate", "WidgetUpdate"] {
        assert!(
            widget.contains(&format!("export interface {record}Result {{")),
            "{record} returns one record"
        );
        assert!(
            !widget.contains(&format!("{record}Row")),
            "{record} needs no row type"
        );
    }
}

/// Defect 2 of the Epic 2 review. The run time renames the members that a
/// field map declares and nothing else, so a `json` value keeps its own keys.
#[test]
fn a_field_map_declares_every_member_and_stops_at_a_json_value() {
    let ir = release();
    let files = emit_ts_client(&ir).unwrap();
    let widget = widget(&files);

    assert!(
        widget.contains(concat!(
            "export const WIDGET_LIST_REQUEST_FIELDS: FieldMap = {\n",
            "  \"maker_id\": \"makerId\",\n",
            "  \"selector\": \"selector\",\n",
            "};\n",
        )),
        "a json member is a plain entry, so the run time copies its value"
    );
    assert!(
        widget.contains(concat!(
            "export const WIDGET_QUERY_REQUEST_FIELDS: FieldMap = {\n",
            "  \"cursor\": \"cursor\",\n",
            "  \"filter\": {\n",
            "    member: \"filter\",\n",
            "    fields: {\n",
            "      \"code\": \"code\",\n",
            "    },\n",
            "  },\n",
            "  \"limit\": \"limit\",\n",
            "  \"sort\": {\n",
            "    member: \"sort\",\n",
            "    fields: {\n",
            "      \"direction\": \"direction\",\n",
            "      \"field\": \"field\",\n",
            "    },\n",
            "  },\n",
            "};\n",
        )),
        "a declared object carries the members inside it"
    );
    assert!(
        widget.contains(concat!(
            "export const WIDGET_LIST_RESULT_FIELDS: FieldMap = {\n",
            "  \"rows\": {\n",
            "    member: \"rows\",\n",
            "    fields: {\n",
            "      \"attributes\": \"attributes\",\n",
            "      \"code\": \"code\",\n",
            "      \"edit_version\": \"editVersion\",\n",
            "      \"id\": \"id\",\n",
            "    },\n",
            "  },\n",
            "};\n",
        )),
        "the result map describes the envelope, and the rows one level down"
    );
    assert!(
        widget.contains("  \"next_cursor\": \"nextCursor\",\n"),
        "a page names its cursor beside the rows"
    );
    assert_eq!(
        widget.matches(": FieldMap = ").count(),
        16,
        "eight operations carry one input map and one result map each"
    );
    assert!(
        widget.contains(
            "      items: items.map((item) => toWire(item, WIDGET_LIST_REQUEST_FIELDS)),\n"
        )
    );
    assert!(widget.contains("    WIDGET_LIST_RESULT_FIELDS,\n"));

    let combined = combined(&files);
    for absent in [
        "toCamel",
        "toSnake",
        "toUpperCase",
        "toLowerCase",
        "replace(",
    ] {
        assert!(
            !combined.contains(absent),
            "the emitted TypeScript holds no name rule: {absent}"
        );
    }
    assert!(
        widget.contains("import { reviveOutcome, toWire } from \"@wamn/web-runtime\";"),
        "one generic pair reads a map, and it lives in the runtime package"
    );
}

/// The second smaller point of the Epic 2 review. The IR holds the declared
/// values, and a member that accepts three spellings is not a `string`.
#[test]
fn a_declared_value_domain_types_as_a_union_of_its_literals() {
    let ir = release();
    let files = emit_ts_client(&ir).unwrap();
    let widget = widget(&files);

    assert!(
        widget.contains("  readonly code: \"priority\" | \"standard\";\n"),
        "a model enum field types as its domain"
    );
    assert!(
        widget.contains("  readonly outcome: \"deleted\" | null;\n"),
        "a delete states the one outcome it exposes"
    );
    assert!(
        widget.contains(concat!(
            "export interface WidgetQueryRequestSort {\n",
            "  /** `text` */\n",
            "  direction: \"ascending\" | \"descending\";\n",
            "  /** `text` */\n",
            "  field: \"created_at\";\n",
            "}\n",
        )),
        "the sort takes its domain from the paging contract, not from the release schema"
    );
    assert!(
        widget.contains("  cursor?: string;\n"),
        "an opaque cursor keeps its type"
    );

    let mut widened = release();
    let sort = widened.models[0]
        .operations
        .iter_mut()
        .find(|operation| operation.name == "query")
        .and_then(|operation| operation.paging.as_mut())
        .and_then(|paging| paging.sort.as_mut())
        .expect("the fixture query sorts");
    sort.fields.push("code".to_owned());
    let widened = emit_ts_client(&widened).unwrap();
    assert!(
        source(&widened, "generated/client-ts/widget.ts")
            .contains("  field: \"created_at\" | \"code\";\n"),
        "the union states exactly what the contract permits"
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
    // `delete` is a reserved word and gains one underscore, so a sibling that
    // already spells the escape takes the same name.
    for (index, name) in ["delete_", "delete"].into_iter().enumerate() {
        let mut operation = template.clone();
        operation.name = name.to_owned();
        operation.operation = format!("platform-fixture:widget/collide-{index}@1.0.0");
        ir.models[0].operations.push(operation);
    }
    let refusal = emit_ts_client(&ir).expect_err("a duplicate TypeScript name refuses");
    assert_eq!(refusal.kind(), ClientTsErrorKind::NameCollision);
    assert!(refusal.to_string().contains("\"delete_\""), "{refusal}");

    let mut unnameable = release();
    unnameable.models[0].operations[0].name = "9lives".to_owned();
    let refusal = emit_ts_client(&unnameable).expect_err("a name with no spelling refuses");
    assert_eq!(refusal.kind(), ClientTsErrorKind::UnnameableIdentifier);
    assert!(refusal.to_string().contains("9lives"), "{refusal}");
}

/// The first smaller point of the Epic 2 review. A contract name that does not
/// reverse is refused where it is written, not discovered by a reader.
#[test]
fn a_contract_name_that_does_not_reverse_refuses_and_names_its_path() {
    let mut operation_name = release();
    operation_name.models[0].operations[0].name = "listWidgets".to_owned();
    let refusal = emit_ts_client(&operation_name).expect_err("a capital does not reverse");
    assert_eq!(refusal.kind(), ClientTsErrorKind::IrreversibleName);
    let text = refusal.to_string();
    assert!(text.starts_with("irreversible_name: "), "{text}");
    assert!(text.contains("platform-fixture:widget/"), "{text}");
    assert!(text.contains("\"list_widgets\""), "{text}");

    let mut field_name = release();
    let archive = field_name.models[0]
        .operations
        .iter_mut()
        .find(|operation| operation.name == "archive")
        .expect("the fixture declares widget.archive");
    archive.input_fields[0].path = "editVersion".to_owned();
    let refusal = emit_ts_client(&field_name).expect_err("a field that does not reverse refuses");
    assert_eq!(refusal.kind(), ClientTsErrorKind::IrreversibleName);
    let text = refusal.to_string();
    assert!(
        text.contains("platform-fixture:widget/archive@1.0.0"),
        "{text}"
    );
    assert!(text.contains("field \"editVersion\""), "{text}");

    emit_ts_client(&release()).expect("every fixture name reverses");
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

#[test]
fn a_package_manifest_carries_the_authored_name_and_the_release_version() {
    let file = wamn_schema_generator::client_ts::emit_ts_package_json(
        "@wamn/platform-fixture-client",
        "1.0.0",
    );
    assert_eq!(file.path(), "generated/client-ts/package.json");
    let source = std::str::from_utf8(file.bytes()).unwrap();
    assert_eq!(
        source,
        concat!(
            "{\n",
            "  \"name\": \"@wamn/platform-fixture-client\",\n",
            "  \"version\": \"1.0.0\",\n",
            "  \"type\": \"module\",\n",
            "  \"private\": true,\n",
            "  \"exports\": {\n",
            "    \".\": \"./index.ts\"\n",
            "  },\n",
            "  \"dependencies\": {\n",
            "    \"@wamn/web-runtime\": \"^0.1.0\",\n",
            "    \"@wamn/ui\": \"^0.1.0\"\n",
            "  }\n",
            "}\n",
        )
    );
    let parsed: serde_json::Value = serde_json::from_str(source).unwrap();
    assert_eq!(
        parsed["dependencies"]["@wamn/web-runtime"], "^0.1.0",
        "the bindings import the hand-written runtime"
    );
    assert_eq!(
        parsed["dependencies"]["@wamn/ui"], "^0.1.0",
        "the components render through the hand-written UI package"
    );
}

#[test]
fn a_package_that_names_no_client_package_generates_no_typescript() {
    let package = fixture::generate_fixture();
    assert!(
        package
            .files()
            .iter()
            .all(|file| !file.path().contains("client-ts")),
        "TypeScript is opt-in"
    );
}

#[test]
fn an_invalid_client_package_name_refuses_and_names_the_package() {
    use wamn_schema_generator::GenerateErrorKind;

    let catalog = fixture::catalog();
    for (name, reason) in [
        ("", "must not be empty"),
        ("Widgets", "lowercase"),
        ("@scope", "scope and a package"),
        ("-leading", "start with"),
        ("has space", "admits only"),
    ] {
        let mut value = fixture::manifest();
        value["client_package"] = serde_json::json!({ "name": name });
        let refusal = fixture::try_generate_with(&catalog, &value)
            .expect_err("an invalid client package name refuses");
        assert_eq!(
            refusal.kind(),
            GenerateErrorKind::InvalidClientPackage,
            "{name:?}"
        );
        let text = refusal.to_string();
        assert!(text.contains("platform_fixture"), "{name:?}: {text}");
        assert!(text.contains(reason), "{name:?}: {text}");
    }

    let mut value = fixture::manifest();
    value["client_package"] = serde_json::json!({ "name": "@wamn/platform-fixture-client" });
    fixture::generate_with(&catalog, &value);
}

/// An application integer is int32 by default, so an integer the operation
/// contract declares is a plain number in the browser and on the wire. A
/// declared int64 is opt-in, stays a string, and stays opaque.
#[test]
fn a_contract_integer_is_a_number_and_a_declared_int64_stays_opaque() {
    let ir = release();
    let files = emit_ts_client(&ir).unwrap();
    let widget = widget(&files);

    assert!(
        widget.contains("  limit?: number;\n"),
        "the page limit is the number the route wants"
    );
    assert!(
        widget.contains("  expectedEditVersion: Int64;\n"),
        "a declared int64 keeps its opaque alias"
    );
}

/// EXIT GATE for `wamn-c2y5.5`: an authored description reaches the type an
/// author reads, and an undescribed member keeps the one line it had.
///
/// A label changes no TypeScript text. A label is for a screen, and a comment
/// is for the person who writes the code that calls this.
#[test]
fn a_described_member_carries_its_description_and_the_rest_are_unchanged() {
    let ir = release();
    let files = emit_ts_client(&ir).unwrap();
    let widget = widget(&files);

    assert!(
        widget.contains(concat!(
            "  /**\n",
            "   * What the operator recorded about this batch.\n",
            "   *\n",
            // The release publishes this command, and its schema does not
            // require the note, so the member states that beside its type.
            "   * `text`, omittable\n",
            "   */\n",
        )),
        "the authored sentence comes before the wire spelling"
    );
    assert!(
        widget.contains(concat!(
            "  /**\n",
            "   * One line for each widget this batch records.\n",
            "   *\n",
            "   * `array`\n",
            "   */\n",
        )),
        "a repeated group's declared description reaches the type, although \
         the published schema states none"
    );
    assert!(
        widget.contains("  /** `uuid` */\n"),
        "a member with no description keeps its single line"
    );
    assert!(
        widget.contains(concat!(
            "/**\n",
            " * Input for `platform-fixture:widget/record-batch@1.0.0`.\n",
            " *\n",
            " * One submission that records several lines at once.\n",
            " */\n",
        )),
        "an operation states its own description above its input type"
    );
    assert!(
        widget.contains("/** Input for `platform-fixture:widget/get@1.0.0`. */\n"),
        "an operation with no description keeps its single line"
    );
    assert!(
        !widget.contains("Widget code"),
        "a label is for a screen, and the bindings state none"
    );
}
