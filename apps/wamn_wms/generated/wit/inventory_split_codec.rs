// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::SplitItem;
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
    new_pallet_code: String,
    occurred_at: String,
    product_id: String,
    quantity: String,
    source_pallet_id: String,
    status: String,
    to_location_id: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::SplitItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::SplitRequest {
                    expected_row_version: request.value.expected_row_version.0,
                    idempotency_key: request.value.idempotency_key,
                    new_pallet_code: request.value.new_pallet_code,
                    occurred_at: request.value.occurred_at,
                    product_id: request.value.product_id,
                    quantity: request.value.quantity,
                    source_pallet_id: request.value.source_pallet_id,
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
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "movement_id": value.movement_id,
                    "source_pallet_id": value.source_pallet_id,
                    "new_pallet_id": value.new_pallet_id,
                    "source_status": value.source_status,
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
        contract::SplitError::PalletNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("pallet_not_found", detail)
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
                json!(JsonInt64(value.expected_row_version)),
            );
            detail.insert(
                "observed_row_version".to_owned(),
                json!(JsonInt64(value.observed_row_version)),
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
        let value = &mut request.product_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.product_id"));
        }
    }
    {
        let value = &mut request.source_pallet_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.source_pallet_id"));
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

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::SplitItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::SplitOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::SplitRequest,
    ) -> Result<contract::SplitResult, contract::SplitError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
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

#[allow(unused_macros)]
macro_rules! row {
    ($row:expr, $target:path) => {{
        let row = $row;
        $target {
            movement_id: row.movement_id.0,
            source_pallet_id: row.source_pallet_id.0,
            new_pallet_id: row.new_pallet_id.0,
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
            let Ok(minimum) = detail("minimum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::SplitError::InternalError;
            };
            let Ok(maximum) = detail("maximum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::SplitError::InternalError;
            };
            let Ok(observed) = detail("observed")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::SplitError::InternalError;
            };
            contract::SplitError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "pallet_not_found" => {
            let Some(field) = detail("field") else {
                return contract::SplitError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::SplitError::InternalError;
            };
            contract::SplitError::PalletNotFound(contract::PalletNotFoundDetail { field, id })
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
            let Ok(maximum) = detail("maximum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::SplitError::InternalError;
            };
            let Ok(observed) = detail("observed")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::SplitError::InternalError;
            };
            contract::SplitError::InsufficientQuantity(contract::InsufficientQuantityDetail {
                field,
                maximum,
                observed,
            })
        }
        "concurrency_conflict" => {
            let Some(expected_row_version) =
                detail("expected_row_version").and_then(|value| value.parse::<i64>().ok())
            else {
                return contract::SplitError::InternalError;
            };
            let Some(observed_row_version) =
                detail("observed_row_version").and_then(|value| value.parse::<i64>().ok())
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
