use wamn_compiler::client_ir::ClientContractIr;
use wamn_compiler::client_ts::{ClientTsErrorType, emit_ts_client, to_camel, to_snake, ts_type};

#[path = "support/platform_fixture.rs"]
mod fixture;
#[path = "support/client_ts_compile.rs"]
mod typescript;

fn release() -> ClientContractIr {
    fixture::client_release()
}

#[test]
#[ignore = "requires: input:fixture, input:web-install, bin:pnpm"]
fn emitted_bindings_preserve_transport_mapping_and_feed_behavior() {
    let original = release();
    let mut hidden = original.clone();
    let index = fixture::widget_index(&hidden);
    let mut handler = hidden.models[index].operations[0].clone();
    handler.name = "widget_archived".into();
    handler.type_ = "event_handler".into();
    handler.operation = "platform-fixture:widget/widget-archived@2.1.1"
        .parse()
        .unwrap();
    hidden.models[index].operations.push(handler);
    hidden.models[index]
        .operations
        .iter_mut()
        .find(|op| op.name == "get")
        .unwrap()
        .route = None;
    let mut manifest = fixture::manifest();
    manifest["models"]["widget"]["operations"]["query"]["feed"] = false.into();
    let no_feed =
        fixture::client_release_of(&fixture::generate_with(&fixture::catalog(), &manifest));
    typescript::behavior(&[
        ("fixture", original),
        ("hidden", hidden),
        ("no_feed", no_feed),
    ])
    .expect("the emitted client preserves its runtime behavior");
}

#[test]
#[ignore = "requires: input:fixture"]
fn invalid_client_names_refuse_before_emission() {
    {
        let mut ir = release();
        let widget_index = fixture::widget_index(&ir);
        let template = ir.models[widget_index].operations[0].clone();
        // `delete` is a reserved word and gains one underscore, so a sibling that
        // already spells the escape takes the same name.
        for (index, name) in ["delete_", "delete"].into_iter().enumerate() {
            let mut operation = template.clone();
            operation.name = name.to_owned();
            operation.operation = format!("platform-fixture:widget/collide-{index}@1.0.0")
                .parse()
                .unwrap();
            ir.models[widget_index].operations.push(operation);
        }
        let refusal = emit_ts_client(&ir).expect_err("a duplicate TypeScript name refuses");
        assert_eq!(refusal.error_type(), ClientTsErrorType::NameCollision);

        let mut unnameable = release();
        let widget_index = fixture::widget_index(&unnameable);
        unnameable.models[widget_index].operations[0].name = "9lives".to_owned();
        let refusal = emit_ts_client(&unnameable).expect_err("a name with no spelling refuses");
        assert_eq!(
            refusal.error_type(),
            ClientTsErrorType::UnnameableIdentifier
        );
    }

    {
        let mut operation_name = release();
        let widget_index = fixture::widget_index(&operation_name);
        operation_name.models[widget_index].operations[0].name = "listWidgets".to_owned();
        let refusal = emit_ts_client(&operation_name).expect_err("a capital does not reverse");
        assert_eq!(refusal.error_type(), ClientTsErrorType::IrreversibleName);

        let mut field_name = release();
        let widget_index = fixture::widget_index(&field_name);
        let archive = field_name.models[widget_index]
            .operations
            .iter_mut()
            .find(|operation| operation.name == "archive")
            .expect("the fixture declares widget.archive");
        archive.input_fields[0].path = "editVersion".to_owned();
        let refusal =
            emit_ts_client(&field_name).expect_err("a field that does not reverse refuses");
        assert_eq!(refusal.error_type(), ClientTsErrorType::IrreversibleName);

        emit_ts_client(&release()).expect("every fixture name reverses");
    }

    {
        let ir = release();
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
                    wamn_compiler::client_ir::leaf_fields(&fields.cloned().collect::<Vec<_>>())
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
                    }
                }
            }
        }
    }

    {
        let catalog = fixture::catalog();
        let mut value = fixture::manifest();
        value["client_package"] = serde_json::json!({ "name": "@wamn/platform-fixture-client" });
        fixture::generate_with(&catalog, &value);
    }
}

#[test]
#[ignore = "requires: input:fixture, input:web-install, bin:pnpm"]
fn emitted_client_compiles_with_workspace_tsc() {
    typescript::compile_types(&release()).expect("the emitted client satisfies strict TypeScript");
}
