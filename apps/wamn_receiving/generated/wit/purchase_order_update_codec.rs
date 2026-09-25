// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::UpdateItem;
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
    id: String,
    expected_row_version: i64,
    change: JsonUpdateChange,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonUpdateChange {
    #[serde(default)]
    supplier_id: JsonChange<String>,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::UpdateItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = match serde_json::from_value::<JsonRequest>(body) {
                Ok(request) => match i32::try_from(request.expected_row_version) {
                    Ok(expected_row_version) => {
                        let request = contract::UpdateRequest {
                            id: request.id,
                            expected_row_version,
                            change: contract::UpdateChange {
                                supplier_id: change(request.change.supplier_id),
                            },
                        };
                        Ok(request)
                    }
                    Err(_) => Err(invalid("expected_row_version")),
                },
                Err(_) => Err(invalid("input")),
            };
            Ok(contract::UpdateItem { request_id, input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::UpdateOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "created_at": value.created_at,
                    "created_by": value.created_by,
                    "id": value.id,
                    "purchase_order_number": value.purchase_order_number,
                    "row_version": value.row_version,
                    "status": value.status,
                    "supplier_id": value.supplier_id,
                    "updated_at": value.updated_at,
                    "updated_by": value.updated_by,
                }
            }),
            Err(error) => json!({
                "request_id": item.request_id,
                "error": error_value(error),
            }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed update outcomes always serialize")
}

fn error_value(error: &contract::UpdateError) -> Value {
    let (code, detail) = match error {
        contract::UpdateError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::UpdateError::NotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("not_found", detail)
        }
        contract::UpdateError::ConcurrencyConflict(value) => {
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
        contract::UpdateError::ForeignKeyViolation(value) => {
            let mut detail = Map::new();
            detail.insert("constraint".to_owned(), json!(value.constraint));
            if let Some(field) = match value.constraint.as_str() {
                "purchase_order_supplier_id_fkey" => Some("change.supplier_id"),
                _ => None,
            } {
                detail.insert("field".to_owned(), json!(field));
            }
            ("foreign_key_violation", detail)
        }
        contract::UpdateError::Retry => ("retry", Map::new()),
        contract::UpdateError::Timeout => ("timeout", Map::new()),
        contract::UpdateError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::UpdateError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::UpdateRequest) -> Result<(), contract::InvalidInputDetail> {
    if !canonical_uuid(&mut request.id) {
        return Err(invalid("id"));
    }
    if matches!(request.change.supplier_id, Some(None)) {
        return Err(invalid("change.supplier_id"));
    }
    if let Some(Some(value)) = &mut request.change.supplier_id
        && (!canonical_uuid(value))
    {
        return Err(invalid("change.supplier_id"));
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::UpdateItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::UpdateOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::UpdateRequest,
    ) -> Result<contract::UpdateResult, contract::UpdateError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::UpdateError::InvalidInput(error)),
            },
            Err(error) => Err(contract::UpdateError::InvalidInput(error)),
        };
        output.push(contract::UpdateOutcome {
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
            created_at: row.created_at.0,
            created_by: row.created_by.0,
            id: row.id.0,
            purchase_order_number: row.purchase_order_number,
            row_version: row.row_version,
            status: row.status,
            supplier_id: row.supplier_id.0,
            updated_at: row.updated_at.0,
            updated_by: row.updated_by.0,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::UpdateError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::UpdateError::InternalError;
            };
            contract::UpdateError::InvalidInput(contract::InvalidInputDetail { field })
        }
        "not_found" => {
            let Some(field) = detail("field") else {
                return contract::UpdateError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::UpdateError::InternalError;
            };
            contract::UpdateError::NotFound(contract::NotFoundDetail { field, id })
        }
        "concurrency_conflict" => {
            let Some(expected_row_version) =
                detail("expected_row_version").and_then(|value| value.parse::<i32>().ok())
            else {
                return contract::UpdateError::InternalError;
            };
            let Some(observed_row_version) =
                detail("observed_row_version").and_then(|value| value.parse::<i32>().ok())
            else {
                return contract::UpdateError::InternalError;
            };
            contract::UpdateError::ConcurrencyConflict(contract::ConcurrencyConflictDetail {
                expected_row_version,
                observed_row_version,
            })
        }
        "foreign_key_violation" => {
            let Some(constraint) = detail("constraint") else {
                return contract::UpdateError::InternalError;
            };
            contract::UpdateError::ForeignKeyViolation(contract::ForeignKeyViolationDetail {
                constraint,
            })
        }
        "retry" => contract::UpdateError::Retry,
        "timeout" => contract::UpdateError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::UpdateError::InternalError;
            };
            contract::UpdateError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::UpdateError::InternalError,
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
                    input: Vec<__contract::UpdateItem>,
                ) -> Result<Vec<__contract::UpdateOutcome>, __node::NodeError> {
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
