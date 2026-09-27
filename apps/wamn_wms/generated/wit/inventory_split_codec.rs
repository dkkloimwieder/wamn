// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::SplitItem;
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
    new_packaging_code: String,
    new_packaging_type: String,
    occurred_at: String,
    product_id: String,
    quantity: String,
    source_packaging_id: String,
    status: String,
    to_location_id: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::SplitItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::SplitRequest {
                    expected_row_version: request.value.expected_row_version,
                    idempotency_key: request.value.idempotency_key,
                    new_packaging_code: request.value.new_packaging_code,
                    new_packaging_type: request.value.new_packaging_type,
                    occurred_at: request.value.occurred_at,
                    product_id: request.value.product_id,
                    quantity: request.value.quantity,
                    source_packaging_id: request.value.source_packaging_id,
                    status: request.value.status,
                    to_location_id: request.value.to_location_id,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::SplitItem { request_id, input })
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

pub(crate) fn encode(output: &[contract::SplitOutcome]) -> String {
    let values = output.iter().map(|item| {
        match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "transaction_ids": value.transaction_ids.iter().map(|element| json!(element)).collect::<Vec<_>>(),
                    "source_packaging_id": value.source_packaging_id,
                    "new_packaging_id": value.new_packaging_id,
                    "source_status": value.source_status,
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

fn error_value(error: &contract::SplitError) -> Value {
    let (code, detail) = match error {
        contract::SplitError::InvalidInput(value) => {
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
        contract::SplitError::PackagingNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("packaging_not_found", detail)
        }
        contract::SplitError::LocationNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("location_not_found", detail)
        }
        contract::SplitError::QuantityNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("quantity_not_found", detail)
        }
        contract::SplitError::InsufficientQuantity(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            if let Some(detail_value) = &value.maximum {
                detail.insert("maximum".to_owned(), json!(detail_value));
            }
            if let Some(detail_value) = &value.observed {
                detail.insert("observed".to_owned(), json!(detail_value));
            }
            ("insufficient_quantity", detail)
        }
        contract::SplitError::ConcurrencyConflict(value) => {
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
        contract::SplitError::IdempotencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("idempotency_conflict", detail)
        }
        contract::SplitError::Retry => ("retry", Map::new()),
        contract::SplitError::Timeout => ("timeout", Map::new()),
        contract::SplitError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::SplitError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::SplitRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    {
        let value = &mut request.new_packaging_type;
        if !["bin", "case", "loose", "pallet", "tote"].contains(&value.as_str()) {
            return Err(invalid("value.new_packaging_type"));
        }
    }
    {
        let value = &mut request.product_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.product_id"));
        }
    }
    {
        let value = &mut request.source_packaging_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.source_packaging_id"));
        }
    }
    {
        let value = &mut request.status;
        if !["available", "held"].contains(&value.as_str()) {
            return Err(invalid("value.status"));
        }
    }
    {
        let value = &mut request.to_location_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.to_location_id"));
        }
    }
    Ok(())
}

include!("write_log_codec.rs");

/// The write log operation of this contract: its operation without the version.
const OPERATION: &str = "wamn-wms:inventory/split";
/// The input field that carries the key the write log claims.
const KEY_FIELD: &str = "value.idempotency_key";

/// The bytes the write log keeps for one validated request: its canonical JSON
/// without the key and the request id, and the participation intent when one
/// is selected.
fn request_bytes(request: &contract::SplitRequest, intent: Option<&str>) -> Vec<u8> {
    let mut value = {
        let mut value = Map::new();
        value.insert(
            "expected_row_version".to_owned(),
            json!(&request.expected_row_version),
        );
        value.insert(
            "new_packaging_code".to_owned(),
            json!(&request.new_packaging_code),
        );
        value.insert(
            "new_packaging_type".to_owned(),
            json!(&request.new_packaging_type),
        );
        value.insert(
            "occurred_at".to_owned(),
            json!(
                wamn_execution_contract::canonical_timestamptz(&request.occurred_at)
                    .unwrap_or_else(|| request.occurred_at.clone())
            ),
        );
        value.insert("product_id".to_owned(), json!(&request.product_id));
        value.insert(
            "quantity".to_owned(),
            json!(
                wamn_execution_contract::canonical_numeric(&request.quantity)
                    .unwrap_or_else(|| request.quantity.clone())
            ),
        );
        value.insert(
            "source_packaging_id".to_owned(),
            json!(&request.source_packaging_id),
        );
        value.insert("status".to_owned(), json!(&request.status));
        value.insert("to_location_id".to_owned(), json!(&request.to_location_id));
        Value::Object(value)
    };
    if let (Some(intent), Value::Object(object)) = (intent, &mut value) {
        object.insert("participation_intent".to_owned(), json!(intent));
    }
    wamn_execution_contract::canonical_json_bytes(&value)
}

/// The result the write log stores for one success: its encoded outcome
/// without the request id.
fn stored_result(result: &contract::SplitResult) -> String {
    let encoded = encode(&[contract::SplitOutcome {
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
    source_packaging_id: String,
    new_packaging_id: String,
    source_status: String,
    row_version: i32,
}

/// The success that one stored result answers.
fn stored_success(result: &str) -> Option<contract::SplitResult> {
    let value: JsonResult = serde_json::from_str(result).ok()?;
    Some(contract::SplitResult {
        transaction_ids: value.transaction_ids,
        source_packaging_id: value.source_packaging_id,
        new_packaging_id: value.new_packaging_id,
        source_status: value.source_status,
        row_version: value.row_version,
    })
}

#[allow(dead_code)]
pub(crate) async fn run<F>(
    input: Vec<contract::SplitItem>,
    connection: &mut wamn_postgres_statements::Connection,
    mut handler: F,
) -> Vec<contract::SplitOutcome>
where
    F: AsyncFnMut(
        &mut wamn_postgres_statements::Transaction,
        contract::SplitRequest,
    ) -> Result<contract::SplitResult, contract::SplitError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => claimed(connection, request, &mut handler).await,
                Err(error) => Err(contract::SplitError::InvalidInput(error)),
            },
            Err(error) => Err(contract::SplitError::InvalidInput(error)),
        };
        output.push(contract::SplitOutcome {
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
    request: contract::SplitRequest,
    handler: &mut F,
) -> Result<contract::SplitResult, contract::SplitError>
where
    F: AsyncFnMut(
        &mut wamn_postgres_statements::Transaction,
        contract::SplitRequest,
    ) -> Result<contract::SplitResult, contract::SplitError>,
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
            source_packaging_id: row.source_packaging_id.0,
            new_packaging_id: row.new_packaging_id.0,
            source_status: row.source_status,
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
) -> contract::SplitError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::SplitError::InternalError;
            };
            let minimum = detail("minimum");
            let maximum = detail("maximum");
            let observed = detail("observed");
            contract::SplitError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "packaging_not_found" => {
            let Some(field) = detail("field") else {
                return contract::SplitError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::SplitError::InternalError;
            };
            contract::SplitError::PackagingNotFound(contract::PackagingNotFoundDetail { field, id })
        }
        "location_not_found" => {
            let Some(field) = detail("field") else {
                return contract::SplitError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::SplitError::InternalError;
            };
            contract::SplitError::LocationNotFound(contract::LocationNotFoundDetail { field, id })
        }
        "quantity_not_found" => {
            let Some(field) = detail("field") else {
                return contract::SplitError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::SplitError::InternalError;
            };
            contract::SplitError::QuantityNotFound(contract::QuantityNotFoundDetail { field, id })
        }
        "insufficient_quantity" => {
            let Some(field) = detail("field") else {
                return contract::SplitError::InternalError;
            };
            let maximum = detail("maximum");
            let observed = detail("observed");
            contract::SplitError::InsufficientQuantity(contract::InsufficientQuantityDetail {
                field,
                maximum,
                observed,
            })
        }
        "concurrency_conflict" => {
            let Some(expected_row_version) =
                detail("expected_row_version").and_then(|value| value.parse::<i32>().ok())
            else {
                return contract::SplitError::InternalError;
            };
            let Some(observed_row_version) =
                detail("observed_row_version").and_then(|value| value.parse::<i32>().ok())
            else {
                return contract::SplitError::InternalError;
            };
            contract::SplitError::ConcurrencyConflict(contract::ConcurrencyConflictDetail {
                expected_row_version,
                observed_row_version,
            })
        }
        "idempotency_conflict" => {
            let Some(field) = detail("field") else {
                return contract::SplitError::InternalError;
            };
            contract::SplitError::IdempotencyConflict(contract::IdempotencyConflictDetail { field })
        }
        "retry" => contract::SplitError::Retry,
        "timeout" => contract::SplitError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::SplitError::InternalError;
            };
            contract::SplitError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::SplitError::InternalError,
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
                    input: Vec<__contract::SplitItem>,
                ) -> Result<Vec<__contract::SplitOutcome>, __node::NodeError> {
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
