//! Typed reads and writes of environment placement and project registration.

use sqlx::PgConnection;

use crate::{Triple, sql};

/// Physical placement recorded for one project deployment.
#[derive(Debug)]
pub struct DatabasePlacement {
    pub instance_suffix: String,
    pub placement_type: String,
    pub pool_cluster: Option<String>,
    pub policy_name: String,
}

/// An organization's database placement.
#[derive(Debug)]
pub struct OrgPlacement {
    pub placement_type: String,
    pub pool_cluster: Option<String>,
}

/// Values recorded when provisioning a project deployment.
#[derive(Debug)]
pub struct Registration<'a> {
    pub triple: &'a Triple,
    pub secret_name: &'a str,
    pub secret_namespace: Option<&'a str>,
    pub instance_suffix: &'a str,
    pub disposable: Option<bool>,
    pub policy: &'a str,
    pub target_id: &'a str,
}

/// Read the physical database placement of a provisioned project.
///
/// # Errors
/// Returns database errors.
pub async fn database_placement(
    connection: &mut PgConnection,
    triple: &Triple,
) -> Result<DatabasePlacement, sqlx::Error> {
    let (instance_suffix, placement_type, pool_cluster, policy_name) = sqlx::query_as(
        "SELECT ep.instance_suffix, o.placement_type, o.pool_cluster, e.policy_name FROM registry.environment_projects ep JOIN registry.orgs o ON o.id=ep.org JOIN registry.environments e USING(org,env) WHERE ep.org=$1 AND ep.project=$2 AND ep.env=$3",
    )
    .bind(&triple.org).bind(&triple.project).bind(triple.env.as_str())
    .fetch_one(connection).await?;
    Ok(DatabasePlacement {
        instance_suffix,
        placement_type,
        pool_cluster,
        policy_name,
    })
}

/// Read an organization's placement, if it is registered.
///
/// # Errors
/// Returns database errors.
pub async fn org_placement(
    connection: &mut PgConnection,
    org: &str,
) -> Result<Option<OrgPlacement>, sqlx::Error> {
    Ok(
        sqlx::query_as::<_, (String, Option<String>)>(sql::select_org_placement_sql())
            .bind(org)
            .fetch_optional(connection)
            .await?
            .map(|(placement_type, pool_cluster)| OrgPlacement {
                placement_type,
                pool_cluster,
            }),
    )
}

/// Read the stored instance suffix, if the project deployment exists.
///
/// # Errors
/// Returns database errors.
pub async fn instance_suffix(
    connection: &mut PgConnection,
    triple: &Triple,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT instance_suffix FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3")
        .bind(&triple.org).bind(&triple.project).bind(triple.env.as_str()).fetch_optional(connection).await
}

/// Read the policy selected for a project deployment, if it exists.
///
/// # Errors
/// Returns database errors.
pub async fn project_policy(
    connection: &mut PgConnection,
    triple: &Triple,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        sql::CONTROL_ENVIRONMENT_SRC_PROVISION_PROJECT_ENV_REGISTRY_READ_PROJECT_ENV_POLICY,
    )
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(triple.env.as_str())
    .fetch_optional(connection)
    .await
}

/// Read the environment's policy.
///
/// # Errors
/// Returns database errors.
pub async fn environment_policy(
    connection: &mut PgConnection,
    org: &str,
    env: &str,
) -> Result<String, sqlx::Error> {
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
    org: &str,
    env: &str,
) -> Result<Vec<String>, sqlx::Error> {
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
        .bind(&triple.org).bind(&triple.project).bind(triple.env.as_str()).fetch_one(connection).await
}

/// Read whether the project deployment is registered.
///
/// # Errors
/// Returns database errors.
pub async fn exists(connection: &mut PgConnection, triple: &Triple) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3)")
        .bind(&triple.org).bind(&triple.project).bind(triple.env.as_str()).fetch_one(connection).await
}

/// Record a project without replacing its existing definition.
///
/// # Errors
/// Returns database errors.
pub async fn record_project(
    connection: &mut PgConnection,
    org: &str,
    project: &str,
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
) -> Result<String, sqlx::Error> {
    let r = registration;
    let (suffix, _): (String, bool) = sqlx::query_as(sql::upsert_project_env_with_policy_sql())
        .bind(&r.triple.org)
        .bind(&r.triple.project)
        .bind(r.triple.env.as_str())
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
