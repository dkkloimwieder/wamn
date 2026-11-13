use serde_json::json;
use wamn_compiler::ManifestErrorType as GenerateErrorType;
use wamn_compiler::manifest::PackageManifest;

use super::{description_json, fixture};

#[test]
fn crud_refuses_authored_protocol_metadata_but_custom_business_errors_remain_authored() {
    for (action, field, value) in [
        (
            "query",
            "filters",
            json!([{"field": "code", "binding": "json_array"}]),
        ),
        (
            "query",
            "sort",
            json!({
                "fields": ["created_at"], "directions": ["ascending", "descending"], "max_fields": 1
            }),
        ),
        (
            "query",
            "pagination",
            json!({
                "type": "keyset",
                "cursor": {"version": 1, "payload": "canonical_compact_json",
                    "encoding": "base64url_unpadded", "opaque": true, "invalid": "invalid_input"},
                "default_sort": {"field": "created_at", "direction": "ascending"},
                "tie_breaker": {"field": "id"}
            }),
        ),
        (
            "query",
            "limit",
            json!({
                "default": 100, "minimum": 1, "maximum": 100, "invalid": "invalid_input"
            }),
        ),
        (
            "update",
            "error_details",
            json!({
                "not_found": {"required": ["field", "id"]}
            }),
        ),
    ] {
        let mut manifest = fixture::manifest();
        manifest["models"]["widget"]["operations"][action][field] = value;
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let error = PackageManifest::from_slice(&bytes)
            .expect_err("obsolete CRUD protocol metadata was accepted");
        assert_eq!(
            error.error_type(),
            GenerateErrorType::InvalidManifest,
            "{action}.{field}"
        );
    }

    let mut envelope = fixture::manifest();
    envelope["custom_operations"]["widget.archive"]["input"]["raw_body_maximum"] = json!(1_048_576);
    envelope["custom_operations"]["widget.archive"]["input"]["envelope"] =
        json!({"minimum": 1, "maximum": 100, "invalid": "invalid_input"});
    envelope["custom_operations"]["widget.archive"]["input"]["item_semantics"] = json!("per_input");
    assert_eq!(
        PackageManifest::from_slice(&serde_json::to_vec(&envelope).unwrap())
            .expect_err("obsolete fixed envelope protocol was accepted")
            .error_type(),
        GenerateErrorType::InvalidManifest
    );

    let package = fixture::generate_fixture();
    let custom = description_json(&package, "widget.archive", "errors");
    let business = custom["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["literal"] == "already_archived")
        .unwrap();
    assert_eq!(business["detail"]["required"], json!(["field"]));
    assert_eq!(business["from"], "transaction_invariant");
}
