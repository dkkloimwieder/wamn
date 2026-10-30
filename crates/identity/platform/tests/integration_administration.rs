//! Route writes keep private values in the store and generations in their target environment.

use std::sync::Mutex;

use serde_json::{Value, json};
use wamn_credential_store::{Store, StoreVersion};
use wamn_platform_identity::{auth_changes::AuthDrops, connections::administration};

#[derive(Debug, Default)]
struct RecordingStore(Mutex<Vec<Vec<u8>>>);

#[async_trait::async_trait]
impl Store for RecordingStore {
    async fn put(
        &self,
        id: uuid::Uuid,
        value: &[u8],
    ) -> Result<StoreVersion, wamn_credential_store::Error> {
        self.0.lock().unwrap().push(value.to_vec());
        Ok(StoreVersion {
            store: "file",
            reference: format!("private-{id}"),
            version: "1".into(),
        })
    }
    async fn get(&self, _: &str, _: &str) -> Result<Vec<u8>, wamn_credential_store::Error> {
        Ok(Vec::new())
    }
    async fn destroy(&self, _: &str, _: &str) -> Result<(), wamn_credential_store::Error> {
        Ok(())
    }
}

async fn execute(
    client: &mut tokio_postgres::Client,
    store: &RecordingStore,
    operation: &str,
    request: Value,
) -> Value {
    let tx = client.transaction().await.unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    let result = administration::execute_with_store(
        &tx,
        "acme",
        operation,
        &request.to_string(),
        &mut AuthDrops::default(),
        &mut wamn_platform_identity::connection_changes::ConnectionDrops::default(),
        Some(store),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    result
}

#[tokio::test]
async fn write_and_copy_keep_environment_generations_and_credentials_separate() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let (mut client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(driver);
    client
        .batch_execute("BEGIN; SET LOCAL ROLE wamn_system")
        .await
        .unwrap();
    client
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
        .unwrap();
    for env in ["dev", "prod"] {
        client
            .batch_execute(
                &wamn_control_provision::test_database::seed_org_project_env_sql(
                    "acme", "app", env,
                ),
            )
            .await
            .unwrap();
    }
    client.batch_execute("UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT").await.unwrap();
    let store = RecordingStore::default();
    execute(
        &mut client,
        &store,
        "integration.write",
        json!({"project":"app","name":"erp","integration_type":"http","auth_type":"http_basic"}),
    )
    .await;
    let mut created = Vec::new();
    for env in ["dev", "prod"] {
        let credential = format!(r#"{{"username":"{env}","password":"private-{env}"}}"#);
        let response = execute(&mut client,&store,"connection.write",json!({"project":"app","env":env,"name":"erp",
            "parameters":{"primary-authority":"https://example.invalid","tls-verification":"disabled"},"credential":credential})).await;
        assert!(!response.to_string().contains("private-"));
        created.push(response);
    }
    execute(&mut client,&store,"connection.write",json!({"project":"app","env":"dev","name":"erp",
        "parameters":{"primary-authority":"https://example.invalid","tls-verification":"verify-authority"}})).await;
    let prod = execute(
        &mut client,
        &store,
        "connection.get",
        json!({"project":"app","env":"prod","name":"erp"}),
    )
    .await;
    assert_eq!(prod["active_generation"], 1);
    assert_eq!(prod["parameters"]["tls-verification"], "disabled");
    let copied = execute(
        &mut client,
        &store,
        "connection.copy",
        json!({"project":"app","env":"prod","source_env":"dev","name":"erp"}),
    )
    .await;
    assert_eq!(copied["active_generation"], 2);
    assert_eq!(copied["parameters"]["tls-verification"], "verify-authority");
    assert_eq!(
        store.0.lock().unwrap().len(),
        2,
        "copy must never write a credential"
    );
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    let rows = client.query("SELECT c.env,c.active_generation,g.credential_id::text FROM registry.connection c \
        JOIN registry.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) ORDER BY c.env", &[]).await.unwrap();
    assert_eq!(rows[0].get::<_, i64>(1), 2);
    assert_eq!(rows[1].get::<_, i64>(1), 2);
    let source: String = rows[0].get(2);
    let target: String = rows[1].get(2);
    assert_ne!(source, target);
    let credential_ids: Vec<String> = client
        .query(
            "SELECT credential_id::text FROM registry.connection_generation \
        WHERE connection_id=$1::text::uuid ORDER BY generation",
            &[&created[1]["id"].as_str().unwrap()],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(credential_ids, vec![target.clone(), target]);
    let metadata: String = client
        .query_one(
            "SELECT jsonb_agg(to_jsonb(k))::text FROM identity.credential k",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(!metadata.contains("private-dev"));
    assert!(!metadata.contains("private-prod"));
}
