//! Author one immutable version of an environment workflow
//! (docs/plan/workflows.md R8, epic 1).
//!
//! A workflow is environment data: its graph is a [`WiringDocument`], and the
//! row lives in `catalog.workflows` of the environment's project database.
//! The graph is gated against the release the environment serves, not against
//! one package, by [`wamn_catalog::gate_workflow_document`]: each node resolves
//! to the component of that release whose descriptor carries the node's
//! component, interface version and operation, and the graph is judged over
//! those per-node facts.
//!
//! The hash is bound exactly as [`crate::author_wiring`] binds it: the gate
//! produces it, and the control store must hold a green gate report under it.
//! An exact resubmission converges. Other bytes under the same version refuse.
//! A version is authored disabled; [`crate::workflow_lifecycle`] enables it.
//!
//! The application is one the environment declares in `environment.k`, as
//! the serving Helm revision keeps it ([`crate::environment::show`]). An
//! application it does not declare refuses before the graph is gated.
//!
//! The trigger is part of the version: `manual`, or `schedule` with a
//! five-field UTC cron, parsed as the schedule task parses it
//! ([`wamn_catalog::parse_workflow_schedule`]). An invalid cron refuses
//! before the graph is gated.

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{DefinitionHash, ServingManifest, WiringDocument, parse_workflow_schedule};

use crate::author_wiring::{AuthorWiringError, read_gate_verdict, require_green_report};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

/// Params: `$1` tenant, `$2` environment, `$3` application, `$4` workflow id,
/// `$5` version, `$6` graph, `$7` hash, `$8` trigger, `$9` execution type,
/// `$10` execution role, `$11` authoring principal, `$12` schedule.
const INSERT_WORKFLOW_SQL: &str = "\
INSERT INTO catalog.workflows (\
       tenant_id, environment, application, workflow_id, version, graph_json, wiring_hash, \
       trigger, execution_type, execution_role, created_by, schedule\
     ) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, $8, $9, $10, $11::text::uuid, $12) \
ON CONFLICT DO NOTHING";

/// The stored version carries exactly this definition. The author is not part
/// of the definition, so a second author of the same bytes converges.
const EXACT_WORKFLOW_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM catalog.workflows \
     WHERE tenant_id = $1 AND environment = $2 AND application = $3 \
       AND workflow_id = $4 AND version = $5 AND graph_json = $6::text::jsonb \
       AND wiring_hash = $7 AND trigger = $8 AND execution_type = $9 \
       AND execution_role IS NOT DISTINCT FROM $10 \
       AND $11::text <> '' AND schedule IS NOT DISTINCT FROM $12\
    )";

const SELECT_RELEASE_SQL: &str = "\
SELECT canonical_bytes FROM catalog.releases \
 WHERE tenant_id = $1 AND manifest_digest = $2";

/// Stable prefix every workflow-authorship refusal renders with.
pub const WORKFLOW_AUTHORSHIP_REFUSAL: &str = "workflow-authorship-refused";

/// Stable predicate that refused one workflow authorship.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorWorkflowErrorType {
    Storage,
    Document,
    /// The graph does not resolve against, or is not compatible with, the
    /// serving release.
    Gate,
    /// No GREEN gate report covers this document's own hash.
    Report,
    Conflict,
    /// The schedule is not a five-field UTC cron.
    Schedule,
    /// The environment declares no application of this name.
    Application,
}

impl AuthorWorkflowErrorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Document => "document",
            Self::Gate => "gate",
            Self::Report => "report",
            Self::Conflict => "conflict",
            Self::Schedule => "schedule",
            Self::Application => "application",
        }
    }
}

/// Contextual refusal from the workflow-authorship boundary.
#[derive(Debug)]
pub struct AuthorWorkflowError {
    type_: AuthorWorkflowErrorType,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl AuthorWorkflowError {
    fn new(type_: AuthorWorkflowErrorType, detail: impl Into<String>) -> Self {
        Self {
            type_,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        type_: AuthorWorkflowErrorType,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            type_,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }

    /// The gate-report refusal of [`crate::author_wiring`], under this
    /// boundary's prefix.
    fn report(error: AuthorWiringError) -> Self {
        let type_ = match error.error_type() {
            crate::author_wiring::AuthorWiringErrorType::Storage => {
                AuthorWorkflowErrorType::Storage
            }
            _ => AuthorWorkflowErrorType::Report,
        };
        Self::with_source(
            type_,
            "the control store holds no green gate report for this graph",
            error,
        )
    }

    /// Stable refusal class for callers that must not match display text.
    pub const fn error_type(&self) -> AuthorWorkflowErrorType {
        self.type_
    }
}

impl std::fmt::Display for AuthorWorkflowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{WORKFLOW_AUTHORSHIP_REFUSAL} ({}): {}",
            self.type_.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for AuthorWorkflowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// The identity a run of the workflow executes as (docs/plan/workflows.md R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowExecution<'a> {
    /// The `workflow` user, with this role active.
    Role(&'a str),
    /// The caller, with its active role.
    Invoker,
}

impl<'a> WorkflowExecution<'a> {
    /// The stored `execution_type`.
    pub const fn type_str(self) -> &'static str {
        match self {
            Self::Role(_) => "role",
            Self::Invoker => "invoker",
        }
    }

    /// The stored `execution_role`.
    pub const fn role(self) -> Option<&'a str> {
        match self {
            Self::Role(role) => Some(role),
            Self::Invoker => None,
        }
    }
}

/// What starts a run of the workflow (docs/plan/workflows.md R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowTrigger<'a> {
    /// The host route `workflow.start`.
    Manual,
    /// The schedule task, at each due moment of this five-field UTC cron.
    Schedule(&'a str),
}

impl<'a> WorkflowTrigger<'a> {
    /// The stored `trigger`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Schedule(_) => "schedule",
        }
    }

    /// The stored `schedule`.
    pub const fn schedule(self) -> Option<&'a str> {
        match self {
            Self::Manual => None,
            Self::Schedule(cron) => Some(cron),
        }
    }
}

/// One workflow version to author. The workflow id and the version are the
/// document's own `wiring-id` and `version`.
#[derive(Clone, Copy, Debug)]
pub struct AuthorWorkflowRequest<'a> {
    pub tenant_id: &'a str,
    pub environment: &'a str,
    /// The application of the workflow, by the name the environment declares.
    pub application: &'a str,
    /// The applications the environment declares, as the serving Helm
    /// revision keeps them
    /// ([`crate::environment::show::serving_applications`]).
    pub declared: &'a wamn_catalog::EnvironmentApplications,
    pub document: &'a WiringDocument,
    pub trigger: WorkflowTrigger<'a>,
    pub execution: WorkflowExecution<'a>,
    /// The authoring principal, as the text of its `platform."user"` id.
    pub created_by: &'a str,
    /// The manifest digest of the release the environment serves.
    pub manifest_digest: &'a str,
}

/// Read the installed release `manifest_digest` and gate `document` against it.
pub(crate) async fn gate_against_release(
    transaction: &Transaction<'_>,
    tenant_id: &str,
    application_packages: &std::collections::BTreeSet<String>,
    manifest_digest: &str,
    document: &WiringDocument,
) -> Result<DefinitionHash, AuthorWorkflowError> {
    let bytes: Vec<u8> = transaction
        .query_opt(SELECT_RELEASE_SQL, &[&tenant_id, &manifest_digest])
        .await
        .map_err(|error| storage("read the serving release", error))?
        .ok_or_else(|| {
            AuthorWorkflowError::new(
                AuthorWorkflowErrorType::Gate,
                format!("release {manifest_digest} is not installed in this environment"),
            )
        })?
        .get(0);
    let (manifest, _) = ServingManifest::from_canonical_bytes(&bytes).map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Storage,
            format!("release {manifest_digest} holds no valid manifest"),
            error,
        )
    })?;
    wamn_catalog::gate_workflow_document(tenant_id, application_packages, &manifest, document)
        .map_err(|error| {
            AuthorWorkflowError::with_source(
                AuthorWorkflowErrorType::Gate,
                "the graph does not gate against the serving release",
                error,
            )
        })
}

/// Gate and append one disabled workflow version in the caller's transaction.
///
/// `control` is the CONTROL-database connection the gate report is read on,
/// as for [`crate::author_wiring::author_wiring`].
pub async fn author_workflow(
    control: &Client,
    transaction: &Transaction<'_>,
    request: &AuthorWorkflowRequest<'_>,
) -> Result<DefinitionHash, AuthorWorkflowError> {
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&request.tenant_id])
        .await
        .map_err(|error| storage("claim the authoring tenant", error))?;
    let Some(declared) = request.declared.get(request.application) else {
        return Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Application,
            format!(
                "the environment declares no application {:?}",
                request.application
            ),
        ));
    };
    if let Some(cron) = request.trigger.schedule() {
        parse_workflow_schedule(cron).map_err(|error| {
            AuthorWorkflowError::with_source(
                AuthorWorkflowErrorType::Schedule,
                format!("schedule {cron:?} is not a five-field UTC cron"),
                error,
            )
        })?;
    }
    let document = request.document;
    let hash = gate_against_release(
        transaction,
        request.tenant_id,
        &declared.packages,
        request.manifest_digest,
        document,
    )
    .await?;
    let verdict = read_gate_verdict(control, request.tenant_id, &hash)
        .await
        .map_err(AuthorWorkflowError::report)?;
    require_green_report(verdict, document, &hash).map_err(AuthorWorkflowError::report)?;

    let version = i32::try_from(document.version).map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Document,
            format!(
                "workflow {:?} version {} exceeds the catalog storage width",
                document.wiring_id, document.version
            ),
            error,
        )
    })?;
    let graph_json = serde_json::to_string(document).expect("a wiring document serializes");
    let stored_hash = hash.as_str();
    let execution_type = request.execution.type_str();
    let execution_role = request.execution.role();
    let trigger = request.trigger.as_str();
    let schedule = request.trigger.schedule();
    let parameters: [&(dyn tokio_postgres::types::ToSql + Sync); 12] = [
        &request.tenant_id,
        &request.environment,
        &request.application,
        &document.wiring_id,
        &version,
        &graph_json,
        &stored_hash,
        &trigger,
        &execution_type,
        &execution_role,
        &request.created_by,
        &schedule,
    ];
    transaction
        .execute(INSERT_WORKFLOW_SQL, &parameters)
        .await
        .map_err(|error| storage("append the workflow version", error))?;
    let exact: bool = transaction
        .query_one(EXACT_WORKFLOW_SQL, &parameters)
        .await
        .map_err(|error| storage("verify the stored workflow version", error))?
        .get(0);
    if !exact {
        return Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Conflict,
            format!(
                "workflow {:?} version {} is already authored with another definition",
                document.wiring_id, document.version
            ),
        ));
    }
    Ok(hash)
}

/// Author one workflow version over its own project and control connections.
pub async fn author_workflow_in_databases(
    database_url: &str,
    control_database_url: &str,
    request: &AuthorWorkflowRequest<'_>,
) -> anyhow::Result<DefinitionHash> {
    let (control, control_connection) = tokio_postgres::connect(control_database_url, NoTls)
        .await
        .context("connect to the control store holding the gate reports")?;
    let control_task = tokio::spawn(control_connection);
    let (mut client, connection) = match tokio_postgres::connect(database_url, NoTls).await {
        Ok(opened) => opened,
        Err(error) => {
            control_task.abort();
            return Err(error).context("connect to the project database");
        }
    };
    let connection_task = tokio::spawn(connection);
    let authored = async {
        let transaction = client
            .transaction()
            .await
            .context("begin the workflow authorship")?;
        let hash = author_workflow(&control, &transaction, request).await?;
        transaction
            .commit()
            .await
            .context("commit the workflow authorship")?;
        anyhow::Ok(hash)
    }
    .await;
    connection_task.abort();
    control_task.abort();
    authored
}

fn storage(context: &'static str, error: tokio_postgres::Error) -> AuthorWorkflowError {
    AuthorWorkflowError::with_source(AuthorWorkflowErrorType::Storage, context, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_execution_maps_to_its_stored_columns() {
        assert_eq!(WorkflowExecution::Role("clerk").type_str(), "role");
        assert_eq!(WorkflowExecution::Role("clerk").role(), Some("clerk"));
        assert_eq!(WorkflowExecution::Invoker.type_str(), "invoker");
        assert_eq!(WorkflowExecution::Invoker.role(), None);
        assert_eq!(WorkflowTrigger::Manual.as_str(), "manual");
        assert_eq!(WorkflowTrigger::Manual.schedule(), None);
        assert_eq!(WorkflowTrigger::Schedule("0 * * * *").as_str(), "schedule");
        assert_eq!(
            WorkflowTrigger::Schedule("0 * * * *").schedule(),
            Some("0 * * * *")
        );
    }
}
