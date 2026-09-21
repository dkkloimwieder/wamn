use serde_json::json;
use wamn_schema_generator::client_ir::{ClientContractIr, FieldIr, OperationIr};
use wamn_schema_generator::client_plan::{ClientPlan, ScreenPlan, SuppliedKind};

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
        ["archive", "create", "delete", "get", "query", "update"],
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
    for name in ["archive", "create", "get", "query", "update"] {
        assert!(screen(&plan, name).revision.is_none(), "{name}");
    }
}
