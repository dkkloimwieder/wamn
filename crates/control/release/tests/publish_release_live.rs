//! Disposable-PostgreSQL tests for package sealing at release publication.

use std::time::Duration;

use tokio_postgres::{Client, NoTls};
use wamn_test_infrastructure::locked_database;

const ENV_KEY: &str = "publish-release-live";
const INSERT_MIGRATION_SQL: &str = "\
INSERT INTO catalog.package_migrations (\
       env_key, package_id, package_version, ordinal, relative_path, sha256\
     ) VALUES ($1, $2, $3, $4, $5, $6)";
/// The project database records a release as its manifest bytes, and its
/// package membership is read from them.
const INSERT_PROJECT_MEMBERSHIP_SQL: &str = "\
INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) \
SELECT $1, 'sha256:' || encode(sha256($2::bytea), 'hex'), $2::bytea";
const PROJECT_MANIFEST: &[u8] =
    br#"{"release":{"packages":[{"package-id":"inventory","package-version":"1.0.0"}]}}"#;

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
        .query_one("SELECT set_config('app.env_key', $1, false)", &[&ENV_KEY])
        .await
        .expect("claim the test tenant");
    client.execute(
        "INSERT INTO catalog.packages (env_key, package_id, package_version, artifact_digest, physical_schema_fingerprint) VALUES ($1, 'inventory', '1.0.0', $2, $3)",
        &[&ENV_KEY, &format!("sha256:{}", "a".repeat(64)), &format!("sha256:{}", "c".repeat(64))]
    ).await.expect("record the installed baseline");
    client
        .execute(
            INSERT_MIGRATION_SQL,
            &[
                &ENV_KEY,
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
        assert_eq!(
            database_error.message(),
            "catalog.package_migrations is immutable"
        );
    }
}

async fn assert_package_seal(url: &str) {
    let installer = connect(url).await;
    seed_package(&installer).await;
    assert_immutable_rows(&installer).await;

    let mut publisher = connect(url).await;
    let migrator = connect(url).await;
    migrator
        .query_one("SELECT set_config('app.env_key', $1, false)", &[&ENV_KEY])
        .await
        .expect("claim the migration tenant");
    let publication = publisher
        .transaction()
        .await
        .expect("begin release membership");
    publication
        .query_one("SELECT set_config('app.env_key', $1, true)", &[&ENV_KEY])
        .await
        .expect("claim the publication tenant");
    publication
        .execute(
            INSERT_PROJECT_MEMBERSHIP_SQL,
            &[&ENV_KEY, &PROJECT_MANIFEST],
        )
        .await
        .expect("insert the first membership while holding the package row");

    let late_hash = format!("sha256:{}", "c".repeat(64));
    let late_ordinal = 2_i32;
    let late_params: [&(dyn tokio_postgres::types::ToSql + Sync); 6] = [
        &ENV_KEY,
        &"inventory",
        &"1.0.0",
        &late_ordinal,
        &"migrations/0002_late.sql",
        &late_hash,
    ];
    let late_migration = migrator.execute(INSERT_MIGRATION_SQL, &late_params);
    tokio::pin!(late_migration);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut late_migration)
            .await
            .is_err(),
        "migration insertion did not serialize on the package row"
    );
    publication
        .commit()
        .await
        .expect("commit release membership and its seal");
    let refused = late_migration
        .await
        .expect_err("migration after committed membership must refuse");
    let refused = refused
        .as_db_error()
        .expect("the package seal refusal comes from PostgreSQL");
    assert_eq!(refused.code().code(), "55000");
    assert_eq!(refused.message(), "package-version-sealed");
    let baseline = installer
        .query_one(
            "SELECT row_to_json(p)::text FROM catalog.packages p WHERE env_key = $1",
            &[&ENV_KEY],
        )
        .await
        .unwrap()
        .get::<_, String>(0);
    let executable =
        br#"{"release":{"packages":[{"package-id":"inventory","package-version":"2.0.0"}]}}"#;
    installer
        .execute(INSERT_PROJECT_MEMBERSHIP_SQL, &[&ENV_KEY, &&executable[..]])
        .await
        .expect("cache an executable that never installed SQL");
    assert_eq!(
        installer
            .query_one(
                "SELECT row_to_json(p)::text FROM catalog.packages p WHERE env_key = $1",
                &[&ENV_KEY]
            )
            .await
            .unwrap()
            .get::<_, String>(0),
        baseline
    );
    assert!(installer.query_opt(
        "SELECT 1 FROM catalog.package_migrations WHERE env_key = $1 AND package_version = '2.0.0'", &[&ENV_KEY]
    ).await.unwrap().is_none());
}

#[tokio::test]
async fn package_seal_is_server_enforced() {
    let project = locked_database::database(wamn_catalog::test_database::tenant);
    assert_package_seal(&project).await;
}
