//! Database operations used by a host-attested application invocation.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{future::BoxFuture, stream::BoxStream};
use wamn_event::Causation;
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::{ApplicationName, ProjectEnvKey, ProjectId};

use crate::session_identity::SessionIdentity;

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
    pub environment: &'a ProjectEnvKey,
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

#[async_trait]
pub trait OperationStore: fmt::Debug + Send + Sync {
    fn host_projects(&self) -> Box<dyn Iterator<Item = &ProjectId> + '_>;

    async fn bind_host_project(
        &self,
        project: &ProjectId,
        environment: &ProjectEnvKey,
    ) -> anyhow::Result<()>;

    async fn read(
        &self,
        target: DatabaseTarget<'_>,
        identity: &SessionIdentity,
        sql: String,
        params: Vec<SqlValue>,
    ) -> Result<SqlRows, SqlError>;

    async fn begin(
        &self,
        scope: &str,
        target: DatabaseTarget<'_>,
        identity: &SessionIdentity,
        read: bool,
        causation: Option<Causation>,
    ) -> Result<Arc<dyn OperationSession>, SqlError>;

    async fn claim(
        &self,
        target: DatabaseTarget<'_>,
        operation: &CanonicalName,
        key: &str,
        request: Vec<u8>,
    ) -> Result<Claim, SqlError>;

    async fn complete(
        &self,
        target: DatabaseTarget<'_>,
        operation: &CanonicalName,
        key: &str,
        result: String,
    ) -> Result<(), SqlError>;

    async fn release(
        &self,
        target: DatabaseTarget<'_>,
        operation: &CanonicalName,
        key: &str,
    ) -> Result<(), SqlError>;
}

#[async_trait]
pub trait OperationSession: fmt::Debug + Send + Sync {
    async fn query(&self, sql: String, params: Vec<SqlValue>) -> Result<SqlRows, SqlError>;

    async fn query_batch(&self, sql: String) -> Result<(), SqlError>;

    async fn claim(
        &self,
        operation: &CanonicalName,
        key: &str,
        request: Vec<u8>,
    ) -> Result<Claim, SqlError>;

    fn stage_finish(&self, operation: &CanonicalName, key: &str, result: String);

    async fn wal_position(&self) -> Result<String, SqlError>;

    async fn counter_sequence(
        &self,
        application: &ApplicationName,
        sequence: &str,
    ) -> Result<Option<(String, String)>, SqlError>;

    async fn set_counter(
        &self,
        sequence: &str,
        counter: &str,
        key: &str,
        value: i64,
    ) -> Result<(), SqlError>;

    async fn commit(&self) -> Result<(), CommitFailure>;

    async fn rollback(&self) -> Result<(), SqlError>;
}
