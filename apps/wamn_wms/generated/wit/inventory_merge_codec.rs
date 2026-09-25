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
    expected_from_row_version: i32,
    expected_to_row_version: i32,
    from_inventory_id: String,
    idempotency_key: String,
    occurred_at: String,
    to_inventory_id: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::MergeItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::MergeRequest {
                    expected_from_row_version: request.value.expected_from_row_version,
                    expected_to_row_version: request.value.expected_to_row_version,
                    from_inventory_id: request.value.from_inventory_id,
                    idempotency_key: request.value.idempotency_key,
                    occurred_at: request.value.occurred_at,
                    to_inventory_id: request.value.to_inventory_id,
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
                    "from_inventory_id": value.from_inventory_id,
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
        contract::MergeError::NotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("not_found", detail)
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
        let value = &mut request.from_inventory_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.from_inventory_id"));
        }
    }
    {
        let value = &mut request.to_inventory_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.to_inventory_id"));
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::MergeItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::MergeOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::MergeRequest,
    ) -> Result<contract::MergeResult, contract::MergeError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
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
            from_inventory_id: row.from_inventory_id.0,
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
            let Ok(minimum) = detail("minimum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::MergeError::InternalError;
            };
            let Ok(maximum) = detail("maximum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::MergeError::InternalError;
            };
            let Ok(observed) = detail("observed")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::MergeError::InternalError;
            };
            contract::MergeError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "not_found" => {
            let Some(field) = detail("field") else {
                return contract::MergeError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::MergeError::InternalError;
            };
            contract::MergeError::NotFound(contract::NotFoundDetail { field, id })
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
