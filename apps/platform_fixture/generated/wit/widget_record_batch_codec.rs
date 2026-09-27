// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::RecordBatchItem;
const MINIMUM: usize = 1;
const MAXIMUM: usize = 100;
const COUNT_ERROR: &str = "operation input item count must be 1..=100";

#[allow(dead_code)]
pub(crate) fn validate(input: &[Item]) -> Result<(), CodecError> {
    validate_count(input.len())?;
    if input.iter().any(|item| item.request_id.is_empty()) {
        return Err(CodecError(
            "every operation item must carry a nonempty string request_id",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonLine {
    amount: String,
    widget_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    value: JsonRoot,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRoot {
    expected_edit_version: JsonInt64,
    grade: String,
    idempotency_key: String,
    inspector_id: Option<String>,
    line: Vec<JsonLine>,
    maker_id: Option<String>,
    note: Option<String>,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::RecordBatchItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::RecordBatchRequest {
                    expected_edit_version: request.value.expected_edit_version.0,
                    grade: request.value.grade,
                    idempotency_key: request.value.idempotency_key,
                    inspector_id: request.value.inspector_id,
                    line: request
                        .value
                        .line
                        .into_iter()
                        .map(|value| contract::RecordBatchLine {
                            amount: value.amount,
                            widget_id: value.widget_id,
                        })
                        .collect(),
                    maker_id: request.value.maker_id,
                    note: request.value.note,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::RecordBatchItem { request_id, input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
        minimum: None,
        maximum: None,
        observed: None,
    }
}

pub(crate) fn encode(output: &[contract::RecordBatchOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "widget_id": value.widget_id,
                }
            }),
            Err(error) => json!({
                "request_id": item.request_id,
                "error": error_value(error),
            }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed outcomes always serialize")
}

fn error_value(error: &contract::RecordBatchError) -> Value {
    let (code, detail) = match error {
        contract::RecordBatchError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            if let Some(detail_value) = &value.minimum {
                detail.insert("minimum".to_owned(), json!(detail_value));
            }
            if let Some(detail_value) = &value.maximum {
                detail.insert("maximum".to_owned(), json!(detail_value));
            }
            if let Some(detail_value) = &value.observed {
                detail.insert("observed".to_owned(), json!(detail_value));
            }
            ("invalid_input", detail)
        }
        contract::RecordBatchError::IdempotencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("idempotency_conflict", detail)
        }
        contract::RecordBatchError::Retry => ("retry", Map::new()),
        contract::RecordBatchError::Timeout => ("timeout", Map::new()),
        contract::RecordBatchError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::RecordBatchError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(
    request: &mut contract::RecordBatchRequest,
) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    {
        let value = &mut request.grade;
        if !["first", "second"].contains(&value.as_str()) {
            return Err(invalid("value.grade"));
        }
    }
    if let Some(value) = &mut request.inspector_id
        && (!canonical_uuid(value))
    {
        return Err(invalid("value.inspector_id"));
    }
    if request.line.is_empty() {
        return Err(invalid("value.line[]"));
    }
    if request.line.len() > 10 {
        return Err(invalid("value.line[]"));
    }
    for value in &mut request.line {
        {
            let value = &mut value.widget_id;
            if !canonical_uuid(value) {
                return Err(invalid("value.line[].widget_id"));
            }
        }
    }
    if let Some(value) = &mut request.maker_id
        && (!canonical_uuid(value))
    {
        return Err(invalid("value.maker_id"));
    }
    Ok(())
}

include!("write_log_codec.rs");

/// The write log operation of this contract: its operation without the version.
const OPERATION: &str = "platform-fixture:widget/record-batch";
/// The input field that carries the key the write log claims.
const KEY_FIELD: &str = "value.idempotency_key";

/// The bytes the write log keeps for one validated request: its canonical JSON
/// without the key and the request id.
fn request_bytes(request: &contract::RecordBatchRequest) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(&{
        let mut value = Map::new();
        value.insert(
            "expected_edit_version".to_owned(),
            json!(&request.expected_edit_version),
        );
        value.insert("grade".to_owned(), json!(&request.grade));
        value.insert("inspector_id".to_owned(), json!(&request.inspector_id));
        value.insert("line".to_owned(), {
            let mut lines = request
                .line
                .iter()
                .map(|element| {
                    let mut value = Map::new();
                    value.insert("amount".to_owned(), json!(&element.amount));
                    value.insert("widget_id".to_owned(), json!(&element.widget_id));
                    Value::Object(value)
                })
                .collect::<Vec<_>>();
            lines.sort_by(|left, right| {
                left["widget_id"]
                    .to_string()
                    .cmp(&right["widget_id"].to_string())
            });
            Value::Array(lines)
        });
        value.insert("maker_id".to_owned(), json!(&request.maker_id));
        value.insert("note".to_owned(), json!(&request.note));
        Value::Object(value)
    })
}

/// The result the write log stores for one success: its encoded outcome
/// without the request id.
fn stored_result(result: &contract::RecordBatchResult) -> String {
    let encoded = encode(&[contract::RecordBatchOutcome {
        request_id: String::new(),
        outcome: Ok(result.clone()),
    }]);
    let mut outcomes: Vec<Value> =
        serde_json::from_str(&encoded).expect("the encoder writes a JSON list");
    outcomes
        .pop()
        .and_then(|mut outcome| outcome.get_mut("value").map(Value::take))
        .expect("an encoded success carries its value")
        .to_string()
}

#[derive(Deserialize)]
struct JsonResult {
    widget_id: String,
}

/// The success that one stored result answers.
fn stored_success(result: &str) -> Option<contract::RecordBatchResult> {
    let value: JsonResult = serde_json::from_str(result).ok()?;
    Some(contract::RecordBatchResult {
        widget_id: value.widget_id,
    })
}

#[allow(dead_code)]
pub(crate) async fn run<F>(
    input: Vec<contract::RecordBatchItem>,
    connection: &mut wamn_postgres_statements::Connection,
    mut handler: F,
) -> Vec<contract::RecordBatchOutcome>
where
    F: AsyncFnMut(
        &mut wamn_postgres_statements::Transaction,
        contract::RecordBatchRequest,
    ) -> Result<contract::RecordBatchResult, contract::RecordBatchError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => claimed(connection, request, &mut handler).await,
                Err(error) => Err(contract::RecordBatchError::InvalidInput(error)),
            },
            Err(error) => Err(contract::RecordBatchError::InvalidInput(error)),
        };
        output.push(contract::RecordBatchOutcome {
            request_id: item.request_id,
            outcome,
        });
    }
    output
}

/// Claim the key of one request, do its work and store its result in one
/// transaction, or answer what a committed claim of the key holds.
async fn claimed<F>(
    connection: &mut wamn_postgres_statements::Connection,
    request: contract::RecordBatchRequest,
    handler: &mut F,
) -> Result<contract::RecordBatchResult, contract::RecordBatchError>
where
    F: AsyncFnMut(
        &mut wamn_postgres_statements::Transaction,
        contract::RecordBatchRequest,
    ) -> Result<contract::RecordBatchResult, contract::RecordBatchError>,
{
    let refuse =
        |error: &wamn_postgres_statements::StatementError| map_error(log_error(error), |_| None);
    let key = request.idempotency_key.clone();
    let bytes = request_bytes(&request);
    let mut transaction = connection.begin().await.map_err(|error| refuse(&error))?;
    match log_claim(&mut transaction, OPERATION, &key, &bytes).await {
        Ok(Logged::Claimed) => {}
        Ok(Logged::Stored(stored, result)) => {
            let _ = transaction.rollback().await;
            if stored != bytes {
                return Err(map_error("idempotency_conflict", |name| {
                    (name == "field").then(|| KEY_FIELD.to_owned())
                }));
            }
            return result
                .as_deref()
                .and_then(stored_success)
                .ok_or_else(|| map_error("internal_error", |_| None));
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            return Err(refuse(&error));
        }
    }
    let result = match handler(&mut transaction, request).await {
        Ok(result) => result,
        Err(error) => {
            let _ = transaction.rollback().await;
            return Err(error);
        }
    };
    if let Err(error) = log_finish(&mut transaction, OPERATION, &key, stored_result(&result)).await
    {
        let _ = transaction.rollback().await;
        return Err(refuse(&error));
    }
    transaction.commit().await.map_err(|error| refuse(&error))?;
    Ok(result)
}

#[allow(unused_macros)]
macro_rules! row {
    ($row:expr, $target:path) => {{
        let row = $row;
        $target {
            widget_id: row.widget_id.0,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::RecordBatchError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::RecordBatchError::InternalError;
            };
            let minimum = detail("minimum");
            let maximum = detail("maximum");
            let observed = detail("observed");
            contract::RecordBatchError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "idempotency_conflict" => {
            let Some(field) = detail("field") else {
                return contract::RecordBatchError::InternalError;
            };
            contract::RecordBatchError::IdempotencyConflict(contract::IdempotencyConflictDetail {
                field,
            })
        }
        "retry" => contract::RecordBatchError::Retry,
        "timeout" => contract::RecordBatchError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::RecordBatchError::InternalError;
            };
            contract::RecordBatchError::PermissionDenied(contract::PermissionDeniedDetail {
                operation,
            })
        }
        _ => contract::RecordBatchError::InternalError,
    }
}

#[allow(unused_macros)]
macro_rules! export_operation {
    ($component:ty, $contract:path, $node:path, $state:expr, $handler:path, $codec:ident) => {
        const _: () = {
            use $codec as __codec;
            use $contract as __contract;
            use $node as __node;

            fn invalid(error: __codec::CodecError) -> __node::NodeError {
                __node::NodeError::InvalidInput(__node::ErrorDetail {
                    message: error.context().to_owned(),
                    code: Some("invalid_input".to_owned()),
                })
            }

            impl __contract::Guest for $component {
                async fn run(
                    _context: __node::NodeContext,
                    input: Vec<__contract::RecordBatchItem>,
                ) -> Result<Vec<__contract::RecordBatchOutcome>, __node::NodeError> {
                    let mut state = $state;
                    __codec::validate(&input).map_err(invalid)?;
                    Ok(__codec::run(input, &mut state, $handler).await)
                }

                async fn run_json(
                    context: __node::NodeContext,
                    input: String,
                ) -> Result<__node::Emission, __node::NodeError> {
                    let input = __codec::decode(&input).map_err(invalid)?;
                    let output = <Self as __contract::Guest>::run(context, input).await?;
                    Ok(__node::Emission {
                        payload: __codec::encode(&output),
                        port: None,
                    })
                }
            }
        };
    };
}
#[allow(unused_imports)]
pub(crate) use export_operation;
