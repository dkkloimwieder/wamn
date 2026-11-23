//! PostgreSQL operations used by the control service.

use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;
use wamn_principal::PrincipalId;
use wamn_registry::{EnvName, OrgId, ProjectId, Triple, instance_suffix::InstanceSuffix};

/// The platform domain used in service principal email addresses.
pub const PLATFORM_DOMAIN_SQL: &str = "SELECT platform_domain FROM registry.meta";

// Every project deployment affected by an environment lifecycle change.
const ENVIRONMENT_PROJECTS: &str = "SELECT project,env FROM registry.environment_projects WHERE org=$1 AND env=ANY($2) ORDER BY env,project";

/// Read the platform domain as stored, including an unset value.
///
/// # Errors
/// Returns an error if the registry metadata row cannot be read.
pub async fn platform_domain(connection: &mut PgConnection) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(PLATFORM_DOMAIN_SQL)
        .fetch_one(connection)
        .await
}

/// Find project deployments for the selected organization and environments, in update order.
///
/// # Errors
/// Returns an error if the query fails or a coordinate is invalid.
pub async fn environment_projects(
    connection: &mut PgConnection,
    org: &OrgId,
    envs: &[EnvName],
) -> Result<Vec<(ProjectId, EnvName)>, sqlx::Error> {
    let names: Vec<_> = envs.iter().map(EnvName::as_str).collect();
    sqlx::query_as::<_, (String, String)>(ENVIRONMENT_PROJECTS)
        .bind(org.as_str())
        .bind(names)
        .fetch_all(connection)
        .await?
        .into_iter()
        .map(|(project, env)| Ok((super::decode_text(project, 0)?, super::decode_text(env, 1)?)))
        .collect()
}

/// List the database generations recorded for an organization's project environments.
///
/// # Errors
/// Returns an error if the query fails or a stored name cannot be decoded.
pub async fn project_deployments(
    pool: &PgPool,
    org: &OrgId,
) -> Result<Vec<(Triple, InstanceSuffix)>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT project, env, instance_suffix FROM registry.environment_projects WHERE org=$1",
    )
    .bind(org.as_str())
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(project, env, instance)| {
            let project = super::decode_text(project, 0)?;
            let env = super::decode_text(env, 1)?;
            let instance = super::decode_text(instance, 2)?;
            Ok((Triple::new(org.clone(), env, project), instance))
        })
        .collect()
}

/// Count memberships in a project environment under the current transaction identity.
///
/// # Errors
/// Returns an error if the count cannot be read.
pub async fn membership_count(
    connection: &mut PgConnection,
    org: &OrgId,
    project: &ProjectId,
    env: &EnvName,
) -> Result<i32, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::int4 FROM identity.project_env_memberships \
         WHERE org = $1 AND project = $2 AND env = $3",
    )
    .bind(org.as_str())
    .bind(project.as_str())
    .bind(env.as_str())
    .fetch_one(connection)
    .await
}

/// Count users with at least one application role, and count the defined roles.
///
/// # Errors
/// Returns an error if either count cannot be read.
pub async fn application_counts(connection: &mut PgConnection) -> Result<(i32, i32), sqlx::Error> {
    sqlx::query_as(
        "SELECT \
         (SELECT count(*)::int4 FROM platform.\"user\" u \
          WHERE u.type = 'user' AND EXISTS \
            (SELECT 1 FROM platform.user_role r WHERE r.user_id = u.id)), \
         (SELECT count(*)::int4 FROM platform.role r)",
    )
    .fetch_one(connection)
    .await
}

/// Check that the current transaction can read project role principals.
///
/// # Errors
/// Returns an error if the rows cannot be read or a principal cannot be decoded.
pub async fn check_project_roles(
    connection: &mut PgConnection,
    org: &OrgId,
    project: &ProjectId,
) -> Result<(), sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT principal_id FROM identity.project_roles WHERE org=$1 AND project=$2",
    )
    .bind(org.as_str())
    .bind(project.as_str())
    .fetch_all(connection)
    .await?;
    Ok(())
}

/// Read the current principal state after acquiring any required principal lock.
///
/// # Errors
/// Returns an error if the principal row cannot be read.
pub async fn principal_active(
    connection: &mut PgConnection,
    principal: &PrincipalId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT status = 'active' FROM identity.principals WHERE id = $1")
        .bind(Uuid::from(*principal))
        .fetch_one(connection)
        .await
}

/// Check that the current database login is registered for the organization.
///
/// # Errors
/// Returns an error if the login check cannot be read.
pub async fn login_matches_org(
    connection: &mut PgConnection,
    org: &OrgId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT COALESCE(registry.login_org() = $1, false)")
        .bind(org.as_str())
        .fetch_one(connection)
        .await
}

/// Serialize first-owner requests across control processes for this organization.
///
/// # Errors
/// Returns an error if the transaction lock cannot be acquired.
pub async fn lock_first_owner(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &OrgId,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("wamn:first-owner:{org}"))
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// Read whether the role has any holders and whether the given email holds it.
///
/// # Errors
/// Returns an error if the role rows cannot be read.
pub async fn org_role_holders(
    connection: &mut PgConnection,
    org: &OrgId,
    role: &str,
    email: &str,
) -> Result<(bool, bool), sqlx::Error> {
    sqlx::query_as(
        "SELECT count(*) > 0, COALESCE(bool_or(p.email = $3), false)
           FROM identity.org_roles r LEFT JOIN identity.principals p ON p.id = r.principal_id
          WHERE r.org = $1 AND r.role = $2",
    )
    .bind(org.as_str())
    .bind(role)
    .bind(email)
    .fetch_one(connection)
    .await
}

/// Run the registry operation for an integration type.
///
/// # Errors
/// Returns an error if the registry operation fails.
pub async fn configure_integration_type(
    connection: &mut PgConnection,
    org: &OrgId,
    operation: &str,
    request: &Value,
) -> Result<Value, sqlx::Error> {
    sqlx::query_scalar("SELECT registry.configure_integration_type($1,$2,$3)")
        .bind(org.as_str())
        .bind(operation)
        .bind(request)
        .fetch_one(connection)
        .await
}

/// Try to take a session lock that excludes concurrent environment deployment.
///
/// # Errors
/// Returns an error if the database cannot attempt the lock.
pub async fn try_environment_lock(
    connection: &mut PgConnection,
    org: &OrgId,
    env: &EnvName,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(super::ENVIRONMENT_LOCK_SQL)
        .bind(org.as_str())
        .bind(env.as_str())
        .fetch_one(connection)
        .await
}

/// Release session advisory locks after a control write.
///
/// # Errors
/// Returns an error if the database cannot release the locks.
pub async fn unlock_all_advisory_locks(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_unlock_all()")
        .fetch_one(connection)
        .await?;
    Ok(())
}

/// Prune transaction identities and set the five-second statement timeout for this transaction.
///
/// # Errors
/// Returns an error if either database statement fails.
pub async fn prune_transaction_identities(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), sqlx::Error> {
    sqlx::raw_sql("SET LOCAL statement_timeout = '5s'")
        .execute(&mut **transaction)
        .await?;
    sqlx::query("SELECT platform.prune_tx_identity()")
        .execute(&mut **transaction)
        .await?;
    Ok(())
}
