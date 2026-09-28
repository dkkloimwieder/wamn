//! The wire scalars a generated data function parses, spelled once.
//!
//! Parse and re-spell, not merely validate. A uuid is lowercase and
//! hyphenated, a timestamp is UTC RFC 3339 with six fractional digits, and a
//! numeric is PostgreSQL's own text for the same datum. One value then reaches
//! the database, and the command bytes, in one form.

use serde_json::{Value, json};
use wamn_execution_contract::{canonical_numeric, canonical_timestamptz};
use wamn_postgres_statements::{Json, Numeric, TimestampTz, Uuid};

use crate::Invalid;

/// A uuid in its one spelling.
///
/// # Errors
///
/// [`Invalid`] when the value is not a uuid.
pub fn uuid(value: &str) -> Result<Uuid, Invalid> {
    value
        .parse::<uuid::Uuid>()
        .map(|parsed| Uuid(parsed.hyphenated().to_string()))
        .map_err(|_| Invalid)
}

/// A timestamp in its one spelling.
///
/// # Errors
///
/// [`Invalid`] when the value is not an RFC 3339 timestamp.
pub fn timestamptz(value: &str) -> Result<TimestampTz, Invalid> {
    canonical_timestamptz(value).map(TimestampTz).ok_or(Invalid)
}

/// A numeric in PostgreSQL's own spelling, at the scale the caller wrote.
///
/// # Errors
///
/// [`Invalid`] when the value is not a plain decimal.
pub fn numeric(value: &str) -> Result<Numeric, Invalid> {
    canonical_numeric(value).map(Numeric).ok_or(Invalid)
}

/// A JSON document.
///
/// # Errors
///
/// [`Invalid`] when the value is not JSON.
pub fn json(value: &str) -> Result<Json, Invalid> {
    serde_json::from_str::<Value>(value)
        .map(|value| Json(value.to_string()))
        .map_err(|_| Invalid)
}

/// The values of a list filter, bound as one JSON array.
pub fn json_list(values: &[String]) -> Json {
    Json(json!(values).to_string())
}

/// The value of an is-null filter, bound as one JSON boolean.
pub fn json_boolean(value: bool) -> Json {
    Json(json!(value).to_string())
}

/// The bounds of a range filter, each inclusive and optional, bound as one
/// JSON object.
pub fn json_range(min: Option<&str>, max: Option<&str>) -> Json {
    Json(json!({ "min": min, "max": max }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_timestamps_and_numerics_are_respelled() {
        assert_eq!(
            uuid("01234567-89AB-CDEF-0123-456789ABCDEF").map(|value| value.0),
            Ok("01234567-89ab-cdef-0123-456789abcdef".to_owned())
        );
        assert_eq!(uuid("not-a-uuid"), Err(Invalid));
        assert_eq!(
            timestamptz("2026-09-05T02:00:00+02:00").map(|value| value.0),
            Ok("2026-09-05T00:00:00.000000Z".to_owned())
        );
        assert_eq!(timestamptz("yesterday"), Err(Invalid));
        assert_eq!(numeric("01.50").map(|value| value.0), Ok("1.50".to_owned()));
        assert_eq!(numeric("1e3"), Err(Invalid));
    }
}
