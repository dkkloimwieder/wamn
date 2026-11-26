//! Database operations used by a host-attested application invocation.

use std::fmt;

use futures_util::{future::BoxFuture, stream::BoxStream};
use wamn_registry::{ProjectEnvKey, ProjectId};

/// Values at the application SQL boundary.
#[derive(Debug)]
pub enum SqlValue {
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

/// Database refusals exposed to an application SQL caller.
#[derive(Debug)]
pub enum SqlError {
    SerializationFailure,
    ConnectionUnavailable,
    StatementTimeout,
    UniqueViolation(String),
    ForeignKeyViolation(String),
    CheckViolation(String),
    ExclusionViolation(String),
    PermissionDenied,
    QueryError((String, String)),
}

impl SqlError {
    /// A transaction whose connection is already unavailable.
    pub fn closed_transaction() -> Self {
        Self::QueryError((
            "WAMN2".into(),
            "transaction already finished or connection lost".into(),
        ))
    }
}

impl fmt::Display for SqlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for SqlError {}

/// The project database selected by the host.
#[derive(Debug, Clone, Copy)]
pub struct DatabaseTarget<'a> {
    pub project: &'a ProjectId,
    pub env: &'a ProjectEnvKey,
}

type RowBatches = BoxStream<'static, Vec<Vec<SqlValue>>>;
type QueryCompletion = BoxFuture<'static, anyhow::Result<Result<(), SqlError>>>;

/// Column names, bounded row batches, and the separate completion result.
pub struct SqlRows {
    pub columns: Vec<String>,
    pub batches: RowBatches,
    pub finished: QueryCompletion,
}

impl fmt::Debug for SqlRows {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SqlRows")
            .field("columns", &self.columns)
            .finish_non_exhaustive()
    }
}

/// Whether a failed commit could have changed durable data.
#[derive(Debug)]
pub enum CommitFailure {
    RolledBack(SqlError),
    Uncertain(SqlError),
}

/// State of the write log entry for one request.
#[derive(Debug)]
pub enum Claim {
    New,
    Finished(String),
    InProgress,
    OutcomeUnknown,
    Conflict,
}
