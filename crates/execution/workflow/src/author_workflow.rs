//! Author one immutable version of an environment workflow
//! (docs/architecture/execution.md#environment-workflows).
//!
//! A workflow is environment data: its graph is a [`WiringDocument`], and the
//! row lives in `catalog.workflows` of the environment's project database.
//! The graph is subject to the conditions of the release the environment serves, not against
//! one package, by [`wamn_catalog::condition_workflow_document`]: each node resolves
//! to the component of that release whose descriptor carries the node's
//! component, interface version and operation, and the graph is judged over
//! those per-node facts.
//!
//! The release validator produces the hash of the admitted graph.
//! An exact resubmission converges. Other bytes under the same version refuse.
//! A version is authored disabled; [`crate::workflow_lifecycle`] enables it.
//!
//! The application is one the environment declares in `environment.k`, as
//! the serving Helm revision keeps it from the deployed environment. An
//! application it does not declare refuses before the graph is conditioned.
//!
//! The trigger is part of the version: `manual`, or `schedule` with a
//! five-field UTC cron, parsed as the schedule task parses it
//! ([`wamn_catalog::parse_workflow_schedule`]). An invalid cron refuses
//! before the graph is conditioned. An event trigger names a captured application
//! model and uses the registration validator for its operations and condition.

use sqlx::{Connection as _, Row as _};
pub(super) mod plan;

use anyhow::Context as _;

use wamn_catalog::{DefinitionHash, ServingManifest, WiringDocument, parse_workflow_schedule};

use std::path::Path;

const ASSIGN_PROJECT_ENV_KEY_SQL: &str = "SELECT set_config('app.project_env_key', $1, true)";

/// Params: `$1` `project_env_key`, `$2` environment, `$3` application, `$4` workflow id,
/// `$5` version, `$6` graph, `$7` hash, `$8` trigger, `$9` execution type,
/// `$10` execution role, `$11` authoring principal, `$12` schedule,
/// `$13` source package, `$14` entity, `$15` ops, `$16` condition.
const INSERT_WORKFLOW_SQL: &str = "\
INSERT INTO catalog.workflows (\
       project_env_key, environment, application, workflow_id, version, graph_json, wiring_hash, \
       trigger, execution_type, execution_role, created_by, schedule, source_package, entity, ops, condition\
     ) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, $8, $9, $10, $11::text::uuid, $12, $13, $14, $15, $16) \
ON CONFLICT DO NOTHING";

/// The stored version carries exactly this definition. The author is not part
/// of the definition, so a second author of the same bytes converges.
const EXACT_WORKFLOW_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM catalog.workflows \
     WHERE project_env_key = $1 AND environment = $2 AND application = $3 \
       AND workflow_id = $4 AND version = $5 AND graph_json = $6::text::jsonb \
       AND wiring_hash = $7 AND trigger = $8 AND execution_type = $9 \
       AND execution_role IS NOT DISTINCT FROM $10 \
       AND $11::text <> '' AND schedule IS NOT DISTINCT FROM $12 \
       AND source_package IS NOT DISTINCT FROM $13 AND entity IS NOT DISTINCT FROM $14 \
       AND ops IS NOT DISTINCT FROM $15 AND condition IS NOT DISTINCT FROM $16\
    )";

const SELECT_RELEASE_SQL: &str = "\
SELECT canonical_bytes FROM catalog.releases \
 WHERE project_env_key = $1 AND manifest_digest = $2";

/// Stable prefix every workflow-authorship refusal renders with.
pub const WORKFLOW_AUTHORSHIP_REFUSAL: &str = "workflow-authorship-refused";

/// Stable predicate that refused one workflow authorship.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorWorkflowErrorType {
    Storage,
    Document,
    /// The graph does not resolve against, or is not compatible with, the
    /// serving release.
    Condition,
    Conflict,
    /// The schedule is not a five-field UTC cron.
    Schedule,
    /// The environment declares no application of this name.
    Application,
    Event,
}

impl AuthorWorkflowErrorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Document => "document",
            Self::Condition => "condition",
            Self::Conflict => "conflict",
            Self::Schedule => "schedule",
            Self::Application => "application",
            Self::Event => "event",
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

pub use plan::WorkflowExecution;

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

pub use plan::WorkflowTrigger;

impl<'a> WorkflowTrigger<'a> {
    /// The stored `trigger`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Schedule(_) => "schedule",
            Self::Event { .. } => "event",
        }
    }

    /// The stored `schedule`.
    pub const fn schedule(self) -> Option<&'a str> {
        match self {
            Self::Manual | Self::Event { .. } => None,
            Self::Schedule(cron) => Some(cron),
        }
    }
}

/// One workflow version to author. The workflow id and the version are the
/// document's own `wiring-id` and `version`.
#[derive(Clone, Copy, Debug)]
pub struct AuthorWorkflowRequest<'a> {
    pub org: &'a str,
    pub project: &'a str,
    pub environment: &'a str,
    /// The application of the workflow, by the name the environment declares.
    pub application: &'a str,
    /// The applications the environment declares, as the serving Helm
    /// revision keeps them
    /// from the deployed environment.
    pub declared: &'a wamn_catalog::EnvironmentApplications,
    pub document: &'a WiringDocument,
    pub trigger: WorkflowTrigger<'a>,
    pub execution: WorkflowExecution<'a>,
    /// The authoring principal, as the text of its `platform."user"` id.
    pub created_by: &'a str,
    /// The manifest digest of the release the environment serves.
    pub manifest_digest: &'a str,
}

impl AuthorWorkflowRequest<'_> {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> String {
        wamn_registry::project_env_key(self.org, self.project, self.environment)
    }
}

/// Read the installed release `manifest_digest` and condition `document` against it.
pub(crate) async fn condition_against_release(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    application_packages: &std::collections::BTreeSet<String>,
    manifest_digest: &str,
    document: &WiringDocument,
) -> Result<DefinitionHash, AuthorWorkflowError> {
    let bytes: Vec<u8> = sqlx::query(SELECT_RELEASE_SQL)
        .bind(project_env_key)
        .bind(manifest_digest)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| storage("read the serving release", error))?
        .ok_or_else(|| {
            AuthorWorkflowError::new(
                AuthorWorkflowErrorType::Condition,
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
    wamn_catalog::condition_workflow_document(
        project_env_key,
        application_packages,
        &manifest,
        document,
    )
    .map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Condition,
            "the graph fails its conditions against the serving release",
            error,
        )
    })
}

/// Validate an event trigger against the installed capture maps and the application map.
pub(crate) async fn validate_event_trigger(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    declared: &wamn_catalog::EnvironmentApplications,
    trigger: WorkflowTrigger<'_>,
) -> Result<(), AuthorWorkflowError> {
    let WorkflowTrigger::Event {
        source_package,
        entity,
        ops,
        condition,
    } = trigger
    else {
        return Ok(());
    };
    if !declared
        .values()
        .any(|application| application.packages.contains(source_package))
    {
        return Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Event,
            format!(
                "source package {source_package:?} is not an application package of the environment"
            ),
        ));
    }
    let schemas = sqlx::query("SELECT DISTINCT n.nspname::text FROM pg_publication_namespace p JOIN pg_namespace n ON n.oid = p.pnnspid ORDER BY 1").fetch_all(&mut **transaction).await.map_err(|error| storage("read captured schemas", error))?;
    let mut models = std::collections::BTreeSet::new();
    for row in schemas {
        let schema: String = row.get(0);
        let sql = format!(
            "SELECT m.entity_id FROM {}.wamn_entities m JOIN pg_class c ON c.oid = m.relation_oid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE m.package_id = $1 AND n.nspname = $2 AND c.relname = m.table_name",
            postgres_protocol::escape::escape_identifier(&schema)
        );
        for model in sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
            .bind(source_package)
            .bind(&schema)
            .fetch_all(&mut **transaction)
            .await
            .map_err(|error| storage("read captured model map", error))?
        {
            models.insert(model.get::<String, _>(0));
        }
    }
    if !models.contains(entity)
        || ops.is_empty()
        || ops.iter().enumerate().any(|(i, op)| ops[..i].contains(op))
    {
        return Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Event,
            "unknown entity or invalid event operations",
        ));
    }
    if let Some(condition) = condition {
        if condition.trim().is_empty() {
            return Err(AuthorWorkflowError::new(
                AuthorWorkflowErrorType::Event,
                "event condition is empty",
            ));
        }
        jmespath::compile(condition).map_err(|error| {
            AuthorWorkflowError::new(AuthorWorkflowErrorType::Event, error.to_string())
        })?;
    }
    Ok(())
}

/// Validate and append one disabled workflow version in the caller's transaction.
pub async fn author_workflow(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &AuthorWorkflowRequest<'_>,
) -> Result<DefinitionHash, AuthorWorkflowError> {
    sqlx::query(ASSIGN_PROJECT_ENV_KEY_SQL)
        .bind(request.project_env_key())
        .fetch_one(&mut **transaction)
        .await
        .map_err(|error| storage("bind the authoring project identity", error))?;
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
    validate_event_trigger(transaction, request.declared, request.trigger).await?;
    let document = request.document;
    let hash = condition_against_release(
        transaction,
        &request.project_env_key(),
        &declared.packages,
        request.manifest_digest,
        document,
    )
    .await?;

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
    let (source_package, entity, ops, condition) = match request.trigger {
        WorkflowTrigger::Event {
            source_package,
            entity,
            ops,
            condition,
        } => (
            Some(source_package),
            Some(entity),
            Some(ops.iter().map(|op| op.as_str()).collect::<Vec<_>>()),
            condition,
        ),
        _ => (None, None, None, None),
    };
    sqlx::query(INSERT_WORKFLOW_SQL)
        .bind(request.project_env_key())
        .bind(request.environment)
        .bind(request.application)
        .bind(&document.wiring_id)
        .bind(version)
        .bind(&graph_json)
        .bind(stored_hash)
        .bind(trigger)
        .bind(execution_type)
        .bind(execution_role)
        .bind(request.created_by)
        .bind(schedule)
        .bind(source_package)
        .bind(entity)
        .bind(&ops)
        .bind(condition)
        .execute(&mut **transaction)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| storage("append the workflow version", error))?;
    let exact: bool = sqlx::query(EXACT_WORKFLOW_SQL)
        .bind(request.project_env_key())
        .bind(request.environment)
        .bind(request.application)
        .bind(&document.wiring_id)
        .bind(version)
        .bind(&graph_json)
        .bind(stored_hash)
        .bind(trigger)
        .bind(execution_type)
        .bind(execution_role)
        .bind(request.created_by)
        .bind(schedule)
        .bind(source_package)
        .bind(entity)
        .bind(&ops)
        .bind(condition)
        .fetch_one(&mut **transaction)
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

/// Author one workflow version over its own project connection.
pub async fn author_workflow_in_databases(
    database_url: &str,
    request: &AuthorWorkflowRequest<'_>,
) -> anyhow::Result<DefinitionHash> {
    let mut client = wamn_postgres::connect(database_url)
        .await
        .context("connect to the project database")?;

    async {
        let mut transaction = client
            .begin()
            .await
            .context("begin the workflow authorship")?;
        let hash = author_workflow(&mut transaction, request).await?;
        transaction
            .commit()
            .await
            .context("commit the workflow authorship")?;
        anyhow::Ok(hash)
    }
    .await
}

fn storage(context: &'static str, error: sqlx::Error) -> AuthorWorkflowError {
    AuthorWorkflowError::with_source(AuthorWorkflowErrorType::Storage, context, error)
}

pub fn read_compiled_wiring_document(path: &Path) -> Result<WiringDocument, AuthorWorkflowError> {
    let bytes = std::fs::read(path).map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Document,
            format!("read compiled wiring {}", path.display()),
            error,
        )
    })?;
    let value = serde_json::from_slice(&bytes).map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Document,
            format!("parse compiled wiring {}", path.display()),
            error,
        )
    })?;
    let document = WiringDocument::parse(&value).map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Document,
            format!("wiring document {} is not a valid wiring", path.display()),
            error,
        )
    })?;
    if document.nodes.values().any(|node| {
        node.operation
            .rsplit_once('@')
            .is_none_or(|(_, version)| version.is_empty())
    }) {
        return Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Document,
            "workflow author requires compiled wiring output with sealed operation identities"
                .to_owned(),
        ));
    }
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_input_requires_compiled_operation_identities() {
        let path =
            std::env::temp_dir().join(format!("workflow-input-{}.json", uuid::Uuid::now_v7()));
        let mut document = wamn_catalog::fixture_graph("sync", 1);
        for (operation, accepted) in [
            ("netsuite:sync/finish@1.0.0", true),
            ("netsuite:sync/finish", false),
        ] {
            document.nodes.values_mut().next().unwrap().operation = operation.to_owned();
            std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
            let result = read_compiled_wiring_document(&path);
            std::fs::remove_file(&path).unwrap();
            if accepted {
                assert!(result.is_ok());
            } else {
                assert!(matches!(
                    result.unwrap_err().error_type(),
                    AuthorWorkflowErrorType::Document
                ));
            }
        }
    }
}
