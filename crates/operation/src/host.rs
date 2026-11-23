//! Compose the operation plugin with project pools and the host's shared authorization.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context as _;
use wamn_application::result_signing::ResultSigner;
use wamn_authentication::authorization::Authorization;
use wamn_identity_protocol::AuthorityClass;
use wamn_postgres::WamnPostgres;

use crate::transactions;

/// Host plugin for guest and native operations over PostgreSQL.
pub struct OperationHost {
    pub(super) pools: Arc<WamnPostgres>,
    pub(super) result_signer: std::sync::OnceLock<std::sync::Weak<dyn ResultSigner>>,
    pub(super) authorization: Option<Arc<Authorization>>,
    pub(super) operation_metrics: super::operation::OperationMetrics,
    pub(super) operation_workloads:
        std::sync::RwLock<HashMap<String, Arc<super::operation::OperationWorkload>>>,
    /// The single invocation and workload authority registry.
    pub scopes: Arc<wamn_engine::invocation_scope::InvocationScopes>,
    /// How many times each half of the un-fused workload bind has run.
    pub(super) bind_counters: super::BindCounters,
}

impl std::fmt::Debug for OperationHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OperationHost")
            .finish_non_exhaustive()
    }
}

impl OperationHost {
    /// Use the project's pools and, when serving applications, the shared authorization store.
    pub fn new(pools: Arc<WamnPostgres>, authorization: Option<Arc<Authorization>>) -> Self {
        Self {
            pools,
            authorization,
            result_signer: std::sync::OnceLock::new(),
            operation_metrics: super::operation::OperationMetrics::default(),
            operation_workloads: std::sync::RwLock::default(),
            scopes: Arc::default(),
            bind_counters: super::BindCounters::default(),
        }
    }

    /// Install native result signing before any workloads bind.
    pub fn set_result_signer(&self, signer: &Arc<dyn ResultSigner>) {
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
            .pools
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

    pub async fn administration_transaction<T, E>(
        &self,
        project: &wamn_registry::ProjectId,
        principal_id: &wamn_identity_protocol::PrincipalId,
        operation: &wamn_package::canonical_name::CanonicalName,
        work: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
    ) -> anyhow::Result<Result<T, E>> {
        let mut connection = self
            .begin_administration(project, principal_id, operation)
            .await?;
        let result = work(connection.connection()).await;
        let end = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
        transactions::finish_transaction(connection.connection(), end)
            .await
            .context("end the host route transaction")?;
        connection.repool();
        Ok(result)
    }

    /// Open an administration transaction on an owned platform session.
    /// The caller can retain the session across commit to hold a session lock.
    pub async fn begin_administration(
        &self,
        project: &wamn_registry::ProjectId,
        principal_id: &wamn_identity_protocol::PrincipalId,
        operation: &wamn_package::canonical_name::CanonicalName,
    ) -> anyhow::Result<wamn_connection::database::ConnectionGuard> {
        let (connection, policy) = self
            .pools
            .checkout_platform(project, AuthorityClass::Platform)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut connection = super::resources::StatementConnectionGuard::new(
            connection,
            Arc::clone(self.pools.destroyed_connections()),
        );
        transactions::begin_with_identity(
            connection.connection(),
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
        .context("begin the host route transaction")?;
        Ok(connection)
    }

    /// The caller of one node of a workflow run that executes as `principal_id`
    /// with `role` active, from the grants the principal holds now, read
    /// through the project's guest login
    /// (docs/architecture/execution.md#environment-workflows). None when the
    /// principal is inactive or no longer holds the role. The workflow service
    /// reads the run role directly from the project platform login.
    pub async fn workflow_node_caller(
        &self,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
        principal_id: &wamn_identity_protocol::PrincipalId,
        role: &str,
    ) -> anyhow::Result<Option<wamn_application::ingress::AuthenticatedCaller>> {
        if *principal_id == wamn_identity_protocol::workflow_user_id() {
            use wamn_application::ingress::{AuthenticatedCaller, CredentialType};
            let permissions = self.executor_read(project, async |connection| {
                Ok(sqlx::query_scalar::<_, Vec<String>>(
                    "SELECT ARRAY(SELECT permission FROM platform.permission WHERE role_name = r.name)
                       FROM platform.role AS r
                      WHERE r.name = $1 AND EXISTS (
                          SELECT 1 FROM platform.\"user\" WHERE id = $2 AND status = 'active'
                      )"
                )
                .bind(role)
                .bind(principal_id)
                .fetch_optional(connection)
                .await?)
            }).await?;
            return Ok(permissions.map(|permissions| {
                AuthenticatedCaller::new(
                    "workflow".parse().expect("fixed workflow caller"),
                    *principal_id,
                    CredentialType::QueuedService,
                    role == wamn_identity_protocol::ADMIN_ROLE,
                    permissions.into_iter().collect(),
                    Some(role.into()),
                )
            }));
        }
        let grants = wamn_authentication::authorization::read_guest_grants(
            &self.pools,
            project,
            project_env_key,
            principal_id,
        )
        .await?;
        Ok(grants
            .active
            .then(|| wamn_authority::auth_cache::role_caller(principal_id, role, &grants.roles))
            .flatten())
    }
}
