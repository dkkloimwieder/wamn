//! Keyset cursors with the record id's declared scalar type.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use wamn_execution_contract::canonical_json_bytes;
use wamn_postgres_statements::Uuid;

use super::{Direction, Key, VERSION};
use crate::Invalid;

/// A record id in the same JSON form as an operation field.
pub trait Id: Sized {
    /// Encode the id as a UUID string, int32 number, or int64 string.
    fn to_json(&self) -> Result<Value, Invalid>;
    /// Read the id in its declared JSON form.
    fn from_json(value: &Value) -> Result<Self, Invalid>;
}

impl Id for i32 {
    #[inline]
    fn to_json(&self) -> Result<Value, Invalid> {
        Key::to_json(self)
    }

    #[inline]
    fn from_json(value: &Value) -> Result<Self, Invalid> {
        Key::from_json(value)
    }
}

impl Id for i64 {
    #[inline]
    fn to_json(&self) -> Result<Value, Invalid> {
        Ok(Value::String(self.to_string()))
    }

    #[inline]
    fn from_json(value: &Value) -> Result<Self, Invalid> {
        value.as_str().ok_or(Invalid)?.parse().map_err(|_| Invalid)
    }
}

impl Id for Uuid {
    #[inline]
    fn to_json(&self) -> Result<Value, Invalid> {
        Key::to_json(self)
    }

    #[inline]
    fn from_json(value: &Value) -> Result<Self, Invalid> {
        Key::from_json(value)
    }
}

/// Encode a keyset cursor with the record id's declared scalar type.
///
/// # Errors
///
/// [`Invalid`] when the key or id has no valid encoding.
pub fn encode_with_id<K: Key, I: Id>(
    field: &str,
    direction: Direction,
    key: &K,
    id: &I,
) -> Result<String, Invalid> {
    Ok(URL_SAFE_NO_PAD.encode(canonical_json_bytes(&json!({
        "direction": direction.as_str(),
        "field": field,
        "id": id.to_json()?,
        "key": key.to_json()?,
        "v": VERSION,
    }))))
}

/// Decode a keyset cursor with the record id's declared scalar type.
///
/// # Errors
///
/// [`Invalid`] for a cursor that [`encode_with_id`] did not write for this sort.
pub fn decode_with_id<K: Key, I: Id>(
    encoded: &str,
    field: &str,
    direction: Direction,
) -> Result<(K, I), Invalid> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| Invalid)?;
    let wire: Value = serde_json::from_slice(&bytes).map_err(|_| Invalid)?;
    let key = K::from_json(wire.get("key").ok_or(Invalid)?)?;
    let id = I::from_json(wire.get("id").ok_or(Invalid)?)?;
    if encode_with_id(field, direction, &key, &id)? != encoded {
        return Err(Invalid);
    }
    Ok((key, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn integer_ids_keep_the_operation_wire_form_and_full_range() {
        let key = "same".to_owned();
        for id in [i32::MIN, 0, i32::MAX] {
            let encoded = encode_with_id("label", Direction::Ascending, &key, &id).unwrap();
            let wire: Value =
                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&encoded).unwrap()).unwrap();
            assert_eq!(wire["id"], json!(id));
            assert_eq!(
                decode_with_id::<String, i32>(&encoded, "label", Direction::Ascending),
                Ok((key.clone(), id))
            );
            assert!(
                decode_with_id::<String, i64>(&encoded, "label", Direction::Ascending).is_err()
            );
        }
        for id in [i64::MIN, 0, 9_007_199_254_740_993, i64::MAX] {
            let encoded = encode_with_id("label", Direction::Descending, &key, &id).unwrap();
            let wire: Value =
                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&encoded).unwrap()).unwrap();
            assert_eq!(wire["id"], json!(id.to_string()));
            assert_eq!(
                decode_with_id::<String, i64>(&encoded, "label", Direction::Descending),
                Ok((key.clone(), id))
            );
            assert!(
                decode_with_id::<String, i32>(&encoded, "label", Direction::Descending).is_err()
            );
            assert!(
                decode_with_id::<String, i64>(&encoded, "label", Direction::Ascending).is_err()
            );
        }
        for id in [
            json!("01"),
            json!("+1"),
            json!("9223372036854775808"),
            json!(1),
            Value::Null,
        ] {
            let wire = json!({"direction": "ascending", "field": "label", "id": id, "key": "same", "v": 1});
            let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&wire).unwrap());
            assert!(
                decode_with_id::<String, i64>(&encoded, "label", Direction::Ascending).is_err()
            );
        }
    }

    #[test]
    fn typed_uuid_cursors_keep_the_existing_wire_form() {
        let key = "same".to_owned();
        let id = Uuid(ID.to_owned());
        let encoded = encode_with_id("label", Direction::Ascending, &key, &id).unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(&encoded).unwrap(), br#"{"direction":"ascending","field":"label","id":"01234567-89ab-cdef-0123-456789abcdef","key":"same","v":1}"#);
        let (decoded_key, decoded_id) =
            decode_with_id::<String, Uuid>(&encoded, "label", Direction::Ascending).unwrap();
        assert_eq!(decoded_key, key);
        assert_eq!(decoded_id.0, id.0);
        assert!(decode_with_id::<String, i32>(&encoded, "label", Direction::Ascending).is_err());
        assert!(decode_with_id::<String, i64>(&encoded, "label", Direction::Ascending).is_err());
    }
}

#[cfg(test)]
mod uuid_tests {
    use super::*;
    use wamn_postgres_statements::TimestampTz;

    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn a_cursor_has_one_wire_spelling_and_round_trips() {
        let key = TimestampTz("2026-08-29T14:34:56+02:00".to_owned());
        let encoded = encode_with_id(
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
        let (key, id) =
            decode_with_id::<TimestampTz, Uuid>(&encoded, "created_at", Direction::Ascending)
                .expect("its own cursor decodes");
        assert_eq!(key.0, "2026-08-29T12:34:56.000000Z");
        assert_eq!(id.0, ID);
    }

    #[test]
    fn a_cursor_from_another_sort_or_spelling_is_invalid() {
        let valid = encode_with_id(
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
                decode_with_id::<String, Uuid>(&encoded, "packaging_code", Direction::Ascending),
                Err(Invalid),
                "{encoded}"
            );
        }
        assert!(
            decode_with_id::<String, Uuid>(&valid, "created_at", Direction::Ascending).is_err()
        );
        assert!(
            decode_with_id::<String, Uuid>(&valid, "packaging_code", Direction::Descending)
                .is_err()
        );
        assert!(
            decode_with_id::<Uuid, Uuid>(&valid, "packaging_code", Direction::Ascending).is_err()
        );
    }
}
