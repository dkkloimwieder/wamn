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

fn validate_count(count: usize) -> Result<(), CodecError> {
    if !(MINIMUM..=MAXIMUM).contains(&count) {
        return Err(CodecError(COUNT_ERROR));
    }
    Ok(())
}

pub(crate) fn validate(input: &[Item]) -> Result<(), CodecError> {
    validate_count(input.len())?;
    if input.iter().any(|item| item.request_id.is_empty()) {
        return Err(CodecError(
            "every operation item must carry a nonempty string request_id",
        ));
    }
    Ok(())
}

fn decode_envelope(input: &str) -> Result<Vec<(String, Value)>, CodecError> {
    let Value::Array(values) = serde_json::from_str(input)
        .map_err(|_| CodecError("operation input must be a JSON array"))?
    else {
        return Err(CodecError("operation input must be a JSON array"));
    };
    validate_count(values.len())?;
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
            Ok((request_id, Value::Object(object)))
        })
        .collect()
}
