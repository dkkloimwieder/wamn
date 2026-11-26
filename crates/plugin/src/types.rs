//! Translate SQL values and refusals at the guest interface.

use wamn_app::database::{SqlError, SqlValue as DatabaseValue};

use super::{PgError, SqlValue};

pub(super) fn into_database_value(value: SqlValue) -> DatabaseValue {
    match value {
        SqlValue::Null => DatabaseValue::Null,
        SqlValue::Boolean(value) => DatabaseValue::Boolean(value),
        SqlValue::Int32(value) => DatabaseValue::Int32(value),
        SqlValue::Int64(value) => DatabaseValue::Int64(value),
        SqlValue::Float64(value) => DatabaseValue::Float64(value),
        SqlValue::Text(value) => DatabaseValue::Text(value),
        SqlValue::Bytes(value) => DatabaseValue::Bytes(value),
        SqlValue::Numeric(value) => DatabaseValue::Numeric(value),
        SqlValue::Timestamptz(value) => DatabaseValue::Timestamptz(value),
        SqlValue::Json(value) => DatabaseValue::Json(value),
        SqlValue::Uuid(value) => DatabaseValue::Uuid(value),
    }
}

pub(super) fn into_guest_value(value: DatabaseValue) -> SqlValue {
    match value {
        DatabaseValue::Null => SqlValue::Null,
        DatabaseValue::Boolean(value) => SqlValue::Boolean(value),
        DatabaseValue::Int32(value) => SqlValue::Int32(value),
        DatabaseValue::Int64(value) => SqlValue::Int64(value),
        DatabaseValue::Float64(value) => SqlValue::Float64(value),
        DatabaseValue::Text(value) => SqlValue::Text(value),
        DatabaseValue::Bytes(value) => SqlValue::Bytes(value),
        DatabaseValue::Numeric(value) => SqlValue::Numeric(value),
        DatabaseValue::Timestamptz(value) => SqlValue::Timestamptz(value),
        DatabaseValue::Json(value) => SqlValue::Json(value),
        DatabaseValue::Uuid(value) => SqlValue::Uuid(value),
    }
}

pub(super) fn into_guest_error(value: SqlError) -> PgError {
    match value {
        SqlError::SerializationFailure => PgError::SerializationFailure,
        SqlError::ConnectionUnavailable => PgError::ConnectionUnavailable,
        SqlError::StatementTimeout => PgError::StatementTimeout,
        SqlError::PermissionDenied => PgError::PermissionDenied,
        SqlError::UniqueViolation(value) => PgError::UniqueViolation(value),
        SqlError::ForeignKeyViolation(value) => PgError::ForeignKeyViolation(value),
        SqlError::CheckViolation(value) => PgError::CheckViolation(value),
        SqlError::ExclusionViolation(value) => PgError::ExclusionViolation(value),
        SqlError::QueryError(value) => PgError::QueryError(value),
    }
}
