use wamn_authoring_model::TestSetInput;

#[test]
fn wire_shape_contains_only_the_exact_definition() {
    let input: TestSetInput =
        serde_json::from_str(r#"{"definition":"{\"schema-version\":\"0.1\",\"cases\":[]}"}"#)
            .expect("test-set input parses");
    assert_eq!(
        serde_json::to_value(input).expect("test-set input serializes"),
        serde_json::json!({
            "definition": "{\"schema-version\":\"0.1\",\"cases\":[]}"
        })
    );
    assert!(
        serde_json::from_value::<TestSetInput>(serde_json::json!({
            "definition": "{}",
            "draft-id": "draft-a"
        }))
        .is_err()
    );
}
