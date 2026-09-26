// @generated from operation declarations; do not edit.

include!("operation_codec.rs");
type Item = contract::RecordReceiptItem;
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
struct JsonLine {
    location_id: String,
    purchase_order_line_id: String,
    quantity: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    value: JsonRoot,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRoot {
    idempotency_key: String,
    line: Vec<JsonLine>,
    occurred_at: String,
    purchase_order_id: String,
    receipt_reference: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::RecordReceiptItem>, CodecError> {
    decode_envelope(input)?
        .into_iter()
        .map(|(request_id, body)| {
            let input = serde_json::from_value::<JsonRequest>(body)
                .map(|request| contract::RecordReceiptRequest {
                    idempotency_key: request.value.idempotency_key,
                    line: request
                        .value
                        .line
                        .into_iter()
                        .map(|value| contract::RecordReceiptLine {
                            location_id: value.location_id,
                            purchase_order_line_id: value.purchase_order_line_id,
                            quantity: value.quantity,
                        })
                        .collect(),
                    occurred_at: request.value.occurred_at,
                    purchase_order_id: request.value.purchase_order_id,
                    receipt_reference: request.value.receipt_reference,
                })
                .map_err(|_| invalid("input"));
            Ok(contract::RecordReceiptItem { request_id, input })
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

pub(crate) fn encode(output: &[contract::RecordReceiptOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "receipt_id": value.receipt_id,
                    "purchase_order_id": value.purchase_order_id,
                    "purchase_order_status": value.purchase_order_status,
                    "row_version": value.row_version,
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

fn error_value(error: &contract::RecordReceiptError) -> Value {
    let (code, detail) = match error {
        contract::RecordReceiptError::InvalidInput(value) => {
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
        contract::RecordReceiptError::PurchaseOrderNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("purchase_order_not_found", detail)
        }
        contract::RecordReceiptError::PurchaseOrderNotOpen(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("purchase_order_not_open", detail)
        }
        contract::RecordReceiptError::PurchaseOrderLineNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("purchase_order_line_not_found", detail)
        }
        contract::RecordReceiptError::PurchaseOrderLineMismatch(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("purchase_order_line_mismatch", detail)
        }
        contract::RecordReceiptError::LocationNotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("location_not_found", detail)
        }
        contract::RecordReceiptError::QuantityExceedsRemaining(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("quantity_exceeds_remaining", detail)
        }
        contract::RecordReceiptError::ReceiptReferenceConflict(value) => {
            let mut detail = Map::new();
            detail.insert("constraint".to_owned(), json!(value.constraint));
            ("receipt_reference_conflict", detail)
        }
        contract::RecordReceiptError::IdempotencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("idempotency_conflict", detail)
        }
        contract::RecordReceiptError::Retry => ("retry", Map::new()),
        contract::RecordReceiptError::Timeout => ("timeout", Map::new()),
        contract::RecordReceiptError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::RecordReceiptError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
#[allow(clippy::unnecessary_wraps)]
fn normalize(
    request: &mut contract::RecordReceiptRequest,
) -> Result<(), contract::InvalidInputDetail> {
    let _ = &request;
    if request.line.is_empty() {
        return Err(invalid("value.line[]"));
    }
    if request.line.len() > 100 {
        return Err(invalid("value.line[]"));
    }
    for value in &mut request.line {
        {
            let value = &mut value.location_id;
            if !canonical_uuid(value) {
                return Err(invalid("value.line[].location_id"));
            }
        }
        {
            let value = &mut value.purchase_order_line_id;
            if !canonical_uuid(value) {
                return Err(invalid("value.line[].purchase_order_line_id"));
            }
        }
    }
    {
        let value = &mut request.purchase_order_id;
        if !canonical_uuid(value) {
            return Err(invalid("value.purchase_order_id"));
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run<S, F>(
    input: Vec<contract::RecordReceiptItem>,
    state: &mut S,
    mut handler: F,
) -> Vec<contract::RecordReceiptOutcome>
where
    F: AsyncFnMut(
        &mut S,
        contract::RecordReceiptRequest,
    ) -> Result<contract::RecordReceiptResult, contract::RecordReceiptError>,
{
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let outcome = match item.input {
            Ok(mut request) => match normalize(&mut request) {
                Ok(()) => handler(state, request).await,
                Err(error) => Err(contract::RecordReceiptError::InvalidInput(error)),
            },
            Err(error) => Err(contract::RecordReceiptError::InvalidInput(error)),
        };
        output.push(contract::RecordReceiptOutcome {
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
            purchase_order_id: row.purchase_order_id.0,
            purchase_order_status: row.purchase_order_status,
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
) -> contract::RecordReceiptError {
    match code {
        "invalid_input" => {
            let Some(field) = detail("field") else {
                return contract::RecordReceiptError::InternalError;
            };
            let minimum = detail("minimum");
            let maximum = detail("maximum");
            let observed = detail("observed");
            contract::RecordReceiptError::InvalidInput(contract::InvalidInputDetail {
                field,
                minimum,
                maximum,
                observed,
            })
        }
        "purchase_order_not_found" => {
            let Some(field) = detail("field") else {
                return contract::RecordReceiptError::InternalError;
            };
            contract::RecordReceiptError::PurchaseOrderNotFound(
                contract::PurchaseOrderNotFoundDetail { field },
            )
        }
        "purchase_order_not_open" => {
            let Some(field) = detail("field") else {
                return contract::RecordReceiptError::InternalError;
            };
            contract::RecordReceiptError::PurchaseOrderNotOpen(
                contract::PurchaseOrderNotOpenDetail { field },
            )
        }
        "purchase_order_line_not_found" => {
            let Some(field) = detail("field") else {
                return contract::RecordReceiptError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::RecordReceiptError::InternalError;
            };
            contract::RecordReceiptError::PurchaseOrderLineNotFound(
                contract::PurchaseOrderLineNotFoundDetail { field, id },
            )
        }
        "purchase_order_line_mismatch" => {
            let Some(field) = detail("field") else {
                return contract::RecordReceiptError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::RecordReceiptError::InternalError;
            };
            contract::RecordReceiptError::PurchaseOrderLineMismatch(
                contract::PurchaseOrderLineMismatchDetail { field, id },
            )
        }
        "location_not_found" => {
            let Some(field) = detail("field") else {
                return contract::RecordReceiptError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::RecordReceiptError::InternalError;
            };
            contract::RecordReceiptError::LocationNotFound(contract::LocationNotFoundDetail {
                field,
                id,
            })
        }
        "quantity_exceeds_remaining" => {
            let Some(field) = detail("field") else {
                return contract::RecordReceiptError::InternalError;
            };
            let Some(id) = detail("id") else {
                return contract::RecordReceiptError::InternalError;
            };
            contract::RecordReceiptError::QuantityExceedsRemaining(
                contract::QuantityExceedsRemainingDetail { field, id },
            )
        }
        "receipt_reference_conflict" => {
            let Some(constraint) = detail("constraint") else {
                return contract::RecordReceiptError::InternalError;
            };
            contract::RecordReceiptError::ReceiptReferenceConflict(
                contract::ReceiptReferenceConflictDetail { constraint },
            )
        }
        "idempotency_conflict" => {
            let Some(field) = detail("field") else {
                return contract::RecordReceiptError::InternalError;
            };
            contract::RecordReceiptError::IdempotencyConflict(contract::IdempotencyConflictDetail {
                field,
            })
        }
        "retry" => contract::RecordReceiptError::Retry,
        "timeout" => contract::RecordReceiptError::Timeout,
        "permission_denied" => {
            let Some(operation) = detail("operation") else {
                return contract::RecordReceiptError::InternalError;
            };
            contract::RecordReceiptError::PermissionDenied(contract::PermissionDeniedDetail {
                operation,
            })
        }
        _ => contract::RecordReceiptError::InternalError,
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
                    input: Vec<__contract::RecordReceiptItem>,
                ) -> Result<Vec<__contract::RecordReceiptOutcome>, __node::NodeError> {
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
