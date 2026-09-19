// @generated from wamn.json and schema IR; do not edit.

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
    id: String,
    expected_row_version: String,
    change: JsonUpdateChange,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonUpdateChange {
    #[serde(default)]
    acme_inspection_required: JsonChange<bool>,
    #[serde(default)]
    acme_quality_status: JsonChange<String>,
}

#[derive(Default)]
enum JsonChange<T> {
    #[default]
    Absent,
    Null,
    Value(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for JsonChange<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[expect(
    clippy::option_option,
    reason = "WIT update fields distinguish absent, null, and value"
)]
fn change<T>(value: JsonChange<T>) -> Option<Option<T>> {
    match value {
        JsonChange::Absent => None,
        JsonChange::Null => Some(None),
        JsonChange::Value(value) => Some(Some(value)),
    }
}

pub(crate) fn decode(input: &str) -> Result<Vec<contract::UpdateItem>, CodecError> {
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
                Ok(request) => match request.expected_row_version.parse::<i64>() {
                    Ok(expected_row_version) => {
                        let request = contract::UpdateRequest {
                            id: request.id,
                            expected_row_version,
                            change: contract::UpdateChange {
                                acme_inspection_required: change(
                                    request.change.acme_inspection_required,
                                ),
                                acme_quality_status: change(request.change.acme_quality_status),
                            },
                        };
                        Ok(request)
                    }
                    Err(_) => Err(invalid("expected_row_version")),
                },
                Err(_) => Err(invalid("input")),
            };
            Ok(contract::UpdateItem { request_id, input })
        })
        .collect()
}

fn invalid(field: &str) -> contract::InvalidInputDetail {
    contract::InvalidInputDetail {
        field: field.to_owned(),
    }
}

pub(crate) fn encode(output: &[contract::UpdateOutcome]) -> String {
    let values = output
        .iter()
        .map(|item| match &item.outcome {
            Ok(value) => json!({
                "request_id": item.request_id,
                "value": {
                    "acme_inspection_required": value.acme_inspection_required,
                    "acme_quality_status": value.acme_quality_status,
                    "created_at": value.created_at,
                    "created_by": value.created_by,
                    "id": value.id,
                    "purchase_order_number": value.purchase_order_number,
                    "row_version": value.row_version.to_string(),
                    "status": value.status,
                    "supplier_id": value.supplier_id,
                    "updated_at": value.updated_at,
                    "updated_by": value.updated_by,
                }
            }),
            Err(error) => json!({
                "request_id": item.request_id,
                "error": error_value(error),
            }),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).expect("typed update outcomes always serialize")
}

fn error_value(error: &contract::UpdateError) -> Value {
    let (code, detail) = match error {
        contract::UpdateError::InvalidInput(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            ("invalid_input", detail)
        }
        contract::UpdateError::NotFound(value) => {
            let mut detail = Map::new();
            detail.insert("field".to_owned(), json!(value.field));
            detail.insert("id".to_owned(), json!(value.id));
            ("not_found", detail)
        }
        contract::UpdateError::ConcurrencyConflict(value) => {
            let mut detail = Map::new();
            detail.insert(
                "expected_row_version".to_owned(),
                json!(value.expected_row_version),
            );
            detail.insert(
                "observed_row_version".to_owned(),
                json!(value.observed_row_version),
            );
            ("concurrency_conflict", detail)
        }
        contract::UpdateError::Retry => ("retry", Map::new()),
        contract::UpdateError::Timeout => ("timeout", Map::new()),
        contract::UpdateError::PermissionDenied(value) => {
            let mut detail = Map::new();
            detail.insert("operation".to_owned(), json!(value.operation));
            ("permission_denied", detail)
        }
        contract::UpdateError::InternalError => ("internal_error", Map::new()),
    };
    json!({"code": code, "detail": detail})
}
