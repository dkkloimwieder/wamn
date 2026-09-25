// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::ListItem;
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
struct JsonRequest {}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::ListItem>, CodecError> {
    decode_read_envelope(input)?
        .into_iter()
        .map(|body| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| {
                    let _ = request;
                    contract::ListRequest::Request
                })
                .map_err(|_| invalid("input"));
            Ok(contract::ListItem { input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::ListOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "value": { "rows": value.rows.iter().map(|row| json!({
                    "id": row.id,
                    "name": row.name,
                })).collect::<Vec<_>>() }
            }),
            Err(error) => json!({
                "error": error_value(error),
            }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed outcomes always serialize")
}

fn error_value(error: &contract::ListError) -> Value {
    let (code, detail) = match error {
        contract::ListError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::ListError::Retry => ("retry", Map::new()),
        contract::ListError::Timeout => ("timeout", Map::new()),
        contract::ListError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::ListError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::ListRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::ListItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::ListOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::ListRequest,
    ) -> Result<contract::ListResult, contract::ListError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::ListError::InvalidInput(error)),
            },
            Err(error) => Err(contract::ListError::InvalidInput(error)),
        };
        output.push(contract::ListOutcome { outcome });
    }
    output
}

#[allow(unused_macros)]
macro_rules! row {
    ($row:expr, $target:path) => {{
        let row = $row;
        $target {
            id: row.id.0,
            name: row.name,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::ListError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::ListError::InternalError;
            };
            contract::ListError::InvalidInput(contract::InvalidInputDetail { field })
        }
        "retry" => contract::ListError::Retry,
        "timeout" => contract::ListError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::ListError::InternalError;
            };
            contract::ListError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::ListError::InternalError,
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
                    input: Vec<__contract::ListItem>,
                ) -> Result<Vec<__contract::ListOutcome>, __node::NodeError> {
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
