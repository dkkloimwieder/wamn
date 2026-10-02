//! The application rows of org and project administration in one project
//! database (docs/plan/platform-ui.md §4.4, application writes).
//!
//! The control host calls these with the environment's `wamn_administration`
//! login, in one transaction per environment that binds the caller and the
//! route's sealed operation id. A grant writes them after its system
//! transaction, and a revoke or a deactivation removes them before its system
//! transaction commits.

use tokio_postgres::GenericClient;

use wamn_project_state::ADMIN_ROLE;

use crate::{IdentityError, IdentityErrorType, PrincipalId, database_error};

/// One project database serves one tenant. Its platform rows, which
/// `reconcile-run-plane` writes first, name it.
const TENANT_SQL: &str =
    "SELECT DISTINCT tenant_id FROM app_system.users WHERE type = 'platform' ORDER BY 1";

const USER_ROW_SQL: &str = "INSERT INTO app_system.users \
    (tenant_id, id, type, email, display_name) VALUES ($1, $2::text::uuid, 'user', $3, $4) \
    ON CONFLICT (tenant_id, id) DO NOTHING";

const ROLE_SQL: &str = "INSERT INTO app_system.roles (tenant_id, name) VALUES ($1, $2) \
    ON CONFLICT (tenant_id, name) DO NOTHING";

const USER_ROLE_SQL: &str = "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) \
    VALUES ($1, $2::text::uuid, $3) ON CONFLICT DO NOTHING";

const DROP_USER_ROLE_SQL: &str = "DELETE FROM app_system.user_roles \
    WHERE tenant_id = $1 AND user_id = $2::text::uuid AND role_name = $3";

const DROP_USER_ROW_SQL: &str = "DELETE FROM app_system.users \
    WHERE tenant_id = $1 AND id = $2::text::uuid AND type = 'user'";

/// The user an application row names.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationUser<'a> {
    pub principal_id: &'a PrincipalId,
    pub email: &'a str,
    pub display_name: &'a str,
}

/// The tenant of the project database. A database without platform rows, or
/// with more than one tenant, is refused.
pub async fn environment_tenant(
    client: &(impl GenericClient + Sync),
) -> Result<String, IdentityError> {
    let tenants: Vec<String> = client
        .query(TENANT_SQL, &[])
        .await
        .map_err(|error| database_error(&error))?
        .iter()
        .map(|row| row.get(0))
        .collect();
    match <[String; 1]>::try_from(tenants) {
        Ok([tenant]) => Ok(tenant),
        Err(tenants) if tenants.is_empty() => Err(IdentityError::new(
            IdentityErrorType::NotFound,
            "the project database has no platform rows. Run reconcile-run-plane first",
        )),
        Err(tenants) => Err(IdentityError::new(
            IdentityErrorType::Conflict,
            format!(
                "the project database names {} tenants in its platform rows",
                tenants.len()
            ),
        )),
    }
}

/// Write the user row of `user`, and keep a row that exists.
pub async fn write_user(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    user: ApplicationUser<'_>,
) -> Result<(), IdentityError> {
    client
        .execute(
            USER_ROW_SQL,
            &[
                &tenant,
                &user.principal_id.as_str(),
                &user.email,
                &user.display_name,
            ],
        )
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Write the user row and the `admin` row of `user`, and the `admin` role
/// when the tenant has none yet.
pub async fn write_admin(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    user: ApplicationUser<'_>,
) -> Result<(), IdentityError> {
    write_user(client, tenant, user).await?;
    client
        .execute(ROLE_SQL, &[&tenant, &ADMIN_ROLE])
        .await
        .map_err(|error| database_error(&error))?;
    client
        .execute(
            USER_ROLE_SQL,
            &[&tenant, &user.principal_id.as_str(), &ADMIN_ROLE],
        )
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Remove the `admin` row of the principal. Its other roles stay.
pub async fn remove_admin(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    principal_id: &PrincipalId,
) -> Result<(), IdentityError> {
    client
        .execute(
            DROP_USER_ROLE_SQL,
            &[&tenant, &principal_id.as_str(), &ADMIN_ROLE],
        )
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Remove the user row of the principal. Its foreign keys remove its role
/// rows and its API keys.
pub async fn remove_user(
    client: &(impl GenericClient + Sync),
    tenant: &str,
    principal_id: &PrincipalId,
) -> Result<(), IdentityError> {
    client
        .execute(DROP_USER_ROW_SQL, &[&tenant, &principal_id.as_str()])
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}
