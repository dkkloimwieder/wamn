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

/// A positive quantity, RE-SPELLED as PostgreSQL's own text for the same
/// datum. Zero is refused here rather than by the `quantity > 0` check
/// constraints, whose violation the contract can only report as
/// `internal_error`.
///
/// The respellings are TEXTUAL, and only textual, because a numeric's scale is
/// part of its value: measured on PostgreSQL 18.6, `12.3400` is scale 4 and
/// `12.34` is scale 2, so collapsing one to the other would change what the
/// caller wrote. These three move digits and leave scale alone --- `01.0` ->
/// `1.0`, `1.` -> `1`, `.1` -> `0.1` --- each matching `(value::numeric)::text`.
///
/// An exponent is REFUSED by decision, not by oversight. PostgreSQL DERIVES
/// scale from an exponent rather than reading it: `1e2` is 100 at scale 0,
/// `1e-2` is 0.01 at scale 2, `1.5e2` is 150 at scale 0. A branch for it could
/// not be normalization; it would have to reimplement that derivation by hand,
/// and this crate carries no decimal dependency to do it with.
/// `receiving-data`'s `canonical_positive_numeric` refuses exponents, and
/// respells these same three spellings, for the same reasons.
pub(crate) fn numeric(field: &str, value: &str) -> Result<Numeric, AccessError> {
    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (value, None),
    };
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    // Requiring a non-zero digit also refuses the two spellings PostgreSQL
    // itself rejects, `""` and `"."`, which carry no digit at all.
    let positive = whole
        .bytes()
        .chain(fraction.unwrap_or_default().bytes())
        .any(|byte| byte != b'0');
    if !digits(whole) || !fraction.is_none_or(digits) || !positive {
        return Err(AccessError::field(AccessErrorKind::InvalidInput, field));
    }
    let whole = match whole.trim_start_matches('0') {
        "" => "0",
        trimmed => trimmed,
    };
    let respelled = match fraction.filter(|fraction| !fraction.is_empty()) {
        Some(fraction) => format!("{whole}.{fraction}"),
        None => whole.to_owned(),
    };
    Ok(Numeric(respelled))
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
    fn a_quantity_status_is_never_consumed() {
        assert!(quantity_status("f", "available").is_ok());
        assert!(quantity_status("f", "held").is_ok());
        assert!(quantity_status("f", "consumed").is_err());
        assert!(quantity_status("f", "").is_err());
    }
}
