// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::ApproveInspectionItem;
const MINIMUM: usize = 1;
const MAXIMUM: usize = 100;
const COUNT_ERROR: &str = "operation input item count must be 1..=100";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    expected_row_version: JsonInt64,
    receipt_id: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::ApproveInspectionItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::ApproveInspectionRequest {
                    expected_row_version: request.expected_row_version.0,
                    receipt_id: request.receipt_id,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::ApproveInspectionItem { request_id, input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::ApproveInspectionOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "receipt_id": value.receipt_id,
                    "status": value.status,
                    "row_version": value.row_version.to_string(),
                    "purchase_order_id": value.purchase_order_id,
                    "purchase_order_row_version": value.purchase_order_row_version,
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

fn error_value(error: &contract::ApproveInspectionError) -> Value {
    let (code, detail) = match error {
        contract::ApproveInspectionError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::ApproveInspectionError::NotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("not_found", detail)
        }
        contract::ApproveInspectionError::ConcurrencyConflict(value) => {
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
        contract::ApproveInspectionError::Retry => ("retry", Map::new()),
        contract::ApproveInspectionError::Timeout => ("timeout", Map::new()),
        contract::ApproveInspectionError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::ApproveInspectionError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(
    request: &mut contract::ApproveInspectionRequest,
) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    {
        let value = &mut request.receipt_id;
        if !canonical_uuid(value) {
            return Err(invalid("receipt_id"));
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::ApproveInspectionItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::ApproveInspectionOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::ApproveInspectionRequest,
    )
        -> Result<contract::ApproveInspectionResult, contract::ApproveInspectionError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::ApproveInspectionError::InvalidInput(error)),
            },
            Err(error) => Err(contract::ApproveInspectionError::InvalidInput(error)),
        };
        output.push(contract::ApproveInspectionOutcome {
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
            receipt_id: row.receipt_id.0,
            status: row.status,
            row_version: row.row_version,
            purchase_order_id: row.purchase_order_id.0,
            purchase_order_row_version: row.purchase_order_row_version,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::ApproveInspectionError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::ApproveInspectionError::InternalError;
            };
            contract::ApproveInspectionError::InvalidInput(contract::InvalidInputDetail { field })
        }
        "not_found" => {
            let Some(field) = detail("field") else {
                return contract::ApproveInspectionError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::ApproveInspectionError::InternalError;
            };
            contract::ApproveInspectionError::NotFound(contract::NotFoundDetail { field, id })
        }
        "concurrency_conflict" => {
            let Some(expected_row_version) =
                detail("expected_row_version").and_then(|value| value.parse::<i64>().ok())
            else {
                return contract::ApproveInspectionError::InternalError;
            };
            let Some(observed_row_version) =
                detail("observed_row_version").and_then(|value| value.parse::<i64>().ok())
            else {
                return contract::ApproveInspectionError::InternalError;
            };
            contract::ApproveInspectionError::ConcurrencyConflict(
                contract::ConcurrencyConflictDetail {
                    expected_row_version,
                    observed_row_version,
                },
            )
        }
        "retry" => contract::ApproveInspectionError::Retry,
        "timeout" => contract::ApproveInspectionError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::ApproveInspectionError::InternalError;
            };
            contract::ApproveInspectionError::PermissionDenied(contract::PermissionDeniedDetail {
                operation,
            })
        }
        _ => contract::ApproveInspectionError::InternalError,
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
                    input: Vec<__contract::ApproveInspectionItem>,
                ) -> Result<Vec<__contract::ApproveInspectionOutcome>, __node::NodeError> {
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
