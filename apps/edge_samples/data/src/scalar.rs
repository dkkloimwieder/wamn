//! The wire scalars the operations parse, spelled once.
//!
//! Parse and re-spell, not merely validate. The canonical command fixes uuids
//! as lowercase-hyphenated and timestamps as UTC RFC 3339 with six fractional
//! digits, so two deliveries of one sample canonicalize alike.

use wamn_postgres_statements::{TimestampTz, Uuid};

use crate::error::{AccessError, AccessErrorKind};

pub(crate) fn uuid(field: &str, value: &str) -> Result<Uuid, AccessError> {
    value
        .parse::<uuid::Uuid>()
        .map(|parsed| Uuid(parsed.hyphenated().to_string()))
        .map_err(|_| AccessError::field(AccessErrorKind::InvalidInput, field))
}

/// A required text value. A blank value refuses on its field.
pub(crate) fn text<'a>(field: &str, value: &'a str) -> Result<&'a str, AccessError> {
    if value.trim().is_empty() {
        return Err(AccessError::field(AccessErrorKind::InvalidInput, field));
    }
    Ok(value)
}

pub(crate) fn timestamp(field: &str, value: &str) -> Result<TimestampTz, AccessError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|parsed| TimestampTz(parsed.to_utc().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()))
        .map_err(|_| AccessError::field(AccessErrorKind::InvalidInput, field))
}
