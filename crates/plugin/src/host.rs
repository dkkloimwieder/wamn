//! Compose the operation plugin with the host's stores.

use std::collections::HashMap;
use std::sync::Arc;

use wamn_app::database::SqlError;
use wamn_app::result_signing::ResultSigner;
use wamn_authentication::authorization::Authorization;
use wamn_connection::connection_facts::ConnectionCache;
use wamn_postgres::WamnPostgres;

use crate::PgError;

/// Host plugin for guest and native operations over PostgreSQL.
pub struct OperationHost {
    pub(super) store: Arc<WamnPostgres>,
    pub(super) result_signer: std::sync::OnceLock<std::sync::Weak<dyn ResultSigner>>,
    pub(super) authorization: Arc<Authorization>,
    pub(super) connection_cache: Option<Arc<dyn ConnectionCache>>,
    pub(super) operation_metrics: super::operation::OperationMetrics,
    pub(super) operation_workloads:
        std::sync::RwLock<HashMap<String, Arc<super::operation::OperationWorkload>>>,
    /// The single invocation and workload authority registry.
    pub scopes: Arc<wamn_engine::invocation_scope::InvocationScopes>,
}

impl std::fmt::Debug for OperationHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OperationHost")
            .finish_non_exhaustive()
    }
}

impl OperationHost {
    /// Use the host's operation and authorization stores and its application connection cache.
    pub fn new(
        store: Arc<WamnPostgres>,
        authorization: Arc<Authorization>,
        connection_cache: Option<Arc<dyn ConnectionCache>>,
    ) -> Self {
        Self {
            store,
            authorization,
            connection_cache,
            result_signer: std::sync::OnceLock::new(),
            operation_metrics: super::operation::OperationMetrics::default(),
            operation_workloads: std::sync::RwLock::default(),
            scopes: Arc::default(),
        }
    }

    /// Install native result signing before any workloads bind.
    pub fn set_result_signer(&self, signer: &Arc<dyn ResultSigner>) {
        self.result_signer
            .set(Arc::downgrade(signer))
            .expect("result signer is installed once");
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
    ) -> anyhow::Result<Option<wamn_app::ingress::AuthenticatedCaller>> {
        if *principal_id == wamn_identity_protocol::workflow_user_id() {
            use wamn_app::ingress::{AuthenticatedCaller, CredentialType};
            let permissions = self
                .authorization
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
        let grants = self
            .authorization
            .guest_principal_grants(project, project_env_key, principal_id)
            .await
            .map_err(|error| {
                if matches!(
                    error.downcast_ref::<SqlError>(),
                    Some(SqlError::ConnectionUnavailable)
                ) {
                    PgError::ConnectionUnavailable.into()
                } else {
                    error
                }
            })?;
        Ok(grants
            .active
            .then(|| wamn_authority::auth_cache::role_caller(principal_id, role, &grants.roles))
            .flatten())
    }
}
