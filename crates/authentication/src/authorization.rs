//! Authorization reads owned by authentication, over a host-authorized connection.

use sqlx::Row as _;
use std::collections::BTreeSet;
use std::sync::Arc;
use wamn_capability_authority::auth_cache::PrincipalGrants;

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
    WHERE u.id = $1::text::uuid";

const USER_OPERATION_PERMISSIONS_SQL: &str = "SELECT user_role.role_name, permission.permission \
    FROM platform.\"user\" AS \"user\" \
    JOIN platform.user_role AS user_role \
      ON user_role.user_id = \"user\".id \
    LEFT JOIN platform.permission AS permission \
      ON permission.role_name = user_role.role_name \
    WHERE \"user\".id = $1::text::uuid AND \"user\".status = 'active'";

fn principal_grants_from_rows(
    rows: &[sqlx::postgres::PgRow],
) -> Result<wamn_capability_authority::auth_cache::PrincipalGrants, sqlx::Error> {
    let mut grants = wamn_capability_authority::auth_cache::PrincipalGrants {
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
    user_id: &str,
) -> Result<PrincipalGrants, sqlx::Error> {
    let rows = sqlx::query(PRINCIPAL_GRANTS_SQL)
        .bind(user_id)
        .fetch_all(connection)
        .await?;
    principal_grants_from_rows(&rows)
}

/// Read the active user's role and permission rows for a host route.
pub async fn user_operation_permissions(
    connection: &mut sqlx::PgConnection,
    user_id: &str,
) -> Result<HeldOperationGrants, sqlx::Error> {
    let rows = sqlx::query(USER_OPERATION_PERMISSIONS_SQL)
        .bind(user_id)
        .fetch_all(connection)
        .await?;
    HeldOperationGrants::from_rows(&rows)
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
    /// Fold rows of `(role_name, permission)`, where a null permission is a
    /// role without permissions. The held references are the rows.
    pub fn from_rows(rows: &[sqlx::postgres::PgRow]) -> Result<Self, sqlx::Error> {
        let mut held = Self::default();
        for row in rows {
            let role: String = row.try_get(0)?;
            held.admin |= role == wamn_identity_protocol::ADMIN_ROLE;
            if let Some(reference) = row.try_get::<Option<String>, _>(1)? {
                held.references.insert(reference);
            }
            held.roles.insert(role);
        }
        Ok(held)
    }

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

/// A session presents authority that its principal does not currently hold.
#[derive(Debug)]
pub struct SessionGrantRefusal {
    type_: &'static str,
}

impl std::fmt::Display for SessionGrantRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.type_)
    }
}

impl std::error::Error for SessionGrantRefusal {}

/// The grants an active principal holds, from all its roles or only from the
/// `signed` roles it still holds.
pub fn session_grants(
    grants: &wamn_capability_authority::auth_cache::PrincipalGrants,
    roles: &[wamn_identity_protocol::RoleName],
) -> anyhow::Result<HeldOperationGrants> {
    if !grants.active || !grants.user {
        return Err(SessionGrantRefusal {
            type_: "session principal is not an active user",
        }
        .into());
    }
    if !roles
        .iter()
        .all(|role| grants.roles.contains_key(role.as_str()))
    {
        return Err(SessionGrantRefusal {
            type_: "session principal does not hold a signed role",
        }
        .into());
    }
    Ok(held_grants(grants, Some(roles)))
}

pub fn held_grants(
    grants: &wamn_capability_authority::auth_cache::PrincipalGrants,
    signed: Option<&[wamn_identity_protocol::RoleName]>,
) -> HeldOperationGrants {
    let mut held = HeldOperationGrants {
        user: grants.user,
        ..HeldOperationGrants::default()
    };
    if !grants.active || !grants.present {
        return held;
    }
    for (role, permissions) in &grants.roles {
        if signed.is_some_and(|signed| !signed.iter().any(|signed| signed.as_str() == role)) {
            continue;
        }
        held.admin |= role == wamn_identity_protocol::ADMIN_ROLE;
        held.references.extend(permissions.iter().cloned());
        held.roles.insert(role.clone());
    }
    held
}

/// Host transport and authorization memory used by route authentication.
#[async_trait::async_trait]
pub trait AuthorizationStore: std::fmt::Debug + Send + Sync {
    fn set_hierarchy_reader(&self, key: &wamn_registry::ProjectEnvKey, reader: sqlx::PgPool);
    fn auth_cache(&self) -> &Arc<wamn_capability_authority::auth_cache::AuthCache>;
    async fn environment_inactive(
        &self,
        project: &str,
        key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<bool>;
    async fn session_operation_permissions(
        &self,
        project: &str,
        key: &wamn_registry::ProjectEnvKey,
        roles: &[wamn_identity_protocol::RoleName],
        principal: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<HeldOperationGrants>;
    async fn user_operation_permissions(
        &self,
        project: &str,
        key: &wamn_registry::ProjectEnvKey,
        principal: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<HeldOperationGrants>;
    async fn principal_grants(
        &self,
        project: &str,
        key: &wamn_registry::ProjectEnvKey,
        principal: &wamn_identity_protocol::PrincipalId,
    ) -> anyhow::Result<PrincipalGrants>;
}
