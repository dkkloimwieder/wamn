//! Calls the typed application through host invocation authority.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::entry_response::{boundary, with_request_id};

use super::bindings::wamn::flow_http_routing::routing::AuthenticatedCaller;
use super::bindings::wamn::node::types::{NodeContext, NodeError};
use super::bindings::wamn::operation::operation;
use crate::route_data::{EtagType, RouteDefinition};
use crate::{HttpResponse, OperationRequest, ReadCache, detailed_error_response, error_response};

pub(super) fn validate_read_limits(
    route: &RouteDefinition,
    payload: &str,
) -> Result<(), crate::SchemaInvalid> {
    let Some(limits) = &route.read_limits else {
        return Ok(());
    };
    let input: Value = serde_json::from_str(payload)
        .map_err(|_| crate::SchemaInvalid::from_refusal(String::new()))?;
    for (index, request_element) in input.as_array().into_iter().flatten().enumerate() {
        if let Some(limit) = request_element.get("limit")
            && !limit.as_u64().is_some_and(|limit| {
                limits
                    .page_minimum
                    .is_none_or(|minimum| limit >= u64::from(minimum))
                    && limits
                        .page_maximum
                        .is_none_or(|maximum| limit <= u64::from(maximum))
            })
        {
            return Err(crate::SchemaInvalid::from_refusal(format!(
                "/{index}/limit"
            )));
        }
        for band in &limits.band_starts {
            let value = request_element
                .get("filter")
                .and_then(|filter| filter.get(&band.field));
            if (band.required || value.is_some())
                && value.and_then(|value| value.get("min")).is_none()
            {
                let field = band.field.replace('~', "~0").replace('/', "~1");
                return Err(crate::SchemaInvalid::from_refusal(format!(
                    "/{index}/filter/{field}"
                )));
            }
        }
    }
    Ok(())
}

pub(super) async fn invoke(
    route: &RouteDefinition,
    request: OperationRequest<AuthenticatedCaller>,
) -> HttpResponse {
    let mut etag = None;
    let mut response = execute(route, &request, &mut etag)
        .await
        .unwrap_or_else(|response| response);
    if matches!(response.status, 200 | 304) {
        response.read = Some(Box::new(ReadCache {
            cache_control: None,
            etag,
        }));
    }
    response
}

fn list_etag(route: &RouteDefinition, position: Option<String>) -> Option<String> {
    let inputs = route.etag_inputs.as_ref()?;
    let EtagType::List(relations) = &inputs.type_ else {
        return None;
    };
    let position = position?;
    let relations = relations
        .iter()
        .map(|relation| json!([relation.schema, relation.name]))
        .collect::<Vec<_>>();
    Some(format!(
        "W/\"{}\"",
        tag_digest(&json!(["list", inputs.release, relations, position]))
    ))
}

async fn execute(
    route: &RouteDefinition,
    request: &OperationRequest<AuthenticatedCaller>,
    etag: &mut Option<String>,
) -> Result<HttpResponse, HttpResponse> {
    let deadline = super::bindings::wasi::clocks::monotonic_clock::now()
        .saturating_add(route.operation_deadline_ms.saturating_mul(1_000_000));
    let operation_id = &route.sealed_operation_id;
    let caller = request.caller.as_ref();
    let mut input: Value =
        serde_json::from_str(&request.payload).map_err(|_| error_response(400, "invalid-input"))?;
    let Some(key_field) = route.idempotency_rule.as_deref() else {
        operation::authorize(caller, operation_id.clone())
            .await
            .map_err(|error| boundary(&error, operation_id))?;
        // A change feed read takes its tag from the cursor it returns
        // (docs/plan/wamn-sync.md), so it needs no WAL position.
        let feed = crate::is_feed_read(&input);
        let result = read_call(
            operation_id,
            context(request, String::new(), remaining_ms(deadline)),
            input.to_string(),
            wants_list_etag(route) && !feed,
        )
        .await;
        match result {
            Ok((result, wal_position)) => {
                *etag = if feed {
                    changes_etag(route, &result)
                } else {
                    list_etag(route, wal_position)
                };
                operation::finish_read()
                    .await
                    .map_err(|error| boundary(&error, operation_id))?;
                if let Some(inputs) = &route.etag_inputs
                    && let EtagType::Get(field) = &inputs.type_
                    && let Some(request_elements) = result.as_array()
                    && let [request_element] = request_elements.as_slice()
                    && let Some(revision) = request_element
                        .get("value")
                        .and_then(|value| value.get(field))
                    && (revision.is_number() || revision.is_string())
                {
                    *etag = Some(format!(
                        "\"{}\"",
                        tag_digest(&json!(["get", inputs.release, revision]))
                    ));
                }
                if matches_tag(request, etag.as_deref()) {
                    return Ok(response(304, String::new()));
                }
                return Ok(response(
                    200,
                    operation::sign_result(result.to_string())
                        .await
                        .map_err(|_| error_response(503, "execution-failed"))?,
                ));
            }
            Err(error) => {
                operation::finish_read()
                    .await
                    .map_err(|error| boundary(&error, operation_id))?;
                return Ok(node_failure(error));
            }
        }
    };
    let Some(request_elements) = input.as_array_mut() else {
        return Ok(invalid("the input is not a request element list"));
    };
    let mut keys = HashSet::new();
    for request_element in request_elements.iter_mut() {
        let Some(key) = field(request_element, key_field).and_then(Value::as_str) else {
            return Ok(invalid(&format!("a request element has no {key_field}")));
        };
        if request_element
            .get("request_id")
            .and_then(Value::as_str)
            .is_none()
            || !keys.insert(key.to_owned())
        {
            return Ok(invalid(
                "a request element has no request_id or repeats its idempotency key",
            ));
        }
    }
    let mut answers = Vec::with_capacity(request_elements.len());
    for request_element in request_elements.iter() {
        let request_id = request_element["request_id"]
            .as_str()
            .expect("request id validated");
        if remaining_ms(deadline) == 0 {
            answers.push(
                json!({"request_id": request_id, "error": {
                    "code": "timeout", "message": "not reached before deadline"
                }})
                .to_string(),
            );
            continue;
        }
        let key = field(request_element, key_field)
            .and_then(Value::as_str)
            .expect("key validated");
        let mut hashed = request_element.clone();
        remove(&mut hashed, &["request_id"]);
        remove(&mut hashed, &key_field.split('.').collect::<Vec<_>>());
        let hash = wamn_digest::canonical_json_sha256(&hashed).into_bytes();
        let begun =
            operation::begin_operation(caller, operation_id.clone(), key.to_owned(), hash).await;
        let intent = format!(
            "{}#{key}",
            operation_id.split('@').next().unwrap_or(operation_id)
        );
        let body = match begun {
            Ok(operation::Begun::Finished(body)) => {
                answers.push(signed_answer(body, request_id).await?);
                continue;
            }
            Err(operation::Refusal::Conflict) => refusal(
                "idempotency_conflict",
                &format!("the key {key} repeats with another input"),
                &json!({"field": key_field, "intent": intent}),
            ),
            Err(error @ (operation::Refusal::InProgress | operation::Refusal::OutcomeUnknown)) => {
                commit_failure(error)
            }
            Err(error) => return Err(boundary(&error, operation_id)),
            Ok(operation::Begun::New(delivery_id)) => {
                let result = call(
                    operation_id,
                    context(request, delivery_id, remaining_ms(deadline)),
                    json!([request_element]).to_string(),
                )
                .await;
                let result = match result {
                    Ok(Value::Array(mut results))
                        if results.len() == 1 && results[0]["request_id"] == request_id =>
                    {
                        let mut result = results.remove(0);
                        result
                            .as_object_mut()
                            .ok_or_else(|| error_response(503, "execution-failed"))?
                            .remove("request_id");
                        result
                    }
                    Ok(_) => {
                        operation::rollback()
                            .await
                            .map_err(|error| boundary(&error, operation_id))?;
                        answers.push(json!({"request_id": request_id, "error": {
                            "code": "internal_error",
                            "message": format!("the export answered nothing for request element {request_id}"),
                            "detail": null,
                        }}).to_string());
                        continue;
                    }
                    Err(error) => {
                        operation::rollback()
                            .await
                            .map_err(|error| boundary(&error, operation_id))?;
                        if request_elements.len() == 1 {
                            return Ok(node_failure(error));
                        }
                        let detail = match error {
                            NodeError::Terminal(detail)
                            | NodeError::Retryable(detail)
                            | NodeError::InvalidInput(detail) => detail,
                            NodeError::RateLimited(limited) => limited.detail,
                            NodeError::Cancelled => {
                                return Ok(error_response(503, "execution-cancelled"));
                            }
                        };
                        answers.push(json!({"request_id": request_id, "error": {"code": detail.code, "message": detail.message}}).to_string());
                        continue;
                    }
                };
                if result.get("value").is_some() {
                    if let Err(error) = operation::commit(result.to_string()).await {
                        commit_failure(error)
                    } else {
                        result
                    }
                } else {
                    operation::rollback()
                        .await
                        .map_err(|error| boundary(&error, operation_id))?;
                    result
                }
            }
        };
        answers.push(signed_answer(body.to_string(), request_id).await?);
    }
    Ok(response(200, format!("[{}]", answers.join(","))))
}

async fn signed_answer(body: String, request_id: &str) -> Result<String, HttpResponse> {
    let body = with_request_id(body, request_id)?;
    let value: Value =
        serde_json::from_str(&body).map_err(|_| error_response(503, "execution-failed"))?;
    if value.get("value").is_none() {
        return Ok(body);
    }
    let signed = operation::sign_result(format!("[{body}]"))
        .await
        .map_err(|_| error_response(503, "execution-failed"))?;
    let mut values: Vec<Value> =
        serde_json::from_str(&signed).map_err(|_| error_response(503, "execution-failed"))?;
    if values.len() != 1 {
        return Err(error_response(503, "execution-failed"));
    }
    Ok(values.remove(0).to_string())
}

fn commit_failure(error: operation::Refusal) -> Value {
    use super::bindings::wamn::postgres::types::PgError;
    match error {
        operation::Refusal::InProgress => refusal(
            "in-progress",
            "the operation is still running",
            &serde_json::Value::Null,
        ),
        operation::Refusal::OutcomeUnknown => refusal(
            "outcome-unknown",
            "check the external system before sending a new key",
            &serde_json::Value::Null,
        ),
        operation::Refusal::Postgres(error) => {
            let code = match error {
                PgError::StatementTimeout => "timeout",
                PgError::SerializationFailure | PgError::ConnectionUnavailable => "retry",
                _ => "internal_error",
            };
            json!({"error": {"code": code, "message": format!("the commit rolled back: {error:?}")}})
        }
        _ => json!({"error": {"code": "internal_error"}}),
    }
}

fn remaining_ms(deadline: u64) -> u64 {
    deadline
        .saturating_sub(super::bindings::wasi::clocks::monotonic_clock::now())
        .div_ceil(1_000_000)
}

fn context(
    request: &OperationRequest<AuthenticatedCaller>,
    delivery_id: String,
    deadline_ms: u64,
) -> NodeContext {
    NodeContext {
        wiring_id: String::new(),
        wiring_version: 0,
        node_id: String::new(),
        delivery_id,
        input_port: None,
        occurrence: 0,
        traceparent: request
            .trace
            .as_ref()
            .map(|trace| trace.traceparent.clone()),
        tracestate: request
            .trace
            .as_ref()
            .and_then(|trace| trace.tracestate.clone()),
        deadline_ms: Some(deadline_ms),
        config: "null".into(),
    }
}

async fn call(id: &str, context: NodeContext, input: String) -> Result<Value, NodeError> {
    read_call(id, context, input, false)
        .await
        .map(|(value, _)| value)
}

/// The weak ETag of a change feed read, from the cursor it returns. An
/// unchanged feed returns the cursor it was given, so its tag holds.
fn changes_etag(route: &RouteDefinition, result: &Value) -> Option<String> {
    let inputs = route.etag_inputs.as_ref()?;
    let [outcome] = result.as_array()?.as_slice() else {
        return None;
    };
    let cursor = outcome.get("value")?.get("next_cursor")?.as_str()?;
    Some(format!(
        "W/\"{}\"",
        tag_digest(&json!(["changes", inputs.release, cursor]))
    ))
}

fn wants_list_etag(route: &RouteDefinition) -> bool {
    route
        .etag_inputs
        .as_ref()
        .is_some_and(|inputs| matches!(inputs.type_, EtagType::List(_)))
}

async fn read_call(
    id: &str,
    context: NodeContext,
    input: String,
    want_wal: bool,
) -> Result<(Value, Option<String>), NodeError> {
    let decode = |text: &str| {
        serde_json::from_str(text).map_err(|_| {
            NodeError::Terminal(super::bindings::wamn::node::types::ErrorDetail {
                code: Some("internal_error".into()),
                message: "operation returned invalid JSON".into(),
            })
        })
    };
    let emission = operation::invoke(id.to_owned(), context, input).await?;
    let position = if want_wal {
        operation::wal_position().await.ok()
    } else {
        None
    };
    decode(&emission.payload).map(|value| (value, position))
}

fn invalid(message: &str) -> HttpResponse {
    detailed_error_response(400, "invalid_input", Some(message), None)
}

fn node_failure(error: NodeError) -> HttpResponse {
    let (status, code, detail) = match error {
        NodeError::InvalidInput(detail) => (400, "invalid-input", detail),
        NodeError::Terminal(detail) => (500, "terminal", detail),
        NodeError::Retryable(detail) => (500, "retry-exhausted", detail),
        NodeError::RateLimited(limited) => (500, "retry-exhausted", limited.detail),
        NodeError::Cancelled => return error_response(503, "execution-cancelled"),
    };
    detailed_error_response(
        status,
        detail.code.as_deref().unwrap_or(code),
        Some(&detail.message),
        None,
    )
}

fn response(status: u16, body: String) -> HttpResponse {
    HttpResponse {
        status,
        content_type: "application/json",
        body: body.into_bytes(),
        read: None,
    }
}

fn tag_digest(value: &Value) -> String {
    wamn_digest::canonical_json_sha256(value)[7..39].to_owned()
}

fn matches_tag(request: &OperationRequest<AuthenticatedCaller>, tag: Option<&str>) -> bool {
    let (Some(value), Some(tag)) = (&request.if_none_match, tag) else {
        return false;
    };
    value.split(',').any(|value| {
        value.trim() == "*" || value.trim().trim_start_matches("W/") == tag.trim_start_matches("W/")
    })
}

fn field<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |value, key| value.get(key))
}

fn remove(value: &mut Value, path: &[&str]) {
    match path {
        [] => {}
        [last] => {
            if let Some(object) = value.as_object_mut() {
                object.remove(*last);
            }
        }
        [first, rest @ ..] => {
            if let Some(name) = first.strip_suffix("[]") {
                if let Some(Value::Array(request_elements)) = value.get_mut(name) {
                    for request_element in request_elements {
                        remove(request_element, rest);
                    }
                }
            } else if let Some(value) = value.get_mut(*first) {
                remove(value, rest);
            }
        }
    }
}

fn refusal(code: &str, message: &str, detail: &Value) -> Value {
    json!({"error": {"code": code, "message": message, "detail": detail}})
}
