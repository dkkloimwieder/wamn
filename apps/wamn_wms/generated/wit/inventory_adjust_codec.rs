// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::AdjustItem;
const MINIMUM: usize = 1;
const MAXIMUM: usize = 100;
const COUNT_ERROR: &str = "operation input item count must be 1..=100";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    value: JsonRoot,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRoot {
    expected_row_version: JsonInt64,
    idempotency_key: String,
    occurred_at: String,
    pallet_id: String,
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
                    expected_row_version: request.value.expected_row_version.0,
                    idempotency_key: request.value.idempotency_key,
                    occurred_at: request.value.occurred_at,
                    pallet_id: request.value.pallet_id,
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
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "movement_id": value.movement_id,
                    "pallet_id": value.pallet_id,
                    "adjusted_quantity": value.adjusted_quantity,
                    "pallet_status": value.pallet_status,
                    "row_version": value.row_version.to_string(),
                }
            }),
            Err(error) => json!({
                "request_id": item.request_id,
                "error": error_value(error),
            }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed receipt outcomes always serialize")
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
        contract::AdjustError::PalletNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("pallet_not_found", detail)
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
                json!(JsonInt64(value.expected_row_version)),
            );
            detail.insert(
                "observed_row_version".to_owned(),
                json!(JsonInt64(value.observed_row_version)),
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
        let value = &mut request.pallet_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.pallet_id"));
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

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::AdjustItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::AdjustOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::AdjustRequest,
    ) -> Result<contract::AdjustResult, contract::AdjustError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
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

#[allow(unused_macros)]
macro_rules! row {
    ($row:expr, $target:path) => {{
        let row = $row;
        $target {
            movement_id: row.movement_id.0,
            pallet_id: row.pallet_id.0,
            adjusted_quantity: row.adjusted_quantity.0,
            pallet_status: row.pallet_status,
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
            let Ok(minimum) = detail("minimum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::AdjustError::InternalError;
            };
            let Ok(maximum) = detail("maximum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::AdjustError::InternalError;
            };
            let Ok(observed) = detail("observed")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::AdjustError::InternalError;
            };
            contract::AdjustError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "pallet_not_found" => {
            let Some(field) = detail("field") else {
                return contract::AdjustError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::AdjustError::InternalError;
            };
            contract::AdjustError::PalletNotFound(contract::PalletNotFoundDetail { field, id })
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
                detail("expected_row_version").and_then(|value| value.parse::<i64>().ok())
            else {
                return contract::AdjustError::InternalError;
            };
            let Some(observed_row_version) =
                detail("observed_row_version").and_then(|value| value.parse::<i64>().ok())
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
