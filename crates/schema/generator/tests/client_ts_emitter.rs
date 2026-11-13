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
fn emitted_bindings_preserve_transport_mapping_and_feed_behavior() {
    let original = release();
    let mut hidden = original.clone();
    let index = fixture::widget_index(&hidden);
    let mut handler = hidden.models[index].operations[0].clone();
    handler.name = "widget_archived".into();
    handler.type_ = "event_handler".into();
    handler.operation = "platform-fixture:widget/widget-archived@2.1.1".into();
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
            operation.operation = format!("platform-fixture:widget/collide-{index}@1.0.0");
            ir.models[widget_index].operations.push(operation);
        }
        let refusal = emit_ts_client(&ir).expect_err("a duplicate TypeScript name refuses");
        assert_eq!(refusal.error_type(), ClientTsErrorType::NameCollision);
        assert_eq!(
            refusal.to_string(),
            "name_collision: operation \"delete\" takes the TypeScript name \"delete_\", which another operation took"
        );

        let mut unnameable = release();
        let widget_index = fixture::widget_index(&unnameable);
        unnameable.models[widget_index].operations[0].name = "9lives".to_owned();
        let refusal = emit_ts_client(&unnameable).expect_err("a name with no spelling refuses");
        assert_eq!(
            refusal.error_type(),
            ClientTsErrorType::UnnameableIdentifier
        );
        assert_eq!(
            refusal.to_string(),
            "unnameable_identifier: \"9lives\" is not a TypeScript identifier"
        );
    }

    {
        let mut operation_name = release();
        let widget_index = fixture::widget_index(&operation_name);
        let operation = operation_name.models[widget_index].operations[0]
            .operation
            .clone();
        operation_name.models[widget_index].operations[0].name = "listWidgets".to_owned();
        let refusal = emit_ts_client(&operation_name).expect_err("a capital does not reverse");
        assert_eq!(refusal.error_type(), ClientTsErrorType::IrreversibleName);
        assert_eq!(
            refusal.to_string(),
            format!(
                "irreversible_name: {operation} takes the member \"listWidgets\", which maps back to \"list_widgets\""
            )
        );

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
        assert_eq!(
            refusal.to_string(),
            "irreversible_name: platform-fixture:widget/archive@2.1.1 field \"editVersion\" takes the member \"editVersion\", which maps back to \"edit_version\""
        );

        emit_ts_client(&release()).expect("every fixture name reverses");
    }

    {
        let ir = release();
        let mut observed = 0;
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
                        observed += 1;
                    }
                }
            }
        }
        assert!(
            observed > 20,
            "the fixture declares enough names: {observed}"
        );
    }

    {
        use wamn_compiler::ManifestErrorType as GenerateErrorType;

        let catalog = fixture::catalog();
        for (name, reason) in [
            ("", "the name must not be empty"),
            (
                "Widgets",
                "each part must start with a lowercase letter or a digit",
            ),
            (
                "@scope",
                "a scoped name needs a scope and a package, as @scope/name",
            ),
            (
                "-leading",
                "each part must start with a lowercase letter or a digit",
            ),
            (
                "has space",
                "each part admits only lowercase letters, digits, and - . _",
            ),
        ] {
            let mut value = fixture::manifest();
            value["client_package"] = serde_json::json!({ "name": name });
            let refusal = fixture::try_generate_with(&catalog, &value)
                .expect_err("an invalid client package name refuses");
            assert_eq!(
                refusal.error_type(),
                GenerateErrorType::InvalidClientPackage,
                "{name:?}"
            );
            assert_eq!(
                refusal.context(),
                format!("platform_fixture declares client package name {name:?}: {reason}")
            );
        }

        let mut value = fixture::manifest();
        value["client_package"] = serde_json::json!({ "name": "@wamn/platform-fixture-client" });
        fixture::generate_with(&catalog, &value);
    }
}

#[test]
fn emitted_client_compiles_with_workspace_tsc() {
    typescript::compile_types(&release()).expect("the emitted client satisfies strict TypeScript");
}
