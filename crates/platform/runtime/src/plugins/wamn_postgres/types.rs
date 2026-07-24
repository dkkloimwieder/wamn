//! Guest↔host value marshalling for `wamn:postgres` (SR4 split, wamn-cjv.18):
//! text-format parameter encoding, binary-format cell decoding (including the
//! manual binary-NUMERIC → canonical-string decoder), column/row helpers, and
//! error mapping. Pure — no pool, clock, or plugin state.

use chrono::{DateTime, SecondsFormat, Utc};
use tokio_postgres::types::{Format, IsNull, ToSql, Type, to_sql_checked};

use super::{Column, PgError, SqlValue};

pub(super) fn columns_of(stmt: &tokio_postgres::Statement) -> Vec<Column> {
    stmt.columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_string(),
            type_name: c.type_().name().to_string(),
        })
        .collect()
}

pub(super) fn decode_row(row: &tokio_postgres::Row) -> Result<Vec<SqlValue>, PgError> {
    (0..row.len())
        .map(|i| {
            row.try_get::<_, SqlCell>(i).map(|c| c.0).map_err(|e| {
                PgError::QueryError((
                    "WAMN1".to_string(),
                    format!("column {i} decode failed: {e}"),
                ))
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

pub(super) fn map_pg_error(e: &tokio_postgres::Error) -> PgError {
    if let Some(db) = e.as_db_error() {
        let constraint = || db.constraint().unwrap_or_default().to_string();
        return match db.code().code() {
            "40001" | "40P01" => PgError::SerializationFailure,
            "57014" => PgError::StatementTimeout,
            "23505" => PgError::UniqueViolation(constraint()),
            "23503" => PgError::ForeignKeyViolation(constraint()),
            "23514" => PgError::CheckViolation(constraint()),
            // RLS / privilege denials deliberately carry no policy detail.
            "42501" => PgError::PermissionDenied,
            code => PgError::QueryError((code.to_string(), db.message().to_string())),
        };
    }
    if e.is_closed() {
        return PgError::ConnectionUnavailable;
    }
    PgError::QueryError(("XX000".to_string(), e.to_string()))
}

// ---------------------------------------------------------------------------
// Guest→host params: text-format wire encoding
// ---------------------------------------------------------------------------

/// Wraps a WIT `sql-value` as a bound parameter. Values are sent in the text
/// wire format, so the server parses them with the exact semantics of SQL
/// literals for the *declared* parameter type: `numeric`/`timestamptz`/
/// `json`/`uuid` strings stay exact, and there is no client-side type
/// negotiation to disagree with the server.
#[derive(Debug)]
pub(super) struct PgParam(pub(super) SqlValue);

impl ToSql for PgParam {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut bytes::BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        use std::fmt::Write as _;
        match &self.0 {
            SqlValue::Null => return Ok(IsNull::Yes),
            SqlValue::Boolean(b) => out.extend_from_slice(if *b { b"t" } else { b"f" }),
            SqlValue::Int32(v) => {
                let mut s = String::new();
                let _ = write!(s, "{v}");
                out.extend_from_slice(s.as_bytes());
            }
            SqlValue::Int64(v) => {
                let mut s = String::new();
                let _ = write!(s, "{v}");
                out.extend_from_slice(s.as_bytes());
            }
            SqlValue::Float64(v) => {
                let s = if v.is_nan() {
                    "NaN".to_string()
                } else if v.is_infinite() {
                    if *v > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
                } else {
                    // {:?} is the shortest round-trip representation.
                    format!("{v:?}")
                };
                out.extend_from_slice(s.as_bytes());
            }
            SqlValue::Text(s) => out.extend_from_slice(s.as_bytes()),
            SqlValue::Bytes(b) => {
                out.extend_from_slice(b"\\x");
                let mut s = String::with_capacity(b.len() * 2);
                for byte in b {
                    let _ = write!(s, "{byte:02x}");
                }
                out.extend_from_slice(s.as_bytes());
            }
            // Canonical-string types: pass through, server parses per the
            // parameter's declared type.
            SqlValue::Numeric(s)
            | SqlValue::Timestamptz(s)
            | SqlValue::Json(s)
            | SqlValue::Uuid(s) => out.extend_from_slice(s.as_bytes()),
        }
        Ok(IsNull::No)
    }

    fn accepts(_ty: &Type) -> bool {
        // The server validates the text form against the declared parameter
        // type; incompatible values fail there with a mappable error.
        true
    }

    fn encode_format(&self, _ty: &Type) -> Format {
        Format::Text
    }

    to_sql_checked!();
}

// ---------------------------------------------------------------------------
// Host→guest cells: binary wire decoding
// ---------------------------------------------------------------------------

struct SqlCell(SqlValue);

impl<'a> tokio_postgres::types::FromSql<'a> for SqlCell {
    fn from_sql(
        ty: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        let v = match ty.name() {
            "bool" => SqlValue::Boolean(bool::from_sql(ty, raw)?),
            "int2" => SqlValue::Int32(i16::from_sql(ty, raw)? as i32),
            "int4" => SqlValue::Int32(i32::from_sql(ty, raw)?),
            "int8" => SqlValue::Int64(i64::from_sql(ty, raw)?),
            "float4" => SqlValue::Float64(f32::from_sql(ty, raw)? as f64),
            "float8" => SqlValue::Float64(f64::from_sql(ty, raw)?),
            "text" | "varchar" | "bpchar" | "name" | "unknown" => {
                SqlValue::Text(String::from_sql(ty, raw)?)
            }
            "bytea" => SqlValue::Bytes(<&[u8]>::from_sql(ty, raw)?.to_vec()),
            "numeric" => SqlValue::Numeric(decode_binary_numeric(raw)?),
            "timestamptz" => SqlValue::Timestamptz(
                DateTime::<Utc>::from_sql(ty, raw)?.to_rfc3339_opts(SecondsFormat::Micros, false),
            ),
            "json" => SqlValue::Json(std::str::from_utf8(raw)?.to_string()),
            "jsonb" => {
                let (version, body) = raw.split_first().ok_or("empty jsonb value")?;
                if *version != 1 {
                    return Err(format!("unsupported jsonb version {version}").into());
                }
                SqlValue::Json(std::str::from_utf8(body)?.to_string())
            }
            "uuid" => {
                if raw.len() != 16 {
                    return Err("uuid value is not 16 bytes".into());
                }
                let h = |r: &[u8]| {
                    r.iter().fold(String::new(), |mut s, b| {
                        use std::fmt::Write as _;
                        let _ = write!(s, "{b:02x}");
                        s
                    })
                };
                SqlValue::Uuid(format!(
                    "{}-{}-{}-{}-{}",
                    h(&raw[0..4]),
                    h(&raw[4..6]),
                    h(&raw[6..8]),
                    h(&raw[8..10]),
                    h(&raw[10..16]),
                ))
            }
            other => return Err(format!("unsupported column type {other}").into()),
        };
        Ok(SqlCell(v))
    }

    fn from_sql_null(_ty: &Type) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(SqlCell(SqlValue::Null))
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }
}

/// Decode Postgres's binary NUMERIC wire format into its canonical string
/// (the same text `numeric_out` would produce): base-10000 digit groups with
/// a weight (group index of the first group relative to the decimal point)
/// and a display scale.
fn decode_binary_numeric(raw: &[u8]) -> Result<String, Box<dyn std::error::Error + Sync + Send>> {
    fn rd_i16(raw: &[u8], at: usize) -> Result<i16, Box<dyn std::error::Error + Sync + Send>> {
        Ok(i16::from_be_bytes(
            raw.get(at..at + 2).ok_or("truncated numeric")?.try_into()?,
        ))
    }
    let ndigits = rd_i16(raw, 0)? as usize;
    let weight = rd_i16(raw, 2)? as i32;
    let sign = rd_i16(raw, 4)? as u16;
    let dscale = rd_i16(raw, 6)? as u16 as usize;
    match sign {
        0x0000 | 0x4000 => {}
        0xC000 => return Ok("NaN".to_string()),
        0xD000 => return Ok("Infinity".to_string()),
        0xF000 => return Ok("-Infinity".to_string()),
        other => return Err(format!("bad numeric sign {other:#x}").into()),
    }
    let mut digits = Vec::with_capacity(ndigits);
    for i in 0..ndigits {
        digits.push(rd_i16(raw, 8 + i * 2)? as u16);
    }

    use std::fmt::Write as _;
    let mut s = String::new();
    if sign == 0x4000 {
        s.push('-');
    }
    if weight < 0 || ndigits == 0 {
        s.push('0');
    } else {
        for i in 0..=(weight as usize) {
            let d = digits.get(i).copied().unwrap_or(0);
            if i == 0 {
                let _ = write!(s, "{d}");
            } else {
                let _ = write!(s, "{d:04}");
            }
        }
    }
    if dscale > 0 {
        let mut frac = String::new();
        let mut gw = -1i32;
        while frac.len() < dscale {
            let i = weight - gw; // digit index of the group with weight `gw`
            let d = if i >= 0 {
                digits.get(i as usize).copied().unwrap_or(0)
            } else {
                0
            };
            let _ = write!(frac, "{d:04}");
            gw -= 1;
        }
        frac.truncate(dscale);
        s.push('.');
        s.push_str(&frac);
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// WIT host implementations
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(ndigits: i16, weight: i16, sign: u16, dscale: u16, digits: &[u16]) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(&ndigits.to_be_bytes());
        raw.extend_from_slice(&weight.to_be_bytes());
        raw.extend_from_slice(&sign.to_be_bytes());
        raw.extend_from_slice(&dscale.to_be_bytes());
        for d in digits {
            raw.extend_from_slice(&d.to_be_bytes());
        }
        raw
    }

    #[test]
    fn numeric_decode_basic() {
        // 12.3400
        assert_eq!(
            decode_binary_numeric(&enc(2, 0, 0, 4, &[12, 3400])).unwrap(),
            "12.3400"
        );
        // 0.0001
        assert_eq!(
            decode_binary_numeric(&enc(1, -1, 0, 4, &[1])).unwrap(),
            "0.0001"
        );
        // 0.00000001 (weight -2)
        assert_eq!(
            decode_binary_numeric(&enc(1, -2, 0, 8, &[1])).unwrap(),
            "0.00000001"
        );
        // 1234567.89
        assert_eq!(
            decode_binary_numeric(&enc(3, 1, 0, 2, &[123, 4567, 8900])).unwrap(),
            "1234567.89"
        );
        // -42
        assert_eq!(
            decode_binary_numeric(&enc(1, 0, 0x4000, 0, &[42])).unwrap(),
            "-42"
        );
        // 0 and 0.00
        assert_eq!(decode_binary_numeric(&enc(0, 0, 0, 0, &[])).unwrap(), "0");
        assert_eq!(
            decode_binary_numeric(&enc(0, 0, 0, 2, &[])).unwrap(),
            "0.00"
        );
        // 10000 (weight 1, single group)
        assert_eq!(
            decode_binary_numeric(&enc(1, 1, 0, 0, &[1])).unwrap(),
            "10000"
        );
        // NaN
        assert_eq!(
            decode_binary_numeric(&enc(0, 0, 0xC000, 0, &[])).unwrap(),
            "NaN"
        );
    }

    #[test]
    fn param_text_encoding() {
        use tokio_postgres::types::ToSql;
        let mut buf = bytes::BytesMut::new();
        let p = PgParam(SqlValue::Bytes(vec![0xde, 0xad, 0x01]));
        assert!(matches!(
            p.to_sql(&Type::BYTEA, &mut buf).unwrap(),
            IsNull::No
        ));
        assert_eq!(&buf[..], b"\\xdead01");

        let mut buf = bytes::BytesMut::new();
        let p = PgParam(SqlValue::Float64(1.5));
        p.to_sql(&Type::FLOAT8, &mut buf).unwrap();
        assert_eq!(&buf[..], b"1.5");

        let mut buf = bytes::BytesMut::new();
        let p = PgParam(SqlValue::Boolean(true));
        p.to_sql(&Type::BOOL, &mut buf).unwrap();
        assert_eq!(&buf[..], b"t");
    }
}
