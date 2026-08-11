use std::str::FromStr as _;

use serde_json::json;
use wamn_catalog::ExecutionNodeId;

#[test]
fn scalar_wire_round_trips_exactly() {
    let expected = ExecutionNodeId::new("normalize-write").expect("node id is valid");

    assert_eq!(expected.as_str(), "normalize-write");
    assert_eq!(expected.as_ref(), "normalize-write");
    assert_eq!(expected.to_string(), "normalize-write");
    assert_eq!(
        ExecutionNodeId::from_str("normalize-write").expect("parse scalar node id"),
        expected
    );

    let encoded = serde_json::to_value(&expected).expect("serialize node id");
    assert_eq!(encoded, json!("normalize-write"));
    assert_eq!(
        serde_json::from_value::<ExecutionNodeId>(encoded).expect("deserialize node id"),
        expected
    );
}

#[test]
fn exact_character_contract_accepts_every_allowed_shape() {
    for valid in ["a", "0", "-", "a-b", "--", "node-42"] {
        assert!(
            ExecutionNodeId::new(valid).is_ok(),
            "allowed scalar {valid:?} was rejected"
        );
    }
}

#[test]
fn invalid_scalar_values_are_refused() {
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
        let error = ExecutionNodeId::new(invalid).expect_err("invalid node id must fail");
        assert_eq!(error.value(), invalid);
        assert_eq!(
            error.to_string(),
            format!("execution node id {invalid:?} must match ^[a-z0-9-]+$")
        );
    }
}

#[test]
fn json_schema_pins_the_scalar_contract() {
    let schema = serde_json::to_value(schemars::schema_for!(ExecutionNodeId))
        .expect("serialize execution-node-id schema");

    assert_eq!(schema["type"], "string");
    assert_eq!(schema["pattern"], "^[a-z0-9-]+$");
    assert!(schema.get("items").is_none());
    assert!(schema.get("minItems").is_none());
}

#[test]
fn path_and_segment_wire_forms_have_no_aliases() {
    for invalid in [json!(["normalize"]), json!(["normalize", "write"])] {
        assert!(serde_json::from_value::<ExecutionNodeId>(invalid).is_err());
    }

    for invalid in ["normalize/write", "/normalize", "normalize/"] {
        assert!(ExecutionNodeId::from_str(invalid).is_err());
    }
}
