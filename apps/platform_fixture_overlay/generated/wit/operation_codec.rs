// @generated from wamn.json and schema IR; do not edit.

use serde::Deserialize;
#[allow(unused_imports)]
use serde_json::{Map, Value, json};

#[allow(dead_code)]
fn canonical_uuid(value: &mut String) -> bool {
    let Ok(parsed) = uuid::Uuid::parse_str(value) else {
        return false;
    };
    *value = parsed.hyphenated().to_string();
    true
}

#[allow(dead_code)]
struct JsonInt64(i64);

impl<'de> Deserialize<'de> for JsonInt64 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map(Self).map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for JsonInt64 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

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

#[allow(dead_code)]
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
#[allow(dead_code)]
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

#[allow(dead_code)]
#[expect(
    clippy::option_option,
    reason = "WIT mutation fields distinguish absent, null, and value"
)]
fn change<T>(value: JsonChange<T>) -> Option<Option<T>> {
    match value {
        JsonChange::Absent => None,
        JsonChange::Null => Some(None),
        JsonChange::Value(value) => Some(Some(value)),
    }
}
