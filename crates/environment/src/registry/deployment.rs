use wamn_registry::{Triple, deployment::Registration, instance_suffix::InstanceSuffix};

/// Read the stored instance suffix, if the project deployment exists.
pub async fn instance_suffix(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<Option<InstanceSuffix>, sqlx::Error> {
    let suffix = sqlx::query_scalar::<_, String>("SELECT instance_suffix FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3")
        .bind(triple.org.as_str()).bind(triple.project.as_str()).bind(triple.env.as_str()).fetch_optional(&mut *connection).await?;
    suffix
        .map(|value| wamn_postgres::decode_text(value, 0))
        .transpose()
}

/// Read the environment's host drain bound in seconds.
pub async fn drain_bound_seconds(
    connection: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    env: &wamn_registry::EnvName,
) -> Result<i32, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT drain_bound_seconds FROM registry.environments WHERE org=$1 AND env=$2",
    )
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_one(&mut *connection)
    .await
}

/// Read the projects deployed in an environment, ordered by project name.
pub async fn projects(
    connection: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    env: &wamn_registry::EnvName,
) -> Result<Vec<wamn_registry::ProjectId>, sqlx::Error> {
    let projects = sqlx::query_scalar::<_, String>("SELECT project FROM registry.environment_projects WHERE org=$1 AND env=$2 ORDER BY project")
        .bind(org.as_str()).bind(env.as_str()).fetch_all(&mut *connection).await?;
    projects
        .into_iter()
        .map(|value| wamn_postgres::decode_text(value, 0))
        .collect()
}

/// Read whether the project deployment's environment permits a database reset.
pub async fn disposable(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT disposable FROM registry.environment_projects JOIN registry.environments USING (org,env) WHERE org=$1 AND project=$2 AND env=$3")
        .bind(triple.org.as_str()).bind(triple.project.as_str()).bind(triple.env.as_str()).fetch_one(&mut *connection).await
}

/// Read whether the project deployment is registered.
pub async fn exists(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3)")
        .bind(triple.org.as_str()).bind(triple.project.as_str()).bind(triple.env.as_str()).fetch_one(&mut *connection).await
}

/// Record a project without replacing its existing definition.
pub async fn record_project(
    connection: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
) -> Result<(), sqlx::Error> {
    sqlx::query(upsert_project_sql())
        .bind(org.as_str())
        .bind(project.as_str())
        .execute(&mut *connection)
        .await?;
    Ok(())
}

/// Find a deployment target by name.
pub async fn target_id(
    connection: &mut sqlx::PgConnection,
    name: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT id::text FROM registry.target WHERE name = $1")
        .bind(name)
        .fetch_optional(&mut *connection)
        .await
}

/// Record a deployment and return its preserved or newly minted instance suffix.
pub async fn record(
    connection: &mut sqlx::PgConnection,
    registration: &Registration<'_>,
) -> Result<InstanceSuffix, sqlx::Error> {
    let r = registration;
    let (suffix, _): (String, bool) = sqlx::query_as(upsert_project_env_sql())
        .bind(r.triple.org.as_str())
        .bind(r.triple.project.as_str())
        .bind(r.triple.env.as_str())
        .bind(r.secret_name)
        .bind(r.secret_namespace)
        .bind(r.instance_suffix.as_str())
        .bind(r.disposable)
        .bind(r.target_id)
        .fetch_one(&mut *connection)
        .await?;
    wamn_postgres::decode_text(suffix, 0)
}

/// Serialize changes to an organization's initial administrator.
pub async fn lock_org(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &wamn_registry::OrgId,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT id FROM registry.orgs WHERE id=$1 FOR UPDATE")
        .bind(org.as_str())
        .fetch_one(&mut **transaction)
        .await?;
    Ok(())
}

/// Upsert a project row into `registry.projects` (idempotent). Params: `$1` org,
/// `$2` id. `ON CONFLICT (org, id) DO NOTHING` — a project carries no mutable
/// placement of its own (placement is per-env), so re-provisioning is a no-op.
fn upsert_project_sql() -> &'static str {
    "INSERT INTO registry.projects (org, id) VALUES ($1, $2) \
     ON CONFLICT (org, id) DO NOTHING"
}

/// Record the environment, then its project database. Parameters follow the caller's
/// org, project, environment, Secret, namespace, suffix, disposable, and target values.
fn upsert_project_env_sql() -> &'static str {
    "WITH parent AS (INSERT INTO registry.environments \
       (org,env,disposable,target_id) \
       VALUES ($1,$3,COALESCE($7,false),$8::text::uuid) \
       ON CONFLICT (org,env) DO UPDATE SET disposable=COALESCE($7,registry.environments.disposable), \
         target_id=EXCLUDED.target_id RETURNING org,env,disposable), \
     deployment AS (INSERT INTO registry.environment_projects \
       (org,env,project,secret_name,secret_namespace,instance_suffix) \
       SELECT org,env,$2,$4,$5,$6 FROM parent \
       ON CONFLICT (org,env,project) DO UPDATE SET secret_name=EXCLUDED.secret_name, \
         secret_namespace=EXCLUDED.secret_namespace RETURNING instance_suffix) \
     SELECT deployment.instance_suffix,parent.disposable FROM deployment CROSS JOIN parent"
}
