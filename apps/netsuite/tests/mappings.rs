use serde_json::{Value, json};

fn mapping(wiring: &str, node: &str, input: &Value, parameters: Option<&Value>) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../publication/wirings/{wiring}.json"));
    let wiring: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let config = &wiring["nodes"][node]["params"];
    let parameters = parameters.unwrap_or(&config["parameters"]);
    serde_json::from_str(
        &jsonata_expression::evaluate_with_parameters(
            config["expression"].as_str().unwrap(),
            input,
            parameters,
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn window_uses_the_checkpoint_and_due_with_stable_order() {
    let due = "2026-10-22T12:01:00Z";
    let result = mapping(
        "netsuite_pull",
        "window",
        &json!([{"value": {
            "value": "2026-10-22T12:00:00Z", "carry": due
        }}]),
        None,
    );
    assert_eq!(result["due"], due);
    assert_eq!(
        result["body"]["q"],
        "SELECT id, entityid AS name FROM customer WHERE lastmodifieddate >= TO_DATE('2026-10-22 12:00:00', 'YYYY-MM-DD HH24:MI:SS') AND lastmodifieddate < TO_DATE('2026-10-22 12:01:00', 'YYYY-MM-DD HH24:MI:SS') ORDER BY id"
    );
    // sync.finish stores the due time in its normalized timestamptz form.
    let normalized = mapping(
        "netsuite_pull",
        "window",
        &json!([{"value": {
            "value": "2026-10-22T12:00:00.000000Z", "carry": due
        }}]),
        None,
    );
    assert_eq!(normalized["body"]["q"], result["body"]["q"]);
    let first = mapping(
        "netsuite_pull",
        "window",
        &json!([{"value": {"value": null, "carry": due}}]),
        None,
    );
    assert!(
        first["body"]["q"]
            .as_str()
            .unwrap()
            .contains("1970-01-01 00:00:00")
    );
}

#[test]
fn pull_rows_become_one_consumer_batch_and_retain_due() {
    let due = "2026-10-22T12:01:00Z";
    let rows = mapping(
        "netsuite_pull",
        "rows",
        &json!({"carry": due, "items": [{"id": "42", "name": "Acme"}]}),
        Some(&json!({"external_system_id":"00000000-0000-4000-8000-00000000e001"})),
    );
    assert_eq!(
        rows,
        json!([{"request_id": due, "value": {"external_system_id":"00000000-0000-4000-8000-00000000e001", "idempotency_key": due, "items": [{"key": "42", "name": "Acme"}]}}])
    );
    let empty = mapping(
        "netsuite_pull",
        "rows",
        &json!({"carry": due, "items": []}),
        Some(&json!({"external_system_id":"00000000-0000-4000-8000-00000000e001"})),
    );
    assert_eq!(empty[0]["value"]["items"], json!([]));
    let finish = mapping(
        "netsuite_pull",
        "finish",
        &json!([{"request_id": due, "value": {"keys": []}}]),
        None,
    );
    assert_eq!(finish[0]["value"]["due"], due);
    let command = super::finish_codec::decode(&finish.to_string()).unwrap();
    assert_eq!(command[0].input.as_ref().unwrap().due, due);
}

#[test]
fn consumer_error_prevents_checkpoint_mapping() {
    let wiring: Value =
        serde_json::from_str(include_str!("../publication/wirings/netsuite_pull.json")).unwrap();
    for input in [
        json!([]),
        json!([{"request_id": "due", "error": {"code": "retry"}}]),
        json!([{"value":{"keys":[]}}, {"error":{"code":"retry"}}]),
    ] {
        assert!(
            jsonata_expression::evaluate(
                wiring["nodes"]["finish"]["params"]["expression"]
                    .as_str()
                    .unwrap(),
                &input
            )
            .is_err()
        );
    }
}

#[test]
fn push_uses_the_stable_key_for_body_and_percent_encoded_eid_path() {
    let event = json!({"event":"insert", "new":{"key":"customer /α?#%", "name":"Acme"}});
    let first = mapping("netsuite_push", "map", &event, None);
    assert_eq!(
        first,
        json!({"path":"/services/rest/record/v1/customer/eid:customer%20%2F%CE%B1%3F%23%25", "body":{"entityId":"Acme","externalId":"customer /α?#%"}})
    );
    let retry = mapping("netsuite_push", "map", &event, None);
    assert_eq!(retry["path"], first["path"]);
    let changed = json!({"event":"insert", "new":{"key":"customer /α?#%", "name":"Changed"}});
    assert_eq!(
        mapping("netsuite_push", "map", &changed, None)["path"],
        first["path"]
    );
}

#[test]
fn create_defaults_to_post_and_optional_pointer_selects_put() {
    let input = json!({"body":{"entityId":"Acme"}, "identity":{"key":"a/b"}});
    let post = mapping("record_create", "map", &input, None);
    assert_eq!(post[0]["port"], "main");
    assert_eq!(post[0]["path"], "/services/rest/record/v1/customer");
    let put = mapping(
        "record_create",
        "map",
        &input,
        Some(&json!({"record_type":"customer", "external_id_input_pointer":"/identity/key"})),
    );
    assert_eq!(put[0]["port"], "items");
    assert_eq!(
        put[0]["path"],
        "/services/rest/record/v1/customer/eid:a%2Fb"
    );
    assert_eq!(put[0]["body"], input["body"]);
}

#[test]
fn record_and_restlet_parameters_build_relative_paths() {
    for operation in ["record_get", "record_update"] {
        let output = mapping(
            operation,
            "map",
            &json!({"id":"12/3", "body":{"name":"Changed"}}),
            Some(&json!({"record_type":"salesOrder"})),
        );
        assert_eq!(output["path"], "/services/rest/record/v1/salesOrder/12%2F3");
    }
    let output = mapping(
        "restlet_call",
        "map",
        &json!({"body":{"ok":true}}),
        Some(&json!({"script":"a&b", "deploy":"1"})),
    );
    assert_eq!(
        output,
        json!({"path":"/app/site/hosting/restlet.nl?script=a%26b&deploy=1","body":{"ok":true}})
    );
}
