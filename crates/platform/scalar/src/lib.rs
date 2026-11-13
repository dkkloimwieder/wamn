//! Scalar text formats and the platform SQL type vocabulary.

use serde::{Deserialize, Serialize};

/// The non-null cases of frozen `wamn:postgres/types.sql-value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlValueType {
    Boolean,
    Int32,
    Int64,
    Float64,
    Text,
    Bytes,
    Numeric,
    Timestamptz,
    Json,
    Uuid,
}

impl SqlValueType {
    /// Canonical PostgreSQL cast name for this transport type.
    pub fn postgres_name(self) -> &'static str {
        POSTGRES_TYPES
            .iter()
            .find(|(_, ty)| *ty == self)
            .expect("every transport type has a PostgreSQL name")
            .0[0]
    }

    /// Frozen wire literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Int32 => "int32",
            Self::Int64 => "int64",
            Self::Float64 => "float64",
            Self::Text => "text",
            Self::Bytes => "bytes",
            Self::Numeric => "numeric",
            Self::Timestamptz => "timestamptz",
            Self::Json => "json",
            Self::Uuid => "uuid",
        }
    }
}

// The first name is the canonical cast. The remaining names are catalog and protocol aliases.
const POSTGRES_TYPES: &[(&[&str], SqlValueType)] = &[
    (&["boolean", "bool"], SqlValueType::Boolean),
    (
        &["int4", "integer", "int2", "smallint"],
        SqlValueType::Int32,
    ),
    (&["int8", "bigint"], SqlValueType::Int64),
    (
        &["float8", "double precision", "float4", "real"],
        SqlValueType::Float64,
    ),
    (
        &[
            "text",
            "varchar",
            "character varying",
            "bpchar",
            "char",
            "character",
            "name",
            "unknown",
        ],
        SqlValueType::Text,
    ),
    (&["bytea"], SqlValueType::Bytes),
    (&["numeric"], SqlValueType::Numeric),
    (
        &["timestamptz", "timestamp with time zone"],
        SqlValueType::Timestamptz,
    ),
    (&["jsonb", "json"], SqlValueType::Json),
    (&["uuid"], SqlValueType::Uuid),
];

/// Map PostgreSQL catalog, protocol and SQLx names to the transport vocabulary.
#[must_use]
pub fn postgres_type(name: &str) -> Option<SqlValueType> {
    POSTGRES_TYPES.iter().find_map(|(names, ty)| {
        names
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
            .then_some(*ty)
    })
}

/// A `timestamptz` in UTC with six fractional digits, or `None` for a value
/// that is not RFC 3339. Fractional digits beyond microseconds are truncated.
#[must_use]
pub fn canonical_timestamptz(value: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| format_timestamptz(parsed.to_utc()))
}

/// Format a decoded timestamp in UTC with six fractional digits.
#[must_use]
pub fn format_timestamptz(value: chrono::DateTime<chrono::Utc>) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

/// A plain decimal with its lexical scale preserved, or `None` for invalid text.
///
/// The respellings are textual and keep the scale, because a numeric's scale
/// is part of its value: `12.3400` is scale 4 and `12.34` is scale 2. They move
/// digits only: `01.0` becomes `1.0`, `1.` becomes `1`, and `.1` becomes `0.1`,
/// each matching `(value::numeric)::text`. An exponent is not a plain decimal:
/// PostgreSQL derives the scale from it. A negative zero keeps its sign,
/// unlike PostgreSQL text output.
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
    use super::{SqlValueType, canonical_numeric, canonical_timestamptz, postgres_type};

    #[test]
    fn postgres_aliases_use_the_same_transport_type() {
        for (name, expected) in [
            ("INT2", SqlValueType::Int32),
            ("float4", SqlValueType::Float64),
            ("BPCHAR", SqlValueType::Text),
            ("CHAR", SqlValueType::Text),
            ("varchar", SqlValueType::Text),
            ("name", SqlValueType::Text),
        ] {
            assert_eq!(postgres_type(name), Some(expected));
            assert_eq!(postgres_type(expected.postgres_name()), Some(expected));
        }
        assert!(postgres_type("date").is_none());
    }

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
