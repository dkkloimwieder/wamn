//! Project and environment connection rows copied from the system database.

use anyhow::Context as _;
use sqlx::Row as _;

use crate::configuration::{Connection, ExternalSystem, Integration, PlatformConfiguration};
use wamn_broker::auth_changes::AuthDrops;

/// A failed projection with its project environment and original cause.
#[derive(Debug)]
pub struct ProjectionError {
    target: String,
    source: anyhow::Error,
}

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "connection projection {}: {}",
            self.target, self.source
        )
    }
}

impl std::error::Error for ProjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn failure(org: &str, project: &str, env: &str, source: anyhow::Error) -> ProjectionError {
    ProjectionError {
        target: wamn_registry::name::host_status(org, project, env),
        source,
    }
}

/// Read the project's external systems and integrations and the environment's connections.
pub async fn projection(
    system: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &str,
    project: &str,
    env: &str,
) -> Result<PlatformConfiguration, ProjectionError> {
    async { rows_from_document(&document(system, org, project, env).await?) }
        .await
        .map_err(|source| failure(org, project, env, source))
}

/// Reconcile copied rows and service principals in an administration transaction.
pub async fn project(
    system: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &str,
    project: &str,
    env: &str,
) -> Result<AuthDrops, ProjectionError> {
    push(system, target, org, project, env)
        .await
        .map_err(|source| failure(org, project, env, source))
}

async fn document(
    system: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &str,
    project: &str,
    env: &str,
) -> anyhow::Result<serde_json::Value> {
    Ok(
        sqlx::query(wamn_registry::sql::IDENTITY_PLATFORM_SRC_CONNECTION_PROJECTION_DOCUMENT)
            .bind(org)
            .bind(project)
            .bind(env)
            .fetch_one(&mut **system)
            .await
            .context("read connection projection")?
            .get(0),
    )
}

fn rows_from_document(document: &serde_json::Value) -> anyhow::Result<PlatformConfiguration> {
    let mut result = PlatformConfiguration::default();
    for row in document["external_system"]
        .as_array()
        .context("external system projection")?
    {
        result.external_systems.push(ExternalSystem {
            id: text(row, "id")?,
            name: text(row, "name")?,
        });
    }
    for row in document["integration"]
        .as_array()
        .context("integration projection")?
    {
        result.integrations.push(Integration {
            id: text(row, "id")?,
            integration_type: text(row, "integration_type")?,
            external_system_id: row["external_system_id"].as_str().map(str::to_owned),
            name: text(row, "name")?,
            status: text(row, "status")?,
        });
    }
    for row in document["connection"]
        .as_array()
        .context("connection projection")?
    {
        result.connections.push(Connection {
            id: text(row, "id")?,
            integration_id: text(row, "integration_id")?,
            integration_type: text(row, "integration_type")?,
            status: text(row, "status")?,
        });
    }
    Ok(result)
}

fn text(row: &serde_json::Value, field: &str) -> anyhow::Result<String> {
    row[field]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("connection projection field {field}"))
}

async fn push(
    system: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &str,
    project: &str,
    env: &str,
) -> anyhow::Result<AuthDrops> {
    let document = document(system, org, project, env).await?;
    let rows = rows_from_document(&document)?;
    let enabled: Vec<_> = rows
        .connections
        .iter()
        .filter(|row| row.status == "active")
        .map(|row| row.id.as_str())
        .collect();
    let disabled: Vec<String> = sqlx::query("SELECT id::text FROM platform.connection WHERE status='active' AND NOT (id::text=ANY($1::text[]))").bind(&enabled).fetch_all(&mut **target).await?.into_iter().map(|row| row.get(0)).collect();
    crate::configuration::write_configuration(target, &rows).await?;
    let ids: Vec<_> = rows.connections.iter().map(|row| row.id.as_str()).collect();
    sqlx::query("DELETE FROM platform.connection WHERE NOT (id::text=ANY($1::text[]))")
        .bind(&ids)
        .execute(&mut **target)
        .await
        .map(|result| result.rows_affected())?;
    let ids: Vec<_> = rows
        .integrations
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    sqlx::query("DELETE FROM platform.integration WHERE NOT (id::text=ANY($1::text[]))")
        .bind(&ids)
        .execute(&mut **target)
        .await
        .map(|result| result.rows_affected())?;
    let ids: Vec<_> = rows
        .external_systems
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    sqlx::query("DELETE FROM platform.external_system WHERE NOT (id::text=ANY($1::text[]))")
        .bind(&ids)
        .execute(&mut **target)
        .await
        .map(|result| result.rows_affected())?;
    let mut drops = AuthDrops::default();
    let project_env_key = wamn_registry::project_env_key(org, project, env);
    for row in document["principal"]
        .as_array()
        .context("connection principal projection")?
    {
        let id = text(row, "id")?;
        let connection_id = text(row, "connection_id")?;
        let email = format!(
            "{}@{}",
            text(row, "subject")?,
            text(row, "platform_domain")?
        );
        let changed = sqlx::query("INSERT INTO platform.\"user\"(id,type,email,display_name,status) VALUES ($1::text::uuid,'service',$2,$3,$4) \
             ON CONFLICT (id) DO UPDATE SET email=EXCLUDED.email, display_name=EXCLUDED.display_name, status=EXCLUDED.status \
             WHERE (\"user\".email,\"user\".display_name,\"user\".status) IS DISTINCT FROM (EXCLUDED.email,EXCLUDED.display_name,EXCLUDED.status)").bind(&id).bind(&email).bind(&text(row, "display_name")?).bind(&text(row, "status")?).execute(&mut **target).await.map(|result| result.rows_affected())?;
        if changed != 0 || disabled.contains(&connection_id) {
            drops.principal(&project_env_key, &id);
        }
    }
    Ok(drops)
}

#[cfg(test)]
mod tests {
    use sqlx::Connection as _;
    #[sqlx::test(migrations = false)]
    async fn copies_project_rows_and_only_environment_connections_with_effective_status(
        pool: sqlx::PgPool,
    ) {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_test_postgres::url(&pool);
        wamn_control_provision::test_database::system(database.as_str()).await;
        let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();

        let mut source = client.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
                .to_string(),
        ))
        .execute(&mut *source)
        .await
        .map(drop)
        .unwrap();
        sqlx::raw_sql("SET LOCAL ROLE wamn_system")
            .execute(&mut *source)
            .await
            .map(drop)
            .unwrap();
        for (project, env) in [
            ("project-a", "dev"),
            ("project-a", "prod"),
            ("project-b", "dev"),
        ] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                (&wamn_control_provision::test_database::seed_org_project_env_sql(
                    "acme", project, env,
                ))
                    .to_string(),
            ))
            .execute(&mut *source)
            .await
            .map(drop)
            .unwrap();
        }
        sqlx::raw_sql(r"
            UPDATE registry.meta SET platform_domain='example.invalid';
            UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud');
            INSERT INTO registry.external_system(org,project,name) VALUES ('acme','project-a','source'),('acme','project-b','source');
            INSERT INTO registry.integration(org,project,name,integration_type,auth_type,status)
            VALUES ('acme','project-a','disabled','http','none','disabled'),
                   ('acme','project-a','active','http','none','active'),
                   ('acme','project-b','active','http','none','active');
            INSERT INTO identity.principals(id,type,subject,display_name)
            SELECT gen_random_uuid(),'service','projection-' || row_number() OVER()::text,'projection' FROM registry.environment_projects WHERE org='acme';
            INSERT INTO registry.connection(integration_id,org,project,env,integration_type,target_id,active_generation,service_principal_id)
            SELECT i.id,i.org,i.project,e.env,i.integration_type,v.target_id,1,p.id
            FROM registry.integration i JOIN registry.environment_projects e ON (e.org,e.project)=(i.org,i.project) JOIN registry.environments v ON (v.org,v.env)=(e.org,e.env)
            JOIN LATERAL (SELECT id FROM identity.principals WHERE subject='projection-' ||
                CASE WHEN i.project='project-b' THEN '3' WHEN e.env='prod' THEN '2' ELSE '1' END) p ON true
            WHERE i.name=CASE WHEN i.project='project-a' THEN 'disabled' ELSE 'active' END;
            INSERT INTO registry.connection_generation(connection_id,generation,created_at,created_by)
            SELECT id,1,now(),'00000000-0000-4000-8000-000000000001' FROM registry.connection;
        ").execute(&mut *source).await.map(drop).unwrap();
        sqlx::raw_sql("SET LOCAL ROLE wamn_control")
            .execute(&mut *source)
            .await
            .map(drop)
            .unwrap();
        let rows = super::projection(&mut source, "acme", "project-a", "dev")
            .await
            .unwrap();
        assert_eq!(rows.external_systems.len(), 1);
        assert_eq!(rows.integrations.len(), 2);
        assert_eq!(rows.connections.len(), 1);
        assert_eq!(rows.connections[0].status, "disabled");
        let other = super::projection(&mut source, "acme", "project-b", "dev")
            .await
            .unwrap();
        assert_eq!(other.connections.len(), 1);
        assert_eq!(other.connections[0].status, "active");
        assert_ne!(rows.connections[0].id, other.connections[0].id);
        sqlx::raw_sql("SET LOCAL ROLE wamn_system; UPDATE registry.connection SET status='disabled' WHERE org='acme' AND project='project-b'; SET LOCAL ROLE wamn_control").execute(&mut *source).await.map(drop).unwrap();
        let disabled = super::projection(&mut source, "acme", "project-b", "dev")
            .await
            .unwrap();
        assert_eq!(disabled.connections[0].status, "disabled");
        source.rollback().await.unwrap();
    }
}
