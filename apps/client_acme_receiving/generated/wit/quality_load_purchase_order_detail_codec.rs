// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::LoadPurchaseOrderDetailItem;
const MINIMUM: usize = 1;
const MAXIMUM: usize = 100;
const COUNT_ERROR: &str = "operation input item count must be 1..=100";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    purchase_order_id: String,
}

pub(crate) fn decode(
    input: &str,
) -> Result<Vec<contract::LoadPurchaseOrderDetailItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::LoadPurchaseOrderDetailRequest {
                    purchase_order_id: request.purchase_order_id,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::LoadPurchaseOrderDetailItem { request_id, input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::LoadPurchaseOrderDetailOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "id": value.id,
                    "purchase_order_number": value.purchase_order_number,
                    "supplier_id": value.supplier_id,
                    "status": value.status,
                    "row_version": value.row_version,
                    "acme_inspection_required": value.acme_inspection_required,
                    "acme_quality_status": value.acme_quality_status,
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

fn error_value(error: &contract::LoadPurchaseOrderDetailError) -> Value {
    let (code, detail) = match error {
        contract::LoadPurchaseOrderDetailError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::LoadPurchaseOrderDetailError::NotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("not_found", detail)
        }
        contract::LoadPurchaseOrderDetailError::Retry => ("retry", Map::new()),
        contract::LoadPurchaseOrderDetailError::Timeout => ("timeout", Map::new()),
        contract::LoadPurchaseOrderDetailError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::LoadPurchaseOrderDetailError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(
    request: &mut contract::LoadPurchaseOrderDetailRequest,
) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    {
        let value = &mut request.purchase_order_id;
        if !canonical_uuid(value) {
            return Err(invalid("purchase_order_id"));
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::LoadPurchaseOrderDetailItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::LoadPurchaseOrderDetailOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::LoadPurchaseOrderDetailRequest,
    ) -> Result<
        contract::LoadPurchaseOrderDetailResult,
        contract::LoadPurchaseOrderDetailError,
    >,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::LoadPurchaseOrderDetailError::InvalidInput(error)),
            },
            Err(error) => Err(contract::LoadPurchaseOrderDetailError::InvalidInput(error)),
        };
        output.push(contract::LoadPurchaseOrderDetailOutcome {
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
            id: row.id.0,
            purchase_order_number: row.purchase_order_number,
            supplier_id: row.supplier_id.0,
            status: row.status,
            row_version: row.row_version,
            acme_inspection_required: row.acme_inspection_required,
            acme_quality_status: row.acme_quality_status,
        }
    }};
}
#[allow(unused_imports)]
pub(crate) use row;

#[allow(dead_code)]
pub(crate) fn map_error(
    code: &str,
    mut detail: impl FnMut(&str) -> Option<String>,
) -> contract::LoadPurchaseOrderDetailError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::LoadPurchaseOrderDetailError::InternalError;
            };
            contract::LoadPurchaseOrderDetailError::InvalidInput(contract::InvalidInputDetail {
                field,
            })
        }
        "not_found" => {
            let Some(field) = detail("field") else {
                return contract::LoadPurchaseOrderDetailError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::LoadPurchaseOrderDetailError::InternalError;
            };
            contract::LoadPurchaseOrderDetailError::NotFound(contract::NotFoundDetail { field, id })
        }
        "retry" => contract::LoadPurchaseOrderDetailError::Retry,
        "timeout" => contract::LoadPurchaseOrderDetailError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::LoadPurchaseOrderDetailError::InternalError;
            };
            contract::LoadPurchaseOrderDetailError::PermissionDenied(
                contract::PermissionDeniedDetail { operation },
            )
        }
        _ => contract::LoadPurchaseOrderDetailError::InternalError,
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
                    input: Vec<__contract::LoadPurchaseOrderDetailItem>,
                ) -> Result<Vec<__contract::LoadPurchaseOrderDetailOutcome>, __node::NodeError>
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
