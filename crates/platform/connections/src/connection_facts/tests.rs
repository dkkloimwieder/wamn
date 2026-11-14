use sqlx::Connection as _;
use std::time::Duration;

use serde_json::json;
use wamn_broker::auth_changes::AuthDrops;
use wamn_platform_identity::connections::administration;

use super::*;
use crate::connection_credentials::test_util::Fixture;
use crate::connection_memory::replicate;
use wamn_capability_authority::auth_cache::test_util::{nats, until};
struct NoProjectDatabase;
#[async_trait::async_trait]
impl crate::database::ProjectDatabase for NoProjectDatabase {
    async fn read_connection(
        &self,
        _: &str,
        _: &str,
    ) -> anyhow::Result<crate::database::ConnectionGuard> {
        anyhow::bail!("project database unavailable")
    }
}

const PROJECT_ENV_KEY: &str = "acme--dev--project-a";

async fn write(fixture: &mut Fixture, password: &str) -> ConnectionDrops {
    let mut tx = fixture.admin.begin().await.unwrap();
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
    administration::execute_with_store(
        &mut tx,
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
    changes: Publisher,
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
    let id = sqlx::query("SELECT id FROM registry.connection WHERE project='project-a'")
        .fetch_one(&mut fixture.admin)
        .await
        .unwrap()
        .get(0);
    let facts = ConnectionFacts::new(
        memory,
        changes,
        Arc::new(fixture.reader("project-a").await),
        &Arc::new(NoProjectDatabase),
        PROJECT_ENV_KEY,
        "project-a",
        "dev",
    );
    (fixture, facts, id)
}

async fn assert_postgres_read(fixture: &mut Fixture, facts: &ConnectionFacts, id: &uuid::Uuid) {
    sqlx::raw_sql("REVOKE EXECUTE ON FUNCTION registry.read_connection_fact(text,text,text,uuid) FROM wamn_conn_reader").execute(&mut fixture.admin).await.map(drop).unwrap();
    let error = facts.connection(id).await.unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<sqlx::Error>()
            .unwrap()
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("42501"),
    );
    sqlx::raw_sql("GRANT EXECUTE ON FUNCTION registry.read_connection_fact(text,text,text,uuid) TO wamn_conn_reader").execute(&mut fixture.admin).await.map(drop).unwrap();
}

#[tokio::test]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN"]
async fn a_committed_write_serves_the_new_generation_after_the_delete() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN",
    );
    let _lock = wamn_test_postgres::lock();
    let (_server, mut client) = nats().await;
    let memory = Arc::new(ConnectionMemory::default());
    let changes = replicate(&mut client, Some(PROJECT_ENV_KEY), Arc::clone(&memory));
    until("the connection watch runs", || memory.watched()).await;
    let (mut fixture, facts, id) = setup(Arc::clone(&memory), changes.clone()).await;
    let old = facts.connection(&id).await.unwrap().unwrap();
    assert_eq!(memory.connection(PROJECT_ENV_KEY, &id), Ok(old.clone()));
    let old_secret = facts.secret(&old.credential_id.unwrap()).await.unwrap();
    let drops = write(&mut fixture, "new").await;
    changes.publish(drops).await;
    until("the host sees the committed delete", || {
        memory.connection(PROJECT_ENV_KEY, &id).is_err()
    })
    .await;
    let new = facts.connection(&id).await.unwrap().unwrap();
    assert_eq!(new.active_generation, old.active_generation + 1);
    assert_ne!(new.credential_id, old.credential_id);
    assert_eq!(memory.connection(PROJECT_ENV_KEY, &id), Ok(new.clone()));
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
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN"]
async fn a_suppressed_delete_serves_the_new_generation_when_the_ttl_expires() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN",
    );
    let _lock = wamn_test_postgres::lock();
    let (_server, mut client) = nats().await;
    let ttl = Duration::from_secs(1);
    let memory = Arc::new(ConnectionMemory::with_ttl(ttl));
    let changes = replicate(&mut client, Some(PROJECT_ENV_KEY), Arc::clone(&memory));
    until("the connection watch runs", || memory.watched()).await;
    let (mut fixture, facts, id) = setup(Arc::clone(&memory), changes).await;
    let old = facts.connection(&id).await.unwrap().unwrap();
    let drops = write(&mut fixture, "new").await;
    // The commit stands, but its caller deliberately omits the bucket delete.
    drop(drops);
    assert_eq!(facts.connection(&id).await.unwrap(), Some(old.clone()));
    until("the local fact expires", || {
        memory.connection(PROJECT_ENV_KEY, &id).is_err()
    })
    .await;
    let new = facts.connection(&id).await.unwrap().unwrap();
    assert_eq!(new.active_generation, old.active_generation + 1);
    assert_ne!(new.credential_id, old.credential_id);
    assert!(memory.watched());
}

#[tokio::test]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN"]
async fn a_stopped_broker_reads_each_generation_without_keeping_a_fact() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATIVE_C_NATS_BIN",
    );
    let _lock = wamn_test_postgres::lock();
    let (server, mut client) = nats().await;
    let memory = Arc::new(ConnectionMemory::default());
    let changes = replicate(&mut client, Some(PROJECT_ENV_KEY), Arc::clone(&memory));
    until("the connection watch runs", || memory.watched()).await;
    let (mut fixture, facts, id) = setup(Arc::clone(&memory), changes).await;
    let old = facts.connection(&id).await.unwrap().unwrap();
    assert_eq!(memory.connection(PROJECT_ENV_KEY, &id), Ok(old.clone()));
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
        assert!(memory.connection(PROJECT_ENV_KEY, &id).is_err());
        assert_postgres_read(&mut fixture, &facts, &id).await;
    }
    let drops = write(&mut fixture, "new").await;
    drop(drops);
    for _ in 0..2 {
        let new = facts.connection(&id).await.unwrap().unwrap();
        assert_eq!(new.active_generation, old.active_generation + 1);
        assert_ne!(new.credential_id, old.credential_id);
        assert!(memory.connection(PROJECT_ENV_KEY, &id).is_err());
        assert_postgres_read(&mut fixture, &facts, &id).await;
    }
}
