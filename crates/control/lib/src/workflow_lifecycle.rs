//! Enable and disable one workflow version (docs/plan/workflows.md R3, R8).
//!
//! Both verbs take the environment's lifecycle lock
//! ([`LifecycleLock`], shared with `env apply`). While another verb holds it,
//! they refuse with "apply in progress", and the caller retries. The lock does
//! not queue, so no workflow becomes enabled between an apply's check and its
//! Helm write.
//!
//! Enable runs in one project transaction. It reads the version, gates its
//! graph against the release the environment serves now, checks the enabler,
//! then disables the enabled version of the same workflow and enables this
//! one. `catalog.workflows_one_enabled` keeps one enabled version, and the
//! transaction makes the switch atomic. For `execution_type = 'role'` the
//! enabler holds the execution role or `admin`, and the `workflow` user holds
//! the execution role. `invoker` on a manual trigger needs no check.
//! `invoker` on an event or schedule trigger runs as the `workflow` user with `admin`
//! active, so it is assigning `admin`: the enabler holds `admin`, and the
//! `workflow` user holds `admin`.
//!
//! Disable sets `enabled = false` and nothing else. Queued and running runs
//! finish under the version they pinned.

use tokio_postgres::{Client, Transaction};
use wamn_catalog::WiringDocument;
use wamn_control_registry::Triple;
use wamn_project_state::{ADMIN_ROLE, workflow_user_id};

use crate::author_workflow::{AuthorWorkflowError, AuthorWorkflowErrorType, gate_against_release};
use crate::environment::lock::{DEPLOYMENT_IN_PROGRESS, LifecycleLock};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

/// Params `$1` to `$5`: the version's key.
const SELECT_VERSION_SQL: &str = "\
SELECT graph_json::text, wiring_hash, execution_role, trigger, source_package, entity, ops, condition FROM catalog.workflows \
 WHERE tenant_id = $1 AND environment = $2 AND application = $3 \
   AND workflow_id = $4 AND version = $5 \
   FOR UPDATE";

/// Params: `$1` user id, `$2` the roles that suffice.
const HOLDS_ROLE_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM platform.user_role \
     WHERE user_id = $1::text::uuid AND role_name = ANY($2)\
    )";

/// Params `$1` to `$5`: the version that stays or becomes enabled.
const DISABLE_OTHERS_SQL: &str = "\
UPDATE catalog.workflows SET enabled = false \
 WHERE tenant_id = $1 AND environment = $2 AND application = $3 \
   AND workflow_id = $4 AND version <> $5 AND enabled";

const ENABLE_SQL: &str = "\
UPDATE catalog.workflows SET enabled = true, enable_sequence = $6 \
 WHERE tenant_id = $1 AND environment = $2 AND application = $3 \
   AND workflow_id = $4 AND version = $5";

const DISABLE_SQL: &str = "\
UPDATE catalog.workflows SET enabled = false \
 WHERE tenant_id = $1 AND environment = $2 AND application = $3 \
   AND workflow_id = $4 AND version = $5";

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
    /// The graph does not gate against the serving release.
    Gate,
    RoleNotHeldByEnabler,
    RoleNotHeldByWorkflowUser,
    Storage,
    Broker,
}

impl WorkflowLifecycleErrorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApplyInProgress => "apply-in-progress",
            Self::NotFound => "not-found",
            Self::Gate => "gate",
            Self::RoleNotHeldByEnabler => "role-not-held-by-enabler",
            Self::RoleNotHeldByWorkflowUser => "role-not-held-by-workflow-user",
            Self::Storage => "storage",
            Self::Broker => "broker",
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

    fn gate(error: AuthorWorkflowError) -> Self {
        let type_ = match error.error_type() {
            AuthorWorkflowErrorType::Storage => WorkflowLifecycleErrorType::Storage,
            _ => WorkflowLifecycleErrorType::Gate,
        };
        Self::with_source(
            type_,
            "gate the workflow against the serving release",
            error,
        )
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

/// The key of one workflow version.
#[derive(Clone, Copy, Debug)]
pub struct WorkflowVersion<'a> {
    pub org: &'a str,
    pub project: &'a str,
    pub environment: &'a str,
    pub application: &'a str,
    pub workflow_id: &'a str,
    pub version: u32,
}

impl WorkflowVersion<'_> {
    /// The database tenant derived from the environment coordinate.
    pub fn tenant(&self) -> String {
        wamn_control_registry::project_env_tenant(self.org, self.project, self.environment)
    }
}

impl WorkflowVersion<'_> {
    fn version(&self) -> Result<i32, WorkflowLifecycleError> {
        i32::try_from(self.version).map_err(|error| {
            WorkflowLifecycleError::with_source(
                WorkflowLifecycleErrorType::NotFound,
                format!(
                    "workflow {:?} version {} exceeds the catalog storage width",
                    self.workflow_id, self.version
                ),
                error,
            )
        })
    }

    fn not_found(&self) -> WorkflowLifecycleError {
        WorkflowLifecycleError::new(
            WorkflowLifecycleErrorType::NotFound,
            format!(
                "workflow {:?} version {} of application {:?} is not authored in environment {:?}",
                self.workflow_id, self.version, self.application, self.environment
            ),
        )
    }
}

/// Enable `workflow` under the lifecycle lock of `triple`, checked against
/// the release `manifest_digest` the environment serves and the authority of
/// `enabler` (the text of its `platform."user"` id). The graph gates within
/// the packages `declared` names for the workflow's application first.
///
/// # Errors
///
/// [`WorkflowLifecycleErrorType::ApplyInProgress`] while another verb holds
/// the lock; otherwise the predicate that refused.
#[expect(
    clippy::too_many_arguments,
    reason = "the lifecycle boundary includes the optional event broker"
)]
pub async fn enable_workflow(
    system_url: &str,
    triple: &Triple,
    project: &mut Client,
    workflow: &WorkflowVersion<'_>,
    enabler: &str,
    manifest_digest: &str,
    declared: &wamn_catalog::EnvironmentApplications,
    events: Option<&crate::environment::EventBroker>,
) -> Result<(), WorkflowLifecycleError> {
    // Enable adds no declaration check: an application the environment does
    // not declare gates with no package of its own.
    let packages = declared
        .get(workflow.application)
        .map(|application| application.packages.clone())
        .unwrap_or_default();
    under_lock(system_url, triple, async || {
        let transaction = begin(project, &workflow.tenant()).await?;
        enable_in(
            &transaction,
            workflow,
            enabler,
            manifest_digest,
            &packages,
            declared,
            triple,
            events,
        )
        .await?;
        commit(transaction).await
    })
    .await
}

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
        let transaction = begin(project, &workflow.tenant()).await?;
        let changed = transaction
            .execute(
                DISABLE_SQL,
                &key(workflow, &workflow.version()?, &workflow.tenant()),
            )
            .await
            .map_err(|error| storage("disable the workflow version", error))?;
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
    match LifecycleLock::hold(system_url, triple, async |_| Ok(verb().await)).await {
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

async fn begin<'a>(
    project: &'a mut Client,
    tenant_id: &str,
) -> Result<Transaction<'a>, WorkflowLifecycleError> {
    let transaction = project
        .transaction()
        .await
        .map_err(|error| storage("begin the workflow transaction", error))?;
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&tenant_id])
        .await
        .map_err(|error| storage("claim the workflow tenant", error))?;
    Ok(transaction)
}

async fn commit(transaction: Transaction<'_>) -> Result<(), WorkflowLifecycleError> {
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit the workflow transaction", error))
}

fn key<'a>(
    workflow: &'a WorkflowVersion<'_>,
    version: &'a i32,
    tenant: &'a String,
) -> [&'a (dyn tokio_postgres::types::ToSql + Sync); 5] {
    [
        tenant,
        &workflow.environment,
        &workflow.application,
        &workflow.workflow_id,
        version,
    ]
}

#[expect(
    clippy::too_many_arguments,
    reason = "the enabling transaction uses the locked environment and broker"
)]
async fn enable_in(
    transaction: &Transaction<'_>,
    workflow: &WorkflowVersion<'_>,
    enabler: &str,
    manifest_digest: &str,
    packages: &std::collections::BTreeSet<String>,
    declared: &wamn_catalog::EnvironmentApplications,
    triple: &Triple,
    events: Option<&crate::environment::EventBroker>,
) -> Result<(), WorkflowLifecycleError> {
    let version = workflow.version()?;
    let tenant = workflow.tenant();
    let key = key(workflow, &version, &tenant);
    let row = transaction
        .query_opt(SELECT_VERSION_SQL, &key)
        .await
        .map_err(|error| storage("read the workflow version", error))?
        .ok_or_else(|| workflow.not_found())?;
    let graph_json: String = row.get(0);
    let stored_hash: String = row.get(1);
    let execution_role: Option<String> = row.get(2);
    let trigger: String = row.get(3);

    let graph = serde_json::from_str(&graph_json)
        .map_err(WorkflowLifecycleError::stored)
        .and_then(|value| WiringDocument::parse(&value).map_err(WorkflowLifecycleError::stored))?;
    let hash = gate_against_release(
        transaction,
        &workflow.tenant(),
        packages,
        manifest_digest,
        &graph,
    )
    .await
    .map_err(WorkflowLifecycleError::gate)?;
    if hash.as_str() != stored_hash {
        return Err(WorkflowLifecycleError::new(
            WorkflowLifecycleErrorType::Gate,
            format!(
                "workflow {:?} version {} stores hash {stored_hash}, and its graph hashes to {}",
                workflow.workflow_id,
                workflow.version,
                hash.as_str()
            ),
        ));
    }

    // The role a run executes with: the execution role, or `admin` for a
    // callerless `invoker` (docs/plan/workflows.md R3).
    let assigned = match (execution_role.as_deref(), trigger.as_str()) {
        (Some(role), _) => Some(role),
        (None, "schedule" | "event") => Some(ADMIN_ROLE),
        (None, _) => None,
    };
    if let Some(role) = assigned {
        let sufficient = [role, ADMIN_ROLE];
        if !holds_role(transaction, enabler, &sufficient).await? {
            return Err(WorkflowLifecycleError::new(
                WorkflowLifecycleErrorType::RoleNotHeldByEnabler,
                format!("the enabler holds neither role {role:?} nor {ADMIN_ROLE:?}"),
            ));
        }
        let workflow_user = workflow_user_id().to_string();
        if !holds_role(transaction, &workflow_user, &[role]).await? {
            return Err(WorkflowLifecycleError::new(
                WorkflowLifecycleErrorType::RoleNotHeldByWorkflowUser,
                format!("the workflow user does not hold role {role:?}"),
            ));
        }
    }

    let enable_sequence = if trigger == "event" {
        let source_package: String = row.get(4);
        let entity: String = row.get(5);
        let stored_ops: Vec<String> = row.get(6);
        let ops = serde_json::from_value::<Vec<wamn_event_reg::Op>>(serde_json::json!(stored_ops))
            .map_err(WorkflowLifecycleError::stored)?;
        let condition: Option<String> = row.get(7);
        crate::author_workflow::validate_event_trigger(
            transaction,
            declared,
            crate::author_workflow::WorkflowTrigger::Event {
                source_package: &source_package,
                entity: &entity,
                ops: &ops,
                condition: condition.as_deref(),
            },
        )
        .await
        .map_err(WorkflowLifecycleError::gate)?;
        let events = events.ok_or_else(|| {
            WorkflowLifecycleError::new(
                WorkflowLifecycleErrorType::Broker,
                "the environment has no event broker",
            )
        })?;
        let tail = crate::event_streams::last_published_sequence(events, triple)
            .await
            .map_err(|error| {
                WorkflowLifecycleError::with_source(
                    WorkflowLifecycleErrorType::Broker,
                    "read the environment event stream tail",
                    error,
                )
            })?;
        Some(i64::try_from(tail).map_err(|error| {
            WorkflowLifecycleError::with_source(
                WorkflowLifecycleErrorType::Broker,
                "the stream sequence exceeds the catalog storage width",
                error,
            )
        })?)
    } else {
        None
    };

    transaction
        .execute(DISABLE_OTHERS_SQL, &key)
        .await
        .map_err(|error| storage("disable the enabled version", error))?;
    transaction
        .execute(
            ENABLE_SQL,
            &[key[0], key[1], key[2], key[3], key[4], &enable_sequence],
        )
        .await
        .map_err(|error| storage("enable the workflow version", error))?;
    Ok(())
}

async fn holds_role(
    transaction: &Transaction<'_>,
    user_id: &str,
    roles: &[&str],
) -> Result<bool, WorkflowLifecycleError> {
    Ok(transaction
        .query_one(HOLDS_ROLE_SQL, &[&user_id, &roles])
        .await
        .map_err(|error| storage("read the role assignments", error))?
        .get(0))
}

impl WorkflowLifecycleError {
    fn stored(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            WorkflowLifecycleErrorType::Storage,
            "the stored graph is not a valid workflow graph",
            error,
        )
    }
}

fn storage(context: &'static str, error: tokio_postgres::Error) -> WorkflowLifecycleError {
    WorkflowLifecycleError::with_source(WorkflowLifecycleErrorType::Storage, context, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_in_progress_renders_without_the_lifecycle_prefix() {
        let refusal = WorkflowLifecycleError::new(
            WorkflowLifecycleErrorType::ApplyInProgress,
            format!("{APPLY_IN_PROGRESS}: acme/wms/prod"),
        );
        assert!(refusal.to_string().starts_with(APPLY_IN_PROGRESS));
        let other = WorkflowLifecycleError::new(WorkflowLifecycleErrorType::NotFound, "absent");
        assert_eq!(
            other.to_string(),
            format!("{WORKFLOW_LIFECYCLE_REFUSAL} (not-found): absent")
        );
    }
}
