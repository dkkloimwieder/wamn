//! Request element response bodies shared by the guest and its native tests.

use serde_json::json;

use crate::{HttpResponse, error_response};

pub(crate) fn with_request_id(mut body: String, request_id: &str) -> Result<String, HttpResponse> {
    // The stored request element is already JSON without request_id. Insert only the
    // current request id, preserving its key order, number spelling and spaces.
    let object = body.trim();
    let members = object
        .strip_prefix('{')
        .and_then(|object| object.strip_suffix('}'))
        .ok_or_else(|| error_response(503, "execution-failed"))?;
    let separator = if members.trim().is_empty() { "" } else { "," };
    let end = body.trim_end().len() - 1;
    body.insert_str(
        end,
        &format!("{separator}\"request_id\":{}", json!(request_id)),
    );
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn replay_requires_an_object_and_keeps_the_result_value() {
        let answer = with_request_id(r#"{"value":7}"#.into(), "request").unwrap();
        let answer: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(answer["value"], 7);
        assert!(answer["request_id"].is_string());
        assert!(with_request_id("[]".into(), "request").is_err());
    }
}
