//! The wire scalars every operation parses, spelled once.
//!
//! Parse scalar text for SQL input. Request identity uses the supplied JSON.

use wamn_postgres_statements::{Numeric, TimestampTz, Uuid};

use crate::data::error::{AccessError, AccessErrorType};

/// The packaging status a command refuses to work on: a consumed packaging is
/// history, not live stock (`inventory_aggregate.sql` says why).
pub(crate) const CONSUMED: &str = "consumed";

pub(crate) fn uuid(field: &str, value: &str) -> Result<Uuid, AccessError> {
    wamn_data_access::scalar::uuid(value)
        .map_err(|_| AccessError::field(AccessErrorType::InvalidInput, field))
}

pub(crate) fn timestamp(field: &str, value: &str) -> Result<TimestampTz, AccessError> {
    wamn_scalar::canonical_timestamptz(value)
        .map(TimestampTz)
        .ok_or_else(|| AccessError::field(AccessErrorType::InvalidInput, field))
}

/// A positive quantity, RE-SPELLED as PostgreSQL's own text for the same
/// datum by [`wamn_scalar::canonical_numeric`], which keeps the
/// scale and refuses an exponent. Zero is refused here rather than by the
/// `quantity > 0` check constraints, whose violation the contract can only
/// report as `internal_error`. A sign is refused too: a quantity is positive.
pub(crate) fn numeric(field: &str, value: &str) -> Result<Numeric, AccessError> {
    let positive = value
        .bytes()
        .any(|byte| byte.is_ascii_digit() && byte != b'0');
    wamn_scalar::canonical_numeric(value)
        .filter(|respelled| positive && !respelled.starts_with('-'))
        .map(Numeric)
        .ok_or_else(|| AccessError::field(AccessErrorType::InvalidInput, field))
}

/// A counted quantity: zero or positive, RE-SPELLED as [`numeric`] does. A
/// count of zero is how an adjust says that the stock is gone.
pub(crate) fn count(field: &str, value: &str) -> Result<Numeric, AccessError> {
    wamn_scalar::canonical_numeric(value)
        .filter(|respelled| !respelled.starts_with('-'))
        .map(Numeric)
        .ok_or_else(|| AccessError::field(AccessErrorType::InvalidInput, field))
}

/// The type of a packaging. A pallet is one type of packaging.
pub(crate) fn packaging_type(field: &str, value: &str) -> Result<String, AccessError> {
    match value {
        "pallet" | "tote" | "bin" | "case" | "loose" => Ok(value.to_owned()),
        _ => Err(AccessError::field(AccessErrorType::InvalidInput, field)),
    }
}

/// The status of a QUANTITY row, which is never `consumed`: consumption is a
/// packaging's fate, and its rows keep the status they had.
pub(crate) fn quantity_status(field: &str, value: &str) -> Result<String, AccessError> {
    match value {
        "available" | "held" => Ok(value.to_owned()),
        _ => Err(AccessError::field(AccessErrorType::InvalidInput, field)),
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
        assert_eq!(numeric("f", "12.3400").unwrap().0, "12.3400");
        // Respelled, not refused: PostgreSQL 18.6 reads each of these as the
        // value on the right, at the same scale.
        for (written, respelled) in [(".5", "0.5"), ("5.", "5"), ("01.0", "1.0"), ("010", "10")] {
            assert_eq!(numeric("f", written).unwrap().0, respelled);
        }
        for refused in ["", ".", "0", "0.0", "00.000", "-1", "1e3", " 1", "1,5"] {
            assert!(numeric("f", refused).is_err(), "{refused:?} must refuse");
        }
    }

    #[test]
    fn a_count_admits_zero_and_refuses_a_sign() {
        for zero in ["0", "0.00"] {
            assert_eq!(count("f", zero).unwrap().0, zero);
        }
        assert_eq!(count("f", "0.5").unwrap().0, "0.5");
        for refused in ["", "-1", "-0", "1e3"] {
            assert!(count("f", refused).is_err(), "{refused:?} must refuse");
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
