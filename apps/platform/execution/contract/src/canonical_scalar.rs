//! The canonical spelling of the scalars that a request is compared by.
//!
//! Two deliveries of one request can spell a time or a number differently.
//! The write log compares requests by their canonical JSON bytes, so the
//! generated codec respells these scalars first, and the data access binds the
//! same spelling. An operation contract states the rule as
//! `timestamptz: utc_rfc3339_six_fractional_digits` and
//! `numeric: postgresql_lexical_scale_preserved`.

/// A `timestamptz` in UTC with six fractional digits, or `None` for a value
/// that is not RFC 3339.
#[must_use]
pub fn canonical_timestamptz(value: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.to_utc().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
}

/// A `numeric` spelled as PostgreSQL's own text for the same datum, or `None`
/// for a value that is not a plain decimal.
///
/// The respellings are textual and keep the scale, because a numeric's scale
/// is part of its value: `12.3400` is scale 4 and `12.34` is scale 2. They move
/// digits only: `01.0` becomes `1.0`, `1.` becomes `1`, and `.1` becomes `0.1`,
/// each matching `(value::numeric)::text`. An exponent is not a plain decimal:
/// PostgreSQL derives the scale from it.
#[must_use]
pub fn canonical_numeric(value: &str) -> Option<String> {
    let (sign, unsigned) = value
        .strip_prefix('-')
        .map_or(("", value), |unsigned| ("-", unsigned));
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (unsigned, None),
    };
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    let fraction = fraction.unwrap_or_default();
    if !digits(whole) || !digits(fraction) || whole.len() + fraction.len() == 0 {
        return None;
    }
    let whole = match whole.trim_start_matches('0') {
        "" => "0",
        trimmed => trimmed,
    };
    Some(if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    })
}

#[cfg(test)]
mod tests {
    use super::{canonical_numeric, canonical_timestamptz};

    #[test]
    fn a_timestamp_is_respelled_in_utc_with_six_fractional_digits() {
        assert_eq!(
            canonical_timestamptz("2026-09-05T02:00:00+02:00").as_deref(),
            Some("2026-09-05T00:00:00.000000Z")
        );
        assert_eq!(
            canonical_timestamptz("2026-09-05T00:00:00.1234567Z").as_deref(),
            Some("2026-09-05T00:00:00.123456Z")
        );
        assert_eq!(canonical_timestamptz("yesterday"), None);
    }

    #[test]
    fn a_numeric_is_respelled_with_its_scale() {
        for (written, respelled) in [
            ("10", "10"),
            ("0.250", "0.250"),
            ("12.3400", "12.3400"),
            (".5", "0.5"),
            ("5.", "5"),
            ("01.0", "1.0"),
            ("010", "10"),
            ("-01.50", "-1.50"),
        ] {
            assert_eq!(canonical_numeric(written).as_deref(), Some(respelled));
        }
        for refused in ["", ".", "-", "1e3", " 1", "1,5", "+1"] {
            assert_eq!(canonical_numeric(refused), None, "{refused:?}");
        }
    }
}
