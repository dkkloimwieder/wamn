//! The wire scalars every operation parses, spelled once.
//!
//! Parse and RE-SPELL, not merely validate. The canonicalization contract
//! fixes uuids as lowercase-hyphenated and timestamps as UTC RFC 3339 with six
//! fractional digits, so a caller's spelling must reach the database -- and
//! the command bytes -- in one form, or two deliveries of the same command
//! would canonicalize differently and the idempotency key would stop working.

use wamn_postgres_statements::{TimestampTz, Uuid};

use crate::error::{AccessError, AccessErrorKind};

/// The three statuses an appointment holds, in the order it moves through.
pub(crate) const SCHEDULED: &str = "scheduled";

pub(crate) fn uuid(field: &str, value: &str) -> Result<Uuid, AccessError> {
    value
        .parse::<uuid::Uuid>()
        .map(|parsed| Uuid(parsed.hyphenated().to_string()))
        .map_err(|_| AccessError::field(AccessErrorKind::InvalidInput, field))
}

pub(crate) fn timestamp(field: &str, value: &str) -> Result<TimestampTz, AccessError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|parsed| TimestampTz(spell(parsed.to_utc())))
        .map_err(|_| AccessError::field(AccessErrorKind::InvalidInput, field))
}

/// One calendar day, read as the UTC day it names.
///
/// A dispatcher asks for "one dock on 2026-10-01". The scenario pins slot
/// times to RFC 3339 with a `Z` offset, so the day that answer covers is the
/// UTC day, half open: `[day, day + 1)`. Naming the boundary here keeps it out
/// of the SQL, where the session time zone would decide it instead.
pub(crate) fn day(field: &str, value: &str) -> Result<(TimestampTz, TimestampTz), AccessError> {
    let refuse = || AccessError::field(AccessErrorKind::InvalidInput, field);
    let start = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| refuse())?;
    let end = start.succ_opt().ok_or_else(refuse)?;
    Ok((TimestampTz(midnight(start)), TimestampTz(midnight(end))))
}

fn midnight(date: chrono::NaiveDate) -> String {
    spell(
        date.and_hms_opt(0, 0, 0)
            .expect("midnight is a valid time")
            .and_utc(),
    )
}

fn spell(moment: chrono::DateTime<chrono::Utc>) -> String {
    moment.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

/// The status an appointment can be asked about. Closed, because the input
/// contract publishes exactly these three.
pub(crate) fn status(field: &str, value: &str) -> Result<String, AccessError> {
    match value {
        SCHEDULED | "arrived" | "departed" => Ok(value.to_owned()),
        _ => Err(AccessError::field(AccessErrorKind::InvalidInput, field)),
    }
}

/// A name a carrier or a dock is addressed by in prose. Blank is refused here
/// rather than by the table's check constraint, whose violation the contract
/// can only report as `internal_error`.
pub(crate) fn name(field: &str, value: &str) -> Result<String, AccessError> {
    if value.trim().is_empty() {
        return Err(AccessError::field(AccessErrorKind::InvalidInput, field));
    }
    Ok(value.to_owned())
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
            timestamp("f", "2026-10-01T11:00:00+02:00").unwrap().0,
            "2026-10-01T09:00:00.000000Z"
        );
        assert!(timestamp("f", "yesterday").is_err());
    }

    /// The spelling above is the one the published input contracts declare.
    /// A reference is never more lenient than the contract it proves, so the
    /// re-spelling and the declared token move together or this fails. The
    /// refusal literals are welded to their contract the same way, in
    /// `error.rs`.
    #[test]
    fn the_canonical_forms_are_the_ones_the_contracts_publish() {
        for operation in [
            "carrier/create",
            "dock/create",
            "appointment/book",
            "appointment/check_in",
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../packages/dock/generated/contracts")
                .join(format!("{operation}.input.json"));
            let contract: serde_json::Value = serde_json::from_slice(
                &std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())),
            )
            .expect("parses");
            assert_eq!(
                contract["canonicalization"]["timestamptz"], "utc_rfc3339_six_fractional_digits",
                "{operation} declares another timestamp form"
            );
            assert_eq!(
                contract["canonicalization"]["uuid"], "lowercase_hyphenated",
                "{operation} declares another uuid form"
            );
        }
    }

    /// A day is the half-open UTC day it names, so an appointment at
    /// `23:59:59Z` belongs to that day and one at `00:00:00Z` the next belongs
    /// to the next.
    #[test]
    fn a_day_is_the_half_open_utc_day() {
        let (start, end) = day("f", "2026-10-01").unwrap();
        assert_eq!(start.0, "2026-10-01T00:00:00.000000Z");
        assert_eq!(end.0, "2026-10-02T00:00:00.000000Z");
        for refused in ["2026-10", "2026-13-01", "01-10-2026", ""] {
            assert!(day("f", refused).is_err(), "{refused:?} must refuse");
        }
    }

    #[test]
    fn a_status_is_one_of_the_three_the_contract_publishes() {
        for admitted in ["scheduled", "arrived", "departed"] {
            assert_eq!(status("f", admitted).unwrap(), admitted);
        }
        assert!(status("f", "cancelled").is_err());
    }

    #[test]
    fn a_blank_name_refuses_before_any_statement() {
        assert!(name("f", "   ").is_err());
        assert_eq!(name("f", "Northwind Freight").unwrap(), "Northwind Freight");
    }
}
