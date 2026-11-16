//! Environment deletion refusals and cancellation of the final transaction.

use sqlx::{Connection as _, Row as _};
use std::time::Duration;

use serde_json::json;
use sqlx::PgConnection as Client;
use wamn_broker::{auth_changes::AuthDrops, connection_changes::ConnectionDrops};
use wamn_catalog::lifecycle_lock::LifecycleLock;
use wamn_environment::{
    environment::delete::delete_registry_rows, provision_project_env::provisioning_transaction,
};
use wamn_platform_identity::connections::administration;
use wamn_registry::Triple;


async fn connect(url: &str) -> anyhow::Result<Client> {
    let client = wamn_postgres::connect(url).await?;

    Ok(client)
}

#[sqlx::test(migrations = false)]
async fn cancellation_retains_registry_inputs_until_cleanup_commits(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut system = connect(database.as_str()).await?;
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut system)
        .await
        .map(drop)?;
    let triple = Triple::new(
        "acme".parse().unwrap(),
        "dev".parse().unwrap(),
        "app".parse().unwrap(),
    );
    let other = Triple::new(
        "acme".parse().unwrap(),
        "prod".parse().unwrap(),
        "app".parse().unwrap(),
    );
    let mut transaction = provisioning_transaction(&mut system).await?;
    for target in [&triple, &other] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::seed_org_project_env_sql(
                &target.org,
                &target.project,
                &target.env,
            ))
                .to_string(),
        ))
        .execute(&mut *transaction)
        .await
        .map(drop)?;
    }
    sqlx::raw_sql(wamn_registry::test_util::deletion_target_sql())
        .execute(&mut *transaction)
        .await
        .map(drop)?;
    let store = wamn_test_infrastructure::credentials::MemoryStore::default();
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
            &mut transaction,
            &"acme".parse().unwrap(),
            operation,
            &request.to_string(),
            &mut auth,
            &mut changes,
            Some(&store),
        )
        .await?;
    }
    transaction.commit().await?;
    let credential = sqlx::query("SELECT id::text, ref, version FROM identity.credential")
        .fetch_one(&mut system)
        .await?;
    let id: String = credential.get(0);
    let reference: String = credential.get(1);
    let version: String = credential.get(2);

    let mut blocker = connect(database.as_str()).await?;
    let blocker_pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await?
        .get(0);
    let mut held = blocker.begin().await?;
    sqlx::raw_sql("LOCK TABLE wamn_authority.author_login_projects IN SHARE MODE")
        .execute(&mut *held)
        .await
        .map(drop)?;
    let mut deleting = connect(database.as_str()).await?;
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut deleting)
        .await
        .map(drop)?;
    let deleting_pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut deleting)
        .await?
        .get(0);
    let url = database.as_str().to_owned();
    let deleting_triple = triple.clone();
    let task = tokio::spawn(async move {
        LifecycleLock::hold(
            wamn_postgres::connect(&url).await?,
            &deleting_triple,
            async |_| delete_registry_rows(&mut deleting, &deleting_triple).await,
        )
        .await
    });
    // PostgreSQL stops the real delete after connection removal, before commit.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query("SELECT $1 = ANY(pg_blocking_pids($2))")
                .bind(&blocker_pid)
                .bind(&deleting_pid)
                .fetch_one(&mut system)
                .await?
                .get(0);
            if waiting {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    held.rollback().await?;
    // Wait for cancellation to close the database connection and roll back.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if sqlx::query("SELECT 1 FROM pg_stat_activity WHERE pid=$1")
                .bind(&deleting_pid)
                .fetch_optional(&mut system)
                .await?
                .is_none()
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert!(
        sqlx::query(wamn_registry::test_util::deleted_deployment_sql())
            .fetch_optional(&mut system)
            .await?
            .is_some()
    );
    let retained =
        sqlx::query("SELECT ref, version FROM identity.credential WHERE id=$1::text::uuid")
            .bind(&id)
            .fetch_one(&mut system)
            .await?;
    assert_eq!(retained.get::<String, _>(0), reference);
    assert_eq!(retained.get::<String, _>(1), version);
    for _ in 0..2 {
        LifecycleLock::hold(
            wamn_postgres::connect(database.as_str()).await?,
            &triple,
            async |_| delete_registry_rows(&mut system, &triple).await,
        )
        .await?;
    }
    assert!(
        sqlx::query(wamn_registry::test_util::deleted_deployment_sql())
            .fetch_optional(&mut system)
            .await?
            .is_none()
    );
    assert!(
        sqlx::query("SELECT 1 FROM identity.credential WHERE id=$1::text::uuid")
            .bind(&id)
            .fetch_optional(&mut system)
            .await?
            .is_none()
    );
    assert!(
        sqlx::query(wamn_registry::test_util::deleted_connections_sql())
            .fetch_optional(&mut system)
            .await?
            .is_none()
    );
    assert!(
        sqlx::query(wamn_registry::test_util::deployment_exists_sql())
            .bind(&other.org)
            .bind(&other.project)
            .bind(&other.env)
            .fetch_optional(&mut system)
            .await?
            .is_some()
    );

    Ok(())
}
