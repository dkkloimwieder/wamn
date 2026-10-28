use wamn_schema_generator::GeneratedFile;
use wamn_schema_generator::client_ir::ClientContractIr;
use wamn_schema_generator::client_ts::{
    ClientTsErrorType, emit_ts_client, to_camel, to_snake, ts_type,
};

#[path = "support/platform_fixture.rs"]
mod fixture;
#[path = "support/client_ts_check.rs"]
mod typescript;

fn release() -> ClientContractIr {
    fixture::client_release()
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
    source(files, "generated/client-ts/widget.ts")
}

#[test]
fn emitted_bindings_preserve_transport_mapping_and_feed_behavior() {
    let original = release();
    let mut hidden = original.clone();
    let index = fixture::widget_index(&hidden);
    let mut handler = hidden.models[index].operations[0].clone();
    handler.name = "widget_archived".into();
    handler.type_ = "event_handler".into();
    handler.operation = "platform-fixture:widget/widget-archived@2.1.0".into();
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
            "irreversible_name: platform-fixture:widget/archive@2.1.0 field \"editVersion\" takes the member \"editVersion\", which maps back to \"edit_version\""
        );

        emit_ts_client(&release()).expect("every fixture name reverses");
    }

    {
        let ir = release();
        let mut checked = 0;
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
                for field in wamn_schema_generator::client_ir::leaf_fields(
                    &fields.cloned().collect::<Vec<_>>(),
                ) {
                    for part in field.path.split('.') {
                        let name = part.trim_end_matches("[]");
                        assert_eq!(to_snake(&to_camel(name)), name, "{}", field.path);
                        assert!(
                            ts_type(&field.type_name).is_ok() || field.type_name == "object",
                            "{} has type {}",
                            field.path,
                            field.type_name
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 20, "the fixture declares enough names: {checked}");
    }

    {
        use wamn_schema_generator::GenerateErrorType;

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
fn generation_is_stable_and_row_shape_changes_its_digest() {
    {
        let mut ir = release();
        let widget_index = fixture::widget_index(&ir);
        let first = emit_ts_client(&ir).unwrap();
        assert_eq!(first, emit_ts_client(&ir).unwrap());
        ir.models[widget_index].operations.reverse();
        assert_eq!(
            first,
            emit_ts_client(&ir).unwrap(),
            "the contract's incidental order does not reach the bytes"
        );
    }

    {
        fn digest(ir: &ClientContractIr) -> String {
            widget(&emit_ts_client(ir).unwrap())
                .lines()
                .find(|line| line.starts_with("export const WIDGET_SHAPE_DIGEST ="))
                .unwrap()
                .to_owned()
        }
        let original = release();
        let baseline = digest(&original);
        let index = fixture::widget_index(&original);
        let mut reordered = original.clone();
        reordered.models[index].operations.reverse();
        for operation in &mut reordered.models[index].operations {
            operation.result_fields.reverse();
            if let Some(route) = &mut operation.route {
                route.response.fields.reverse();
                for field in &mut route.response.fields {
                    field.label = Some("Another label".into());
                }
            }
        }
        assert_eq!(baseline, digest(&reordered));
        for change in 0..3 {
            let mut changed = original.clone();
            let operation = changed.models[index]
                .operations
                .iter_mut()
                .find(|operation| operation.name == "query")
                .unwrap();
            let fields = &mut operation.route.as_mut().unwrap().response.fields;
            let field = fields.first_mut().unwrap();
            match change {
                0 => field.path = "renamed_field".into(),
                1 => field.type_name = "boolean".into(),
                _ => field.nullable = !field.nullable,
            }
            assert_ne!(baseline, digest(&changed), "row shape change {change}");
        }
    }
}

#[test]
fn client_package_is_opt_in_and_carries_its_authored_metadata() {
    let file = wamn_schema_generator::client_ts::emit_ts_package_json(
        "@wamn/platform-fixture-client",
        "1.0.0",
    );
    assert_eq!(file.path(), "generated/client-ts/package.json");
    let source = std::str::from_utf8(file.bytes()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(source).unwrap();
    assert_eq!(
        parsed,
        serde_json::json!({
            "name": "@wamn/platform-fixture-client", "version": "1.0.0",
            "type": "module", "private": true, "exports": {".": "./index.ts"},
            "dependencies": {"@wamn/web-runtime": "^0.1.0"}
        })
    );
    let package = fixture::generate_fixture();
    assert!(
        package
            .files()
            .iter()
            .all(|file| !std::path::Path::new(file.path()).starts_with("generated/client-ts")),
        "TypeScript is opt-in"
    );
}

#[test]
fn the_host_route_clients_are_written_below_the_output_root() {
    let root = std::env::temp_dir().join(format!("wamn-host-route-client-{}", std::process::id()));
    let stale = root.join("wamn_control/client-ts/stale.ts");
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, "stale").unwrap();
    wamn_schema_generator::materialize_host_route_client(&root).unwrap();
    for client in ["wamn_control/client-ts", "wamn_control/control/client-ts"] {
        for file in ["index.ts", "package.json"] {
            assert!(root.join(client).join(file).is_file(), "{client}/{file}");
        }
    }
    assert!(
        !stale.exists(),
        "a file the contracts do not generate is removed"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn counter_feed_and_changes_preserve_int64_ids() {
    let package = fixture::generate_fixture();
    let operation: serde_json::Value = serde_json::from_slice(
        package
            .files()
            .iter()
            .find(|file| file.path() == "generated/contracts/counter/query.operation.json")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    let changes = operation["statements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "changes")
        .unwrap();
    let id = changes["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "change_id")
        .unwrap();
    assert_eq!(id["type"], "int64");
}

#[test]
fn emitted_client_type_checks_with_workspace_tsc() {
    typescript::type_check(&release()).expect("the emitted client satisfies strict TypeScript");
}
