//! Park, release and list runs on a connection authorized by the caller.
use crate::RunStatus;
use crate::queue::{
    list_workflow_runs_sql, park_queued_run_sql, release_parked_run_sql, select_run_queue_state_sql,
};
use serde::Serialize;
use sqlx::Row as _;
use std::fmt;

/// One stored workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub trigger_source: Option<String>,
    pub status: RunStatus,
    /// A queue row exists: the run waits for a lease or is running.
    pub queued: bool,
    pub parked: bool,
    /// RFC 3339, UTC.
    pub created_at: String,
}

/// Why a contract call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowErrorType {
    /// No run with this id exists in the environment.
    NotFound,
    /// The run is running or finished, so it cannot be parked.
    NotParkable,
    /// The run is not parked, so it cannot be released.
    NotParked,
    /// The database failed.
    Storage,
}

/// A contract call failure, with its type and context.
#[derive(Debug)]
pub struct WorkflowError {
    type_: WorkflowErrorType,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl WorkflowError {
    pub fn new(type_: WorkflowErrorType, message: impl Into<String>) -> Self {
        Self {
            type_,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(
        type_: WorkflowErrorType,
        context: &str,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            type_,
            message: context.to_owned(),
            source: Some(source.into()),
        }
    }

    pub fn storage(
        context: &str,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::with_source(WorkflowErrorType::Storage, context, source)
    }

    pub fn error_type(&self) -> WorkflowErrorType {
        self.type_
    }
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkflowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Why a park or a release changed nothing: the run's status, whether a queue
/// row exists, whether it is parked, and whether a replica holds a live lease.
struct QueueState {
    status: String,
    queued: bool,
    parked: bool,
    leased: bool,
}

/// The facts that name why a park or a release changed nothing.
async fn queue_state(
    connection: &mut sqlx::PgConnection,
    environment: &str,
    run_id: &str,
) -> Result<Option<QueueState>, WorkflowError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(select_run_queue_state_sql()))
        .bind(run_id)
        .bind(environment)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| WorkflowError::storage("read the run's queue state", error))?;
    row.map(|row| {
        Ok(QueueState {
            status: row.try_get(0)?,
            queued: row.try_get(1)?,
            parked: row.try_get(2)?,
            leased: row.try_get(3)?,
        })
    })
    .transpose()
    .map_err(|error: sqlx::Error| WorkflowError::storage("decode the run's queue state", error))
}
/// Hold an unleased queued run until an explicit release.
pub async fn park(
    connection: &mut sqlx::PgConnection,
    environment: &str,
    run_id: &str,
) -> Result<(), WorkflowError> {
    let parked = sqlx::query(sqlx::AssertSqlSafe(park_queued_run_sql()))
        .bind(run_id)
        .bind(environment)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| WorkflowError::storage("park the run", error))?;
    if parked.is_none() {
        match queue_state(connection, environment, run_id).await? {
            None => {
                return Err(WorkflowError::new(
                    WorkflowErrorType::NotFound,
                    format!("run {run_id} does not exist"),
                ));
            }
            Some(QueueState {
                queued: true,
                parked: true,
                ..
            }) => {}
            Some(QueueState {
                status,
                queued,
                leased,
                ..
            }) => {
                let state = if !queued {
                    "finished"
                } else if leased {
                    "running"
                } else {
                    "not dispatched"
                };
                return Err(WorkflowError::new(
                    WorkflowErrorType::NotParkable,
                    format!("run {run_id} is {state} (status {status}), so it cannot be parked"),
                ));
            }
        }
    }
    Ok(())
}

/// Make a parked run eligible for leasing.
pub async fn release(
    connection: &mut sqlx::PgConnection,
    environment: &str,
    run_id: &str,
) -> Result<(), WorkflowError> {
    let released = sqlx::query(sqlx::AssertSqlSafe(release_parked_run_sql()))
        .bind(run_id)
        .bind(environment)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| WorkflowError::storage("release the run", error))?;
    if released.is_none() {
        return Err(match queue_state(connection, environment, run_id).await? {
            None => WorkflowError::new(
                WorkflowErrorType::NotFound,
                format!("run {run_id} does not exist"),
            ),
            Some(_) => WorkflowError::new(
                WorkflowErrorType::NotParked,
                format!("run {run_id} is not parked"),
            ),
        });
    }
    Ok(())
}

/// Read the newest runs in the authorized environment.
pub async fn list(
    connection: &mut sqlx::PgConnection,
    environment: &str,
    limit: u32,
) -> Result<Vec<WorkflowRun>, WorkflowError> {
    let limit = i64::from(limit);

    let rows = sqlx::query(sqlx::AssertSqlSafe(list_workflow_runs_sql()))
        .bind(environment)
        .bind(limit)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| WorkflowError::storage("list the runs", error))?;
    let runs = rows
        .iter()
        .map(|row| {
            let decode = |error| WorkflowError::storage("decode a run", error);
            let version: i32 = row.try_get(3).map_err(decode)?;
            let status: String = row.try_get(5).map_err(decode)?;
            Ok(WorkflowRun {
                run_id: row.try_get(0).map_err(decode)?,
                package_id: row.try_get(1).map_err(decode)?,
                wiring_id: row.try_get(2).map_err(decode)?,
                wiring_version: u32::try_from(version).map_err(|_| {
                    WorkflowError::new(WorkflowErrorType::Storage, "a wiring version is negative")
                })?,
                trigger_source: row.try_get(4).map_err(decode)?,
                status: RunStatus::from_sql(&status).ok_or_else(|| {
                    WorkflowError::new(
                        WorkflowErrorType::Storage,
                        format!("run status {status:?} is unknown"),
                    )
                })?,
                queued: row.try_get(6).map_err(decode)?,
                parked: row.try_get(7).map_err(decode)?,
                created_at: row.try_get(8).map_err(decode)?,
            })
        })
        .collect::<Result<Vec<_>, WorkflowError>>()?;
    Ok(runs)
}
