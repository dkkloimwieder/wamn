//! Applied migration records remain immutable as schema state advances.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _, Statement as _};

use wamn_test_infrastructure::locked_database;

const PROJECT_ENV_KEY: &str = "publish-release-live";
const INSERT_MIGRATION_SQL: &str = "\
INSERT INTO catalog.package_migrations (\
       project_env_key, package_id, package_version, ordinal, relative_path, sha256, outcome, applied_at\
     ) VALUES ($1, $2, $3, $4, $5, $6, 'applied', now())";
async fn connect(url: &str) -> Client {
    let mut client = wamn_postgres::connect(url)
        .await
        .expect("connect to disposable PostgreSQL");

    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SELECT set_config('app.user_id', '{}', false), set_config('app.operation', 'admin:seed-catalog-fixture', false)",
        wamn_control_provision::PlatformComponent::Provisioning.principal_id()
    ))).execute(&mut client).await.map(drop).unwrap();
    client
}

async fn seed_package(client: &mut Client) {
    sqlx::query("SELECT set_config('app.project_env_key', $1, false)")
        .bind(&PROJECT_ENV_KEY)
        .fetch_one(&mut *client)
        .await
        .expect("bind the test project identity");
    sqlx::query("INSERT INTO catalog.packages (project_env_key, package_id, package_version, artifact_digest, physical_schema_fingerprint) VALUES ($1, 'inventory', '1.0.0', $2, $3)").bind(&PROJECT_ENV_KEY).bind(&format!("sha256:{}", "a".repeat(64))).bind(&format!("sha256:{}", "c".repeat(64))).execute(&mut *client).await.map(|result| result.rows_affected()).expect("record the installed baseline");
    sqlx::query(INSERT_MIGRATION_SQL)
        .bind(&PROJECT_ENV_KEY)
        .bind(&"inventory")
        .bind(&"1.0.0")
        .bind(&1_i32)
        .bind(&"migrations/0001_initial.sql")
        .bind(&format!("sha256:{}", "b".repeat(64)))
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .expect("record the migration that precedes release membership");
}

async fn assert_immutable_rows(client: &mut Client) {
    let row = sqlx::query(
        "SELECT pg_get_userbyid(proowner)::text, current_user::text, \
             NOT EXISTS (SELECT FROM aclexplode(COALESCE(proacl, acldefault('f', proowner))) \
                         WHERE grantee = 0 AND privilege_type = 'EXECUTE') \
               FROM pg_proc \
              WHERE oid = 'catalog.reject_immutable_row_change()'::regprocedure",
    )
    .fetch_one(&mut *client)
    .await
    .expect("read the installed integrity function owner and privileges");
    assert_eq!(row.get::<String, _>(0), row.get::<String, _>(1));
    assert!(
        row.get::<bool, _>(2),
        "PUBLIC cannot execute the integrity function"
    );

    for statement in [
        "UPDATE catalog.package_migrations SET sha256 = 'sha256:' || repeat('c', 64)",
        "DELETE FROM catalog.package_migrations",
    ] {
        let error = sqlx::query(sqlx::AssertSqlSafe((statement).to_string()))
            .execute(&mut *client)
            .await
            .map(|result| result.rows_affected())
            .expect_err("the installed trigger must refuse changes to recorded migration bytes");
        let database_error = error
            .as_database_error()
            .expect("PostgreSQL enforces immutability");
        assert_eq!(database_error.code().as_deref().unwrap(), "55000");
    }
}

#[tokio::test]
async fn migration_records_remain_immutable() {
    let project = locked_database::database(wamn_catalog::test_database::project);
    let mut client = connect(&project).await;
    seed_package(&mut client).await;
    assert_immutable_rows(&mut client).await;
}
