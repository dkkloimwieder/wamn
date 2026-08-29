//! Opaque keyset cursor encoding for Receiving query operations.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::error::AccessError;

const VERSION: u8 = 1;

/// Sort direction shared by the primary key and UUID tie-breaker.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CursorDirection {
    Ascending,
    Descending,
}

/// A decoded cursor whose key type is supplied by its owning operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedCursor<Key> {
    pub(crate) key: Key,
    pub(crate) id: Uuid,
}

/// A key type admitted by the closed cursor wire contract.
pub(crate) trait CursorKey: Sized {
    fn to_json(&self) -> Value;
    fn from_json(value: Value) -> Result<Self, AccessError>;

    fn validate(&self) -> Result<(), AccessError> {
        Ok(())
    }
}

impl CursorKey for Box<str> {
    fn to_json(&self) -> Value {
        Value::String(self.to_string())
    }

    fn from_json(value: Value) -> Result<Self, AccessError> {
        match value {
            Value::String(value) => Ok(value.into_boxed_str()),
            _ => Err(AccessError::invalid(
                "cursor key type does not match the sort field",
            )),
        }
    }
}

impl CursorKey for DateTime<Utc> {
    fn to_json(&self) -> Value {
        Value::String(canonical_timestamp(self))
    }

    fn from_json(value: Value) -> Result<Self, AccessError> {
        let Value::String(value) = value else {
            return Err(AccessError::invalid(
                "cursor key type does not match the sort field",
            ));
        };
        DateTime::parse_from_rfc3339(&value)
            .ok()
            .map(|timestamp| timestamp.to_utc())
            .filter(|timestamp| canonical_timestamp(timestamp) == value)
            .ok_or_else(|| {
                AccessError::invalid(
                    "timestamptz cursor key must be UTC RFC3339 with six fractional digits",
                )
            })
    }
}

#[derive(Debug, Serialize)]
struct CursorWire<'a> {
    v: u8,
    field: &'a str,
    direction: CursorDirection,
    key: Value,
    id: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecodedCursorWire {
    v: u8,
    field: Box<str>,
    direction: CursorDirection,
    key: Value,
    id: Box<str>,
}

/// Encode one cursor as canonical compact JSON and unpadded base64url.
pub(crate) fn encode_cursor<Key: CursorKey>(
    field: &str,
    direction: CursorDirection,
    key: &Key,
    id: Uuid,
) -> Result<String, AccessError> {
    validate_field(field)?;
    key.validate()?;
    let id = id.hyphenated().to_string();
    let bytes = canonical_bytes(field, direction, key, &id);
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Decode one cursor for the exact field, direction, and ambient Rust key type.
pub(crate) fn decode_cursor<Key: CursorKey>(
    encoded: &str,
    expected_field: &str,
    expected_direction: CursorDirection,
) -> Result<DecodedCursor<Key>, AccessError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| AccessError::invalid("cursor is not unpadded base64url"))?;
    let wire: DecodedCursorWire = serde_json::from_slice(&bytes)
        .map_err(|_| AccessError::invalid("cursor payload is not the closed v1 JSON shape"))?;
    if wire.v != VERSION {
        return Err(AccessError::invalid("cursor version is not supported"));
    }
    if wire.field.as_ref() != expected_field || wire.direction != expected_direction {
        return Err(AccessError::invalid(
            "cursor field or direction does not match the requested sort",
        ));
    }
    validate_field(&wire.field)?;
    let id = parse_uuid(&wire.id)?;
    let key = Key::from_json(wire.key)?;
    let canonical = canonical_bytes(&wire.field, wire.direction, &key, &wire.id);
    if canonical != bytes || URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err(AccessError::invalid("cursor is not canonically encoded"));
    }
    Ok(DecodedCursor { key, id })
}

fn canonical_bytes<Key: CursorKey>(
    field: &str,
    direction: CursorDirection,
    key: &Key,
    id: &str,
) -> Vec<u8> {
    serde_json::to_vec(&CursorWire {
        v: VERSION,
        field,
        direction,
        key: key.to_json(),
        id,
    })
    .expect("cursor wire values always serialize")
}

fn canonical_timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_uuid(value: &str) -> Result<Uuid, AccessError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|parsed| parsed.hyphenated().to_string() == value)
        .ok_or_else(|| AccessError::invalid("cursor id is not a canonical lowercase UUID"))
}

fn validate_field(field: &str) -> Result<(), AccessError> {
    let mut bytes = field.bytes();
    if bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !field.ends_with('_')
        && !field.contains("__")
    {
        Ok(())
    } else {
        Err(AccessError::invalid("cursor field is not snake_case"))
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;
    use crate::error::AccessErrorKind;

    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn timestamp_cursor_has_one_wire_spelling_and_round_trips() {
        let key = Utc
            .with_ymd_and_hms(2026, 8, 29, 12, 34, 56)
            .single()
            .expect("test timestamp is valid");
        let id = Uuid::parse_str(ID).expect("test UUID is valid");

        let encoded = encode_cursor("created_at", CursorDirection::Ascending, &key, id).unwrap();

        assert_eq!(
            URL_SAFE_NO_PAD.decode(&encoded).unwrap(),
            br#"{"v":1,"field":"created_at","direction":"ascending","key":"2026-08-29T12:34:56.000000Z","id":"01234567-89ab-cdef-0123-456789abcdef"}"#
        );
        assert_eq!(
            decode_cursor::<DateTime<Utc>>(&encoded, "created_at", CursorDirection::Ascending,)
                .unwrap(),
            DecodedCursor { key, id }
        );
    }

    #[test]
    fn text_cursor_uses_the_ambient_string_type() {
        let id = Uuid::parse_str(ID).unwrap();
        let key: Box<str> = "open".into();

        let encoded = encode_cursor("status", CursorDirection::Ascending, &key, id).unwrap();

        assert_eq!(
            decode_cursor::<Box<str>>(&encoded, "status", CursorDirection::Ascending).unwrap(),
            DecodedCursor { key, id }
        );
    }

    #[test]
    fn malformed_noncanonical_or_mismatched_cursor_is_invalid_input() {
        let id = Uuid::parse_str(ID).unwrap();
        let key: Box<str> = "open".into();
        let valid = encode_cursor("status", CursorDirection::Ascending, &key, id).unwrap();
        let payloads = [
            "not-base64!".to_owned(),
            format!("{valid}="),
            URL_SAFE_NO_PAD.encode(
                br#"{"field":"status","v":1,"direction":"ascending","key":"open","id":"01234567-89ab-cdef-0123-456789abcdef"}"#,
            ),
            URL_SAFE_NO_PAD.encode(
                br#"{"v":2,"field":"status","direction":"ascending","key":"open","id":"01234567-89ab-cdef-0123-456789abcdef"}"#,
            ),
            URL_SAFE_NO_PAD.encode(
                br#"{"v":1,"field":"status","direction":"ascending","key":"open","id":"01234567-89AB-CDEF-0123-456789ABCDEF"}"#,
            ),
        ];

        for encoded in payloads {
            assert_eq!(
                decode_cursor::<Box<str>>(&encoded, "status", CursorDirection::Ascending)
                    .unwrap_err()
                    .kind(),
                AccessErrorKind::InvalidInput
            );
        }
        assert_eq!(
            decode_cursor::<Box<str>>(&valid, "created_at", CursorDirection::Ascending)
                .unwrap_err()
                .kind(),
            AccessErrorKind::InvalidInput
        );
        assert_eq!(
            decode_cursor::<Box<str>>(&valid, "status", CursorDirection::Descending)
                .unwrap_err()
                .kind(),
            AccessErrorKind::InvalidInput
        );
    }

    #[test]
    fn noncanonical_timestamp_is_invalid_input() {
        let timestamp_payload = URL_SAFE_NO_PAD.encode(
            br#"{"v":1,"field":"created_at","direction":"ascending","key":"2026-08-29T12:34:56Z","id":"01234567-89ab-cdef-0123-456789abcdef"}"#,
        );
        assert_eq!(
            decode_cursor::<DateTime<Utc>>(
                &timestamp_payload,
                "created_at",
                CursorDirection::Ascending,
            )
            .unwrap_err()
            .kind(),
            AccessErrorKind::InvalidInput
        );
    }
}
