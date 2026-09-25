// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::AggregateItem;
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

pub(crate) fn decode(input: &str) -> Result<Vec<contract::AggregateItem>, CodecError> {
    decode_read_envelope(input)?
        .into_iter()
        .map(|body| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| {
                    let _ = request;
                    contract::AggregateRequest::Request
                })
                .map_err(|_| invalid("input"));
            Ok(contract::AggregateItem { input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::AggregateOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "value": { "rows": value.rows.iter().map(|row| json!({
                    "product_id": row.product_id,
                    "location_id": row.location_id,
                    "disposition": row.disposition,
                    "quantity": row.quantity,
                    "packaging_count": row.packaging_count,
                })).collect::<Vec<_>>() }
            }),
            Err(error) => json!({
                "error": error_value(error),
            }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed outcomes always serialize")
}

fn error_value(error: &contract::AggregateError) -> Value {
    let (code, detail) = match error {
        contract::AggregateError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::AggregateError::Retry => ("retry", Map::new()),
        contract::AggregateError::Timeout => ("timeout", Map::new()),
        contract::AggregateError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::AggregateError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::AggregateRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::AggregateItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::AggregateOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::AggregateRequest,
    ) -> Result<contract::AggregateResult, contract::AggregateError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::AggregateError::InvalidInput(error)),
            },
            Err(error) => Err(contract::AggregateError::InvalidInput(error)),
        };
        output.push(contract::AggregateOutcome { outcome });
    }
    output
}

#[allow(unused_macros)]
macro_rules! row {
    ($row:expr, $target:path) => {{
        let row = $row;
        $target {
            product_id: row.product_id.0,
            location_id: row.location_id.0,
            disposition: row.disposition,
            quantity: row.quantity.0,
            packaging_count: row.packaging_count,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::AggregateError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::AggregateError::InternalError;
            };
            contract::AggregateError::InvalidInput(contract::InvalidInputDetail { field })
        }
        "retry" => contract::AggregateError::Retry,
        "timeout" => contract::AggregateError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::AggregateError::InternalError;
            };
            contract::AggregateError::PermissionDenied(contract::PermissionDeniedDetail {
                operation,
            })
        }
        _ => contract::AggregateError::InternalError,
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
                    input: Vec<__contract::AggregateItem>,
                ) -> Result<Vec<__contract::AggregateOutcome>, __node::NodeError> {
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
