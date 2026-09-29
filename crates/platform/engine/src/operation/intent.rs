//! The intent rules of one route call (`docs/plan/edge.md` 4.7).
//!
//! A call of a kind that changes records logs one intent for each input item
//! before its export runs. The key is the item field that the route names for
//! its idempotency key, or else the item's `request_id`. Only new items run,
//! each alone, as a one-item list (`docs/plan/host-transaction.md` section 2):
//! an item whose key finished answers its stored outcome, an item whose key
//! began and never finished answers `intent-uncertain`, and an item whose key
//! an operator resolved answers `intent-resolved` with the operator's basis.
//! A refused item releases its key. The answers merge into one item list in
//! the order of the input.

use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

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
/// The member that carries the participation intent in the request hash.
const PARTICIPATION_INTENT: &str = "participation_intent";

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
    /// The intent of the pre-commit participant that the call selects, if
    /// any. It is part of the request hash, so one key under two intents is
    /// two requests.
    pub participation: Option<&'a str>,
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
            .field("participation", &self.participation)
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

/// The request hash of one item's intent: the canonical JSON SHA-256 of the
/// item without its `request_id`, with the key kept, and with the selected
/// participation intent added. A retry under one idempotency key with a new
/// request id is the same request.
#[must_use]
pub fn item_input_hash(item: &Value, participation: Option<&str>) -> String {
    let mut body = item.clone();
    if let Some(object) = body.as_object_mut() {
        object.remove(REQUEST_ID);
        if let Some(intent) = participation {
            object.insert(PARTICIPATION_INTENT.to_owned(), Value::from(intent));
        }
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

/// Log the intents of one call, run each new item alone, and merge every
/// answer.
///
/// A trap, a missed deadline or a host failure leaves the running item's
/// intent begun, so it is uncertain, because the export may have changed
/// records. An item not yet reached begins no intent.
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
    // The items share the deadline of the call.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(call.deadline_ms);
    let mut answers = Vec::with_capacity(items.len());
    let mut port = None;
    for item in &items {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        // An item not reached before the deadline begins no intent.
        anyhow::ensure!(
            !remaining.is_zero(),
            "native node enclosing deadline elapsed"
        );
        let deadline_ms = u64::try_from(remaining.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let begun = intent
            .store
            .begin(&Intent {
                tenant: intent.tenant,
                release: intent.release,
                package: intent.package,
                operation: call.operation,
                idempotency_key: item.key,
                input_hash: &item_input_hash(item.value, intent.participation),
                deadline_ms,
            })
            .await?;
        let id = match begun {
            Begun::New(id) => id,
            Begun::Finished(StoredOutcome(body)) => {
                answers.push(body);
                continue;
            }
            Begun::Uncertain(id) => {
                answers.push(uncertain(&id));
                continue;
            }
            Begun::Resolved { id, basis } => {
                answers.push(refusal(
                    INTENT_RESOLVED,
                    format!(
                        "an operator resolved intent {} by {basis}; send a new key",
                        id.0
                    ),
                    json!({"intent": id.0, "basis": basis.as_str()}),
                ));
                continue;
            }
            Begun::Conflict(id) => {
                answers.push(refusal(
                    IDEMPOTENCY_CONFLICT,
                    format!("the key {} repeats with another input", item.key),
                    json!({"field": intent.key_field.unwrap_or(REQUEST_ID), "intent": id.0}),
                ));
                continue;
            }
        };
        let one = Value::Array(vec![item.value.clone()]);
        let outcome = run_export(
            host,
            OperationCall {
                closure: call.closure,
                component: call.component,
                operation: call.operation,
                context: node_types::NodeContext {
                    deadline_ms: Some(deadline_ms),
                    ..call.context.clone()
                },
                input: &one,
                deadline_ms,
                facts: call.facts.clone(),
            },
        )
        .await?;
        match outcome {
            Err(node_types::NodeError::Cancelled) => {
                return Ok(Err(node_types::NodeError::Cancelled));
            }
            Err(error) => {
                intent.store.release(&id).await?;
                // A call of one item answers as an unlogged call does.
                if items.len() == 1 {
                    return Ok(Err(error));
                }
                answers.push(failed_body(&error));
            }
            Ok(emission) => {
                let results: Vec<Map<String, Value>> = serde_json::from_str(&emission.payload)
                    .context("a logged call emitted no item list")?;
                let body = results.into_iter().find_map(|mut result| {
                    let request_id = result.remove(REQUEST_ID)?;
                    (request_id.as_str() == Some(item.request_id)).then_some(Value::Object(result))
                });
                // An item the export answered nothing for stays uncertain.
                let Some(body) = body else {
                    answers.push(uncertain(&id));
                    continue;
                };
                if body.get("error").is_some() {
                    intent.store.release(&id).await?;
                } else {
                    intent
                        .store
                        .finish(&id, &StoredOutcome(body.clone()))
                        .await?;
                }
                answers.push(body);
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
            if let Value::Object(body) = body {
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
            item_input_hash(
                &json!({"request_id": "r-1", "value": {"idempotency_key": "k"}}),
                None
            ),
            item_input_hash(
                &json!({"request_id": "r-2", "value": {"idempotency_key": "k"}}),
                None
            )
        );
        assert_ne!(
            item_input_hash(&json!({"request_id": "r-1", "value": 1}), None),
            item_input_hash(&json!({"request_id": "r-1", "value": 2}), None)
        );
    }

    #[test]
    fn the_request_hash_keeps_the_key_and_adds_the_participation_intent() {
        let item = json!({"request_id": "r-1", "value": {"idempotency_key": "k", "n": 1}});
        assert_ne!(
            item_input_hash(&item, None),
            item_input_hash(
                &json!({"request_id": "r-1", "value": {"idempotency_key": "other", "n": 1}}),
                None
            ),
            "the key is part of the request"
        );
        assert_ne!(
            item_input_hash(&item, Some("ship")),
            item_input_hash(&item, Some("hold")),
            "one key under two intents is two requests"
        );
        assert_eq!(
            item_input_hash(&item, Some("ship")),
            wamn_execution_contract::canonical_json_sha256(&json!({
                "value": {"idempotency_key": "k", "n": 1},
                "participation_intent": "ship"
            }))
        );
    }
}
