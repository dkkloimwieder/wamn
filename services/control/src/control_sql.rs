//! PostgreSQL operations used by the control service.

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::{
    EnvName, EnvStatus, OrgId, ProjectId, Triple, instance_suffix::InstanceSuffix,
};

/// The admission value a status command writes to one project database.
#[derive(Debug)]
pub(crate) struct StatusMirror {
    pub(crate) project: ProjectId,
    pub(crate) env: EnvName,
    pub(crate) status: EnvStatus,
}

/// Read the platform domain as stored, including an unset value.
///
/// # Errors
/// Returns an error if the registry metadata row cannot be read.
pub(crate) async fn platform_domain(
    connection: &mut PgConnection,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(wamn_identity::identity::app::service::PLATFORM_DOMAIN_SQL)
        .fetch_one(connection)
        .await
}

/// Read effective admission for active deployments in the selected project or env.
pub(crate) async fn status_mirrors(
    connection: &mut PgConnection,
    org: &OrgId,
    project: Option<&ProjectId>,
    env: Option<&EnvName>,
) -> Result<Vec<StatusMirror>, sqlx::Error> {
    sqlx::query_as::<_, (String, String, bool)>(
        "SELECT d.project, d.env, p.active AND e.active
         FROM registry.project_env d
         JOIN registry.project p ON p.org=d.org AND p.id=d.project
         JOIN registry.env e ON e.org=d.org AND e.env=d.env
         WHERE d.org=$1 AND d.active AND ($2::text IS NULL OR d.project=$2)
           AND ($3::text IS NULL OR d.env=$3)
         ORDER BY d.env, d.project",
    )
    .bind(org.as_str())
    .bind(project.map(ProjectId::as_str))
    .bind(env.map(EnvName::as_str))
    .fetch_all(connection)
    .await?
    .into_iter()
    .map(|(project, env, active)| {
        Ok(StatusMirror {
            project: wamn_postgres::decode_text(project, 0)?,
            env: wamn_postgres::decode_text(env, 1)?,
            status: if active {
                EnvStatus::Active
            } else {
                EnvStatus::Inactive
            },
        })
    })
    .collect()
}

/// List the active database generations for an organization.
///
/// # Errors
/// Returns an error if the query fails or a stored name cannot be decoded.
pub(crate) async fn project_deployments(
    pool: &PgPool,
    org: &OrgId,
) -> Result<Vec<(Triple, InstanceSuffix)>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT project, env, instance_suffix FROM registry.project_env WHERE org=$1 AND active ORDER BY project, env",
    )
    .bind(org.as_str())
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(project, env, instance)| {
            let project = wamn_postgres::decode_text(project, 0)?;
            let env = wamn_postgres::decode_text(env, 1)?;
            let instance = wamn_postgres::decode_text(instance, 2)?;
            Ok((Triple::new(org.clone(), env, project), instance))
        })
        .collect()
}

/// Count memberships in a project environment under the current transaction identity.
///
/// # Errors
/// Returns an error if the count cannot be read.
pub(crate) async fn membership_count(
    connection: &mut PgConnection,
    org: &OrgId,
    project: &ProjectId,
    env: &EnvName,
) -> Result<i32, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::int4 FROM identity.project_env_membership m \
         JOIN registry.project_env p USING (org, env, project) \
         WHERE m.org = $1 AND m.project = $2 AND m.env = $3 AND p.active",
    )
    .bind(org.as_str())
    .bind(project.as_str())
    .bind(env.as_str())
    .fetch_one(connection)
    .await
}

/// Count users with an active application role, and count active roles.
///
/// # Errors
/// Returns an error if either count cannot be read.
pub(crate) async fn app_counts(connection: &mut PgConnection) -> Result<(i32, i32), sqlx::Error> {
    sqlx::query_as(
        "SELECT \
         (SELECT count(*)::int4 FROM platform.\"user\" u \
          WHERE u.type = 'user' AND EXISTS \
            (SELECT 1 FROM platform.user_role r JOIN platform.role a ON a.name = r.role_name \
             WHERE r.user_id = u.id AND a.active)), \
         (SELECT count(*)::int4 FROM platform.role WHERE active)",
    )
    .fetch_one(connection)
    .await
}

/// Change the project flag within the caller's org-admin transaction.
pub(crate) async fn set_project_status(
    connection: &mut PgConnection,
    org: &OrgId,
    project: &ProjectId,
    status: EnvStatus,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE registry.project SET active=$3 WHERE org=$1 AND id=$2")
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(status == EnvStatus::Active)
        .execute(connection)
        .await?;
    Ok(result.rows_affected() != 0)
}

/// Check that the current transaction can read project role principals.
///
/// # Errors
/// Returns an error if the rows cannot be read or a principal cannot be decoded.
pub(crate) async fn check_project_roles(
    connection: &mut PgConnection,
    org: &OrgId,
    project: &ProjectId,
) -> Result<(), sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT principal_id FROM identity.project_role WHERE org=$1 AND project=$2",
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
pub(crate) async fn principal_active(
    connection: &mut PgConnection,
    principal: &PrincipalId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT active FROM identity.principal WHERE id = $1")
        .bind(Uuid::from(*principal))
        .fetch_one(connection)
        .await
}

/// Check that the current database login is registered for the organization.
///
/// # Errors
/// Returns an error if the login check cannot be read.
pub(crate) async fn login_matches_org(
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
pub(crate) async fn lock_first_owner(
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
pub(crate) async fn org_role_holders(
    connection: &mut PgConnection,
    org: &OrgId,
    role: &str,
    email: &str,
) -> Result<(bool, bool), sqlx::Error> {
    sqlx::query_as(
        "SELECT count(*) > 0, COALESCE(bool_or(p.email = $3), false)
           FROM identity.org_role r LEFT JOIN identity.principal p ON p.id = r.principal_id
          WHERE r.org = $1 AND r.role = $2",
    )
    .bind(org.as_str())
    .bind(role)
    .bind(email)
    .fetch_one(connection)
    .await
}

/// Prune transaction identities and set the five-second statement timeout for this transaction.
///
/// # Errors
/// Returns an error if either database statement fails.
pub(crate) async fn prune_transaction_identities(
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
