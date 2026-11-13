//! Credential references backed by an in-memory store for runtime tests.

use super::ConnectionCredentials;
use sqlx::Connection as _;
use std::sync::Arc;
use wamn_kubernetes::{Store as _, test_util::MemoryStore};

pub(crate) struct Fixture {
    pub database: wamn_test_postgres::Database,
    pub admin: sqlx::PgConnection,
    pub store: Arc<MemoryStore>,
}

impl Fixture {
    pub async fn new() -> Self {
        let database = wamn_control_provision::test_database::system();
        let mut admin = wamn_postgres::connect(database.url()).await.unwrap();

        sqlx::raw_sql("SET ROLE wamn_system")
            .execute(&mut admin)
            .await
            .map(drop)
            .unwrap();
        sqlx::query("SELECT set_config('app.user_id', $1, false)")
            .bind(
                &wamn_control_provision::PlatformComponent::Provisioning
                    .principal_id()
                    .to_string(),
            )
            .execute(&mut admin)
            .await
            .map(|result| result.rows_affected())
            .unwrap();
        for project in ["project-a", "project-b", "host-project", "other-project"] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                (&wamn_control_provision::test_database::seed_org_project_env_sql(
                    "acme", project, "dev",
                ))
                    .to_string(),
            ))
            .execute(&mut admin)
            .await
            .map(drop)
            .unwrap();
        }
        sqlx::raw_sql("INSERT INTO registry.integration_type_auth VALUES ('http','custom'); INSERT INTO registry.target(name,type,status) VALUES ('fixture','local','active'); UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='fixture'); RESET ROLE").execute(&mut admin).await.map(drop).unwrap();
        let store = Arc::new(MemoryStore::default());
        Self {
            database,
            admin,
            store,
        }
    }

    pub async fn put(&mut self, name: &str, value: &str, projects: &[&str]) {
        self.put_type(name, value, projects, "http").await;
    }

    pub async fn put_store(&mut self, name: &str, value: &str, projects: &[&str]) {
        self.put_type(name, value, projects, "s3").await;
    }

    async fn put_type(
        &mut self,
        name: &str,
        value: &str,
        projects: &[&str],
        integration_type: &str,
    ) {
        let parameters = if integration_type == "s3" {
            serde_json::json!({"provider":"s3","endpoint":"http://fixture.invalid","container":"files","prefix":"test/"})
        } else {
            serde_json::json!({"primary-authority":"https://fixture.invalid/","tls-verification":"verify-authority"})
        };
        self.put_configuration(
            name,
            value,
            projects,
            (integration_type, "custom", parameters),
        )
        .await;
    }

    pub async fn put_http(
        &mut self,
        name: &str,
        value: &str,
        projects: &[&str],
        (auth_type, auth): (&str, serde_json::Value),
    ) {
        self.put_configuration(name, value, projects, ("http", auth_type,
            serde_json::json!({"primary-authority":"https://fixture.invalid/","tls-verification":"verify-authority","auth":auth}))).await;
    }

    async fn put_configuration(
        &mut self,
        name: &str,
        value: &str,
        projects: &[&str],
        (integration_type, auth_type, parameters): (&str, &str, serde_json::Value),
    ) {
        for project in projects {
            let mut tx = self.admin.begin().await.unwrap();
            sqlx::raw_sql("SET LOCAL ROLE wamn_system")
                .execute(&mut *tx)
                .await
                .map(drop)
                .unwrap();
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                (&wamn_control_provision::bind_platform_principal_sql(
                    wamn_control_provision::PlatformComponent::Provisioning,
                ))
                    .to_string(),
            ))
            .execute(&mut *tx)
            .await
            .map(drop)
            .unwrap();
            let stored = self
                .store
                .put(
                    &wamn_registry::Triple::new("acme", "dev", *project),
                    uuid::Uuid::new_v4(),
                    value.as_bytes(),
                )
                .await
                .unwrap();
            wamn_test_infrastructure::credentials::define(&mut tx,"acme",&serde_json::json!({"project":project,"env":"dev","name":name,"integration_type":integration_type,"auth_type":auth_type,"parameters":parameters,"credential":if auth_type == "custom" { "{}" } else { value }}),&wamn_test_infrastructure::credentials::ReferencedCredentials(&stored)).await.unwrap();
            tx.commit().await.unwrap();
        }
    }

    pub async fn reader(&self, project: &str) -> ConnectionCredentials {
        let mut admin = wamn_postgres::connect(self.database.url()).await.unwrap();
        let role = format!("c35_{}_{}", self.database.name(), project.replace('-', "_"));
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "BEGIN; {} {}",
                wamn_control_provision::bind_platform_principal_sql(
                    wamn_control_provision::PlatformComponent::Provisioning
                ),
                wamn_control_provision::test_database::login_sql(
                    wamn_control_provision::WorkloadRoleFamily::ConnectionReader,
                    self.database.name(),
                    &role,
                    "fixture-password"
                )
            ))
                .to_string(),
        ))
        .execute(&mut admin)
        .await
        .map(drop)
        .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(
            (wamn_control_provision::sql::upsert_control_author_project_env_key_mapping_sql())
                .to_string(),
        ))
        .bind(&role)
        .bind(&wamn_registry::project_env_key("acme", project, "dev"))
        .bind(&"acme")
        .bind(&project)
        .bind(&"dev")
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
        sqlx::raw_sql("COMMIT")
            .execute(&mut admin)
            .await
            .map(drop)
            .unwrap();
        let mut url = url::Url::parse(self.database.url()).unwrap();
        url.set_username(&role).unwrap();
        url.set_password(Some("fixture-password")).unwrap();
        ConnectionCredentials::from_database(
            wamn_postgres::pool(url.as_str(), 4).await.unwrap(),
            "acme",
        )
        .with_store(Arc::<MemoryStore>::clone(&self.store))
    }
}
