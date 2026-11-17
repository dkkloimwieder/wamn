//! PostgreSQL identity validation and injection from the shared invocation record.

use sqlx::{Row as _, Statement as _};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use anyhow::Context as _;
use tracing::Instrument as _;

use wamn_event_wire::Causation;
use wamn_identity_protocol::AuthorityClass;

use wamn_authority::identity::{ReleaseIdentity, SessionIdentity};
use wamn_connection::connection::ConnectionInvocation;

use super::pool::{
    ClassCredentials, CredentialProvider, PoolKey, ProjectConfig, ProjectPool,
    StaticCredentialProvider, WamnPostgresConfig,
};
use super::{DEFAULT_PROJECT, PgError};
use crate::invocation::Authority;
use wamn_authentication::authorization::{HeldOperationGrants, held_grants};

mod pools;
pub(super) mod transactions;

#[derive(Debug)]
pub(super) struct SessionIdentityError {
    pub(super) type_: SessionIdentityErrorType,
    pub(super) context: &'static str,
    pub(super) source: anyhow::Error,
}

#[derive(Debug)]
pub(super) enum SessionIdentityErrorType {
    Authorization,
    Infrastructure,
}

impl std::fmt::Display for SessionIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.context, self.source)
    }
}

impl std::error::Error for SessionIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn principal_read_error(context: &'static str, source: sqlx::Error) -> SessionIdentityError {
    SessionIdentityError {
        type_: if source
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref()
            == Some("42501")
        {
            SessionIdentityErrorType::Authorization
        } else {
            SessionIdentityErrorType::Infrastructure
        },
        context,
        source: source.into(),
    }
}

/// Names the shape only: the credential provider and the live pools carry
/// connection material that must not reach a log.
impl std::fmt::Debug for WamnPostgres {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WamnPostgres")
            .finish_non_exhaustive()
    }
}

pub struct WamnPostgres {
    pub(super) result_signer:
        std::sync::OnceLock<std::sync::Weak<dyn crate::result_signing::ResultSigner>>,
    pub(super) connection_cache:
        std::sync::OnceLock<Arc<dyn wamn_connection::connection_facts::ConnectionCache>>,
    pub(super) authorization:
        std::sync::OnceLock<Arc<dyn wamn_authentication::authorization::AuthorizationStore>>,
    pub(super) operation_metrics: super::operation::OperationMetrics,
    pub(super) operation_workloads:
        std::sync::RwLock<HashMap<String, Arc<super::operation::OperationWorkload>>>,
    /// Resolves a project id → its database connection + policy.
    provider: Arc<dyn CredentialProvider>,
    pub(super) host_projects:
        std::collections::BTreeMap<wamn_registry::ProjectId, super::host_project::HostProject>,
    pub(super) host_credentials: Option<Arc<super::host_project::HostCredentials>>,
    pub(super) host_project_bind: tokio::sync::Mutex<()>,
    /// Pools indexed by project and authenticated login.
    pub(super) pools: std::sync::RwLock<HashMap<PoolKey, Arc<ProjectPool>>>,
    /// The single invocation and workload authority registry.
    pub scopes: Arc<wamn_engine::invocation_scope::InvocationScopes>,
    /// Connections destroyed instead of repooled (chaos-acceptance-test observability).
    pub(super) destroyed: Arc<AtomicU64>,
    /// How many times each half of the un-fused workload bind has run.
    pub(super) bind_counters: super::BindCounters,
}

/// The bound principal has no `platform."user"` row in the project_env_key it identity
/// (`wamn-0h0g.9.19`).
///
/// A credential for such a principal is a provisioning defect: every write it
/// makes records an actor that names no row. The bind refuses it, and the
/// refusal is an authorization fact, so the owning boundary translates it to
/// the `permission-denied` the operation vocabulary already carries. The host
/// router does that translation in
/// `crates/execution-host/src/operation/native_policy.rs`.
///
/// It carries the project_env_key, the project database read, and the principal id, so
/// an operator can find the missing row. It carries no connection material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnprovisionedPrincipal {
    project_env_key: wamn_registry::ProjectEnvKey,
    project: wamn_registry::ProjectId,
    principal_id: wamn_identity_protocol::PrincipalId,
}

impl UnprovisionedPrincipal {
    /// The project_env_key whose `platform."user"` table was read.
    #[must_use]
    pub fn project_env_key(&self) -> &wamn_registry::ProjectEnvKey {
        &self.project_env_key
    }

    /// The project database the read used.
    #[must_use]
    pub fn project(&self) -> &wamn_registry::ProjectId {
        &self.project
    }

    /// The caller identity that named no row.
    #[must_use]
    pub fn principal_id(&self) -> &wamn_identity_protocol::PrincipalId {
        &self.principal_id
    }
}

impl std::fmt::Display for UnprovisionedPrincipal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "principal {} has no platform.\"user\" row in project_env_key {} of project {}",
            self.principal_id, self.project_env_key, self.project
        )
    }
}

impl std::error::Error for UnprovisionedPrincipal {}

/// Reject a malformed session identity before it is bound (R16). Since R2 these
/// validators are not the injection boundary: identity values use PostgreSQL
/// literal escaping in the begin batch, so a `'`/`;`/`--` value is inert data, but a
/// malformed identity still fails closed: they define what a *legal* id is (and
/// the no-hyphen `valid_schema` rule still matters where a schema name is quoted
/// into DDL elsewhere).
fn validate_session_identity(
    schema: Option<&str>,
    runner: Option<&str>,
    role: Option<&str>,
    _operation: Option<&wamn_catalog::canonical_name::CanonicalName>,
) -> Result<(), PgError> {
    if let Some(schema) = schema
        && !valid_schema(schema)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid search_path schema".to_string(),
        )));
    }
    if let Some(runner) = runner
        && !valid_runner(runner)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid runner owner".to_string(),
        )));
    }
    if let Some(role) = role
        && !role
            .parse::<wamn_identity_protocol::RoleName>()
            .is_ok_and(|parsed| parsed.as_str() == role)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid caller role".to_string(),
        )));
    }
    Ok(())
}

impl WamnPostgres {
    /// Plugin over a single default database (the [`WamnPostgresConfig`]
    /// credentials). Pools are built lazily; `credentials: None` ⇒ every call
    /// returns `connection-unavailable`.
    pub fn new(cfg: WamnPostgresConfig) -> anyhow::Result<Self> {
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::default_only(ProjectConfig::from_global_config(cfg)),
        )))
    }

    /// Plugin over an explicit [`CredentialProvider`] (multi-project / tests).
    pub fn with_provider(provider: Arc<dyn CredentialProvider>) -> Self {
        Self {
            result_signer: std::sync::OnceLock::new(),
            authorization: std::sync::OnceLock::new(),
            provider,
            host_projects: std::collections::BTreeMap::new(),
            host_credentials: None,
            host_project_bind: tokio::sync::Mutex::new(()),
            pools: std::sync::RwLock::new(HashMap::new()),
            scopes: Arc::default(),
            operation_workloads: std::sync::RwLock::default(),
            connection_cache: std::sync::OnceLock::new(),
            operation_metrics: super::operation::OperationMetrics::default(),
            destroyed: Arc::new(AtomicU64::new(0)),
            bind_counters: super::BindCounters::default(),
        }
    }

    /// Install native result signing before any workloads bind.
    pub fn set_result_signer(&self, signer: &Arc<dyn crate::result_signing::ResultSigner>) {
        self.result_signer
            .set(Arc::downgrade(signer))
            .expect("result signer is installed once");
    }

    /// Hold a file's session lock on the configured platform login until drop.
    pub async fn lock_file(
        &self,
        project: &wamn_registry::ProjectId,
        id: uuid::Uuid,
    ) -> anyhow::Result<sqlx::PgConnection> {
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::Platform)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        // A session lock must never return to a pool still holding its lock.
        let mut connection = connection.detach();
        sqlx::query("SELECT set_config('lock_timeout', $1, false)")
            .bind(format!("{}ms", policy.statement_timeout_ms))
            .execute(&mut connection)
            .await?;
        sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
            .bind(format!("wamn:file:{id}"))
            .execute(&mut connection)
            .await?;
        Ok(connection)
    }

    /// Install the host's authorization memory before binding any workloads.
    pub fn set_authorization_store(
        &self,
        store: Arc<dyn wamn_authentication::authorization::AuthorizationStore>,
    ) {
        self.authorization
            .set(store)
            .expect("host authorization store is installed once");
    }

    /// Build from the deployment's configuration: the default project from the
    /// credential the COMPOSITION ROOT names.
    ///
    /// # Why the caller passes the credential (`wamn-0h0g.22.8.3`)
    ///
    /// `wamn-0h0g.22.8.2` removed the ambient `WAMN_PG_URL` read from
    /// [`WamnPostgresConfig::from_env`], because a credential picked up
    /// implicitly there made the runtime a SECOND credential source competing
    /// with whatever a caller supplied — the conflict
    /// [`AmbientCredentialState`](super::credential_exactness::AmbientCredentialState)
    /// already declares.
    ///
    /// The environment is still the TRANSPORT; that is how Kubernetes injects a
    /// Secret, and `deploy/platform` does exactly that via `secretKeyRef`. What
    /// changed is WHERE it is read: once, at trusted composition, where it is
    /// the named explicit source rather than a silent fallback buried in the
    /// config layer. `services/executor` already composed this way; this is the
    /// host taking the same shape.
    ///
    /// `wamn-0h0g.22.16`: the parameter is the caller's PER-CLASS credential
    /// set, not one url, so the composition root states which authority each
    /// login belongs to instead of leaving one login to serve every authority
    /// implicitly.
    pub fn from_env(credentials: Option<ClassCredentials>) -> anyhow::Result<Self> {
        let cfg = WamnPostgresConfig::from_env();
        let default = credentials.map(|credentials| ProjectConfig::from_global(credentials, &cfg));
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::default_only(default),
        )))
    }

    /// Build from the deployment's configuration with the composition root's
    /// credential bound to its declared project rather than the default key.
    ///
    /// A per-project serving host already carries the trusted project identity.
    /// Registering that host's Secret under `default` makes every named-project
    /// lookup refuse despite having the exact credential. No other project is
    /// registered, so an unlisted project still refuses at resolution.
    pub fn from_env_for_project(
        project: &wamn_registry::ProjectId,
        credentials: Option<ClassCredentials>,
    ) -> anyhow::Result<Self> {
        let cfg = WamnPostgresConfig::from_env();
        let mut projects = HashMap::new();
        if let Some(credentials) = credentials {
            projects.insert(
                project.to_owned(),
                ProjectConfig::from_global(credentials, &cfg),
            );
        }
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::new(projects, None),
        )))
    }

    /// Register the project_env_key identity for a component id. The bench harness calls
    /// this directly; the host path feeds it from workload bind.
    pub fn set_project_env_key(
        &self,
        component_id: &str,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) {
        self.scopes.update::<Authority, _>(component_id, |record| {
            record.identity.project_env_key = Some(project_env_key.clone());
        });
    }

    pub(super) fn project_env_key_for(
        &self,
        component_id: &str,
    ) -> Option<wamn_registry::ProjectEnvKey> {
        self.scopes
            .read::<Authority, _>(component_id, |record| {
                record.identity.project_env_key.clone()
            })
            .flatten()
    }

    /// Register which project's database a component uses. The bench harness
    /// calls this directly; the host path feeds it from the `wamn.project`
    /// workload config. Absent ⇒ the default project.
    pub fn set_project(
        &self,
        component_id: &str,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<()> {
        self.scopes.update::<Authority, _>(component_id, |record| {
            record.identity.project = Some(project.clone());
        });
        Ok(())
    }

    pub(super) fn project_for(&self, component_id: &str) -> wamn_registry::ProjectId {
        self.scopes
            .read::<Authority, _>(component_id, |record| record.identity.project.clone())
            .flatten()
            .unwrap_or_else(|| (*DEFAULT_PROJECT).clone())
    }

    /// Register the `search_path` schema for a component id. When set, every
    /// transaction the plugin opens for that component also runs
    /// `SET LOCAL search_path`, so the guest's unqualified table names resolve
    /// to a host-chosen schema. The bench harness calls this directly; the host
    /// path feeds it from the `wamn.schema` workload config.
    pub fn set_schema(&self, component_id: &str, schema: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_schema(schema),
            "invalid schema {schema:?}: 1-63 chars of [A-Za-z0-9_] starting with a letter/underscore required"
        );
        self.scopes.update::<Authority, _>(component_id, |record| {
            record.identity.schema = Some(schema.to_string());
        });
        Ok(())
    }

    pub(super) fn schema_for(&self, component_id: &str) -> Option<String> {
        self.scopes
            .read::<Authority, _>(component_id, |record| record.identity.schema.clone())
            .flatten()
    }

    /// Register the durable queue lease owner for a component id.
    /// Each transaction records it in the protected caller identity.
    pub fn set_runner(&self, component_id: &str, runner: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_runner(runner),
            "invalid runner owner {runner:?}: 1-128 chars of [A-Za-z0-9_-] required"
        );
        self.scopes.update::<Authority, _>(component_id, |record| {
            record.identity.runner = Some(runner.to_string());
        });
        Ok(())
    }

    pub(super) fn runner_for(&self, component_id: &str) -> Option<String> {
        self.scopes
            .read::<Authority, _>(component_id, |record| record.identity.runner.clone())
            .flatten()
    }

    /// Register the caller's role for a component id.
    /// Each transaction records it for the compiled row policies.
    pub fn set_role(&self, component_id: &str, role: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            role.parse::<wamn_identity_protocol::RoleName>()
                .is_ok_and(|parsed| parsed.as_str() == role),
            "invalid caller role {role:?}: a non-empty roles.name is required"
        );
        self.scopes.update::<Authority, _>(component_id, |record| {
            record.identity.role = Some(role.to_string());
        });
        Ok(())
    }

    pub(super) fn role_for(&self, component_id: &str) -> Option<String> {
        self.scopes
            .read::<Authority, _>(component_id, |record| record.identity.role.clone())
            .flatten()
    }

    /// Register the executing principal for a component id.
    /// Each transaction records it for the record history triggers.
    pub fn set_user_id(
        &self,
        component_id: &str,
        user_id: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<()> {
        self.scopes.update::<Authority, _>(component_id, |record| {
            record.identity.user_id = Some(*user_id);
        });
        Ok(())
    }

    pub(super) fn user_id_for(
        &self,
        component_id: &str,
    ) -> Option<wamn_identity_protocol::PrincipalId> {
        self.scopes
            .read::<Authority, _>(component_id, |record| record.identity.user_id)
            .flatten()
    }

    /// Register the operation for a component id.
    /// Each transaction records it as the source of each write.
    pub fn set_operation(
        &self,
        component_id: &str,
        operation: &wamn_catalog::canonical_name::CanonicalName,
    ) -> anyhow::Result<()> {
        self.scopes.update::<Authority, _>(component_id, |record| {
            record.identity.operation = Some(operation.clone());
        });
        Ok(())
    }

    pub(super) fn operation_for(
        &self,
        component_id: &str,
    ) -> Option<wamn_catalog::canonical_name::CanonicalName> {
        self.scopes
            .read::<Authority, _>(component_id, |record| record.identity.operation.clone())
            .flatten()
    }

    /// Register the application that runs the component's operation.
    /// Each transaction records it in the protected caller identity.
    pub fn set_application(&self, component_id: &str, application: wamn_catalog::ApplicationName) {
        self.scopes.update::<Authority, _>(component_id, |record| {
            record.identity.application = Some(application);
        });
    }

    pub(super) fn application_for(
        &self,
        component_id: &str,
    ) -> Option<wamn_catalog::ApplicationName> {
        self.scopes
            .read::<Authority, _>(component_id, |record| record.identity.application.clone())
            .flatten()
    }

    /// Register the release this pod carries for a component id. The production
    /// identity verifies its digest against the admission pin of every run it
    /// leases. The bench harness and live tests call this directly. Absent leaves the identity
    /// verifying nothing.
    ///
    /// The digest arrives as [`wamn_digest::Digest`], so its shape is already validated.
    ///
    /// # Why effect authority needs no equality test against this record
    ///
    /// The digest comes from the same loaded object every reader resolves
    /// against, so a run leased under it runs the manifest the pod loaded —
    /// structurally, not because anything compares them (owner ruling
    /// `wamn-0h0g.15.102`, after `wamn-0h0g.15.103` struck the asserted carrier).
    pub fn set_release_identity(
        &self,
        component_id: &str,
        identity: ReleaseIdentity,
    ) -> anyhow::Result<()> {
        self.scopes.update::<Authority, _>(component_id, |record| {
            record.identity.release = Some(identity);
        });
        Ok(())
    }

    /// Declare (`Some`) or clear (`None`) the causation context of the run a
    /// component is driving (l5i9.12.2). While set, each write transaction the plugin
    /// opens for the component carries a `wamn.causation` message. Host-injected
    /// like the other identity registries — callers set it directly, exactly like
    /// [`set_project_env_key`](Self::set_project_env_key) / [`set_runner`](Self::set_runner).
    pub fn set_current_run(&self, component_id: &str, ctx: Option<Causation>) {
        self.scopes
            .update::<Authority, _>(component_id, |record| record.causation = ctx);
    }

    pub(super) fn current_run_for(&self, component_id: &str) -> Option<Causation> {
        if self.active_read(component_id) {
            return None;
        }
        self.scopes
            .read::<Authority, _>(component_id, |record| record.causation.clone())
            .flatten()
    }

    /// Bind the exact invocation facts before entering one pooled component
    /// (`wamn-0h0g.7.9`), so a postgres effect can name the wiring and node
    /// position it was raised under.
    ///
    /// Test fixtures populate the same authority record that execution readers use.
    #[cfg(any(test, feature = "test-util"))]
    pub fn bind_invocation(
        &self,
        component_id: &str,
        invocation: ConnectionInvocation,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!component_id.is_empty(), "component id must be present");
        self.scopes
            .update::<wamn_connection::invocation::Scope, _>(component_id, |record| {
                anyhow::ensure!(record.invocation.is_none(), "invocation is already bound");
                record.invocation = Some(invocation);
                Ok(())
            })
    }

    /// Release the invocation when the instance returns to the pool.
    #[cfg(any(test, feature = "test-util"))]
    pub fn revoke_invocation(&self, component_id: &str) {
        self.scopes
            .update_existing::<wamn_connection::invocation::Scope, _>(component_id, |record| {
                record.invocation = None
            });
    }

    /// The invocation currently bound to one pooled instance.
    #[must_use]
    pub fn invocation(&self, component_id: &str) -> Option<ConnectionInvocation> {
        wamn_connection::invocation::invocation(&self.scopes, component_id)
    }

    /// Bind workload identity and verify that its executing principal is provisioned.
    /// An absent optional member clears that member's previous authority.
    pub async fn bind_session_identity(
        &self,
        component_id: &str,
        identity: &SessionIdentity,
    ) -> anyhow::Result<()> {
        self.write_session_identity(component_id, identity)?;
        // An acquisition that names no principal reads nothing. The
        // record-history triggers already refuse such a write with
        // actor-required.
        if let Some(user_id) = identity.user_id.as_ref() {
            let project = identity.project.as_ref().unwrap_or(&DEFAULT_PROJECT);
            self.require_provisioned_principal(
                identity
                    .project_env_key
                    .as_ref()
                    .context("project environment identity is absent")?,
                project,
                user_id,
            )
            .await?;
        }
        Ok(())
    }

    /// Bind `identity` as [`bind_session_identity`](Self::bind_session_identity)
    /// does, without its principal read, for the call inside a request element whose
    /// transaction a bind of the same identity began (`wamn-uq4r`).
    ///
    /// The request element's bind read the principal's user row moments before, in the
    /// same invocation, so a second read answers nothing new and takes a
    /// second guest connection. The caller passes the identity of that bind.
    pub fn bind_request_element_session_identity(
        &self,
        component_id: &str,
        identity: &SessionIdentity,
    ) -> anyhow::Result<()> {
        self.write_session_identity(component_id, identity)
    }

    fn write_session_identity(
        &self,
        component_id: &str,
        identity: &SessionIdentity,
    ) -> anyhow::Result<()> {
        validate_identity(identity)?;
        self.scopes
            .read::<wamn_connection::invocation::Scope, _>(component_id, |record| {
                record
                    .context
                    .as_ref()
                    .map(|context| crate::invocation::validate_context(identity, context))
                    .transpose()
            })
            .transpose()?;
        self.scopes.update::<Authority, _>(component_id, |record| {
            record.identity = identity.clone();
            record.causation = None;
        });
        Ok(())
    }

    /// Read one `platform."user"` row and refuse the principal that owns none
    /// (`wamn-0h0g.9.19`).
    ///
    /// The read uses the project_env_key's own guest login, for two reasons. It is the
    /// only authority class every composition root names, because the platform
    /// classes are unnamed when their generation is absent. And it is the
    /// credential that already holds `SELECT` on this table, so the test
    /// needs no new grant. The database is the org boundary.
    ///
    /// A read that cannot run refuses the bind with its own error rather than
    /// the `UnprovisionedPrincipal` refusal. An unreachable table is an
    /// infrastructure defect and not an authorization fact, and the bind must
    /// fail closed either way.
    async fn require_provisioned_principal(
        &self,
        project_env_key: &wamn_registry::ProjectEnvKey,
        project: &wamn_registry::ProjectId,
        principal_id: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<()> {
        let grants = self
            .read_principal_grants(
                project,
                project_env_key,
                principal_id,
                AuthorityClass::GuestSql,
            )
            .await
            .map_err(|source| match source.downcast::<PgError>() {
                Ok(source) => SessionIdentityError {
                    type_: SessionIdentityErrorType::Infrastructure,
                    context: "check out the principal grants connection",
                    source: source.into(),
                }
                .into(),
                Err(source) => source,
            })?;
        if !grants.present {
            tracing::warn!(
                %project_env_key,
                %project,
                %principal_id,
                "wamn:postgres: refusing a bind for a principal with no platform.\"user\" row"
            );
            return Err(UnprovisionedPrincipal {
                project_env_key: project_env_key.to_owned(),
                project: project.to_owned(),
                principal_id: principal_id.to_owned(),
            }
            .into());
        }
        Ok(())
    }

    /// Read back the identity set one component id currently resolves to.
    ///
    /// `None` is the base denial policy: no project_env_key is bound, so
    /// [`require_project_env_key`](Self::require_project_env_key) refuses every call made under
    /// this id. That is what an idle, never-acquired execution instance must
    /// report, and it is the read half of the checkout-identity seam — the pair
    /// this and [`bind_session_identity`](Self::bind_session_identity) form is what
    /// lets a test assert that two concurrent acquisitions never share one
    /// identity.
    pub fn session_identity(&self, component_id: &str) -> Option<SessionIdentity> {
        crate::invocation::identity(&self.scopes, component_id)
    }

    /// End SQL authority while retaining the invocation's result-signing authority.
    pub fn revoke_session_identity(&self, component_id: &str) {
        self.scopes
            .update_existing::<Authority, _>(component_id, |record| {
                record.identity = SessionIdentity::default();
                record.causation = None;
            });
    }

    /// Release every scope owned by a stopped workload.
    pub(super) fn clear_component_identity(&self, workload_id: &str) {
        self.scopes.revoke_workload(workload_id);
        self.operation_workloads
            .write()
            .expect("operation workloads lock poisoned")
            .retain(|scope, entry| {
                entry.workload.as_ref() != workload_id && !scope.starts_with(workload_id)
            });
    }

    /// Find the local principal for a verified issuer and subject.
    pub async fn resolve_subject(
        &self,
        project: &wamn_registry::ProjectId,
        issuer: &str,
        subject: &str,
    ) -> anyhow::Result<Option<wamn_identity_protocol::PrincipalId>> {
        let (mut connection, _policy) = self
            .checkout_platform(project, AuthorityClass::Platform)
            .await?;
        let result = sqlx::query_scalar(
            "SELECT id FROM platform.\"user\" WHERE issuer = $1 AND sub = $2 AND status = 'active' AND type = 'user'",
        )
        .bind(issuer)
        .bind(subject)
        .fetch_optional(&mut *connection)
        .await;
        match result {
            Ok(principal) => Ok(principal),
            Err(error) => {
                self.destroy(connection);
                Err(principal_read_error("resolve the access token subject", error).into())
            }
        }
    }

    /// The current WAL position used by weak ETags.
    ///
    /// The shared WAL position covers commits from every writer without a counter.
    /// Unrelated writes can invalidate the tag too. Read it before the query snapshot.
    pub async fn model_version(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<String> {
        let (mut connection, _policy) = self
            .checkout_platform(project, AuthorityClass::Platform)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let statement = sqlx::Executor::prepare(
            &mut *connection,
            sqlx::SqlSafeStr::into_sql_str("SELECT pg_current_wal_lsn()::text"),
        )
        .await
        .context("prepare the WAL position for model versions")?;
        let row = sqlx::query(statement.sql().clone())
            .fetch_one(&mut *connection)
            .await
            .context("read the WAL position for model versions")?;
        Ok(row.try_get(0)?)
    }

    /// The grants every role of an active org-issued user gives it in this
    /// project_env_key, from [`principal_grants`](Self::principal_grants).
    ///
    /// Route authentication first requires membership in the release environment.
    pub async fn user_operation_permissions(
        &self,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
        principal_id: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<HeldOperationGrants> {
        let grants = self
            .principal_grants(project, project_env_key, principal_id)
            .await?;
        Ok(held_grants(&grants))
    }

    /// The user row, roles and permissions of one principal, from the host's
    /// memory or, on a miss, from one read under the callable-HTTP authority
    /// with no session identity. The read also answers the provisioned-principal
    /// test of the identity bind.
    pub async fn principal_grants(
        &self,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
        principal: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<wamn_authority::auth_cache::PrincipalGrants> {
        self.read_principal_grants(
            project,
            project_env_key,
            principal,
            AuthorityClass::Platform,
        )
        .await
    }

    async fn read_principal_grants(
        &self,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
        principal: &wamn_identity_protocol::PrincipalId,
        class: AuthorityClass,
    ) -> anyhow::Result<wamn_authority::auth_cache::PrincipalGrants> {
        let (mut connection, _policy) = match class {
            AuthorityClass::GuestSql => self.checkout_guest(project, project_env_key).await?,
            _ => self.checkout_platform(project, class).await?,
        };
        let result =
            wamn_authentication::authorization::principal_grants(&mut connection, principal)
                .instrument(tracing::info_span!("wamn.auth.perm.query"))
                .await;
        let grants = match result {
            Ok(grants) => grants,
            Err(error) => {
                self.destroy(connection);
                return Err(principal_read_error("read the principal grants", error).into());
            }
        };
        Ok(grants)
    }

    /// Whether the environment of `project_env_key` is inactive,
    /// from its row of `platform.environment`, read as `wamn_app`
    /// (docs/architecture/execution.md#routing-and-availability). No row means
    /// active. The host keeps the answer in its [auth
    /// memory](wamn_authority::auth_cache).
    pub async fn environment_inactive(
        &self,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<bool> {
        let (mut connection, _pool) = self
            .checkout_guest(project, project_env_key)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let inactive =
            match wamn_authentication::authorization::environment_inactive(&mut connection).await {
                Ok(inactive) => inactive,
                Err(error) => {
                    self.destroy(connection);
                    return Err(error).context("read the environment status");
                }
            };
        Ok(inactive)
    }

    pub async fn administration_transaction<T, E>(
        &self,
        project: &wamn_registry::ProjectId,
        principal_id: &wamn_identity_protocol::PrincipalId,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        work: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
    ) -> anyhow::Result<Result<T, E>> {
        let (mut connection, policy) = self
            .checkout_platform(project, AuthorityClass::Platform)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_identity(
                &mut connection,
                AuthorityClass::Platform,
                false,
                None,
                None,
                None,
                Some(principal_id),
                Some(operation),
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(anyhow::anyhow!(error.to_string()));
        }
        let result = work(&mut connection).await;
        let end = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
        if let Err(error) = transactions::finish_transaction(&mut connection, end).await {
            self.destroy(connection);
            return Err(error).context("end the host route transaction");
        }
        Ok(result)
    }

    /// The caller of one node of a workflow run that executes as `principal_id`
    /// with `role` active, from the grants the principal holds now, read
    /// through the host's auth memory as a request reads them
    /// (docs/architecture/execution.md#environment-workflows). None when the
    /// principal is inactive or no longer holds `role`.
    pub async fn workflow_node_caller(
        &self,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
        principal_id: &wamn_identity_protocol::PrincipalId,
        role: &str,
    ) -> anyhow::Result<Option<wamn_engine::flow_http_routing::AuthenticatedCaller>> {
        let grants = self
            .read_principal_grants(
                project,
                project_env_key,
                principal_id,
                AuthorityClass::GuestSql,
            )
            .await?;
        Ok(grants
            .active
            .then(|| wamn_authority::auth_cache::role_caller(principal_id, role, &grants.roles))
            .flatten())
    }

    pub(super) fn require_project_env_key(
        &self,
        component_id: &str,
    ) -> Result<wamn_registry::ProjectEnvKey, PgError> {
        self.project_env_key_for(component_id).ok_or_else(|| {
            tracing::warn!(
                component_id,
                "wamn:postgres call from component with no project_env_key identity"
            );
            PgError::QueryError((
                "WAMN0".to_string(),
                "no project_env_key identity configured for this component".to_string(),
            ))
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;

/// A durable-queue lease owner. An identity-format contract: bounded
/// `[A-Za-z0-9_-]`, no quotes/backslashes. Since R2 this is NO LONGER the
/// injection boundary on the plugin path: the begin batch escapes the runner
/// as a PostgreSQL literal, so a quote/backslash is inert data, but a malformed owner still
/// fails closed.
pub fn valid_runner(runner: &str) -> bool {
    !runner.is_empty()
        && runner.len() <= 128
        && runner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A `search_path` schema name. Stricter than a project: no hyphens. Since R2 the
/// value uses PostgreSQL literal escaping in the begin batch, but the no-hyphen
/// rule still matters where a schema
/// name is quoted into DDL elsewhere (e.g. the migrate / copy paths), and a
/// malformed schema still fails closed.
pub fn valid_schema(schema: &str) -> bool {
    !schema.is_empty()
        && schema.len() <= 63
        && schema
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && schema
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(crate) fn validate_identity(identity: &SessionIdentity) -> anyhow::Result<()> {
    anyhow::ensure!(
        identity.project_env_key.is_some(),
        "project environment identity is absent"
    );
    anyhow::ensure!(
        identity.schema.as_deref().is_none_or(valid_schema),
        "invalid schema"
    );
    anyhow::ensure!(
        identity.runner.as_deref().is_none_or(valid_runner),
        "invalid runner"
    );
    anyhow::ensure!(
        identity.role.as_deref().is_none_or(|role| role
            .parse::<wamn_identity_protocol::RoleName>()
            .is_ok_and(|parsed| parsed.as_str() == role)),
        "invalid role"
    );
    Ok(())
}

#[async_trait::async_trait]
impl wamn_authentication::authorization::AuthorizationStore for WamnPostgres {
    async fn route_pat(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
        reader: &sqlx::PgPool,
        token: &str,
    ) -> anyhow::Result<Option<wamn_identity::AuthenticatedPrincipal>> {
        if let Some(store) = self.authorization.get() {
            return store.route_pat(project, key, reader, token).await;
        }
        wamn_authentication::authorization::route_pat(reader, token).await
    }

    async fn environment_inactive(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<bool> {
        if let Some(store) = self.authorization.get() {
            return store.environment_inactive(project, key).await;
        }
        self.environment_inactive(project, key).await
    }

    async fn resolve_subject(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
        issuer: &str,
        subject: &str,
    ) -> anyhow::Result<Option<wamn_identity_protocol::PrincipalId>> {
        if let Some(store) = self.authorization.get() {
            return store.resolve_subject(project, key, issuer, subject).await;
        }
        self.resolve_subject(project, issuer, subject).await
    }

    async fn user_operation_permissions(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
        principal: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<HeldOperationGrants> {
        if let Some(store) = self.authorization.get() {
            return store
                .user_operation_permissions(project, key, principal)
                .await;
        }
        self.user_operation_permissions(project, key, principal)
            .await
    }

    async fn principal_grants(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
        principal: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<wamn_authority::auth_cache::PrincipalGrants> {
        if let Some(store) = self.authorization.get() {
            return store.principal_grants(project, key, principal).await;
        }
        self.principal_grants(project, key, principal).await
    }
}
