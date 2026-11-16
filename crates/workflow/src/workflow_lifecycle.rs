//! Enable and disable one workflow version
//! (docs/architecture/execution.md#environment-workflows).
//!
//! Both verbs take the environment's lifecycle lock
//! ([`LifecycleLock`], shared with `env apply`). While another verb holds it,
//! they refuse with "apply in progress", and the caller retries. The lock does
//! not queue, so no workflow becomes enabled between an apply's test and its
//! Helm write.
//!
//! Enable runs in one project transaction. It reads the version and evaluates the graph conditions against the release
//! the environment serves now. It makes sure that the enabler has authority,
//! then disables the enabled version of the same workflow and enables this
//! one. `catalog.workflows_one_enabled` keeps one enabled version, and the
//! transaction makes the switch atomic. For `execution_type = 'role'` the
//! enabler holds the execution role or `admin`, and the `workflow` user holds
//! the execution role. `invoker` on a manual trigger needs no test.
//! `invoker` on an event or schedule trigger runs as the `workflow` user with `admin`
//! active, so it is assigning `admin`: the enabler holds `admin`, and the
//! `workflow` user holds `admin`.
//!
//! Disable sets `enabled = false` and nothing else. Queued and running runs
//! finish under the version they pinned.

use sqlx::PgConnection as Client;
use sqlx::Connection as _;
pub(super) mod plan;

use wamn_registry::Triple;

use wamn_catalog::lifecycle_lock::{DEPLOYMENT_IN_PROGRESS, LifecycleLock};

const DISABLE_SQL: &str = "\
UPDATE catalog.workflows SET enabled = false \
 WHERE environment = $1 AND application = $2 \
   AND workflow_id = $3 AND version = $4";

/// The start of the refusal while another verb holds the lifecycle lock.
pub const APPLY_IN_PROGRESS: &str = "apply in progress";

/// Stable prefix of every other lifecycle refusal.
pub const WORKFLOW_LIFECYCLE_REFUSAL: &str = "workflow-lifecycle-refused";

/// Stable predicate that refused one enable or disable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowLifecycleErrorType {
    /// Another verb holds the lifecycle lock. Retry.
    ApplyInProgress,
    NotFound,
    /// The graph fails its conditions against the serving release.
    Storage,
}

impl WorkflowLifecycleErrorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApplyInProgress => "apply-in-progress",
            Self::NotFound => "not-found",
            Self::Storage => "storage",
        }
    }
}

/// Contextual refusal from the workflow-lifecycle boundary. An
/// [`WorkflowLifecycleErrorType::ApplyInProgress`] refusal renders starting
/// with [`APPLY_IN_PROGRESS`]; every other one with
/// [`WORKFLOW_LIFECYCLE_REFUSAL`].
#[derive(Debug)]
pub struct WorkflowLifecycleError {
    type_: WorkflowLifecycleErrorType,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl WorkflowLifecycleError {
    fn new(type_: WorkflowLifecycleErrorType, detail: impl Into<String>) -> Self {
        Self {
            type_,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        type_: WorkflowLifecycleErrorType,
        detail: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            type_,
            detail: detail.into(),
            source: Some(source.into()),
        }
    }


    /// Stable refusal class for callers that must not match display text.
    pub const fn error_type(&self) -> WorkflowLifecycleErrorType {
        self.type_
    }
}

impl std::fmt::Display for WorkflowLifecycleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.type_ == WorkflowLifecycleErrorType::ApplyInProgress {
            return formatter.write_str(&self.detail);
        }
        write!(
            formatter,
            "{WORKFLOW_LIFECYCLE_REFUSAL} ({}): {}",
            self.type_.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for WorkflowLifecycleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

pub use plan::WorkflowVersion;


/// Disable `workflow` under the lifecycle lock of `triple`.
///
/// # Errors
///
/// [`WorkflowLifecycleErrorType::ApplyInProgress`] while another verb holds
/// the lock, [`WorkflowLifecycleErrorType::NotFound`] when the version is not
/// authored.
pub async fn disable_workflow(
    system_url: &str,
    triple: &Triple,
    project: &mut Client,
    workflow: &WorkflowVersion<'_>,
) -> Result<(), WorkflowLifecycleError> {
    under_lock(system_url, triple, async || {
        let mut transaction = begin(project).await?;
        let changed = sqlx::query(DISABLE_SQL)
            .bind(workflow.environment)
            .bind(workflow.application)
            .bind(workflow.workflow_id)
            .bind(workflow.version()?)
            .execute(&mut *transaction)
            .await
            .map_err(|error| storage("disable the workflow version", error))?
            .rows_affected();
        if changed == 0 {
            return Err(workflow.not_found());
        }
        commit(transaction).await
    })
    .await
}

/// Run `verb` while holding the lifecycle lock of `triple`. The lock is
/// released when `verb` ends, with its result or its error.
async fn under_lock(
    system_url: &str,
    triple: &Triple,
    verb: impl AsyncFnOnce() -> Result<(), WorkflowLifecycleError>,
) -> Result<(), WorkflowLifecycleError> {
    let result = async {
        LifecycleLock::hold(
            wamn_postgres::connect(system_url).await?,
            triple,
            async |_| Ok(verb().await),
        )
        .await
    }
    .await;
    match result {
        Ok(result) => result,
        Err(error) if error.to_string().starts_with(DEPLOYMENT_IN_PROGRESS) => {
            Err(WorkflowLifecycleError::new(
                WorkflowLifecycleErrorType::ApplyInProgress,
                format!(
                    "{APPLY_IN_PROGRESS}: another verb holds the lifecycle lock of {triple}; retry"
                ),
            ))
        }
        Err(error) => Err(WorkflowLifecycleError::with_source(
            WorkflowLifecycleErrorType::Storage,
            "take the lifecycle lock",
            error,
        )),
    }
}

async fn begin(
    project: &mut Client,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, WorkflowLifecycleError> {
    project
        .begin()
        .await
        .map_err(|error| storage("begin the workflow transaction", error))
}

async fn commit(
    transaction: sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), WorkflowLifecycleError> {
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit the workflow transaction", error))
}



fn storage(context: &'static str, error: sqlx::Error) -> WorkflowLifecycleError {
    WorkflowLifecycleError::with_source(WorkflowLifecycleErrorType::Storage, context, error)
}
