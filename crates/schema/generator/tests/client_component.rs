use serde_json::{Value, json};
use wamn_schema_generator::GeneratedFile;
use wamn_schema_generator::client_component::emit_ts_components;
use wamn_schema_generator::client_ir::{ClientContractIr, FieldIr};
use wamn_schema_generator::client_plan::{ClientPlan, Role};

#[path = "support/platform_fixture.rs"]
mod fixture;
#[path = "support/client_component_check.rs"]
mod typescript;

fn emit(ir: &ClientContractIr) -> Vec<GeneratedFile> {
    emit_ts_components(&ClientPlan::from_ir(ir)).expect("the fixture emits descriptions")
}

fn source<'a>(files: &'a [GeneratedFile], name: &str) -> &'a str {
    let path = format!("generated/client-ts/components/{name}.ts");
    std::str::from_utf8(
        files
            .iter()
            .find(|file| file.path() == path)
            .unwrap()
            .bytes(),
    )
    .unwrap()
}

fn description(files: &[GeneratedFile], module: &str, name: &str) -> Value {
    let source = source(files, module);
    let body = source
        .split(&format!("export const {name} = "))
        .nth(1)
        .unwrap();
    serde_json::from_str(body.split(" as const satisfies ").next().unwrap()).unwrap()
}

#[test]
fn every_served_role_emits_a_typed_description_without_components() {
    let ir = fixture::client_release();
    let plan = ClientPlan::from_ir(&ir);
    let files = emit(&ir);
    for model in &plan.models {
        for screen in &model.screens {
            if screen.contract.route.is_none() || !screen.role.is_supported() {
                continue;
            }
            let role = match screen.role {
                Role::Table => "Table",
                Role::Detail => "Detail",
                Role::Form => "Form",
                Role::Delete => "Delete",
                Role::Unsupported(_) => unreachable!(),
            };
            let stem = wamn_schema_generator::client_ts::type_stem(screen.model, screen.name);
            let value = description(&files, &model.model.name, &format!("{stem}{role}"));
            assert_eq!(value["id"], screen.contract.operation);
            assert_eq!(value["title"], screen.contract.label.as_deref().unwrap());
            assert!(
                source(&files, &model.model.name)
                    .contains(&format!("as const satisfies {role}Description"))
            );
        }
    }
    for file in &files {
        assert_eq!(std::path::Path::new(file.path()).extension().unwrap(), "ts");
        let source = std::str::from_utf8(file.bytes()).unwrap();
        for retired in [
            "solid-js",
            "@wamn/ui",
            "export function",
            "createSignal",
            "<form",
            "Transport",
        ] {
            assert!(
                !source.contains(retired),
                "{} contains {retired}",
                file.path()
            );
        }
    }
}

#[test]
fn tables_and_details_keep_paths_authored_labels_and_matching_formats() {
    let files = emit(&fixture::client_release());
    let table = description(&files, "widget", "WidgetQueryTable");
    assert_eq!(table["title"], "Find widgets");
    assert_eq!(table["columns"]["code"]["label"], "Widget code");
    assert_eq!(table["columns"]["edit_version"]["label"], "edit_version");
    assert_eq!(table["columns"]["edit_version"]["format"], "number-plain");
    let detail = description(&files, "widget", "WidgetGetDetail");
    let fields = detail["fields"].as_array().unwrap();
    assert!(
        fields
            .iter()
            .any(|field| field["field"] == "code" && field["label"] == "Widget code")
    );
    let mut ir = fixture::client_release();
    let get = ir
        .models
        .iter_mut()
        .flat_map(|model| &mut model.operations)
        .find(|op| op.name == "get")
        .unwrap();
    let response = &mut get.route.as_mut().unwrap().response.fields;
    for (field, ty) in response.iter_mut().zip(["boolean", "timestamptz", "int32"]) {
        field.type_name = ty.to_owned();
    }
    let files = emit(&ir);
    let detail = description(&files, "widget", "WidgetGetDetail");
    let formats: Vec<_> = detail["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|field| field["format"].as_str())
        .collect();
    for format in ["boolean-symbol", "datetime", "number-plain"] {
        assert!(formats.contains(&format));
    }
}

#[test]
fn forms_keep_repeated_groups_domains_and_reserved_input_exclusion() {
    let files = emit(&fixture::client_release());
    let form = description(&files, "widget", "WidgetRecordBatchForm");
    let fields = form["fields"].as_array().unwrap();
    let group = fields
        .iter()
        .find(|field| field["control"] == "repeat")
        .unwrap();
    assert_eq!(group["field"], "value.line[]");
    assert_eq!(group["label"], "Batch lines");
    let rows = group["fields"].as_array().unwrap();
    let amount = rows
        .iter()
        .find(|field| field["field"] == "amount")
        .unwrap();
    assert_eq!(amount["label"], "Quantity received");
    assert_eq!(amount["control"], "text");
    assert!(
        fields
            .iter()
            .any(|field| field["field"] == "value.grade" && field["control"] == "select")
    );
    for field in fields {
        assert!(
            ![
                "request_id",
                "value.idempotency_key",
                "value.expected_edit_version"
            ]
            .contains(&field["field"].as_str().unwrap())
        );
    }
    assert_eq!(
        form["submit"],
        json!({"id":"platform-fixture:widget/record-batch@2.1.0", "label":"Record a batch"})
    );
    assert!(source(&files, "widget").contains("\"value.line[]\": {"));
}

#[test]
fn optional_and_nullable_controls_obey_wfe_scalar_types() {
    let mut ir = fixture::client_release();
    let op = ir
        .models
        .iter_mut()
        .flat_map(|model| &mut model.operations)
        .find(|op| op.name == "create")
        .unwrap();
    let mut field = op.input_fields[0].clone();
    field.path = "number".into();
    field.children.clear();
    field.values.clear();
    field.type_name = "int32".into();
    field.required = false;
    field.nullable = true;
    op.input_fields = vec![field.clone()];
    let files = emit(&ir);
    let form = description(&files, "widget", "WidgetCreateForm");
    assert_eq!(form["fields"][0]["control"], "number");
    assert_eq!(form["fields"][0]["empty"], "undefined");
    field.type_name = "text".into();
    ir.models
        .iter_mut()
        .flat_map(|model| &mut model.operations)
        .find(|op| op.name == "create")
        .unwrap()
        .input_fields = vec![field];
    let files = emit(&ir);
    assert_eq!(
        description(&files, "widget", "WidgetCreateForm")["fields"][0]["control"],
        "custom"
    );
}

#[test]
fn delete_has_confirmation_and_uses_the_operation_label() {
    let mut ir = fixture::client_release();
    let op = ir
        .models
        .iter_mut()
        .flat_map(|model| &mut model.operations)
        .find(|op| op.name == "delete")
        .unwrap();
    op.label = Some("Remove widget".into());
    let files = emit(&ir);
    assert_eq!(
        description(&files, "widget", "WidgetDeleteDelete"),
        json!({
            "id":"platform-fixture:widget/delete@2.1.0", "title":"Remove widget", "confirmation":"Confirm Remove widget?"
        })
    );
}

#[test]
fn unserved_operations_emit_no_description_and_generation_is_stable() {
    let mut ir = fixture::client_release();
    let op = ir
        .models
        .iter_mut()
        .flat_map(|model| &mut model.operations)
        .find(|op| op.name == "create")
        .unwrap();
    op.route = None;
    let first = emit(&ir);
    let second = emit(&ir);
    assert!(!source(&first, "widget").contains("export const WidgetCreateForm"));
    assert!(
        source(&first, "index").contains("platform-fixture:widget/create@2.1.0: no HTTP route")
    );
    assert_eq!(
        first
            .iter()
            .map(|file| (file.path(), file.bytes()))
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|file| (file.path(), file.bytes()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn every_ir_field_has_an_authored_label_or_its_exact_path() {
    fn visit(fields: &[FieldIr]) {
        for field in fields {
            assert!(field.label.is_some(), "{} has no label", field.path);
            visit(&field.children);
        }
    }
    let ir = fixture::client_release();
    for model in &ir.models {
        visit(&model.fields);
        for op in &model.operations {
            assert!(op.label.is_some());
            visit(&op.input_fields);
            visit(&op.result_fields);
            if let Some(route) = &op.route {
                visit(&route.response.fields);
            }
        }
    }
}

#[test]
fn omitted_labels_default_after_projection_for_every_operation_and_field() {
    fn remove_labels(value: &mut Value) {
        match value {
            Value::Object(members) => {
                members.remove("label");
                for value in members.values_mut() {
                    remove_labels(value);
                }
            }
            Value::Array(values) => {
                for value in values {
                    remove_labels(value);
                }
            }
            _ => {}
        }
    }
    fn defaults(fields: &[FieldIr]) {
        for field in fields {
            assert_eq!(field.label.as_deref(), Some(field.path.as_str()));
            defaults(&field.children);
        }
    }
    let mut contracts = fixture::contracts(&fixture::generate_fixture());
    for bytes in contracts.values_mut() {
        let mut value: Value = serde_json::from_slice(bytes).unwrap();
        remove_labels(&mut value);
        *bytes = serde_json::to_vec(&value).unwrap();
    }
    let ir = ClientContractIr::from_release_contracts(
        "platform_fixture",
        &contracts,
        &std::collections::BTreeMap::default(),
    )
    .unwrap();
    for model in &ir.models {
        defaults(&model.fields);
        for op in &model.operations {
            assert_eq!(op.label.as_deref(), Some(op.operation.as_str()));
            defaults(&op.input_fields);
            defaults(&op.result_fields);
        }
    }
}

#[test]
fn declared_group_labels_reach_published_response_groups() {
    fn clear(fields: &mut [FieldIr]) {
        for field in fields {
            field.label = None;
            clear(&mut field.children);
        }
    }
    let ir = fixture::client_release();
    let op = ir
        .models
        .iter()
        .flat_map(|model| &model.operations)
        .find(|op| op.name == "record_batch")
        .unwrap();
    let mut served = op.input_fields.clone();
    clear(&mut served);
    FieldIr::carry_declared(&mut served, &op.input_fields);
    assert_eq!(served, op.input_fields);
}

#[test]
fn emitted_descriptions_type_check_against_wfe() {
    typescript::check_descriptions(&fixture::client_release())
        .expect("the emitted descriptions satisfy the pinned wfe types");
}
