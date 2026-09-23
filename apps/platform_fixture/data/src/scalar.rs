//! The wire scalars the fixture operations parse, spelled once.
//!
//! Parse and re-spell, not merely validate. A command hashes the re-spelled
//! text, so two spellings of one value make one command.

use wamn_postgres_statements::{Numeric, Uuid};

use crate::error::{AccessError, AccessErrorKind};

pub(crate) fn uuid(field: &str, value: &str) -> Result<Uuid, AccessError> {
    value
        .parse::<uuid::Uuid>()
        .map(|parsed| Uuid(parsed.hyphenated().to_string()))
        .map_err(|_| AccessError::field(AccessErrorKind::InvalidInput, field))
}

/// A positive numeric, re-spelled as PostgreSQL's own text for the same datum.
///
/// The re-spelling moves digits and keeps the scale: `01.0` becomes `1.0`,
/// `1.` becomes `1`, and `.1` becomes `0.1`. An exponent is refused, as the
/// Receiving and WMS data access refuse it, because PostgreSQL derives the
/// scale of an exponent rather than reading it.
pub(crate) fn positive_numeric(field: &str, value: &str) -> Result<Numeric, AccessError> {
    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (value, None),
    };
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
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
    Ok(Numeric(
        match fraction.filter(|fraction| !fraction.is_empty()) {
            Some(fraction) => format!("{whole}.{fraction}"),
            None => whole.to_owned(),
        },
    ))
}
