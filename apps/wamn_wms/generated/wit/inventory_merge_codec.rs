// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::MergeItem;
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
    source_pallet_id: String,
    target_pallet_id: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::MergeItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::MergeRequest {
                    expected_row_version: request.value.expected_row_version,
                    idempotency_key: request.value.idempotency_key,
                    occurred_at: request.value.occurred_at,
                    source_pallet_id: request.value.source_pallet_id,
                    target_pallet_id: request.value.target_pallet_id,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::MergeItem { request_id, input })
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

pub(crate) fn encode(output: &[contract::MergeOutcome]) -> String {
    let values = output.iter().map(|item| {
        match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "movement_ids": value.movement_ids.iter().map(|element| json!(element)).collect::<Vec<_>>(),
                    "source_pallet_id": value.source_pallet_id,
                    "target_pallet_id": value.target_pallet_id,
                    "target_status": value.target_status,
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

fn error_value(error: &contract::MergeError) -> Value {
    let (code, detail) = match error {
        contract::MergeError::InvalidInput(value) => {
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
        contract::MergeError::PalletNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("pallet_not_found", detail)
        }
        contract::MergeError::ConcurrencyConflict(value) => {
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
        contract::MergeError::IdempotencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("idempotency_conflict", detail)
        }
        contract::MergeError::Retry => ("retry", Map::new()),
        contract::MergeError::Timeout => ("timeout", Map::new()),
        contract::MergeError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::MergeError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::MergeRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    {
        let value = &mut request.source_pallet_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.source_pallet_id"));
        }
    }
    {
        let value = &mut request.target_pallet_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.target_pallet_id"));
        }
    }
    Ok(())
}

include!("write_log_codec.rs");

/// The write log operation of this contract: its operation without the version.
const OPERATION: &str = "wamn-wms:inventory/merge";
/// The input field that carries the key the write log claims.
const KEY_FIELD: &str = "value.idempotency_key";

/// The bytes the write log keeps for one validated request: its canonical JSON
/// without the key and the request id, and the participation intent when one
/// is selected.
fn request_bytes(request: &contract::MergeRequest, intent: Option<&str>) -> Vec<u8> {
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
        value.insert(
            "source_pallet_id".to_owned(),
            json!(&request.source_pallet_id),
        );
        value.insert(
            "target_pallet_id".to_owned(),
            json!(&request.target_pallet_id),
        );
        Value::Object(value)
    };
    if let (Some(intent), Value::Object(object)) = (intent, &mut value) {
        object.insert("participation_intent".to_owned(), json!(intent));
    }
    wamn_execution_contract::canonical_json_bytes(&value)
}

/// The result the write log stores for one success: its encoded outcome
/// without the request id.
fn stored_result(result: &contract::MergeResult) -> String {
    let encoded = encode(&[contract::MergeOutcome {
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
    movement_ids: Vec<String>,
    source_pallet_id: String,
    target_pallet_id: String,
    target_status: String,
    row_version: i32,
}

/// The success that one stored result answers.
fn stored_success(result: &str) -> Option<contract::MergeResult> {
    let value: JsonResult = serde_json::from_str(result).ok()?;
    Some(contract::MergeResult {
        movement_ids: value.movement_ids,
        source_pallet_id: value.source_pallet_id,
        target_pallet_id: value.target_pallet_id,
        target_status: value.target_status,
        row_version: value.row_version,
    })
}

#[allow(dead_code)]
pub(crate) async fn run<F>(
    input: Vec<contract::MergeItem>,
    connection: &mut wamn_postgres_statements::Connection,
    mut handler: F,
) -> Vec<contract::MergeOutcome>
where
    F: AsyncFnMut(
        &mut wamn_postgres_statements::Transaction,
        contract::MergeRequest,
    ) -> Result<contract::MergeResult, contract::MergeError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => claimed(connection, request, &mut handler).await,
                Err(error) => Err(contract::MergeError::InvalidInput(error)),
            },
            Err(error) => Err(contract::MergeError::InvalidInput(error)),
        };
        output.push(contract::MergeOutcome {
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
    request: contract::MergeRequest,
    handler: &mut F,
) -> Result<contract::MergeResult, contract::MergeError>
where
    F: AsyncFnMut(
        &mut wamn_postgres_statements::Transaction,
        contract::MergeRequest,
    ) -> Result<contract::MergeResult, contract::MergeError>,
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
            movement_ids: row.movement_ids.into_iter().map(|value| value.0).collect(),
            source_pallet_id: row.source_pallet_id.0,
            target_pallet_id: row.target_pallet_id.0,
            target_status: row.target_status,
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
) -> contract::MergeError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::MergeError::InternalError;
            };
            let minimum = detail("minimum");
            let maximum = detail("maximum");
            let observed = detail("observed");
            contract::MergeError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "pallet_not_found" => {
            let Some(field) = detail("field") else {
                return contract::MergeError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::MergeError::InternalError;
            };
            contract::MergeError::PalletNotFound(contract::PalletNotFoundDetail { field, id })
        }
        "concurrency_conflict" => {
            let Some(expected_row_version) =
                detail("expected_row_version").and_then(|value| value.parse::<i32>().ok())
            else {
                return contract::MergeError::InternalError;
            };
            let Some(observed_row_version) =
                detail("observed_row_version").and_then(|value| value.parse::<i32>().ok())
            else {
                return contract::MergeError::InternalError;
            };
            contract::MergeError::ConcurrencyConflict(contract::ConcurrencyConflictDetail {
                expected_row_version,
                observed_row_version,
            })
        }
        "idempotency_conflict" => {
            let Some(field) = detail("field") else {
                return contract::MergeError::InternalError;
            };
            contract::MergeError::IdempotencyConflict(contract::IdempotencyConflictDetail { field })
        }
        "retry" => contract::MergeError::Retry,
        "timeout" => contract::MergeError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::MergeError::InternalError;
            };
            contract::MergeError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::MergeError::InternalError,
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
                    input: Vec<__contract::MergeItem>,
                ) -> Result<Vec<__contract::MergeOutcome>, __node::NodeError> {
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
