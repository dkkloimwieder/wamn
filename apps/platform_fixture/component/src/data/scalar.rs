//! The wire scalars the fixture operations parse, spelled once.
//!
//! Parse and re-spell, not merely validate. A command hashes the re-spelled
//! text, so two spellings of one value make one command.

use wamn_postgres_statements::{Numeric, Uuid};

use crate::data::error::{AccessError, AccessErrorType};

pub(crate) fn uuid(field: &str, value: &str) -> Result<Uuid, AccessError> {
    wamn_data_access::scalar::uuid(value)
        .map_err(|_| AccessError::field(AccessErrorType::InvalidInput, field))
}

/// A positive numeric, re-spelled as PostgreSQL's own text for the same datum.
///
/// The re-spelling moves digits and keeps the scale: `01.0` becomes `1.0`,
/// `1.` becomes `1`, and `.1` becomes `0.1`. An exponent is refused, as the
/// Receiving and WMS data access refuse it, because PostgreSQL derives the
/// scale of an exponent rather than reading it.
pub(crate) fn positive_numeric(field: &str, value: &str) -> Result<Numeric, AccessError> {
    let positive = value
        .bytes()
        .any(|byte| byte.is_ascii_digit() && byte != b'0');
    wamn_data_access::scalar::numeric(value)
        .ok()
        .filter(|parsed| positive && !parsed.0.starts_with('-'))
        .ok_or_else(|| AccessError::field(AccessErrorType::InvalidInput, field))
}
