// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::GetItem;
const MINIMUM: usize = 1;
const MAXIMUM: usize = 100;
const COUNT_ERROR: &str = "operation input item count must be 1..=100";

#[allow(dead_code)]
pub(crate) fn validate(input: &[Item]) -> Result<(), CodecError> {
    validate_count(input.len())?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    id: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::GetItem>, CodecError> {
    decode_read_envelope(input)?
        .into_iter()
        .map(|body| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::GetRequest { id: request.id })
                .map_err(|_| invalid("input"));
            Ok(contract::GetItem { input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::GetOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({ "value":
            json!({
                                "created_at": value.value.created_at,
                                "id": value.value.id,
                                "location_code": value.value.location_code,
                                "row_version": value.value.row_version,
                        })
                    }),
            Err(error) => json!({ "error": error_value(error) }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed operation outcomes always serialize")
}

fn error_value(error: &contract::GetError) -> Value {
    let (code, detail) = match error {
        contract::GetError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::GetError::NotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("not_found", detail)
        }
        contract::GetError::Retry => ("retry", Map::new()),
        contract::GetError::Timeout => ("timeout", Map::new()),
        contract::GetError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::GetError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::GetRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    if !canonical_uuid(&mut request.id) {
        return Err(invalid("id"));
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::GetItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::GetOutcome>
where
    F: AsyncFnMut(&mut S, contract::GetRequest) -> Result<contract::GetResult, contract::GetError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::GetError::InvalidInput(error)),
            },
            Err(error) => Err(contract::GetError::InvalidInput(error)),
        };
        output.push(contract::GetOutcome { outcome });
    }
    output
}

#[allow(unused_macros)]
macro_rules! row {
    ($row:expr, $target:path) => {{
        let row = $row;
        $target {
            created_at: row.created_at.0,
            id: row.id.0,
            location_code: row.location_code,
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
) -> contract::GetError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::GetError::InternalError;
            };
            contract::GetError::InvalidInput(contract::InvalidInputDetail { field })
        }
        "not_found" => {
            let Some(field) = detail("field") else {
                return contract::GetError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::GetError::InternalError;
            };
            contract::GetError::NotFound(contract::NotFoundDetail { field, id })
        }
        "retry" => contract::GetError::Retry,
        "timeout" => contract::GetError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::GetError::InternalError;
            };
            contract::GetError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::GetError::InternalError,
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
                    input: Vec<__contract::GetItem>,
                ) -> Result<Vec<__contract::GetOutcome>, __node::NodeError> {
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
