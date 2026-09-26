use serde_json::json;
use wamn_schema_generator::client_ir::{ClientContractIr, FieldIr, OperationIr};
use wamn_schema_generator::client_plan::{
    ChosenRevision, ClientPlan, LinkReason, NoRole, RecordRead, ResolvedColumn, Role, RowLink,
    Rows, ScreenPlan, SuppliedKind, UnresolvedColumn, UnsearchableSelector,
    effective_result_fields,
};

#[path = "support/platform_fixture.rs"]
mod fixture;

fn release() -> ClientContractIr {
    fixture::client_release()
}

fn operation<'a>(ir: &'a mut ClientContractIr, name: &str) -> &'a mut OperationIr {
    ir.models
        .iter_mut()
        .flat_map(|model| model.operations.iter_mut())
        .find(|operation| operation.name == name)
        .expect("the platform fixture declares the operation")
}

fn screen<'a>(plan: &'a ClientPlan<'a>, name: &str) -> &'a ScreenPlan<'a> {
    plan.screens()
        .find(|screen| screen.name == name)
        .expect("the plan holds a screen for the operation")
}

#[test]
fn screens_drop_event_handlers_and_sort_within_their_model() {
    let mut ir = release();
    let mut handler = operation(&mut ir, "get").clone();
    handler.name = "widget_archived".to_owned();
    handler.kind = "event_handler".to_owned();
    handler.operation = "platform-fixture:widget/widget-archived@1.0.0".to_owned();
    let widget = fixture::widget_index(&ir);
    ir.models[widget].operations.push(handler);
    ir.models[widget].operations.reverse();

    let plan = ClientPlan::from_ir(&ir);
    assert_eq!(plan.package, "platform_fixture");
    assert_eq!(plan.models.len(), 3, "a private operation keeps its model");
    assert_eq!(plan.models[widget].model.name, "widget");
    assert_eq!(
        plan.models[widget]
            .screens
            .iter()
            .map(|screen| screen.name)
            .collect::<Vec<_>>(),
        [
            "archive",
            "create",
            "delete",
            "get",
            "list",
            "query",
            "record_batch",
            "update"
        ],
        "the plan drops the event handler and sorts the rest by name"
    );
    assert!(
        plan.screens()
            .all(|screen| ["widget", "widget_maker", "widget_tag"].contains(&screen.model)),
        "every screen states the model that owns it"
    );
}

#[test]
fn supplied_inputs_name_only_the_reserved_contract_paths() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    assert_eq!(
        screen(&plan, "create")
            .supplied
            .iter()
            .map(|field| (field.path, field.kind))
            .collect::<Vec<_>>(),
        [
            ("idempotency_key", SuppliedKind::IdempotencyKey),
            ("request_id", SuppliedKind::RequestId),
        ],
        "reserved paths keep their contract order"
    );
    assert!(
        screen(&plan, "archive").supplied.is_empty(),
        "widget.archive declares no reserved input"
    );

    let mut declared = release();
    let archive = operation(&mut declared, "archive");
    let template = archive.input_fields[0].clone();
    archive.input_fields = [
        "request_id",
        "idempotency_key",
        "value.idempotency_key",
        "occurred_at",
        "value.occurred_at",
        "id",
        "requested_id",
        "value.request_id",
        "occurred_at_local",
    ]
    .into_iter()
    .map(|path| FieldIr {
        path: path.to_owned(),
        children: Vec::new(),
        ..template.clone()
    })
    .collect();
    let plan = ClientPlan::from_ir(&declared);
    assert_eq!(
        screen(&plan, "archive")
            .supplied
            .iter()
            .map(|field| (field.path, field.kind))
            .collect::<Vec<_>>(),
        [
            ("request_id", SuppliedKind::RequestId),
            ("idempotency_key", SuppliedKind::IdempotencyKey),
            ("value.idempotency_key", SuppliedKind::IdempotencyKey),
            ("occurred_at", SuppliedKind::OccurredAt),
            ("value.occurred_at", SuppliedKind::OccurredAt),
        ],
        "a path that resembles a reserved path is operator input"
    );
}

#[test]
fn revision_inputs_join_the_declared_and_guarded_paths() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    for name in ["archive", "delete", "update"] {
        assert_eq!(
            screen(&plan, name).revision_inputs,
            ["expected_edit_version"],
            "{name} sends the revision it read"
        );
    }
    for name in ["create", "get", "query"] {
        assert!(screen(&plan, name).revision_inputs.is_empty(), "{name}");
    }

    let mut guarded = release();
    let archive = operation(&mut guarded, "archive");
    for field in &mut archive.input_fields {
        field.revision = wamn_schema_generator::Revision::default();
    }
    let plan = ClientPlan::from_ir(&guarded);
    assert_eq!(
        screen(&plan, "archive").revision_inputs,
        ["expected_edit_version"],
        "an idempotence guard names a revision input by itself"
    );

    let mut unguarded = guarded.clone();
    operation(&mut unguarded, "archive").idempotent_by = Some(json!({"state": {"guards": {}}}));
    let plan = ClientPlan::from_ir(&unguarded);
    assert!(screen(&plan, "archive").revision_inputs.is_empty());
}

#[test]
fn record_links_repeat_the_contract_record() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    for name in ["delete", "get", "update"] {
        let record = screen(&plan, name)
            .record
            .expect("a record operation links its relation");
        assert_eq!(record.relation, "inventory.widget", "{name}");
        assert_eq!(record.key_field, "id", "{name}");
        assert_eq!(record.key_input, Some("id"), "{name}");
    }
    for name in ["create", "query"] {
        let record = screen(&plan, name)
            .record
            .unwrap_or_else(|| panic!("{name} links its relation"));
        assert_eq!(record.relation, "inventory.widget", "{name}");
        assert_eq!(
            record.key_input, None,
            "{name} reads no record key from input"
        );
    }
    assert!(
        screen(&plan, "archive").record.is_none(),
        "an authored command declares no record link"
    );
}

#[test]
fn revision_bindings_name_the_read_operation_that_supplies_them() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    let binding = screen(&plan, "delete")
        .revision
        .expect("a revision delete reads its current revision first");
    assert_eq!(binding.read_operation, "platform-fixture:widget/get@1.0.0");
    assert_eq!(binding.read_key_input, "id");
    assert_eq!(binding.key_field, "id");
    assert_eq!(binding.revision_field, "edit_version");
    assert_eq!(binding.command_key_input, "id");
    assert_eq!(binding.command_revision_input, "expected_edit_version");
    // The update binds the same read, because its published input declares the
    // record key and the revision it expects.
    let update = screen(&plan, "update")
        .revision
        .expect("a revision update reads its current revision first");
    assert_eq!(update.read_operation, "platform-fixture:widget/get@1.0.0");
    assert_eq!(update.command_revision_input, "expected_edit_version");
    for name in ["archive", "create", "get", "query"] {
        assert!(screen(&plan, name).revision.is_none(), "{name}");
    }
}

#[test]
fn each_platform_operation_takes_its_role_from_kind_and_result_class() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    for (name, kind, result, role) in [
        ("query", "query", Some("page"), Role::Table),
        ("get", "get", Some("one"), Role::Detail),
        ("create", "create", Some("one"), Role::Form),
        ("update", "update", Some("one"), Role::Form),
        ("archive", "command", Some("one"), Role::Form),
        ("delete", "delete", Some("one"), Role::Delete),
    ] {
        let screen = screen(&plan, name);
        assert_eq!(screen.contract.kind, kind, "{name}");
        assert_eq!(screen.result_class, result, "{name}");
        assert_eq!(screen.role, role, "{name}");
        assert!(screen.role.is_supported(), "{name}");
    }
    assert!(plan.unsupported().is_empty());
}

#[test]
fn a_bounded_list_read_and_a_projection_both_take_the_table_role() {
    let mut ir = release();
    let query = operation(&mut ir, "query");
    query.kind = "projection".to_owned();
    query
        .route
        .as_mut()
        .expect("the query serves a route")
        .response
        .result_class = Some("bounded_list".to_owned());
    let plan = ClientPlan::from_ir(&ir);
    assert_eq!(screen(&plan, "query").role, Role::Table);
}

#[test]
fn the_served_result_class_selects_the_role_over_the_declared_one() {
    let mut ir = release();
    operation(&mut ir, "get")
        .route
        .as_mut()
        .expect("the read serves a route")
        .response
        .result_class = Some("page".to_owned());
    let plan = ClientPlan::from_ir(&ir);
    assert_eq!(screen(&plan, "get").contract.result_class, "one");
    assert_eq!(screen(&plan, "get").result_class, Some("page"));
    assert_eq!(
        screen(&plan, "get").role,
        Role::Unsupported(NoRole::UnsupportedResult),
        "the role reads what the release serves"
    );
}

#[test]
fn shapes_with_no_role_are_listed_by_operation_name_with_a_reason() {
    let mut ir = release();
    operation(&mut ir, "get")
        .route
        .as_mut()
        .expect("the read serves a route")
        .response
        .result_class = None;
    let widget_wrangler = operation(&mut ir, "archive");
    widget_wrangler.kind = "wrangle".to_owned();
    let plan = ClientPlan::from_ir(&ir);
    assert_eq!(
        plan.unsupported(),
        [
            ("platform-fixture:widget/archive@1.0.0", NoRole::UnknownKind),
            (
                "platform-fixture:widget/get@1.0.0",
                NoRole::UnsupportedResult
            ),
        ],
        "a report names the canonical operation and why no role fits"
    );
    assert_eq!(
        NoRole::UnknownKind.reason(),
        "the operation kind has no screen role"
    );
    assert_eq!(
        NoRole::UnsupportedResult.to_string(),
        "the result class does not fit the operation kind"
    );
    assert_eq!(
        plan.screens()
            .filter(|screen| screen.role.is_supported())
            .count(),
        10,
        "the other screens keep their role, the second model lists and reads one, and the third updates"
    );
}

fn paths<'a>(fields: &[&'a FieldIr]) -> Vec<&'a str> {
    fields.iter().map(|field| field.path.as_str()).collect()
}

#[test]
fn columns_are_the_served_result_leaves_in_contract_order() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    for name in ["create", "get", "query", "update"] {
        assert_eq!(
            paths(&screen(&plan, name).columns),
            [
                "code",
                "created_at",
                "edit_version",
                "id",
                "maker_id",
                "note"
            ],
            "{name}"
        );
    }
    assert_eq!(paths(&screen(&plan, "delete").columns), ["outcome"]);
    assert_eq!(
        paths(&screen(&plan, "archive").columns),
        ["edit_version", "id", "note"]
    );

    let mut served = release();
    let route = operation(&mut served, "get")
        .route
        .as_mut()
        .expect("the read serves a route");
    route.response.fields.retain(|field| field.path == "code");
    let plan = ClientPlan::from_ir(&served);
    assert_eq!(
        paths(&screen(&plan, "get").columns),
        ["code"],
        "the served result fields win over the declared ones"
    );
}

#[test]
fn inputs_drop_the_supplied_revision_and_cursor_paths() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    assert_eq!(
        paths(&screen(&plan, "delete").inputs),
        ["id"],
        "request_id is supplied and expected_edit_version is a revision"
    );
    assert_eq!(
        paths(&screen(&plan, "create").inputs),
        ["code", "maker_id", "note"]
    );
    assert!(
        screen(&plan, "query").inputs.is_empty(),
        "every input of the fixture query is a page control or a supplied value"
    );
    assert_eq!(
        paths(&screen(&plan, "update").inputs),
        ["change.code", "change.maker_id", "change.note", "id"],
        "the record key is operator input, and the revision is not"
    );

    assert_eq!(
        screen(&plan, "query")
            .paging
            .as_ref()
            .expect("a paged query declares paging")
            .cursor_input,
        Some("cursor"),
        "a page result with a cursor input pages"
    );

    let mut bounded = release();
    operation(&mut bounded, "query")
        .route
        .as_mut()
        .expect("the query serves a route")
        .response
        .result_class = Some("bounded_list".to_owned());
    let plan = ClientPlan::from_ir(&bounded);
    assert!(
        paths(&screen(&plan, "query").inputs).contains(&"cursor"),
        "a bounded list has no page to ask for, so its cursor input is ordinary"
    );
}

#[test]
fn rows_come_from_the_served_result_class() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    assert_eq!(screen(&plan, "query").rows, Rows::List { key: "item" });
    for name in ["archive", "create", "delete", "get", "update"] {
        assert_eq!(screen(&plan, name).rows, Rows::Single, "{name}");
    }

    let mut bounded = release();
    operation(&mut bounded, "query")
        .route
        .as_mut()
        .expect("the query serves a route")
        .response
        .result_class = Some("bounded_list".to_owned());
    let plan = ClientPlan::from_ir(&bounded);
    assert_eq!(screen(&plan, "query").rows, Rows::List { key: "rows" });
}

#[test]
fn paging_carries_the_declared_filters_sort_and_limit() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    let paging = screen(&plan, "query")
        .paging
        .as_ref()
        .expect("the query declares filters, sort and a limit");
    assert_eq!(
        paging
            .filters
            .iter()
            .map(|f| f.field.as_str())
            .collect::<Vec<_>>(),
        ["code"]
    );
    let sort = paging.sort.expect("the query sorts");
    assert_eq!(sort.fields, ["created_at"]);
    assert_eq!(sort.directions, ["ascending", "descending"]);
    let limit = paging.limit.expect("the query limits its rows");
    assert_eq!((limit.default, limit.minimum, limit.maximum), (100, 1, 100));
    assert_eq!(
        paging.cursor_input,
        Some("cursor"),
        "the served query publishes a cursor input"
    );
    assert_eq!(paging.filter_inputs, ["filter.code[]"]);
    assert_eq!(paging.sort_field_input, Some("sort.field"));
    assert_eq!(paging.sort_direction_input, Some("sort.direction"));
    assert_eq!(paging.limit_input, Some("limit"));
    assert_eq!(
        paging.inputs(),
        [
            "filter.code[]",
            "sort.field",
            "sort.direction",
            "limit",
            "cursor"
        ],
        "every page control names the input path that carries it"
    );
    for name in ["archive", "create", "delete", "get", "update"] {
        assert!(screen(&plan, name).paging.is_none(), "{name}");
    }
}

/// A page control is input, and it is not one of the operator's own fields.
/// An emitter that renders `inputs` must get no page control by accident.
#[test]
fn a_screen_names_its_page_controls_apart_from_the_operator_fields() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    let query = screen(&plan, "query");
    assert!(query.inputs.is_empty(), "the fixture query is all controls");

    let mut mixed = release();
    let query = operation(&mut mixed, "query");
    let template = query.input_fields[0].clone();
    query.input_fields.push(FieldIr {
        path: "note".to_owned(),
        children: Vec::new(),
        revision: wamn_schema_generator::Revision::default(),
        ..template
    });
    let plan = ClientPlan::from_ir(&mixed);
    assert_eq!(
        paths(&screen(&plan, "query").inputs),
        ["note"],
        "an operator field stays, and every page control leaves"
    );

    let mut unsorted = release();
    operation(&mut unsorted, "query")
        .paging
        .as_mut()
        .expect("the query pages")
        .sort = None;
    let plan = ClientPlan::from_ir(&unsorted);
    let paging = screen(&plan, "query")
        .paging
        .as_ref()
        .expect("the query still filters and limits");
    assert_eq!(paging.sort_field_input, None);
    assert_eq!(paging.sort_direction_input, None);
    assert_eq!(
        paths(&screen(&plan, "query").inputs),
        ["sort.direction", "sort.field"],
        "a control the contract does not declare is ordinary input"
    );
}

#[test]
fn row_links_open_the_record_read_and_the_revision_command() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    for name in ["create", "delete", "query", "update"] {
        assert_eq!(
            screen(&plan, name).row_links,
            [RowLink {
                operation: "platform-fixture:widget/get@1.0.0",
                reason: LinkReason::Record,
            }],
            "{name} rows open the record read"
        );
    }
    assert_eq!(
        screen(&plan, "get").row_links,
        [
            RowLink {
                operation: "platform-fixture:widget/delete@1.0.0",
                reason: LinkReason::Revision,
            },
            RowLink {
                operation: "platform-fixture:widget/update@1.0.0",
                reason: LinkReason::Revision,
            }
        ],
        "a read row opens every command that sends the revision it read"
    );
    assert!(
        screen(&plan, "archive").row_links.is_empty(),
        "an operation with no record link opens nothing"
    );

    let mut unserved = release();
    operation(&mut unserved, "get").route = None;
    let plan = ClientPlan::from_ir(&unserved);
    assert!(
        plan.screens().all(|screen| screen
            .row_links
            .iter()
            .all(|link| link.operation != "platform-fixture:widget/get@1.0.0")),
        "an unserved operation is no link target"
    );
}

#[test]
fn reads_and_confirmed_deletes_follow_from_kind_and_role() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    for name in ["get", "query"] {
        assert!(screen(&plan, name).is_read(), "{name}");
    }
    for name in ["archive", "create", "delete", "update"] {
        assert!(!screen(&plan, name).is_read(), "{name}");
    }
    assert!(screen(&plan, "delete").confirms());
    for name in ["archive", "create", "get", "query", "update"] {
        assert!(!screen(&plan, name).confirms(), "{name}");
    }
}

/// EXIT GATE for `wamn-c2y5.3`: the plan hands an emitter the authored text
/// with no rule of its own, so no emitter reads `wamn.json`.
///
/// Columns and inputs are field references, so the text is already there. The
/// test exists because "already there" is a property an emitter depends on,
/// and a later change that copies fields instead would break it silently.
#[test]
fn the_plan_hands_the_authored_text_to_an_emitter() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    let column = |name: &str, path: &str| {
        screen(&plan, name)
            .columns
            .iter()
            .find(|field| field.path == path)
            .unwrap_or_else(|| panic!("{name} states the column {path}"))
            .label
            .as_deref()
    };
    assert_eq!(column("get", "code"), Some("Widget code"));
    assert_eq!(column("get", "id"), None);

    let input = |name: &str, path: &str| {
        screen(&plan, name)
            .inputs
            .iter()
            .find(|field| field.path == path)
            .unwrap_or_else(|| panic!("{name} states the input {path}"))
            .label
            .as_deref()
    };
    assert_eq!(input("update", "change.code"), Some("Widget code"));
    assert_eq!(
        input("record_batch", "value.line[].amount"),
        Some("Quantity received")
    );

    assert_eq!(
        screen(&plan, "query").contract.label.as_deref(),
        Some("Find widgets")
    );
    assert_eq!(screen(&plan, "get").contract.label, None);
}

/// EXIT GATE for `wamn-rm14.2`: the plan states which list offers each input
/// that names a record, and it states nothing where no list can.
///
/// An emitter reads this and calls one binding. It never matches a name, and
/// it never reads the manifest.
#[test]
fn the_plan_states_the_list_that_offers_each_referenced_input() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    let populated = |name: &str, path: &str| {
        screen(&plan, name)
            .population
            .iter()
            .find(|input| input.input == path)
            .unwrap_or_else(|| panic!("{name} populates {path}"))
    };

    // An authored reference, served by the second model's query. Its display
    // field is absent, so the plan takes the first text column.
    let maker = populated("record_batch", "value.maker_id");
    assert_eq!(
        maker.list_operation,
        "platform-fixture:widget-maker/query@1.0.0"
    );
    assert_eq!(maker.key_field, "id");
    assert_eq!(maker.display_field, "name", "the default is the first text");
    assert_eq!(maker.narrowed_by, None);
    assert_eq!(
        maker.search_input,
        Some("filter.name[]"),
        "a selector searches by the declared filter on its display field"
    );
    assert_eq!(
        maker.cursor_input,
        Some("cursor"),
        "the list serves pages, so the selector reads the next one"
    );

    // A nested reference inside a repeated group, narrowed by a sibling.
    let line = populated("record_batch", "value.line[].widget_id");
    assert_eq!(line.list_operation, "platform-fixture:widget/list@1.0.0");
    assert_eq!(
        line.display_field, "code",
        "an authored display field wins over the default"
    );
    assert_eq!(
        (line.search_input, line.cursor_input),
        (None, None),
        "the list declares no filter and serves one page, so neither control exists"
    );
    let narrowing = line.narrowed_by.expect("the line list is narrowed");
    assert_eq!(narrowing.input, "value.maker_id");
    assert_eq!(
        narrowing.list_input, "maker_id",
        "the list input is matched by the model it names, never by its name"
    );

    // A derived reference on a generated action.
    let update = populated("update", "change.maker_id");
    assert_eq!(
        update.list_operation,
        "platform-fixture:widget-maker/query@1.0.0"
    );

    // An input that names no record states nothing.
    assert!(
        screen(&plan, "update")
            .population
            .iter()
            .all(|input| input.input != "change.code"),
        "a plain control is absent from the population"
    );
    assert!(
        screen(&plan, "get").population.is_empty(),
        "a record read fills nothing from a list"
    );
}

/// EXIT GATE for `wamn-sxb5.1`: the plan names every selector that cannot
/// search, so a generator reports the gap and an author closes it.
///
/// The report states the screen, the input it fills and the list it reads.
/// A selector whose list declares the filter on its display field is absent.
#[test]
fn selectors_whose_list_declares_no_display_filter_are_reported() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);

    assert_eq!(
        plan.unsearchable(),
        [UnsearchableSelector {
            operation: "platform-fixture:widget/record-batch@1.0.0",
            input: "value.line[].widget_id",
            list_operation: "platform-fixture:widget/list@1.0.0",
        }],
        "the report names the screen, the input and the list"
    );
}

/// EXIT GATE for `wamn-rm14.3`: a table states the form its row opens and the
/// values it hands over, by declared facts alone.
///
/// The pair is the key field of the rows and the input that names that model.
/// A form that names no such record is absent, and the two existing row-link
/// reasons do not move.
#[test]
fn a_table_states_the_form_its_row_opens_and_the_pairs_it_carries() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);

    let maker_rows = screen(&plan, "list");
    assert!(
        maker_rows.row_forms.is_empty()
            || maker_rows
                .row_forms
                .iter()
                .all(|form| form.operation != maker_rows.contract.operation),
        "a screen never opens itself"
    );

    let makers = plan
        .screens()
        .find(|screen| screen.model == "widget_maker" && screen.name == "query")
        .expect("the second model lists");
    // The batch form names widget_maker twice, in value.maker_id and
    // value.inspector_id. No declared path says which one a maker row is, so
    // the row fills neither (wamn-6jcm).
    assert!(
        makers
            .row_forms
            .iter()
            .all(|form| form.operation != "platform-fixture:widget/record-batch@1.0.0"),
        "a row fills no input when two inputs name its model"
    );
    let update = makers
        .row_forms
        .iter()
        .find(|form| form.operation == "platform-fixture:widget/update@1.0.0")
        .expect("a maker row opens the update form");
    assert_eq!(
        update.pairs,
        [("id", "change.maker_id")],
        "the row's key fills the one input that names that model"
    );

    let widgets = screen(&plan, "list");
    let line = widgets
        .row_forms
        .iter()
        .find(|form| form.operation == "platform-fixture:widget/record-batch@1.0.0")
        .expect("a widget row opens the batch form");
    assert_eq!(line.pairs, [("id", "value.line[].widget_id")]);

    assert!(
        screen(&plan, "get").row_forms.is_empty(),
        "a record read has no rows to hand over"
    );
    assert!(
        screen(&plan, "query")
            .row_links
            .iter()
            .any(|link| link.reason == LinkReason::Record),
        "the two existing row-link reasons are unchanged"
    );
}

/// EXIT GATE for the uniformity rule, owner instruction of 2026-09-22: a
/// generated read and an authored read state the same member, and the plan
/// applies one rule to both.
///
/// A reader that branched on the origin would give two screens of the same
/// shape two behaviors, which is the defect this states cannot happen.
/// EXIT GATE for `wamn-zrrg`: a table column that names a record states the
/// read that shows the record's text, and the plan reports a column that no
/// served read can show.
///
/// The text is the display field of the model's served list, so a cell shows
/// what a selector offers for the same record.
#[test]
fn a_table_column_that_names_a_record_states_the_read_that_shows_it() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    let widgets = plan
        .screens()
        .find(|screen| screen.model == "widget" && screen.name == "query")
        .expect("the widget query");
    assert_eq!(
        widgets.resolved_columns,
        [ResolvedColumn {
            column: "maker_id",
            read_operation: "platform-fixture:widget-maker/get@1.0.0",
            read_model: "widget_maker",
            read_name: "get",
            key_input: "id",
            display_field: "name",
        }],
        "the maker column reads the maker's name through its get"
    );
    assert!(
        screen(&plan, "get").resolved_columns.is_empty(),
        "only a table resolves its columns"
    );
    assert!(plan.unresolved().is_empty(), "every named record resolves");

    // With no served get, the list's rows open no record read, so the
    // column shows the key and the plan names it.
    let mut unserved = release();
    let maker = unserved
        .models
        .iter_mut()
        .find(|model| model.name == "widget_maker")
        .expect("the second model");
    maker
        .operations
        .iter_mut()
        .find(|operation| operation.name == "get")
        .expect("the maker get")
        .route = None;
    let plan = ClientPlan::from_ir(&unserved);
    assert_eq!(
        plan.unresolved(),
        [UnresolvedColumn {
            operation: "platform-fixture:widget/query@1.0.0",
            column: "maker_id",
            model: "widget_maker",
        }]
    );
}

/// A selector states the read that loads a held record its list did not
/// return, by the rule a table column uses (wamn-1jrv).
#[test]
fn a_selector_states_the_read_that_loads_a_record_off_its_list() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    let read = |name: &str, path: &str| {
        screen(&plan, name)
            .population
            .iter()
            .find(|input| input.input == path)
            .unwrap_or_else(|| panic!("{name} populates {path}"))
            .read
    };
    assert_eq!(
        read("update", "change.maker_id"),
        Some(RecordRead {
            operation: "platform-fixture:widget-maker/get@1.0.0",
            model: "widget_maker",
            name: "get",
            key_input: "id",
        }),
        "the maker selector loads a maker through the get its list's rows open"
    );

    // With no served get, the list's rows open no record read, so the
    // selector shows only the rows its list returns.
    let mut unserved = release();
    unserved
        .models
        .iter_mut()
        .find(|model| model.name == "widget_maker")
        .expect("the second model")
        .operations
        .iter_mut()
        .find(|operation| operation.name == "get")
        .expect("the maker get")
        .route = None;
    let plan = ClientPlan::from_ir(&unserved);
    assert_eq!(
        screen(&plan, "update")
            .population
            .iter()
            .find(|input| input.input == "change.maker_id")
            .expect("the maker selector")
            .read,
        None
    );
}

#[test]
fn a_generated_read_and_an_authored_read_populate_by_the_same_rule() {
    let ir = release();
    let plan = ClientPlan::from_ir(&ir);
    for name in ["query", "list"] {
        let lists = screen(&plan, name)
            .contract
            .lists
            .as_ref()
            .unwrap_or_else(|| panic!("{name} states what it lists"));
        assert_eq!(lists.model.as_deref(), Some("widget"), "{name}");
        assert_eq!(lists.key_field, ["id"], "{name}");
    }
    // The generated query authors no display field, so the plan defaults it,
    // and the authored list states one. Both reach an emitter the same way.
    let update = screen(&plan, "update")
        .population
        .iter()
        .find(|input| input.input == "change.maker_id")
        .expect("the derived reference is populated");
    assert_eq!(update.display_field, "name");
    let batch = screen(&plan, "record_batch")
        .population
        .iter()
        .find(|input| input.input == "value.line[].widget_id")
        .expect("the authored reference is populated");
    assert_eq!(batch.display_field, "code");
}

#[test]
fn the_result_fields_of_an_operation_have_one_owner() {
    // client_rust.rs held the same rule and could drift from the columns.
    // wamn-7kmr.
    let mut ir = release();
    let served = operation(&mut ir, "get");
    let route_fields = served
        .route
        .as_ref()
        .expect("the release serves get")
        .response
        .fields
        .clone();
    assert_eq!(
        effective_result_fields(served)
            .iter()
            .map(|field| field.path.as_str())
            .collect::<Vec<_>>(),
        route_fields
            .iter()
            .map(|field| field.path.as_str())
            .collect::<Vec<_>>(),
        "a served route states its own result fields"
    );

    let unserved = operation(&mut ir, "get");
    unserved.route = None;
    assert_eq!(
        effective_result_fields(unserved)
            .iter()
            .map(|field| field.path.as_str())
            .collect::<Vec<_>>(),
        unserved
            .result_fields
            .iter()
            .map(|field| field.path.as_str())
            .collect::<Vec<_>>(),
        "an operation with no route keeps the fields it declared"
    );
}

/// A revision that names an input is supplied by the row the operator
/// chooses for that input, and by no other input (wamn-nv87).
///
/// The batch names `widget_maker` twice. Without the declaration no path
/// says whose revision it sends, so the page supplies it.
#[test]
fn a_revision_names_the_one_selector_whose_row_supplies_it() {
    let ir = fixture::client_release();
    let plan = ClientPlan::from_ir(&ir);
    let batch = screen(&plan, "record_batch");
    let revision = |input: &str| {
        batch
            .population
            .iter()
            .find(|populated| populated.input == input)
            .expect("the input is chosen from a list")
            .revision
    };
    assert_eq!(
        revision("value.inspector_id"),
        Some(ChosenRevision {
            input: "value.expected_edit_version",
            field: "edit_version",
        }),
        "the inspector's row supplies the revision declared for it"
    );
    assert_eq!(
        revision("value.maker_id"),
        None,
        "the other input of the same model supplies nothing"
    );
    assert_eq!(
        batch.revision_inputs,
        ["value.expected_edit_version"],
        "the revision stays reserved, so the operator never types it"
    );

    let mut undeclared = fixture::manifest();
    for field in undeclared["custom_operations"]["widget.record_batch"]["input"]["fields"]
        .as_array_mut()
        .expect("the batch declares its input")
    {
        let field = field.as_object_mut().expect("a field object");
        if field
            .get("revision")
            .is_some_and(serde_json::Value::is_string)
        {
            field.insert("revision".to_owned(), json!(true));
        }
    }
    let undeclared =
        fixture::client_release_of(&fixture::generate_with(&fixture::catalog(), &undeclared));
    let plan = ClientPlan::from_ir(&undeclared);
    assert!(
        screen(&plan, "record_batch")
            .population
            .iter()
            .all(|populated| populated.revision.is_none()),
        "no input supplies a revision that names no record"
    );

    // Only a revision names a record, and the input it names states a model.
    let mut refused = fixture::manifest();
    refused["custom_operations"]["widget.record_batch"]["input"]["fields"]
        .as_array_mut()
        .expect("the batch declares its input")
        .iter_mut()
        .find(|field| field["path"] == "value.expected_edit_version")
        .expect("the batch takes a revision")["revision"] = json!("value.note");
    let error = fixture::try_generate_with(&fixture::catalog(), &refused)
        .expect_err("a revision of an input that names no model is refused");
    assert!(
        error.to_string().contains("is the revision of value.note"),
        "{error}"
    );
}
