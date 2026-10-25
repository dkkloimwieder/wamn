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
//! The hash is bound exactly as the graph gate binds it: the gate
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
//! before the graph is gated. An event trigger names a captured application
//! model and uses the registration validator for its operations and condition.

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{DefinitionHash, ServingManifest, WiringDocument, parse_workflow_schedule};

use std::path::Path;

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

/// Params: `$1` tenant, `$2` environment, `$3` application, `$4` workflow id,
/// `$5` version, `$6` graph, `$7` hash, `$8` trigger, `$9` execution type,
/// `$10` execution role, `$11` authoring principal, `$12` schedule,
/// `$13` source package, `$14` entity, `$15` ops, `$16` condition.
const INSERT_WORKFLOW_SQL: &str = "\
INSERT INTO catalog.workflows (\
       tenant_id, environment, application, workflow_id, version, graph_json, wiring_hash, \
       trigger, execution_type, execution_role, created_by, schedule, source_package, entity, ops, condition\
     ) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, $8, $9, $10, $11::text::uuid, $12, $13, $14, $15, $16) \
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
       AND $11::text <> '' AND schedule IS NOT DISTINCT FROM $12 \
       AND source_package IS NOT DISTINCT FROM $13 AND entity IS NOT DISTINCT FROM $14 \
       AND ops IS NOT DISTINCT FROM $15 AND condition IS NOT DISTINCT FROM $16\
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
    Event,
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
    /// A captured model change, filtered with the registration vocabulary.
    Event {
        source_package: &'a str,
        entity: &'a str,
        ops: &'a [wamn_event_reg::Op],
        condition: Option<&'a str>,
    },
}

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

/// Validate an event trigger against the installed capture maps and the application map.
pub(crate) async fn validate_event_trigger(
    transaction: &Transaction<'_>,
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
    let schemas = transaction.query(
        "SELECT DISTINCT n.nspname::text FROM pg_publication_namespace p JOIN pg_namespace n ON n.oid = p.pnnspid ORDER BY 1", &[]
    ).await.map_err(|error| storage("read captured schemas", error))?;
    let mut models = std::collections::BTreeSet::new();
    for row in schemas {
        let schema: String = row.get(0);
        let sql = format!(
            "SELECT m.entity_id FROM {}.wamn_entities m JOIN pg_class c ON c.oid = m.relation_oid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE m.package_id = $1 AND n.nspname = $2 AND c.relname = m.table_name",
            wamn_pg_core::quote_ident(&schema)
        );
        for model in transaction
            .query(&sql, &[&source_package, &schema])
            .await
            .map_err(|error| storage("read captured model map", error))?
        {
            models.insert(model.get::<_, String>(0));
        }
    }
    let registration = wamn_event_reg::EventRegistration {
        schema_version: wamn_event_reg::SCHEMA_VERSION.to_owned(),
        registration_id: "workflow".to_owned(),
        package_id: source_package.to_owned(),
        source_package_id: source_package.to_owned(),
        entity: entity.to_owned(),
        ops: ops.to_vec(),
        input: wamn_event_reg::RegistrationInput::Event,
        condition: condition.map(str::to_owned),
    };
    wamn_event_reg::validate(&registration, source_package, source_package, &models).map_err(
        |issues| {
            AuthorWorkflowError::new(
                AuthorWorkflowErrorType::Event,
                issues
                    .iter()
                    .map(|issue| format!("{}: {}", issue.path, issue.message))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        },
    )
}

/// Gate and append one disabled workflow version in the caller's transaction.
///
/// `control` is the CONTROL-database connection the gate report is read on,
/// as for workflow authoring.
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
    validate_event_trigger(transaction, request.declared, request.trigger).await?;
    let document = request.document;
    let hash = gate_against_release(
        transaction,
        request.tenant_id,
        &declared.packages,
        request.manifest_digest,
        document,
    )
    .await?;
    let verdict = read_gate_verdict(control, request.tenant_id, &hash).await?;
    require_green_report(verdict, document, &hash)?;

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
    let parameters: [&(dyn tokio_postgres::types::ToSql + Sync); 16] = [
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
        &source_package,
        &entity,
        &ops,
        &condition,
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

pub fn read_wiring_document(path: &Path) -> Result<WiringDocument, AuthorWorkflowError> {
    // The document names each package operation by reference, and the version
    // comes from the wamn.json of the package that holds it.
    let value = wamn_schema_generator::operation_reference::read_authored_document(
        path,
        wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
    )
    .map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Document,
            format!("read wiring document {}", path.display()),
            error,
        )
    })?;
    WiringDocument::parse(&value).map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Document,
            format!("wiring document {} is not a valid wiring", path.display()),
            error,
        )
    })
}

pub(crate) async fn read_gate_verdict(
    control: &Client,
    tenant_id: &str,
    wiring_hash: &DefinitionHash,
) -> Result<Option<bool>, AuthorWorkflowError> {
    control
        .query_one("SELECT set_config('app.tenant', $1, false)", &[&tenant_id])
        .await
        .map_err(|error| storage("claim the gate-report tenant", error))?;
    let hash = wiring_hash.as_str();
    let report = control
        .query_opt(
            "SELECT passed FROM wamn_run.gate_reports WHERE tenant_id = $1 AND wiring_hash = $2",
            &[&tenant_id, &hash],
        )
        .await
        .map_err(|error| storage("read the wiring's gate report", error))?;
    Ok(report.map(|row| row.get(0)))
}

/// Require a GREEN verdict from the report read under this document's own hash.
///
/// The hash is the whole binding. This decides only what the control store
/// answered for that exact key, so a green report belonging to another document
/// cannot reach here as `Some(true)` — it is not under this key at all, and
/// arrives as the same `None` a never-gated document does.
pub(crate) fn require_green_report(
    verdict: Option<bool>,
    document: &WiringDocument,
    wiring_hash: &DefinitionHash,
) -> Result<(), AuthorWorkflowError> {
    match verdict {
        Some(true) => Ok(()),
        Some(false) => Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Report,
            format!(
                "wiring {:?} version {} was gated and REFUSED at {}",
                document.wiring_id,
                document.version,
                wiring_hash.as_str()
            ),
        )),
        None => Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Report,
            format!(
                "wiring {:?} version {} has no gate report at {}",
                document.wiring_id,
                document.version,
                wiring_hash.as_str()
            ),
        )),
    }
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
