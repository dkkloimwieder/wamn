//! SQL in the host operation transaction and document numbering in the guest.

#[expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "wamn:postgres-statements/postgres-statements@0.3.0",
        path: [
            "../../wit/wamn-postgres-0.4",
            "wit",
        ],
        generate_all,
    });
}

use std::error::Error;
use std::fmt;

use bindings::wamn::postgres::statements as host;
use bindings::wamn::postgres::types as wire;
use wit_bindgen::rt::async_support::{FutureReader, StreamReader, StreamResult};

pub use bindings::wamn::postgres::types::SqlValue;

/// Rows collected by a typed accessor after SQL completion succeeds.
#[derive(Debug)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
}

/// Exact decimal text mapped to the WIT `numeric` case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Numeric(pub String);

/// RFC 3339 text mapped to the WIT `timestamptz` case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimestampTz(pub String);

/// JSON document text mapped to the WIT `json` case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Json(pub String);

/// Canonical UUID text mapped to the WIT `uuid` case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Uuid(pub String);

/// Stable classification of a statement interface failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatementErrorType {
    SerializationFailure,
    ConnectionUnavailable,
    StatementTimeout,
    UniqueViolation,
    ForeignKeyViolation,
    CheckViolation,
    ExclusionViolation,
    PermissionDenied,
    QueryError,
    InvalidResult,
}

/// Contextual failure returned by the statement interface or row decoder.
#[derive(Debug)]
pub struct StatementError {
    type_: StatementErrorType,
    statement: Option<Box<str>>,
    constraint: Option<Box<str>>,
    context: Box<str>,
}

impl StatementError {
    /// Stable failure class; callers must not match display text.
    pub const fn type_(&self) -> StatementErrorType {
        self.type_
    }

    /// Statement identity supplied by the host or local decoder, when available.
    pub fn statement(&self) -> Option<&str> {
        self.statement.as_deref()
    }

    /// Named PostgreSQL constraint for typed violation cases.
    pub fn constraint(&self) -> Option<&str> {
        self.constraint.as_deref()
    }

    fn invalid_result(statement: &str, context: impl Into<Box<str>>) -> Self {
        Self {
            type_: StatementErrorType::InvalidResult,
            statement: Some(statement.into()),
            constraint: None,
            context: context.into(),
        }
    }

    fn from_postgres(source: wire::PgError) -> Self {
        let (type_, constraint, context) = match source {
            wire::PgError::SerializationFailure => (
                StatementErrorType::SerializationFailure,
                None,
                "serialization failure",
            ),
            wire::PgError::ConnectionUnavailable => (
                StatementErrorType::ConnectionUnavailable,
                None,
                "database connection unavailable",
            ),
            wire::PgError::StatementTimeout => (
                StatementErrorType::StatementTimeout,
                None,
                "statement timeout",
            ),
            wire::PgError::UniqueViolation(name) => (
                StatementErrorType::UniqueViolation,
                Some(name.into_boxed_str()),
                "unique constraint violation",
            ),
            wire::PgError::ForeignKeyViolation(name) => (
                StatementErrorType::ForeignKeyViolation,
                Some(name.into_boxed_str()),
                "foreign-key constraint violation",
            ),
            wire::PgError::CheckViolation(name) => (
                StatementErrorType::CheckViolation,
                Some(name.into_boxed_str()),
                "check constraint violation",
            ),
            wire::PgError::ExclusionViolation(name) => (
                StatementErrorType::ExclusionViolation,
                Some(name.into_boxed_str()),
                "exclusion constraint violation",
            ),
            wire::PgError::PermissionDenied => (
                StatementErrorType::PermissionDenied,
                None,
                "database permission denied",
            ),
            wire::PgError::QueryError((state, message)) => {
                return Self {
                    type_: StatementErrorType::QueryError,
                    statement: None,
                    constraint: None,
                    context: format!("database query failed ({state}): {message}").into(),
                };
            }
        };
        Self {
            type_,
            statement: None,
            constraint,
            context: context.into(),
        }
    }
}

impl fmt::Display for StatementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.context)
    }
}

impl Error for StatementError {}

/// Access to the current host operation transaction.
#[derive(Debug, Default)]
pub struct Connection;
impl Connection {
    pub const fn new() -> Self {
        Self
    }

    /// Run SQL in the transaction owned by the current operation.
    pub async fn query(
        &mut self,
        sql: &str,
        params: Vec<SqlValue>,
    ) -> Result<Rows, StatementError> {
        operation_transaction().await?.query(sql, params).await
    }
}

/// A borrowed operation transaction. The host owns its completion.
pub struct Transaction {
    inner: host::Transaction,
}
impl fmt::Debug for Transaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Transaction")
            .finish_non_exhaustive()
    }
}
impl Transaction {
    /// Execute SQL and collect rows for a typed accessor.
    pub async fn query(
        &mut self,
        sql: &str,
        params: Vec<SqlValue>,
    ) -> Result<Rows, StatementError> {
        let (columns, rows, end) = self
            .inner
            .query(sql.to_owned(), params)
            .await
            .map_err(StatementError::from_postgres)?;
        collect(columns, rows, end).await
    }

    /// Execute SQL without parameters in the host transaction.
    pub async fn query_batch(&mut self, sql: &str) -> Result<(), StatementError> {
        self.inner
            .query_batch(sql.to_owned())
            .await
            .map_err(StatementError::from_postgres)
    }
}

/// Borrow the transaction of the current operation.
pub async fn operation_transaction() -> Result<Transaction, StatementError> {
    host::operation_transaction()
        .await
        .map(|inner| Transaction { inner })
        .map_err(StatementError::from_postgres)
}

async fn collect(
    columns: Vec<String>,
    mut rows: StreamReader<Vec<SqlValue>>,
    end: FutureReader<Result<(), wire::PgError>>,
) -> Result<Rows, StatementError> {
    let mut values = Vec::new();
    loop {
        let (status, batch) = rows.read(Vec::with_capacity(500)).await;
        values.extend(batch);
        match status {
            StreamResult::Complete(_) => {}
            StreamResult::Dropped | StreamResult::Cancelled => break,
        }
    }
    end.await.map_err(StatementError::from_postgres)?;
    Ok(Rows {
        columns,
        rows: values,
    })
}

mod numbering;
pub use numbering::{next_number, peek_number};

/// Convert one generated accessor argument into the frozen WIT value vocabulary.
pub fn into_sql_value(value: impl IntoSqlValue) -> SqlValue {
    value.into_sql_value()
}

/// Conversion owned by generated statement accessors.
pub trait IntoSqlValue {
    fn into_sql_value(self) -> SqlValue;
}

macro_rules! scalar_value {
    ($rust:ty, $variant:ident) => {
        impl IntoSqlValue for $rust {
            fn into_sql_value(self) -> wire::SqlValue {
                wire::SqlValue::$variant(self)
            }
        }

        impl FromSqlValue for $rust {
            fn from_sql_value(value: wire::SqlValue) -> Option<Self> {
                match value {
                    wire::SqlValue::$variant(value) => Some(value),
                    _ => None,
                }
            }
        }
    };
}

scalar_value!(bool, Boolean);
scalar_value!(i32, Int32);
scalar_value!(i64, Int64);
scalar_value!(f64, Float64);
scalar_value!(String, Text);
scalar_value!(Vec<u8>, Bytes);

macro_rules! text_value {
    ($rust:ty, $variant:ident) => {
        impl IntoSqlValue for $rust {
            fn into_sql_value(self) -> wire::SqlValue {
                wire::SqlValue::$variant(self.0)
            }
        }

        impl FromSqlValue for $rust {
            fn from_sql_value(value: wire::SqlValue) -> Option<Self> {
                match value {
                    wire::SqlValue::$variant(value) => Some(Self(value)),
                    _ => None,
                }
            }
        }
    };
}

text_value!(Numeric, Numeric);
text_value!(TimestampTz, Timestamptz);
text_value!(Json, Json);
text_value!(Uuid, Uuid);

impl<T> IntoSqlValue for Option<T>
where
    T: IntoSqlValue,
{
    fn into_sql_value(self) -> wire::SqlValue {
        self.map_or(wire::SqlValue::Null, IntoSqlValue::into_sql_value)
    }
}

/// Decode one value from the frozen WIT vocabulary.
pub trait FromSqlValue: Sized {
    fn from_sql_value(value: SqlValue) -> Option<Self>;
}

impl<T> FromSqlValue for Option<T>
where
    T: FromSqlValue,
{
    fn from_sql_value(value: wire::SqlValue) -> Option<Self> {
        match value {
            wire::SqlValue::Null => Some(None),
            value => T::from_sql_value(value).map(Some),
        }
    }
}

/// Positional decoder that verifies generated column names and value types.
pub struct RowDecoder<'a> {
    statement: &'a str,
    columns: std::slice::Iter<'a, String>,
    values: std::vec::IntoIter<wire::SqlValue>,
}

impl fmt::Debug for RowDecoder<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RowDecoder")
            .field("statement", &self.statement)
            .field("remaining_columns", &self.columns.len())
            .field("remaining_values", &self.values.len())
            .finish()
    }
}

impl<'a> RowDecoder<'a> {
    fn new(statement: &'a str, columns: &'a [String], values: Vec<wire::SqlValue>) -> Self {
        Self {
            statement,
            columns: columns.iter(),
            values: values.into_iter(),
        }
    }

    /// Decode the next positional value under its generated column name.
    pub fn decode<T>(&mut self, expected_name: &str) -> Result<T, StatementError>
    where
        T: FromSqlValue,
    {
        let column = self.columns.next().ok_or_else(|| {
            StatementError::invalid_result(
                self.statement,
                format!("result omitted column `{expected_name}`"),
            )
        })?;
        if column != expected_name && column.strip_suffix(['!', '?']) != Some(expected_name) {
            return Err(StatementError::invalid_result(
                self.statement,
                format!("result column `{column}` does not match expected `{expected_name}`"),
            ));
        }
        let value = self.values.next().ok_or_else(|| {
            StatementError::invalid_result(
                self.statement,
                format!("result omitted value for `{expected_name}`"),
            )
        })?;
        T::from_sql_value(value).ok_or_else(|| {
            StatementError::invalid_result(
                self.statement,
                format!("result value for `{expected_name}` has the wrong type"),
            )
        })
    }

    fn finish(&self) -> Result<(), StatementError> {
        if self.columns.len() == 0 && self.values.len() == 0 {
            Ok(())
        } else {
            Err(StatementError::invalid_result(
                self.statement,
                "result contains undeclared columns or values",
            ))
        }
    }
}

/// Decode exactly one returned row.
pub fn decode_one<T>(
    statement: &str,
    rows: Rows,
    decode: impl FnOnce(&mut RowDecoder<'_>) -> Result<T, StatementError>,
) -> Result<T, StatementError> {
    let [values] =
        <Vec<Vec<wire::SqlValue>> as TryInto<[Vec<wire::SqlValue>; 1]>>::try_into(rows.rows)
            .map_err(|rows| {
                StatementError::invalid_result(
                    statement,
                    format!("expected one row, received {}", rows.len()),
                )
            })?;
    decode_row(statement, &rows.columns, values, decode)
}

/// Decode zero or one returned row.
pub fn decode_optional<T>(
    statement: &str,
    rows: Rows,
    decode: impl FnOnce(&mut RowDecoder<'_>) -> Result<T, StatementError>,
) -> Result<Option<T>, StatementError> {
    let mut values = rows.rows.into_iter();
    let Some(row) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(StatementError::invalid_result(
            statement,
            "expected at most one row",
        ));
    }
    decode_row(statement, &rows.columns, row, decode).map(Some)
}

/// Decode every returned row in order.
pub fn decode_all<T>(
    statement: &str,
    rows: Rows,
    mut decode: impl FnMut(&mut RowDecoder<'_>) -> Result<T, StatementError>,
) -> Result<Vec<T>, StatementError> {
    rows.rows
        .into_iter()
        .map(|values| decode_row(statement, &rows.columns, values, &mut decode))
        .collect()
}

fn decode_row<T>(
    statement: &str,
    columns: &[String],
    values: Vec<wire::SqlValue>,
    decode: impl FnOnce(&mut RowDecoder<'_>) -> Result<T, StatementError>,
) -> Result<T, StatementError> {
    let mut row = RowDecoder::new(statement, columns, values);
    let decoded = decode(&mut row)?;
    row.finish()?;
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(name: &str, count: usize) -> Rows {
        Rows {
            columns: vec![name.into()],
            rows: vec![vec![SqlValue::Int32(42)]; count],
        }
    }

    fn decode_id(row: &mut RowDecoder<'_>) -> Result<i32, StatementError> {
        row.decode("id")
    }

    #[test]
    fn decode_one_refuses_zero_or_two_rows() {
        for count in [0, 2] {
            let error = decode_one("read-id", rows("id", count), decode_id).unwrap_err();
            assert_eq!(error.type_(), StatementErrorType::InvalidResult);
        }
    }

    #[test]
    fn decoders_refuse_a_column_name_mismatch() {
        let error = decode_one("read-id", rows("other", 1), decode_id).unwrap_err();
        assert_eq!(error.type_(), StatementErrorType::InvalidResult);
        let error = decode_all("read-id", rows("other", 2), decode_id).unwrap_err();
        assert_eq!(error.type_(), StatementErrorType::InvalidResult);
    }

    #[test]
    fn postgres_wire_errors_keep_their_type_and_constraint() {
        for (wire, expected, constraint) in [
            (
                wire::PgError::SerializationFailure,
                StatementErrorType::SerializationFailure,
                None,
            ),
            (
                wire::PgError::ConnectionUnavailable,
                StatementErrorType::ConnectionUnavailable,
                None,
            ),
            (
                wire::PgError::StatementTimeout,
                StatementErrorType::StatementTimeout,
                None,
            ),
            (
                wire::PgError::UniqueViolation("unique".into()),
                StatementErrorType::UniqueViolation,
                Some("unique"),
            ),
            (
                wire::PgError::ForeignKeyViolation("foreign".into()),
                StatementErrorType::ForeignKeyViolation,
                Some("foreign"),
            ),
            (
                wire::PgError::CheckViolation("check".into()),
                StatementErrorType::CheckViolation,
                Some("check"),
            ),
            (
                wire::PgError::ExclusionViolation("exclusion".into()),
                StatementErrorType::ExclusionViolation,
                Some("exclusion"),
            ),
            (
                wire::PgError::PermissionDenied,
                StatementErrorType::PermissionDenied,
                None,
            ),
            (
                wire::PgError::QueryError(("42P01".into(), "missing relation".into())),
                StatementErrorType::QueryError,
                None,
            ),
        ] {
            let error = StatementError::from_postgres(wire);
            assert_eq!(error.type_(), expected);
            assert_eq!(error.constraint().is_some(), constraint.is_some());
        }
    }
}
