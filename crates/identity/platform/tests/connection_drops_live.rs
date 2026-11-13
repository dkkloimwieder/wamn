//! The list of every administration writer of a fact that the host keeps in
//! its connection memory (docs/architecture/execution.md#connection-memory).
//! Each writer runs once on real PostgreSQL, its caller deletes the drops it
//! recorded from a private bucket, and the test tests the keys that went.

use sqlx::{Connection as _, Row as _};
use std::collections::BTreeSet;
use std::sync::Mutex;
use wamn_broker::Publisher;

use serde_json::{Value, json};
use wamn_broker::auth_changes::AuthDrops;
use wamn_broker::connection_changes::{
    BUCKET, ConnectionDrops, connection_key, connections_key, release_key,
};
use wamn_kubernetes::{Store, StoreVersion};
use wamn_platform_identity::connections::administration;

#[derive(Debug, Default)]
struct RecordingStore(Mutex<Vec<Vec<u8>>>);

#[async_trait::async_trait]
impl Store for RecordingStore {
    async fn put(
        &self,
        _target: &wamn_registry::Triple,
        id: uuid::Uuid,
        value: &[u8],
    ) -> Result<StoreVersion, wamn_kubernetes::Error> {
        self.0.lock().unwrap().push(value.to_vec());
        Ok(StoreVersion {
            store: "k8s",
            reference: format!("private-{id}"),
            version: "1".into(),
        })
    }
    async fn get(&self, _: &str, _: &str) -> Result<Vec<u8>, wamn_kubernetes::Error> {
        Ok(Vec::new())
    }
    async fn destroy(&self, _: &str, _: &str) -> Result<(), wamn_kubernetes::Error> {
        Ok(())
    }
}

/// Run one writer in its own transaction and return its result and drops.
async fn execute(
    client: &mut sqlx::PgConnection,
    operation: &str,
    request: Value,
) -> (Value, ConnectionDrops) {
    let mut tx = client.begin().await.unwrap();
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
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
    let mut drops = ConnectionDrops::default();
    let result = administration::execute_with_store(
        &mut tx,
        "acme",
        operation,
        &request.to_string(),
        &mut AuthDrops::default(),
        &mut drops,
        Some(&RecordingStore::default()),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (result, drops)
}

const CREDENTIAL: &str = r#"{"username":"fixture","password":"private"}"#;

/// The broker must be private to this test run.
#[tokio::test]
#[ignore = "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY"]
async fn every_connection_writer_deletes_exactly_its_connection_keys() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY",
    );
    let _lock = wamn_test_postgres::lock();
    let broker = wamn_broker::nats::PlatformNats::from_env()
        .unwrap()
        .connect()
        .await
        .unwrap();
    wamn_broker::kv_memory::create_buckets(&broker)
        .await
        .unwrap();
    let bucket = wamn_broker::kv_memory::open_bucket(&broker, BUCKET)
        .await
        .unwrap();
    let publisher = Publisher::new(broker);

    let database = wamn_control_provision::test_database::system();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    sqlx::raw_sql("BEGIN; SET LOCAL ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    for env in ["dev", "prod"] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::seed_org_project_env_sql("acme", "app", env))
                .to_string(),
        ))
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    }
    sqlx::raw_sql("UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT").execute(&mut client).await.map(drop).unwrap();
    // A new integration has no connection, so it deletes no key.
    let (_, drops) = execute(
        &mut client,
        "integration.write",
        json!({"project":"app","name":"erp","integration_type":"http","auth_type":"http_basic"}),
    )
    .await;
    assert!(drops.is_empty(), "integration.write");
    let parameters =
        json!({"primary-authority":"https://example.invalid","tls-verification":"disabled"});
    let mut ids = Vec::new();
    for env in ["dev", "prod"] {
        let (created, _) = execute(
            &mut client,
            "connection.write",
            json!({"project":"app","env":env,"name":"erp","parameters":parameters,"credential":CREDENTIAL}),
        )
        .await;
        ids.push(
            created["id"]
                .as_str()
                .unwrap()
                .parse::<uuid::Uuid>()
                .unwrap(),
        );
    }
    let dev = connection_key("acme--dev--app", &ids[0]);
    let prod = connection_key("acme--prod--app", &ids[1]);
    let release = release_key("acme--dev--app", &format!("sha256:{}", "ab".repeat(32)));
    let other = connection_key("acme--dev--other", &ids[0]);
    let dev_map = connections_key("acme--dev--app");
    let prod_map = connections_key("acme--prod--app");
    let keys = [&dev, &prod, &release, &other, &dev_map, &prod_map];

    let cases: [(&str, Value, &[&String]); 9] = [
        (
            "connection.write",
            json!({"project":"app","env":"dev","name":"erp","parameters":parameters}),
            &[&dev, &dev_map],
        ),
        (
            "connection.copy",
            json!({"project":"app","env":"prod","source_env":"dev","name":"erp"}),
            &[&prod, &prod_map],
        ),
        (
            "connection.set_credential",
            json!({"project":"app","env":"dev","name":"erp","credential":CREDENTIAL}),
            &[&dev],
        ),
        (
            "connection.disable",
            json!({"project":"app","env":"dev","name":"erp"}),
            &[&dev],
        ),
        (
            "connection.enable",
            json!({"project":"app","env":"dev","name":"erp"}),
            &[&dev],
        ),
        (
            "integration.disable",
            json!({"project":"app","name":"erp"}),
            &[&dev, &prod],
        ),
        (
            "integration.enable",
            json!({"project":"app","name":"erp"}),
            &[&dev, &prod],
        ),
        (
            "connection.delete",
            json!({"project":"app","env":"prod","name":"erp"}),
            &[&prod, &prod_map],
        ),
        (
            "connection.delete",
            json!({"project":"app","env":"dev","name":"erp"}),
            &[&dev, &dev_map],
        ),
    ];
    for (operation, request, deleted) in cases {
        for key in keys {
            bucket.put(key.as_str(), "cached".into()).await.unwrap();
        }
        let (result, drops) = execute(&mut client, operation, request).await;
        assert!(result.get("connections").is_none(), "{operation}");
        publisher.publish(drops).await;
        let mut gone = BTreeSet::new();
        for key in keys {
            if bucket.get(key.as_str()).await.unwrap().is_none() {
                gone.insert(key.as_str());
            }
        }
        let expected: BTreeSet<&str> = deleted.iter().map(|key| key.as_str()).collect();
        assert_eq!(gone, expected, "{operation}");
    }

    // An integration without connections deletes no key.
    for key in keys {
        bucket.put(key.as_str(), "cached".into()).await.unwrap();
    }
    let (_, drops) = execute(
        &mut client,
        "integration.delete",
        json!({"project":"app","name":"erp"}),
    )
    .await;
    assert!(drops.is_empty(), "integration.delete");
}
