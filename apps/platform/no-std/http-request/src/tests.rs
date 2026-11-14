use super::*;
use serde_json::json;

fn terminal<T>(result: Result<T, NodeError>) {
    assert!(matches!(result, Err(NodeError::Terminal(_))));
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
        terminal(request_path(&config, &input));
    }
    terminal(request_path(&json!({}), &json!({})));
}

#[test]
fn both_path_sources_refuse_absolute_and_network_paths() {
    for path in ["https://other.invalid/", "//other.invalid/", "records", ""] {
        terminal(request_path(&json!({"path-and-query":path}), &json!({})));
        terminal(request_path(
            &json!({"path-input-pointer":"/path"}),
            &json!({"path":path}),
        ));
    }
}

#[test]
fn missing_body_targets_and_invalid_pointer_parameters_refuse() {
    terminal(request_body(
        &json!({"body-input-pointer":"/absent"}),
        &json!({}),
        "{}",
    ));
    for pointer in [json!(null), json!(1), json!([])] {
        terminal(request_path(
            &json!({"path-input-pointer":pointer}),
            &json!({}),
        ));
        terminal(request_body(
            &json!({"body-input-pointer":pointer}),
            &json!({}),
            "{}",
        ));
    }
    terminal(request_path(
        &json!({"path-input-pointer":"invalid"}),
        &json!({}),
    ));
}
