use super::*;
use serde_json::{Value, json};

#[test]
fn pointers_select_the_path_and_json_body() {
    let input = json!({"path":"/records/eid:a%2Fb?expand=true", "body":{"name":"Example"}});
    let config = json!({"path-and-query":"/fixed", "path-input-pointer":"/path", "body-input-pointer":"/body"});
    assert_eq!(
        request_path(&config, &input).unwrap(),
        "/records/eid:a%2Fb?expand=true"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request_body(&config, &input, "").unwrap()).unwrap(),
        input["body"]
    );
}

#[test]
fn default_keeps_the_fixed_path_and_original_body_bytes() {
    let original = "{ \"path\" : \"/ignored\", \"body\" : null }";
    let input = serde_json::from_str(original).unwrap();
    let config = json!({"path-and-query":"/fixed?limit=2"});
    assert_eq!(request_path(&config, &input).unwrap(), "/fixed?limit=2");
    assert_eq!(
        request_body(&config, &input, original).unwrap(),
        original.as_bytes()
    );
}

fn terminal_code<T>(result: Result<T, NodeError>, expected: &str) {
    let Err(NodeError::Terminal(detail)) = result else {
        panic!("expected terminal error");
    };
    assert_eq!(detail.code.as_deref(), Some(expected));
}

#[test]
fn missing_and_nonstring_path_targets_refuse_without_fallback() {
    let config = json!({"path-and-query":"/fallback", "path-input-pointer":"/path"});
    for input in [
        json!({}),
        json!({"path":null}),
        json!({"path":1}),
        json!({"path":[]}),
    ] {
        terminal_code(request_path(&config, &input), "invalid-input-pointer");
    }
    terminal_code(request_path(&json!({}), &json!({})), "invalid-config");
}

#[test]
fn both_path_sources_refuse_absolute_and_network_paths() {
    for path in ["https://other.invalid/", "//other.invalid/", "records", ""] {
        terminal_code(
            request_path(&json!({"path-and-query":path}), &json!({})),
            "invalid-config",
        );
        terminal_code(
            request_path(
                &json!({"path-input-pointer":"/path"}),
                &json!({"path":path}),
            ),
            "invalid-config",
        );
    }
}

#[test]
fn missing_body_targets_and_invalid_pointer_parameters_refuse() {
    terminal_code(
        request_body(&json!({"body-input-pointer":"/absent"}), &json!({}), "{}"),
        "invalid-input-pointer",
    );
    for pointer in [json!(null), json!(1), json!([])] {
        terminal_code(
            request_path(&json!({"path-input-pointer":pointer}), &json!({})),
            "invalid-config",
        );
        terminal_code(
            request_body(&json!({"body-input-pointer":pointer}), &json!({}), "{}"),
            "invalid-config",
        );
    }
    terminal_code(
        request_path(&json!({"path-input-pointer":"invalid"}), &json!({})),
        "invalid-input-pointer",
    );
}

#[test]
fn pointers_accept_the_root_and_json_null() {
    assert_eq!(
        request_path(&json!({"path-input-pointer":""}), &json!("/root")).unwrap(),
        "/root"
    );
    assert_eq!(
        request_body(
            &json!({"body-input-pointer":"/body"}),
            &json!({"body":null}),
            ""
        )
        .unwrap(),
        b"null"
    );
    assert_eq!(
        request_body(&json!({"body-input-pointer":""}), &json!([1, 2]), "").unwrap(),
        b"[1,2]"
    );
}
