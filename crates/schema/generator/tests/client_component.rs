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
            "generated/client-ts/components/widget_maker.tsx",
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
        8,
        "every screen the plan gives a role has one component"
    );
    // The columns are the plan's columns, in contract order, with the cell type
    // that the release declared.
    assert!(widget.contains(concat!(
        "const LIST_COLUMNS: ColumnDef<WidgetListRow, unknown>[] = [\n",
        "  {\n",
        "    accessorKey: \"attributes\",\n",
        // The fixture authors this column's label, so the header is the
        // authored text rather than the path. `wamn-c2y5.4` states the rule.
        "    header: \"Attributes\",\n",
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
        "  readonly input: WidgetGetDetailInput;\n",
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
        detail.contains("{ ...input, requestId: newRequestId() } as WidgetGetRequest,"),
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
        "  makerId: z.string().regex(UUID_TEXT, \"expected a UUID\").nullable().optional(),\n",
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
    for reserved in ["requestId", "idempotencyKey"] {
        assert!(
            !create.contains(&format!("<form.Field name={{`{reserved}`}}>")),
            "the operator never sees {reserved}"
        );
    }
    assert_eq!(
        create.matches("<form.Field").count(),
        3,
        "one control for each plan input, and no other"
    );
    assert!(
        create.contains("member: refusedMember(outcome.detail)"),
        "a refusal that names a member reaches that member"
    );
    assert!(
        create.contains("<Show when={refusalMarks(refusal()?.member ?? null, \"code\")}>"),
        "the control states the path it declares, and the runtime decides"
    );

    // A nested input keeps its shape in the schema and in the field name.
    assert!(widget.contains(concat!(
        "const UPDATE_INPUT = z.object({\n",
        "  change: z\n",
        "    .object({\n",
        "      code: z.string().optional(),\n",
    )));
    assert!(widget.contains("<form.Field name={`change.code`}>"));
    let update = widget
        .split("export function WidgetUpdateForm")
        .nth(1)
        .expect("the update form exists");
    assert!(
        update.contains("<Show when={refusalMarks(refusal()?.member ?? null, \"change.code\")}>"),
        "a nested control states its whole declared path"
    );
}

/// A refusal that names a path inside a repeated group marks the control of the
/// line it names, and of every line when it names none. The runtime holds that
/// rule, and the control states its declared path and its own index.
#[test]
fn a_repeated_control_states_its_declared_path_and_its_index() {
    let files = emit(&release());
    let widget = widget(&files);
    let batch = widget
        .split("export function WidgetRecordBatchForm")
        .nth(1)
        .expect("the record batch form exists");

    assert!(
        batch.contains("<form.Field name={\"value.line\"} mode=\"array\">"),
        "the repeated group renders as a list"
    );
    assert!(
        batch.contains(concat!(
            "<Show when={refusalMarks(refusal()?.member ?? null, ",
            "\"value.line[].quantity\", index())}>"
        )),
        "a control inside the group states its declared path and its index"
    );
    assert!(
        batch.contains("<Show when={refusalMarks(refusal()?.member ?? null, \"value.note\")}>"),
        "a control outside the group states no index"
    );
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
        widget.contains("  readonly key: WidgetGetDetailInput;"),
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
        widget.contains("  readonly key: WidgetGetDetailInput;"),
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
        !update.contains("<form.Field name={`id`}>"),
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
            "      makerId: z.string().regex(UUID_TEXT, \"expected a UUID\").nullable().optional(),\n",
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
    unsupported
        .models
        .iter_mut()
        .flat_map(|model| model.operations.iter_mut())
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

/// A caller prefills one member of a nested value, so the initial values state
/// every level as optional. A supplied field and a repeated group are not part
/// of that type: the platform writes the first, and the form owns the second.
#[test]
fn initial_values_reach_a_nested_member_and_name_no_group() {
    let files = emit(&release());
    let widget = widget(&files);

    assert!(
        widget.contains(concat!(
            "export interface WidgetUpdateFormInitial {\n",
            "  change?: {\n",
            "    code?: string;\n",
            "    makerId?: Uuid | null;\n",
            "    note?: string | null;\n",
            "  };\n",
            "}\n",
        )),
        "a nested member is optional at every level, and it takes the type the \
         served input states"
    );
    assert!(
        widget.contains(concat!(
            "export interface WidgetRecordBatchFormInitial {\n",
            "  value?: {\n",
            "    makerId?: Uuid | null;\n",
            "    note?: string | null;\n",
            "  };\n",
            "}\n",
        )),
        "the repeated group is not a member, and neither is a supplied field"
    );
    assert!(
        widget.contains("  readonly initial?: WidgetRecordBatchFormInitial;"),
        "the prop states that type"
    );
    assert!(
        !widget.contains("readonly initial?: Partial<"),
        "no form states a shallow partial of its request"
    );
}

/// A screen that names a record states the record inputs alone. The request
/// identity is supplied, so no caller states a value that the component writes
/// over.
#[test]
fn a_record_prop_names_the_record_inputs_and_no_supplied_value() {
    let files = emit(&release());
    let widget = widget(&files);

    assert!(
        widget.contains(concat!(
            "export interface WidgetGetDetailInput {\n",
            "  readonly id: Uuid;\n",
            "}\n",
        )),
        "the detail states the record inputs alone"
    );
    assert!(
        widget.contains("  readonly input: WidgetGetDetailInput;"),
        "the detail prop takes that type"
    );
    assert!(
        widget.contains("  readonly key: WidgetGetDetailInput;"),
        "a command that binds a read takes the same record"
    );
    assert!(
        !widget.contains("readonly input: WidgetGetRequest;")
            && !widget.contains("readonly key: WidgetGetRequest;"),
        "no prop takes the whole request of a read"
    );
    assert!(
        widget.contains("{ ...input, requestId: newRequestId() } as WidgetGetRequest,"),
        "the component writes the request identity itself"
    );
}

/// EXIT GATE for `wamn-c2y5.4`: every place a component states text for a
/// person reads the authored label, and a field with none keeps the derived
/// name.
///
/// The screen name is an exported constant and not an element. A component
/// does not own a heading, because the page that places it does. Owner ruling
/// of 2026-09-22.
#[test]
fn a_component_reads_the_authored_label_everywhere_it_states_text() {
    let files = emit(&release());
    let widget = widget(&files);

    // A table column header and a detail term.
    assert!(
        widget.contains("    header: \"Attributes\","),
        "a table column header reads the authored label"
    );
    assert!(
        widget.contains("        <dt>Widget code</dt>"),
        "a detail term reads the authored label"
    );
    assert!(
        widget.contains("        <dt>created at</dt>"),
        "a column with no authored label keeps its derived name"
    );

    // A form control, and one inside a repeated group.
    assert!(
        widget.contains("      Batch note"),
        "a form control reads the authored label"
    );
    assert!(
        widget.contains("Quantity received"),
        "a control inside a repeated group reads its own authored label"
    );
    assert!(
        widget.contains("            <legend>Batch lines</legend>"),
        "a repeated group reads the label its line bound declares"
    );

    // A page control that names a model column.
    assert!(
        widget.contains("          Widget code\n          <input type=\"text\""),
        "a filter control reads the column's authored label"
    );

    // The screen name, exported once for each component and rendered nowhere.
    assert!(widget.contains("export const WidgetQueryTableLabel = \"Find widgets\";"));
    assert!(
        widget.contains("export const WidgetRecordBatchFormLabel = \"Record a batch\";"),
        "an authored command states its own screen name"
    );
    assert!(
        widget.contains("export const WidgetGetDetailLabel = \"get\";"),
        "a screen with no authored label takes its operation name"
    );
    assert_eq!(
        widget
            .matches("export const Widget")
            .filter(|_| true)
            .count(),
        widget.matches("export function Widget").count(),
        "one exported label for each component"
    );
    for heading in ["<h1", "<h2", "<h3"] {
        assert!(
            !widget.contains(heading),
            "no component renders a heading: {heading}"
        );
    }
}

/// EXIT GATE for `wamn-rm14.4`: an input that names a record renders a
/// selector fed by the list the plan chose, and a plain input does not.
///
/// The options carry the key as the value and the display field as the text,
/// so the operator reads a name and the release receives an identity.
#[test]
fn a_populated_input_renders_a_selector_fed_by_its_list() {
    let files = emit(&release());
    let widget = widget(&files);

    // The list of another model is imported under an alias, because two
    // models can both declare a `list`.
    assert!(
        widget.contains(concat!(
            "import {\n",
            "  query as widgetMakerQuery,\n",
            "  type WidgetMakerQueryRequest,\n",
            "  type WidgetMakerQueryRow,\n",
            "} from \"../widget_maker.js\";\n",
        )),
        "{widget}"
    );
    assert!(
        widget.contains("value={String(row.id)}"),
        "the option carries the key the plan named"
    );
    assert!(
        widget.contains("{String(row.name)}"),
        "the default display field reaches the option text"
    );
    assert!(
        widget.contains("{String(row.code)}"),
        "an authored display field reaches the option text"
    );
    assert!(
        widget.contains("selected={String(field().state.value ?? \"\") === String(row.id)}"),
        "the option states its own selected state, because the rows arrive \
         after the first render"
    );

    // A narrowed selector reads the value the operator already chose, and it
    // reads again when that value changes.
    assert!(
        widget.contains("  const formValues = useStore(form.store, (state) => state.values);\n")
    );
    assert!(widget.contains(concat!(
        "  createEffect(() => {\n",
        "    formValues();\n",
        "    const narrowed = form.getFieldValue(`value.makerId`) as string | null;\n",
        "    void readWidgetListOptions(narrowed ?? null);\n",
        "  });\n",
    )));
    assert!(
        widget.contains("{ requestId: newRequestId(), makerId: narrowed } as WidgetListRequest,"),
        "the narrowing value fills the list input the plan named"
    );
    assert!(
        widget.contains(concat!(
            "    if (narrowed === null || narrowed === \"\") {\n",
            "      setWidgetListOptions([]);\n",
            "      return;\n",
            "    }\n",
        )),
        "a narrowed selector offers nothing until the operator chooses the \
         record it narrows by, instead of calling the list with no value"
    );

    // A plain control is untouched.
    let create = widget
        .split("export function WidgetCreateForm")
        .nth(1)
        .expect("the create form exists")
        .split("\n/**")
        .next()
        .expect("the form ends");
    assert!(
        create.contains(concat!(
            "            Widget code\n",
            "            <input\n",
            "              type=\"text\"\n",
        )),
        "an input that names no record stays a text control"
    );
}

/// EXIT GATE for `wamn-rm14.5`: a row hands its values to the form the plan
/// named, and the form takes them through the initial-values prop.
///
/// The callback carries the declared pairs and nothing else, so the page
/// decides what opening a row means, which the Epic 4 verdict states.
#[test]
fn a_row_hands_its_values_to_the_form_the_plan_named() {
    let files = emit(&release());
    let maker = source(&files, "generated/client-ts/components/widget_maker.tsx");

    assert!(
        maker.contains(
            "  readonly onFillWidgetCreate?: (initial: WidgetCreateFormInitial) => void;"
        ),
        "the table states one callback for each form its rows open"
    );
    assert!(
        maker.contains(concat!(
            "import {\n",
            "  type WidgetCreateFormInitial,\n",
            "  type WidgetRecordBatchFormInitial,\n",
            "  type WidgetUpdateFormInitial,\n",
            "} from \"./widget.js\";\n",
        )),
        "a form of another model states its type in that model's module"
    );
    assert!(
        maker.contains(
            "onClick={() => props.onFillWidgetCreate?.(writeMember({} as WidgetCreateFormInitial, [\"makerId\"], row.original.id))}"
        ),
        "the row writes its key at the declared input path"
    );
    assert!(
        maker.contains(
            "props.onFillWidgetRecordBatch?.(writeMember({} as WidgetRecordBatchFormInitial, [\"value\", \"makerId\"], row.original.id))"
        ),
        "a nested input path is written one member at a time"
    );

    let widget = widget(&files);
    assert!(
        widget.contains("  readonly initial?: WidgetRecordBatchFormInitial;"),
        "the form takes those values through the prop Epic 5 shaped"
    );
    assert!(
        !widget.contains("onFillWidgetGet"),
        "a record read is not a form, so no row fills it"
    );
}

/// EXIT GATE for `wamn-rm14.6`: a repeated group states its authored label,
/// its declared bounds and the spelling of every value that travels as text.
///
/// Three filed defects close here: `wamn-j3yr` for the legend, `wamn-z5vd`
/// for the bounds, and `wamn-s3kd` for the spellings. All three are input
/// rules. The browser still trusts the platform for the values inside a
/// reply, which is the owner ruling of 2026-09-21.
#[test]
fn a_repeated_group_states_its_label_its_bounds_and_its_spellings() {
    let files = emit(&release());
    let widget = widget(&files);

    // wamn-j3yr: the line bound declares the group, so the legend is authored.
    assert!(
        widget.contains("            <legend>Batch lines</legend>"),
        "the group reads the label its line bound declares"
    );

    // wamn-z5vd: the bounds reach the schema and both controls.
    assert!(widget.contains(concat!(
        "        .array()\n",
        "        .min(1)\n",
        "        .max(10)\n",
        "        .optional(),\n",
    )));
    assert!(
        widget.contains("disabled={!canAdd(group().state.value ?? [], 10)}"),
        "the add control stops at the declared maximum"
    );
    assert!(
        widget.contains("disabled={!canRemove(group().state.value ?? [], 1)}"),
        "the remove control stops at the declared minimum"
    );

    // wamn-s3kd: a value that travels as text states its spelling once per
    // module, and every control that needs it names that spelling.
    assert!(widget.contains(concat!(
        "/** What the release accepts: one UUID, hyphenated. */\n",
        "const UUID_TEXT = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}",
        "-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;\n",
    )));
    assert!(widget.contains("z.string().regex(UUID_TEXT, \"expected a UUID\")"));
    assert!(widget.contains("z.string().regex(NUMERIC_TEXT, \"expected decimal text\")"));
    assert!(
        !widget.contains("const TIMESTAMP_TEXT"),
        "a module writes no rule it does not apply"
    );
}
