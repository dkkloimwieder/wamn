//! Internal development-administrator adapter for the flow-draft loop.
//!
//! There is intentionally no CLI or public transport in this module. Item 5
//! owns retained client identity and client-facing authorization; this adapter
//! proves the shared typed command/query boundary first.

use std::collections::BTreeSet;
use std::time::SystemTime;

use anyhow::{Context as _, bail};
use tokio_postgres::{Client, NoTls};

#[cfg(test)]
use wamn_catalog::entry_input_schema_hash;
use wamn_catalog::{
    CALLABLE_CONTRACT_VERSION, CallFlowInstruction, CallableContract, CallableEffectCeiling,
    CallableReturnContract, DraftArtifact, ExecutionConnectionRequirement, ExecutionEffectPolicy,
    ExecutionNodeId, ExecutionPlanBody, ExecutionPlanEdge, ExecutionPlanNode, ExecutionPlanV2,
    ExecutionRuntimeRevision, ExecutionSourceMapEntry, RootTerminalBehavior,
    ValidatedDraftIdentity, ValidatedDraftIdentityInput, execution_bundle_hash,
    read_execution_plan,
};
use wamn_execution_host::TrustedExecutionRuntimeRevision;
use wamn_schema_control::BareSchemaName;

use crate::store::drafts;
#[cfg(test)]
use crate::store::sha256;

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
           ('catalog', 'execution_bundles', 'INSERT'), \
           ('catalog', 'draft_safe_connection_grants', 'INSERT'), \
           ('catalog', 'draft_safe_connection_grants', 'UPDATE'), \
           ('catalog', 'authoring_command_audit', 'INSERT'), \
           ($1::text, 'authoring_test_run_reservations', 'INSERT'), \
           ($1::text, 'authoring_test_run_reservations', 'UPDATE'), \
           ($1::text, 'authoring_test_case_runs', 'INSERT'), \
           ($1::text, 'authoring_test_case_runs', 'UPDATE'), \
           ($1::text, 'authoring_test_reports', 'INSERT'), \
           ($1::text, 'authoring_test_sets', 'INSERT') \
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
       pg_catalog.has_table_privilege(current_user, 'catalog.execution_bundles', 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, 'catalog.execution_bundles', 'INSERT') \
         AND NOT pg_catalog.has_table_privilege(current_user, 'catalog.execution_bundles', 'UPDATE') \
         AND NOT pg_catalog.has_table_privilege(current_user, 'catalog.execution_bundles', 'DELETE'), \
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
         AND pg_catalog.has_table_privilege(current_user, $3, 'INSERT') \
         AND pg_catalog.has_table_privilege(current_user, $3, 'UPDATE'), \
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
       pg_catalog.has_table_privilege(current_user, $7, 'SELECT') \
         AND pg_catalog.has_table_privilege(current_user, $7, 'INSERT'), \
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
/// validated run-plane schema. The private capability token is never returned
/// to callers.
pub struct InternalAuthoringBackend {
    authority: InternalDevAdmin,
    client: Client,
    connection_task: tokio::task::JoinHandle<()>,
    tenant_id: Box<str>,
    source_schema: BareSchemaName,
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
        let source_schema = BareSchemaName::new(source_schema.into())
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
        let reservations = qualified("authoring_test_run_reservations");
        let case_runs = qualified("authoring_test_case_runs");
        let reports = qualified("authoring_test_reports");
        let test_sets = qualified("authoring_test_sets");
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
                    &reservations,
                    &case_runs,
                    &reports,
                    &lock_catalog_head,
                    &runs,
                    &test_sets,
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
        let backend = Self {
            authority: InternalDevAdmin::at_process_boundary(),
            client,
            connection_task,
            tenant_id: tenant_id.into_boxed_str(),
            source_schema,
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

/// Command to validate one exact saved draft revision.
#[derive(Clone, Debug)]
pub struct ValidateFlowDraft {
    pub tenant_id: String,
    pub draft_id: String,
    pub draft_revision: i64,
    pub catalog_id: String,
    pub catalog_version: i32,
    pub environment: String,
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
    CallFlowCalleeNotFound { site: String, flow_id: String },
    CallFlowNotCallable { site: String, flow_id: String },
    CallFlowContractIncompatible { site: String, flow_id: String },
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
            Self::CallFlowCalleeNotFound { site, flow_id } => {
                write!(
                    formatter,
                    "call-flow site {site:?} resolves no flow {flow_id:?}"
                )
            }
            Self::CallFlowNotCallable { site, flow_id } => {
                write!(
                    formatter,
                    "call-flow site {site:?} target {flow_id:?} has no recorded callable contract"
                )
            }
            Self::CallFlowContractIncompatible { site, flow_id } => {
                write!(
                    formatter,
                    "call-flow site {site:?} target {flow_id:?} has an incompatible callable contract"
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
                drafts::insert_flow_draft_sql(),
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
                drafts::update_flow_draft_sql(),
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
) -> Result<Vec<wamn_flow::node_contract::NodeInterface>, DraftRunRefusal> {
    let node_types: BTreeSet<_> = flow
        .nodes
        .iter()
        .map(|node| node.node_type.as_str())
        .collect();
    let mut unresolved = Vec::new();
    let mut implementations = Vec::with_capacity(node_types.len());
    for node_type in node_types {
        if node_type == "call-flow" {
            continue;
        }
        let Some(interface) = wamn_standard_nodes::describe_interface(node_type) else {
            unresolved.push(node_type.to_string());
            continue;
        };
        implementations.push(interface.clone());
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

struct CompiledExecutionPlan {
    plan: ExecutionPlanV2,
    exact_bytes: Vec<u8>,
    execution_bundle_hash: String,
}

/// Compile one deterministic exact-byte own-flow execution plan.
fn compile_root_execution_plan(
    flow: &wamn_flow::Flow,
    root_artifact_hash: &str,
    runtime_revision: ExecutionRuntimeRevision,
) -> anyhow::Result<CompiledExecutionPlan> {
    let node_id = |value: &str| ExecutionNodeId::new(value);
    let entry = flow
        .nodes
        .iter()
        .find(|node| node.entry_kind().is_some())
        .context("validated flow has no entry node")?;
    let entry_instruction = node_id(&entry.id)?;
    let entry_input_schema_guard = match entry.entry_kind().expect("entry node membership checked")
    {
        wamn_flow::EntryKind::Request => {
            serde_json::from_value::<wamn_flow::RequestConfig>(entry.config.clone())
                .context("validated request entry config is invalid")?
                .input_schema
        }
        wamn_flow::EntryKind::Event => serde_json::Value::Bool(true),
    };
    let mut semantic_requirements = flow.connection_requirements.iter().collect::<Vec<_>>();
    semantic_requirements.sort_by(|left, right| left.name.cmp(&right.name));
    let mut semantic_nodes = flow.nodes.iter().collect::<Vec<_>>();
    semantic_nodes.sort_by(|left, right| left.id.cmp(&right.id));
    let nodes = semantic_nodes
        .iter()
        .map(|node| {
            if node.node_type == "call-flow" {
                let config =
                    serde_json::from_value::<wamn_flow::CallFlowConfig>(node.config.clone())
                        .with_context(|| {
                            format!("validated call-flow node {:?} has invalid config", node.id)
                        })?;
                return Ok(ExecutionPlanNode {
                    local_node_id: node_id(&node.id)?,
                    source_node_id: node.id.clone(),
                    node_type: "call-flow".to_string(),
                    config: serde_json::json!({
                        "site": node.id.clone(),
                        "flow-id": config.flow_id,
                    }),
                    effect_policy: ExecutionEffectPolicy::Effectful,
                    source_connection_requirement: None,
                });
            }
            let interface =
                wamn_standard_nodes::describe_interface(&node.node_type).with_context(|| {
                    format!("validated node type {:?} is not standard", node.node_type)
                })?;
            let source_connection_requirement = node
                .connection
                .as_ref()
                .map(|name| {
                    let requirement = semantic_requirements
                        .binary_search_by(|candidate| candidate.name.as_str().cmp(name.as_str()))
                        .ok()
                        .map(|index| semantic_requirements[index])
                        .with_context(|| {
                            format!("node {:?} has unresolved connection {name:?}", node.id)
                        })?;
                    Ok::<_, anyhow::Error>(ExecutionConnectionRequirement {
                        name: name.clone(),
                        descriptor: requirement.requirement.clone(),
                    })
                })
                .transpose()?;
            Ok(ExecutionPlanNode {
                local_node_id: node_id(&node.id)?,
                source_node_id: node.id.clone(),
                node_type: node.node_type.clone(),
                config: node.config.clone(),
                effect_policy: match interface.effect_policy {
                    wamn_flow::node_contract::EffectPolicy::Pure => ExecutionEffectPolicy::Pure,
                    wamn_flow::node_contract::EffectPolicy::Effectful => {
                        ExecutionEffectPolicy::Effectful
                    }
                },
                source_connection_requirement,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut semantic_edges = flow.edges.iter().collect::<Vec<_>>();
    semantic_edges.sort_by(|left, right| {
        (
            left.from.as_str(),
            left.from_port.as_str(),
            left.ordinal.unwrap_or(0),
            left.to.as_str(),
            left.to_port.as_deref().unwrap_or(""),
        )
            .cmp(&(
                right.from.as_str(),
                right.from_port.as_str(),
                right.ordinal.unwrap_or(0),
                right.to.as_str(),
                right.to_port.as_deref().unwrap_or(""),
            ))
    });
    let edges = semantic_edges
        .iter()
        .map(|edge| {
            Ok(ExecutionPlanEdge {
                source: node_id(&edge.from)?,
                source_port: edge.from_port.clone(),
                destination: node_id(&edge.to)?,
                destination_port: edge.to_port.clone(),
                fan_out_ordinal: edge.ordinal.unwrap_or(0),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let responder_ids = flow
        .nodes
        .iter()
        .filter(|node| node.node_type == "respond")
        .map(|node| node_id(&node.id).map_err(anyhow::Error::new))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let root_terminal_behavior = if responder_ids.is_empty() {
        RootTerminalBehavior::FrontierExhaustion
    } else {
        RootTerminalBehavior::Respond {
            responders: responder_ids,
        }
    };
    let source_map = nodes
        .iter()
        .map(|node| ExecutionSourceMapEntry {
            local_node_id: node.local_node_id.clone(),
            source_node_id: node.source_node_id.clone(),
        })
        .collect();
    let mut body = ExecutionPlanBody {
        entry_instruction,
        nodes,
        edges,
        root_terminal_behavior,
        entry_input_schema_guard,
        callable_contract: None,
        source_map,
    };
    let probe = ExecutionPlanV2 {
        header: wamn_catalog::ExecutionPlanHeader {
            format_version: wamn_catalog::EXECUTION_PLAN_FORMAT_VERSION.to_string(),
            plan_compiler_revision: wamn_catalog::PLAN_COMPILER_REVISION.to_string(),
            runtime_revision: runtime_revision.clone(),
            root_artifact_hash: root_artifact_hash.to_string(),
        },
        body: body.clone(),
    };
    body.callable_contract = probe.derived_callable_contract()?;
    let plan = ExecutionPlanV2::new(runtime_revision, root_artifact_hash, body)
        .map_err(anyhow::Error::new)?;
    let exact_bytes = serde_json::to_vec(&plan).context("serialize exact execution bundle")?;
    let execution_bundle_hash = execution_bundle_hash(&exact_bytes);
    Ok(CompiledExecutionPlan {
        plan,
        exact_bytes,
        execution_bundle_hash,
    })
}

async fn validate_call_flow_callees(
    transaction: &tokio_postgres::Transaction<'_>,
    request: &ValidateFlowDraft,
    own_flow_id: &str,
    plan: &ExecutionPlanV2,
) -> anyhow::Result<Result<(), DraftRunRefusal>> {
    for node in &plan.body.nodes {
        if node.node_type != "call-flow" {
            continue;
        }
        let instruction = serde_json::from_value::<CallFlowInstruction>(node.config.clone())
            .context("validated call-flow instruction failed to parse")?;
        if let Some(result) = validate_own_call_flow_contract(
            &instruction,
            own_flow_id,
            plan.body.callable_contract.as_ref(),
        ) {
            if let Err(refusal) = result {
                return Ok(Err(refusal));
            }
            continue;
        }
        let row = transaction
            .query_opt(
                drafts::select_call_flow_callee_plan_sql(),
                &[
                    &request.tenant_id,
                    &request.catalog_id,
                    &request.catalog_version,
                    &instruction.flow_id,
                ],
            )
            .await
            .with_context(|| {
                format!(
                    "resolve call-flow callee {:?} in catalog {:?} version {}",
                    instruction.flow_id, request.catalog_id, request.catalog_version
                )
            })?;
        let Some(row) = row else {
            return Ok(Err(call_flow_callee_not_found(&instruction)));
        };
        let execution_bundle_hash: String = row.get(1);
        let exact_bytes: Vec<u8> = row.get(2);
        let callee_plan = match read_execution_plan(&execution_bundle_hash, &exact_bytes) {
            Ok(plan) => plan,
            Err(error) => {
                return Ok(Err(DraftRunRefusal::InvalidDraft {
                    detail: format!(
                        "call-flow site {:?} target {:?} has invalid execution plan: {error}",
                        instruction.site, instruction.flow_id
                    ),
                }));
            }
        };
        if let Err(refusal) =
            validate_call_flow_contract(&instruction, callee_plan.body.callable_contract.as_ref())
        {
            return Ok(Err(refusal));
        }
    }
    Ok(Ok(()))
}

fn validate_own_call_flow_contract(
    instruction: &CallFlowInstruction,
    own_flow_id: &str,
    contract: Option<&CallableContract>,
) -> Option<Result<(), DraftRunRefusal>> {
    (instruction.flow_id == own_flow_id).then(|| validate_call_flow_contract(instruction, contract))
}

fn call_flow_callee_not_found(instruction: &CallFlowInstruction) -> DraftRunRefusal {
    DraftRunRefusal::CallFlowCalleeNotFound {
        site: instruction.site.to_string(),
        flow_id: instruction.flow_id.clone(),
    }
}

fn validate_call_flow_contract(
    instruction: &CallFlowInstruction,
    contract: Option<&CallableContract>,
) -> Result<(), DraftRunRefusal> {
    let Some(contract) = contract else {
        return Err(DraftRunRefusal::CallFlowNotCallable {
            site: instruction.site.to_string(),
            flow_id: instruction.flow_id.clone(),
        });
    };
    if contract.version != CALLABLE_CONTRACT_VERSION
        || contract.return_contract != CallableReturnContract::UntypedJsonBody
        || contract.effect_ceiling != CallableEffectCeiling::Effectful
    {
        return Err(DraftRunRefusal::CallFlowContractIncompatible {
            site: instruction.site.to_string(),
            flow_id: instruction.flow_id.clone(),
        });
    }
    Ok(())
}

pub(crate) async fn validate_flow_draft(
    _authority: &InternalDevAdmin,
    client: &mut Client,
    request: &ValidateFlowDraft,
    flowrunner_bytes: &[u8],
) -> anyhow::Result<Result<ValidatedDraftPin, DraftRunRefusal>> {
    let row = client
        .query_opt(
            drafts::select_flow_draft_sql(),
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
    let artifact =
        match wamn_catalog::Artifact::new(&request.tenant_id, &flow, implementations.clone()) {
            Ok(artifact) => artifact,
            Err(error) => {
                return Ok(Err(DraftRunRefusal::InvalidDraft {
                    detail: error.to_string(),
                }));
            }
        };
    let compiled_plan = compile_root_execution_plan(
        &flow,
        artifact.identity().artifact_hash().as_str(),
        TrustedExecutionRuntimeRevision::from_flowrunner_bytes(flowrunner_bytes)
            .execution_runtime_revision(),
    )?;
    let draft = match DraftArtifact::new(
        &request.tenant_id,
        &flow,
        implementations,
        compiled_plan.plan.clone(),
    ) {
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
            drafts::lock_draft_catalog_head_sql(),
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
            drafts::select_draft_catalog_source_member_sql(),
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.environment,
                &request.catalog_version,
                &flow_id,
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

    if let Err(refusal) =
        validate_call_flow_callees(&transaction, request, &flow_id, draft.execution_plan()).await?
    {
        transaction.rollback().await?;
        return Ok(Err(refusal));
    }

    let artifact = draft.artifact();
    let runtime_flow_version = i32::try_from(flow.version).context("flow version exceeds i32")?;
    let catalog_version =
        u32::try_from(request.catalog_version).context("catalog version must be a positive u32")?;
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
        execution_bundle_hash: &compiled_plan.execution_bundle_hash,
        catalog_id: &request.catalog_id,
        catalog_version,
        environment: &request.environment,
        binding_base_artifact_hash: &binding_base_artifact_hash,
    })?;
    let graph_json = flow.to_json();
    transaction
        .execute(
            wamn_schema_control::sql::insert_execution_bundle_sql(),
            &[
                &request.tenant_id,
                &compiled_plan.execution_bundle_hash,
                &compiled_plan.exact_bytes,
            ],
        )
        .await
        .context("persist exact execution bundle")?;
    let inserted = transaction
        .query_opt(
            drafts::insert_validated_flow_draft_sql(),
            &[
                &request.tenant_id,
                &request.draft_id,
                &request.draft_revision,
                &draft.content_hash().as_str(),
                &request.catalog_id,
                &request.catalog_version,
                &request.environment,
                &runtime_flow_version,
                &graph_json,
                &artifact.graph_hash(),
                &artifact.identity().artifact_hash().as_str(),
                &compiled_plan.execution_bundle_hash,
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
                drafts::select_validated_flow_draft_sql(),
                &[
                    &request.tenant_id,
                    &request.draft_id,
                    &request.draft_revision,
                    &draft.content_hash().as_str(),
                    &request.catalog_id,
                    &request.catalog_version,
                    &request.environment,
                    &runtime_flow_version,
                    &artifact.identity().artifact_hash().as_str(),
                    &compiled_plan.execution_bundle_hash,
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
        runtime_flow_version,
        catalog_id: request.catalog_id.clone(),
        catalog_version: request.catalog_version,
        environment: request.environment.clone(),
        execution_bundle_hash: compiled_plan.execution_bundle_hash,
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
            drafts::grant_draft_safe_generation_sql(),
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
            drafts::revoke_draft_safe_generation_sql(),
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
    fn draft_validation_runtime_revision_uses_trusted_host_derivation() {
        let trusted = TrustedExecutionRuntimeRevision::from_flowrunner_bytes(b"flowrunner-a");

        assert_eq!(
            trusted.execution_runtime_revision(),
            ExecutionRuntimeRevision {
                flowrunner_component_digest: sha256(b"flowrunner-a"),
                effect_provider_revision: trusted.effect_provider_revision().to_string(),
                host_effect_contract_version: trusted.host_effect_contract_version().to_string(),
            }
        );
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
        assert!(AUTHORING_ROLE_PROBE_SQL.contains("($1::text, 'authoring_test_sets', 'INSERT')"));
        assert!(
            AUTHORING_ROLE_PROBE_SQL
                .contains("pg_catalog.has_table_privilege(current_user, $7, 'SELECT')")
        );
        assert!(
            AUTHORING_ROLE_PROBE_SQL
                .contains("pg_catalog.has_table_privilege(current_user, $7, 'INSERT')")
        );
        assert!(
            AUTHORING_ROLE_PROBE_SQL
                .contains("pg_catalog.has_table_privilege(current_user, $3, 'UPDATE')")
        );
        assert!(!AUTHORING_ROLE_PROBE_SQL.contains("($1::text, 'authoring_test_sets', 'UPDATE')"));
        for allowed in [
            "($1::text, 'authoring_test_run_reservations', 'INSERT')",
            "($1::text, 'authoring_test_run_reservations', 'UPDATE')",
            "($1::text, 'authoring_test_case_runs', 'INSERT')",
            "($1::text, 'authoring_test_case_runs', 'UPDATE')",
            "($1::text, 'authoring_test_reports', 'INSERT')",
        ] {
            assert!(
                AUTHORING_ROLE_PROBE_SQL.contains(allowed),
                "missing {allowed}"
            );
        }
        for forbidden in [
            "($1::text, 'authoring_test_run_reservations', 'DELETE')",
            "($1::text, 'authoring_test_case_runs', 'DELETE')",
            "($1::text, 'authoring_test_reports', 'UPDATE')",
            "($1::text, 'authoring_test_reports', 'DELETE')",
        ] {
            assert!(
                !AUTHORING_ROLE_PROBE_SQL.contains(forbidden),
                "unexpected {forbidden}"
            );
        }
        assert!(
            !AUTHORING_ROLE_PROBE_SQL
                .to_ascii_uppercase()
                .contains("SET ROLE")
        );
    }

    #[test]
    fn call_flow_refusals_are_typed_and_self_reference_skips_release_lookup() {
        let instruction = CallFlowInstruction {
            site: ExecutionNodeId::new("self-call").unwrap(),
            flow_id: "caller".into(),
        };
        let contract = CallableContract {
            version: CALLABLE_CONTRACT_VERSION.into(),
            input_schema_hash: entry_input_schema_hash(&serde_json::json!(true)),
            return_contract: CallableReturnContract::UntypedJsonBody,
            effect_ceiling: CallableEffectCeiling::Effectful,
        };

        assert_eq!(
            validate_own_call_flow_contract(&instruction, "caller", Some(&contract)),
            Some(Ok(()))
        );
        assert_eq!(
            validate_own_call_flow_contract(&instruction, "other", Some(&contract)),
            None,
            "non-self callees fall through to the pinned release lookup"
        );
        assert!(matches!(
            call_flow_callee_not_found(&instruction),
            DraftRunRefusal::CallFlowCalleeNotFound { .. }
        ));
        assert!(matches!(
            validate_call_flow_contract(&instruction, None),
            Err(DraftRunRefusal::CallFlowNotCallable { .. })
        ));

        let incompatible = CallableContract {
            version: "foreign".into(),
            ..contract
        };
        assert!(matches!(
            validate_call_flow_contract(&instruction, Some(&incompatible)),
            Err(DraftRunRefusal::CallFlowContractIncompatible { .. })
        ));
    }

    #[test]
    fn call_flow_resolution_is_after_exact_environment_head_lock() {
        let source = include_str!("authoring.rs");
        let validate = source
            .split("pub(crate) async fn validate_flow_draft")
            .nth(1)
            .unwrap()
            .split("pub(crate) async fn grant_draft_safe_generation")
            .next()
            .unwrap();
        let lock = validate.find("lock_draft_catalog_head_sql").unwrap();
        let source_member = validate
            .find("select_draft_catalog_source_member_sql")
            .unwrap();
        let call_flow = validate.find("validate_call_flow_callees").unwrap();

        assert!(lock < source_member);
        assert!(source_member < call_flow);
        assert!(validate.contains("&request.environment"));
        assert!(validate.contains("locked_catalog_version != Some(request.catalog_version)"));
    }

    fn test_runtime_revision() -> ExecutionRuntimeRevision {
        let digest_a = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let digest_b = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        ExecutionRuntimeRevision {
            flowrunner_component_digest: digest_b.into(),
            effect_provider_revision: digest_a.into(),
            host_effect_contract_version: "0.1".into(),
        }
    }

    fn test_root_artifact_hash() -> &'static str {
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }

    fn hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        out
    }

    fn golden_flow() -> wamn_flow::Flow {
        wamn_flow::Flow::from_json(
            r#"{
              "schema-version":"0.1","flow-id":"golden-flow","version":1,
              "nodes":[
                {"id":"respond","type":"respond","config":{"status":200}},
                {"id":"transform","type":"transform","config":{"expression":"@"}},
                {"id":"request","type":"request","config":{"input-schema":{"type":"object"}}}
              ],
              "edges":[
                {"from":"transform","to":"respond","ordinal":7},
                {"from":"request","to":"transform","ordinal":3}
              ]
            }"#,
        )
        .unwrap()
    }

    fn compiled_golden_flow() -> CompiledExecutionPlan {
        compile_root_execution_plan(
            &golden_flow(),
            test_root_artifact_hash(),
            test_runtime_revision(),
        )
        .unwrap()
    }

    #[test]
    fn deterministic_plan_compiler_sorts_unordered_nodes_and_ordinal_keyed_edges() {
        let first = wamn_flow::Flow::from_json(
            r#"{
              "schema-version":"0.1","flow-id":"permuted","version":1,
              "nodes":[
                {"id":"respond","type":"respond","config":{"status":200}},
                {"id":"transform","type":"transform","config":{"expression":"@"}},
                {"id":"request","type":"request","config":{"input-schema":true}}
              ],
              "edges":[
                {"from":"transform","to":"respond","ordinal":9},
                {"from":"request","to":"respond","ordinal":2},
                {"from":"request","to":"transform","ordinal":1}
              ]
            }"#,
        )
        .unwrap();
        let second = wamn_flow::Flow::from_json(
            r#"{
              "schema-version":"0.1","flow-id":"permuted","version":1,
              "edges":[
                {"from":"request","to":"transform","ordinal":1},
                {"from":"transform","to":"respond","ordinal":9},
                {"from":"request","to":"respond","ordinal":2}
              ],
              "nodes":[
                {"id":"request","type":"request","config":{"input-schema":true}},
                {"id":"respond","type":"respond","config":{"status":200}},
                {"id":"transform","type":"transform","config":{"expression":"@"}}
              ]
            }"#,
        )
        .unwrap();

        let left =
            compile_root_execution_plan(&first, test_root_artifact_hash(), test_runtime_revision())
                .unwrap();
        let right = compile_root_execution_plan(
            &second,
            test_root_artifact_hash(),
            test_runtime_revision(),
        )
        .unwrap();

        assert_eq!(left.exact_bytes, right.exact_bytes);
        assert_eq!(left.execution_bundle_hash, right.execution_bundle_hash);
        assert_eq!(
            left.plan
                .body
                .nodes
                .iter()
                .map(|node| node.local_node_id.as_str())
                .collect::<Vec<_>>(),
            ["request", "respond", "transform"]
        );
        assert_eq!(
            left.plan
                .body
                .edges
                .iter()
                .map(|edge| {
                    (
                        edge.source.as_str(),
                        edge.destination.as_str(),
                        edge.fan_out_ordinal,
                    )
                })
                .collect::<Vec<_>>(),
            [
                ("request", "transform", 1),
                ("request", "respond", 2),
                ("transform", "respond", 9)
            ]
        );
    }

    #[test]
    fn deterministic_plan_compiler_golden_bytes_and_hash_are_exact() {
        let compiled = compiled_golden_flow();
        let expected_bytes = br#"{"header":{"format-version":"0.1","plan-compiler-revision":"0.1","runtime-revision":{"flowrunner-component-digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","effect-provider-revision":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","host-effect-contract-version":"0.1"},"root-artifact-hash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"body":{"entry-instruction":"request","nodes":[{"local-node-id":"request","source-node-id":"request","type":"request","config":{"input-schema":{"type":"object"}},"effect-policy":"pure"},{"local-node-id":"respond","source-node-id":"respond","type":"respond","config":{"status":200},"effect-policy":"pure"},{"local-node-id":"transform","source-node-id":"transform","type":"transform","config":{"expression":"@"},"effect-policy":"pure"}],"edges":[{"source":"request","source-port":"main","destination":"transform","fan-out-ordinal":3},{"source":"transform","source-port":"main","destination":"respond","fan-out-ordinal":7}],"root-terminal-behavior":{"kind":"respond","responders":["respond"]},"entry-input-schema-guard":{"type":"object"},"callable-contract":{"version":"0.1","input-schema-hash":"sha256:a2c799262a3ce3c19ef5cdd983bf3d12b43ab3c426227091b909dcb7054738c0","return-contract":"untyped-json-body","effect-ceiling":"effectful"},"source-map":[{"local-node-id":"request","source-node-id":"request"},{"local-node-id":"respond","source-node-id":"respond"},{"local-node-id":"transform","source-node-id":"transform"}]}}"#;
        let expected_hash =
            "sha256:874c72a9efee67d1b2e8afd16c3bd38c6ca12a1ff35a2a5b0ff13a4c5569cad8";

        assert_eq!(
            compiled.exact_bytes.as_slice(),
            expected_bytes.as_slice(),
            "actual json={}",
            String::from_utf8(compiled.exact_bytes.clone()).unwrap()
        );
        assert_eq!(
            compiled.execution_bundle_hash,
            expected_hash,
            "actual hash={} bytes={}",
            compiled.execution_bundle_hash,
            hex(&compiled.exact_bytes)
        );
        assert!(compiled.execution_bundle_hash.starts_with("sha256:"));
        assert!(
            compiled.execution_bundle_hash["sha256:".len()..]
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        );
    }

    #[test]
    fn deterministic_plan_compiler_child_process_rebuild_probe() {
        if std::env::var("WAMN_0H0G_3_2_CHILD_REBUILD").as_deref() != Ok("1") {
            return;
        }
        let compiled = compiled_golden_flow();
        println!(
            "WAMN_CHILD_REBUILD {} {}",
            compiled.execution_bundle_hash,
            hex(&compiled.exact_bytes)
        );
    }

    #[test]
    fn deterministic_plan_compiler_cross_process_rebuild_is_identical() {
        let compiled = compiled_golden_flow();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env("WAMN_0H0G_3_2_CHILD_REBUILD", "1")
            .arg("deterministic_plan_compiler_child_process_rebuild_probe")
            .arg("--nocapture")
            .output()
            .expect("spawn child process deterministic rebuild probe");
        assert!(
            output.status.success(),
            "child process failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("child stdout is utf-8");
        let line = stdout
            .lines()
            .find(|line| line.starts_with("WAMN_CHILD_REBUILD "))
            .expect("child rebuild line present");
        let parts = line.split_whitespace().collect::<Vec<_>>();
        assert_eq!(parts[1], compiled.execution_bundle_hash);
        assert_eq!(parts[2], hex(&compiled.exact_bytes));
    }

    #[test]
    fn root_plan_derives_callable_contract_guard_hash_and_source_map() {
        let flow = wamn_flow::Flow::from_json(
            r#"{
              "schema-version":"0.1","flow-id":"multi-respond","version":1,
              "nodes":[
                {"id":"request","type":"request","config":{"input-schema":true}},
                {"id":"respond-z","type":"respond","config":{"status":200}},
                {"id":"respond-a","type":"respond","config":{"status":201}}
              ],
              "edges":[
                {"from":"request","to":"respond-z"},
                {"from":"request","to":"respond-a"}
              ]
            }"#,
        )
        .unwrap();
        let plan =
            compile_root_execution_plan(&flow, test_root_artifact_hash(), test_runtime_revision())
                .unwrap()
                .plan;

        assert_eq!(
            plan.body.root_terminal_behavior,
            RootTerminalBehavior::Respond {
                responders: vec![
                    ExecutionNodeId::new("respond-z").unwrap(),
                    ExecutionNodeId::new("respond-a").unwrap(),
                ],
            }
        );
        assert_eq!(plan.body.entry_input_schema_guard, serde_json::json!(true));
        assert_eq!(
            plan.body
                .source_map
                .iter()
                .map(|entry| (entry.local_node_id.as_str(), entry.source_node_id.as_str()))
                .collect::<Vec<_>>(),
            [
                ("request", "request"),
                ("respond-a", "respond-a"),
                ("respond-z", "respond-z")
            ]
        );
        assert_eq!(
            plan.body.callable_contract,
            Some(CallableContract {
                version: "0.1".into(),
                input_schema_hash: entry_input_schema_hash(&serde_json::json!(true)),
                return_contract: CallableReturnContract::UntypedJsonBody,
                effect_ceiling: CallableEffectCeiling::Effectful,
            })
        );
    }

    #[test]
    fn root_plan_callable_derivation_uses_the_full_graph_rule() {
        let flow = wamn_flow::Flow::from_json(
            r#"{
              "schema-version":"0.1","flow-id":"trapped","version":1,
              "nodes":[
                {"id":"request","type":"request","config":{"input-schema":true}},
                {"id":"loop-a","type":"transform","config":{"expression":"@"}},
                {"id":"loop-b","type":"transform","config":{"expression":"@"}},
                {"id":"respond","type":"respond","config":{"status":200}}
              ],
              "edges":[
                {"from":"request","to":"loop-a"},
                {"from":"loop-a","to":"loop-b"},
                {"from":"loop-b","to":"loop-a"},
                {"from":"request","from-port":"error","to":"respond"}
              ]
            }"#,
        )
        .unwrap();
        let plan =
            compile_root_execution_plan(&flow, test_root_artifact_hash(), test_runtime_revision())
                .unwrap()
                .plan;

        assert_eq!(plan.body.callable_contract, None);
    }

    #[test]
    fn root_plan_derives_callable_contract_for_answerable_cycles() {
        let flow = wamn_flow::Flow::from_json(
            r#"{
              "schema-version":"0.1","flow-id":"answerable-cycle","version":1,
              "nodes":[
                {"id":"request","type":"request","config":{"input-schema":true}},
                {"id":"loop-a","type":"transform","config":{"expression":"@"}},
                {"id":"loop-b","type":"transform","config":{"expression":"@"}},
                {"id":"respond","type":"respond","config":{"status":200}}
              ],
              "edges":[
                {"from":"request","to":"loop-a"},
                {"from":"loop-a","to":"loop-b"},
                {"from":"loop-b","to":"loop-a"},
                {"from":"loop-b","to":"respond"}
              ]
            }"#,
        )
        .unwrap();
        let plan =
            compile_root_execution_plan(&flow, test_root_artifact_hash(), test_runtime_revision())
                .unwrap()
                .plan;

        assert!(plan.body.callable_contract.is_some());
    }

    #[test]
    fn root_plan_lowers_call_flow_to_non_binding_instruction() {
        let flow = wamn_flow::Flow::from_json(
            r#"{
              "schema-version":"0.1","flow-id":"caller","version":1,
              "nodes":[
                {"id":"event","type":"event"},
                {"id":"call-child","type":"call-flow","config":{"flow-id":"callee"}}
              ],
              "edges":[{"from":"event","to":"call-child"}]
            }"#,
        )
        .unwrap();
        let compiled =
            compile_root_execution_plan(&flow, test_root_artifact_hash(), test_runtime_revision())
                .unwrap();
        let plan = compiled.plan;

        let call = plan
            .body
            .nodes
            .iter()
            .find(|node| node.local_node_id.as_str() == "call-child")
            .unwrap();
        assert_eq!(call.node_type, "call-flow");
        assert_eq!(
            call.config,
            serde_json::json!({"site": "call-child", "flow-id": "callee"})
        );
        assert_eq!(call.effect_policy, ExecutionEffectPolicy::Effectful);
        assert!(call.source_connection_requirement.is_none());
        assert_eq!(plan.body.edges[0].fan_out_ordinal, 0);
        assert!(plan.requires_idempotency_key());
        assert_eq!(
            compiled.execution_bundle_hash,
            execution_bundle_hash(&compiled.exact_bytes)
        );
    }
}
