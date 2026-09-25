//! The intent rules of one route call (`docs/plan/edge.md` 4.7).
//!
//! A call of a kind that changes records logs one intent for each input item
//! before its export runs. The key is the item field that the route names for
//! its idempotency key, or else the item's `request_id`. Only new items run:
//! an item whose key finished answers its stored outcome, an item whose key
//! began and never finished answers `intent-uncertain`, and an item whose key
//! an operator resolved answers `intent-resolved` with the operator's basis.
//! The answers merge into one item list in the order of the input.

use std::collections::{HashMap, HashSet};
use std::fmt;

use anyhow::Context as _;
use serde_json::{Map, Value, json};
use wamn_catalog::OperationKind;
use wamn_run_state::IntentStore;
use wamn_run_state::intent_store::{Begun, Intent, IntentId, StoredOutcome};

use super::{ApplicationHost, OperationCall, node_types, run_export};

/// The item error of a key that began and never finished.
pub const INTENT_UNCERTAIN: &str = "intent-uncertain";
/// The item error of a key that an operator resolved.
pub const INTENT_RESOLVED: &str = "intent-resolved";
/// The generated error literal of a key repeated with another input.
pub const IDEMPOTENCY_CONFLICT: &str = "idempotency_conflict";
/// The generated error literal of an input that is not the item envelope.
const INVALID_INPUT: &str = "invalid_input";
/// The item field that correlates an item with its result.
const REQUEST_ID: &str = "request_id";

/// What one route call needs to log its intents.
#[derive(Clone, Copy)]
pub struct IntentContext<'a> {
    pub store: &'a dyn IntentStore,
    pub tenant: &'a str,
    /// The release that serves the call.
    pub release: &'a str,
    pub package: &'a str,
    /// The kind of the route's operation. Only [`logs_intent`] kinds log.
    pub kind: OperationKind,
    /// The item field of the idempotency key, such as
    /// `value.idempotency_key`. `None` keys each item by its `request_id`.
    pub key_field: Option<&'a str>,
}

impl fmt::Debug for IntentContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IntentContext")
            .field("tenant", &self.tenant)
            .field("release", &self.release)
            .field("package", &self.package)
            .field("kind", &self.kind)
            .field("key_field", &self.key_field)
            .finish_non_exhaustive()
    }
}

/// Whether a call of `kind` logs its intents. A read never does.
#[must_use]
pub fn logs_intent(kind: OperationKind) -> bool {
    matches!(
        kind,
        OperationKind::Create
            | OperationKind::Update
            | OperationKind::Delete
            | OperationKind::Command
    )
}

/// One input item and the key of its intent.
struct Item<'v> {
    request_id: &'v str,
    key: &'v str,
    value: &'v Value,
}

/// Read the item envelope, or say why the input is not one.
fn items<'v>(input: &'v Value, key_field: Option<&str>) -> Result<Vec<Item<'v>>, String> {
    let items = input.as_array().ok_or("the input is not an item list")?;
    let mut keys = HashSet::with_capacity(items.len());
    items
        .iter()
        .map(|value| {
            let request_id = value
                .get(REQUEST_ID)
                .and_then(Value::as_str)
                .ok_or("an item has no request_id")?;
            let key = match key_field {
                Some(field) => field
                    .split('.')
                    .try_fold(value, |value, name| value.get(name))
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("item {request_id} has no {field}"))?,
                None => request_id,
            };
            if !keys.insert(key) {
                return Err(format!("the key {key} repeats within one call"));
            }
            Ok(Item {
                request_id,
                key,
                value,
            })
        })
        .collect()
}

/// The input hash of one item's intent: the canonical JSON SHA-256 of the item
/// without its `request_id`, so that a retry under one idempotency key with a
/// new request id is the same input.
#[must_use]
pub fn item_input_hash(item: &Value) -> String {
    let mut body = item.clone();
    if let Some(object) = body.as_object_mut() {
        object.remove(REQUEST_ID);
    }
    wamn_execution_contract::canonical_json_sha256(&body)
}

/// An item body that answers with an error and runs nothing.
fn refusal(code: &str, message: String, detail: Value) -> Value {
    let error = Map::from_iter([
        ("code".to_owned(), Value::from(code)),
        ("message".to_owned(), Value::String(message)),
        ("detail".to_owned(), detail),
    ]);
    Value::Object(Map::from_iter([("error".to_owned(), Value::Object(error))]))
}

/// The item body of an error that failed the whole call.
fn failed_body(error: &node_types::NodeError) -> Value {
    let detail = match error {
        node_types::NodeError::Retryable(detail)
        | node_types::NodeError::Terminal(detail)
        | node_types::NodeError::InvalidInput(detail) => detail,
        node_types::NodeError::RateLimited(limited) => &limited.detail,
        node_types::NodeError::Cancelled => unreachable!("a cancelled call finishes no intent"),
    };
    json!({"error": {"code": detail.code, "message": detail.message}})
}

/// The answer to a key whose intent began and never finished.
fn uncertain(id: &IntentId) -> Value {
    refusal(
        INTENT_UNCERTAIN,
        format!(
            "intent {} began and never finished; an operator resolves it",
            id.0
        ),
        json!({"intent": id.0}),
    )
}

/// Log the intents of one call, run its new items, and merge every answer.
///
/// A trap, a missed deadline or a host failure leaves the new intents begun, so
/// they are uncertain, because the export may have changed records.
pub(super) async fn invoke_logged<H: ApplicationHost>(
    host: &H,
    call: OperationCall<'_, H::Policy>,
    intent: IntentContext<'_>,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    let items = match items(call.input, intent.key_field) {
        Ok(items) => items,
        Err(message) => {
            return Ok(Err(node_types::NodeError::InvalidInput(
                node_types::ErrorDetail {
                    message,
                    code: Some(INVALID_INPUT.to_owned()),
                },
            )));
        }
    };
    let mut answers: Vec<Option<Value>> = Vec::with_capacity(items.len());
    let mut running = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let begun = intent
            .store
            .begin(&Intent {
                tenant: intent.tenant,
                release: intent.release,
                package: intent.package,
                operation: call.operation,
                idempotency_key: item.key,
                input_hash: &item_input_hash(item.value),
                deadline_ms: call.deadline_ms,
            })
            .await?;
        answers.push(match begun {
            Begun::New(id) => {
                running.push((index, id));
                None
            }
            Begun::Finished(StoredOutcome::Completed(body) | StoredOutcome::Failed(body)) => {
                Some(body)
            }
            Begun::Uncertain(id) => Some(uncertain(&id)),
            Begun::Resolved { id, basis } => Some(refusal(
                INTENT_RESOLVED,
                format!(
                    "an operator resolved intent {} by {basis}; send a new key",
                    id.0
                ),
                json!({"intent": id.0, "basis": basis.as_str()}),
            )),
            Begun::Conflict(id) => Some(refusal(
                IDEMPOTENCY_CONFLICT,
                format!("the key {} repeats with another input", item.key),
                json!({"field": intent.key_field.unwrap_or(REQUEST_ID), "intent": id.0}),
            )),
        });
    }
    let mut port = None;
    if !running.is_empty() {
        let batch = Value::Array(
            running
                .iter()
                .map(|(index, _)| items[*index].value.clone())
                .collect(),
        );
        let outcome = run_export(
            host,
            OperationCall {
                input: &batch,
                ..call
            },
        )
        .await?;
        match outcome {
            Err(node_types::NodeError::Cancelled) => {
                return Ok(Err(node_types::NodeError::Cancelled));
            }
            Err(error) => {
                let body = failed_body(&error);
                for (index, id) in &running {
                    intent
                        .store
                        .finish(id, &StoredOutcome::Failed(body.clone()))
                        .await?;
                    answers[*index] = Some(body.clone());
                }
                // A call that ran every item answers as an unlogged call does.
                if running.len() == items.len() {
                    return Ok(Err(error));
                }
            }
            Ok(emission) => {
                let results: Vec<Map<String, Value>> = serde_json::from_str(&emission.payload)
                    .context("a logged call emitted no item list")?;
                let mut by_request: HashMap<String, Value> = results
                    .into_iter()
                    .filter_map(|mut result| {
                        let request_id = result.remove(REQUEST_ID)?.as_str()?.to_owned();
                        Some((request_id, Value::Object(result)))
                    })
                    .collect();
                for (index, id) in &running {
                    // An item the export answered nothing for stays uncertain.
                    let Some(body) = by_request.remove(items[*index].request_id) else {
                        answers[*index] = Some(uncertain(id));
                        continue;
                    };
                    let outcome = if body.get("error").is_some() {
                        StoredOutcome::Failed(body.clone())
                    } else {
                        StoredOutcome::Completed(body.clone())
                    };
                    intent.store.finish(id, &outcome).await?;
                    answers[*index] = Some(body);
                }
                port = emission.port;
            }
        }
    }
    let merged = items
        .iter()
        .zip(answers)
        .map(|(item, body)| {
            let mut answer = Map::new();
            answer.insert(REQUEST_ID.to_owned(), Value::from(item.request_id));
            if let Some(Value::Object(body)) = body {
                answer.extend(body);
            }
            Value::Object(answer)
        })
        .collect();
    Ok(Ok(node_types::Emission {
        payload: serde_json::to_string(&Value::Array(merged))?,
        port,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::OperationKind;

    use super::{item_input_hash, items, logs_intent};

    #[test]
    fn only_a_kind_that_changes_records_logs() {
        let logged: Vec<_> = [
            OperationKind::Get,
            OperationKind::Query,
            OperationKind::Create,
            OperationKind::Update,
            OperationKind::Delete,
            OperationKind::Command,
            OperationKind::Projection,
            OperationKind::EventHandler,
        ]
        .into_iter()
        .filter(|kind| logs_intent(*kind))
        .collect();
        assert_eq!(
            logged,
            [
                OperationKind::Create,
                OperationKind::Update,
                OperationKind::Delete,
                OperationKind::Command
            ]
        );
    }

    #[test]
    fn the_key_is_the_named_field_or_else_the_request_id() {
        let input = json!([
            {"request_id": "r-1", "value": {"idempotency_key": "k-1"}},
            {"request_id": "r-2", "value": {"idempotency_key": "k-2"}},
        ]);
        let keyed = items(&input, Some("value.idempotency_key")).expect("an envelope");
        assert_eq!(
            keyed.iter().map(|item| item.key).collect::<Vec<_>>(),
            ["k-1", "k-2"]
        );
        let by_request = items(&input, None).expect("an envelope");
        assert_eq!(
            by_request.iter().map(|item| item.key).collect::<Vec<_>>(),
            ["r-1", "r-2"]
        );
    }

    #[test]
    fn an_input_that_is_not_the_item_envelope_is_refused() {
        for (input, field) in [
            (json!({"request_id": "r-1"}), None),
            (json!([{"value": 1}]), None),
            (
                json!([{"request_id": "r-1"}]),
                Some("value.idempotency_key"),
            ),
            (json!([{"request_id": "r-1"}, {"request_id": "r-1"}]), None),
        ] {
            assert!(items(&input, field).is_err(), "{input}");
        }
    }

    #[test]
    fn a_new_request_id_under_one_key_is_the_same_input() {
        assert_eq!(
            item_input_hash(&json!({"request_id": "r-1", "value": {"idempotency_key": "k"}})),
            item_input_hash(&json!({"request_id": "r-2", "value": {"idempotency_key": "k"}}))
        );
        assert_ne!(
            item_input_hash(&json!({"request_id": "r-1", "value": 1})),
            item_input_hash(&json!({"request_id": "r-1", "value": 2}))
        );
    }
}
