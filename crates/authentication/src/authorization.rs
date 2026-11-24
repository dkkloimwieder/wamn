//! Authorization reads owned by authentication, over a host-authorized connection.

use sqlx::Row as _;
use std::{collections::BTreeSet, sync::Arc, time::SystemTime};
use wamn_authority::auth_cache::PrincipalGrants;
use wamn_identity_protocol::{PrincipalId, secret::PatToken};

/// The status row of the environment, which `wamn_app` reads under its
/// database boundary.
const ENVIRONMENT_INACTIVE_SQL: &str = "SELECT status = 'inactive' FROM platform.environment";

/// The user row of one principal, each role it holds and each permission
/// of that role, in one read. No row means no user row. A role without
/// permissions, such as `admin`, yields one row with a null permission, and a
/// principal without roles yields one row with a null role.
const PRINCIPAL_GRANTS_SQL: &str = "SELECT u.status = 'active', r.role_name, p.permission, u.type = 'user' \
    FROM platform.\"user\" u LEFT JOIN platform.user_role r \
    ON r.user_id = u.id \
    LEFT JOIN platform.permission p \
    ON p.role_name = r.role_name \
    WHERE u.id = $1";

fn principal_grants_from_rows(
    rows: &[sqlx::postgres::PgRow],
) -> Result<wamn_authority::auth_cache::PrincipalGrants, sqlx::Error> {
    let mut grants = wamn_authority::auth_cache::PrincipalGrants {
        present: !rows.is_empty(),
        ..Default::default()
    };
    for row in rows {
        grants.active = row.try_get(0)?;
        grants.user = row.try_get(3)?;
        if let Some(role) = row.try_get::<Option<String>, _>(1)? {
            let permissions = grants.roles.entry(role).or_default();
            permissions.extend(row.try_get::<Option<String>, _>(2)?);
        }
    }
    Ok(grants)
}

/// Read the current user, roles and permissions without choosing the transaction identity.
pub async fn principal_grants(
    connection: &mut sqlx::PgConnection,
    user_id: &wamn_identity_protocol::PrincipalId,
) -> Result<PrincipalGrants, sqlx::Error> {
    let rows = sqlx::query(PRINCIPAL_GRANTS_SQL)
        .bind(uuid::Uuid::from(*user_id))
        .fetch_all(connection)
        .await?;
    principal_grants_from_rows(&rows)
}

/// An absent environment row remains active.
pub async fn environment_inactive(
    connection: &mut sqlx::PgConnection,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar(ENVIRONMENT_INACTIVE_SQL)
        .fetch_optional(connection)
        .await?
        .unwrap_or(false))
}

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

/// Check one PAT on the host's platform login.
pub async fn project_pat(
    project: &mut sqlx::PgConnection,
    token: &PatToken,
) -> Result<Option<Arc<ProjectPat>>, sqlx::Error> {
    let row = sqlx::query(
        r#"SELECT u.id, u.sub, p.hash, p.expires_at
           FROM platform.pat p JOIN platform."user" u ON u.id=p.user_id
           WHERE p.prefix=$1 AND p.revoked_at IS NULL AND p.expires_at > now()
             AND u.issuer='wamn' AND u.type='service' AND u.status='active'"#,
    )
    .bind(token.lookup().to_string())
    .fetch_optional(project)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let hash =
        row.try_get::<&str, _>("hash")?
            .parse()
            .map_err(|source| sqlx::Error::ColumnDecode {
                index: "hash".into(),
                source: Box::new(source),
            })?;
    if !token.matches(&hash) {
        return Ok(None);
    }
    let deadline: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
        row.try_get("expires_at")?;
    Ok(Some(Arc::new(ProjectPat {
        principal: PrincipalId::from(row.try_get::<uuid::Uuid, _>("id")?),
        subject: row.try_get::<&str, _>("sub")?.into(),
        deadline: deadline.into(),
    })))
}

mod project_reads;
mod store;

pub use project_reads::{SessionIdentityError, SessionIdentityErrorType, read_guest_grants};
pub use store::Authorization;
