//! Item response bodies shared by the guest and its native tests.

use serde_json::{Value, json};

use crate::DeliveryError;

pub(crate) fn refusal(code: &str, message: String, detail: Value) -> Value {
    json!({"error": {"code": code, "message": message, "detail": detail}})
}

pub(crate) fn resolved(intent: &str, basis: &str) -> Value {
    refusal(
        "intent-resolved",
        format!("an operator resolved intent {intent} by {basis}; send a new key"),
        json!({"intent": intent, "basis": basis}),
    )
}

pub(crate) fn uncertain(intent: &str) -> Value {
    refusal(
        "intent-uncertain",
        format!("intent {intent} began and never finished; an operator resolves it"),
        json!({"intent": intent}),
    )
}

pub(crate) fn with_request_id(mut body: String, request_id: &str) -> Result<String, DeliveryError> {
    // The stored item is already JSON without request_id. Insert only the
    // current request id, preserving its key order, number spelling and spaces.
    let object = body.trim();
    let members = object
        .strip_prefix('{')
        .and_then(|object| object.strip_suffix('}'))
        .ok_or(DeliveryError::ExecutionFailed)?;
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

    #[test]
    fn resolved_preserves_the_existing_refusal_shape() {
        assert_eq!(
            resolved("base:widget/update#key", "external-evidence"),
            json!({"error": {
                "code": "intent-resolved",
                "message": "an operator resolved intent base:widget/update#key by external-evidence; send a new key",
                "detail": {"intent": "base:widget/update#key", "basis": "external-evidence"},
            }}),
        );
    }

    #[test]
    fn uncertain_preserves_the_existing_refusal_shape() {
        assert_eq!(
            uncertain("base:widget/update#key"),
            json!({"error": {
                "code": "intent-uncertain",
                "message": "intent base:widget/update#key began and never finished; an operator resolves it",
                "detail": {"intent": "base:widget/update#key"},
            }}),
        );
    }

    #[test]
    fn replay_preserves_stored_bytes_and_escapes_the_current_request_id() {
        let body = " \n{ \"value\": {\"z\":1e+00, \"a\":-0.00}, \"other\": [] }\t";
        let answer = with_request_id(body.into(), "new\"\nrequest").unwrap();
        assert_eq!(
            answer,
            " \n{ \"value\": {\"z\":1e+00, \"a\":-0.00}, \"other\": [] ,\"request_id\":\"new\\\"\\nrequest\"}\t",
        );
        assert_eq!(
            serde_json::from_str::<Value>(&answer).unwrap()["request_id"],
            "new\"\nrequest",
        );
        assert_eq!(
            with_request_id("{ }".into(), "r").unwrap(),
            "{ \"request_id\":\"r\"}"
        );
        assert!(with_request_id("[]".into(), "r").is_err());
    }
}
