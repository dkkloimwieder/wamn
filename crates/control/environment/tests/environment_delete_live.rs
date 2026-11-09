//! Cancellation and retry of the product's final environment delete transaction.

use std::time::Duration;

use serde_json::json;
use tokio_postgres::Client;
use wamn_control_registry::Triple;
use wamn_environment_control::{
    environment::{
        delete::{CONTROL_TABLES, delete_registry_rows},
        lock::LifecycleLock,
    },
    provision_project_env::provisioning_transaction,
};
use wamn_platform_identity::{
    auth_changes::AuthDrops, connection_changes::ConnectionDrops, connections::administration,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
    tokio::spawn(connection);
    Ok(client)
}

#[tokio::test]
async fn cancellation_retains_registry_inputs_until_cleanup_commits() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut system = connect(database.url()).await?;
    system.batch_execute("SET ROLE wamn_system").await?;
    let triple = Triple::new("acme", "app", "dev");
    let other = Triple::new("acme", "app", "prod");
    let transaction = provisioning_transaction(&mut system).await?;
    for scope in [&triple, &other] {
        transaction
            .batch_execute(
                &wamn_control_provision::test_database::seed_org_project_env_sql(
                    &scope.org,
                    &scope.project,
                    &scope.env,
                ),
            )
            .await?;
        let env_key =
            wamn_control_provision::project_env_key(&scope.org, &scope.project, &scope.env);
        transaction
            .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
            .await?;
        transaction.execute(
            "INSERT INTO catalog.authoring_command_audit \
             (env_key,command_id,command_type,principal_id,principal_type,principal_subject,effective_role,org,project,environment,target_ref,request_hash,outcome_bytes) \
             VALUES ($1,'delete-fixture','gate','fixture','service','fixture','project-author',$2,$3,$4,'fixture','sha256:'||repeat('a',64),'{}'::bytea)",
            &[&env_key, &scope.org, &scope.project, &scope.env.as_str()],
        ).await?;
        transaction
            .execute(
                "INSERT INTO wamn_run.gate_reports (env_key,wiring_hash,passed,summary) \
             VALUES ($1,'sha256:'||repeat('a',64),true,'{}')",
                &[&env_key],
            )
            .await?;
    }
    transaction.batch_execute(
        "INSERT INTO registry.target(name,type,status) VALUES ('delete-fixture','local','active'); \
         UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='delete-fixture')",
    ).await?;
    let scratch = ScratchRoot::create()?;
    let store = wamn_credential_store::file::FileStore::new(scratch.path().to_path_buf());
    let mut auth = AuthDrops::default();
    let mut changes = ConnectionDrops::default();
    for (operation, request) in [
        (
            "integration.write",
            json!({"project":"app","name":"erp","integration_type":"http","auth_type":"http_basic"}),
        ),
        (
            "connection.write",
            json!({"project":"app","env":"dev","name":"erp","parameters":{"primary-authority":"https://example.invalid","tls-verification":"disabled"},"credential":"{\"username\":\"fixture\",\"password\":\"private\"}"}),
        ),
    ] {
        administration::execute_with_store(
            &transaction,
            "acme",
            operation,
            &request.to_string(),
            &mut auth,
            &mut changes,
            Some(&store),
        )
        .await?;
    }
    transaction.commit().await?;
    let credential = system
        .query_one(
            "SELECT id::text, ref, version FROM identity.credential",
            &[],
        )
        .await?;
    let id: String = credential.get(0);
    let reference: String = credential.get(1);
    let version: String = credential.get(2);
    assert_control_facts(&mut system, &triple, true).await?;
    assert_control_facts(&mut system, &other, true).await?;

    let mut blocker = connect(database.url()).await?;
    let blocker_pid: i32 = blocker
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    let held = blocker.transaction().await?;
    held.batch_execute("LOCK TABLE catalog.tenant_environments IN SHARE MODE")
        .await?;
    let mut deleting = connect(database.url()).await?;
    deleting.batch_execute("SET ROLE wamn_system").await?;
    let deleting_pid: i32 = deleting
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    let url = database.url().to_owned();
    let deleting_triple = triple.clone();
    let task = tokio::spawn(async move {
        LifecycleLock::hold(&url, &deleting_triple, async |_| {
            delete_registry_rows(&mut deleting, &deleting_triple, "aaaaaaa1", Some(&store)).await
        })
        .await
    });
    // PostgreSQL stops the real delete after connection removal, before commit.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = system
                .query_one(
                    "SELECT $1 = ANY(pg_blocking_pids($2))",
                    &[&blocker_pid, &deleting_pid],
                )
                .await?
                .get(0);
            if waiting {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert!(!scratch.path().join(&reference).exists());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    held.rollback().await?;
    // Wait for cancellation to close the database connection and roll back.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if system
                .query_opt(
                    "SELECT 1 FROM pg_stat_activity WHERE pid=$1",
                    &[&deleting_pid],
                )
                .await?
                .is_none()
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert!(system.query_opt("SELECT 1 FROM registry.project_envs WHERE org='acme' AND project='app' AND env='dev'", &[]).await?.is_some());
    let retained = system
        .query_one(
            "SELECT ref, version FROM identity.credential WHERE id=$1::text::uuid",
            &[&id],
        )
        .await?;
    assert_eq!(retained.get::<_, String>(0), reference);
    assert_eq!(retained.get::<_, String>(1), version);
    assert_control_facts(&mut system, &triple, true).await?;
    let store = wamn_credential_store::file::FileStore::new(scratch.path().to_path_buf());
    for _ in 0..2 {
        LifecycleLock::hold(database.url(), &triple, async |_| {
            delete_registry_rows(&mut system, &triple, "aaaaaaa1", Some(&store)).await
        })
        .await?;
    }
    assert!(system.query_opt("SELECT 1 FROM registry.project_envs WHERE org='acme' AND project='app' AND env='dev'", &[]).await?.is_none());
    assert!(
        system
            .query_opt(
                "SELECT 1 FROM identity.credential WHERE id=$1::text::uuid",
                &[&id]
            )
            .await?
            .is_none()
    );
    assert!(system.query_opt("SELECT 1 FROM registry.connection WHERE org='acme' AND project='app' AND env='dev'", &[]).await?.is_none());
    assert_control_facts(&mut system, &triple, false).await?;
    assert_control_facts(&mut system, &other, true).await?;
    Ok(())
}

async fn assert_control_facts(
    system: &mut Client,
    triple: &Triple,
    present: bool,
) -> anyhow::Result<()> {
    let env_key =
        wamn_control_provision::project_env_key(&triple.org, &triple.project, &triple.env);
    for (table, trigger) in CONTROL_TABLES {
        let transaction = provisioning_transaction(system).await?;
        transaction
            .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
            .await?;
        assert_eq!(
            transaction
                .query_opt(
                    &format!("SELECT 1 FROM {table} WHERE env_key=$1"),
                    &[&env_key]
                )
                .await?
                .is_some(),
            present,
            "control fact in {table} for {triple}"
        );
        if present && trigger.is_some() {
            let error = transaction
                .execute(
                    &format!("DELETE FROM {table} WHERE env_key=$1"),
                    &[&env_key],
                )
                .await
                .expect_err("control facts remain immutable outside environment deletion");
            assert_eq!(
                error.code(),
                Some(&tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
            );
        }
        transaction.rollback().await?;
    }
    Ok(())
}
