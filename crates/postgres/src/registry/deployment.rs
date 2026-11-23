use wamn_registry::{
    Triple,
    deployment::{DeploymentStore, OrganizationLock, Registration},
    instance_suffix::InstanceSuffix,
};

impl DeploymentStore for super::Registry<'_> {
    type Error = sqlx::Error;

    async fn instance_suffix(
        &mut self,
        triple: &Triple,
    ) -> Result<Option<InstanceSuffix>, sqlx::Error> {
        let suffix = sqlx::query_scalar::<_, String>("SELECT instance_suffix FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3")
        .bind(triple.org.as_str()).bind(triple.project.as_str()).bind(triple.env.as_str()).fetch_optional(&mut *self.connection).await?;
        suffix.map(|value| super::decode_text(value, 0)).transpose()
    }

    async fn drain_bound_seconds(
        &mut self,
        org: &wamn_registry::OrgId,
        env: &wamn_registry::EnvName,
    ) -> Result<i32, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT drain_bound_seconds FROM registry.environments WHERE org=$1 AND env=$2",
        )
        .bind(org.as_str())
        .bind(env.as_str())
        .fetch_one(&mut *self.connection)
        .await
    }

    async fn projects(
        &mut self,
        org: &wamn_registry::OrgId,
        env: &wamn_registry::EnvName,
    ) -> Result<Vec<wamn_registry::ProjectId>, sqlx::Error> {
        let projects = sqlx::query_scalar::<_, String>("SELECT project FROM registry.environment_projects WHERE org=$1 AND env=$2 ORDER BY project")
        .bind(org.as_str()).bind(env.as_str()).fetch_all(&mut *self.connection).await?;
        projects
            .into_iter()
            .map(|value| super::decode_text(value, 0))
            .collect()
    }

    async fn disposable(&mut self, triple: &Triple) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar("SELECT disposable FROM registry.environment_projects JOIN registry.environments USING (org,env) WHERE org=$1 AND project=$2 AND env=$3")
        .bind(triple.org.as_str()).bind(triple.project.as_str()).bind(triple.env.as_str()).fetch_one(&mut *self.connection).await
    }

    async fn exists(&mut self, triple: &Triple) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar("SELECT EXISTS(SELECT FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3)")
        .bind(triple.org.as_str()).bind(triple.project.as_str()).bind(triple.env.as_str()).fetch_one(&mut *self.connection).await
    }

    async fn record_project(
        &mut self,
        org: &wamn_registry::OrgId,
        project: &wamn_registry::ProjectId,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(upsert_project_sql())
            .bind(org.as_str())
            .bind(project.as_str())
            .execute(&mut *self.connection)
            .await?;
        Ok(())
    }

    async fn target_id(&mut self, name: &str) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT id::text FROM registry.target WHERE name = $1")
            .bind(name)
            .fetch_optional(&mut *self.connection)
            .await
    }

    async fn record(
        &mut self,
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
            .bind(r.readiness_budget_seconds)
            .bind(r.drain_bound_seconds)
            .bind(r.target_id)
            .fetch_one(&mut *self.connection)
            .await?;
        super::decode_text(suffix, 0)
    }
}

impl OrganizationLock for super::RegistryTransaction<'_, '_> {
    type Error = sqlx::Error;

    async fn lock_org(&mut self, org: &wamn_registry::OrgId) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT id FROM registry.orgs WHERE id=$1 FOR UPDATE")
            .bind(org.as_str())
            .fetch_one(&mut **self.transaction)
            .await?;
        Ok(())
    }
}

/// Upsert a project row into `registry.projects` (idempotent). Params: `$1` org,
/// `$2` id. `ON CONFLICT (org, id) DO NOTHING` — a project carries no mutable
/// placement of its own (placement is per-env), so re-provisioning is a no-op.
fn upsert_project_sql() -> &'static str {
    "INSERT INTO registry.projects (org, id) VALUES ($1, $2) \
     ON CONFLICT (org, id) DO NOTHING"
}

/// Record the environment, then its project database. Parameters follow the caller's
/// org, project, environment, Secret, namespace, suffix, disposable, readiness budget,
/// drain bound, and target values.
fn upsert_project_env_sql() -> &'static str {
    "WITH parent AS (INSERT INTO registry.environments \
       (org,env,disposable,readiness_budget_seconds,drain_bound_seconds,target_id) \
       VALUES ($1,$3,COALESCE($7,false),$8,$9,$10::text::uuid) \
       ON CONFLICT (org,env) DO UPDATE SET disposable=COALESCE($7,registry.environments.disposable), \
         target_id=EXCLUDED.target_id RETURNING org,env,disposable), \
     deployment AS (INSERT INTO registry.environment_projects \
       (org,env,project,secret_name,secret_namespace,instance_suffix) \
       SELECT org,env,$2,$4,$5,$6 FROM parent \
       ON CONFLICT (org,env,project) DO UPDATE SET secret_name=EXCLUDED.secret_name, \
         secret_namespace=EXCLUDED.secret_namespace RETURNING instance_suffix) \
     SELECT deployment.instance_suffix,parent.disposable FROM deployment CROSS JOIN parent"
}
