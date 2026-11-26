use wamn_registry::{Triple, deployment::Registration, instance_suffix::InstanceSuffix};

/// Read the stored instance suffix, if the project deployment is active.
pub async fn instance_suffix(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<Option<InstanceSuffix>, sqlx::Error> {
    let suffix = sqlx::query_scalar::<_, String>(
        "SELECT instance_suffix FROM registry.project_env WHERE org=$1 AND project=$2 AND env=$3 AND active",
    )
    .bind(triple.org.as_str())
    .bind(triple.project.as_str())
    .bind(triple.env.as_str())
    .fetch_optional(&mut *connection)
    .await?;
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
    sqlx::query_scalar("SELECT drain_bound_seconds FROM registry.env WHERE org=$1 AND env=$2")
        .bind(org.as_str())
        .bind(env.as_str())
        .fetch_one(&mut *connection)
        .await
}

/// Read the active projects in an environment, ordered by project name.
pub async fn projects(
    connection: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    env: &wamn_registry::EnvName,
) -> Result<Vec<wamn_registry::ProjectId>, sqlx::Error> {
    let projects = sqlx::query_scalar::<_, String>(
        "SELECT project FROM registry.project_env WHERE org=$1 AND env=$2 AND active ORDER BY project",
    )
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_all(&mut *connection)
    .await?;
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
    sqlx::query_scalar("SELECT disposable FROM registry.project_env p JOIN registry.env USING (org,env) WHERE org=$1 AND project=$2 AND env=$3 AND p.active")
        .bind(triple.org.as_str()).bind(triple.project.as_str()).bind(triple.env.as_str()).fetch_one(&mut *connection).await
}

/// Read whether the project deployment is active.
pub async fn exists(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT FROM registry.project_env WHERE org=$1 AND project=$2 AND env=$3 AND active)",
    )
    .bind(triple.org.as_str())
    .bind(triple.project.as_str())
    .bind(triple.env.as_str())
    .fetch_one(&mut *connection)
    .await
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

/// Reserve a deployment in a provisioned env, retaining the suffix on an active retry.
/// Re-add starts a new generation with no applied schema or capture registration.
pub async fn record(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    registration: &Registration<'_>,
) -> Result<InstanceSuffix, sqlx::Error> {
    let r = registration;
    let (suffix, reactivated): (String, bool) = sqlx::query_as(upsert_project_env_sql())
        .bind(r.triple.org.as_str())
        .bind(r.triple.project.as_str())
        .bind(r.triple.env.as_str())
        .bind(r.secret_name)
        .bind(r.secret_namespace)
        .bind(r.instance_suffix.as_str())
        .bind(r.disposable)
        .bind(r.target_id)
        .fetch_one(&mut **transaction)
        .await?;
    if reactivated {
        // The gap rows reference the registration. History preserves their old images.
        sqlx::query("DELETE FROM registry.capture_gap WHERE org=$1 AND project=$2 AND env=$3")
            .bind(r.triple.org.as_str())
            .bind(r.triple.project.as_str())
            .bind(r.triple.env.as_str())
            .execute(&mut **transaction)
            .await?;
        sqlx::query("DELETE FROM registry.event_reader WHERE org=$1 AND project=$2 AND env=$3")
            .bind(r.triple.org.as_str())
            .bind(r.triple.project.as_str())
            .bind(r.triple.env.as_str())
            .execute(&mut **transaction)
            .await?;
    }
    wamn_postgres::decode_text(suffix, 0)
}

/// Mark a torn-down deployment inactive and remove its memberships and app bindings.
pub async fn deactivate(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    triple: &Triple,
) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(
        "UPDATE registry.project_env SET active=false WHERE org=$1 AND project=$2 AND env=$3 AND active",
    )
    .bind(triple.org.as_str())
    .bind(triple.project.as_str())
    .bind(triple.env.as_str())
    .execute(&mut **transaction)
    .await?;
    if updated.rows_affected() != 0 {
        // Take a fresh snapshot after the update has excluded concurrent grants.
        sqlx::query(
            "DELETE FROM identity.project_env_membership WHERE org=$1 AND project=$2 AND env=$3",
        )
        .bind(triple.org.as_str())
        .bind(triple.project.as_str())
        .bind(triple.env.as_str())
        .execute(&mut **transaction)
        .await?;
        // A retired app binding must not prevent env apply from removing its integration pin.
        sqlx::query(
            "DELETE FROM registry.env_app_connection WHERE org=$1 AND project=$2 AND env=$3",
        )
        .bind(triple.org.as_str())
        .bind(triple.project.as_str())
        .bind(triple.env.as_str())
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

/// Serialize changes to an organization's initial administrator.
pub async fn lock_org(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &wamn_registry::OrgId,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT id FROM registry.org WHERE id=$1 FOR UPDATE")
        .bind(org.as_str())
        .fetch_one(&mut **transaction)
        .await?;
    Ok(())
}

/// Upsert a project row into `registry.project` (idempotent). Params: `$1` org,
/// `$2` id. `ON CONFLICT (org, id) DO NOTHING` — a project carries no mutable
/// placement of its own (placement is per-env), so re-provisioning is a no-op.
fn upsert_project_sql() -> &'static str {
    "INSERT INTO registry.project (org, id) VALUES ($1, $2) \
     ON CONFLICT (org, id) DO NOTHING"
}

/// Update the provisioned environment, then record its project database. Parameters follow the caller's
/// org, project, environment, Secret, namespace, suffix, disposable, and target values.
fn upsert_project_env_sql() -> &'static str {
    "WITH parent AS (UPDATE registry.env SET disposable=COALESCE($7,disposable), \
       target_id=$8::text::uuid WHERE org=$1 AND env=$3 RETURNING org,env) \
     INSERT INTO registry.project_env AS deployment \
       (org,env,project,secret_name,secret_namespace,instance_suffix) \
       SELECT org,env,$2,$4,$5,$6 FROM parent \
       ON CONFLICT (org,env,project) DO UPDATE SET secret_name=EXCLUDED.secret_name, \
         secret_namespace=EXCLUDED.secret_namespace, active=true, \
         instance_suffix=CASE WHEN deployment.active THEN deployment.instance_suffix ELSE EXCLUDED.instance_suffix END, \
         schema_mode=CASE WHEN deployment.active THEN deployment.schema_mode END \
       RETURNING new.instance_suffix, old.active IS FALSE"
}
