//! The intent rules of one route call (`docs/history/edge.md` 4.7).
//!
//! A call of a kind that changes records logs one intent for each input request element
//! before its export runs. The key is the request element field that the route names
//! for its idempotency key, or else the request element's `request_id`. Only new
//! request elements run, each alone, as a one request element list
//! (`docs/history/host-transaction.md` section 2): a request element whose key finished
//! answers its stored outcome, a request element whose key began and never finished
//! answers `intent-uncertain`, and a request element whose key an operator resolved
//! answers `intent-resolved` with the operator's basis. The route's canonical form
//! applies to each request element before its hash and before its export gets it, so
//! two spellings of one request are one request. A refused request element releases its
//! key. The answers merge into one request element list in the order of the input.

use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Map, Value, json};
use tracing::Instrument as _;
use wamn_catalog::{OperationType, RouteCanonicalization};
use wamn_run_state::IntentStore;
use wamn_run_state::intent_store::{Begun, Intent, IntentId, StoredOutcome};

use super::invocation_policy::{CommitFailure, InvocationPolicy as _, Transaction};
use super::{
    ApplicationHost, OperationCall, application_of, component_id_of, node_types, run_export,
};

/// The request element error of a key that began and never finished.
pub const INTENT_UNCERTAIN: &str = "intent-uncertain";
/// The request element error of a key that an operator resolved.
pub const INTENT_RESOLVED: &str = "intent-resolved";
/// The generated error literal of a key repeated with another input.
pub const IDEMPOTENCY_CONFLICT: &str = "idempotency_conflict";
/// The generated error literal of an input that is not the request element envelope.
const INVALID_INPUT: &str = "invalid_input";
/// The request element field that correlates a request element with its result.
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
    pub kind: OperationType,
    /// The request element field of the idempotency key, such as
    /// `value.idempotency_key`. `None` keys each request element by its `request_id`.
    pub key_field: Option<&'a str>,
    /// The canonical form of a request element, from the route. `None` takes each
    /// request element as it is sent.
    pub canonicalization: Option<&'a RouteCanonicalization>,
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
            .field("canonicalization", &self.canonicalization)
            .finish_non_exhaustive()
    }
}

/// Whether a call of `kind` logs its intents. A read never does.
#[must_use]
pub fn logs_intent(kind: OperationType) -> bool {
    matches!(
        kind,
        OperationType::Create
            | OperationType::Update
            | OperationType::Delete
            | OperationType::Command
    )
}

/// One input request element and the key of its intent.
struct RequestElement<'v> {
    request_id: &'v str,
    key: &'v str,
    value: &'v Value,
}

/// Read the request element envelope, or say why the input is not one.
fn request_elements<'v>(
    input: &'v Value,
    key_field: Option<&str>,
) -> Result<Vec<RequestElement<'v>>, String> {
    let request_elements = input
        .as_array()
        .ok_or("the input is not a request element list")?;
    let mut keys = HashSet::with_capacity(request_elements.len());
    request_elements
        .iter()
        .map(|value| {
            let request_id = value
                .get(REQUEST_ID)
                .and_then(Value::as_str)
                .ok_or("a request element has no request_id")?;
            let key = match key_field {
                Some(field) => field
                    .split('.')
                    .try_fold(value, |value, name| value.get(name))
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("request element {request_id} has no {field}"))?,
                None => request_id,
            };
            if !keys.insert(key) {
                return Err(format!("the key {key} repeats within one call"));
            }
            Ok(RequestElement {
                request_id,
                key,
                value,
            })
        })
        .collect()
}

/// The request hash of one request element's intent: the canonical JSON SHA-256 of the
/// request element without its `request_id`, with the key kept. A retry under one
/// idempotency key with a new request id is the same request.
#[must_use]
pub fn request_element_hash(request_element: &Value) -> String {
    let mut body = request_element.clone();
    if let Some(object) = body.as_object_mut() {
        object.remove(REQUEST_ID);
    }
    wamn_execution_contract::canonical_json_sha256(&body)
}

/// A request element body that answers with an error and runs nothing.
fn refusal(code: &str, message: String, detail: Value) -> Value {
    let error = Map::from_iter([
        ("code".to_owned(), Value::from(code)),
        ("message".to_owned(), Value::String(message)),
        ("detail".to_owned(), detail),
    ]);
    Value::Object(Map::from_iter([("error".to_owned(), Value::Object(error))]))
}

/// The request element body of an error that failed the whole call.
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

/// Log the intents of one call, run each new request element alone, and merge every
/// answer.
///
/// A trap, a missed deadline or a host failure leaves the running request element's
/// intent begun, so it is uncertain, because the export may have changed
/// records. A request element not yet reached begins no intent.
pub(super) async fn invoke_logged<H: ApplicationHost>(
    host: &H,
    call: OperationCall<'_, H::Policy>,
    intent: IntentContext<'_>,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    let canonical;
    let input = match intent.canonicalization {
        Some(form) => {
            let mut input = call.input.clone();
            if let Some(request_elements) = input.as_array_mut() {
                for request_element in request_elements {
                    form.apply(request_element);
                }
            }
            canonical = input;
            &canonical
        }
        None => call.input,
    };
    let request_elements = match request_elements(input, intent.key_field) {
        Ok(request_elements) => request_elements,
        Err(message) => {
            return Ok(Err(node_types::NodeError::InvalidInput(
                node_types::ErrorDetail {
                    message,
                    code: Some(INVALID_INPUT.to_owned()),
                },
            )));
        }
    };
    let application = application_of(host, call.closure).await?;
    let component_id = component_id_of(&application, call.component)?;
    // The request elements share the deadline of the call.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(call.deadline_ms);
    let mut answers = Vec::with_capacity(request_elements.len());
    let mut port = None;
    for request_element in &request_elements {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        // A request element not reached before the deadline begins no intent.
        anyhow::ensure!(
            !remaining.is_zero(),
            "native node enclosing deadline elapsed"
        );
        let deadline_ms = u64::try_from(remaining.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        // The request element's host transaction begins before its claim, so an operation
        // with SQL claims in it (`docs/history/host-transaction.md` 4.2).
        let transaction = application
            .policy
            .admit_and_open_transaction(component_id, call.operation, &call.facts)
            .instrument(tracing::info_span!(
                "wamn.operation.admit_and_open_transaction"
            ))
            .await?;
        let facts = match &transaction {
            Some(transaction) => application
                .policy
                .transaction_facts(&call.facts, transaction),
            None => call.facts.clone(),
        };
        let request_element_call = OperationCall {
            closure: call.closure,
            component: call.component,
            operation: call.operation,
            context: node_types::NodeContext {
                deadline_ms: Some(deadline_ms),
                ..call.context.clone()
            },
            input,
            deadline_ms,
            facts,
        };
        // An operation with SQL claims in its request element transaction.
        let store = transaction
            .as_ref()
            .and_then(Transaction::intent_store)
            .unwrap_or(intent.store);
        let settled = settle_request_element(
            host,
            request_element_call,
            IntentContext { store, ..intent },
            request_element,
            transaction.is_some(),
        )
        .await?;
        let (body, id, commit) = match settled {
            Settled::Cancelled | Settled::Failed(_) => {
                if let Some(transaction) = &transaction {
                    transaction.rollback().await?;
                }
                return Ok(Err(match settled {
                    Settled::Failed(error) => error,
                    _ => node_types::NodeError::Cancelled,
                }));
            }
            Settled::Answer {
                body,
                id,
                commit,
                port: request_element_port,
            } => {
                if request_element_port.is_some() {
                    port = request_element_port;
                }
                (body, id, commit)
            }
        };
        let Some(transaction) = transaction else {
            answers.push(body);
            continue;
        };
        if !commit {
            transaction.rollback().await?;
            answers.push(body);
            continue;
        }
        answers.push(
            match transaction
                .commit()
                .instrument(tracing::info_span!("wamn.operation.commit"))
                .await
            {
                Ok(()) => body,
                Err(CommitFailure::Uncertain { .. }) => {
                    uncertain(&id.context("a committed request element has an intent")?)
                }
                Err(CommitFailure::RolledBack { code, message }) => {
                    json!({"error": {"code": code, "message": message}})
                }
            },
        );
    }
    let merged = request_elements
        .iter()
        .zip(answers)
        .map(|(request_element, body)| {
            let mut answer = Map::new();
            answer.insert(
                REQUEST_ID.to_owned(),
                Value::from(request_element.request_id),
            );
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

/// How one request element of a logged call settled.
enum Settled {
    /// The request element's answer. `commit` says whether its host transaction commits.
    Answer {
        body: Value,
        /// The intent that a commit with an unknown outcome leaves uncertain.
        id: Option<IntentId>,
        commit: bool,
        port: Option<String>,
    },
    /// The call was cancelled.
    Cancelled,
    /// The one request element of a one request element call failed, which answers as
    /// an unlogged call does.
    Failed(node_types::NodeError),
}

/// Claim one request element's key, run the request element when the key is new, and
/// record its outcome. `call.input` is the whole call's input, and the guest gets the
/// request element alone.
async fn settle_request_element<H: ApplicationHost>(
    host: &H,
    call: OperationCall<'_, H::Policy>,
    intent: IntentContext<'_>,
    request_element: &RequestElement<'_>,
    transacted: bool,
) -> anyhow::Result<Settled> {
    let answer = |body| Settled::Answer {
        body,
        id: None,
        commit: false,
        port: None,
    };
    let hashed = intent
        .canonicalization
        .map(|form| form.hashed(request_element.value));
    let begun = intent
        .store
        .begin(&Intent {
            tenant: intent.tenant,
            release: intent.release,
            package: intent.package,
            operation: call.operation,
            idempotency_key: request_element.key,
            input_hash: &request_element_hash(hashed.as_ref().unwrap_or(request_element.value)),
            deadline_ms: call.deadline_ms,
        })
        .instrument(tracing::info_span!("wamn.write_log.claim"))
        .await?;
    let id = match begun {
        Begun::New(id) => id,
        Begun::Finished(StoredOutcome(body)) => return Ok(answer(body)),
        Begun::Uncertain(id) => return Ok(answer(uncertain(&id))),
        Begun::Resolved { id, basis } => {
            return Ok(answer(refusal(
                INTENT_RESOLVED,
                format!(
                    "an operator resolved intent {} by {basis}; send a new key",
                    id.0
                ),
                json!({"intent": id.0, "basis": basis.as_str()}),
            )));
        }
        Begun::Conflict(id) => {
            return Ok(answer(refusal(
                IDEMPOTENCY_CONFLICT,
                format!("the key {} repeats with another input", request_element.key),
                json!({"field": intent.key_field.unwrap_or(REQUEST_ID), "intent": id.0}),
            )));
        }
    };
    let whole = call.input.as_array().map_or(0, Vec::len);
    let one = Value::Array(vec![request_element.value.clone()]);
    let outcome = run_export(
        host,
        OperationCall {
            input: &one,
            ..call
        },
    )
    .await?;
    let emission = match outcome {
        Err(node_types::NodeError::Cancelled) => return Ok(Settled::Cancelled),
        Err(error) => {
            intent.store.release(&id).await?;
            if whole == 1 {
                return Ok(Settled::Failed(error));
            }
            return Ok(answer(failed_body(&error)));
        }
        Ok(emission) => emission,
    };
    let results: Vec<Map<String, Value>> = serde_json::from_str(&emission.payload)
        .context("a logged call emitted no request element list")?;
    let body = results.into_iter().find_map(|mut result| {
        let request_id = result.remove(REQUEST_ID)?;
        (request_id.as_str() == Some(request_element.request_id)).then_some(Value::Object(result))
    });
    let Some(body) = body else {
        // Without a host transaction, the export may have changed records, so
        // the request element stays uncertain. With one, the rollback undoes the work.
        if !transacted {
            return Ok(answer(uncertain(&id)));
        }
        intent.store.release(&id).await?;
        return Ok(answer(refusal(
            "internal_error",
            format!(
                "the export answered nothing for request element {}",
                request_element.request_id
            ),
            Value::Null,
        )));
    };
    if body.get("error").is_some() {
        intent.store.release(&id).await?;
        return Ok(answer(body));
    }
    intent
        .store
        .finish(&id, &StoredOutcome(body.clone()))
        .instrument(tracing::info_span!("wamn.write_log.finish"))
        .await?;
    Ok(Settled::Answer {
        body,
        id: Some(id),
        commit: true,
        port: emission.port,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::json;
    use wamn_catalog::{CanonicalSpelling, OperationType, RouteCanonicalization, RouteLineOrder};

    use super::{logs_intent, request_element_hash, request_elements};

    #[test]
    fn only_a_kind_that_changes_records_logs() {
        let logged: Vec<_> = [
            OperationType::Get,
            OperationType::Query,
            OperationType::Create,
            OperationType::Update,
            OperationType::Delete,
            OperationType::Command,
            OperationType::Projection,
            OperationType::EventHandler,
        ]
        .into_iter()
        .filter(|kind| logs_intent(*kind))
        .collect();
        assert_eq!(
            logged,
            [
                OperationType::Create,
                OperationType::Update,
                OperationType::Delete,
                OperationType::Command
            ]
        );
    }

    #[test]
    fn the_key_is_the_named_field_or_else_the_request_id() {
        let input = json!([
            {"request_id": "r-1", "value": {"idempotency_key": "k-1"}},
            {"request_id": "r-2", "value": {"idempotency_key": "k-2"}},
        ]);
        let keyed = request_elements(&input, Some("value.idempotency_key")).expect("an envelope");
        assert_eq!(
            keyed
                .iter()
                .map(|request_element| request_element.key)
                .collect::<Vec<_>>(),
            ["k-1", "k-2"]
        );
        let by_request = request_elements(&input, None).expect("an envelope");
        assert_eq!(
            by_request
                .iter()
                .map(|request_element| request_element.key)
                .collect::<Vec<_>>(),
            ["r-1", "r-2"]
        );
    }

    #[test]
    fn an_input_that_is_not_the_request_element_envelope_is_refused() {
        for (input, field) in [
            (json!({"request_id": "r-1"}), None),
            (json!([{"value": 1}]), None),
            (
                json!([{"request_id": "r-1"}]),
                Some("value.idempotency_key"),
            ),
            (json!([{"request_id": "r-1"}, {"request_id": "r-1"}]), None),
        ] {
            assert!(request_elements(&input, field).is_err(), "{input}");
        }
    }

    #[test]
    fn a_new_request_id_under_one_key_is_the_same_input() {
        assert_eq!(
            request_element_hash(&json!({"request_id": "r-1", "value": {"idempotency_key": "k"}})),
            request_element_hash(&json!({"request_id": "r-2", "value": {"idempotency_key": "k"}}))
        );
        assert_ne!(
            request_element_hash(&json!({"request_id": "r-1", "value": 1})),
            request_element_hash(&json!({"request_id": "r-1", "value": 2}))
        );
    }

    #[test]
    fn the_request_hash_keeps_the_key_and_is_the_request_element_alone() {
        let request_element =
            json!({"request_id": "r-1", "value": {"idempotency_key": "k", "n": 1}});
        assert_ne!(
            request_element_hash(&request_element),
            request_element_hash(
                &json!({"request_id": "r-1", "value": {"idempotency_key": "other", "n": 1}})
            ),
            "the key is part of the request"
        );
        assert_eq!(
            request_element_hash(&request_element),
            wamn_execution_contract::canonical_json_sha256(&json!({
                "value": {"idempotency_key": "k", "n": 1}
            }))
        );
    }

    /// The canonical form of a command with lines, as publish derives it from
    /// the fixture's `widget.record_batch` contract.
    fn record_batch() -> RouteCanonicalization {
        RouteCanonicalization {
            excluded: BTreeSet::from(["value.note".to_owned()]),
            lines: Some(RouteLineOrder {
                path: "value.line".to_owned(),
                ascending_by: "widget_id".to_owned(),
            }),
            spelling: BTreeMap::from([
                ("value.line[].amount".to_owned(), CanonicalSpelling::Numeric),
                (
                    "value.occurred_at".to_owned(),
                    CanonicalSpelling::Timestamptz,
                ),
            ]),
        }
    }

    fn canonical_hash(
        form: &RouteCanonicalization,
        mut request_element: serde_json::Value,
    ) -> String {
        form.apply(&mut request_element);
        request_element_hash(&form.hashed(&request_element))
    }

    #[test]
    fn reordered_lines_and_other_spellings_are_one_request() {
        let form = record_batch();
        let sent = json!({"request_id": "r-1", "value": {
            "idempotency_key": "k",
            "note": "first",
            "occurred_at": "2026-09-29T12:00:00Z",
            "line": [
                {"widget_id": "b", "amount": "2"},
                {"widget_id": "a", "amount": "1.50"},
            ],
        }});
        let retried = json!({"request_id": "r-2", "value": {
            "idempotency_key": "k",
            "note": "an excluded field",
            "occurred_at": "2026-09-29T14:00:00.000000+02:00",
            "line": [
                {"widget_id": "a", "amount": "01.50"},
                {"widget_id": "b", "amount": "2."},
            ],
        }});
        assert_eq!(
            canonical_hash(&form, sent.clone()),
            canonical_hash(&form, retried)
        );
        let changed = json!({"request_id": "r-2", "value": {
            "idempotency_key": "k",
            "note": "first",
            "occurred_at": "2026-09-29T12:00:00Z",
            "line": [
                {"widget_id": "a", "amount": "1.5"},
                {"widget_id": "b", "amount": "2"},
            ],
        }});
        assert_ne!(
            canonical_hash(&form, sent.clone()),
            canonical_hash(&form, changed),
            "a numeric keeps its scale"
        );
        // The component gets the canonical request element, excluded fields kept.
        let mut request_element = sent;
        form.apply(&mut request_element);
        assert_eq!(
            request_element["value"],
            json!({
                "idempotency_key": "k",
                "note": "first",
                "occurred_at": "2026-09-29T12:00:00.000000Z",
                "line": [
                    {"widget_id": "a", "amount": "1.50"},
                    {"widget_id": "b", "amount": "2"},
                ],
            })
        );
    }
}
