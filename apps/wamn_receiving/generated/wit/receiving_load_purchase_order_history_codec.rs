// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::LoadPurchaseOrderHistoryItem;
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
    after_cursor: Option<String>,
    id: String,
    limit: i32,
}

pub(crate) fn decode(
    input: &str,
) -> Result<Vec<contract::LoadPurchaseOrderHistoryItem>, CodecError> {
    decode_read_envelope(input)?
        .into_iter()
        .map(|body| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::LoadPurchaseOrderHistoryRequest {
                    after_cursor: request.after_cursor,
                    id: request.id,
                    limit: request.limit,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::LoadPurchaseOrderHistoryItem { input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::LoadPurchaseOrderHistoryOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "value": { "rows": value.rows.iter().map(|row| json!({
                    "cursor": row.cursor,
                    "kind": row.kind,
                    "operation": row.operation,
                    "changed_by": row.changed_by,
                    "changed_at": row.changed_at,
                    "before": row.before,
                    "after": row.after,
                    "current": row.current,
                })).collect::<Vec<_>>() }
            }),
            Err(error) => json!({
                "error": error_value(error),
            }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed outcomes always serialize")
}

fn error_value(error: &contract::LoadPurchaseOrderHistoryError) -> Value {
    let (code, detail) = match error {
        contract::LoadPurchaseOrderHistoryError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::LoadPurchaseOrderHistoryError::Retry => ("retry", Map::new()),
        contract::LoadPurchaseOrderHistoryError::Timeout => ("timeout", Map::new()),
        contract::LoadPurchaseOrderHistoryError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::LoadPurchaseOrderHistoryError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(
    request: &mut contract::LoadPurchaseOrderHistoryRequest,
) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    {
        let value = &mut request.id;
        if !canonical_uuid(value) {
            return Err(invalid("id"));
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::LoadPurchaseOrderHistoryItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::LoadPurchaseOrderHistoryOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::LoadPurchaseOrderHistoryRequest,
    ) -> Result<
        contract::LoadPurchaseOrderHistoryResult,
        contract::LoadPurchaseOrderHistoryError,
    >,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::LoadPurchaseOrderHistoryError::InvalidInput(error)),
            },
            Err(error) => Err(contract::LoadPurchaseOrderHistoryError::InvalidInput(error)),
        };
        output.push(contract::LoadPurchaseOrderHistoryOutcome { outcome });
    }
    output
}

#[allow(unused_macros)]
macro_rules! row {
    ($row:expr, $target:path) => {{
        let row = $row;
        $target {
            cursor: row.cursor,
            kind: row.kind,
            operation: row.operation,
            changed_by: row.changed_by.0,
            changed_at: row.changed_at.0,
            before: row.before,
            after: row.after,
            current: row.current,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::LoadPurchaseOrderHistoryError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::LoadPurchaseOrderHistoryError::InternalError;
            };
            contract::LoadPurchaseOrderHistoryError::InvalidInput(contract::InvalidInputDetail {
                field,
            })
        }
        "retry" => contract::LoadPurchaseOrderHistoryError::Retry,
        "timeout" => contract::LoadPurchaseOrderHistoryError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::LoadPurchaseOrderHistoryError::InternalError;
            };
            contract::LoadPurchaseOrderHistoryError::PermissionDenied(
                contract::PermissionDeniedDetail { operation },
            )
        }
        _ => contract::LoadPurchaseOrderHistoryError::InternalError,
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
                    input: Vec<__contract::LoadPurchaseOrderHistoryItem>,
                ) -> Result<Vec<__contract::LoadPurchaseOrderHistoryOutcome>, __node::NodeError>
                {
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
