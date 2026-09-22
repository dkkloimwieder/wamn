use serde_json::json;
use wamn_schema_generator::client_ir::{ClientContractIr, FieldIr, OperationIr};
use wamn_schema_generator::client_plan::{
    ClientPlan, LinkReason, NoRole, Role, RowLink, Rows, ScreenPlan, SuppliedKind,
};

#[path = "support/platform_fixture.rs"]
mod fixture;

fn release() -> ClientContractIr {
    fixture::client_release()
}

fn operation<'a>(ir: &'a mut ClientContractIr, name: &str) -> &'a mut OperationIr {
    ir.models[0]
        .operations
        .iter_mut()
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
    ir.models[0].operations.push(handler);
    ir.models[0].operations.reverse();

    let plan = ClientPlan::from_ir(&ir);
    assert_eq!(plan.package, "platform_fixture");
    assert_eq!(plan.models.len(), 1, "a private operation keeps its model");
    assert_eq!(plan.models[0].model.name, "widget");
    assert_eq!(
        plan.models[0]
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
    assert!(plan.screens().all(|screen| screen.model == "widget"));
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
        field.revision = false;
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
        6,
        "the other screens keep their role"
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
            ["code", "created_at", "edit_version", "id", "note"],
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
    assert_eq!(paths(&screen(&plan, "create").inputs), ["code", "note"]);
    assert!(
        screen(&plan, "query").inputs.is_empty(),
        "every input of the fixture query is a page control or a supplied value"
    );
    assert_eq!(
        paths(&screen(&plan, "update").inputs),
        ["change.code", "change.note", "id"],
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
        revision: false,
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
        input("record_batch", "value.line[].quantity"),
        Some("Quantity received")
    );

    assert_eq!(
        screen(&plan, "query").contract.label.as_deref(),
        Some("Find widgets")
    );
    assert_eq!(screen(&plan, "get").contract.label, None);
}
