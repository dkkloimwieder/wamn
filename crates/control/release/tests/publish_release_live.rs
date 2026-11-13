//! Applied migration records remain immutable as schema state advances.

use tokio_postgres::{Client, NoTls};
use wamn_test_infrastructure::locked_database;

const PROJECT_ENV_KEY: &str = "publish-release-live";
const INSERT_MIGRATION_SQL: &str = "\
INSERT INTO catalog.package_migrations (\
       project_env_key, package_id, package_version, ordinal, relative_path, sha256, outcome, applied_at\
     ) VALUES ($1, $2, $3, $4, $5, $6, 'applied', now())";
async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client.batch_execute(&format!(
        "SELECT set_config('app.user_id', '{}', false), set_config('app.operation', 'admin:seed-catalog-fixture', false)",
        wamn_control_provision::PlatformComponent::Provisioning.principal_id()
    )).await.unwrap();
    client
}

async fn seed_package(client: &Client) {
    client
        .query_one(
            "SELECT set_config('app.project_env_key', $1, false)",
            &[&PROJECT_ENV_KEY],
        )
        .await
        .expect("claim the test project");
    client.execute(
        "INSERT INTO catalog.packages (project_env_key, package_id, package_version, artifact_digest, physical_schema_fingerprint) VALUES ($1, 'inventory', '1.0.0', $2, $3)",
        &[&PROJECT_ENV_KEY, &format!("sha256:{}", "a".repeat(64)), &format!("sha256:{}", "c".repeat(64))]
    ).await.expect("record the installed baseline");
    client
        .execute(
            INSERT_MIGRATION_SQL,
            &[
                &PROJECT_ENV_KEY,
                &"inventory",
                &"1.0.0",
                &1_i32,
                &"migrations/0001_initial.sql",
                &format!("sha256:{}", "b".repeat(64)),
            ],
        )
        .await
        .expect("record the migration that precedes release membership");
}

async fn assert_immutable_rows(client: &Client) {
    let row = client
        .query_one(
            "SELECT pg_get_userbyid(proowner)::text, current_user::text, \
             NOT EXISTS (SELECT FROM aclexplode(COALESCE(proacl, acldefault('f', proowner))) \
                         WHERE grantee = 0 AND privilege_type = 'EXECUTE') \
               FROM pg_proc \
              WHERE oid = 'catalog.reject_immutable_row_change()'::regprocedure",
            &[],
        )
        .await
        .expect("read the installed integrity function owner and privileges");
    assert_eq!(row.get::<_, String>(0), row.get::<_, String>(1));
    assert!(
        row.get::<_, bool>(2),
        "PUBLIC cannot execute the integrity function"
    );

    for statement in [
        "UPDATE catalog.package_migrations SET sha256 = 'sha256:' || repeat('c', 64)",
        "DELETE FROM catalog.package_migrations",
    ] {
        let error = client
            .execute(statement, &[])
            .await
            .expect_err("the installed trigger must refuse changes to recorded migration bytes");
        let database_error = error
            .as_db_error()
            .expect("PostgreSQL enforces immutability");
        assert_eq!(database_error.code().code(), "55000");
    }
}

#[tokio::test]
async fn migration_records_remain_immutable() {
    let project = locked_database::database(wamn_catalog::test_database::project);
    let client = connect(&project).await;
    seed_package(&client).await;
    assert_immutable_rows(&client).await;
}
