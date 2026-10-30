//! Credential references backed by private files for runtime tests.

use super::ConnectionCredentials;
use std::sync::Arc;
use wamn_credential_store::{Store as _, file::FileStore};

pub(crate) struct Fixture {
    pub database: wamn_test_postgres::Database,
    pub admin: tokio_postgres::Client,
    pub store: Arc<FileStore>,
    directory: std::path::PathBuf,
}

impl Fixture {
    pub async fn new() -> Self {
        let database = wamn_control_provision::test_database::system();
        let (admin, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(driver);
        admin.batch_execute("SET ROLE wamn_system").await.unwrap();
        admin
            .execute(
                "SELECT set_config('app.user_id', $1, false)",
                &[&wamn_control_provision::PlatformComponent::Provisioning
                    .principal_id()
                    .to_string()],
            )
            .await
            .unwrap();
        for project in ["project-a", "project-b", "host-project", "other-project"] {
            admin
                .batch_execute(
                    &wamn_control_provision::test_database::seed_org_project_env_sql(
                        "acme", project, "dev",
                    ),
                )
                .await
                .unwrap();
        }
        admin.batch_execute("INSERT INTO registry.integration_type_auth VALUES ('http','custom'); INSERT INTO registry.target(name,type,status) VALUES ('fixture','local','active'); UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='fixture'); RESET ROLE").await.unwrap();
        let directory =
            std::env::temp_dir().join(format!("wamn-credentials-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let store = Arc::new(FileStore::new(directory.clone()));
        Self {
            database,
            admin,
            store,
            directory,
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
        for project in projects {
            let tx = self.admin.transaction().await.unwrap();
            tx.batch_execute("SET LOCAL ROLE wamn_system")
                .await
                .unwrap();
            tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
            .await
            .unwrap();
            let stored = self
                .store
                .put(uuid::Uuid::new_v4(), value.as_bytes())
                .await
                .unwrap();
            let parameters = if integration_type == "s3" {
                serde_json::json!({"provider":"s3","endpoint":"http://fixture.invalid","container":"files","prefix":"test/"})
            } else {
                serde_json::json!({"primary-authority":"https://fixture.invalid/","tls-verification":"verify-authority"})
            };
            wamn_test_infrastructure::credentials::define(&tx,"acme",&serde_json::json!({"project":project,"env":"dev","name":name,"integration_type":integration_type,"auth_type":"custom","parameters":parameters}),&wamn_test_infrastructure::credentials::ReferencedCredentials(&stored)).await.unwrap();
            tx.commit().await.unwrap();
        }
    }

    pub async fn reader(&self, project: &str) -> ConnectionCredentials {
        let role = format!("c35_{}_{}", self.database.name(), project.replace('-', "_"));
        self.admin
            .batch_execute(&format!(
                "BEGIN; {} {}",
                wamn_control_provision::bind_platform_principal_sql(
                    wamn_control_provision::PlatformComponent::Provisioning
                ),
                wamn_control_provision::sql::prepare_workload_generation_sql(
                    wamn_control_provision::WorkloadRoleFamily::ConnectionReader,
                    self.database.name(),
                    &role,
                    "fixture-password",
                    "2099-01-01T00:00:00Z"
                )
            ))
            .await
            .unwrap();
        self.admin
            .execute(
                wamn_control_provision::sql::upsert_control_author_env_key_mapping_sql(),
                &[
                    &role,
                    &format!("acme--{project}--dev"),
                    &"acme",
                    &project,
                    &"dev",
                ],
            )
            .await
            .unwrap();
        self.admin.batch_execute("COMMIT").await.unwrap();
        let mut url = url::Url::parse(self.database.url()).unwrap();
        url.set_username(&role).unwrap();
        url.set_password(Some("fixture-password")).unwrap();
        let (client, driver) = tokio_postgres::connect(url.as_str(), tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(driver);
        ConnectionCredentials::from_database(Arc::new(client), "acme")
            .with_store(self.store.clone())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
