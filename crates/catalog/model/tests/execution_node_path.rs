use std::str::FromStr as _;

use serde_json::json;
use wamn_catalog::ExecutionNodePath;

fn path(segments: &[&str]) -> ExecutionNodePath {
    ExecutionNodePath::new(
        segments
            .iter()
            .map(|segment| (*segment).to_owned())
            .collect(),
    )
    .expect("test path is valid")
}

#[test]
fn structured_and_flattened_forms_round_trip_exactly() {
    let expected = path(&["normalize", "write"]);

    assert_eq!(expected.segments(), ["normalize", "write"]);
    assert_eq!(expected.to_string(), "normalize/write");
    assert_eq!(
        ExecutionNodePath::from_str("normalize/write").expect("parse canonical path"),
        expected
    );

    let encoded = serde_json::to_value(&expected).expect("serialize path");
    assert_eq!(encoded, json!(["normalize", "write"]));
    assert_eq!(
        serde_json::from_value::<ExecutionNodePath>(encoded).expect("deserialize path"),
        expected
    );
}

#[test]
fn construction_refuses_empty_and_noncanonical_segments() {
    let empty = ExecutionNodePath::new(Vec::new()).expect_err("empty path must fail");
    assert!(empty.is_empty_path());
    assert_eq!(empty.segment_index(), None);
    assert_eq!(empty.segment(), None);

    for invalid in [
        "",
        "Upper",
        "under_score",
        "has/slash",
        "has.dot",
        " leading",
        "trailing ",
        "café",
    ] {
        let error = ExecutionNodePath::new(vec!["valid".to_owned(), invalid.to_owned()])
            .expect_err("noncanonical segment must fail");
        assert_eq!(error.segment_index(), Some(1), "segment {invalid:?}");
        assert_eq!(error.segment(), Some(invalid), "segment {invalid:?}");
    }
}

#[test]
fn flattened_form_has_no_aliases_or_escaping() {
    for invalid in [
        "",
        "/normalize",
        "normalize/",
        "normalize//write",
        "normalize%2fwrite",
        r"normalize\write",
    ] {
        assert!(
            ExecutionNodePath::from_str(invalid).is_err(),
            "flattened alias {invalid:?} must fail"
        );
    }

    assert!(serde_json::from_value::<ExecutionNodePath>(json!("normalize/write")).is_err());
    assert!(serde_json::from_value::<ExecutionNodePath>(json!(["normalize/write"])).is_err());
}

#[test]
fn flattened_encoding_preserves_order_and_path_boundaries() {
    let paths = [
        path(&["a", "bc"]),
        path(&["ab", "c"]),
        path(&["bc", "a"]),
        path(&["a", "b", "c"]),
    ];
    let encodings = paths
        .iter()
        .map(ToString::to_string)
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(encodings.len(), paths.len());
    assert_eq!(paths[0].to_string(), "a/bc");
    assert_eq!(paths[2].to_string(), "bc/a");
}

#[test]
fn json_schema_pins_the_structured_path_contract() {
    let schema = serde_json::to_value(schemars::schema_for!(ExecutionNodePath))
        .expect("serialize execution-node-path schema");

    assert_eq!(schema["type"], "array");
    assert_eq!(schema["minItems"], 1);
    assert_eq!(schema["items"]["type"], "string");
    assert_eq!(schema["items"]["pattern"], "^[a-z0-9-]+$");
    assert!(schema.get("uniqueItems").is_none());
}

#[test]
fn error_display_identifies_the_rejected_boundary() {
    let empty = ExecutionNodePath::new(Vec::new()).expect_err("empty path must fail");
    assert_eq!(
        empty.to_string(),
        "execution node path must contain at least one segment"
    );

    let invalid = ExecutionNodePath::new(vec!["ok".to_owned(), "bad/path".to_owned()])
        .expect_err("separator-bearing segment must fail");
    assert_eq!(
        invalid.to_string(),
        "execution node path segment 1 \"bad/path\" must match ^[a-z0-9-]+$"
    );
}
