//! The opaque keyset cursor: the closed v1 shape that each package's
//! `generated/contracts/cursor-v1.json` fixes.
//!
//! A cursor is canonical compact JSON of `{direction, field, id, key, v}` in
//! unpadded base64url. A decode refuses anything it would not have minted
//! itself: another version, field or direction, another member, and every
//! spelling that is not the one [`encode`] writes.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use wamn_execution_contract::{canonical_json_bytes, canonical_numeric, canonical_timestamptz};
use wamn_postgres_statements::{Numeric, TimestampTz, Uuid};

use crate::{Invalid, scalar};

const VERSION: u64 = 1;

/// The direction of a sort, shared by its key and its `id` tie-breaker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Ascending,
    Descending,
}

impl Direction {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ascending => "ascending",
            Self::Descending => "descending",
        }
    }
}

/// A sort key the cursor carries, as the key's own column type spells it.
pub trait Key: Sized {
    /// The key's JSON, or [`Invalid`] when a row holds a value outside its
    /// type's one spelling.
    fn to_json(&self) -> Result<Value, Invalid>;
    /// The key a cursor carries, only in the spelling [`Key::to_json`] writes.
    fn from_json(value: &Value) -> Result<Self, Invalid>;
}

impl Key for String {
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::String(self.clone()))
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        value.as_str().map(str::to_owned).ok_or(Invalid)
    }
}

impl Key for Uuid {
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::String(scalar::uuid(&self.0)?.0))
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        let value = value.as_str().ok_or(Invalid)?;
        scalar::uuid(value)
            .ok()
            .filter(|parsed| parsed.0 == value)
            .ok_or(Invalid)
    }
}

impl Key for TimestampTz {
    fn to_json(&self) -> Result<Value, Invalid> {
        canonical_timestamptz(&self.0)
            .map(Value::String)
            .ok_or(Invalid)
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        let value = value.as_str().ok_or(Invalid)?;
        canonical_timestamptz(value)
            .filter(|canonical| canonical == value)
            .map(TimestampTz)
            .ok_or(Invalid)
    }
}

impl Key for Numeric {
    fn to_json(&self) -> Result<Value, Invalid> {
        canonical_numeric(&self.0).map(Value::String).ok_or(Invalid)
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        let value = value.as_str().ok_or(Invalid)?;
        canonical_numeric(value)
            .filter(|canonical| canonical == value)
            .map(Numeric)
            .ok_or(Invalid)
    }
}

impl Key for i64 {
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::from(*self))
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        value.as_i64().ok_or(Invalid)
    }
}

impl Key for i32 {
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::from(*self))
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .ok_or(Invalid)
    }
}

impl Key for bool {
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::Bool(*self))
    }

    fn from_json(value: &Value) -> Result<Self, Invalid> {
        value.as_bool().ok_or(Invalid)
    }
}

/// The cursor that starts the next read after the row with this key and id.
///
/// # Errors
///
/// [`Invalid`] when the row holds a key or an id outside its one spelling.
pub fn encode<K: Key>(
    field: &str,
    direction: Direction,
    key: &K,
    id: &Uuid,
) -> Result<String, Invalid> {
    let id = scalar::uuid(&id.0)?;
    Ok(URL_SAFE_NO_PAD.encode(canonical_json_bytes(&json!({
        "direction": direction.as_str(),
        "field": field,
        "id": id.0,
        "key": key.to_json()?,
        "v": VERSION,
    }))))
}

/// The key and the id of a cursor minted under exactly this field and
/// direction.
///
/// # Errors
///
/// [`Invalid`] for a cursor that [`encode`] did not write for this sort.
pub fn decode<K: Key>(
    encoded: &str,
    field: &str,
    direction: Direction,
) -> Result<(K, Uuid), Invalid> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| Invalid)?;
    let wire: Value = serde_json::from_slice(&bytes).map_err(|_| Invalid)?;
    let key = K::from_json(wire.get("key").ok_or(Invalid)?)?;
    let id = <Uuid as Key>::from_json(wire.get("id").ok_or(Invalid)?)?;
    // Minting the cursor again refuses every other version, field, direction,
    // member and spelling.
    if encode(field, direction, &key, &id)? != encoded {
        return Err(Invalid);
    }
    Ok((key, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn a_cursor_has_one_wire_spelling_and_round_trips() {
        let key = TimestampTz("2026-08-29T14:34:56+02:00".to_owned());
        let encoded = encode(
            "created_at",
            Direction::Ascending,
            &key,
            &Uuid(ID.to_owned()),
        )
        .expect("a row key encodes");
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&encoded).expect("base64url"),
            br#"{"direction":"ascending","field":"created_at","id":"01234567-89ab-cdef-0123-456789abcdef","key":"2026-08-29T12:34:56.000000Z","v":1}"#
        );
        let (key, id) = decode::<TimestampTz>(&encoded, "created_at", Direction::Ascending)
            .expect("its own cursor decodes");
        assert_eq!(key.0, "2026-08-29T12:34:56.000000Z");
        assert_eq!(id.0, ID);
    }

    #[test]
    fn a_cursor_from_another_sort_or_spelling_is_invalid() {
        let valid = encode(
            "packaging_code",
            Direction::Ascending,
            &"PAL-1".to_owned(),
            &Uuid(ID.to_owned()),
        )
        .expect("a text key encodes");
        for encoded in [
            "not-base64!".to_owned(),
            format!("{valid}="),
            URL_SAFE_NO_PAD.encode(
                br#"{"field":"packaging_code","v":1,"direction":"ascending","key":"PAL-1","id":"01234567-89ab-cdef-0123-456789abcdef"}"#,
            ),
            URL_SAFE_NO_PAD.encode(
                br#"{"direction":"ascending","field":"packaging_code","id":"01234567-89AB-CDEF-0123-456789ABCDEF","key":"PAL-1","v":1}"#,
            ),
        ] {
            assert_eq!(
                decode::<String>(&encoded, "packaging_code", Direction::Ascending),
                Err(Invalid),
                "{encoded}"
            );
        }
        assert!(decode::<String>(&valid, "created_at", Direction::Ascending).is_err());
        assert!(decode::<String>(&valid, "packaging_code", Direction::Descending).is_err());
        assert!(decode::<Uuid>(&valid, "packaging_code", Direction::Ascending).is_err());
    }
}
