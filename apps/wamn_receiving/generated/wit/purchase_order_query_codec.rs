// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::QueryItem;
const MINIMUM: usize = 1;
const MAXIMUM: usize = 100;
const COUNT_ERROR: &str = "operation input item count must be 1..=100";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    #[serde(default)]
    filter: Option<JsonFilter>,
    #[serde(default)]
    sort: Option<JsonSort>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonFilter {
    #[serde(default)]
    supplier_id: Option<Vec<String>>,
    #[serde(default)]
    status: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonSort {
    field: String,
    direction: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::QueryItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|mut request| contract::QueryRequest {
                    supplier_id: request
                        .filter
                        .as_mut()
                        .and_then(|filter| filter.supplier_id.take()),
                    status: request
                        .filter
                        .as_mut()
                        .and_then(|filter| filter.status.take()),
                    sort_field: request.sort.as_ref().map(|sort| sort.field.clone()),
                    sort_direction: request.sort.map(|sort| sort.direction),
                    cursor: request.cursor,
                    limit: request.limit,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::QueryItem { request_id, input })
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

pub(crate) fn encode(output: &[contract::QueryOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({ "request_id": item.request_id, "value":
            { "item": value.value.iter().map(|row| json!({
                                "created_at": row.created_at,
                                "created_by": row.created_by,
                                "id": row.id,
                                "purchase_order_number": row.purchase_order_number,
                                "row_version": row.row_version.to_string(),
                                "status": row.status,
                                "supplier_id": row.supplier_id,
                                "updated_at": row.updated_at,
                                "updated_by": row.updated_by,
                        })).collect::<Vec<_>>(), "next_cursor": value.next_cursor }
                    }),
            Err(error) => json!({ "request_id": item.request_id, "error": error_value(error) }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed operation outcomes always serialize")
}

fn error_value(error: &contract::QueryError) -> Value {
    let (code, detail) = match error {
        contract::QueryError::InvalidInput(value) => {
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
        contract::QueryError::Retry => ("retry", Map::new()),
        contract::QueryError::Timeout => ("timeout", Map::new()),
        contract::QueryError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::QueryError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(request: &mut contract::QueryRequest) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    if let Some(values) = &mut request.supplier_id {
        for value in values {
            if !canonical_uuid(value) {
                return Err(invalid("filter.supplier_id"));
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::QueryItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::QueryOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::QueryRequest,
    ) -> Result<contract::QueryResult, contract::QueryError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::QueryError::InvalidInput(error)),
            },
            Err(error) => Err(contract::QueryError::InvalidInput(error)),
        };
        output.push(contract::QueryOutcome {
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
) -> contract::QueryError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::QueryError::InternalError;
            };
            let Ok(minimum) = detail("minimum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::QueryError::InternalError;
            };
            let Ok(maximum) = detail("maximum")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::QueryError::InternalError;
            };
            let Ok(observed) = detail("observed")
                .map(|value| value.parse::<i64>())
                .transpose()
            else {
                return contract::QueryError::InternalError;
            };
            contract::QueryError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "retry" => contract::QueryError::Retry,
        "timeout" => contract::QueryError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::QueryError::InternalError;
            };
            contract::QueryError::PermissionDenied(contract::PermissionDeniedDetail { operation })
        }
        _ => contract::QueryError::InternalError,
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
                    input: Vec<__contract::QueryItem>,
                ) -> Result<Vec<__contract::QueryOutcome>, __node::NodeError> {
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
