//! Compose the operation plugin with project pools and the host's shared authorization.

use std::collections::HashMap;
use std::sync::Arc;

use wamn_application::database::OperationStore;
use wamn_application::result_signing::ResultSigner;
use wamn_authentication::authorization::Authorization;
use wamn_postgres::{CheckoutError, WamnPostgres};

use crate::PgError;

/// Host plugin for guest and native operations over PostgreSQL.
pub struct OperationHost {
    // Native callbacks still use an open SQLx connection.
    pub(super) pools: Arc<WamnPostgres>,
    pub(super) store: Arc<dyn OperationStore>,
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
    pub fn new(
        pools: Arc<WamnPostgres>,
        store: Arc<dyn OperationStore>,
        authorization: Option<Arc<Authorization>>,
    ) -> Self {
        Self {
            pools,
            store,
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
        self.pools.lock_file(project, id).await
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
        self.pools
            .administration_transaction(project, principal_id, operation, work)
            .await
    }

    /// Open a native administration transaction.
    pub async fn begin_administration(
        &self,
        project: &wamn_registry::ProjectId,
        principal_id: &wamn_identity_protocol::PrincipalId,
        operation: &wamn_package::canonical_name::CanonicalName,
    ) -> anyhow::Result<wamn_connection::database::ConnectionGuard> {
        self.pools
            .begin_administration(project, principal_id, operation)
            .await
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
            let permissions = self
                .pools
                .workflow_role_permissions(project, principal_id, role)
                .await?;
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
        .await
        .map_err(|error| match error.downcast::<CheckoutError>() {
            Ok(_) => PgError::ConnectionUnavailable.into(),
            Err(error) => error,
        })?;
        Ok(grants
            .active
            .then(|| wamn_authority::auth_cache::role_caller(principal_id, role, &grants.roles))
            .flatten())
    }
}
