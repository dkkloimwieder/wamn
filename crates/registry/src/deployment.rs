//! Typed reads and writes of environment placement and project registration.

use sqlx::PgConnection;

use crate::{Triple, instance_suffix::InstanceSuffix, sql};

/// Values recorded when provisioning a project deployment.
#[derive(Debug)]
pub struct Registration<'a> {
    pub triple: &'a Triple,
    pub secret_name: &'a str,
    pub secret_namespace: Option<&'a str>,
    pub instance_suffix: &'a InstanceSuffix,
    pub disposable: Option<bool>,
    pub policy: &'a crate::EnvName,
    pub target_id: &'a str,
}

/// Read the stored instance suffix, if the project deployment exists.
///
/// # Errors
/// Returns database errors.
pub async fn instance_suffix(
    connection: &mut PgConnection,
    triple: &Triple,
) -> Result<Option<InstanceSuffix>, sqlx::Error> {
    sqlx::query_scalar("SELECT instance_suffix FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3")
        .bind(&triple.org).bind(&triple.project).bind(&triple.env).fetch_optional(connection).await
}

/// Read the policy selected for a project deployment, if it exists.
///
/// # Errors
/// Returns database errors.
pub async fn project_policy(
    connection: &mut PgConnection,
    triple: &Triple,
) -> Result<Option<crate::EnvName>, sqlx::Error> {
    sqlx::query_scalar(
        sql::CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_READ_PROJECT_ENV_POLICY,
    )
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(&triple.env)
    .fetch_optional(connection)
    .await
}

/// Read the environment's policy.
///
/// # Errors
/// Returns database errors.
pub async fn environment_policy(
    connection: &mut PgConnection,
    org: &crate::OrgId,
    env: &crate::EnvName,
) -> Result<crate::EnvName, sqlx::Error> {
    sqlx::query_scalar("SELECT policy_name FROM registry.environments WHERE org=$1 AND env=$2")
        .bind(org)
        .bind(env)
        .fetch_one(connection)
        .await
}

/// Read the projects deployed in an environment, ordered by project name.
///
/// # Errors
/// Returns database errors.
pub async fn projects(
    connection: &mut PgConnection,
    org: &crate::OrgId,
    env: &crate::EnvName,
) -> Result<Vec<crate::ProjectId>, sqlx::Error> {
    sqlx::query_scalar("SELECT project FROM registry.environment_projects WHERE org=$1 AND env=$2 ORDER BY project")
        .bind(org).bind(env).fetch_all(connection).await
}

/// Read whether a project deployment's environment permits resetting its database.
///
/// # Errors
/// Returns database errors.
pub async fn disposable(
    connection: &mut PgConnection,
    triple: &Triple,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT disposable FROM registry.environment_projects JOIN registry.environments USING (org,env) WHERE org=$1 AND project=$2 AND env=$3")
        .bind(&triple.org).bind(&triple.project).bind(&triple.env).fetch_one(connection).await
}

/// Read whether the project deployment is registered.
///
/// # Errors
/// Returns database errors.
pub async fn exists(connection: &mut PgConnection, triple: &Triple) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3)")
        .bind(&triple.org).bind(&triple.project).bind(&triple.env).fetch_one(connection).await
}

/// Record a project without replacing its existing definition.
///
/// # Errors
/// Returns database errors.
pub async fn record_project(
    connection: &mut PgConnection,
    org: &crate::OrgId,
    project: &crate::ProjectId,
) -> Result<(), sqlx::Error> {
    sqlx::query(sql::upsert_project_sql())
        .bind(org)
        .bind(project)
        .execute(connection)
        .await?;
    Ok(())
}

/// Find a deployment target by name.
///
/// # Errors
/// Returns database errors.
pub async fn target_id(
    connection: &mut PgConnection,
    name: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        sql::CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_DO_RECORD_PROJECT_ENV,
    )
    .bind(name)
    .fetch_optional(connection)
    .await
}

/// Record a deployment and return its preserved or newly minted instance suffix.
///
/// # Errors
/// Returns database errors.
pub async fn record(
    connection: &mut PgConnection,
    registration: &Registration<'_>,
) -> Result<InstanceSuffix, sqlx::Error> {
    let r = registration;
    let (suffix, _): (InstanceSuffix, bool) =
        sqlx::query_as(sql::upsert_project_env_with_policy_sql())
            .bind(&r.triple.org)
            .bind(&r.triple.project)
            .bind(&r.triple.env)
            .bind(r.secret_name)
            .bind(r.secret_namespace)
            .bind(r.instance_suffix)
            .bind(r.disposable)
            .bind(r.policy)
            .bind(r.target_id)
            .fetch_one(connection)
            .await?;
    Ok(suffix)
}

/// Serialize changes to an organization's initial administrator.
///
/// # Errors
/// Returns a database error if the organization is absent or cannot be locked.
pub async fn lock_org(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &crate::OrgId,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT id FROM registry.orgs WHERE id=$1 FOR UPDATE")
        .bind(org)
        .fetch_one(&mut **transaction)
        .await?;
    Ok(())
}
