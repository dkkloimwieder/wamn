//! Internal development-administrator adapter for the flow-draft loop.
//!
//! There is intentionally no CLI or public transport in this module. Item 5
//! owns retained client identity and client-facing authorization; this adapter
//! proves the shared typed command/query boundary first.

use std::collections::BTreeSet;
use std::time::SystemTime;

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio_postgres::{Client, NoTls};

use wamn_catalog::{
    DraftArtifact, ExecutionBundleIdentity, ExecutionBundleInput, ExecutionBundlePackaging,
    ExecutionPlugManifest, NodeImplementation, PinnedDraftArtifact, StoredValidatedDraftContext,
    ValidatedDraftIdentity, ValidatedDraftIdentityInput,
};
use wamn_scenario_model::{
    AuthoringCaseReport, AuthoringExecutionResult, AuthoringReport, AuthoringReportState,
    ExecutionLineage, FailKind, Outcome, PendingAuthoringReport, PendingAuthoringReportReason,
    RunStatus, ScenarioRefusal,
};
use wamn_scenario_runtime::ScenarioSchemaName;

const AUTHORING_ROLE_PROBE_SQL: &str = "\
WITH session_role AS ( \
    SELECT oid, rolsuper, rolcreatedb, rolcreaterole, rolreplication, rolbypassrls \
      FROM pg_catalog.pg_roles WHERE rolname = session_user \
), author_role AS ( \
    SELECT rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolreplication, rolbypassrls \
      FROM pg_catalog.pg_roles WHERE rolname = 'wamn_scenario_author' \
), allowed_mutation(schema_name, table_name, privilege) AS ( \
    VALUES ('catalog', 'flow_drafts', 'INSERT'), \
           ('catalog', 'flow_drafts', 'UPDATE'), \
           ('catalog', 'validated_flow_drafts', 'INSERT'), \
           ('catalog', 'draft_safe_connection_grants', 'INSERT'), \
           ('catalog', 'draft_safe_connection_grants', 'UPDATE'), \
           ('catalog', 'authoring_command_audit', 'INSERT'), \
           ($1::text, 'authoring_report_reservations', 'INSERT'), \
           ($1::text, 'authoring_report_reservations', 'UPDATE'), \
           ($1::text, 'authoring_suite_case_facts', 'INSERT'), \
           ($1::text, 'authoring_suite_reports', 'INSERT') \
) \
SELECT current_user = session_user, \
       COALESCE(NOT session_role.rolsuper AND NOT session_role.rolcreatedb \
                AND NOT session_role.rolcreaterole AND NOT session_role.rolreplication \
                AND NOT session_role.rolbypassrls, false), \
       COALESCE(NOT author_role.rolcanlogin AND NOT author_role.rolsuper \
                AND NOT author_role.rolcreatedb AND NOT author_role.rolcreaterole \
                AND NOT author_role.rolreplication AND NOT author_role.rolbypassrls, false), \
       pg_catalog.pg_has_role(session_user, 'wamn_scenario_author', 'MEMBER'), \
       pg_catalog.pg_has_role(session_user, 'wamn_scenario_author', 'USAGE'), \
       NOT pg_catalog.pg_has_role(session_user, 'wamn_app', 'MEMBER'), \
       NOT pg_catalog.pg_has_role(session_user, 'wamn_app', 'USAGE'), \
       pg_catalog.has_schema_privilege(current_user, 'catalog', 'USAGE'), \
       pg_catalog.has_schema_privilege(current_user, $1, 'USAGE'), \
       NOT pg_catalog.has_schema_privilege(current_user, 'catalog', 'CREATE'), \
       NOT pg_catalog.has_schema_privilege(current_user, $1, 'CREATE'), \
       pg_catalog.has_table_privilege(current_user, 'catalog.catalog_heads', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.release_flows', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.release_manifests', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.flow_artifacts', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.connection_requirements', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.connection_bindings', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.connection_instances', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.connection_generations', 'SELECT'), \
       pg_catalog.has_table_privilege(current_user, 'catalog.flow_drafts', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.flow_drafts', 'INSERT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.flow_drafts', 'UPDATE'), \
       pg_catalog.has_table_privilege(current_user, 'catalog.validated_flow_drafts', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.validated_flow_drafts', 'INSERT'), \
       pg_catalog.has_table_privilege(current_user, 'catalog.draft_safe_connection_grants', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.draft_safe_connection_grants', 'INSERT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.draft_safe_connection_grants', 'UPDATE'), \
       pg_catalog.has_table_privilege(current_user, 'catalog.authoring_command_audit', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.authoring_command_audit', 'INSERT') \
         AND NOT pg_catalog.has_table_privilege( \
             current_user, 'catalog.authoring_command_audit', 'UPDATE') \
         AND NOT pg_catalog.has_table_privilege( \
             current_user, 'catalog.authoring_command_audit', 'DELETE'), \
       pg_catalog.has_table_privilege(current_user, $2, 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, $2, 'INSERT') \
         AND pg_catalog.has_table_privilege(current_user, $2, 'UPDATE'), \
       pg_catalog.has_table_privilege(current_user, $3, 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, $3, 'INSERT'), \
       pg_catalog.has_table_privilege(current_user, $4, 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, $4, 'INSERT'), \
       pg_catalog.has_function_privilege(current_user, $5, 'EXECUTE'), \
       pg_catalog.has_table_privilege(current_user, $6, 'SELECT') \
         AND NOT EXISTS ( \
             SELECT 1 \
               FROM pg_catalog.unnest( \
                   ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) \
                    AS run_mutation(privilege) \
              WHERE pg_catalog.has_table_privilege( \
                  current_user, $6, run_mutation.privilege) \
                 OR (run_mutation.privilege IN ('INSERT','UPDATE','REFERENCES') \
                     AND pg_catalog.has_any_column_privilege( \
                         current_user, $6, run_mutation.privilege)) \
         ), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_roles AS role \
            WHERE role.rolname NOT IN (session_user, 'wamn_scenario_author') \
              AND pg_catalog.pg_has_role(session_user, role.oid, 'MEMBER') \
       ), \
       NOT EXISTS ( \
           SELECT 1 \
             FROM pg_catalog.pg_class AS relation \
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
             CROSS JOIN pg_catalog.unnest(ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) \
                  AS candidate(privilege) \
            WHERE relation.relkind IN ('r','p') \
              AND namespace.nspname IN ('catalog', $1) \
              AND (pg_catalog.has_table_privilege(current_user, relation.oid, candidate.privilege) \
                   OR (candidate.privilege IN ('INSERT','UPDATE','REFERENCES') \
                       AND pg_catalog.has_any_column_privilege( \
                           current_user, relation.oid, candidate.privilege))) \
              AND NOT EXISTS ( \
                  SELECT 1 FROM allowed_mutation AS allowed \
                   WHERE allowed.schema_name = namespace.nspname \
                     AND allowed.table_name = relation.relname \
                     AND allowed.privilege = candidate.privilege \
              ) \
       ), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_database \
            WHERE datname = pg_catalog.current_database() AND datdba = session_role.oid \
       ), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_namespace \
            WHERE nspname IN ('catalog', $1) AND nspowner = session_role.oid \
       ), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_class AS relation \
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
            WHERE namespace.nspname IN ('catalog', $1) \
              AND relation.relkind IN ('r', 'p') \
              AND relation.relowner = session_role.oid \
       ), \
       NOT EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_proc AS routine \
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = routine.pronamespace \
            WHERE namespace.nspname IN ('catalog', $1) \
              AND routine.proowner = session_role.oid \
       ) \
  FROM session_role CROSS JOIN author_role";

const AUTHORING_SCOPE_SQL: &str = "SELECT \
    pg_catalog.set_config('app.tenant', $1, false), \
    pg_catalog.set_config('search_path', $2, false)";

/// Capability token held only by the trusted host-side development adapter.
#[derive(Debug)]
pub(crate) struct InternalDevAdmin {
    _private: (),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthoringDatabaseIdentity {
    pub database: String,
    pub session_user: String,
    pub server_address: Option<String>,
    pub server_port: Option<i32>,
    pub source_schema: String,
}

impl InternalDevAdmin {
    /// Construct the token at the internal process boundary.
    ///
    /// This does not authenticate a client and must not be exposed through a
    /// retained API surface before PLAN item 5 supplies that boundary.
    pub(crate) fn at_process_boundary() -> Self {
        Self { _private: () }
    }
}

/// Trusted process-local adapter for the typed authoring commands and queries.
///
/// It owns a dedicated host-author database connection, a fixed tenant, and a
/// validated run-plane schema. The guest/runtime database URL is never stored
/// here and the private capability token is never returned to callers.
pub struct InternalAuthoringBackend {
    authority: InternalDevAdmin,
    client: Client,
    connection_task: tokio::task::JoinHandle<()>,
    tenant_id: Box<str>,
    source_schema: ScenarioSchemaName,
    authoring_url_hash: String,
    database_identity: AuthoringDatabaseIdentity,
}

impl InternalAuthoringBackend {
    /// Connect using a credential that has only the inherited, host-side
    /// `wamn_scenario_author` authority for one fixed tenant/run schema.
    pub async fn connect(
        authoring_database_url: &str,
        tenant_id: impl Into<String>,
        source_schema: impl Into<String>,
    ) -> anyhow::Result<Self> {
        if authoring_database_url.is_empty() {
            bail!("authoring database URL must not be empty");
        }
        let tenant_id = tenant_id.into();
        if !wamn_control_registry::identifiers::valid_tenant(&tenant_id) {
            bail!("invalid fixed authoring tenant identity");
        }
        let source_schema = ScenarioSchemaName::new(source_schema.into())
            .context("invalid fixed authoring run schema")?;
        let (client, connection) = tokio_postgres::connect(authoring_database_url, NoTls)
            .await
            .context("connect dedicated authoring database credential")?;
        let connection_task = tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::error!(%error, "authoring database connection failed");
            }
        });
        client
            .query_one(
                "SELECT pg_catalog.set_config('search_path', 'pg_catalog', false)",
                &[],
            )
            .await
            .context("pin trusted search path before authoring authority probe")?;
        let qualified = |table: &str| format!("{}.{}", source_schema.as_str(), table);
        let reservation = qualified("authoring_report_reservations");
        let case_facts = qualified("authoring_suite_case_facts");
        let reports = qualified("authoring_suite_reports");
        let runs = qualified("runs");
        let lock_catalog_head = format!(
            "{}.lock_catalog_head(text,text,text)",
            source_schema.as_str()
        );
        let role_row = client
            .query_one(
                AUTHORING_ROLE_PROBE_SQL,
                &[
                    &source_schema.as_str(),
                    &reservation,
                    &case_facts,
                    &reports,
                    &lock_catalog_head,
                    &runs,
                ],
            )
            .await
            .context("verify effective dedicated authoring authority")?;
        for index in 0..role_row.len() {
            if !role_row
                .try_get::<_, Option<bool>>(index)
                .context("decode authoring authority probe")?
                .unwrap_or(false)
            {
                connection_task.abort();
                bail!(
                    "database session is not an unprivileged, effectively authorized, author-only credential"
                );
            }
        }
        let identity_row = client
            .query_one(
                "SELECT pg_catalog.current_database(), session_user, \
                        pg_catalog.inet_server_addr()::text, pg_catalog.inet_server_port()",
                &[],
            )
            .await
            .context("read dedicated authoring database identity")?;
        let database_identity = AuthoringDatabaseIdentity {
            database: identity_row.get(0),
            session_user: identity_row.get(1),
            server_address: identity_row.get(2),
            server_port: identity_row.get(3),
            source_schema: source_schema.as_str().to_string(),
        };
        let backend = Self {
            authority: InternalDevAdmin::at_process_boundary(),
            client,
            connection_task,
            tenant_id: tenant_id.into_boxed_str(),
            source_schema,
            authoring_url_hash: sha256(authoring_database_url.as_bytes()),
            database_identity,
        };
        backend.scope().await?;
        Ok(backend)
    }

    fn require_tenant(&self, tenant_id: &str) -> anyhow::Result<()> {
        if tenant_id != self.tenant_id.as_ref() {
            bail!("authoring command tenant differs from the backend's fixed tenant");
        }
        Ok(())
    }

    async fn scope(&self) -> anyhow::Result<()> {
        self.client
            .query_one(
                AUTHORING_SCOPE_SQL,
                &[&self.tenant_id.as_ref(), &self.source_schema.as_str()],
            )
            .await
            .context("inject fixed authoring tenant and run schema")?;
        Ok(())
    }

    fn require_distinct_runtime_url(&self, args: &super::ScenarioWorkerArgs) -> anyhow::Result<()> {
        let runtime_url = super::database_url(args)?;
        if sha256(runtime_url.as_bytes()) == self.authoring_url_hash {
            bail!("runtime/guest and host-author database credentials must be distinct");
        }
        Ok(())
    }

    fn require_run_scope(&self, args: &super::ScenarioWorkerArgs) -> anyhow::Result<()> {
        self.require_tenant(&args.tenant)?;
        if args.source_schema != self.source_schema.as_str() {
            bail!("scenario source schema differs from the backend's fixed run schema");
        }
        self.require_distinct_runtime_url(args)
    }
}

impl Drop for InternalAuthoringBackend {
    fn drop(&mut self) {
        self.connection_task.abort();
    }
}

/// Save one mutable flow document under optimistic revision control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveFlowDraft {
    pub tenant_id: String,
    pub draft_id: String,
    pub flow_id: String,
    /// Zero creates the draft; a positive value replaces exactly that revision.
    pub expected_revision: i64,
    /// Exact submitted text, stored byte for byte and never parsed here.
    pub definition: String,
}

/// Result of a mutable draft save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveFlowDraftResult {
    Saved {
        revision: i64,
        edited_at: SystemTime,
    },
    RevisionConflict,
}

/// Immutable inputs that identify the executable used for a draft run.
#[derive(Clone, Debug)]
pub struct DraftBundleInputs {
    pub packaging: ExecutionBundlePackaging,
    pub runner_identity: String,
    pub composition_tool: ExecutionBundleInput,
    /// Trusted composition output; it is never accepted from the draft graph.
    pub plugs: Vec<ExecutionPlugManifest>,
    pub adapters: Vec<ExecutionBundleInput>,
}

/// Command to validate one exact saved draft revision for a stored suite.
#[derive(Clone, Debug)]
pub struct ValidateFlowDraft {
    pub tenant_id: String,
    pub draft_id: String,
    pub draft_revision: i64,
    pub catalog_id: String,
    pub catalog_version: i32,
    pub environment: String,
    pub suite_flow_version: i32,
    pub bundle: DraftBundleInputs,
}

/// Exact immutable pins produced by draft validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedDraftPin {
    pub tenant_id: String,
    pub draft_id: String,
    pub draft_revision: i64,
    /// Version-independent document/cache identity; never the executable pin.
    pub draft_content_hash: String,
    /// Ordinary exact artifact hash, including the proposed runtime/publish version.
    pub draft_artifact_hash: String,
    pub flow_id: String,
    /// Immutable released version that owns the selected stored suite.
    pub suite_flow_version: i32,
    /// Proposed publish/runtime version carried by the validated draft graph.
    pub runtime_flow_version: i32,
    pub catalog_id: String,
    pub catalog_version: i32,
    pub environment: String,
    pub execution_bundle_hash: String,
    pub validated_draft_hash: String,
    /// Immutable applied-release artifact used only as the base for existing
    /// attachment/connection bindings; never executable draft membership.
    pub binding_base_artifact_hash: String,
}

/// Product refusal returned before a draft run is admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftRunRefusal {
    DraftRevisionNotFound,
    InvalidDraft { detail: String },
    CatalogDrift,
    UnresolvedNodes { node_types: Vec<String> },
}

impl std::fmt::Display for DraftRunRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DraftRevisionNotFound => formatter.write_str("draft revision not found"),
            Self::InvalidDraft { detail } => write!(formatter, "draft is invalid: {detail}"),
            Self::CatalogDrift => formatter.write_str("applied catalog or source member drifted"),
            Self::UnresolvedNodes { node_types } => {
                write!(
                    formatter,
                    "draft has unresolved node types: {}",
                    node_types.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for DraftRunRefusal {}

/// Install draft-safe authority for one exact immutable connection generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantDraftSafeGeneration {
    pub tenant_id: String,
    pub environment: String,
    pub instance_id: String,
    pub generation: i64,
    pub reason: String,
}

/// Revoke one exact connection-generation grant without affecting siblings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevokeDraftSafeGeneration {
    pub tenant_id: String,
    pub environment: String,
    pub instance_id: String,
    pub generation: i64,
}

/// Idempotent result of revoking exact draft-safe authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevokeDraftSafeGenerationResult {
    Revoked,
    AlreadyRevokedOrAbsent,
}

/// Read-only report lookup input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoringReportQuery {
    pub tenant_id: String,
    pub report_id: String,
}

/// One exact stored-suite case bound into an immutable report command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct AuthoringCommandCase {
    pub case_id: String,
    pub case_content_hash: String,
    pub run_id: String,
    pub execution_schema: String,
}

/// Observation-affecting command inputs reserved before the first admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct AuthoringReportCommand {
    pub version: u32,
    pub target_kind: String,
    pub flowrunner_digest: String,
    pub source_schema: String,
    pub execution_schema_template: String,
    pub project: String,
    pub allowed_hosts: Vec<String>,
    pub scenario_credentials_digest: Option<String>,
    pub postgres_pool_max_size: u64,
    pub postgres_wait_timeout_ms: u64,
    pub postgres_statement_timeout_ms: u32,
    pub postgres_row_limit: u64,
    pub epoch_secs: u64,
    pub random_seed: u64,
    pub lease_ttl_ms: u64,
    pub cases: Vec<AuthoringCommandCase>,
}

impl InternalAuthoringBackend {
    /// Save an incrementally editable flow document under optimistic revision control.
    pub async fn save_flow_draft(
        &mut self,
        request: &SaveFlowDraft,
    ) -> anyhow::Result<SaveFlowDraftResult> {
        self.require_tenant(&request.tenant_id)?;
        self.scope().await?;
        save_flow_draft(&self.authority, &self.client, request).await
    }

    /// Validate one exact draft revision and persist its executable identity.
    pub async fn validate_flow_draft(
        &mut self,
        request: &ValidateFlowDraft,
        flowrunner_bytes: &[u8],
    ) -> anyhow::Result<Result<ValidatedDraftPin, DraftRunRefusal>> {
        self.require_tenant(&request.tenant_id)?;
        self.scope().await?;
        validate_flow_draft(&self.authority, &mut self.client, request, flowrunner_bytes).await
    }

    /// Grant draft use of one exact immutable connection generation.
    pub async fn grant_draft_safe_generation(
        &mut self,
        grant: &GrantDraftSafeGeneration,
    ) -> anyhow::Result<()> {
        self.require_tenant(&grant.tenant_id)?;
        self.scope().await?;
        grant_draft_safe_generation(&self.authority, &self.client, grant).await
    }

    /// Revoke draft use of one exact immutable connection generation.
    pub async fn revoke_draft_safe_generation(
        &mut self,
        revoke: &RevokeDraftSafeGeneration,
    ) -> anyhow::Result<RevokeDraftSafeGenerationResult> {
        self.require_tenant(&revoke.tenant_id)?;
        self.scope().await?;
        revoke_draft_safe_generation(&self.authority, &self.client, revoke).await
    }

    /// Execute a stored suite against one exact validated draft and retain its report.
    pub async fn execute_validated_draft(
        &mut self,
        args: &super::ScenarioWorkerArgs,
        pin: &ValidatedDraftPin,
        report_id: &str,
    ) -> anyhow::Result<AuthoringExecutionResult> {
        self.require_run_scope(args)?;
        self.require_tenant(&pin.tenant_id)?;
        self.scope().await?;
        super::execute_validated_draft(
            &self.authority,
            &mut self.client,
            &self.database_identity,
            args,
            pin,
            report_id,
        )
        .await
    }

    /// Execute a released stored suite while retaining the same lineage read model.
    pub async fn execute_released_with_report(
        &mut self,
        args: &super::ScenarioWorkerArgs,
        report_id: &str,
    ) -> anyhow::Result<AuthoringExecutionResult> {
        self.require_run_scope(args)?;
        self.scope().await?;
        super::execute_released_with_report(
            &self.authority,
            &mut self.client,
            &self.database_identity,
            args,
            report_id,
        )
        .await
    }

    /// Record one authorized management command on the append-only ledger.
    ///
    /// The row is written with the same author credential and fixed tenant
    /// scope as the command it attributes. This adapter never learns a
    /// principal any other way: the management transport owns that context and
    /// hands over an already-built row.
    pub(crate) async fn record_command_audit(
        &mut self,
        audit: &crate::management::CommandAudit,
    ) -> anyhow::Result<()> {
        self.require_tenant(audit.tenant_id())?;
        self.scope().await?;
        crate::management::insert_command_audit(&self.client, audit).await
    }

    /// Read one missing, pending, or finalized immutable authoring report.
    pub async fn authoring_report(
        &mut self,
        query: &AuthoringReportQuery,
    ) -> anyhow::Result<AuthoringReportState> {
        self.require_tenant(&query.tenant_id)?;
        self.scope().await?;
        authoring_report(&self.authority, &self.client, &self.source_schema, query).await
    }
}

pub(crate) fn sha256(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    let mut output = String::with_capacity(71);
    output.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn validate_identity(value: &str, name: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        bail!("{name} must not be empty");
    }
    Ok(())
}

/// Save one mutable draft document without requiring it to parse or validate.
///
/// The definition is persisted exactly as submitted, so a half-finished edit is
/// a preserved draft rather than a failed command. `validate` parses the stored
/// text at its own stage and returns the canonical typed refusal.
pub(crate) async fn save_flow_draft(
    _authority: &InternalDevAdmin,
    client: &Client,
    request: &SaveFlowDraft,
) -> anyhow::Result<SaveFlowDraftResult> {
    for (value, name) in [
        (&request.tenant_id, "tenant-id"),
        (&request.draft_id, "draft-id"),
        (&request.flow_id, "flow-id"),
    ] {
        validate_identity(value, name)?;
    }
    if request.expected_revision < 0 {
        bail!("expected-revision must not be negative");
    }
    let row = if request.expected_revision == 0 {
        client
            .query_opt(
                wamn_scenario_catalog::authoring::insert_flow_draft_sql(),
                &[
                    &request.tenant_id,
                    &request.draft_id,
                    &request.flow_id,
                    &request.definition,
                ],
            )
            .await
            .context("insert flow draft")?
    } else {
        client
            .query_opt(
                wamn_scenario_catalog::authoring::update_flow_draft_sql(),
                &[
                    &request.tenant_id,
                    &request.draft_id,
                    &request.flow_id,
                    &request.expected_revision,
                    &request.definition,
                ],
            )
            .await
            .context("update flow draft")?
    };
    Ok(row.map_or(SaveFlowDraftResult::RevisionConflict, |row| {
        SaveFlowDraftResult::Saved {
            revision: row.get(0),
            edited_at: row.get(1),
        }
    }))
}

fn resolve_standard_implementations(
    flow: &wamn_flow::Flow,
) -> Result<Vec<NodeImplementation>, DraftRunRefusal> {
    let node_types: BTreeSet<_> = flow
        .nodes
        .iter()
        .map(|node| node.node_type.as_str())
        .collect();
    let mut unresolved = Vec::new();
    let mut implementations = Vec::with_capacity(node_types.len());
    for node_type in node_types {
        let Some(descriptor) = wamn_standard_nodes::describe(node_type) else {
            unresolved.push(node_type.to_string());
            continue;
        };
        let contract = wamn_standard_nodes::resolve_descriptor(descriptor).map_err(|error| {
            DraftRunRefusal::InvalidDraft {
                detail: error.to_string(),
            }
        })?;
        implementations.push(
            NodeImplementation::from_resolved_platform_contract(contract).map_err(|error| {
                DraftRunRefusal::InvalidDraft {
                    detail: error.to_string(),
                }
            })?,
        );
    }
    if unresolved.is_empty() {
        Ok(implementations)
    } else {
        Err(DraftRunRefusal::UnresolvedNodes {
            node_types: unresolved,
        })
    }
}

fn validate_workspace_flow_identity(
    flow: &wamn_flow::Flow,
    stored_flow_id: &str,
) -> Result<(), DraftRunRefusal> {
    if flow.flow_id != stored_flow_id {
        return Err(DraftRunRefusal::InvalidDraft {
            detail: "draft flow identity differs from its workspace".to_string(),
        });
    }
    Ok(())
}

/// Validate and persist one exact draft revision without creating a release.
pub(crate) async fn validate_flow_draft(
    _authority: &InternalDevAdmin,
    client: &mut Client,
    request: &ValidateFlowDraft,
    flowrunner_bytes: &[u8],
) -> anyhow::Result<Result<ValidatedDraftPin, DraftRunRefusal>> {
    let row = client
        .query_opt(
            wamn_scenario_catalog::authoring::select_flow_draft_sql(),
            &[
                &request.tenant_id,
                &request.draft_id,
                &request.draft_revision,
            ],
        )
        .await
        .context("read exact flow draft revision")?;
    let Some(row) = row else {
        return Ok(Err(DraftRunRefusal::DraftRevisionNotFound));
    };
    let flow_id: String = row.get(0);
    // Exactly the bytes the client saved. Parsing them is this stage's job, and
    // failing to parse them is this stage's typed refusal.
    let definition: String = row.get(1);
    let edited_at: SystemTime = row.get(2);
    let flow = match wamn_flow::Flow::from_json(&definition) {
        Ok(flow) => flow,
        Err(error) => {
            return Ok(Err(DraftRunRefusal::InvalidDraft {
                detail: error.to_string(),
            }));
        }
    };
    if let Err(refusal) = validate_workspace_flow_identity(&flow, &flow_id) {
        return Ok(Err(refusal));
    }
    let implementations = match resolve_standard_implementations(&flow) {
        Ok(implementations) => implementations,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let runner = ExecutionBundleInput::new(
        request.bundle.runner_identity.clone(),
        sha256(flowrunner_bytes),
    )
    .map_err(anyhow::Error::new)?;
    let execution_bundle = ExecutionBundleIdentity::builder(
        request.bundle.packaging,
        runner,
        request.bundle.composition_tool.clone(),
    )
    .implementations(implementations.clone())
    .plugs(request.bundle.plugs.clone())
    .adapters(request.bundle.adapters.clone())
    .build()
    .map_err(anyhow::Error::new)?;
    let draft =
        match DraftArtifact::new(&request.tenant_id, &flow, implementations, execution_bundle) {
            Ok(draft) => draft,
            Err(error) => {
                return Ok(Err(DraftRunRefusal::InvalidDraft {
                    detail: error.to_string(),
                }));
            }
        };

    let transaction = client
        .transaction()
        .await
        .context("begin validated draft transaction")?;
    let locked_catalog_version: Option<i32> = transaction
        .query_one(
            wamn_scenario_catalog::authoring::lock_draft_catalog_head_sql(),
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.environment,
            ],
        )
        .await
        .context("lock draft validation catalog head")?
        .get(0);
    if locked_catalog_version != Some(request.catalog_version) {
        transaction.rollback().await?;
        return Ok(Err(DraftRunRefusal::CatalogDrift));
    }
    let source = transaction
        .query_opt(
            wamn_scenario_catalog::authoring::select_draft_catalog_source_member_sql(),
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.environment,
                &request.catalog_version,
                &flow_id,
                &request.suite_flow_version,
            ],
        )
        .await
        .context("lock applied catalog and source release member")?;
    let Some(source) = source else {
        transaction.rollback().await?;
        return Ok(Err(DraftRunRefusal::CatalogDrift));
    };
    let applied_catalog_version: i32 = source.get(0);
    let binding_base_artifact_hash: String = source.get(1);
    if applied_catalog_version != request.catalog_version {
        transaction.rollback().await?;
        return Ok(Err(DraftRunRefusal::CatalogDrift));
    }

    let artifact = draft.artifact();
    let interface_bundle_json =
        String::from_utf8(artifact.interface_bundle().canonical_bytes().to_vec())
            .context("interface bundle is not UTF-8 JSON")?;
    let component_digests_json = serde_json::to_string(artifact.supplied_components())?;
    let occurrence_recovery_json = String::from_utf8(artifact.occurrence_recovery_bytes().to_vec())
        .context("occurrence recovery is not UTF-8 JSON")?;
    let runtime_flow_version = i32::try_from(flow.version).context("flow version exceeds i32")?;
    let catalog_version =
        u32::try_from(request.catalog_version).context("catalog version must be a positive u32")?;
    let suite_flow_version = u32::try_from(request.suite_flow_version)
        .context("suite flow version must be a positive u32")?;
    let draft_revision =
        u64::try_from(request.draft_revision).context("draft revision must be a positive u64")?;
    let validated_identity = ValidatedDraftIdentity::new(ValidatedDraftIdentityInput {
        tenant_id: &request.tenant_id,
        draft_id: &request.draft_id,
        draft_revision,
        flow_id: &flow_id,
        runtime_flow_version: flow.version,
        draft_content_hash: draft.content_hash().as_str(),
        draft_artifact_hash: artifact.identity().artifact_hash().as_str(),
        execution_bundle_hash: draft.execution_bundle().hash(),
        catalog_id: &request.catalog_id,
        catalog_version,
        environment: &request.environment,
        suite_flow_version,
        binding_base_artifact_hash: &binding_base_artifact_hash,
    })?;
    let graph_json = flow.to_json();
    let execution_bundle_bytes = draft.execution_bundle().canonical_bytes().to_vec();
    let inserted = transaction
        .query_opt(
            wamn_scenario_catalog::authoring::insert_validated_flow_draft_sql(),
            &[
                &request.tenant_id,
                &request.draft_id,
                &request.draft_revision,
                &draft.content_hash().as_str(),
                &request.catalog_id,
                &request.catalog_version,
                &request.environment,
                &request.suite_flow_version,
                &runtime_flow_version,
                &graph_json,
                &artifact.graph_hash(),
                &artifact.identity().artifact_hash().as_str(),
                &interface_bundle_json,
                &artifact.interface_bundle().hash(),
                &component_digests_json,
                &occurrence_recovery_json,
                &artifact.occurrence_recovery_hash(),
                &execution_bundle_bytes,
                &draft.execution_bundle().hash(),
                &binding_base_artifact_hash,
                &validated_identity.as_str(),
                &definition,
            ],
        )
        .await
        .context("persist validated flow draft")?;
    let persisted_edited_at: SystemTime = if let Some(row) = inserted {
        row.get(0)
    } else {
        let existing = transaction
            .query_opt(
                wamn_scenario_catalog::authoring::select_validated_flow_draft_sql(),
                &[
                    &request.tenant_id,
                    &request.draft_id,
                    &request.draft_revision,
                    &draft.content_hash().as_str(),
                    &request.catalog_id,
                    &request.catalog_version,
                    &request.environment,
                    &request.suite_flow_version,
                    &runtime_flow_version,
                    &artifact.identity().artifact_hash().as_str(),
                    &draft.execution_bundle().hash(),
                    &binding_base_artifact_hash,
                    &validated_identity.as_str(),
                ],
            )
            .await
            .context("read idempotent validated flow draft")?;
        let Some(existing) = existing else {
            transaction.rollback().await?;
            return Ok(Err(DraftRunRefusal::DraftRevisionNotFound));
        };
        existing.get(2)
    };
    if persisted_edited_at != edited_at {
        transaction.rollback().await?;
        bail!("immutable validation edit origin differs from its exact source revision");
    }
    transaction
        .commit()
        .await
        .context("commit validated flow draft")?;

    Ok(Ok(ValidatedDraftPin {
        tenant_id: request.tenant_id.clone(),
        draft_id: request.draft_id.clone(),
        draft_revision: request.draft_revision,
        draft_content_hash: draft.content_hash().as_str().to_string(),
        draft_artifact_hash: artifact.identity().artifact_hash().as_str().to_string(),
        flow_id,
        suite_flow_version: request.suite_flow_version,
        runtime_flow_version,
        catalog_id: request.catalog_id.clone(),
        catalog_version: request.catalog_version,
        environment: request.environment.clone(),
        execution_bundle_hash: draft.execution_bundle().hash().to_string(),
        validated_draft_hash: validated_identity.as_str().to_string(),
        binding_base_artifact_hash,
    }))
}

/// Install one exact, revocable draft-safe generation grant.
pub(crate) async fn grant_draft_safe_generation(
    _authority: &InternalDevAdmin,
    client: &Client,
    grant: &GrantDraftSafeGeneration,
) -> anyhow::Result<()> {
    if grant.generation <= 0 {
        bail!("draft-safe generation must be positive");
    }
    for (value, name) in [
        (&grant.tenant_id, "tenant-id"),
        (&grant.environment, "environment"),
        (&grant.instance_id, "instance-id"),
        (&grant.reason, "reason"),
    ] {
        validate_identity(value, name)?;
    }
    client
        .execute(
            wamn_scenario_catalog::authoring::grant_draft_safe_generation_sql(),
            &[
                &grant.tenant_id,
                &grant.environment,
                &grant.instance_id,
                &grant.generation,
                &grant.reason,
            ],
        )
        .await
        .context("grant draft-safe connection generation")?;
    Ok(())
}

/// Revoke one exact draft-safe generation under host-only authority.
pub(crate) async fn revoke_draft_safe_generation(
    _authority: &InternalDevAdmin,
    client: &Client,
    revoke: &RevokeDraftSafeGeneration,
) -> anyhow::Result<RevokeDraftSafeGenerationResult> {
    if revoke.generation <= 0 {
        bail!("draft-safe generation must be positive");
    }
    for (value, name) in [
        (&revoke.tenant_id, "tenant-id"),
        (&revoke.environment, "environment"),
        (&revoke.instance_id, "instance-id"),
    ] {
        validate_identity(value, name)?;
    }
    let changed = client
        .execute(
            wamn_scenario_catalog::authoring::revoke_draft_safe_generation_sql(),
            &[
                &revoke.tenant_id,
                &revoke.environment,
                &revoke.instance_id,
                &revoke.generation,
            ],
        )
        .await
        .context("revoke draft-safe connection generation")?;
    Ok(if changed == 1 {
        RevokeDraftSafeGenerationResult::Revoked
    } else {
        RevokeDraftSafeGenerationResult::AlreadyRevokedOrAbsent
    })
}

/// Reverified executable draft loaded from the immutable validation row.
#[derive(Debug)]
pub(crate) struct LoadedValidatedDraft {
    pub graph_json: String,
    pub draft_edited_at: SystemTime,
    pub runner_digest: String,
}

/// Reload and rehash every exact draft pin before a worker instantiates it.
pub(crate) async fn load_validated_draft(
    client: &Client,
    pin: &ValidatedDraftPin,
) -> anyhow::Result<Result<LoadedValidatedDraft, DraftRunRefusal>> {
    let row = client
        .query_opt(
            wamn_scenario_catalog::authoring::select_validated_flow_draft_sql(),
            &[
                &pin.tenant_id,
                &pin.draft_id,
                &pin.draft_revision,
                &pin.draft_content_hash,
                &pin.catalog_id,
                &pin.catalog_version,
                &pin.environment,
                &pin.suite_flow_version,
                &pin.runtime_flow_version,
                &pin.draft_artifact_hash,
                &pin.execution_bundle_hash,
                &pin.binding_base_artifact_hash,
                &pin.validated_draft_hash,
            ],
        )
        .await
        .context("reload exact validated flow draft")?;
    let Some(row) = row else {
        return Ok(Err(DraftRunRefusal::CatalogDrift));
    };
    let stored_draft_id: String = row.get(0);
    let stored_draft_revision: i64 = row.get(1);
    let draft_edited_at: SystemTime = row.get(2);
    let stored_environment: String = row.get(3);
    let stored_flow_id: String = row.get(4);
    let stored_runtime_version: i32 = row.get(5);
    if stored_draft_id != pin.draft_id
        || stored_draft_revision != pin.draft_revision
        || stored_environment != pin.environment
        || stored_flow_id != pin.flow_id
        || stored_runtime_version != pin.runtime_flow_version
    {
        return Ok(Err(DraftRunRefusal::CatalogDrift));
    }
    let runtime_flow_version = u32::try_from(stored_runtime_version)
        .context("stored draft runtime version must be a positive u32")?;
    let draft_revision = u64::try_from(stored_draft_revision)
        .context("stored draft revision must be a positive u64")?;
    let graph_json: String = row.get(6);
    let graph_hash: String = row.get(7);
    let draft_artifact_hash: String = row.get(8);
    let interface_bundle_json: String = row.get(9);
    let interface_bundle_hash: String = row.get(10);
    let component_digests_json: String = row.get(11);
    let occurrence_recovery_json: Option<String> = row.get(12);
    let occurrence_recovery_hash: Option<String> = row.get(13);
    let execution_bundle_bytes: Vec<u8> = row.get(14);
    let stored_binding_base: String = row.get(15);
    let artifact = match PinnedDraftArtifact::from_storage(
        &pin.tenant_id,
        &pin.flow_id,
        runtime_flow_version,
        &pin.draft_content_hash,
        &graph_json,
        &graph_hash,
        &draft_artifact_hash,
        &interface_bundle_json,
        &interface_bundle_hash,
        &component_digests_json,
        occurrence_recovery_json.as_deref(),
        occurrence_recovery_hash.as_deref(),
        execution_bundle_bytes,
        &pin.execution_bundle_hash,
        StoredValidatedDraftContext {
            expected_identity_hash: &pin.validated_draft_hash,
            draft_id: &pin.draft_id,
            draft_revision,
            catalog_id: &pin.catalog_id,
            catalog_version: u32::try_from(pin.catalog_version)
                .context("stored draft catalog version must be a positive u32")?,
            environment: &pin.environment,
            suite_flow_version: u32::try_from(pin.suite_flow_version)
                .context("stored draft suite version must be a positive u32")?,
            binding_base_artifact_hash: &stored_binding_base,
        },
    ) {
        Ok(artifact) => artifact,
        Err(_) => return Ok(Err(DraftRunRefusal::CatalogDrift)),
    };
    if artifact.validated_identity().as_str() != pin.validated_draft_hash
        || artifact.execution_bundle().hash() != pin.execution_bundle_hash
        || draft_artifact_hash != pin.draft_artifact_hash
        || stored_binding_base != pin.binding_base_artifact_hash
    {
        return Ok(Err(DraftRunRefusal::CatalogDrift));
    }
    let runner_digest = match artifact.execution_bundle().runner_input() {
        Ok(runner) => runner.digest().to_string(),
        Err(_) => return Ok(Err(DraftRunRefusal::CatalogDrift)),
    };
    Ok(Ok(LoadedValidatedDraft {
        graph_json: artifact.artifact().flow().to_json(),
        draft_edited_at,
        runner_digest,
    }))
}

fn parse_run_status(value: &str) -> anyhow::Result<RunStatus> {
    match value {
        "dispatched" => Ok(RunStatus::Dispatched),
        "running" => Ok(RunStatus::Running),
        "completed" => Ok(RunStatus::Completed),
        "failed" => Ok(RunStatus::Failed),
        "cancelled" => Ok(RunStatus::Cancelled),
        "infrastructure-failure" => Ok(RunStatus::InfrastructureFailure),
        other => bail!("unknown report run status {other:?}"),
    }
}

fn parse_fail_kind(value: Option<&str>) -> anyhow::Result<Option<FailKind>> {
    value
        .map(|value| match value {
            "terminal" => Ok(FailKind::Terminal),
            "retry-exhausted" => Ok(FailKind::RetryExhausted),
            "invalid-input" => Ok(FailKind::InvalidInput),
            "runaway-budget" => Ok(FailKind::RunawayBudget),
            "effect-uncertain" => Ok(FailKind::EffectUncertain),
            other => bail!("unknown report fail kind {other:?}"),
        })
        .transpose()
}

fn run_status_sql(value: RunStatus) -> &'static str {
    match value {
        RunStatus::Dispatched => "dispatched",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
        RunStatus::InfrastructureFailure => "infrastructure-failure",
    }
}

fn fail_kind_sql(value: FailKind) -> &'static str {
    match value {
        FailKind::Terminal => "terminal",
        FailKind::RetryExhausted => "retry-exhausted",
        FailKind::InvalidInput => "invalid-input",
        FailKind::RunawayBudget => "runaway-budget",
        FailKind::EffectUncertain => "effect-uncertain",
    }
}

/// Exact reservation identity written before the first case admission.
pub(crate) struct AuthoringReportReservation<'a> {
    pub tenant_id: &'a str,
    pub report_id: &'a str,
    pub execution_id: &'a str,
    pub flow_id: &'a str,
    pub suite_flow_version: i32,
    pub suite_id: &'a str,
    pub command: &'a AuthoringReportCommand,
    pub lineage: &'a ExecutionLineage,
}

struct StoredAuthoringReservation {
    execution_id: String,
    flow_id: String,
    suite_flow_version: i32,
    suite_id: String,
    command: AuthoringReportCommand,
    command_hash: String,
    lineage: ExecutionLineage,
    lineage_hash: String,
    state: String,
}

fn canonical_json<T: Serialize>(value: &T) -> anyhow::Result<(String, String)> {
    let value = serde_json::to_value(value).context("serialize canonical authoring JSON")?;
    let bytes = wamn_flow::canonical_json_bytes(&value);
    let json = String::from_utf8(bytes).expect("canonical JSON is UTF-8");
    Ok((json, wamn_flow::canonical_json_sha256(&value)))
}

fn decode_reservation(row: &tokio_postgres::Row) -> anyhow::Result<StoredAuthoringReservation> {
    let command_json: String = row.get(4);
    let command_hash: String = row.get(5);
    let command_value: serde_json::Value =
        serde_json::from_str(&command_json).context("parse reserved authoring command")?;
    if wamn_flow::canonical_json_sha256(&command_value) != command_hash {
        bail!("reserved authoring command hash does not match its JSON");
    }
    let lineage_json: String = row.get(6);
    let lineage_hash: String = row.get(7);
    let lineage_value: serde_json::Value =
        serde_json::from_str(&lineage_json).context("parse reserved authoring lineage")?;
    if wamn_flow::canonical_json_sha256(&lineage_value) != lineage_hash {
        bail!("reserved authoring lineage hash does not match its JSON");
    }
    Ok(StoredAuthoringReservation {
        execution_id: row.get(0),
        flow_id: row.get(1),
        suite_flow_version: row.get(2),
        suite_id: row.get(3),
        command: serde_json::from_value(command_value)
            .context("decode reserved authoring command")?,
        command_hash,
        lineage: serde_json::from_value(lineage_value)
            .context("decode reserved authoring lineage")?,
        lineage_hash,
        state: row.get(8),
    })
}

async fn select_reservation(
    client: &Client,
    tenant_id: &str,
    report_id: &str,
) -> anyhow::Result<Option<StoredAuthoringReservation>> {
    client
        .query_opt(
            wamn_scenario_catalog::authoring::select_authoring_report_reservation_sql(),
            &[&tenant_id, &report_id],
        )
        .await
        .context("read authoring report reservation")?
        .as_ref()
        .map(decode_reservation)
        .transpose()
}

async fn select_case_facts(
    client: &Client,
    tenant_id: &str,
    report_id: &str,
    command: &AuthoringReportCommand,
) -> anyhow::Result<Vec<AuthoringCaseReport>> {
    let rows = client
        .query(
            wamn_scenario_catalog::authoring::select_authoring_suite_case_facts_sql(),
            &[&tenant_id, &report_id],
        )
        .await
        .context("read immutable authoring case facts")?;
    if rows.len() > command.cases.len() {
        bail!("authoring report has more facts than reserved cases");
    }
    let mut facts = Vec::with_capacity(rows.len());
    for (expected_ordinal, row) in rows.into_iter().enumerate() {
        let stored_ordinal: i32 = row.get(0);
        if stored_ordinal != i32::try_from(expected_ordinal).context("case ordinal exceeds i32")? {
            bail!("authoring case facts are not a contiguous prefix");
        }
        let expected = &command.cases[expected_ordinal];
        let case_id: String = row.get(1);
        let run_id: String = row.get(2);
        if case_id != expected.case_id || run_id != expected.run_id {
            bail!("authoring case fact does not match its reserved command position");
        }
        let stored_passed: bool = row.get(3);
        let status_text: String = row.get(4);
        let fail_kind_text: Option<String> = row.get(5);
        let fail_node: Option<String> = row.get(6);
        let outcome_json: String = row.get(7);
        let outcome: Outcome =
            serde_json::from_str(&outcome_json).context("parse authoring case outcome")?;
        let fact = AuthoringCaseReport::new(
            case_id,
            run_id,
            parse_run_status(&status_text)?,
            parse_fail_kind(fail_kind_text.as_deref())?,
            fail_node,
            outcome,
        );
        if fact.passed != stored_passed {
            bail!("stored case pass/fail disagrees with its immutable outcome");
        }
        facts.push(fact);
    }
    Ok(facts)
}

fn pending_report(
    query: &AuthoringReportQuery,
    reservation: &StoredAuthoringReservation,
    reason: PendingAuthoringReportReason,
    captured_cases: Vec<AuthoringCaseReport>,
) -> PendingAuthoringReport {
    PendingAuthoringReport {
        report_id: query.report_id.clone(),
        execution_id: reservation.execution_id.clone(),
        tenant_id: query.tenant_id.clone(),
        flow_id: reservation.flow_id.clone(),
        suite_flow_version: reservation.suite_flow_version,
        suite_id: reservation.suite_id.clone(),
        lineage: reservation.lineage.clone(),
        reason,
        captured_cases,
    }
}

/// Reserve a deterministic report identity before any case admission.
pub(crate) async fn reserve_authoring_report(
    authority: &InternalDevAdmin,
    client: &Client,
    source_schema: &ScenarioSchemaName,
    reservation: &AuthoringReportReservation<'_>,
) -> anyhow::Result<AuthoringReportState> {
    for (value, name) in [
        (reservation.tenant_id, "tenant-id"),
        (reservation.report_id, "report-id"),
        (reservation.execution_id, "execution-id"),
        (reservation.flow_id, "flow-id"),
        (reservation.suite_id, "suite-id"),
    ] {
        validate_identity(value, name)?;
    }
    let (command_json, command_hash) = canonical_json(reservation.command)?;
    let (lineage_json, lineage_hash) = canonical_json(reservation.lineage)?;
    client
        .query_opt(
            wamn_scenario_catalog::authoring::insert_authoring_report_reservation_sql(),
            &[
                &reservation.tenant_id,
                &reservation.report_id,
                &reservation.execution_id,
                &reservation.flow_id,
                &reservation.suite_flow_version,
                &reservation.suite_id,
                &command_json,
                &command_hash,
                &lineage_json,
                &lineage_hash,
            ],
        )
        .await
        .context("reserve immutable authoring report identity")?;
    let stored = select_reservation(client, reservation.tenant_id, reservation.report_id)
        .await?
        .context("authoring report reservation disappeared after insert")?;
    if stored.execution_id != reservation.execution_id
        || stored.flow_id != reservation.flow_id
        || stored.suite_flow_version != reservation.suite_flow_version
        || stored.suite_id != reservation.suite_id
        || stored.command != *reservation.command
        || stored.command_hash != command_hash
        || stored.lineage != *reservation.lineage
        || stored.lineage_hash != lineage_hash
    {
        bail!("report identity is already reserved for a different authoring command");
    }
    authoring_report(
        authority,
        client,
        source_schema,
        &AuthoringReportQuery {
            tenant_id: reservation.tenant_id.to_string(),
            report_id: reservation.report_id.to_string(),
        },
    )
    .await
}

/// Append one observed case fact beneath a pending reservation.
pub(crate) async fn append_authoring_case_fact(
    _authority: &InternalDevAdmin,
    client: &Client,
    tenant_id: &str,
    report_id: &str,
    ordinal: usize,
    fact: &AuthoringCaseReport,
) -> anyhow::Result<()> {
    let ordinal = i32::try_from(ordinal).context("report case ordinal exceeds i32")?;
    let fail_kind = fact.fail_kind.map(fail_kind_sql);
    let outcome_json = serde_json::to_string(&fact.outcome)?;
    client
        .query_opt(
            wamn_scenario_catalog::authoring::insert_authoring_suite_case_fact_sql(),
            &[
                &tenant_id,
                &report_id,
                &ordinal,
                &fact.case_id,
                &fact.run_id,
                &fact.passed,
                &run_status_sql(fact.status),
                &fail_kind,
                &fact.fail_node,
                &outcome_json,
            ],
        )
        .await
        .with_context(|| format!("append immutable case fact {:?}", fact.case_id))?;
    let reservation = select_reservation(client, tenant_id, report_id)
        .await?
        .context("case fact has no report reservation")?;
    if reservation.state != "pending" {
        bail!("cannot append a case fact after report finalization");
    }
    let facts = select_case_facts(client, tenant_id, report_id, &reservation.command).await?;
    let stored = facts
        .get(usize::try_from(ordinal).expect("non-negative i32 fits usize"))
        .context("case fact insert conflicted without the expected stored fact")?;
    if stored != fact {
        bail!("stored case fact differs from the observed result");
    }
    Ok(())
}

/// Finalize one immutable summary from its already appended fact prefix.
pub(crate) async fn finalize_authoring_report(
    authority: &InternalDevAdmin,
    client: &mut Client,
    source_schema: &ScenarioSchemaName,
    report: &AuthoringReport,
) -> anyhow::Result<()> {
    let query = AuthoringReportQuery {
        tenant_id: report.tenant_id.clone(),
        report_id: report.report_id.clone(),
    };
    match authoring_report(authority, client, source_schema, &query).await? {
        AuthoringReportState::NotFound => bail!("cannot finalize an unreserved report"),
        AuthoringReportState::Finalized(existing) if existing == *report => return Ok(()),
        AuthoringReportState::Finalized(_) => {
            bail!("finalized authoring report differs from the requested summary")
        }
        AuthoringReportState::Pending(pending) => {
            if matches!(
                pending.reason,
                PendingAuthoringReportReason::CaptureInterrupted { .. }
            ) {
                bail!("capture-interrupted authoring report must remain pending");
            }
            if pending.captured_cases != report.cases {
                bail!("final report cases differ from the immutable captured fact prefix");
            }
        }
    }
    let (lineage_json, lineage_hash) = canonical_json(&report.lineage)?;
    let refusal_json = report
        .refusal
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let edit_to_run_ms = report
        .edit_to_run_ms
        .map(i64::try_from)
        .transpose()
        .context("edit-to-run latency exceeds i64")?;
    let transaction = client
        .transaction()
        .await
        .context("begin immutable authoring report finalization")?;
    let locked_state: String = transaction
        .query_opt(
            wamn_scenario_catalog::authoring::lock_authoring_report_reservation_state_sql(),
            &[&report.tenant_id, &report.report_id],
        )
        .await
        .context("lock authoring report reservation for finalization")?
        .context("authoring report reservation disappeared during finalization")?
        .get(0);
    if locked_state == "finalized" {
        transaction
            .commit()
            .await
            .context("release finalized authoring report reservation lock")?;
        return match authoring_report(authority, client, source_schema, &query).await? {
            AuthoringReportState::Finalized(existing) if existing == *report => Ok(()),
            AuthoringReportState::Finalized(_) => {
                bail!("concurrent finalized report differs from the requested summary")
            }
            _ => bail!("finalized reservation lost its immutable report summary"),
        };
    }
    if locked_state != "pending" {
        bail!("unknown locked authoring report state {locked_state:?}");
    }
    transaction
        .execute(
            wamn_scenario_catalog::authoring::insert_authoring_suite_report_sql(),
            &[
                &report.tenant_id,
                &report.report_id,
                &report.execution_id,
                &report.flow_id,
                &report.suite_flow_version,
                &report.suite_id,
                &report.passed,
                &lineage_json,
                &lineage_hash,
                &edit_to_run_ms,
                &refusal_json,
            ],
        )
        .await
        .context("persist immutable authoring suite summary")?;
    let finalized = transaction
        .execute(
            wamn_scenario_catalog::authoring::finalize_authoring_report_reservation_sql(),
            &[&report.tenant_id, &report.report_id],
        )
        .await
        .context("finalize immutable authoring report reservation")?;
    if finalized != 1 {
        bail!("authoring report reservation was not pending during finalization");
    }
    transaction
        .commit()
        .await
        .context("commit immutable authoring report finalization")?;
    Ok(())
}

async fn finalized_report(
    client: &Client,
    query: &AuthoringReportQuery,
    reservation: &StoredAuthoringReservation,
    cases: Vec<AuthoringCaseReport>,
) -> anyhow::Result<AuthoringReport> {
    let row = client
        .query_opt(
            wamn_scenario_catalog::authoring::select_authoring_suite_report_sql(),
            &[&query.tenant_id, &query.report_id],
        )
        .await
        .context("read finalized authoring suite report")?
        .context("finalized reservation has no immutable suite summary")?;
    let execution_id: String = row.get(0);
    let flow_id: String = row.get(1);
    let suite_flow_version: i32 = row.get(2);
    let suite_id: String = row.get(3);
    let stored_passed: bool = row.get(4);
    let lineage_json: String = row.get(5);
    let lineage_value: serde_json::Value =
        serde_json::from_str(&lineage_json).context("parse finalized report lineage")?;
    if wamn_flow::canonical_json_sha256(&lineage_value) != reservation.lineage_hash {
        bail!("final report lineage does not match its reservation hash");
    }
    let lineage: ExecutionLineage = serde_json::from_value(lineage_value)?;
    if execution_id != reservation.execution_id
        || flow_id != reservation.flow_id
        || suite_flow_version != reservation.suite_flow_version
        || suite_id != reservation.suite_id
        || lineage != reservation.lineage
    {
        bail!("final report identity differs from its immutable reservation");
    }
    let edit_to_run_ms: Option<i64> = row.get(6);
    let refusal_json: Option<String> = row.get(7);
    let refusal: Option<ScenarioRefusal> = refusal_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?;
    let report = AuthoringReport::new(
        &query.report_id,
        execution_id,
        &query.tenant_id,
        flow_id,
        suite_flow_version,
        suite_id,
        lineage,
        edit_to_run_ms
            .map(u64::try_from)
            .transpose()
            .context("negative edit-to-run milliseconds")?,
        refusal,
        cases,
    );
    if report.passed != stored_passed {
        bail!("stored suite pass/fail disagrees with its immutable case outcomes");
    }
    Ok(report)
}

/// Read one missing, pending, or finalized report without exposing mutation.
pub(crate) async fn authoring_report(
    _authority: &InternalDevAdmin,
    client: &Client,
    source_schema: &ScenarioSchemaName,
    query: &AuthoringReportQuery,
) -> anyhow::Result<AuthoringReportState> {
    let Some(reservation) = select_reservation(client, &query.tenant_id, &query.report_id).await?
    else {
        return Ok(AuthoringReportState::NotFound);
    };
    if reservation.command.source_schema != source_schema.as_str() {
        bail!("reserved authoring command names a different fixed source schema");
    }
    let cases = select_case_facts(
        client,
        &query.tenant_id,
        &query.report_id,
        &reservation.command,
    )
    .await?;
    match reservation.state.as_str() {
        "finalized" => Ok(AuthoringReportState::Finalized(
            finalized_report(client, query, &reservation, cases).await?,
        )),
        "pending" => {
            let mut admitted = Vec::new();
            for expected in &reservation.command.cases[cases.len()..] {
                let execution_schema = ScenarioSchemaName::new(&expected.execution_schema)
                    .context("reserved command has an invalid execution schema")?;
                let sql = format!(
                    "SELECT run_id FROM {}.runs \
                     WHERE tenant_id = pg_catalog.current_setting('app.tenant', true) \
                       AND run_id = $1",
                    execution_schema.as_str()
                );
                if client
                    .query_opt(&sql, &[&expected.run_id])
                    .await
                    .context("detect admitted authoring run without a captured fact")?
                    .is_some()
                {
                    admitted.push(expected.run_id.clone());
                }
            }
            let reason = if admitted.is_empty() {
                PendingAuthoringReportReason::AwaitingAdmission
            } else {
                PendingAuthoringReportReason::CaptureInterrupted { run_ids: admitted }
            };
            Ok(AuthoringReportState::Pending(pending_report(
                query,
                &reservation,
                reason,
                cases,
            )))
        }
        other => bail!("unknown authoring report reservation state {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_hash_is_bound_to_actual_flowrunner_bytes() {
        let first = sha256(b"flowrunner-a");
        let second = sha256(b"flowrunner-b");
        assert_ne!(first, second);
        assert!(first.starts_with("sha256:"));
    }

    #[test]
    fn internal_adapter_carries_no_client_principal() {
        let source = include_str!("authoring.rs");
        let token = source
            .split("pub struct InternalDevAdmin")
            .nth(1)
            .unwrap()
            .split("impl InternalDevAdmin")
            .next()
            .unwrap();
        assert!(!token.contains("principal"));
        assert!(!token.contains("user_id"));
        assert!(!token.contains("subject"));
    }

    #[test]
    fn authoring_probe_rejects_app_authority_and_every_protected_ownership_plane() {
        assert!(
            AUTHORING_ROLE_PROBE_SQL
                .contains("NOT pg_catalog.pg_has_role(session_user, 'wamn_app', 'MEMBER')")
        );
        assert!(
            AUTHORING_ROLE_PROBE_SQL
                .contains("NOT pg_catalog.pg_has_role(session_user, 'wamn_app', 'USAGE')")
        );
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("pg_catalog.pg_database"));
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("pg_catalog.pg_namespace"));
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("relation.relowner = session_role.oid"));
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("routine.proowner = session_role.oid"));
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("pg_catalog.has_any_column_privilege"));
        assert!(
            !AUTHORING_ROLE_PROBE_SQL
                .to_ascii_uppercase()
                .contains("SET ROLE")
        );
    }

    #[test]
    fn stored_suite_version_is_distinct_from_the_proposed_draft_version() {
        let proposed_v8 = wamn_flow::Flow::from_json(
            r#"{
              "schema-version":"0.1","flow-id":"flow-a","version":8,
              "nodes":[
                {"id":"request","type":"request","config":{"input-schema":true}},
                {"id":"respond","type":"respond","config":{"status":200}}
              ],
              "edges":[{"from":"request","to":"respond"}]
            }"#,
        )
        .unwrap();

        assert_eq!(proposed_v8.version, 8);
        assert!(validate_workspace_flow_identity(&proposed_v8, "flow-a").is_ok());
        let source_suite_version = 7;
        assert_ne!(proposed_v8.version, source_suite_version);
    }
}
