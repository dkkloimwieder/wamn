// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::CreateItem;
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
    code: String,
    idempotency_key: String,
    location_id: String,
    r#type: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::CreateItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::CreateRequest {
                    code: request.value.code,
                    idempotency_key: request.value.idempotency_key,
                    location_id: request.value.location_id,
                    type_: request.value.r#type,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::CreateItem { request_id, input })
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

pub(crate) fn encode(output: &[contract::CreateOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "operation_id": value.operation_id,
                    "packaging_id": value.packaging_id,
                    "type": value.type_,
                    "code": value.code,
                    "location_id": value.location_id,
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

fn error_value(error: &contract::CreateError) -> Value {
    let (code, detail) = match error {
        contract::CreateError::InvalidInput(value) => {
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
        contract::CreateError::NotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("not_found", detail)
        }
        contract::CreateError::ConcurrencyConflict(value) => {
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
        contract::CreateError::IdempotencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("idempotency_conflict", detail)
        }
        contract::CreateError::Retry => ("retry", Map::new()),
        contract::CreateError::Timeout => ("timeout", Map::new()),
        contract::CreateError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::CreateError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::CreateRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    {
        let value = &mut request.location_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.location_id"));
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::CreateItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::CreateOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::CreateRequest,
    ) -> Result<contract::CreateResult, contract::CreateError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::CreateError::InvalidInput(error)),
            },
            Err(error) => Err(contract::CreateError::InvalidInput(error)),
        };
        output.push(contract::CreateOutcome {
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
            packaging_id: row.packaging_id.0,
            type_: row.r#type,
            code: row.code,
            location_id: row.location_id.0,
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
) -> contract::CreateError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::CreateError::InternalError;
            };
            let Ok(minimum) = detail("minimum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::CreateError::InternalError;
            };
            let Ok(maximum) = detail("maximum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::CreateError::InternalError;
            };
            let Ok(observed) = detail("observed")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "not_found" => {
            let Some(field) = detail("field") else {
                return contract::CreateError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::NotFound(contract::NotFoundDetail { field, id })
        }
        "concurrency_conflict" => {
            let Some(expected_row_version) =
                detail("expected_row_version").and_then(|value| value.parse::<i32>().ok())
            else {
                return contract::CreateError::InternalError;
            };
            let Some(observed_row_version) =
                detail("observed_row_version").and_then(|value| value.parse::<i32>().ok())
            else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::ConcurrencyConflict(contract::ConcurrencyConflictDetail {
                expected_row_version,
                observed_row_version,
            })
        }
        "idempotency_conflict" => {
            let Some(field) = detail("field") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::IdempotencyConflict(contract::IdempotencyConflictDetail {
                field,
            })
        }
        "retry" => contract::CreateError::Retry,
        "timeout" => contract::CreateError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::CreateError::InternalError,
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
                    input: Vec<__contract::CreateItem>,
                ) -> Result<Vec<__contract::CreateOutcome>, __node::NodeError> {
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
