//! Connection declarations carry shape, never deployed catalog authority.
use serde_json::json;
use wamn_compiler::manifest::{ConnectionMode, PackageManifest};

fn manifest() -> serde_json::Value {
    json!({"package":{"id":"fixture","version":"1.0.0"},
        "required_platform_policy_contract":{"id":"fixture_data_access","state":"satisfied"},
        "models":{},"components":{"fixture":{}},
        "connections":{"signed-upstream":{"interface":"wamn:modbus/read@1.0.0","mode":"read"}}})
}

#[test]
fn connection_shape_round_trips_without_a_catalog() {
    let value = manifest();
    let parsed = PackageManifest::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        parsed.connections["signed-upstream"].mode,
        ConnectionMode::Read
    );
    assert_eq!(
        serde_json::to_value(parsed).unwrap()["connections"],
        value["connections"]
    );
}

#[test]
fn connection_shape_refuses_missing_invalid_and_value_fields() {
    for entry in [
        json!({"mode":"read"}),
        json!({"interfaces":["wamn:connection/http@0.1.0"]}),
        json!({"integration_type":"http","mode":"read"}),
        json!({"integration":"http","mode":"read"}),
        json!({"interface":"invalid","mode":"read"}),
        json!({"interfaces":[],"mode":"read"}),
        json!({"interface":"wamn:connection/http@0.1.0","mode":"execute"}),
        json!({"interface":"wamn:connection/http@0.1.0","mode":"read","url":"https://example.com"}),
        json!({"interface":"wamn:connection/http@0.1.0","mode":"read","credential":"secret"}),
        json!({"interface":"wamn:connection/http@0.1.0","mode":"read","parameters":{}}),
    ] {
        let mut value = manifest();
        value["connections"]["signed-upstream"] = entry;
        assert!(
            PackageManifest::from_slice(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{value}"
        );
    }
}

#[test]
fn kcl_requires_interfaces_mode_and_no_values() {
    let directory =
        std::env::temp_dir().join(format!("wamn-connections-kcl-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("wamn.k");
    for (entry, valid) in [
        (
            r#"interface = "example:meter/read@1.0.0", mode = "read""#,
            true,
        ),
        (
            r#"interface = "wamn:connection/http@0.1.0", mode = "write""#,
            true,
        ),
        (r#"integration = "http""#, false),
        (r#"mode = "read""#, false),
        (r#"interfaces = [], mode = "read""#, false),
        (r#"interface = "", mode = "read""#, false),
        (r#"integration = "http", mode = "execute""#, false),
        (
            r#"integration = "http", mode = "read", url = "https://example.com""#,
            false,
        ),
    ] {
        std::fs::write(
            &path,
            format!(
                r#"import manifest
manifest.Package {{
 package = {{id = "fixture", version = "1.0.0"}}
 required_platform_policy_contract = {{id = "fixture_data_access", state = "satisfied"}}
 models = {{}}
 components = {{fixture = {{}}}}
 connections = {{erp = {{{entry}}}}}
}}
"#
            ),
        )
        .unwrap();
        let compiled = wamn_compiler::compile_manifest(&directory);
        assert_eq!(compiled.is_ok(), valid, "{entry}: {compiled:?}");
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn connection_names_use_the_same_label_rule_as_install() {
    for (name, valid) in [
        ("a".to_owned(), true),
        ("a".repeat(63), true),
        ("a".repeat(64), false),
        ("with-digits-42".to_owned(), true),
        (String::new(), false),
        ("Upper".to_owned(), false),
        ("has_underscore".to_owned(), false),
        ("1first".to_owned(), false),
        ("two.words".to_owned(), false),
    ] {
        let mut value = manifest();
        let declaration = value["connections"]["signed-upstream"].clone();
        value["connections"] = json!({name.clone(): declaration});
        let parsed = PackageManifest::from_slice(&serde_json::to_vec(&value).unwrap());
        assert_eq!(parsed.is_ok(), valid, "{name:?}: {parsed:?}");
    }
}
