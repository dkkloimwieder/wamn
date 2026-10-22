//! Calls the application inside the composed component.

use std::collections::HashSet;

use serde_json::{Value, json};
use wit_bindgen::rt::async_support::{StreamReader, StreamResult, spawn_local};

use crate::entry_response::{refusal, resolved, uncertain, with_request_id};

use super::bindings::wamn::flow_http_routing::routing::{
    AuthenticatedCaller, CanonicalSpelling, EtagKind, RequestHashRule, RouteDefinition,
};
use super::bindings::wamn::node::types::{NodeContext, NodeError};
use super::bindings::wamn::{application_dispatch::dispatch, operation::operation};
use super::{
    DeliveryError, DeliveryFailure, DeliveryFailureKind, DeliveryOutcome, DeliveryReport,
    DeliveryRequest,
};

pub(super) fn validate_read_limits(
    route: &RouteDefinition,
    payload: &str,
) -> Result<(), super::SchemaInvalid> {
    let Some(limits) = &route.read_limits else {
        return Ok(());
    };
    let input: Value = serde_json::from_str(payload)
        .map_err(|_| super::SchemaInvalid::from_refusal(String::new()))?;
    for (index, item) in input.as_array().into_iter().flatten().enumerate() {
        if let Some(limit) = item.get("limit")
            && !limit.as_u64().is_some_and(|limit| {
                limits
                    .page_minimum
                    .is_none_or(|minimum| limit >= u64::from(minimum))
                    && limits
                        .page_maximum
                        .is_none_or(|maximum| limit <= u64::from(maximum))
            })
        {
            return Err(super::SchemaInvalid::from_refusal(format!(
                "/{index}/limit"
            )));
        }
        for band in &limits.band_starts {
            let value = item
                .get("filter")
                .and_then(|filter| filter.get(&band.field));
            if (band.required || value.is_some())
                && value.and_then(|value| value.get("min")).is_none()
            {
                let field = band.field.replace('~', "~0").replace('/', "~1");
                return Err(super::SchemaInvalid::from_refusal(format!(
                    "/{index}/filter/{field}"
                )));
            }
        }
    }
    Ok(())
}

pub(super) async fn deliver(
    route: &RouteDefinition,
    request: DeliveryRequest<AuthenticatedCaller>,
) -> DeliveryReport {
    let mut etag = None;
    let outcome = execute(route, &request, &mut etag).await;
    DeliveryReport {
        outcome,
        etag,
        deadline_adjustments: Vec::new(),
    }
}

fn list_etag(route: &RouteDefinition, position: Option<String>) -> Option<String> {
    let inputs = route.etag_inputs.as_ref()?;
    let EtagKind::List(relations) = &inputs.kind else {
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

pub(super) async fn deliver_stream(
    route: &RouteDefinition,
    request: DeliveryRequest<AuthenticatedCaller>,
) -> Result<(StreamReader<u8>, Option<String>), DeliveryReport> {
    let report = |outcome, etag| DeliveryReport {
        outcome,
        etag,
        deadline_adjustments: Vec::new(),
    };
    if let Some(maximum) = route.stream_ceiling
        && let Some(refusal) = limit_refusal(&request.payload, maximum)
    {
        return Err(report(Ok(DeliveryOutcome::Respond(refusal)), None));
    }
    let id = route
        .sealed_operation_id
        .as_ref()
        .expect("base route has an operation");
    operation::authorize(request.caller.as_ref(), id.clone())
        .await
        .map_err(|error| report(Err(boundary(error, id)), None))?;
    let query = dispatch::query(
        id.clone(),
        context(&request, String::new(), route.operation_deadline_ms),
        request.payload.clone(),
        wants_list_etag(route),
    )
    .await;
    let (metadata, mut rows, end) = match query {
        Ok(query) => query,
        Err(error) => {
            let _ = operation::finish_read().await;
            return Err(report(Ok(node_failure(error)), None));
        }
    };
    let etag = list_etag(route, metadata.await.wal_position);
    if matches_tag(&request, &etag) {
        drop(rows);
        drop(end);
        operation::finish_read()
            .await
            .map_err(|error| report(Err(boundary(error, id)), None))?;
        return Err(report(Ok(DeliveryOutcome::NotModified), etag));
    }
    // Wait for the first batch so an empty refused read keeps its HTTP refusal.
    let (mut status, mut batch) = rows.read(Vec::with_capacity(super::LINES_PER_READ)).await;
    let mut end = Some(end);
    let early = if batch.is_empty() && !matches!(status, StreamResult::Complete(_)) {
        let outcome = finish_read(&end.take().expect("read has an outcome").await).await;
        if let Some(error) = outcome.get("error") {
            return Err(report(
                Ok(DeliveryOutcome::Respond(
                    json!([{"error": error}]).to_string(),
                )),
                None,
            ));
        }
        Some(outcome)
    } else {
        None
    };
    let (mut writer, lines) = super::bindings::wit_stream::new::<u8>();
    spawn_local(async move {
        loop {
            let mut bytes = Vec::new();
            for row in batch {
                bytes.extend_from_slice(format!("{{\"row\":{row}}}\n").as_bytes());
            }
            if !writer.write_all(bytes).await.is_empty() {
                drop(rows);
                drop(end);
                let _ = operation::finish_read().await;
                return;
            }
            if !matches!(status, StreamResult::Complete(_)) {
                break;
            }
            (status, batch) = rows.read(Vec::with_capacity(super::LINES_PER_READ)).await;
        }
        drop(rows);
        let outcome = match early {
            Some(outcome) => outcome,
            None => finish_read(&end.expect("read has an outcome").await).await,
        };
        let mut bytes = json!({"outcome": outcome}).to_string().into_bytes();
        bytes.push(b'\n');
        let _ = writer.write_all(bytes).await;
    });
    Ok((lines, etag))
}

async fn finish_read(text: &str) -> Value {
    let outcome = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
    if let Some(value) = outcome.get("value") {
        if operation::finish_read().await.is_err() {
            return json!({"uncertain": {}});
        }
        json!({"value": {"more": value.get("next_cursor").is_some_and(|cursor| !cursor.is_null())}})
    } else {
        let _ = operation::finish_read().await;
        outcome
            .get("error")
            .map_or_else(|| json!({"uncertain": {}}), |error| json!({"error": error}))
    }
}

async fn execute(
    route: &RouteDefinition,
    request: &DeliveryRequest<AuthenticatedCaller>,
    etag: &mut Option<String>,
) -> Result<DeliveryOutcome, DeliveryError> {
    let deadline = super::bindings::wasi::clocks::monotonic_clock::now()
        .saturating_add(route.operation_deadline_ms.saturating_mul(1_000_000));
    let operation_id = route
        .sealed_operation_id
        .as_ref()
        .ok_or(DeliveryError::InvalidRequest)?;
    let caller = request.caller.as_ref();
    let mut input: Value =
        serde_json::from_str(&request.payload).map_err(|_| DeliveryError::InvalidPayload)?;
    let Some(key_field) = route.idempotency_rule.as_deref() else {
        operation::authorize(caller, operation_id.clone())
            .await
            .map_err(|error| boundary(error, operation_id))?;
        let result = read_call(
            operation_id,
            context(request, String::new(), remaining_ms(deadline)),
            input.to_string(),
            wants_list_etag(route),
        )
        .await;
        match result {
            Ok((result, wal_position)) => {
                *etag = list_etag(route, wal_position);
                operation::finish_read()
                    .await
                    .map_err(|error| boundary(error, operation_id))?;
                if let Some(inputs) = &route.etag_inputs
                    && let EtagKind::Get(field) = &inputs.kind
                    && let Some(items) = result.as_array()
                    && let [item] = items.as_slice()
                    && let Some(revision) = item.get("value").and_then(|value| value.get(field))
                    && (revision.is_number() || revision.is_string())
                {
                    *etag = Some(format!(
                        "\"{}\"",
                        tag_digest(&json!(["get", inputs.release, revision]))
                    ));
                }
                if matches_tag(request, etag) {
                    return Ok(DeliveryOutcome::NotModified);
                }
                return Ok(DeliveryOutcome::Respond(result.to_string()));
            }
            Err(error) => {
                operation::finish_read()
                    .await
                    .map_err(|error| boundary(error, operation_id))?;
                return Ok(node_failure(error));
            }
        }
    };
    let Some(items) = input.as_array_mut() else {
        return Ok(invalid("the input is not an item list"));
    };
    let mut keys = HashSet::new();
    for item in items.iter_mut() {
        if let Some(rule) = &route.request_hash_rule {
            canonicalize(item, rule);
        }
        let Some(key) = field(item, key_field).and_then(Value::as_str) else {
            return Ok(invalid(&format!("an item has no {key_field}")));
        };
        if item.get("request_id").and_then(Value::as_str).is_none() || !keys.insert(key.to_owned())
        {
            return Ok(invalid(
                "an item has no request_id or repeats its idempotency key",
            ));
        }
    }
    let mut answers = Vec::with_capacity(items.len());
    for item in items.iter() {
        let request_id = item["request_id"].as_str().expect("request id checked");
        if remaining_ms(deadline) == 0 {
            answers.push(
                json!({"request_id": request_id, "error": {
                    "code": "timeout", "message": "not reached before deadline"
                }})
                .to_string(),
            );
            continue;
        }
        let key = field(item, key_field)
            .and_then(Value::as_str)
            .expect("key checked");
        let mut hashed = item.clone();
        if let Some(rule) = &route.request_hash_rule {
            for path in &rule.excluded {
                remove(&mut hashed, &path.split('.').collect::<Vec<_>>());
            }
        }
        let hash = wamn_execution_contract::canonical_json_sha256(&hashed).into_bytes();
        let begun =
            operation::begin_operation(caller, operation_id.clone(), key.to_owned(), hash).await;
        let intent = format!(
            "{}#{key}",
            operation_id.split('@').next().unwrap_or(operation_id)
        );
        let body = match begun {
            Ok(operation::Begun::Finished(body)) => {
                answers.push(with_request_id(body, request_id)?);
                continue;
            }
            Err(operation::Refusal::Conflict) => refusal(
                "idempotency_conflict",
                format!("the key {key} repeats with another input"),
                json!({"field": key_field, "intent": intent}),
            ),
            Err(operation::Refusal::Uncertain) => uncertain(&intent),
            Err(operation::Refusal::Resolved(basis)) => resolved(&intent, &basis),
            Err(error) => return Err(boundary(error, operation_id)),
            Ok(operation::Begun::New(delivery_id)) => {
                let result = call(
                    operation_id,
                    context(request, delivery_id, remaining_ms(deadline)),
                    json!([item]).to_string(),
                )
                .await;
                let result = match result {
                    Ok(Value::Array(mut results))
                        if results.len() == 1 && results[0]["request_id"] == request_id =>
                    {
                        let mut result = results.remove(0);
                        result
                            .as_object_mut()
                            .ok_or(DeliveryError::ExecutionFailed)?
                            .remove("request_id");
                        result
                    }
                    Ok(_) => {
                        operation::rollback()
                            .await
                            .map_err(|error| boundary(error, operation_id))?;
                        answers.push(json!({"request_id": request_id, "error": {
                            "code": "internal_error",
                            "message": format!("the export answered nothing for item {request_id}"),
                            "detail": null,
                        }}).to_string());
                        continue;
                    }
                    Err(error) => {
                        operation::rollback()
                            .await
                            .map_err(|error| boundary(error, operation_id))?;
                        if items.len() == 1 {
                            return Ok(node_failure(error));
                        }
                        let detail = match error {
                            NodeError::Terminal(detail)
                            | NodeError::Retryable(detail)
                            | NodeError::InvalidInput(detail) => detail,
                            NodeError::RateLimited(limited) => limited.detail,
                            NodeError::Cancelled => return Ok(DeliveryOutcome::Cancelled),
                        };
                        answers.push(json!({"request_id": request_id, "error": {"code": detail.code, "message": detail.message}}).to_string());
                        continue;
                    }
                };
                if result.get("value").is_some() {
                    if let Err(error) = operation::commit(result.to_string()).await {
                        commit_failure(error, &intent)
                    } else {
                        result
                    }
                } else {
                    operation::rollback()
                        .await
                        .map_err(|error| boundary(error, operation_id))?;
                    result
                }
            }
        };
        answers.push(with_request_id(body.to_string(), request_id)?);
    }
    Ok(DeliveryOutcome::Respond(format!("[{}]", answers.join(","))))
}

fn commit_failure(error: operation::Refusal, intent: &str) -> Value {
    use super::bindings::wamn::postgres::types::PgError;
    match error {
        operation::Refusal::Uncertain => uncertain(intent),
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
    request: &DeliveryRequest<AuthenticatedCaller>,
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

fn wants_list_etag(route: &RouteDefinition) -> bool {
    route
        .etag_inputs
        .as_ref()
        .is_some_and(|inputs| matches!(inputs.kind, EtagKind::List(_)))
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
    if dispatch::is_projection(id) {
        let (emission, position) = dispatch::read(id.to_owned(), context, input, want_wal).await?;
        return decode(&emission.payload).map(|value| (value, position));
    }
    if !dispatch::is_query(id) {
        return decode(&dispatch::run(id.to_owned(), context, input).await?.payload)
            .map(|value| (value, None));
    }
    let (metadata, mut rows, end) =
        dispatch::query(id.to_owned(), context, input, want_wal).await?;
    let wal_position = metadata.await.wal_position;
    let mut values = Vec::<Value>::new();
    loop {
        let (status, batch) = rows.read(Vec::with_capacity(256)).await;
        for row in batch {
            values.push(decode(&row)?);
        }
        if !matches!(status, StreamResult::Complete(_)) {
            break;
        }
    }
    let mut outcome: Value = decode(&end.await)?;
    if let Some(value) = outcome.get_mut("value").and_then(Value::as_object_mut) {
        value.insert("item".into(), Value::Array(values));
    }
    Ok((json!([outcome]), wal_position))
}

fn boundary(error: operation::Refusal, id: &str) -> DeliveryError {
    match error {
        operation::Refusal::GrantDenied | operation::Refusal::InvalidCaller => {
            DeliveryError::PermissionDenied {
                operation: id.to_owned(),
            }
        }
        operation::Refusal::UnknownOperation => DeliveryError::SourceNotFound,
        _ => DeliveryError::ExecutionFailed,
    }
}

fn invalid(message: &str) -> DeliveryOutcome {
    DeliveryOutcome::Failed(DeliveryFailure {
        kind: DeliveryFailureKind::InvalidInput,
        code: Some("invalid_input".into()),
        message: message.into(),
    })
}

fn node_failure(error: NodeError) -> DeliveryOutcome {
    let (kind, detail) = match error {
        NodeError::InvalidInput(detail) => (DeliveryFailureKind::InvalidInput, detail),
        NodeError::Terminal(detail) => (DeliveryFailureKind::Terminal, detail),
        NodeError::Retryable(detail) => (DeliveryFailureKind::RetryExhausted, detail),
        NodeError::RateLimited(limited) => (DeliveryFailureKind::RetryExhausted, limited.detail),
        NodeError::Cancelled => return DeliveryOutcome::Cancelled,
    };
    DeliveryOutcome::Failed(DeliveryFailure {
        kind,
        code: detail.code,
        message: detail.message,
    })
}

fn tag_digest(value: &Value) -> String {
    wamn_execution_contract::canonical_json_sha256(value)[7..39].to_owned()
}

fn matches_tag(request: &DeliveryRequest<AuthenticatedCaller>, tag: &Option<String>) -> bool {
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

fn canonicalize(item: &mut Value, rule: &RequestHashRule) {
    for spelling in &rule.spelling {
        respell(
            item,
            &spelling.path.split('.').collect::<Vec<_>>(),
            spelling.spelling,
        );
    }
    if let Some(order) = &rule.lines
        && let Some(Value::Array(lines)) = order
            .path
            .split('.')
            .try_fold(item, |value, key| value.get_mut(key))
    {
        lines.sort_by_cached_key(|line| {
            line.get(&order.ascending_by)
                .map_or_else(String::new, Value::to_string)
        });
    }
}

fn respell(value: &mut Value, path: &[&str], spelling: CanonicalSpelling) {
    let Some((first, rest)) = path.split_first() else {
        if let Value::String(text) = value {
            let canonical = match spelling {
                CanonicalSpelling::Numeric => wamn_execution_contract::canonical_numeric(text),
                CanonicalSpelling::Timestamptz => {
                    wamn_execution_contract::canonical_timestamptz(text)
                }
            };
            if let Some(canonical) = canonical {
                *text = canonical;
            }
        }
        return;
    };
    if let Some(name) = first.strip_suffix("[]") {
        if let Some(Value::Array(items)) = value.get_mut(name) {
            for item in items {
                respell(item, rest, spelling);
            }
        }
    } else if let Some(value) = value.get_mut(*first) {
        respell(value, rest, spelling);
    }
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
                if let Some(Value::Array(items)) = value.get_mut(name) {
                    for item in items {
                        remove(item, rest);
                    }
                }
            } else if let Some(value) = value.get_mut(*first) {
                remove(value, rest);
            }
        }
    }
}

pub(super) fn limit_refusal(payload: &str, maximum: u32) -> Option<String> {
    let payload: Value = serde_json::from_str(payload).ok()?;
    let limit = payload.get(0)?.get("limit")?.as_i64()?;
    (limit > i64::from(maximum)).then(|| {
        json!([{
            "error": {"code": "invalid_input", "detail": {
                "field": "limit", "minimum": "1",
                "maximum": maximum.to_string(), "observed": limit.to_string(),
            }},
        }])
        .to_string()
    })
}
