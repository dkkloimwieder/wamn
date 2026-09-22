use serde_json::json;

use super::fixture;

fn source(package: &wamn_schema_generator::GeneratedPackage, path: &str) -> String {
    String::from_utf8(package.file(path).unwrap().bytes().to_vec()).unwrap()
}

#[test]
fn typed_wit_and_codec_follow_package_and_envelope_declarations() {
    for package_id in ["platform_fixture", "other_fixture"] {
        let mut manifest = fixture::manifest();
        manifest["package"]["id"] = json!(package_id);
        manifest["models"]["widget"]["owner"] = json!(package_id);
        manifest["models"]["widget_maker"]["owner"] = json!(package_id);
        manifest["custom_operations"]["widget.archive"]["input"]["raw_body_maximum"] = json!(4096);
        manifest["custom_operations"]["widget.archive"]["input"]["envelope"] =
            json!({"minimum": 2, "maximum": 7});
        let package = fixture::generate_with(&fixture::catalog(), &manifest);
        let wit = source(
            &package,
            &format!(
                "generated/wit/deps/{}-widget/package.wit",
                package_id.replace('_', "-")
            ),
        );
        assert!(wit.contains("run: async func"));
        assert!(wit.contains("run-json: async func"));
        assert!(wit.contains("input: result<archive-request, invalid-input-detail>"));
        let codec = source(&package, "generated/wit/widget_archive_codec.rs");
        for expected in [
            "edit_version.to_string()",
            "map_err(|_| invalid(\"input\"))",
            "ArchiveError::AlreadyArchived",
            "const MINIMUM: usize = 2;",
            "const MAXIMUM: usize = 7;",
            "item count must be 2..=7",
        ] {
            assert!(codec.contains(expected), "{expected}");
        }
    }
}

#[test]
fn typed_custom_shapes_do_not_depend_on_application_names() {
    let mut manifest = fixture::manifest();
    let mut operation = manifest["custom_operations"]["widget.archive"].clone();
    operation["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({"path": "entries[].key", "type": "uuid", "nullable": false}),
            json!({"path": "entries[].quantity", "type": "numeric", "nullable": false}),
        ]);
    for action in ["archive", "adjust", "merge", "split", "summarize"] {
        operation["permission"] = json!(format!("widget.{action}"));
        manifest["custom_operations"][format!("widget.{action}")] = operation.clone();
    }
    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    let wit = source(
        &package,
        "generated/wit/deps/platform-fixture-widget/package.wit",
    );
    for action in ["archive", "adjust", "merge", "split", "summarize"] {
        assert!(wit.contains(&format!("interface {action}")));
    }
    assert!(wit.contains("record archive-entries"));
    assert!(wit.contains("entries: list<archive-entries>"));
    assert!(!wit.contains("record archive-line"));
    let codec = source(&package, "generated/wit/widget_archive_codec.rs");
    assert!(codec.contains("struct JsonEntries"));
    assert!(codec.contains("contract::ArchiveEntries"));
    assert!(!codec.contains("JsonLine"));

    let mut manifest = fixture::manifest();
    let operation = manifest["custom_operations"]["widget.archive"]
        .as_object_mut()
        .unwrap();
    operation.insert("kind".into(), json!("event_handler"));
    operation.insert("visibility".into(), json!("private"));
    for field in [
        "permission",
        "result",
        "transaction",
        "automatic_retry",
        "idempotent_by",
    ] {
        operation.remove(field);
    }
    operation["errors"]
        .as_array_mut()
        .unwrap()
        .retain(|error| error != "permission_denied" && error != "concurrency_conflict");
    operation.insert(
        "registration".into(),
        json!({"source_package": "platform_fixture", "entity": "widget", "ops": ["insert"]}),
    );
    operation.insert(
        "input".into(),
        json!({"fields": [
            {"path": "action", "type": "text", "nullable": false, "values": ["created"]},
            {"path": "payload.key", "type": "uuid", "nullable": false}
        ]}),
    );
    let package = fixture::generate_with(&fixture::catalog(), &manifest);
    let codec = source(&package, "generated/wit/widget_archive_codec.rs");
    assert!(codec.contains("struct JsonPayload"));
    assert!(codec.contains("request.action"));
    assert!(codec.contains("request.payload.key"));
    assert!(!codec.contains("value.event"));
}
