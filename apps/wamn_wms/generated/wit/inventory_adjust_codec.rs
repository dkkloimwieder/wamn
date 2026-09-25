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
    inventory_id: String,
    occurred_at: String,
    reason: String,
    to_quantity: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::AdjustItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::AdjustRequest {
                    expected_row_version: request.value.expected_row_version,
                    idempotency_key: request.value.idempotency_key,
                    inventory_id: request.value.inventory_id,
                    occurred_at: request.value.occurred_at,
                    reason: request.value.reason,
                    to_quantity: request.value.to_quantity,
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
                    "operation_id": value.operation_id,
                    "inventory_id": value.inventory_id,
                    "product_id": value.product_id,
                    "packaging_id": value.packaging_id,
                    "location_id": value.location_id,
                    "quantity": value.quantity,
                    "disposition": value.disposition,
                    "lifecycle": value.lifecycle,
                    "row_version": value.row_version,
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
        contract::AdjustError::NotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("not_found", detail)
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
        let value = &mut request.inventory_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.inventory_id"));
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
            operation_id: row.operation_id.0,
            inventory_id: row.inventory_id.0,
            product_id: row.product_id.0,
            packaging_id: row.packaging_id.0,
            location_id: row.location_id.0,
            quantity: row.quantity.0,
            disposition: row.disposition,
            lifecycle: row.lifecycle,
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
        "not_found" => {
            let Some(field) = detail("field") else {
                return contract::AdjustError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::AdjustError::InternalError;
            };
            contract::AdjustError::NotFound(contract::NotFoundDetail { field, id })
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
