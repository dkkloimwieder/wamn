// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::DeleteItem;
const MINIMUM: usize = 1;
const MAXIMUM: usize = 100;
const COUNT_ERROR: &str = "operation input item count must be 1..=100";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    id: String,
    expected_edit_version: JsonInt64,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::DeleteItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::DeleteRequest {
                    id: request.id,
                    expected_edit_version: request.expected_edit_version.0,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::DeleteItem { request_id, input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::DeleteOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({ "request_id": item.request_id, "value":
            json!({
                                "outcome": value.value.outcome,
                        })
                    }),
            Err(error) => json!({ "request_id": item.request_id, "error": error_value(error) }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed operation outcomes always serialize")
}

fn error_value(error: &contract::DeleteError) -> Value {
    let (code, detail) = match error {
        contract::DeleteError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::DeleteError::NotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("not_found", detail)
        }
        contract::DeleteError::ConcurrencyConflict(value) => {
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
        contract::DeleteError::Retry => ("retry", Map::new()),
        contract::DeleteError::Timeout => ("timeout", Map::new()),
        contract::DeleteError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::DeleteError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::DeleteRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    if !canonical_uuid(&mut request.id) {
        return Err(invalid("id"));
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::DeleteItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::DeleteOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::DeleteRequest,
    ) -> Result<contract::DeleteResult, contract::DeleteError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::DeleteError::InvalidInput(error)),
            },
            Err(error) => Err(contract::DeleteError::InvalidInput(error)),
        };
        output.push(contract::DeleteOutcome {
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
            outcome: row.outcome,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::DeleteError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::DeleteError::InternalError;
            };
            contract::DeleteError::InvalidInput(contract::InvalidInputDetail { field })
        }
        "not_found" => {
            let Some(field) = detail("field") else {
                return contract::DeleteError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::DeleteError::InternalError;
            };
            contract::DeleteError::NotFound(contract::NotFoundDetail { field, id })
        }
        "concurrency_conflict" => {
            let Some(expected_row_version) =
                detail("expected_row_version").and_then(|value| value.parse::<i64>().ok())
            else {
                return contract::DeleteError::InternalError;
            };
            let Some(observed_row_version) =
                detail("observed_row_version").and_then(|value| value.parse::<i64>().ok())
            else {
                return contract::DeleteError::InternalError;
            };
            contract::DeleteError::ConcurrencyConflict(contract::ConcurrencyConflictDetail {
                expected_row_version,
                observed_row_version,
            })
        }
        "retry" => contract::DeleteError::Retry,
        "timeout" => contract::DeleteError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::DeleteError::InternalError;
            };
            contract::DeleteError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::DeleteError::InternalError,
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
                    input: Vec<__contract::DeleteItem>,
                ) -> Result<Vec<__contract::DeleteOutcome>, __node::NodeError> {
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
