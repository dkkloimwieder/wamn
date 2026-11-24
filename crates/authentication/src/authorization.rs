//! Project authorization contracts and held grants.

use std::{collections::BTreeSet, sync::Arc, time::SystemTime};
use wamn_authority::auth_cache::PrincipalGrants;
use wamn_identity_protocol::{PrincipalId, secret::PatToken};

/// The application authority a caller holds now: whether it holds `admin`, and
/// the stable operation references its other roles grant under the loaded
/// release (docs/architecture/execution.md#routing-and-availability).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldOperationGrants {
    pub admin: bool,
    /// A service holds permissions but never selects a user's active role.
    pub user: bool,
    pub references: BTreeSet<String>,
    /// The names of the held roles.
    pub roles: BTreeSet<String>,
}

impl HeldOperationGrants {
    /// The one role a caller with these grants acts with
    /// (docs/architecture/execution.md#environment-workflows): `admin` for a
    /// holder of `admin`, the single role of a caller that holds exactly one,
    /// and none for a caller that holds no role or more than one.
    pub fn active_role(&self) -> Option<Box<str>> {
        if !self.user {
            return None;
        }
        self.pat_role()
    }

    /// A PAT caller selects only a role held by its own active principal.
    /// Service callers never inherit the role of a user session.
    pub fn pat_role(&self) -> Option<Box<str>> {
        if self.admin {
            return Some(wamn_identity_protocol::ADMIN_ROLE.into());
        }
        let mut roles = self.roles.iter();
        match (roles.next(), roles.next()) {
            (Some(role), None) => Some(role.as_str().into()),
            _ => None,
        }
    }
}

/// Collect permissions and roles from an active project principal.
pub fn held_grants(grants: &PrincipalGrants) -> HeldOperationGrants {
    let mut held = HeldOperationGrants::default();
    if !grants.active || !grants.present {
        return held;
    }
    held.user = grants.user;
    for (role, permissions) in &grants.roles {
        held.admin |= role == wamn_identity_protocol::ADMIN_ROLE;
        held.references.extend(permissions.iter().cloned());
        held.roles.insert(role.clone());
    }
    held
}

/// Host transport and authorization memory used by route authentication.
#[async_trait::async_trait]
pub trait AuthorizationStore: std::fmt::Debug + Send + Sync {
    /// Register deploy-owned cache coordinates when an application workload binds.
    fn configure_application(
        &self,
        _application: &wamn_application::application_config::ApplicationConfig,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Admit a project PAT; connection ownership remains a separate check.
    async fn route_pat(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
        token: &PatToken,
    ) -> anyhow::Result<Option<Arc<ProjectPat>>>;

    async fn environment_inactive(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<bool>;
    /// Resolve an external identity to an active user in the project database.
    async fn resolve_subject(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
        issuer: &str,
        subject: &str,
    ) -> anyhow::Result<Option<wamn_identity_protocol::PrincipalId>>;
    async fn user_operation_permissions(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
        principal: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<HeldOperationGrants>;
    async fn principal_grants(
        &self,
        project: &wamn_registry::ProjectId,
        key: &wamn_registry::ProjectEnvKey,
        principal: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<PrincipalGrants>;
}

/// The active local service admitted by a PAT.
#[derive(Debug)]
pub struct ProjectPat {
    pub principal: PrincipalId,
    pub subject: Box<str>,
    pub deadline: SystemTime,
}

#[derive(Debug)]
pub struct SessionIdentityError {
    pub type_: SessionIdentityErrorType,
    pub context: &'static str,
    pub source: anyhow::Error,
}

#[derive(Debug)]
pub enum SessionIdentityErrorType {
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
