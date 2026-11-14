//! SQLx value conversion for the PostgreSQL guest interface.

use chrono::{DateTime, Utc};
use sqlx::postgres::{PgArgumentBuffer, PgRow, PgStatement, PgTypeInfo, PgValueFormat, PgValueRef};
use sqlx::types::{BigDecimal, Json};
use sqlx::{
    Column as _, Decode, Encode, Row as _, Statement as _, Type, TypeInfo as _, ValueRef as _,
};

use super::{Column, PgError, SqlValue};

pub(super) fn columns_of(stmt: &PgStatement) -> Vec<Column> {
    stmt.columns()
        .iter()
        .map(|column| Column {
            name: column.name().to_owned(),
            type_name: match column.type_info().name() {
                "CHAR" => "bpchar".into(),
                name => name.to_ascii_lowercase(),
            },
        })
        .collect()
}

pub(super) fn decode_row(row: &PgRow) -> Result<Vec<SqlValue>, PgError> {
    (0..row.len())
        .map(|index| {
            let value = row
                .try_get_raw(index)
                .map_err(|error| map_pg_error(&error))?;
            if value.is_null() {
                return Ok(SqlValue::Null);
            }
            let decoded = match value.type_info().name() {
                "BOOL" => SqlValue::Boolean(row.try_get(index).map_err(|e| map_pg_error(&e))?),
                "INT2" => SqlValue::Int32(i32::from(
                    row.try_get::<i16, _>(index).map_err(|e| map_pg_error(&e))?,
                )),
                "INT4" => SqlValue::Int32(row.try_get(index).map_err(|e| map_pg_error(&e))?),
                "INT8" => SqlValue::Int64(row.try_get(index).map_err(|e| map_pg_error(&e))?),
                "FLOAT4" => SqlValue::Float64(f64::from(
                    row.try_get::<f32, _>(index).map_err(|e| map_pg_error(&e))?,
                )),
                "FLOAT8" => SqlValue::Float64(row.try_get(index).map_err(|e| map_pg_error(&e))?),
                "BYTEA" => SqlValue::Bytes(row.try_get(index).map_err(|e| map_pg_error(&e))?),
                "NUMERIC" => SqlValue::Numeric(
                    row.try_get::<Numeric, _>(index)
                        .map_err(|e| map_pg_error(&e))?
                        .0,
                ),
                "TIMESTAMPTZ" => SqlValue::Timestamptz(canonical_timestamptz(
                    row.try_get(index).map_err(|e| map_pg_error(&e))?,
                )),
                "JSON" | "JSONB" => SqlValue::Json(
                    row.try_get::<Json<Box<serde_json::value::RawValue>>, _>(index)
                        .map_err(|e| map_pg_error(&e))?
                        .0
                        .get()
                        .to_owned(),
                ),
                "UUID" => SqlValue::Uuid(
                    row.try_get::<uuid::Uuid, _>(index)
                        .map_err(|e| map_pg_error(&e))?
                        .to_string(),
                ),
                "TEXT" | "VARCHAR" | "CHAR" | "NAME" | "UNKNOWN" => {
                    SqlValue::Text(row.try_get(index).map_err(|e| map_pg_error(&e))?)
                }
                other => {
                    return Err(PgError::QueryError((
                        "WAMN1".into(),
                        format!("unsupported column type {other}"),
                    )));
                }
            };
            Ok(decoded)
        })
        .collect()
}

pub(super) fn map_pg_error(error: &sqlx::Error) -> PgError {
    if let Some(database) = error.as_database_error() {
        let constraint = || database.constraint().unwrap_or_default().to_owned();
        return match database.code().as_deref().unwrap_or("XX000") {
            "40001" | "40P01" => PgError::SerializationFailure,
            "57014" => PgError::StatementTimeout,
            "23505" => PgError::UniqueViolation(constraint()),
            "23503" => PgError::ForeignKeyViolation(constraint()),
            "23514" => PgError::CheckViolation(constraint()),
            "23P01" => PgError::ExclusionViolation(constraint()),
            "42501" => PgError::PermissionDenied,
            code => PgError::QueryError((code.to_owned(), database.message().to_owned())),
        };
    }
    if connection_lost(error) {
        return PgError::ConnectionUnavailable;
    }
    PgError::QueryError(("XX000".into(), error.to_string()))
}

pub(super) fn connection_lost(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::Protocol(_)
            | sqlx::Error::PoolClosed
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::WorkerCrashed
    )
}

/// SQLx decodes finite decimals. PostgreSQL also permits three non-finite values.
struct Numeric(String);

impl Type<sqlx::Postgres> for Numeric {
    fn type_info() -> PgTypeInfo {
        BigDecimal::type_info()
    }
}

impl<'r> Decode<'r, sqlx::Postgres> for Numeric {
    fn decode(value: PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        if value.format() == PgValueFormat::Text {
            return Ok(Self(value.as_str()?.to_owned()));
        }
        let special = match value.as_bytes()?.get(4..6) {
            Some([0xc0, 0]) => Some("NaN"),
            Some([0xd0, 0]) => Some("Infinity"),
            Some([0xf0, 0]) => Some("-Infinity"),
            _ => None,
        };
        Ok(Self(match special {
            Some(value) => value.to_owned(),
            None => BigDecimal::decode(value)?.to_plain_string(),
        }))
    }
}

/// A guest value binds with its native PostgreSQL type.
#[derive(Debug)]
pub(super) struct PgParam<'a>(pub(super) &'a SqlValue, pub(super) Option<&'a PgTypeInfo>);

impl<'a> PgParam<'a> {
    pub(super) fn prepared(value: &'a SqlValue, statement: &'a PgStatement, index: usize) -> Self {
        let ty = match statement.parameters() {
            Some(sqlx::Either::Left(parameters)) => parameters.get(index),
            _ => None,
        };
        Self(value, ty)
    }
}

impl Type<sqlx::Postgres> for PgParam<'_> {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(sqlx::postgres::types::Oid(0))
    }
    fn compatible(_: &PgTypeInfo) -> bool {
        true
    }
}

impl Encode<'_, sqlx::Postgres> for PgParam<'_> {
    fn produces(&self) -> Option<PgTypeInfo> {
        if let Some(ty) = self.1 {
            return Some(ty.clone());
        }
        Some(match self.0 {
            SqlValue::Null => Self::type_info(),
            SqlValue::Boolean(_) => <bool as Type<sqlx::Postgres>>::type_info(),
            SqlValue::Int32(_) => <i32 as Type<sqlx::Postgres>>::type_info(),
            SqlValue::Int64(_) => <i64 as Type<sqlx::Postgres>>::type_info(),
            SqlValue::Float64(_) => <f64 as Type<sqlx::Postgres>>::type_info(),
            SqlValue::Bytes(_) => <Vec<u8> as Type<sqlx::Postgres>>::type_info(),
            SqlValue::Text(_) => <String as Type<sqlx::Postgres>>::type_info(),
            SqlValue::Numeric(_) => BigDecimal::type_info(),
            SqlValue::Timestamptz(_) => <DateTime<Utc> as Type<sqlx::Postgres>>::type_info(),
            SqlValue::Json(_) => <Json<serde_json::Value> as Type<sqlx::Postgres>>::type_info(),
            SqlValue::Uuid(_) => <uuid::Uuid as Type<sqlx::Postgres>>::type_info(),
        })
    }

    fn encode_by_ref(
        &self,
        buffer: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        match self.0 {
            SqlValue::Null => Ok(sqlx::encode::IsNull::Yes),
            SqlValue::Boolean(value) => {
                <bool as Encode<sqlx::Postgres>>::encode_by_ref(value, buffer)
            }
            SqlValue::Int32(value) if self.1.is_some_and(|ty| ty.name() == "INT2") => {
                <i16 as Encode<sqlx::Postgres>>::encode_by_ref(&i16::try_from(*value)?, buffer)
            }
            SqlValue::Int32(value) => <i32 as Encode<sqlx::Postgres>>::encode_by_ref(value, buffer),
            SqlValue::Int64(value) => <i64 as Encode<sqlx::Postgres>>::encode_by_ref(value, buffer),
            SqlValue::Float64(value) if self.1.is_some_and(|ty| ty.name() == "FLOAT4") => {
                let narrowed = *value as f32;
                if value.is_finite() && !narrowed.is_finite() {
                    return Err("value is outside the PostgreSQL REAL range".into());
                }
                <f32 as Encode<sqlx::Postgres>>::encode_by_ref(&narrowed, buffer)
            }
            SqlValue::Float64(value) => {
                <f64 as Encode<sqlx::Postgres>>::encode_by_ref(value, buffer)
            }
            SqlValue::Bytes(value) => {
                <Vec<u8> as Encode<sqlx::Postgres>>::encode_by_ref(value, buffer)
            }
            SqlValue::Text(value) => {
                <String as Encode<sqlx::Postgres>>::encode_by_ref(value, buffer)
            }
            SqlValue::Numeric(value) => {
                let sign = match value.as_str() {
                    "NaN" => Some(0xc0),
                    "Infinity" => Some(0xd0),
                    "-Infinity" => Some(0xf0),
                    _ => None,
                };
                if let Some(sign) = sign {
                    buffer.extend_from_slice(&[0, 0, 0, 0, sign, 0, 0, 0]);
                    Ok(sqlx::encode::IsNull::No)
                } else {
                    value.parse::<BigDecimal>()?.encode_by_ref(buffer)
                }
            }
            SqlValue::Timestamptz(value) => {
                <DateTime<Utc> as Encode<sqlx::Postgres>>::encode_by_ref(
                    &value.parse::<DateTime<Utc>>()?,
                    buffer,
                )
            }
            SqlValue::Json(value) => {
                <Json<Box<serde_json::value::RawValue>> as Encode<sqlx::Postgres>>::encode_by_ref(
                    &Json(serde_json::from_str::<Box<serde_json::value::RawValue>>(
                        value,
                    )?),
                    buffer,
                )
            }
            SqlValue::Uuid(value) => <uuid::Uuid as Encode<sqlx::Postgres>>::encode_by_ref(
                &value.parse::<uuid::Uuid>()?,
                buffer,
            ),
        }
    }
}

pub fn canonical_timestamptz(value: DateTime<Utc>) -> String {
    wamn_scalar::format_timestamptz(value)
}
