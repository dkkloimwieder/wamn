//! The wire scalars every operation parses, spelled once.
//!
//! Parse and RE-SPELL, not merely validate. The canonicalization contract
//! fixes uuids as lowercase-hyphenated and timestamps as UTC RFC 3339 with six
//! fractional digits, so a caller's spelling must reach the database -- and
//! the command bytes -- in one form, or two deliveries of the same command
//! would canonicalize differently and the idempotency key would stop working.

use wamn_postgres_statements::{Numeric, TimestampTz, Uuid};

use crate::error::{AccessError, AccessErrorKind};

/// The pallet status a command refuses to work on: a consumed pallet is
/// history, not live stock (`inventory_aggregate.sql` says why).
pub(crate) const CONSUMED: &str = "consumed";

pub(crate) fn uuid(field: &str, value: &str) -> Result<Uuid, AccessError> {
    value
        .parse::<uuid::Uuid>()
        .map(|parsed| Uuid(parsed.hyphenated().to_string()))
        .map_err(|_| AccessError::field(AccessErrorKind::InvalidInput, field))
}

pub(crate) fn timestamp(field: &str, value: &str) -> Result<TimestampTz, AccessError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|parsed| TimestampTz(parsed.to_utc().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()))
        .map_err(|_| AccessError::field(AccessErrorKind::InvalidInput, field))
}

/// A positive quantity in the contract's lexical form, `[0-9]+(.[0-9]+)?`,
/// passed to PostgreSQL scale-preserved. Zero is refused here rather than by
/// the `quantity > 0` check constraints, whose violation the contract can only
/// report as `internal_error`.
pub(crate) fn numeric(field: &str, value: &str) -> Result<Numeric, AccessError> {
    let refuse = || AccessError::field(AccessErrorKind::InvalidInput, field);
    let (whole, fraction) = value.split_once('.').map_or((value, ""), |(w, f)| (w, f));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(whole) || (value.contains('.') && !digits(fraction)) {
        return Err(refuse());
    }
    if value.bytes().all(|byte| byte == b'0' || byte == b'.') {
        return Err(refuse());
    }
    Ok(Numeric(value.to_owned()))
}

/// The status of a QUANTITY row, which is never `consumed`: consumption is a
/// pallet's fate, and its rows keep the status they had.
pub(crate) fn quantity_status(field: &str, value: &str) -> Result<String, AccessError> {
    match value {
        "available" | "held" => Ok(value.to_owned()),
        _ => Err(AccessError::field(AccessErrorKind::InvalidInput, field)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_and_timestamps_are_respelled_canonically() {
        assert_eq!(
            uuid("f", "01234567-89AB-CDEF-0123-456789ABCDEF").unwrap().0,
            "01234567-89ab-cdef-0123-456789abcdef"
        );
        assert!(uuid("f", "not-a-uuid").is_err());
        assert_eq!(
            timestamp("f", "2026-09-05T02:00:00+02:00").unwrap().0,
            "2026-09-05T00:00:00.000000Z"
        );
        assert!(timestamp("f", "yesterday").is_err());
    }

    #[test]
    fn a_quantity_is_lexical_positive_and_scale_preserved() {
        assert_eq!(numeric("f", "10").unwrap().0, "10");
        assert_eq!(numeric("f", "0.250").unwrap().0, "0.250");
        for refused in [
            "", "0", "0.0", "00.000", ".5", "5.", "-1", "1e3", " 1", "1,5",
        ] {
            assert!(numeric("f", refused).is_err(), "{refused:?} must refuse");
        }
    }

    #[test]
    fn a_quantity_status_is_never_consumed() {
        assert!(quantity_status("f", "available").is_ok());
        assert!(quantity_status("f", "held").is_ok());
        assert!(quantity_status("f", "consumed").is_err());
        assert!(quantity_status("f", "").is_err());
    }
}
