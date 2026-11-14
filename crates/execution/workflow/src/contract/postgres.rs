//! [`PostgresWorkflows`]: the workflow contract over the run plane.

use super::{WorkflowError, WorkflowRun};
use sqlx::Connection as _;
use sqlx::PgConnection as Client;
use tokio::sync::Mutex;
use wamn_identity_protocol::PlatformComponent;

/// The workflow contract over one project and environment.
///
/// Automation uses project-admin authority.
pub struct PostgresWorkflows {
    client: Mutex<Client>,
    /// The run plane schema, for example `wamn_run`.
    schema: String,
    project_env_key: String,
    environment: String,
}

impl std::fmt::Debug for PostgresWorkflows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresWorkflows")
            .field("schema", &self.schema)
            .field("project_env_key", &self.project_env_key)
            .field("environment", &self.environment)
            .finish_non_exhaustive()
    }
}

impl PostgresWorkflows {
    /// Serve the runs of `project_env_key` and `environment` in `schema`.
    pub fn new(
        client: Client,
        schema: impl Into<String>,
        project_env_key: impl Into<String>,
        environment: impl Into<String>,
    ) -> Self {
        Self {
            client: Mutex::new(client),
            schema: schema.into(),
            project_env_key: project_env_key.into(),
            environment: environment.into(),
        }
    }

    fn project_env_key(&self) -> &str {
        &self.project_env_key
    }

    fn environment(&self) -> &str {
        &self.environment
    }

    /// Bind the transaction to the run plane and the release project.
    async fn target(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), WorkflowError> {
        let executor = PlatformComponent::Executor;
        sqlx::query("SELECT set_config('search_path', $1, true), set_config('app.project_env_key', $2, true), \
                 set_config('app.user_id', $3, true), set_config('app.operation', $4, true)").bind(&self.schema).bind(self.project_env_key()).bind(executor.principal_id().to_string()).bind(executor.principal_name()).fetch_one(&mut **transaction)
            .await
            .map_err(|error| WorkflowError::storage("target the transaction", error))?;
        Ok(())
    }
}

impl PostgresWorkflows {
    pub async fn park(&self, run_id: &str) -> Result<(), WorkflowError> {
        let mut client = self.client.lock().await;
        let mut tx = client
            .begin()
            .await
            .map_err(|error| WorkflowError::storage("begin park", error))?;
        self.target(&mut tx).await?;
        wamn_run_state::management::park(&mut tx, self.environment(), run_id).await?;
        tx.commit()
            .await
            .map_err(|error| WorkflowError::storage("commit park", error))?;
        Ok(())
    }
    pub async fn release(&self, run_id: &str) -> Result<(), WorkflowError> {
        let mut client = self.client.lock().await;
        let mut tx = client
            .begin()
            .await
            .map_err(|error| WorkflowError::storage("begin release", error))?;
        self.target(&mut tx).await?;
        wamn_run_state::management::release(&mut tx, self.environment(), run_id).await?;
        tx.commit()
            .await
            .map_err(|error| WorkflowError::storage("commit release", error))?;
        Ok(())
    }
    pub async fn list(&self, limit: u32) -> Result<Vec<WorkflowRun>, WorkflowError> {
        let mut client = self.client.lock().await;
        let mut tx = client
            .begin()
            .await
            .map_err(|error| WorkflowError::storage("begin list", error))?;
        self.target(&mut tx).await?;
        let result = wamn_run_state::management::list(&mut tx, self.environment(), limit).await?;
        tx.commit()
            .await
            .map_err(|error| WorkflowError::storage("commit list", error))?;
        Ok(result)
    }
}
