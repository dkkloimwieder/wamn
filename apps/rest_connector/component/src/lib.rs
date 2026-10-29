//! Pull into the consumer and retain integration state through the composed base.
#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits equal length and capacity"
)]

wit_bindgen::generate!({
    world: "rest-connector:component/rest-connector@0.1.0",
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../target/wamn/wamn_integration/wit/deps/wamn-integration-setting",
        "../../target/wamn/wamn_integration/wit/deps/wamn-integration-log",
        "../../target/wamn/rest_connector/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

use exports::rest_connector::sync::{finish, pushed};
use serde_json::{Value, json};
use wamn::node::types::{Emission, ErrorDetail, NodeContext, NodeError};
use wamn_integration::{
    log::append,
    setting::{read, write},
};

struct Component;
export!(Component);

mod finish_codec {
    use super::finish as contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/rest_connector/wit/sync_finish_codec.rs"
    ));
}
mod pushed_codec {
    use super::pushed as contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/rest_connector/wit/sync_pushed_codec.rs"
    ));
}

fn invalid(message: &str) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: message.to_owned(),
        code: Some("invalid_input".into()),
    })
}

fn base_failure(message: &str) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message: message.to_owned(),
        code: Some("internal_error".into()),
    })
}

fn checkpoint_failure(action: &str, mut error: NodeError) -> NodeError {
    let message = match &mut error {
        NodeError::InvalidInput(detail)
        | NodeError::Retryable(detail)
        | NodeError::Terminal(detail) => &mut detail.message,
        NodeError::RateLimited(detail) => &mut detail.detail.message,
        NodeError::Cancelled => return error,
    };
    *message = format!("checkpoint {action} failed: {message}");
    error
}

fn checkpoint(value: Option<String>) -> Result<Option<Value>, NodeError> {
    value
        .map(|value| {
            let timestamp: String = serde_json::from_str(&value)
                .map_err(|_| base_failure("checkpoint must be a JSON timestamp string"))?;
            chrono::DateTime::parse_from_rfc3339(&timestamp)
                .map_err(|_| base_failure("checkpoint must be an RFC 3339 timestamp"))?;
            Ok(Value::String(timestamp))
        })
        .transpose()
}

impl exports::wamn::node::async_handler::Guest for Component {
    async fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input: Value =
            serde_json::from_str(&input).map_err(|error| invalid(&error.to_string()))?;
        let due = input
            .get("due")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("schedule input requires due"))?;
        chrono::DateTime::parse_from_rfc3339(due).map_err(|error| invalid(&error.to_string()))?;
        let rows = read::run(
            context,
            vec![read::ReadItem {
                input: Ok(read::ReadRequest {
                    key: "last_synced_at".into(),
                    carry: None,
                }),
            }],
        )
        .await
        .map_err(|error| checkpoint_failure("read", error))?;
        let [row]: [read::ReadOutcome; 1] = rows
            .try_into()
            .map_err(|_| base_failure("checkpoint read returned the wrong result count"))?;
        let value = row.outcome.map_err(|error| match error {
            read::ReadError::Retry | read::ReadError::Timeout => {
                NodeError::Retryable(ErrorDetail {
                    message: format!("checkpoint read failed: {error:?}"),
                    code: Some("retry".into()),
                })
            }
            _ => base_failure(&format!("checkpoint read failed: {error:?}")),
        })?;
        Ok(Emission {
            payload: json!({"due": due, "last_synced_at": checkpoint(value.value)?}).to_string(),
            port: None,
        })
    }
}

fn completed_due(input: &[finish::FinishItem]) -> Result<String, NodeError> {
    let [request_element] = input else {
        return Err(invalid("checkpoint requires one consumer batch outcome"));
    };
    if request_element.input.is_err() {
        return Err(invalid("consumer batch did not succeed"));
    }
    chrono::DateTime::parse_from_rfc3339(&request_element.request_id)
        .map_err(|error| invalid(&error.to_string()))?;
    Ok(request_element.request_id.clone())
}

impl finish::Guest for Component {
    async fn run(
        context: NodeContext,
        input: Vec<finish::FinishItem>,
    ) -> Result<Vec<finish::FinishOutcome>, NodeError> {
        finish_codec::validate(&input).map_err(|error| invalid(error.context()))?;
        let due = completed_due(&input)?;
        let mut failure = None;
        let rows = finish_codec::run(input, &mut failure, async |failure, _request| {
            let rows = write::run(
                context.clone(),
                vec![write::WriteItem {
                    request_id: context.delivery_id.clone(),
                    input: Ok(write::WriteRequest {
                        idempotency_key: context.delivery_id.clone(),
                        key: "last_synced_at".into(),
                        value: json!(due).to_string(),
                    }),
                }],
            )
            .await
            .map_err(|error| {
                *failure = Some(checkpoint_failure("write", error));
                finish::FinishError::InternalError
            })?;
            let [row]: [write::WriteOutcome; 1] = rows
                .try_into()
                .map_err(|_| finish::FinishError::InternalError)?;
            row.outcome.map_err(|error| match error {
                write::WriteError::Retry => finish::FinishError::Retry,
                write::WriteError::Timeout => finish::FinishError::Timeout,
                _ => finish::FinishError::InternalError,
            })?;
            Ok(finish::FinishResult { due: due.clone() })
        })
        .await;
        match failure {
            Some(error) => Err(error),
            None => Ok(rows),
        }
    }

    async fn run_json(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input = finish_codec::decode(&input).map_err(|error| invalid(error.context()))?;
        let rows = <Self as finish::Guest>::run(context, input).await?;
        Ok(Emission {
            payload: finish_codec::encode(&rows),
            port: None,
        })
    }
}

impl pushed::Guest for Component {
    async fn run(
        context: NodeContext,
        input: Vec<pushed::PushedItem>,
    ) -> Result<Vec<pushed::PushedOutcome>, NodeError> {
        pushed_codec::validate(&input).map_err(|error| invalid(error.context()))?;
        Ok(pushed_codec::run(input, &mut (), async |(), request| {
            let rows = append::run(
                context.clone(),
                vec![append::AppendItem {
                    request_id: request.record_id.clone(),
                    input: Ok(append::AppendRequest {
                        idempotency_key: format!("{}:{}", context.delivery_id, request.record_id),
                        delivery_id: context.delivery_id.clone(),
                        record_id: request.record_id,
                        outcome: "pushed".into(),
                        detail: None,
                    }),
                }],
            )
            .await
            .map_err(|_| pushed::PushedError::InternalError)?;
            let [row]: [append::AppendOutcome; 1] = rows
                .try_into()
                .map_err(|_| pushed::PushedError::InternalError)?;
            let value = row.outcome.map_err(|error| match error {
                append::AppendError::Retry => pushed::PushedError::Retry,
                append::AppendError::Timeout => pushed::PushedError::Timeout,
                _ => pushed::PushedError::InternalError,
            })?;
            Ok(pushed::PushedResult {
                record_id: value.record_id,
            })
        })
        .await)
    }

    async fn run_json(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input = pushed_codec::decode(&input).map_err(|error| invalid(error.context()))?;
        let rows = <Self as pushed::Guest>::run(context, input).await?;
        Ok(Emission {
            payload: pushed_codec::encode(&rows),
            port: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_errors_keep_the_base_cause_and_retry_class() {
        for action in ["read", "write"] {
            let error = checkpoint_failure(
                action,
                NodeError::Retryable(ErrorDetail {
                    message: "database connection unavailable".into(),
                    code: Some("retry".into()),
                }),
            );
            let NodeError::Retryable(detail) = error else {
                panic!("retry class survives")
            };
            assert_eq!(
                detail.message,
                format!("checkpoint {action} failed: database connection unavailable")
            );
            assert_eq!(detail.code.as_deref(), Some("retry"));
        }
    }

    fn evaluate(expression: &str, input: &Value) -> Value {
        let ast = jsonata_core::parser::parse(expression).unwrap();
        let data = jsonata_core::value::JValue::from(input.clone());
        let result = jsonata_core::evaluator::Evaluator::new()
            .evaluate(&ast, &data)
            .unwrap();
        serde_json::from_str(&result.to_json_string().unwrap()).unwrap()
    }

    #[test]
    fn the_authored_mapping_preserves_due_even_without_items() {
        let wiring: Value =
            serde_json::from_str(include_str!("../../publication/wirings/rest_item.json")).unwrap();
        let due = "2026-10-21T12:00:00Z";
        for items in [json!([]), json!([{"id": "1", "name": "one"}])] {
            let mapped = evaluate(
                wiring["nodes"]["rows"]["params"]["expression"]
                    .as_str()
                    .unwrap(),
                &json!({"carry": due, "items": items}),
            );
            assert_eq!(mapped.as_array().unwrap().len(), 1);
            assert_eq!(mapped[0]["request_id"], due);
            assert_eq!(
                mapped[0]["value"]["items"].as_array().unwrap().len(),
                items.as_array().unwrap().len()
            );
            let results = json!([{"request_id": mapped[0]["request_id"], "value": {"keys": []}}]);
            let input = finish_codec::decode(&results.to_string()).unwrap();
            assert_eq!(completed_due(&input).unwrap(), due);
        }
    }

    #[test]
    fn a_missing_checkpoint_is_valid_but_a_non_timestamp_is_not() {
        assert_eq!(checkpoint(None).unwrap(), None);
        assert!(checkpoint(Some(json!("2026-10-21T12:00:00Z").to_string())).is_ok());
        for value in ["42", "null", "{}", "\"yesterday\""] {
            assert!(checkpoint(Some(value.into())).is_err());
        }
    }

    #[test]
    fn every_consumer_result_must_succeed_before_the_checkpoint_write() {
        let due = "2026-10-21T12:00:00Z";
        let success = json!({"request_id": due, "value": {"keys": ["1"]}});
        let empty = json!({"request_id": due, "value": {"keys": []}});
        let refused = json!({"request_id": due, "error": {"code": "retry"}});
        let accepted = |rows: Value| {
            finish_codec::decode(&rows.to_string()).is_ok_and(|input| completed_due(&input).is_ok())
        };
        assert!(accepted(json!([success])));
        assert!(accepted(json!([empty])));
        assert!(!accepted(json!([refused])));
        assert!(!accepted(json!([success, refused])));
        assert!(!accepted(json!([])));
        assert!(!accepted(
            json!([{"request_id": "other", "value": {"keys": []}}])
        ));
    }
}
