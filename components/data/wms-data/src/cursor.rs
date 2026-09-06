//! Opaque keyset cursor encoding for `pallet.query`, the closed v1 shape
//! `packages/wms/generated/contracts/cursor-v1.json` fixes: canonical compact
//! JSON of `{direction, field, id, key, v}`, unpadded base64url, and a decode
//! that refuses anything it would not have minted itself.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use wamn_execution_contract::canonical_json_bytes;

use crate::error::{AccessError, AccessErrorKind};

const VERSION: u8 = 1;

/// Sort direction shared by the primary key and UUID tie-breaker.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CursorDirection {
    Ascending,
    Descending,
}

/// A decoded cursor whose key type is supplied by its owning sort.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedCursor<Key> {
    pub(crate) key: Key,
    pub(crate) id: Uuid,
}

/// A key type admitted by the closed cursor wire contract: the bare value,
/// typed by the manifest IR of the sort field.
pub(crate) trait CursorKey: Sized {
    fn to_json(&self) -> Value;
    fn from_json(value: Value) -> Result<Self, AccessError>;
}

fn invalid() -> AccessError {
    AccessError::field(AccessErrorKind::InvalidInput, "cursor")
}

impl CursorKey for String {
    fn to_json(&self) -> Value {
        Value::String(self.clone())
    }

    fn from_json(value: Value) -> Result<Self, AccessError> {
        match value {
            Value::String(value) => Ok(value),
            _ => Err(invalid()),
        }
    }
}

impl CursorKey for Uuid {
    fn to_json(&self) -> Value {
        Value::String(self.hyphenated().to_string())
    }

    fn from_json(value: Value) -> Result<Self, AccessError> {
        match value {
            Value::String(value) => parse_uuid(&value),
            _ => Err(invalid()),
        }
    }
}

impl CursorKey for DateTime<Utc> {
    fn to_json(&self) -> Value {
        Value::String(canonical_timestamp(self))
    }

    fn from_json(value: Value) -> Result<Self, AccessError> {
        let Value::String(value) = value else {
            return Err(invalid());
        };
        DateTime::parse_from_rfc3339(&value)
            .ok()
            .map(|timestamp| timestamp.to_utc())
            .filter(|timestamp| canonical_timestamp(timestamp) == value)
            .ok_or_else(invalid)
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
    field: String,
    direction: CursorDirection,
    key: Value,
    id: String,
}

/// Encode one cursor as canonical compact JSON and unpadded base64url.
pub(crate) fn encode_cursor<Key: CursorKey>(
    field: &str,
    direction: CursorDirection,
    key: &Key,
    id: Uuid,
) -> String {
    let id = id.hyphenated().to_string();
    URL_SAFE_NO_PAD.encode(canonical_bytes(field, direction, key, &id))
}

/// Decode one cursor for the exact field, direction and key type of the sort
/// the caller asked for; a cursor minted under another sort is refused.
pub(crate) fn decode_cursor<Key: CursorKey>(
    encoded: &str,
    expected_field: &str,
    expected_direction: CursorDirection,
) -> Result<DecodedCursor<Key>, AccessError> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
    let wire: DecodedCursorWire = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if wire.v != VERSION || wire.field != expected_field || wire.direction != expected_direction {
        return Err(invalid());
    }
    let id = parse_uuid(&wire.id)?;
    let key = Key::from_json(wire.key)?;
    // Noncanonical spellings are refused even when they decode: one cursor,
    // one byte string, or two callers holding "the same" page would disagree.
    if canonical_bytes(&wire.field, wire.direction, &key, &wire.id) != bytes
        || URL_SAFE_NO_PAD.encode(&bytes) != encoded
    {
        return Err(invalid());
    }
    Ok(DecodedCursor { key, id })
}

fn canonical_bytes<Key: CursorKey>(
    field: &str,
    direction: CursorDirection,
    key: &Key,
    id: &str,
) -> Vec<u8> {
    let value = serde_json::to_value(CursorWire {
        v: VERSION,
        field,
        direction,
        key: key.to_json(),
        id,
    })
    .expect("cursor wire values always serialize");
    canonical_json_bytes(&value)
}

pub(crate) fn canonical_timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_uuid(value: &str) -> Result<Uuid, AccessError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|parsed| parsed.hyphenated().to_string() == value)
        .ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn a_cursor_has_one_wire_spelling_and_round_trips() {
        let key = Utc
            .with_ymd_and_hms(2026, 8, 29, 12, 34, 56)
            .single()
            .unwrap();
        let id = Uuid::parse_str(ID).unwrap();
        let encoded = encode_cursor("created_at", CursorDirection::Ascending, &key, id);
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&encoded).unwrap(),
            br#"{"direction":"ascending","field":"created_at","id":"01234567-89ab-cdef-0123-456789abcdef","key":"2026-08-29T12:34:56.000000Z","v":1}"#
        );
        assert_eq!(
            decode_cursor::<DateTime<Utc>>(&encoded, "created_at", CursorDirection::Ascending)
                .unwrap(),
            DecodedCursor { key, id }
        );

        let location = Uuid::parse_str("11234567-89ab-cdef-0123-456789abcdef").unwrap();
        let encoded = encode_cursor("location_id", CursorDirection::Descending, &location, id);
        assert_eq!(
            decode_cursor::<Uuid>(&encoded, "location_id", CursorDirection::Descending).unwrap(),
            DecodedCursor { key: location, id }
        );
    }

    #[test]
    fn a_cursor_from_another_sort_or_spelling_is_invalid_input() {
        let id = Uuid::parse_str(ID).unwrap();
        let valid = encode_cursor(
            "pallet_code",
            CursorDirection::Ascending,
            &"PAL-1".to_owned(),
            id,
        );
        for encoded in [
            "not-base64!".to_owned(),
            format!("{valid}="),
            URL_SAFE_NO_PAD.encode(
                br#"{"field":"pallet_code","v":1,"direction":"ascending","key":"PAL-1","id":"01234567-89ab-cdef-0123-456789abcdef"}"#,
            ),
            URL_SAFE_NO_PAD.encode(
                br#"{"direction":"ascending","field":"pallet_code","id":"01234567-89AB-CDEF-0123-456789ABCDEF","key":"PAL-1","v":1}"#,
            ),
        ] {
            let error = decode_cursor::<String>(&encoded, "pallet_code", CursorDirection::Ascending)
                .unwrap_err();
            assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
        }
        assert!(decode_cursor::<String>(&valid, "created_at", CursorDirection::Ascending).is_err());
        assert!(
            decode_cursor::<String>(&valid, "pallet_code", CursorDirection::Descending).is_err()
        );
        assert!(decode_cursor::<Uuid>(&valid, "pallet_code", CursorDirection::Ascending).is_err());
    }
}
