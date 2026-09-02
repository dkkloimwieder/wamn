//! Thin typed access to host-admitted, content-addressed PostgreSQL statements.

#[expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "postgres-statements",
        path: "wit",
        generate_all,
    });
}

use std::error::Error;
use std::fmt;
use std::pin::Pin;

use bindings::wamn::postgres::statements as host;
use bindings::wamn::postgres::types as wire;

pub use bindings::wamn::postgres::types::{RowSet, SqlValue};

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

/// Stable classification of a statement-capability failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatementErrorKind {
    UnknownStatement,
    StatementContractMismatch,
    SerializationFailure,
    ConnectionUnavailable,
    StatementTimeout,
    RowLimitExceeded,
    UniqueViolation,
    ForeignKeyViolation,
    CheckViolation,
    PermissionDenied,
    QueryError,
    InvalidResult,
}

/// Side of a statement contract that did not match.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContractPart {
    Binds,
    Columns,
}

/// Count and ordered WIT type names observed at one contract boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValueShape {
    pub count: u32,
    pub types: Vec<String>,
}

/// Structured host report for a statement-contract mismatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractMismatch {
    pub statement_digest: Box<str>,
    pub part: ContractPart,
    pub expected: ValueShape,
    pub observed: ValueShape,
}

/// Contextual failure returned by the statement capability or row decoder.
#[derive(Debug)]
pub struct StatementError {
    kind: StatementErrorKind,
    statement_digest: Option<Box<str>>,
    constraint: Option<Box<str>>,
    contract_mismatch: Option<ContractMismatch>,
    context: Box<str>,
}

impl StatementError {
    /// Stable failure class; callers must not match display text.
    pub const fn kind(&self) -> StatementErrorKind {
        self.kind
    }

    /// Statement identity supplied by the host or local decoder, when available.
    pub fn statement_digest(&self) -> Option<&str> {
        self.statement_digest.as_deref()
    }

    /// Named PostgreSQL constraint for typed violation cases.
    pub fn constraint(&self) -> Option<&str> {
        self.constraint.as_deref()
    }

    /// Structured request/result shape mismatch reported by the host.
    pub const fn contract_mismatch(&self) -> Option<&ContractMismatch> {
        self.contract_mismatch.as_ref()
    }

    fn invalid_result(statement_digest: &str, context: impl Into<Box<str>>) -> Self {
        Self {
            kind: StatementErrorKind::InvalidResult,
            statement_digest: Some(statement_digest.into()),
            constraint: None,
            contract_mismatch: None,
            context: context.into(),
        }
    }

    fn from_wire(source: host::StatementError) -> Self {
        match source {
            host::StatementError::UnknownStatement(statement_digest) => Self {
                kind: StatementErrorKind::UnknownStatement,
                statement_digest: Some(statement_digest.into()),
                constraint: None,
                contract_mismatch: None,
                context: "host does not admit the statement digest".into(),
            },
            host::StatementError::StatementContractMismatch(mismatch) => {
                let mismatch = ContractMismatch {
                    statement_digest: mismatch.statement_digest.into(),
                    part: match mismatch.part {
                        host::ContractPart::Binds => ContractPart::Binds,
                        host::ContractPart::Columns => ContractPart::Columns,
                    },
                    expected: ValueShape {
                        count: mismatch.expected.count,
                        types: mismatch.expected.types,
                    },
                    observed: ValueShape {
                        count: mismatch.observed.count,
                        types: mismatch.observed.types,
                    },
                };
                Self {
                    kind: StatementErrorKind::StatementContractMismatch,
                    statement_digest: Some(mismatch.statement_digest.clone()),
                    constraint: None,
                    contract_mismatch: Some(mismatch),
                    context: "statement values do not match the admitted contract".into(),
                }
            }
            host::StatementError::Postgres(error) => Self::from_postgres(error),
        }
    }

    fn from_postgres(source: wire::PgError) -> Self {
        let (kind, constraint, context) = match source {
            wire::PgError::SerializationFailure => (
                StatementErrorKind::SerializationFailure,
                None,
                "serialization failure",
            ),
            wire::PgError::ConnectionUnavailable => (
                StatementErrorKind::ConnectionUnavailable,
                None,
                "database connection unavailable",
            ),
            wire::PgError::StatementTimeout => (
                StatementErrorKind::StatementTimeout,
                None,
                "statement timeout",
            ),
            wire::PgError::RowLimitExceeded(_) => (
                StatementErrorKind::RowLimitExceeded,
                None,
                "statement row limit exceeded",
            ),
            wire::PgError::UniqueViolation(name) => (
                StatementErrorKind::UniqueViolation,
                Some(name.into_boxed_str()),
                "unique constraint violation",
            ),
            wire::PgError::ForeignKeyViolation(name) => (
                StatementErrorKind::ForeignKeyViolation,
                Some(name.into_boxed_str()),
                "foreign-key constraint violation",
            ),
            wire::PgError::CheckViolation(name) => (
                StatementErrorKind::CheckViolation,
                Some(name.into_boxed_str()),
                "check constraint violation",
            ),
            wire::PgError::PermissionDenied => (
                StatementErrorKind::PermissionDenied,
                None,
                "database permission denied",
            ),
            wire::PgError::QueryError(_) => (
                StatementErrorKind::QueryError,
                None,
                "database query failed",
            ),
        };
        Self {
            kind,
            statement_digest: None,
            constraint,
            contract_mismatch: None,
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

/// Ambient connection to host-admitted statements.
#[derive(Debug, Default)]
pub struct Connection;

impl Connection {
    /// Open the ambient capability; no socket or credential is accepted.
    pub const fn new() -> Self {
        Self
    }

    /// Run one admitted statement in an implicit transaction.
    pub async fn run(
        &mut self,
        statement_digest: &str,
        params: Vec<SqlValue>,
    ) -> Result<RowSet, StatementError> {
        host::run(statement_digest, &params).map_err(StatementError::from_wire)
    }

    /// Begin one explicit host-owned transaction.
    pub async fn begin(&mut self) -> Result<Transaction, StatementError> {
        host::begin()
            .map(|inner| Transaction { inner })
            .map_err(StatementError::from_wire)
    }
}

/// Host-owned transaction. Dropping it without commit rolls it back.
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
    /// Run one admitted statement in this transaction.
    pub async fn run(
        &mut self,
        statement_digest: &str,
        params: Vec<SqlValue>,
    ) -> Result<RowSet, StatementError> {
        self.inner
            .run(statement_digest, &params)
            .map_err(StatementError::from_wire)
    }

    /// Commit this transaction.
    pub async fn commit(self) -> Result<(), StatementError> {
        self.inner.commit().map_err(StatementError::from_wire)
    }

    /// Roll back this transaction.
    pub async fn rollback(self) -> Result<(), StatementError> {
        self.inner.rollback().map_err(StatementError::from_wire)
    }
}

/// Run one callback in exactly one transaction, with no automatic retry.
pub async fn run_transaction<T, E, F>(connection: &mut Connection, callback: F) -> Result<T, E>
where
    F: for<'a> FnOnce(&'a mut Transaction) -> Pin<Box<dyn Future<Output = Result<T, E>> + 'a>>,
    E: From<StatementError>,
{
    let mut transaction = connection.begin().await.map_err(E::from)?;
    let output = callback(&mut transaction).await?;
    transaction.commit().await.map_err(E::from)?;
    Ok(output)
}

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
    statement_digest: &'a str,
    columns: std::slice::Iter<'a, wire::Column>,
    values: std::vec::IntoIter<wire::SqlValue>,
}

impl fmt::Debug for RowDecoder<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RowDecoder")
            .field("statement_digest", &self.statement_digest)
            .field("remaining_columns", &self.columns.len())
            .field("remaining_values", &self.values.len())
            .finish()
    }
}

impl<'a> RowDecoder<'a> {
    fn new(
        statement_digest: &'a str,
        columns: &'a [wire::Column],
        values: Vec<wire::SqlValue>,
    ) -> Self {
        Self {
            statement_digest,
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
                self.statement_digest,
                format!("result omitted column `{expected_name}`"),
            )
        })?;
        if column.name != expected_name {
            return Err(StatementError::invalid_result(
                self.statement_digest,
                format!(
                    "result column `{}` does not match expected `{expected_name}`",
                    column.name
                ),
            ));
        }
        let value = self.values.next().ok_or_else(|| {
            StatementError::invalid_result(
                self.statement_digest,
                format!("result omitted value for `{expected_name}`"),
            )
        })?;
        T::from_sql_value(value).ok_or_else(|| {
            StatementError::invalid_result(
                self.statement_digest,
                format!("result value for `{expected_name}` has the wrong type"),
            )
        })
    }

    fn finish(&self) -> Result<(), StatementError> {
        if self.columns.len() == 0 && self.values.len() == 0 {
            Ok(())
        } else {
            Err(StatementError::invalid_result(
                self.statement_digest,
                "result contains undeclared columns or values",
            ))
        }
    }
}

/// Decode exactly one returned row.
pub fn decode_one<T>(
    statement_digest: &str,
    rows: RowSet,
    decode: impl FnOnce(&mut RowDecoder<'_>) -> Result<T, StatementError>,
) -> Result<T, StatementError> {
    let [values] =
        <Vec<Vec<wire::SqlValue>> as TryInto<[Vec<wire::SqlValue>; 1]>>::try_into(rows.rows)
            .map_err(|rows| {
                StatementError::invalid_result(
                    statement_digest,
                    format!("expected one row, received {}", rows.len()),
                )
            })?;
    decode_row(statement_digest, &rows.columns, values, decode)
}

/// Decode zero or one returned row.
pub fn decode_optional<T>(
    statement_digest: &str,
    rows: RowSet,
    decode: impl FnOnce(&mut RowDecoder<'_>) -> Result<T, StatementError>,
) -> Result<Option<T>, StatementError> {
    let mut values = rows.rows.into_iter();
    let Some(row) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(StatementError::invalid_result(
            statement_digest,
            "expected at most one row",
        ));
    }
    decode_row(statement_digest, &rows.columns, row, decode).map(Some)
}

/// Decode every returned row in order.
pub fn decode_all<T>(
    statement_digest: &str,
    rows: RowSet,
    mut decode: impl FnMut(&mut RowDecoder<'_>) -> Result<T, StatementError>,
) -> Result<Vec<T>, StatementError> {
    rows.rows
        .into_iter()
        .map(|values| decode_row(statement_digest, &rows.columns, values, &mut decode))
        .collect()
}

fn decode_row<T>(
    statement_digest: &str,
    columns: &[wire::Column],
    values: Vec<wire::SqlValue>,
    decode: impl FnOnce(&mut RowDecoder<'_>) -> Result<T, StatementError>,
) -> Result<T, StatementError> {
    let mut row = RowDecoder::new(statement_digest, columns, values);
    let decoded = decode(&mut row)?;
    row.finish()?;
    Ok(decoded)
}
