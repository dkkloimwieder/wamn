use wamn_schema_generator::GeneratedFile;
use wamn_schema_generator::client_component::emit_ts_components;
use wamn_schema_generator::client_ir::ClientContractIr;
use wamn_schema_generator::client_plan::ClientPlan;

#[path = "support/platform_fixture.rs"]
mod fixture;

fn release() -> ClientContractIr {
    fixture::client_release()
}

fn emit(ir: &ClientContractIr) -> Vec<GeneratedFile> {
    emit_ts_components(&ClientPlan::from_ir(ir)).expect("the fixture emits its components")
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
    source(files, "generated/client-ts/components/widget.tsx")
}

#[test]
fn every_table_screen_gets_one_component_and_its_plan_columns() {
    let files = emit(&release());
    assert_eq!(
        files.iter().map(GeneratedFile::path).collect::<Vec<_>>(),
        [
            "generated/client-ts/components/widget.tsx",
            "generated/client-ts/components/index.ts",
        ]
    );
    let widget = widget(&files);
    for stem in ["WidgetQuery", "WidgetList"] {
        assert!(
            widget.contains(&format!(
                "export function {stem}Table(props: {stem}TableProps) {{"
            )),
            "{stem} takes the table role"
        );
    }
    assert_eq!(
        widget.matches("export function ").count(),
        2,
        "only the two table screens get a component in this epic"
    );
    // The columns are the plan's columns, in contract order, with the cell type
    // that the release declared.
    assert!(widget.contains(concat!(
        "const LIST_COLUMNS: ColumnDef<WidgetListRow, unknown>[] = [\n",
        "  {\n",
        "    accessorKey: \"attributes\",\n",
        "    header: \"attributes\",\n",
        "    cell: (cell) => cellText(cell.getValue() as JsonValue, \"json\"),\n",
        "  },\n",
    )));
    assert!(
        widget.contains("    cell: (cell) => cellText(cell.getValue() as JsonValue, \"int64\"),"),
        "a revision keeps its declared type, so the cell states every digit"
    );
}

#[test]
fn a_page_table_renders_every_control_the_plan_names() {
    let files = emit(&release());
    let widget = widget(&files);
    let query = widget
        .split("export function WidgetQueryTable")
        .nth(1)
        .expect("the query table exists");

    assert!(
        widget.contains("change([\"filter\", \"code\"], event.currentTarget.value.split(\",\")"),
        "a repeated filter takes a list"
    );
    assert!(
        widget.contains("<select onChange={(event) => change([\"sort\", \"field\"], event.currentTarget.value)}>"),
        "the sort field is a choice"
    );
    assert!(
        widget.contains("<option value=\"created_at\">created at</option>"),
        "the choices are exactly what the contract permits"
    );
    assert!(widget.contains("<option value=\"ascending\">ascending</option>"));
    assert!(
        widget.contains("change([\"limit\"], event.currentTarget.value)"),
        "the limit is a control, not an operator field"
    );
    assert!(
        widget.contains("min={1}")
            && widget.contains("max={100}")
            && widget.contains("value={100}"),
        "the limit states the bounds the contract declares"
    );
    assert!(
        query.contains("writeMember(request, [\"cursor\"], cursor)"),
        "the next page carries the cursor the last reply returned"
    );
    assert!(
        query.contains("appendPage(page(), rows, outcome.value.nextCursor)"),
        "a page appends, as the terminal does"
    );
    assert!(
        query.contains("const rows = outcome.value.item;"),
        "a page carries its rows under item"
    );
}

#[test]
fn a_bounded_list_has_no_control_and_asks_for_no_next_page() {
    let files = emit(&release());
    let widget = widget(&files);
    let list = widget
        .split("export function WidgetListTable")
        .nth(1)
        .expect("the list table exists")
        .split("export function")
        .next()
        .expect("the list table ends");
    assert!(
        !list.contains("const change ="),
        "there is nothing to change"
    );
    assert!(!list.contains("<select"), "there is no control");
    assert!(
        list.contains("const rows = outcome.value.rows;"),
        "a bounded list carries its rows under rows"
    );
    assert!(
        list.contains("firstPage(rows, null)"),
        "a bounded list has no cursor"
    );
}

#[test]
fn a_row_link_becomes_one_callback_named_from_its_target() {
    let files = emit(&release());
    let widget = widget(&files);
    assert!(
        widget.contains("readonly onOpenWidgetGet?: (row: WidgetQueryRow) => void;"),
        "the plan's row link reaches the props"
    );
    assert!(
        widget.contains("onClick={() => props.onOpenWidgetGet?.(row.original)}"),
        "the row carries the key, and the parent decides what to open"
    );
    for absent in ["href=", "navigate", "router", "<a "] {
        assert!(!widget.contains(absent), "a link is a callback: {absent}");
    }
}

#[test]
fn the_index_names_every_shape_that_gets_no_component() {
    let supported = emit(&release());
    assert!(
        source(&supported, "generated/client-ts/components/index.ts")
            .contains("// Every operation of this release has a screen role."),
        "the fixture gives every operation a role"
    );

    let mut unsupported = release();
    unsupported.models[0]
        .operations
        .iter_mut()
        .find(|operation| operation.name == "archive")
        .expect("the fixture declares widget.archive")
        .kind = "wrangle".to_owned();
    let index = emit(&unsupported);
    let index = source(&index, "generated/client-ts/components/index.ts");
    assert!(
        index.contains(
            "// platform-fixture:widget/archive@1.0.0: the operation kind has no screen role"
        ),
        "{index}"
    );
    assert!(
        !index.contains("WidgetArchive"),
        "a shape with no role gets no component"
    );
}

#[test]
fn a_component_states_no_deployment_fact_and_no_supplied_input() {
    let files = emit(&release());
    let combined: String = files
        .iter()
        .map(|file| std::str::from_utf8(file.bytes()).unwrap())
        .collect();
    for absent in [
        "localhost",
        "http",
        "Bearer",
        "Authorization",
        "baseUrl",
        "fetch(",
    ] {
        assert!(!combined.contains(absent), "deployment fact {absent}");
    }
    // The plan drops the reserved paths, so a control can never carry one.
    for reserved in ["idempotency_key", "occurred_at", "expected_edit_version"] {
        assert!(
            !combined.contains(reserved),
            "the operator never sees {reserved}"
        );
    }
    assert!(
        combined.contains("requestId: newRequestId(),"),
        "the request identity comes from the runtime"
    );
}
