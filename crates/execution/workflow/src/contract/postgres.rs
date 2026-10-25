//! [`PostgresWorkflows`]: the workflow contract over the run plane.

use tokio::sync::Mutex;
use tokio_postgres::{Client, Transaction};
use wamn_run_state::RunStatus;
use wamn_run_state::queue::{
    list_workflow_runs_sql, park_queued_run_sql, release_parked_run_sql, select_run_queue_state_sql,
};

use super::{WorkflowError, WorkflowErrorType, WorkflowRun, Workflows};

/// Why a park or a release changed nothing: the run's status, whether a queue
/// row exists, whether it is parked, and whether a replica holds a live lease.
struct QueueState {
    status: String,
    queued: bool,
    parked: bool,
    leased: bool,
}

/// The workflow contract over one tenant and environment.
///
/// Automation uses project-admin authority.
pub struct PostgresWorkflows {
    client: Mutex<Client>,
    /// The run plane schema, for example `wamn_run`.
    schema: String,
    tenant: String,
    environment: String,
}

impl std::fmt::Debug for PostgresWorkflows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresWorkflows")
            .field("schema", &self.schema)
            .field("tenant", &self.tenant)
            .field("environment", &self.environment)
            .finish_non_exhaustive()
    }
}

impl PostgresWorkflows {
    /// Serve the runs of `tenant` and `environment` in `schema`.
    pub fn new(
        client: Client,
        schema: impl Into<String>,
        tenant: impl Into<String>,
        environment: impl Into<String>,
    ) -> Self {
        Self {
            client: Mutex::new(client),
            schema: schema.into(),
            tenant: tenant.into(),
            environment: environment.into(),
        }
    }

    fn tenant(&self) -> &str {
        &self.tenant
    }

    fn environment(&self) -> &str {
        &self.environment
    }

    /// Bind the transaction to the run plane and the release tenant.
    async fn scope(&self, transaction: &Transaction<'_>) -> Result<(), WorkflowError> {
        transaction
            .query_one(
                "SELECT set_config('search_path', $1, true), set_config('app.tenant', $2, true)",
                &[&self.schema, &self.tenant()],
            )
            .await
            .map_err(|error| WorkflowError::storage("scope the transaction", error))?;
        Ok(())
    }

    /// The facts that name why a park or a release changed nothing.
    async fn queue_state(
        &self,
        transaction: &Transaction<'_>,
        run_id: &str,
    ) -> Result<Option<QueueState>, WorkflowError> {
        let row = transaction
            .query_opt(
                &select_run_queue_state_sql(),
                &[&run_id, &self.environment()],
            )
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
        .map_err(|error: tokio_postgres::Error| {
            WorkflowError::storage("decode the run's queue state", error)
        })
    }
}

#[async_trait::async_trait]
impl Workflows for PostgresWorkflows {
    async fn park(&self, run_id: &str) -> Result<(), WorkflowError> {
        let mut client = self.client.lock().await;
        let transaction = client
            .transaction()
            .await
            .map_err(|error| WorkflowError::storage("begin the park", error))?;
        self.scope(&transaction).await?;
        let parked = transaction
            .query_opt(&park_queued_run_sql(), &[&run_id, &self.environment()])
            .await
            .map_err(|error| WorkflowError::storage("park the run", error))?;
        if parked.is_none() {
            match self.queue_state(&transaction, run_id).await? {
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
                        format!(
                            "run {run_id} is {state} (status {status}), so it cannot be parked"
                        ),
                    ));
                }
            }
        }
        transaction
            .commit()
            .await
            .map_err(|error| WorkflowError::storage("commit the park", error))
    }

    async fn release(&self, run_id: &str) -> Result<(), WorkflowError> {
        let mut client = self.client.lock().await;
        let transaction = client
            .transaction()
            .await
            .map_err(|error| WorkflowError::storage("begin the release", error))?;
        self.scope(&transaction).await?;
        let released = transaction
            .query_opt(&release_parked_run_sql(), &[&run_id, &self.environment()])
            .await
            .map_err(|error| WorkflowError::storage("release the run", error))?;
        if released.is_none() {
            return Err(match self.queue_state(&transaction, run_id).await? {
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
        transaction
            .commit()
            .await
            .map_err(|error| WorkflowError::storage("commit the release", error))
    }

    async fn list(&self, limit: u32) -> Result<Vec<WorkflowRun>, WorkflowError> {
        let limit = i64::from(limit);
        let mut client = self.client.lock().await;
        let transaction = client
            .transaction()
            .await
            .map_err(|error| WorkflowError::storage("begin the list", error))?;
        self.scope(&transaction).await?;
        let rows = transaction
            .query(&list_workflow_runs_sql(), &[&self.environment(), &limit])
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
                        WorkflowError::new(
                            WorkflowErrorType::Storage,
                            "a wiring version is negative",
                        )
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
        transaction
            .commit()
            .await
            .map_err(|error| WorkflowError::storage("end the list", error))?;
        Ok(runs)
    }
}
