// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::CreateItem;
const MINIMUM: usize = 1;
const MAXIMUM: usize = 100;
const COUNT_ERROR: &str = "operation input item count must be 1..=100";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    idempotency_key: String,
    #[serde(default)]
    pallet_code: JsonChange<String>,
    #[serde(default)]
    location_id: JsonChange<String>,
    #[serde(default)]
    status: JsonChange<String>,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::CreateItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::CreateRequest {
                    idempotency_key: request.idempotency_key,
                    pallet_code: change(request.pallet_code),
                    location_id: change(request.location_id),
                    status: change(request.status),
                })
                .map_err(|_| invalid("input"));
            Ok(contract::CreateItem { request_id, input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::CreateOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({ "request_id": item.request_id, "value":
            json!({
                                "created_at": value.value.created_at,
                                "created_by": value.value.created_by,
                                "id": value.value.id,
                                "location_id": value.value.location_id,
                                "pallet_code": value.value.pallet_code,
                                "row_version": value.value.row_version.to_string(),
                                "status": value.value.status,
                                "updated_at": value.value.updated_at,
                                "updated_by": value.value.updated_by,
                        })
                    }),
            Err(error) => json!({ "request_id": item.request_id, "error": error_value(error) }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed operation outcomes always serialize")
}

fn error_value(error: &contract::CreateError) -> Value {
    let (code, detail) = match error {
        contract::CreateError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::CreateError::IdempotencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("idempotency_conflict", detail)
        }
        contract::CreateError::UniqueViolation(value) => {
            let mut detail = Map::new();
            detail.insert("constraint".to_owned(), json!(value.constraint));
            ("unique_violation", detail)
        }
        contract::CreateError::ForeignKeyViolation(value) => {
            let mut detail = Map::new();
            detail.insert("constraint".to_owned(), json!(value.constraint));
            ("foreign_key_violation", detail)
        }
        contract::CreateError::CheckViolation(value) => {
            let mut detail = Map::new();
            detail.insert("constraint".to_owned(), json!(value.constraint));
            ("check_violation", detail)
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
    if matches!(request.pallet_code, Some(None)) {
        return Err(invalid("pallet_code"));
    }
    if matches!(request.location_id, Some(None)) {
        return Err(invalid("location_id"));
    }
    if let Some(Some(value)) = &mut request.location_id
        && (!canonical_uuid(value))
    {
        return Err(invalid("location_id"));
    }
    if matches!(request.status, Some(None)) {
        return Err(invalid("status"));
    }
    if let Some(Some(value)) = &mut request.status
        && (!["available", "held", "consumed"].contains(&value.as_str()))
    {
        return Err(invalid("status"));
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
            created_at: row.created_at.0,
            created_by: row.created_by.0,
            id: row.id.0,
            location_id: row.location_id.0,
            pallet_code: row.pallet_code,
            row_version: row.row_version,
            status: row.status,
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
) -> contract::CreateError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::InvalidInput(contract::InvalidInputDetail { field })
        }
        "idempotency_conflict" => {
            let Some(field) = detail("field") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::IdempotencyConflict(contract::IdempotencyConflictDetail {
                field,
            })
        }
        "unique_violation" => {
            let Some(constraint) = detail("constraint") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::UniqueViolation(contract::UniqueViolationDetail { constraint })
        }
        "foreign_key_violation" => {
            let Some(constraint) = detail("constraint") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::ForeignKeyViolation(contract::ForeignKeyViolationDetail {
                constraint,
            })
        }
        "check_violation" => {
            let Some(constraint) = detail("constraint") else {
                return contract::CreateError::InternalError;
            };
            contract::CreateError::CheckViolation(contract::CheckViolationDetail { constraint })
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
