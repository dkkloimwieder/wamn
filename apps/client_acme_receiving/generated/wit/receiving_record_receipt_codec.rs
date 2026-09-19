// @generated from wamn.json; do not edit.

use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Debug)]
pub(crate) struct CodecError(&'static str);

impl CodecError {
    pub(crate) const fn context(&self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for CodecError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRequest {
    value: JsonValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonValue {
    idempotency_key: String,
    purchase_order_id: String,
    receipt_reference: String,
    occurred_at: String,
    line: Vec<JsonLine>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonLine {
    purchase_order_line_id: String,
    quantity: String,
    location_id: String,
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::RecordReceiptItem>, CodecError> {
    let Value::Array(values) = serde_json::from_str(input)
        .map_err(|_| CodecError("operation input must be a JSON array"))?
    else {
        return Err(CodecError("operation input must be a JSON array"));
    };
    if !(1..=100).contains(&values.len()) {
        return Err(CodecError("operation input item count must be 1..=100"));
    }
    values
        .into_iter()
        .map(|value| {
            let Value::Object(mut object) = value else {
                return Err(CodecError("every operation item must be a JSON object"));
            };
            let Some(Value::String(request_id)) = object.remove("request_id") else {
                return Err(CodecError(
                    "every operation item must carry a nonempty string request_id",
                ));
            };
            if request_id.is_empty() {
                return Err(CodecError(
                    "every operation item must carry a nonempty string request_id",
                ));
            }
            let input = match serde_json::from_value::<JsonRequest>(Value::Object(object)) {
                Ok(request) => Ok(contract::RecordReceiptRequest {
                    idempotency_key: request.value.idempotency_key,
                    purchase_order_id: request.value.purchase_order_id,
                    receipt_reference: request.value.receipt_reference,
                    occurred_at: request.value.occurred_at,
                    line: request
                        .value
                        .line
                        .into_iter()
                        .map(|line| contract::RecordReceiptLine {
                            purchase_order_line_id: line.purchase_order_line_id,
                            quantity: line.quantity,
                            location_id: line.location_id,
                        })
                        .collect(),
                }),
                Err(_) => Err(contract::InvalidInputDetail {
                    field: "input".to_owned(),
                    minimum: None,
                    maximum: None,
                    observed: None,
                }),
            };
            Ok(contract::RecordReceiptItem { request_id, input })
        })
        .collect()
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
                    "row_version": value.row_version.to_string(),
                }
            }),
            Err(error) => json!({
                "request_id": item.request_id,
                "error": error_value(error),
            }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed receipt outcomes always serialize")
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
