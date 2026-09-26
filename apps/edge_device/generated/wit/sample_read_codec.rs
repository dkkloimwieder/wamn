// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::ReadItem;
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
    captured_at: String,
    frame: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::ReadItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::ReadRequest {
                    captured_at: request.value.captured_at,
                    frame: request.value.frame,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::ReadItem { request_id, input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::ReadOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "frame": value.frame,
                    "captured_at": value.captured_at,
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

fn error_value(error: &contract::ReadError) -> Value {
    let (code, detail) = match error {
        contract::ReadError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::ReadError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::ReadError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::ReadRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::ReadItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::ReadOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::ReadRequest,
    ) -> Result<contract::ReadResult, contract::ReadError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::ReadError::InvalidInput(error)),
            },
            Err(error) => Err(contract::ReadError::InvalidInput(error)),
        };
        output.push(contract::ReadOutcome {
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
            frame: row.frame,
            captured_at: row.captured_at.0,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::ReadError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::ReadError::InternalError;
            };
            contract::ReadError::InvalidInput(contract::InvalidInputDetail { field })
        }
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::ReadError::InternalError;
            };
            contract::ReadError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::ReadError::InternalError,
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
                    input: Vec<__contract::ReadItem>,
                ) -> Result<Vec<__contract::ReadOutcome>, __node::NodeError> {
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
