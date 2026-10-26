//! Connection declarations carry shape, never deployed catalog authority.
use serde_json::json;
use wamn_schema_generator::{ConnectionMode, PackageManifest};

fn manifest() -> serde_json::Value {
    json!({"package":{"id":"fixture","version":"1.0.0"},
        "required_platform_policy_contract":{"id":"fixture_data_access","state":"satisfied"},
        "models":{},"components":{"fixture":{"connections":[]}},
        "connections":{"signed-upstream":{"integration":"vendor_http","mode":"read"}}})
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
        json!({"integration":"http"}),
        json!({"integration":"","mode":"read"}),
        json!({"integration":"http","mode":"execute"}),
        json!({"integration":"http","mode":"read","url":"https://example.com"}),
        json!({"integration":"http","mode":"read","credential":"secret"}),
        json!({"integration":"http","mode":"read","parameters":{}}),
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
fn kcl_checks_integration_mode_and_no_values() {
    let directory =
        std::env::temp_dir().join(format!("wamn-connections-kcl-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("wamn.k");
    for (entry, valid) in [
        (r#"integration = "future_connector", mode = "read""#, true),
        (r#"integration = "http", mode = "write""#, true),
        (r#"integration = "http""#, false),
        (r#"mode = "read""#, false),
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
 components = {{fixture = {{connections = []}}}}
 connections = {{erp = {{{entry}}}}}
}}
"#
            ),
        )
        .unwrap();
        assert_eq!(
            wamn_schema_generator::compile_manifest(&directory).is_ok(),
            valid,
            "{entry}"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}
