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
            let integration: String = tx.query_one("INSERT INTO registry.integration(org,project,name,integration_type,auth_type) VALUES ('acme',$1,$2,$3,'custom') ON CONFLICT (org,project,name) DO UPDATE SET status=registry.integration.status RETURNING id::text", &[project, &name, &integration_type]).await.unwrap().get(0);
            let existing = tx.query_opt("SELECT id::text,active_generation FROM registry.connection WHERE integration_id=$1::text::uuid AND env='dev'", &[&integration]).await.unwrap();
            let (connection, generation) = if let Some(row) = existing {
                (row.get::<_, String>(0), row.get::<_, i64>(1) + 1)
            } else {
                let connection_id = uuid::Uuid::new_v4().to_string();
                let principal: String = tx.query_one("INSERT INTO identity.principals(type,subject,display_name) VALUES ('service',$1,$1) RETURNING id::text", &[&format!("connection-{connection_id}")]).await.unwrap().get(0);
                let id: String = tx.query_one("INSERT INTO registry.connection(id,integration_id,org,project,env,integration_type,target_id,active_generation,service_principal_id) SELECT $4::text::uuid,$1::text::uuid,'acme',$2,'dev',$5,target_id,2,$3::text::uuid FROM registry.project_envs WHERE org='acme' AND project=$2 AND env='dev' RETURNING id::text", &[&integration, project, &principal, &connection_id, &integration_type]).await.unwrap().get(0);
                (id, 2)
            };
            let id = uuid::Uuid::new_v4();
            let stored = self.store.put(id, value.as_bytes()).await.unwrap();
            let id = id.to_string();
            tx.execute("INSERT INTO identity.credential(id,connection_id,store,ref,version,created_at,created_by) VALUES ($1::text::uuid,$2::text::uuid,$3,$4,$5,now(),'00000000-0000-0000-0000-000000000001')", &[&id,&connection,&stored.store,&stored.reference,&stored.version]).await.unwrap();
            tx.execute("INSERT INTO registry.connection_generation(connection_id,generation,credential_id,created_at,created_by) VALUES ($1::text::uuid,$2,$3::text::uuid,now(),'00000000-0000-0000-0000-000000000001')", &[&connection,&generation,&id]).await.unwrap();
            tx.execute(
                "UPDATE registry.connection SET active_generation=$2 WHERE id=$1::text::uuid",
                &[&connection, &generation],
            )
            .await
            .unwrap();
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
