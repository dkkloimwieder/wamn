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
        7,
        "every screen the plan gives a role has one component"
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

/// A detail screen shows one record, so it reads on its own and needs no form
/// machinery.
#[test]
fn a_detail_screen_reads_its_record_and_shows_every_plan_column() {
    let files = emit(&release());
    let widget = widget(&files);
    assert!(widget.contains(concat!(
        "export interface WidgetGetDetailProps {\n",
        "  /** The transport the application supplies. */\n",
        "  readonly transport: Transport;\n",
        "  /** The input that names the record. */\n",
        "  readonly input: WidgetGetRequest;\n",
    )));
    let detail = widget
        .split("export function WidgetGetDetail")
        .nth(1)
        .expect("the detail exists")
        .split("\n/**")
        .next()
        .expect("the detail ends where the next screen begins");
    assert!(
        detail.contains("createResource(\n    () => props.input,"),
        "it reads on mount and again when the input changes"
    );
    assert!(
        detail.contains("{ ...input, requestId: newRequestId() },"),
        "the request identity comes from the runtime"
    );
    for (label, member, cell) in [
        ("id", "[\"id\"]", "uuid"),
        ("edit version", "[\"editVersion\"]", "int64"),
        ("created at", "[\"createdAt\"]", "timestamptz"),
    ] {
        assert!(
            detail.contains(&format!(
                "<dt>{label}</dt>\n        <dd>{{cellText(readMember(record(), {member}), {cell:?})}}</dd>"
            )),
            "{label} is one plan column"
        );
    }
    assert!(
        !detail.contains("createSolidTable"),
        "one record needs no table"
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

/// A form renders what the operator fills, and the platform supplies the rest.
#[test]
fn a_form_renders_the_plan_inputs_and_supplies_the_reserved_ones() {
    let files = emit(&release());
    let widget = widget(&files);
    let create = widget
        .split("export function WidgetCreateForm")
        .nth(1)
        .expect("the create form exists")
        .split("\n/**")
        .next()
        .expect("the form ends");

    assert!(widget.contains(concat!(
        "const CREATE_INPUT = z.object({\n",
        "  code: z.string().optional(),\n",
        "  note: z.string().nullable().optional(),\n",
        "});\n",
    )));
    assert!(
        create.contains("const checked = CREATE_INPUT.safeParse(value);"),
        "the operator's input is checked before the request goes out"
    );
    assert!(
        create.contains("item = writeMember(item, [\"requestId\"], newRequestId());")
            && create
                .contains("item = writeMember(item, [\"idempotencyKey\"], newIdempotencyKey());"),
        "the reserved inputs come from the runtime at submit time"
    );
    for reserved in ["requestId\"", "idempotencyKey\""] {
        assert!(
            !create.contains(&format!("<form.Field name={{\"{reserved}}}>")),
            "the operator never sees {reserved}"
        );
    }
    assert_eq!(
        create.matches("<form.Field").count(),
        2,
        "one control for each plan input, and no other"
    );
    assert!(
        create.contains("member: refusedMember(outcome.detail)"),
        "a refusal that names a member reaches that member"
    );
    assert!(
        create.contains("<Show when={refusal()?.member === \"code\"}>"),
        "the marked member shows the refusal beside its control"
    );

    // A nested input keeps its shape in the schema and in the field name.
    assert!(widget.contains(concat!(
        "const UPDATE_INPUT = z.object({\n",
        "  change: z\n",
        "    .object({\n",
        "      code: z.string().optional(),\n",
    )));
    assert!(widget.contains("<form.Field name={\"change.code\"}>"));
}

/// A command that sends a revision reads the record first, because a stale
/// revision is what the conflict outcome names.
#[test]
fn a_revision_bound_form_reads_the_record_before_it_sends() {
    let files = emit(&release());
    let widget = widget(&files);
    let form = widget
        .split("export function WidgetUpdateForm")
        .nth(1)
        .expect("the update form exists");
    assert!(
        widget.contains("  readonly key: WidgetGetRequest;"),
        "the form takes the record it changes"
    );
    assert!(
        form.contains("const record = await get(props.transport, [\n        { ...props.key, requestId: newRequestId() },\n      ]);"),
        "it reads that record first"
    );
    assert!(
        form.contains("item = writeMember(item, [\"expectedEditVersion\"], readMember(record.value, [\"editVersion\"]) ?? null);"),
        "it sends the revision it read"
    );
    assert!(
        form.contains("if (record.status !== \"completed\") {"),
        "a read that establishes nothing stops the submission"
    );
    let props = widget
        .split("export interface WidgetUpdateFormProps {")
        .nth(1)
        .expect("the update props exist")
        .split('}')
        .next()
        .expect("the props end");
    assert!(
        !props.contains("readonly expectedEditVersion:"),
        "a bound revision is not a prop: {props}"
    );
    assert!(
        widget.contains(
            "readonly expectedEditVersion: WidgetArchiveRequest[\"expectedEditVersion\"];"
        ),
        "a revision with no binding stays a prop, because only the caller knows it"
    );
}

/// A removal is not an edit that an operator undoes, so it confirms first.
#[test]
fn a_delete_confirms_and_sends_the_revision_it_read() {
    let files = emit(&release());
    let widget = widget(&files);
    assert!(widget.contains(concat!(
        "export interface WidgetDeleteDeleteProps {\n",
        "  /** The transport the application supplies. */\n",
        "  readonly transport: Transport;\n",
    )));
    assert!(
        widget.contains("  readonly key: WidgetGetRequest;"),
        "the delete takes the record it removes"
    );
    let remove = widget
        .split("export function WidgetDeleteDelete")
        .nth(1)
        .expect("the delete exists")
        .split("\n/**")
        .next()
        .expect("the delete ends");
    assert!(
        remove.contains("const [confirming, setConfirming] = createSignal(false);"),
        "it asks before it acts"
    );
    assert!(
        remove.contains("<p>remove this record?</p>") && remove.contains(">\n          confirm\n"),
        "the confirmation states what it removes"
    );
    assert!(
        remove.contains("const record = await get(props.transport, ["),
        "it reads the record first"
    );
    assert!(
        remove.contains(
            "item = writeMember(item, [\"expectedEditVersion\"], readMember(record.value, [\"editVersion\"]) ?? null);"
        ),
        "it sends the revision it read"
    );
    assert!(!remove.contains("<form.Field"), "a delete fills no field");
}

/// A bound command writes its key from the record it read, so a control for
/// that key would show a value the submission overwrites.
#[test]
fn a_bound_key_is_a_prop_and_never_a_control() {
    let files = emit(&release());
    let widget = widget(&files);
    let update = widget
        .split("export function WidgetUpdateForm")
        .nth(1)
        .expect("the update form exists")
        .split("\n/**")
        .next()
        .expect("the form ends");
    assert!(
        !update.contains("<form.Field name={\"id\"}>"),
        "the record key comes from the props"
    );
    assert!(
        update.contains(
            "item = writeMember(item, [\"id\"], readMember(record.value, [\"id\"]) ?? null);"
        ),
        "the submission writes the key from the record it read"
    );
    assert!(
        widget.contains(concat!(
            "const UPDATE_INPUT = z.object({\n",
            "  change: z\n",
            "    .object({\n",
            "      code: z.string().optional(),\n",
            "      note: z.string().nullable().optional(),\n",
            "    })\n",
            "    .optional(),\n",
            "});\n",
        )),
        "the schema states what the operator fills, and the bound key is absent"
    );
}

/// The bytes must not move when nothing in the contract moved.
#[test]
fn the_emitted_components_are_byte_stable() {
    let mut ir = release();
    let first = emit(&ir);
    assert_eq!(first, emit(&ir));
    ir.models[0].operations.reverse();
    assert_eq!(
        first,
        emit(&ir),
        "the contract's incidental order does not reach the bytes"
    );
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
