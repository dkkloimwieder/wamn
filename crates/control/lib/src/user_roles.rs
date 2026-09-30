//! `grant-role` and `revoke-role`: give a person or a service one user role
//! of an environment, or take it away.
//!
//! `apply-package` writes the built-in role `admin`, and every other role is
//! authored. This module writes only `app_system.user_roles`. The target is resolved in the
//! registry the way `reconcile-run-plane` resolves it, because a role grant
//! names an environment, not a database. The user is named by email and must
//! already have its tenant `users` row, which `reconcile-run-plane` writes.

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::{
    PlatformComponent, bind_platform_principal_sql, project_env_database_name, validate_project_env,
};
use wamn_control_registry::Triple;

/// One grant or revoke of a user role in one environment.
#[derive(Debug, Clone)]
pub struct UserRoleRequest {
    /// Administrative Postgres URL to the system registry.
    pub system_database_url: String,
    /// Administrative Postgres URL to the registry-derived project database.
    /// It must be SUPERUSER or BYPASSRLS, because the tables force RLS.
    pub admin_database_url: String,
    /// Registry organization.
    pub org: String,
    /// Registry project.
    pub project: String,
    /// Registry environment.
    pub env: String,
    /// Tenant of the environment.
    pub tenant: String,
    /// Email of the person or service in the tenant `users` table.
    pub user: String,
    /// A role that exists in the tenant, such as `admin`.
    pub role: String,
}

/// What a grant or revoke changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRoleOutcome {
    /// The id of the resolved user.
    pub user_id: String,
    /// Whether a `user_roles` row was written or removed.
    pub changed: bool,
}

/// Give the user the role. A second grant of the same role changes nothing.
pub async fn grant_role(request: &UserRoleRequest) -> anyhow::Result<UserRoleOutcome> {
    change_role(request, Change::Grant).await
}

/// Take the role from the user. A revoke of a role the user does not hold
/// changes nothing.
pub async fn revoke_role(request: &UserRoleRequest) -> anyhow::Result<UserRoleOutcome> {
    change_role(request, Change::Revoke).await
}

#[derive(Clone, Copy)]
enum Change {
    Grant,
    Revoke,
}

async fn change_role(request: &UserRoleRequest, change: Change) -> anyhow::Result<UserRoleOutcome> {
    let (mut client, connection) = connect_target(&EnvironmentTarget {
        system_database_url: request.system_database_url.clone(),
        admin_database_url: request.admin_database_url.clone(),
        org: request.org.clone(),
        project: request.project.clone(),
        env: request.env.clone(),
        tenant: request.tenant.clone(),
    })
    .await?;
    let result = change_user_role(
        &mut client,
        &request.tenant,
        &request.user,
        &request.role,
        change,
    )
    .await;
    drop(client);
    let _ = connection.await;
    result
}

/// Give the role to the user of `tenant` with the email `user`, on a
/// connection the caller already checked. The development loop uses it on its
/// cloned target database, which the registry does not name.
pub async fn grant_role_on(
    client: &mut Client,
    tenant: &str,
    user: &str,
    role: &str,
) -> anyhow::Result<UserRoleOutcome> {
    change_user_role(client, tenant, user, role, Change::Grant).await
}

async fn change_user_role(
    client: &mut Client,
    tenant: &str,
    user: &str,
    role: &str,
    change: Change,
) -> anyhow::Result<UserRoleOutcome> {
    anyhow::ensure!(!role.is_empty(), "--role must not be empty");
    anyhow::ensure!(!tenant.is_empty(), "--tenant must not be empty");
    anyhow::ensure!(!user.is_empty(), "--user must not be empty");
    let transaction = client
        .transaction()
        .await
        .context("open the user role transaction")?;
    let user_id = resolve_user(&transaction, tenant, user).await?;
    transaction
        .batch_execute(&bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        ))
        .await
        .context("bind wamn:provisioning for the role change")?;
    let changed = match change {
        Change::Grant => {
            let role_exists = transaction
                .query_opt(
                    "SELECT 1 FROM app_system.roles WHERE tenant_id = $1 AND name = $2",
                    &[&tenant, &role],
                )
                .await
                .context("read the role row")?
                .is_some();
            anyhow::ensure!(
                role_exists,
                "role {role} does not exist in tenant {tenant}: apply-package writes \
                 admin, and every other role is authored"
            );
            transaction
                .execute(
                    "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) \
                     VALUES ($1, $2::text::uuid, $3) ON CONFLICT DO NOTHING",
                    &[&tenant, &user_id, &role],
                )
                .await
                .context("write the user role row")?
        }
        Change::Revoke => transaction
            .execute(
                "DELETE FROM app_system.user_roles \
                 WHERE tenant_id = $1 AND user_id = $2::text::uuid AND role_name = $3",
                &[&tenant, &user_id, &role],
            )
            .await
            .context("remove the user role row")?,
    };
    transaction
        .commit()
        .await
        .context("commit the user role change")?;
    Ok(UserRoleOutcome {
        user_id,
        changed: changed == 1,
    })
}

/// Resolve the email to exactly one user of the tenant. Email matching ignores
/// case, so two rows that differ only in case make the email ambiguous.
async fn resolve_user(
    transaction: &tokio_postgres::Transaction<'_>,
    tenant: &str,
    user: &str,
) -> anyhow::Result<String> {
    let rows = transaction
        .query(
            "SELECT id::text FROM app_system.users \
             WHERE tenant_id = $1 AND lower(email) = lower($2) ORDER BY id",
            &[&tenant, &user],
        )
        .await
        .context("resolve the user email")?;
    match rows.as_slice() {
        [] => anyhow::bail!(
            "no user with email {user} in tenant {tenant}: the users row comes from \
             reconcile-run-plane, so run it after the principal exists"
        ),
        [row] => Ok(row.get(0)),
        _ => anyhow::bail!(
            "email {user} names {} users in tenant {tenant}; the grant needs exactly one",
            rows.len()
        ),
    }
}

/// One environment named in the registry, with the administrative URLs of the
/// role and permission verbs.
#[derive(Debug, Clone)]
pub struct EnvironmentTarget {
    /// Administrative Postgres URL to the system registry.
    pub system_database_url: String,
    /// Administrative Postgres URL to the registry-derived project database.
    /// It must be SUPERUSER or BYPASSRLS, because the tables force RLS.
    pub admin_database_url: String,
    /// Registry organization.
    pub org: String,
    /// Registry project.
    pub project: String,
    /// Registry environment.
    pub env: String,
    /// Tenant of the environment.
    pub tenant: String,
}

/// Connect to the project database and make sure that it is the database the
/// registry records for the environment.
pub(crate) async fn connect_target(
    request: &EnvironmentTarget,
) -> anyhow::Result<(
    Client,
    tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
)> {
    let triple = Triple::new(&request.org, &request.project, request.env.as_str());
    validate_project_env(&request.org, &request.project, &request.env)
        .with_context(|| format!("registry target identity {triple} is invalid"))?;
    let instance = crate::provision_project_env::read_project_env_instance(
        &request.system_database_url,
        &triple,
    )
    .await
    .with_context(|| format!("registry target {triple} has no usable recorded instance"))?;
    let expected =
        project_env_database_name(&request.org, &request.project, &request.env, &instance);
    let (client, connection) = tokio_postgres::connect(&request.admin_database_url, NoTls)
        .await
        .with_context(|| format!("database target for registry triple {triple} did not connect"))?;
    let connection = tokio::spawn(connection);
    let checked = async {
        let row = client
            .query_one(
                "SELECT pg_catalog.current_database()::text, \
                        (SELECT rolsuper OR rolbypassrls FROM pg_catalog.pg_roles \
                          WHERE rolname = CURRENT_USER)",
                &[],
            )
            .await
            .context("identify the database target")?;
        let actual: String = row.get(0);
        anyhow::ensure!(
            actual == expected,
            "database target for registry triple {triple} is {actual}, but the registry \
             records {expected}"
        );
        anyhow::ensure!(
            row.get::<_, bool>(1),
            "the role verbs require SUPERUSER or BYPASSRLS, because app_system forces RLS"
        );
        Ok(())
    }
    .await;
    match checked {
        Ok(()) => Ok((client, connection)),
        Err(error) => {
            drop(client);
            let _ = connection.await;
            Err(error)
        }
    }
}
