// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::AdjustItem;
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
struct JsonRequest {
    value: JsonRoot,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRoot {
    expected_row_version: i32,
    idempotency_key: String,
    occurred_at: String,
    packaging_id: String,
    product_id: String,
    quantity: String,
    reason_code: String,
    status: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::AdjustItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::AdjustRequest {
                    expected_row_version: request.value.expected_row_version,
                    idempotency_key: request.value.idempotency_key,
                    occurred_at: request.value.occurred_at,
                    packaging_id: request.value.packaging_id,
                    product_id: request.value.product_id,
                    quantity: request.value.quantity,
                    reason_code: request.value.reason_code,
                    status: request.value.status,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::AdjustItem { request_id, input })
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

pub(crate) fn encode(output: &[contract::AdjustOutcome]) -> String {
    let values = output.iter().map(|item| {
        match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "transaction_ids": value.transaction_ids.iter().map(|element| json!(element)).collect::<Vec<_>>(),
                    "packaging_id": value.packaging_id,
                    "adjusted_quantity": value.adjusted_quantity,
                    "packaging_status": value.packaging_status,
                    "row_version": value.row_version,
                }
            }),
            Err(error) => json!({
                "request_id": item.request_id,
                "error": error_value(error),
            }),
        }
    }).collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed outcomes always serialize")
}

fn error_value(error: &contract::AdjustError) -> Value {
    let (code, detail) = match error {
        contract::AdjustError::InvalidInput(value) => {
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
        contract::AdjustError::PackagingNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("packaging_not_found", detail)
        }
        contract::AdjustError::QuantityNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("quantity_not_found", detail)
        }
        contract::AdjustError::ConcurrencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert(
                "expected_row_version".to_owned(),
                json!(value.expected_row_version),
            );
            detail.insert(
                "observed_row_version".to_owned(),
                json!(value.observed_row_version),
            );
            ("concurrency_conflict", detail)
        }
        contract::AdjustError::IdempotencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("idempotency_conflict", detail)
        }
        contract::AdjustError::Retry => ("retry", Map::new()),
        contract::AdjustError::Timeout => ("timeout", Map::new()),
        contract::AdjustError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::AdjustError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::AdjustRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    {
        let value = &mut request.packaging_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.packaging_id"));
        }
    }
    {
        let value = &mut request.product_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.product_id"));
        }
    }
    {
        let value = &mut request.status;
        if !["available", "held"].contains(&value.as_str()) {
            return Err(invalid("value.status"));
        }
    }
    Ok(())
}

include!("write_log_codec.rs");

/// The write log operation of this contract: its operation without the version.
const OPERATION: &str = "wamn-wms:inventory/adjust";
/// The input field that carries the key the write log claims.
const KEY_FIELD: &str = "value.idempotency_key";

/// The bytes the write log keeps for one validated request: its canonical JSON
/// without the key and the request id, and the participation intent when one
/// is selected.
fn request_bytes(request: &contract::AdjustRequest, intent: Option<&str>) -> Vec<u8> {
    let mut value = {
        let mut value = Map::new();
        value.insert(
            "expected_row_version".to_owned(),
            json!(&request.expected_row_version),
        );
        value.insert(
            "occurred_at".to_owned(),
            json!(
                wamn_execution_contract::canonical_timestamptz(&request.occurred_at)
                    .unwrap_or_else(|| request.occurred_at.clone())
            ),
        );
        value.insert("packaging_id".to_owned(), json!(&request.packaging_id));
        value.insert("product_id".to_owned(), json!(&request.product_id));
        value.insert(
            "quantity".to_owned(),
            json!(
                wamn_execution_contract::canonical_numeric(&request.quantity)
                    .unwrap_or_else(|| request.quantity.clone())
            ),
        );
        value.insert("reason_code".to_owned(), json!(&request.reason_code));
        value.insert("status".to_owned(), json!(&request.status));
        Value::Object(value)
    };
    if let (Some(intent), Value::Object(object)) = (intent, &mut value) {
        object.insert("participation_intent".to_owned(), json!(intent));
    }
    wamn_execution_contract::canonical_json_bytes(&value)
}

/// The result the write log stores for one success: its encoded outcome
/// without the request id.
fn stored_result(result: &contract::AdjustResult) -> String {
    let encoded = encode(&[contract::AdjustOutcome {
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
    transaction_ids: Vec<String>,
    packaging_id: String,
    adjusted_quantity: String,
    packaging_status: String,
    row_version: i32,
}

/// The success that one stored result answers.
fn stored_success(result: &str) -> Option<contract::AdjustResult> {
    let value: JsonResult = serde_json::from_str(result).ok()?;
    Some(contract::AdjustResult {
        transaction_ids: value.transaction_ids,
        packaging_id: value.packaging_id,
        adjusted_quantity: value.adjusted_quantity,
        packaging_status: value.packaging_status,
        row_version: value.row_version,
    })
}

#[allow(dead_code)]
pub(crate) async fn run<F>(
    input: Vec<contract::AdjustItem>,
    connection: &mut wamn_postgres_statements::Connection,
    mut handler: F,
) -> Vec<contract::AdjustOutcome>
where
    F: AsyncFnMut(
        &mut wamn_postgres_statements::Transaction,
        contract::AdjustRequest,
    ) -> Result<contract::AdjustResult, contract::AdjustError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => claimed(connection, request, &mut handler).await,
                Err(error) => Err(contract::AdjustError::InvalidInput(error)),
            },
            Err(error) => Err(contract::AdjustError::InvalidInput(error)),
        };
        output.push(contract::AdjustOutcome {
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
    request: contract::AdjustRequest,
    handler: &mut F,
) -> Result<contract::AdjustResult, contract::AdjustError>
where
    F: AsyncFnMut(
        &mut wamn_postgres_statements::Transaction,
        contract::AdjustRequest,
    ) -> Result<contract::AdjustResult, contract::AdjustError>,
{
    let refuse =
        |error: &wamn_postgres_statements::StatementError| map_error(log_error(error), |_| None);
    let key = request.idempotency_key.clone();
    let intent: Option<String> = None;
    let bytes = request_bytes(&request, intent.as_deref());
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
            transaction_ids: row
                .transaction_ids
                .into_iter()
                .map(|value| value.0)
                .collect(),
            packaging_id: row.packaging_id.0,
            adjusted_quantity: row.adjusted_quantity.0,
            packaging_status: row.packaging_status,
            row_version: row.row_version,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::AdjustError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::AdjustError::InternalError;
            };
            let minimum = detail("minimum");
            let maximum = detail("maximum");
            let observed = detail("observed");
            contract::AdjustError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "packaging_not_found" => {
            let Some(field) = detail("field") else {
                return contract::AdjustError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::AdjustError::InternalError;
            };
            contract::AdjustError::PackagingNotFound(contract::PackagingNotFoundDetail {
                field,
                id,
            })
        }
        "quantity_not_found" => {
            let Some(field) = detail("field") else {
                return contract::AdjustError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::AdjustError::InternalError;
            };
            contract::AdjustError::QuantityNotFound(contract::QuantityNotFoundDetail { field, id })
        }
        "concurrency_conflict" => {
            let Some(expected_row_version) =
                detail("expected_row_version").and_then(|value| value.parse::<i32>().ok())
            else {
                return contract::AdjustError::InternalError;
            };
            let Some(observed_row_version) =
                detail("observed_row_version").and_then(|value| value.parse::<i32>().ok())
            else {
                return contract::AdjustError::InternalError;
            };
            contract::AdjustError::ConcurrencyConflict(contract::ConcurrencyConflictDetail {
                expected_row_version,
                observed_row_version,
            })
        }
        "idempotency_conflict" => {
            let Some(field) = detail("field") else {
                return contract::AdjustError::InternalError;
            };
            contract::AdjustError::IdempotencyConflict(contract::IdempotencyConflictDetail {
                field,
            })
        }
        "retry" => contract::AdjustError::Retry,
        "timeout" => contract::AdjustError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::AdjustError::InternalError;
            };
            contract::AdjustError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::AdjustError::InternalError,
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
                    input: Vec<__contract::AdjustItem>,
                ) -> Result<Vec<__contract::AdjustOutcome>, __node::NodeError> {
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
