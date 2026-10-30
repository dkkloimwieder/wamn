use std::time::Duration;

use serde_json::json;
use wamn_platform_identity::auth_changes::AuthDrops;
use wamn_platform_identity::connections::administration;

use super::*;
use crate::auth_cache::test_util::{nats, until};
use crate::connection_credentials::test_util::Fixture;
use crate::connection_memory::replicate;
use crate::plugins::wamn_postgres::WamnPostgresConfig;

const ENV_KEY: &str = "acme--project-a--dev";

async fn write(fixture: &mut Fixture, password: &str) -> ConnectionDrops {
    let tx = fixture.admin.transaction().await.unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    let mut drops = ConnectionDrops::default();
    administration::execute_with_store(
        &tx,
        "acme",
        "connection.write",
        &json!({
            "project":"project-a", "env":"dev", "name":"erp",
            "parameters":{"primary-authority":"https://fixture.invalid/","tls-verification":"verify-authority","auth":{}},
            "credential":json!({"username":"fixture","password":password}).to_string()
        }).to_string(),
        &mut AuthDrops::default(),
        &mut drops,
        Some(fixture.store.as_ref()),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(!drops.is_empty());
    drops
}

async fn setup(
    memory: Arc<ConnectionMemory>,
    changes: ConnectionChanges,
) -> (Fixture, ConnectionFacts, uuid::Uuid) {
    let mut fixture = Fixture::new().await;
    fixture
        .put_http(
            "erp",
            r#"{"username":"fixture","password":"old"}"#,
            &["project-a"],
            ("http_basic", json!({})),
        )
        .await;
    let id = fixture
        .admin
        .query_one(
            "SELECT id FROM registry.connection WHERE project='project-a'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let facts = ConnectionFacts::new(
        memory,
        changes,
        Arc::new(fixture.reader("project-a").await),
        Arc::new(
            WamnPostgres::new(WamnPostgresConfig {
                credentials: None,
                guest_pool_max_size: 1,
                platform_pool_max_size: 1,
                wait_timeout_ms: 5_000,
                statement_timeout_ms: 10_000,
                row_limit: 10_000,
            })
            .unwrap(),
        ),
        ENV_KEY,
        "project-a",
        "dev",
    );
    (fixture, facts, id)
}

async fn assert_postgres_read(fixture: &Fixture, facts: &ConnectionFacts, id: &uuid::Uuid) {
    fixture.admin.batch_execute(
        "REVOKE EXECUTE ON FUNCTION registry.read_connection_fact(text,text,text,uuid) FROM wamn_conn_reader",
    ).await.unwrap();
    let error = facts.connection(id).await.unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<tokio_postgres::Error>()
            .unwrap()
            .code(),
        Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE),
    );
    fixture.admin.batch_execute(
        "GRANT EXECUTE ON FUNCTION registry.read_connection_fact(text,text,text,uuid) TO wamn_conn_reader",
    ).await.unwrap();
}

#[tokio::test]
#[ignore = "requires: PostgreSQL 18, WAMN_NATIVE_C_NATS_BIN"]
async fn a_committed_write_serves_the_new_generation_after_the_delete() {
    wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_BIN"]);
    let _lock = wamn_test_postgres::lock();
    let (_server, client) = nats().await;
    let memory = Arc::new(ConnectionMemory::default());
    let changes = replicate(&client, Some(ENV_KEY), Arc::clone(&memory));
    until("the connection watch runs", || memory.watched()).await;
    let (mut fixture, facts, id) = setup(Arc::clone(&memory), changes.clone()).await;
    let old = facts.connection(&id).await.unwrap().unwrap();
    assert_eq!(memory.connection(ENV_KEY, &id), Ok(old.clone()));
    let old_secret = facts.secret(&old.credential_id.unwrap()).await.unwrap();
    let drops = write(&mut fixture, "new").await;
    changes.publish(drops).await;
    until("the host sees the committed delete", || {
        memory.connection(ENV_KEY, &id).is_err()
    })
    .await;
    let new = facts.connection(&id).await.unwrap().unwrap();
    assert_eq!(new.active_generation, old.active_generation + 1);
    assert_ne!(new.credential_id, old.credential_id);
    assert_eq!(memory.connection(ENV_KEY, &id), Ok(new.clone()));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            facts
                .secret(&new.credential_id.unwrap())
                .await
                .unwrap()
                .expose()
        )
        .unwrap()["password"],
        "new"
    );
    assert!(facts.secrets.get(&old.credential_id.unwrap()).is_none());
    assert!(old_secret.expose().contains("old"));
}

#[tokio::test]
#[ignore = "requires: PostgreSQL 18, WAMN_NATIVE_C_NATS_BIN"]
async fn a_suppressed_delete_serves_the_new_generation_when_the_ttl_expires() {
    wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_BIN"]);
    let _lock = wamn_test_postgres::lock();
    let (_server, client) = nats().await;
    let ttl = Duration::from_secs(1);
    let memory = Arc::new(ConnectionMemory::with_ttl(ttl));
    let changes = replicate(&client, Some(ENV_KEY), Arc::clone(&memory));
    until("the connection watch runs", || memory.watched()).await;
    let (mut fixture, facts, id) = setup(Arc::clone(&memory), changes).await;
    let old = facts.connection(&id).await.unwrap().unwrap();
    let drops = write(&mut fixture, "new").await;
    // The commit stands, but its caller deliberately omits the bucket delete.
    drop(drops);
    assert_eq!(facts.connection(&id).await.unwrap(), Some(old.clone()));
    until("the local fact expires", || {
        memory.connection(ENV_KEY, &id).is_err()
    })
    .await;
    let new = facts.connection(&id).await.unwrap().unwrap();
    assert_eq!(new.active_generation, old.active_generation + 1);
    assert_ne!(new.credential_id, old.credential_id);
    assert!(memory.watched());
}

#[tokio::test]
#[ignore = "requires: PostgreSQL 18, WAMN_NATIVE_C_NATS_BIN"]
async fn a_stopped_broker_reads_each_generation_without_keeping_a_fact() {
    wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_BIN"]);
    let _lock = wamn_test_postgres::lock();
    let (server, client) = nats().await;
    let memory = Arc::new(ConnectionMemory::default());
    let changes = replicate(&client, Some(ENV_KEY), Arc::clone(&memory));
    until("the connection watch runs", || memory.watched()).await;
    let (mut fixture, facts, id) = setup(Arc::clone(&memory), changes).await;
    let old = facts.connection(&id).await.unwrap().unwrap();
    assert_eq!(memory.connection(ENV_KEY, &id), Ok(old.clone()));
    drop(server);
    tokio::time::timeout(Duration::from_secs(30), async {
        while memory.watched() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the broken watch drops its facts");
    for _ in 0..2 {
        assert_eq!(facts.connection(&id).await.unwrap(), Some(old.clone()));
        assert!(memory.connection(ENV_KEY, &id).is_err());
        assert_postgres_read(&fixture, &facts, &id).await;
    }
    let drops = write(&mut fixture, "new").await;
    drop(drops);
    for _ in 0..2 {
        let new = facts.connection(&id).await.unwrap().unwrap();
        assert_eq!(new.active_generation, old.active_generation + 1);
        assert_ne!(new.credential_id, old.credential_id);
        assert!(memory.connection(ENV_KEY, &id).is_err());
        assert_postgres_read(&fixture, &facts, &id).await;
    }
}
