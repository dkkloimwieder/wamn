//! SQLx 0.9 driver over the synchronous `wamn:postgres` capability.

#[expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "postgres-sqlx",
        path: "wit",
        generate_all,
    });
}

use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use futures_core::future::BoxFuture;
use futures_core::stream::BoxStream;
use futures_util::future::FutureExt as _;
use futures_util::stream::{self, StreamExt as _};
use sqlx_core::Either;
use sqlx_core::arguments::{Arguments, IntoArguments};
use sqlx_core::column::{Column, ColumnIndex};
use sqlx_core::connection::{ConnectOptions, Connection, LogSettings};
use sqlx_core::database::Database;
use sqlx_core::decode::Decode;
use sqlx_core::encode::{Encode, IsNull};
use sqlx_core::error::{BoxDynError, DatabaseError, Error, ErrorKind, UnexpectedNullError};
use sqlx_core::executor::{Execute, Executor};
use sqlx_core::row::Row;
use sqlx_core::sql_str::SqlStr;
use sqlx_core::statement::Statement;
use sqlx_core::transaction::{Transaction, TransactionManager};
use sqlx_core::type_info::TypeInfo;
use sqlx_core::types::Type;
use sqlx_core::value::{Value, ValueRef};

use bindings::wamn::postgres::client as host;
use bindings::wamn::postgres::types as wire;

/// SQLx database identity for the `wamn:postgres` capability.
#[derive(Debug)]
pub struct WamnPostgres;

impl Database for WamnPostgres {
    type Connection = WamnConnection;
    type TransactionManager = WamnTransactionManager;
    type Row = WamnRow;
    type QueryResult = WamnQueryResult;
    type Column = WamnColumn;
    type TypeInfo = WamnTypeInfo;
    type Value = WamnValue;
    type ValueRef<'r> = WamnValueRef<'r>;
    type Arguments = WamnArguments;
    type ArgumentBuffer = Vec<WamnValue>;
    type Statement = WamnStatement;

    const NAME: &'static str = "Wamn Postgres capability";
    const URL_SCHEMES: &'static [&'static str] = &["wamn-postgres"];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ValueKind {
    Null,
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

/// Type metadata carried by a `wamn:postgres` result column.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WamnTypeInfo {
    kind: ValueKind,
    name: &'static str,
}

impl WamnTypeInfo {
    const NULL: Self = Self::new(ValueKind::Null, "NULL");
    const BOOLEAN: Self = Self::new(ValueKind::Boolean, "BOOLEAN");
    const INT32: Self = Self::new(ValueKind::Int32, "INT4");
    const INT64: Self = Self::new(ValueKind::Int64, "INT8");
    const FLOAT64: Self = Self::new(ValueKind::Float64, "FLOAT8");
    const TEXT: Self = Self::new(ValueKind::Text, "TEXT");
    const BYTES: Self = Self::new(ValueKind::Bytes, "BYTEA");
    const NUMERIC: Self = Self::new(ValueKind::Numeric, "NUMERIC");
    const TIMESTAMPTZ: Self = Self::new(ValueKind::Timestamptz, "TIMESTAMPTZ");
    const JSON: Self = Self::new(ValueKind::Json, "JSONB");
    const UUID: Self = Self::new(ValueKind::Uuid, "UUID");

    const fn new(kind: ValueKind, name: &'static str) -> Self {
        Self { kind, name }
    }

    fn from_postgres_name(name: &str) -> Result<Self, Error> {
        match name.to_ascii_lowercase().as_str() {
            "bool" | "boolean" => Ok(Self::BOOLEAN),
            "int4" | "integer" => Ok(Self::INT32),
            "int8" | "bigint" => Ok(Self::INT64),
            "float8" | "double precision" => Ok(Self::FLOAT64),
            "text" | "varchar" | "character varying" | "bpchar" | "character" | "name" => {
                Ok(Self::TEXT)
            }
            "bytea" => Ok(Self::BYTES),
            "numeric" | "decimal" => Ok(Self::NUMERIC),
            "timestamptz" | "timestamp with time zone" => Ok(Self::TIMESTAMPTZ),
            "json" | "jsonb" => Ok(Self::JSON),
            "uuid" => Ok(Self::UUID),
            _ => Err(Error::Protocol(format!(
                "wamn:postgres returned unsupported column type `{name}`"
            ))),
        }
    }
}

impl Display for WamnTypeInfo {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name)
    }
}

impl TypeInfo for WamnTypeInfo {
    fn is_null(&self) -> bool {
        self.kind == ValueKind::Null
    }

    fn name(&self) -> &str {
        self.name
    }

    fn type_compatible(&self, other: &Self) -> bool {
        self.is_null() || other.is_null() || self.kind == other.kind
    }
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

/// Owned value covering every case in frozen `wamn:postgres/types.sql-value`.
#[derive(Clone, Debug, PartialEq)]
pub enum WamnValue {
    Null,
    Boolean(bool),
    Int32(i32),
    Int64(i64),
    Float64(f64),
    Text(String),
    Bytes(Vec<u8>),
    Numeric(String),
    Timestamptz(String),
    Json(String),
    Uuid(String),
}

impl WamnValue {
    fn type_info_value(&self) -> WamnTypeInfo {
        match self {
            Self::Null => WamnTypeInfo::NULL,
            Self::Boolean(_) => WamnTypeInfo::BOOLEAN,
            Self::Int32(_) => WamnTypeInfo::INT32,
            Self::Int64(_) => WamnTypeInfo::INT64,
            Self::Float64(_) => WamnTypeInfo::FLOAT64,
            Self::Text(_) => WamnTypeInfo::TEXT,
            Self::Bytes(_) => WamnTypeInfo::BYTES,
            Self::Numeric(_) => WamnTypeInfo::NUMERIC,
            Self::Timestamptz(_) => WamnTypeInfo::TIMESTAMPTZ,
            Self::Json(_) => WamnTypeInfo::JSON,
            Self::Uuid(_) => WamnTypeInfo::UUID,
        }
    }

    fn into_wire(self) -> wire::SqlValue {
        match self {
            Self::Null => wire::SqlValue::Null,
            Self::Boolean(value) => wire::SqlValue::Boolean(value),
            Self::Int32(value) => wire::SqlValue::Int32(value),
            Self::Int64(value) => wire::SqlValue::Int64(value),
            Self::Float64(value) => wire::SqlValue::Float64(value),
            Self::Text(value) => wire::SqlValue::Text(value),
            Self::Bytes(value) => wire::SqlValue::Bytes(value),
            Self::Numeric(value) => wire::SqlValue::Numeric(value),
            Self::Timestamptz(value) => wire::SqlValue::Timestamptz(value),
            Self::Json(value) => wire::SqlValue::Json(value),
            Self::Uuid(value) => wire::SqlValue::Uuid(value),
        }
    }

    fn from_wire(value: wire::SqlValue) -> Self {
        match value {
            wire::SqlValue::Null => Self::Null,
            wire::SqlValue::Boolean(value) => Self::Boolean(value),
            wire::SqlValue::Int32(value) => Self::Int32(value),
            wire::SqlValue::Int64(value) => Self::Int64(value),
            wire::SqlValue::Float64(value) => Self::Float64(value),
            wire::SqlValue::Text(value) => Self::Text(value),
            wire::SqlValue::Bytes(value) => Self::Bytes(value),
            wire::SqlValue::Numeric(value) => Self::Numeric(value),
            wire::SqlValue::Timestamptz(value) => Self::Timestamptz(value),
            wire::SqlValue::Json(value) => Self::Json(value),
            wire::SqlValue::Uuid(value) => Self::Uuid(value),
        }
    }
}

impl Value for WamnValue {
    type Database = WamnPostgres;

    fn as_ref(&self) -> WamnValueRef<'_> {
        WamnValueRef {
            value: self,
            type_info: self.type_info_value(),
        }
    }

    fn type_info(&self) -> Cow<'_, WamnTypeInfo> {
        Cow::Owned(self.type_info_value())
    }

    fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

/// Borrowed SQLx value reference.
#[derive(Clone, Copy, Debug)]
pub struct WamnValueRef<'r> {
    value: &'r WamnValue,
    type_info: WamnTypeInfo,
}

impl<'r> ValueRef<'r> for WamnValueRef<'r> {
    type Database = WamnPostgres;

    fn to_owned(&self) -> WamnValue {
        self.value.clone()
    }

    fn type_info(&self) -> Cow<'_, WamnTypeInfo> {
        Cow::Owned(self.type_info)
    }

    fn is_null(&self) -> bool {
        matches!(self.value, WamnValue::Null)
    }
}

fn unexpected_value(expected: &str, value: &WamnValue) -> BoxDynError {
    if matches!(value, WamnValue::Null) {
        Box::new(UnexpectedNullError)
    } else {
        format!("expected {expected}, received {value:?}").into()
    }
}

impl Type<WamnPostgres> for WamnValue {
    fn type_info() -> WamnTypeInfo {
        WamnTypeInfo::NULL
    }

    fn compatible(ty: &WamnTypeInfo) -> bool {
        matches!(
            ty.kind,
            ValueKind::Null
                | ValueKind::Boolean
                | ValueKind::Int32
                | ValueKind::Int64
                | ValueKind::Float64
                | ValueKind::Text
                | ValueKind::Bytes
                | ValueKind::Numeric
                | ValueKind::Timestamptz
                | ValueKind::Json
                | ValueKind::Uuid
        )
    }
}

impl Encode<'_, WamnPostgres> for WamnValue {
    fn encode_by_ref(&self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        let is_null = matches!(self, Self::Null);
        buffer.push(self.clone());
        Ok(if is_null { IsNull::Yes } else { IsNull::No })
    }

    fn produces(&self) -> Option<WamnTypeInfo> {
        Some(self.type_info_value())
    }
}

impl<'r> Decode<'r, WamnPostgres> for WamnValue {
    fn decode(value: WamnValueRef<'r>) -> Result<Self, BoxDynError> {
        Ok(value.value.clone())
    }
}

impl Type<WamnPostgres> for bool {
    fn type_info() -> WamnTypeInfo {
        WamnTypeInfo::BOOLEAN
    }
}

impl Encode<'_, WamnPostgres> for bool {
    fn encode_by_ref(&self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        buffer.push(WamnValue::Boolean(*self));
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, WamnPostgres> for bool {
    fn decode(value: WamnValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            WamnValue::Boolean(value) => Ok(*value),
            other => Err(unexpected_value("boolean", other)),
        }
    }
}

impl Type<WamnPostgres> for i32 {
    fn type_info() -> WamnTypeInfo {
        WamnTypeInfo::INT32
    }
}

impl Encode<'_, WamnPostgres> for i32 {
    fn encode_by_ref(&self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        buffer.push(WamnValue::Int32(*self));
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, WamnPostgres> for i32 {
    fn decode(value: WamnValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            WamnValue::Int32(value) => Ok(*value),
            other => Err(unexpected_value("int32", other)),
        }
    }
}

impl Type<WamnPostgres> for i64 {
    fn type_info() -> WamnTypeInfo {
        WamnTypeInfo::INT64
    }
}

impl Encode<'_, WamnPostgres> for i64 {
    fn encode_by_ref(&self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        buffer.push(WamnValue::Int64(*self));
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, WamnPostgres> for i64 {
    fn decode(value: WamnValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            WamnValue::Int64(value) => Ok(*value),
            other => Err(unexpected_value("int64", other)),
        }
    }
}

impl Type<WamnPostgres> for f64 {
    fn type_info() -> WamnTypeInfo {
        WamnTypeInfo::FLOAT64
    }
}

impl Encode<'_, WamnPostgres> for f64 {
    fn encode_by_ref(&self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        buffer.push(WamnValue::Float64(*self));
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, WamnPostgres> for f64 {
    fn decode(value: WamnValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            WamnValue::Float64(value) => Ok(*value),
            other => Err(unexpected_value("float64", other)),
        }
    }
}

impl Type<WamnPostgres> for str {
    fn type_info() -> WamnTypeInfo {
        WamnTypeInfo::TEXT
    }
}

impl Type<WamnPostgres> for String {
    fn type_info() -> WamnTypeInfo {
        WamnTypeInfo::TEXT
    }
}

impl<'q> Encode<'q, WamnPostgres> for &'q str {
    fn encode_by_ref(&self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        buffer.push(WamnValue::Text((*self).to_owned()));
        Ok(IsNull::No)
    }
}

impl Encode<'_, WamnPostgres> for String {
    fn encode(self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        buffer.push(WamnValue::Text(self));
        Ok(IsNull::No)
    }

    fn encode_by_ref(&self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        buffer.push(WamnValue::Text(self.clone()));
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, WamnPostgres> for &'r str {
    fn decode(value: WamnValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            WamnValue::Text(value) => Ok(value),
            other => Err(unexpected_value("text", other)),
        }
    }
}

impl<'r> Decode<'r, WamnPostgres> for String {
    fn decode(value: WamnValueRef<'r>) -> Result<Self, BoxDynError> {
        <&str as Decode<WamnPostgres>>::decode(value).map(ToOwned::to_owned)
    }
}

impl Type<WamnPostgres> for [u8] {
    fn type_info() -> WamnTypeInfo {
        WamnTypeInfo::BYTES
    }
}

impl Type<WamnPostgres> for Vec<u8> {
    fn type_info() -> WamnTypeInfo {
        WamnTypeInfo::BYTES
    }
}

impl<'q> Encode<'q, WamnPostgres> for &'q [u8] {
    fn encode_by_ref(&self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        buffer.push(WamnValue::Bytes((*self).to_vec()));
        Ok(IsNull::No)
    }
}

impl Encode<'_, WamnPostgres> for Vec<u8> {
    fn encode(self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        buffer.push(WamnValue::Bytes(self));
        Ok(IsNull::No)
    }

    fn encode_by_ref(&self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
        buffer.push(WamnValue::Bytes(self.clone()));
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, WamnPostgres> for &'r [u8] {
    fn decode(value: WamnValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.value {
            WamnValue::Bytes(value) => Ok(value),
            other => Err(unexpected_value("bytes", other)),
        }
    }
}

impl<'r> Decode<'r, WamnPostgres> for Vec<u8> {
    fn decode(value: WamnValueRef<'r>) -> Result<Self, BoxDynError> {
        <&[u8] as Decode<WamnPostgres>>::decode(value).map(ToOwned::to_owned)
    }
}

macro_rules! impl_text_case {
    ($type:ty, $variant:ident, $type_info:expr, $expected:literal) => {
        impl Type<WamnPostgres> for $type {
            fn type_info() -> WamnTypeInfo {
                $type_info
            }
        }

        impl<'q> Encode<'q, WamnPostgres> for $type {
            fn encode(self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
                buffer.push(WamnValue::$variant(self.0));
                Ok(IsNull::No)
            }

            fn encode_by_ref(&self, buffer: &mut Vec<WamnValue>) -> Result<IsNull, BoxDynError> {
                buffer.push(WamnValue::$variant(self.0.clone()));
                Ok(IsNull::No)
            }
        }

        impl<'r> Decode<'r, WamnPostgres> for $type {
            fn decode(value: WamnValueRef<'r>) -> Result<Self, BoxDynError> {
                match value.value {
                    WamnValue::$variant(value) => Ok(Self(value.clone())),
                    other => Err(unexpected_value($expected, other)),
                }
            }
        }
    };
}

impl_text_case!(Numeric, Numeric, WamnTypeInfo::NUMERIC, "numeric");
impl_text_case!(
    TimestampTz,
    Timestamptz,
    WamnTypeInfo::TIMESTAMPTZ,
    "timestamptz"
);
impl_text_case!(Json, Json, WamnTypeInfo::JSON, "json");
impl_text_case!(Uuid, Uuid, WamnTypeInfo::UUID, "uuid");

sqlx_core::impl_encode_for_option!(WamnPostgres);

/// Bound argument list translated directly into WIT values.
#[derive(Debug, Default)]
pub struct WamnArguments {
    values: Vec<WamnValue>,
}

impl Arguments for WamnArguments {
    type Database = WamnPostgres;

    fn reserve(&mut self, additional: usize, _size: usize) {
        self.values.reserve(additional);
    }

    fn add<'t, T>(&mut self, value: T) -> Result<(), BoxDynError>
    where
        T: Encode<'t, WamnPostgres> + Type<WamnPostgres>,
    {
        let before = self.values.len();
        let is_null = value.encode(&mut self.values)?;
        match (self.values.len() - before, is_null.is_null()) {
            (0, true) => self.values.push(WamnValue::Null),
            (1, _) => {}
            (count, _) => {
                return Err(format!("an encoded argument produced {count} WIT values").into());
            }
        }
        Ok(())
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn format_placeholder<W: fmt::Write>(&self, writer: &mut W) -> fmt::Result {
        write!(writer, "${}", self.len() + 1)
    }
}

impl IntoArguments<WamnPostgres> for WamnArguments {
    fn into_arguments(self) -> Self {
        self
    }
}

/// Result metadata for an executed statement.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WamnQueryResult {
    rows_affected: u64,
}

impl WamnQueryResult {
    pub fn rows_affected(&self) -> u64 {
        self.rows_affected
    }
}

impl Extend<Self> for WamnQueryResult {
    fn extend<T: IntoIterator<Item = Self>>(&mut self, iter: T) {
        self.rows_affected += iter
            .into_iter()
            .map(|result| result.rows_affected)
            .sum::<u64>();
    }
}

/// Result column metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WamnColumn {
    ordinal: usize,
    name: Box<str>,
    type_info: WamnTypeInfo,
}

impl Column for WamnColumn {
    type Database = WamnPostgres;

    fn ordinal(&self) -> usize {
        self.ordinal
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn type_info(&self) -> &WamnTypeInfo {
        &self.type_info
    }
}

/// Validated row returned by the capability.
#[derive(Clone, Debug)]
pub struct WamnRow {
    columns: Vec<WamnColumn>,
    values: Vec<WamnValue>,
}

impl Row for WamnRow {
    type Database = WamnPostgres;

    fn columns(&self) -> &[WamnColumn] {
        &self.columns
    }

    fn try_get_raw<I>(&self, index: I) -> Result<WamnValueRef<'_>, Error>
    where
        I: ColumnIndex<Self>,
    {
        let index = index.index(self)?;
        Ok(WamnValueRef {
            value: &self.values[index],
            type_info: self.columns[index].type_info,
        })
    }
}

sqlx_core::impl_column_index_for_row!(WamnRow);

impl ColumnIndex<WamnRow> for &str {
    fn index(&self, row: &WamnRow) -> Result<usize, Error> {
        row.columns
            .iter()
            .position(|column| column.name() == *self)
            .ok_or_else(|| Error::ColumnNotFound((*self).to_owned()))
    }
}

/// Client-side SQLx statement metadata; preparation remains host-owned.
#[derive(Clone, Debug)]
pub struct WamnStatement {
    sql: SqlStr,
    parameters: Vec<WamnTypeInfo>,
}

impl Statement for WamnStatement {
    type Database = WamnPostgres;

    fn into_sql(self) -> SqlStr {
        self.sql
    }

    fn sql(&self) -> &SqlStr {
        &self.sql
    }

    fn parameters(&self) -> Option<Either<&[WamnTypeInfo], usize>> {
        Some(Either::Left(&self.parameters))
    }

    fn columns(&self) -> &[WamnColumn] {
        &[]
    }

    sqlx_core::impl_statement_query!(WamnArguments);
}

sqlx_core::impl_column_index_for_statement!(WamnStatement);

impl ColumnIndex<WamnStatement> for &str {
    fn index(&self, _statement: &WamnStatement) -> Result<usize, Error> {
        Err(Error::ColumnNotFound((*self).to_owned()))
    }
}

/// Exact WIT error taxonomy exposed through SQLx's database-error seam.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WamnPgError {
    SerializationFailure,
    ConnectionUnavailable,
    StatementTimeout,
    RowLimitExceeded(u64),
    UniqueViolation(String),
    ForeignKeyViolation(String),
    CheckViolation(String),
    PermissionDenied,
    QueryError { sqlstate: String, message: String },
}

/// SQLx database error adapter preserving the frozen WIT error case.
#[derive(Debug)]
pub struct WamnDatabaseError {
    error: WamnPgError,
    message: String,
}

impl WamnDatabaseError {
    pub fn pg_error(&self) -> &WamnPgError {
        &self.error
    }

    fn from_wire(error: wire::PgError) -> Self {
        let error = match error {
            wire::PgError::SerializationFailure => WamnPgError::SerializationFailure,
            wire::PgError::ConnectionUnavailable => WamnPgError::ConnectionUnavailable,
            wire::PgError::StatementTimeout => WamnPgError::StatementTimeout,
            wire::PgError::RowLimitExceeded(limit) => WamnPgError::RowLimitExceeded(limit),
            wire::PgError::UniqueViolation(name) => WamnPgError::UniqueViolation(name),
            wire::PgError::ForeignKeyViolation(name) => WamnPgError::ForeignKeyViolation(name),
            wire::PgError::CheckViolation(name) => WamnPgError::CheckViolation(name),
            wire::PgError::PermissionDenied => WamnPgError::PermissionDenied,
            wire::PgError::QueryError((sqlstate, message)) => {
                WamnPgError::QueryError { sqlstate, message }
            }
        };
        Self::new(error)
    }

    fn new(error: WamnPgError) -> Self {
        let message = match &error {
            WamnPgError::SerializationFailure => "serialization failure or deadlock".to_owned(),
            WamnPgError::ConnectionUnavailable => "connection unavailable".to_owned(),
            WamnPgError::StatementTimeout => "statement timeout".to_owned(),
            WamnPgError::RowLimitExceeded(limit) => format!("row limit exceeded ({limit})"),
            WamnPgError::UniqueViolation(name) => format!("unique violation ({name})"),
            WamnPgError::ForeignKeyViolation(name) => {
                format!("foreign key violation ({name})")
            }
            WamnPgError::CheckViolation(name) => format!("check violation ({name})"),
            WamnPgError::PermissionDenied => "permission denied".to_owned(),
            WamnPgError::QueryError { message, .. } => message.clone(),
        };
        Self { error, message }
    }
}

impl Display for WamnDatabaseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for WamnDatabaseError {}

impl DatabaseError for WamnDatabaseError {
    fn message(&self) -> &str {
        &self.message
    }

    fn code(&self) -> Option<Cow<'_, str>> {
        match &self.error {
            WamnPgError::SerializationFailure => Some(Cow::Borrowed("40001/40P01")),
            WamnPgError::StatementTimeout => Some(Cow::Borrowed("57014")),
            WamnPgError::UniqueViolation(_) => Some(Cow::Borrowed("23505")),
            WamnPgError::ForeignKeyViolation(_) => Some(Cow::Borrowed("23503")),
            WamnPgError::CheckViolation(_) => Some(Cow::Borrowed("23514")),
            WamnPgError::PermissionDenied => Some(Cow::Borrowed("42501")),
            WamnPgError::QueryError { sqlstate, .. } => Some(Cow::Borrowed(sqlstate)),
            WamnPgError::ConnectionUnavailable | WamnPgError::RowLimitExceeded(_) => None,
        }
    }

    fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
        self
    }

    fn is_transient_in_connect_phase(&self) -> bool {
        matches!(self.error, WamnPgError::ConnectionUnavailable)
    }

    fn constraint(&self) -> Option<&str> {
        match &self.error {
            WamnPgError::UniqueViolation(name)
            | WamnPgError::ForeignKeyViolation(name)
            | WamnPgError::CheckViolation(name) => Some(name),
            _ => None,
        }
    }

    fn kind(&self) -> ErrorKind {
        match self.error {
            WamnPgError::UniqueViolation(_) => ErrorKind::UniqueViolation,
            WamnPgError::ForeignKeyViolation(_) => ErrorKind::ForeignKeyViolation,
            WamnPgError::CheckViolation(_) => ErrorKind::CheckViolation,
            _ => ErrorKind::Other,
        }
    }
}

fn database_error(error: wire::PgError) -> Error {
    Error::Database(Box::new(WamnDatabaseError::from_wire(error)))
}

trait TransactionCapability: Send {
    fn query(&mut self, sql: &str, params: Vec<WamnValue>) -> Result<wire::RowSet, wire::PgError>;
    fn execute(&mut self, sql: &str, params: Vec<WamnValue>) -> Result<u64, wire::PgError>;
    fn commit(&mut self) -> Result<(), wire::PgError>;
    fn rollback(&mut self) -> Result<(), wire::PgError>;
}

trait Capability: Send {
    fn query(&mut self, sql: &str, params: Vec<WamnValue>) -> Result<wire::RowSet, wire::PgError>;
    fn execute(&mut self, sql: &str, params: Vec<WamnValue>) -> Result<u64, wire::PgError>;
    fn begin(&mut self) -> Result<Box<dyn TransactionCapability>, wire::PgError>;
}

#[derive(Debug)]
struct HostCapability;

struct HostTransaction(host::Transaction);

impl Capability for HostCapability {
    fn query(&mut self, sql: &str, params: Vec<WamnValue>) -> Result<wire::RowSet, wire::PgError> {
        host::query(
            sql,
            &params
                .into_iter()
                .map(WamnValue::into_wire)
                .collect::<Vec<_>>(),
        )
    }

    fn execute(&mut self, sql: &str, params: Vec<WamnValue>) -> Result<u64, wire::PgError> {
        host::execute(
            sql,
            &params
                .into_iter()
                .map(WamnValue::into_wire)
                .collect::<Vec<_>>(),
        )
    }

    fn begin(&mut self) -> Result<Box<dyn TransactionCapability>, wire::PgError> {
        host::begin().map(|transaction| Box::new(HostTransaction(transaction)) as _)
    }
}

impl TransactionCapability for HostTransaction {
    fn query(&mut self, sql: &str, params: Vec<WamnValue>) -> Result<wire::RowSet, wire::PgError> {
        self.0.query(
            sql,
            &params
                .into_iter()
                .map(WamnValue::into_wire)
                .collect::<Vec<_>>(),
        )
    }

    fn execute(&mut self, sql: &str, params: Vec<WamnValue>) -> Result<u64, wire::PgError> {
        self.0.execute(
            sql,
            &params
                .into_iter()
                .map(WamnValue::into_wire)
                .collect::<Vec<_>>(),
        )
    }

    fn commit(&mut self) -> Result<(), wire::PgError> {
        self.0.commit()
    }

    fn rollback(&mut self) -> Result<(), wire::PgError> {
        self.0.rollback()
    }
}

/// SQLx connection backed only by the ambient `wamn:postgres` capability.
pub struct WamnConnection {
    capability: Box<dyn Capability>,
    transaction: Option<Box<dyn TransactionCapability>>,
}

impl fmt::Debug for WamnConnection {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WamnConnection")
            .field("in_transaction", &self.transaction.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for WamnConnection {
    fn default() -> Self {
        Self::new()
    }
}

impl WamnConnection {
    /// Opens the ambient capability; no socket or credential is accepted.
    pub fn new() -> Self {
        Self {
            capability: Box::new(HostCapability),
            transaction: None,
        }
    }

    fn query(&mut self, sql: &str, arguments: WamnArguments) -> Result<Vec<WamnRow>, Error> {
        let result = match self.transaction.as_mut() {
            Some(transaction) => transaction.query(sql, arguments.values),
            None => self.capability.query(sql, arguments.values),
        }
        .map_err(database_error)?;
        rows_from_wire(result)
    }

    fn execute_statement(
        &mut self,
        sql: &str,
        arguments: WamnArguments,
    ) -> Result<WamnQueryResult, Error> {
        let rows_affected = match self.transaction.as_mut() {
            Some(transaction) => transaction.execute(sql, arguments.values),
            None => self.capability.execute(sql, arguments.values),
        }
        .map_err(database_error)?;
        Ok(WamnQueryResult { rows_affected })
    }
}

/// Capability-only connection options; the sole accepted URL is `wamn-postgres:`.
#[derive(Clone, Debug, Default)]
pub struct WamnConnectOptions {
    log_settings: LogSettings,
}

impl FromStr for WamnConnectOptions {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "wamn-postgres:" {
            Ok(Self::default())
        } else {
            Err(Error::Configuration(
                "expected capability URL `wamn-postgres:`; sockets and credentials are not accepted"
                    .into(),
            ))
        }
    }
}

impl ConnectOptions for WamnConnectOptions {
    type Connection = WamnConnection;

    fn from_url(url: &sqlx_core::Url) -> Result<Self, Error> {
        url.as_str().parse()
    }

    fn connect(&self) -> impl Future<Output = Result<WamnConnection, Error>> + Send + '_ {
        std::future::ready(Ok(WamnConnection::new()))
    }

    fn log_statements(mut self, level: log::LevelFilter) -> Self {
        self.log_settings.statements_level = level;
        self
    }

    fn log_slow_statements(
        mut self,
        level: log::LevelFilter,
        duration: std::time::Duration,
    ) -> Self {
        self.log_settings.slow_statements_level = level;
        self.log_settings.slow_statements_duration = duration;
        self
    }
}

impl Connection for WamnConnection {
    type Database = WamnPostgres;
    type Options = WamnConnectOptions;

    fn close(self) -> impl Future<Output = Result<(), Error>> + Send + 'static {
        std::future::ready(Ok(()))
    }

    fn close_hard(self) -> impl Future<Output = Result<(), Error>> + Send + 'static {
        std::future::ready(Ok(()))
    }

    fn ping(&mut self) -> impl Future<Output = Result<(), Error>> + Send + '_ {
        std::future::ready(Ok(()))
    }

    fn begin(
        &mut self,
    ) -> impl Future<Output = Result<Transaction<'_, WamnPostgres>, Error>> + Send + '_ {
        Transaction::begin(self, None)
    }

    fn shrink_buffers(&mut self) {}

    fn flush(&mut self) -> impl Future<Output = Result<(), Error>> + Send + '_ {
        std::future::ready(Ok(()))
    }

    fn should_flush(&self) -> bool {
        false
    }
}

/// Transaction manager rejecting nested transactions and savepoints.
#[derive(Debug)]
pub struct WamnTransactionManager;

impl TransactionManager for WamnTransactionManager {
    type Database = WamnPostgres;

    fn begin(
        connection: &mut WamnConnection,
        statement: Option<SqlStr>,
    ) -> impl Future<Output = Result<(), Error>> + Send + '_ {
        let result = if statement.is_some() {
            Err(Error::InvalidArgument(
                "custom transaction statements are unsupported".to_owned(),
            ))
        } else if connection.transaction.is_some() {
            Err(Error::InvalidArgument(
                "nested transactions and savepoints are unsupported".to_owned(),
            ))
        } else {
            connection
                .capability
                .begin()
                .map(|transaction| connection.transaction = Some(transaction))
                .map_err(database_error)
        };
        std::future::ready(result)
    }

    fn commit(
        connection: &mut WamnConnection,
    ) -> impl Future<Output = Result<(), Error>> + Send + '_ {
        let result = if let Some(mut transaction) = connection.transaction.take() {
            transaction.commit().map_err(database_error)
        } else {
            Err(Error::InvalidArgument(
                "no transaction to commit".to_owned(),
            ))
        };
        std::future::ready(result)
    }

    fn rollback(
        connection: &mut WamnConnection,
    ) -> impl Future<Output = Result<(), Error>> + Send + '_ {
        let result = if let Some(mut transaction) = connection.transaction.take() {
            transaction.rollback().map_err(database_error)
        } else {
            Err(Error::InvalidArgument(
                "no transaction to roll back".to_owned(),
            ))
        };
        std::future::ready(result)
    }

    fn start_rollback(connection: &mut WamnConnection) {
        if let Some(mut transaction) = connection.transaction.take() {
            let _ = transaction.rollback();
        }
    }

    fn get_transaction_depth(connection: &WamnConnection) -> usize {
        usize::from(connection.transaction.is_some())
    }
}

impl<'c> Executor<'c> for &'c mut WamnConnection {
    type Database = WamnPostgres;

    fn execute<'e, 'q: 'e, E>(self, mut query: E) -> BoxFuture<'e, Result<WamnQueryResult, Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, WamnPostgres>,
    {
        let arguments = take_arguments(&mut query);
        let sql = query.sql();
        async move { self.execute_statement(sql.as_str(), arguments?) }.boxed()
    }

    fn fetch_many<'e, 'q: 'e, E>(
        self,
        mut query: E,
    ) -> BoxStream<'e, Result<Either<WamnQueryResult, WamnRow>, Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, WamnPostgres>,
    {
        let arguments = take_arguments(&mut query);
        let sql = query.sql();
        match arguments.and_then(|arguments| self.query(sql.as_str(), arguments)) {
            Ok(rows) => stream::iter(rows.into_iter().map(|row| Ok(Either::Right(row)))).boxed(),
            Err(error) => stream::once(async { Err(error) }).boxed(),
        }
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        mut query: E,
    ) -> BoxFuture<'e, Result<Option<WamnRow>, Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, WamnPostgres>,
    {
        let arguments = take_arguments(&mut query);
        let sql = query.sql();
        async move {
            let mut rows = self.query(sql.as_str(), arguments?)?;
            Ok(if rows.is_empty() {
                None
            } else {
                Some(rows.remove(0))
            })
        }
        .boxed()
    }

    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [WamnTypeInfo],
    ) -> BoxFuture<'e, Result<WamnStatement, Error>>
    where
        'c: 'e,
    {
        async move {
            Ok(WamnStatement {
                sql,
                parameters: parameters.to_vec(),
            })
        }
        .boxed()
    }
}

fn take_arguments<'q, E>(query: &mut E) -> Result<WamnArguments, Error>
where
    E: Execute<'q, WamnPostgres>,
{
    query
        .take_arguments()
        .map_err(Error::Encode)
        .map(Option::unwrap_or_default)
}

fn rows_from_wire(result: wire::RowSet) -> Result<Vec<WamnRow>, Error> {
    let columns = result
        .columns
        .into_iter()
        .enumerate()
        .map(|(ordinal, column)| {
            Ok(WamnColumn {
                ordinal,
                name: column.name.into_boxed_str(),
                type_info: WamnTypeInfo::from_postgres_name(&column.type_name)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    result
        .rows
        .into_iter()
        .enumerate()
        .map(|(row_index, values)| {
            if values.len() != columns.len() {
                return Err(Error::Protocol(format!(
                    "wamn:postgres row {row_index} has {} values for {} columns",
                    values.len(),
                    columns.len()
                )));
            }
            let values = values
                .into_iter()
                .map(WamnValue::from_wire)
                .collect::<Vec<_>>();
            for (column, value) in columns.iter().zip(&values) {
                let actual = value.type_info_value();
                if !value.is_null() && !column.type_info.type_compatible(&actual) {
                    return Err(Error::Protocol(format!(
                        "wamn:postgres row {row_index} column `{}` declares {} but carries {}",
                        column.name, column.type_info, actual
                    )));
                }
            }
            Ok(WamnRow {
                columns: columns.clone(),
                values,
            })
        })
        .collect()
}

/// Runs one callback in exactly one transaction, with no automatic retry.
pub async fn run_transaction<'a, F, T, E>(
    connection: &'a mut WamnConnection,
    callback: F,
) -> Result<T, E>
where
    for<'c> F: FnOnce(&'c mut Transaction<'_, WamnPostgres>) -> BoxFuture<'c, Result<T, E>>
        + Send
        + Sync
        + 'a,
    T: Send,
    E: From<Error> + Send,
{
    connection.transaction(callback).await
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use futures_executor::block_on;

    use super::*;

    #[derive(Debug, Default)]
    struct Calls {
        begin: usize,
        query: usize,
        execute: usize,
        commit: usize,
        rollback: usize,
        params: Vec<Vec<WamnValue>>,
        query_results: VecDeque<Result<wire::RowSet, wire::PgError>>,
        execute_results: VecDeque<Result<u64, wire::PgError>>,
    }

    struct MockCapability(Arc<Mutex<Calls>>);

    struct MockTransaction(Arc<Mutex<Calls>>);

    impl Capability for MockCapability {
        fn query(
            &mut self,
            _sql: &str,
            params: Vec<WamnValue>,
        ) -> Result<wire::RowSet, wire::PgError> {
            let mut calls = self.0.lock().expect("mock calls lock");
            calls.query += 1;
            calls.params.push(params);
            calls.query_results.pop_front().expect("mock query result")
        }

        fn execute(&mut self, _sql: &str, params: Vec<WamnValue>) -> Result<u64, wire::PgError> {
            let mut calls = self.0.lock().expect("mock calls lock");
            calls.execute += 1;
            calls.params.push(params);
            calls
                .execute_results
                .pop_front()
                .expect("mock execute result")
        }

        fn begin(&mut self) -> Result<Box<dyn TransactionCapability>, wire::PgError> {
            self.0.lock().expect("mock calls lock").begin += 1;
            Ok(Box::new(MockTransaction(Arc::clone(&self.0))))
        }
    }

    impl TransactionCapability for MockTransaction {
        fn query(
            &mut self,
            _sql: &str,
            params: Vec<WamnValue>,
        ) -> Result<wire::RowSet, wire::PgError> {
            let mut calls = self.0.lock().expect("mock calls lock");
            calls.query += 1;
            calls.params.push(params);
            calls.query_results.pop_front().expect("mock query result")
        }

        fn execute(&mut self, _sql: &str, params: Vec<WamnValue>) -> Result<u64, wire::PgError> {
            let mut calls = self.0.lock().expect("mock calls lock");
            calls.execute += 1;
            calls.params.push(params);
            calls
                .execute_results
                .pop_front()
                .expect("mock execute result")
        }

        fn commit(&mut self) -> Result<(), wire::PgError> {
            self.0.lock().expect("mock calls lock").commit += 1;
            Ok(())
        }

        fn rollback(&mut self) -> Result<(), wire::PgError> {
            self.0.lock().expect("mock calls lock").rollback += 1;
            Ok(())
        }
    }

    fn connection(calls: &Arc<Mutex<Calls>>) -> WamnConnection {
        WamnConnection {
            capability: Box::new(MockCapability(Arc::clone(calls))),
            transaction: None,
        }
    }

    fn rows(columns: &[(&str, &str)], rows: Vec<Vec<wire::SqlValue>>) -> wire::RowSet {
        wire::RowSet {
            columns: columns
                .iter()
                .map(|(name, type_name)| wire::Column {
                    name: (*name).to_owned(),
                    type_name: (*type_name).to_owned(),
                })
                .collect(),
            rows,
        }
    }

    #[test]
    fn query_as_binds_decodes_and_looks_up_rows() {
        let calls = Arc::new(Mutex::new(Calls {
            query_results: VecDeque::from([Ok(rows(
                &[("id", "int4"), ("name", "text")],
                vec![vec![
                    wire::SqlValue::Int32(7),
                    wire::SqlValue::Text("Ada".into()),
                ]],
            ))]),
            ..Calls::default()
        }));
        let mut connection = connection(&calls);

        let row = block_on(
            sqlx::query_as::<WamnPostgres, (i32, String)>(
                "select id, name from people where id = $1 and name = $2",
            )
            .bind(7_i32)
            .bind("Ada")
            .fetch_one(&mut connection),
        )
        .expect("query_as result");

        assert_eq!(row, (7, "Ada".to_owned()));
        let calls = calls.lock().expect("mock calls lock");
        assert_eq!(calls.query, 1);
        assert_eq!(
            calls.params,
            [vec![WamnValue::Int32(7), WamnValue::Text("Ada".into())]]
        );
    }

    #[test]
    fn row_lookup_by_name_and_shape_validation_are_strict() {
        let result = rows_from_wire(rows(
            &[("item_id", "int8")],
            vec![vec![wire::SqlValue::Int64(91)]],
        ))
        .expect("valid row set");
        assert_eq!(
            result[0].try_get::<i64, _>("item_id").expect("named value"),
            91
        );

        let error = rows_from_wire(rows(
            &[("item_id", "int8"), ("name", "text")],
            vec![vec![wire::SqlValue::Int64(91)]],
        ))
        .expect_err("short row must fail");
        assert!(error.to_string().contains("1 values for 2 columns"));

        let error = rows_from_wire(rows(
            &[("item_id", "int8")],
            vec![vec![wire::SqlValue::Text("wrong variant".into())]],
        ))
        .expect_err("declared type and value case must agree");
        assert!(error.to_string().contains("declares INT8 but carries TEXT"));
    }

    #[test]
    fn every_frozen_value_case_binds_and_decodes() {
        let mut arguments = WamnArguments::default();
        arguments.add(None::<i32>).expect("null bind");
        arguments.add(true).expect("boolean bind");
        arguments.add(3_i32).expect("int32 bind");
        arguments.add(4_i64).expect("int64 bind");
        arguments.add(5.5_f64).expect("float64 bind");
        arguments.add("text").expect("text bind");
        arguments.add(&b"bytes"[..]).expect("bytes bind");
        arguments
            .add(Numeric("12.3400".into()))
            .expect("numeric bind");
        arguments
            .add(TimestampTz("2026-08-28T10:00:00-04:00".into()))
            .expect("timestamptz bind");
        arguments
            .add(Json("{\"ok\":true}".into()))
            .expect("json bind");
        arguments
            .add(Uuid("0198f39d-3600-7e00-8000-000000000001".into()))
            .expect("uuid bind");

        assert_eq!(
            arguments.values,
            vec![
                WamnValue::Null,
                WamnValue::Boolean(true),
                WamnValue::Int32(3),
                WamnValue::Int64(4),
                WamnValue::Float64(5.5),
                WamnValue::Text("text".into()),
                WamnValue::Bytes(b"bytes".to_vec()),
                WamnValue::Numeric("12.3400".into()),
                WamnValue::Timestamptz("2026-08-28T10:00:00-04:00".into()),
                WamnValue::Json("{\"ok\":true}".into()),
                WamnValue::Uuid("0198f39d-3600-7e00-8000-000000000001".into()),
            ]
        );

        let row = rows_from_wire(rows(
            &[
                ("null", "int4"),
                ("boolean", "bool"),
                ("int32", "int4"),
                ("int64", "int8"),
                ("float64", "float8"),
                ("text", "text"),
                ("bytes", "bytea"),
                ("numeric", "numeric"),
                ("timestamptz", "timestamptz"),
                ("json", "jsonb"),
                ("uuid", "uuid"),
            ],
            vec![
                arguments
                    .values
                    .into_iter()
                    .map(WamnValue::into_wire)
                    .collect(),
            ],
        ))
        .expect("all cases row")
        .remove(0);

        assert_eq!(
            row.try_get::<Option<i32>, _>("null").expect("null decode"),
            None
        );
        assert!(row.try_get::<bool, _>("boolean").expect("boolean decode"));
        assert_eq!(row.try_get::<i32, _>("int32").expect("int32 decode"), 3);
        assert_eq!(row.try_get::<i64, _>("int64").expect("int64 decode"), 4);
        let float = row.try_get::<f64, _>("float64").expect("float64 decode");
        assert!((float - 5.5).abs() < f64::EPSILON);
        assert_eq!(
            row.try_get::<String, _>("text").expect("text decode"),
            "text"
        );
        assert_eq!(
            row.try_get::<Vec<u8>, _>("bytes").expect("bytes decode"),
            b"bytes"
        );
        assert_eq!(
            row.try_get::<Numeric, _>("numeric")
                .expect("numeric decode"),
            Numeric("12.3400".into())
        );
        assert_eq!(
            row.try_get::<TimestampTz, _>("timestamptz")
                .expect("timestamptz decode"),
            TimestampTz("2026-08-28T10:00:00-04:00".into())
        );
        assert_eq!(
            row.try_get::<Json, _>("json").expect("json decode"),
            Json("{\"ok\":true}".into())
        );
        assert_eq!(
            row.try_get::<Uuid, _>("uuid").expect("uuid decode"),
            Uuid("0198f39d-3600-7e00-8000-000000000001".into())
        );
    }

    #[test]
    fn transaction_commits_once_on_success() {
        let calls = Arc::new(Mutex::new(Calls {
            execute_results: VecDeque::from([Ok(3)]),
            ..Calls::default()
        }));
        let mut connection = connection(&calls);

        let affected = block_on(run_transaction(&mut connection, |transaction| {
            Box::pin(async move {
                sqlx::query::<WamnPostgres>("update things set ready = $1")
                    .bind(true)
                    .execute(&mut **transaction)
                    .await
                    .map(|result| result.rows_affected())
            })
        }))
        .expect("transaction result");

        assert_eq!(affected, 3);
        let calls = calls.lock().expect("mock calls lock");
        assert_eq!((calls.begin, calls.commit, calls.rollback), (1, 1, 0));
    }

    #[test]
    fn serialization_failure_rolls_back_without_retry() {
        let calls = Arc::new(Mutex::new(Calls {
            execute_results: VecDeque::from([Err(wire::PgError::SerializationFailure)]),
            ..Calls::default()
        }));
        let mut connection = connection(&calls);
        let callback_calls = Arc::new(Mutex::new(0_usize));
        let callback_counter = Arc::clone(&callback_calls);

        let error = block_on(run_transaction(&mut connection, move |transaction| {
            *callback_counter.lock().expect("callback calls lock") += 1;
            Box::pin(async move {
                sqlx::query::<WamnPostgres>("update things set ready = true")
                    .execute(&mut **transaction)
                    .await?;
                Ok::<_, Error>(())
            })
        }))
        .expect_err("serialization failure");

        let database = error.as_database_error().expect("database error");
        let error = database
            .as_error()
            .downcast_ref::<WamnDatabaseError>()
            .expect("wamn error");
        assert_eq!(error.pg_error(), &WamnPgError::SerializationFailure);
        assert_eq!(*callback_calls.lock().expect("callback calls lock"), 1);
        let calls = calls.lock().expect("mock calls lock");
        assert_eq!(
            (calls.begin, calls.execute, calls.commit, calls.rollback),
            (1, 1, 0, 1)
        );
    }
}
