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
        .filter(|model| model.name == "widget")
        .flat_map(|model| &mut model.operations)
        .find(|op| op.name == "get")
        .unwrap();
    let response = &mut get.route.as_mut().unwrap().response.fields;
    for (field, ty) in response.iter_mut().zip(["boolean", "timestamptz", "int32"]) {
        field.type_name = ty.to_owned();
        field.visible = Some(true);
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
fn populated_references_use_custom_controls_in_scalar_and_repeated_fields() {
    let files = emit(&fixture::client_release());
    let form = description(&files, "widget", "WidgetRecordBatchForm");
    let fields = form["fields"].as_array().unwrap();
    assert_eq!(
        fields
            .iter()
            .find(|field| field["field"] == "value.maker_id")
            .unwrap()["control"],
        "custom"
    );
    let group = fields
        .iter()
        .find(|field| field["field"] == "value.line[]")
        .unwrap();
    assert_eq!(
        group["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["field"] == "widget_id")
            .unwrap()["control"],
        "custom"
    );
}

#[test]
fn descriptions_carry_reference_display_fields_and_label_navigation() {
    let mut ir = fixture::client_release();
    // A supplier-like model can offer a list without a single-record read.
    let maker = ir
        .models
        .iter_mut()
        .find(|model| model.name == "widget_maker")
        .unwrap();
    maker
        .operations
        .iter_mut()
        .find(|op| op.name == "get")
        .unwrap()
        .route = None;
    let files = emit(&ir);
    let table = description(&files, "widget", "WidgetQueryTable");
    assert_eq!(table["presentation"]["displayField"], "code");
    for name in ["WidgetQueryTable", "WidgetGetDetail"] {
        let value = description(&files, "widget", name);
        let reference = &value["presentation"]["references"]["maker_id"];
        assert_eq!(
            reference["operation"],
            "platform-fixture:widget-maker/query@2.1.0"
        );
        assert_eq!(reference["keyField"], "id");
        assert_eq!(reference["displayField"], "name");
        assert_eq!(reference["cursorInput"], "cursor");
        assert!(reference.get("read").is_none());
    }
    let form = description(&files, "widget", "WidgetRecordBatchForm");
    assert_eq!(
        form["presentation"]["references"]["value.maker_id"]["displayField"],
        "name"
    );
    assert_eq!(
        form["presentation"]["references"]["value.line[].widget_id"]["displayField"],
        "code"
    );
    assert!(
        form["presentation"]["references"]["value.line[].widget_id"]
            .get("searchField")
            .is_none()
    );
}

#[test]
fn optional_and_nullable_controls_obey_wfe_scalar_types() {
    let mut ir = fixture::client_release();
    let op = ir
        .models
        .iter_mut()
        .filter(|model| model.name == "widget")
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
        .filter(|model| model.name == "widget")
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
        .filter(|model| model.name == "widget")
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
        .filter(|model| model.name == "widget")
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

#[test]
fn visibility_survives_contracts_and_selects_tables_and_details() {
    let mut manifest = fixture::manifest();
    manifest["models"]["widget"]["field_text"]["maker_id"] =
        json!({"label": "Maker", "visible": true});
    manifest["models"]["widget"]["field_text"]["code"]["visible"] = json!(false);
    manifest["custom_operations"]["widget.list"]["result"]["fields"][3]["visible"] = json!(true);
    let generated = fixture::generate_with(&fixture::catalog(), &manifest);
    let ir = fixture::client_release_of(&generated);
    let widget = ir
        .models
        .iter()
        .find(|model| model.name == "widget")
        .unwrap();
    assert_eq!(
        widget
            .fields
            .iter()
            .find(|field| field.path == "maker_id")
            .unwrap()
            .visible,
        Some(true)
    );
    let files = emit(&ir);
    let custom = description(&files, "widget", "WidgetListTable");
    assert_eq!(
        custom["columns"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["attributes"],
    );
    let table = description(&files, "widget", "WidgetQueryTable");
    assert_eq!(table["columns"], json!({"maker_id": {"label": "Maker"}}));
    let detail = description(&files, "widget", "WidgetGetDetail");
    assert_eq!(
        detail["fields"],
        json!([{"field": "maker_id", "label": "Maker"}])
    );
    // The selected reference retains its human display metadata.
    assert_eq!(
        table["presentation"]["references"]["maker_id"]["displayField"],
        "name"
    );
}

#[test]
fn unmarked_screens_hide_keys_ids_audit_fields_and_timestamps() {
    let mut ir = fixture::client_release();
    let widget = ir
        .models
        .iter_mut()
        .find(|model| model.name == "widget")
        .unwrap();
    for op in &mut widget.operations {
        if !matches!(op.name.as_str(), "query" | "get") {
            continue;
        }
        let fields = &mut op.route.as_mut().unwrap().response.fields;
        let template = fields[0].clone();
        *fields = [
            ("id", "text"),
            ("maker_id", "int32"),
            ("opaque_identity", "uuid"),
            ("created_by", "text"),
            ("updated_by", "text"),
            ("created_at", "text"),
            ("updated_at", "text"),
            ("occurred_at", "timestamptz"),
            ("code", "text"),
            ("edit_version", "int32"),
            ("hidden", "text"),
        ]
        .into_iter()
        .map(|(path, ty)| FieldIr {
            path: path.into(),
            type_name: ty.into(),
            children: Vec::new(),
            visible: (path == "hidden").then_some(false),
            ..template.clone()
        })
        .collect();
        if let Some(lists) = &mut op.lists {
            lists.key_field = vec!["code".into()];
        }
    }
    let files = emit(&ir);
    let table = description(&files, "widget", "WidgetQueryTable");
    assert_eq!(
        table["columns"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["edit_version"]
    );
    let detail = description(&files, "widget", "WidgetGetDetail");
    assert_eq!(
        detail["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["field"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["code", "edit_version"]
    );
}

#[test]
fn explicit_visibility_can_show_keys_and_timestamps() {
    let mut ir = fixture::client_release();
    let op = ir
        .models
        .iter_mut()
        .find(|model| model.name == "widget")
        .unwrap()
        .operations
        .iter_mut()
        .find(|op| op.name == "query")
        .unwrap();
    for field in &mut op.route.as_mut().unwrap().response.fields {
        field.visible = Some(matches!(field.path.as_str(), "id" | "created_at"));
    }
    let table = description(&emit(&ir), "widget", "WidgetQueryTable");
    assert_eq!(
        table["columns"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["created_at", "id"]
    );
}

#[test]
fn tables_carry_only_the_page_inputs_their_read_declares() {
    let mut ir = fixture::client_release();
    // A history read names its position `after_cursor` and declares its own
    // page size.
    let list = ir
        .models
        .iter_mut()
        .find(|model| model.name == "widget")
        .unwrap()
        .operations
        .iter_mut()
        .find(|op| op.name == "list")
        .unwrap();
    let template = list.result_fields[0].clone();
    list.input_fields = [("after_cursor", "text"), ("limit", "int32")]
        .into_iter()
        .map(|(path, ty)| FieldIr {
            path: path.into(),
            type_name: ty.into(),
            children: Vec::new(),
            ..template.clone()
        })
        .collect();
    let files = emit(&ir);
    let page = |module: &str, name: &str| {
        description(&files, module, name)["presentation"]["page"].clone()
    };
    // A read with a change feed returns a page, but it declares no page
    // control, so a table sends none (wamn-fu8cj).
    assert_eq!(
        page("feed_line", "FeedLineScreenTable"),
        json!({"limit": false, "cursor": null, "filters": [], "sort": []})
    );
    assert_eq!(
        page("widget", "WidgetQueryTable"),
        json!({
            "limit": true,
            "cursor": "cursor",
            "filters": ["code", "maker_id", "note"],
            "sort": ["created_at"],
        })
    );
    assert_eq!(
        page("widget", "WidgetListTable"),
        json!({"limit": true, "cursor": "after_cursor", "filters": [], "sort": []})
    );
}
