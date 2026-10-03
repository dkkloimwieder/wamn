//! Fresh-install and installed-database coverage for immutable upgrade evidence.

use tokio_postgres::{Client, NoTls, error::SqlState};
use wamn_control::upgrade_schema::{UpgradeSchemaRequest, upgrade_schema};

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// Describe the database-enforced contract without database-local object IDs.
async fn carrier_shape(client: &Client) -> Vec<String> {
    client
        .query(
            "WITH carrier AS ( \
               SELECT oid, relrowsecurity, relforcerowsecurity, relacl \
                 FROM pg_class WHERE oid = 'catalog.package_upgrade_qualifications'::regclass \
             ) SELECT fact FROM ( \
               SELECT 'column ' || a.attnum || ' ' || a.attname || ' ' || \
                      format_type(a.atttypid, a.atttypmod) || ' ' || a.attnotnull || ' ' || \
                      coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS fact \
                 FROM pg_attribute a JOIN carrier c ON c.oid = a.attrelid \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                WHERE a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL SELECT 'constraint ' || conname || ' ' || pg_get_constraintdef(oid) \
                 FROM pg_constraint WHERE conrelid = (SELECT oid FROM carrier) \
               UNION ALL SELECT 'index ' || pg_get_indexdef(indexrelid) FROM pg_index \
                WHERE indrelid = (SELECT oid FROM carrier) \
               UNION ALL SELECT 'policy ' || policyname || ' ' || permissive || ' ' || \
                      roles::text || ' ' || cmd || ' ' || qual || ' ' || with_check \
                 FROM pg_policies WHERE schemaname = 'catalog' \
                  AND tablename = 'package_upgrade_qualifications' \
               UNION ALL SELECT 'trigger ' || pg_get_triggerdef(oid) FROM pg_trigger \
                WHERE tgrelid = (SELECT oid FROM carrier) AND NOT tgisinternal \
               UNION ALL SELECT 'security ' || relrowsecurity || ' ' || relforcerowsecurity \
                      || ' ' || coalesce(relacl::text, '') FROM carrier \
             ) facts ORDER BY fact COLLATE \"C\"",
            &[],
        )
        .await
        .expect("describe the qualification carrier")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

const INSERT_EVIDENCE: &str = "\
INSERT INTO catalog.package_upgrade_qualifications \
    (tenant_id, package_id, candidate_package_version, canonical_bytes, result_sha256, \
     predecessor_release_id, predecessor_manifest_digest) \
VALUES ('upgrade-live', 'platform_fixture', '2.1.0', 'proof'::bytea, \
        'sha256:' || encode(sha256('proof'::bytea), 'hex'), 1, \
        'sha256:' || encode(sha256('manifest'::bytea), 'hex'))";

#[tokio::test]
async fn fresh_and_upgrade_schema_install_the_same_immutable_carrier() {
    let mut server = wamn_test_postgres::start(&[]).expect("start disposable PostgreSQL");
    let admin = connect(server.database("postgres").unwrap().url()).await;
    let database =
        wamn_control_provision::project_env_database_name("upgrade", "carrier", "test", "a1b2c3d4");
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {}",
            wamn_pg_core::quote_ident(&database)
        ))
        .await
        .expect("create the project database");
    admin
        .batch_execute("CREATE ROLE wamn_app NOLOGIN; CREATE ROLE wamn_scenario_author NOLOGIN;")
        .await
        .expect("create catalog roles");
    let coordinate = server.database(&database).unwrap();
    let mut client = connect(coordinate.url()).await;
    client
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .expect("install the fresh catalog");
    let fresh = carrier_shape(&client).await;
    assert!(
        fresh
            .iter()
            .any(|fact| fact.starts_with("security true true"))
    );
    assert!(fresh.iter().any(|fact| fact.contains("_immutable")));

    // A pre-change project holds migrations 1–8 but has no evidence carrier.
    // Only this registry projection is needed to register the disposable target.
    client
        .batch_execute(
            "DROP TABLE catalog.package_upgrade_qualifications; \
             CREATE SCHEMA app_system; \
             CREATE SCHEMA registry; \
             CREATE TABLE registry.project_envs \
               (org text, project text, env text, instance_suffix text); \
             INSERT INTO registry.project_envs \
               VALUES ('upgrade', 'carrier', 'test', 'a1b2c3d4');",
        )
        .await
        .expect("prepare the installed pre-change database");
    let request = UpgradeSchemaRequest {
        system_database_url: coordinate.url().to_owned(),
        admin_database_url: Some(coordinate.url().to_owned()),
        baseline: Some(8),
        confirm: true,
    };
    upgrade_schema(&request)
        .await
        .expect("upgrade the installed project through the production verb");
    assert_eq!(carrier_shape(&client).await, fresh);
    let migrations: i64 = client
        .query_one("SELECT count(*) FROM app_system.schema_migrations", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(migrations, 9);
    upgrade_schema(&UpgradeSchemaRequest {
        baseline: None,
        ..request
    })
    .await
    .expect("retry an installed schema upgrade without mutation");

    client
        .batch_execute(
            "INSERT INTO catalog.packages \
               (tenant_id, package_id, package_version, manifest_sha256) \
             VALUES ('upgrade-live', 'platform_fixture', '2.1.0', 'sha256:' || repeat('a', 64)); \
             INSERT INTO catalog.effective_releases \
               (tenant_id, effective_release_id, environment) VALUES ('upgrade-live', 1, 'test'); \
             INSERT INTO catalog.release_manifest_snapshots \
               (tenant_id, effective_release_id, manifest_digest, canonical_bytes) \
             VALUES ('upgrade-live', 1, \
               'sha256:' || encode(sha256('manifest'::bytea), 'hex'), 'manifest'::bytea);",
        )
        .await
        .expect("seed the exact candidate and predecessor identities");
    let mismatch =
        INSERT_EVIDENCE.replace("encode(sha256('proof'::bytea), 'hex')", "repeat('0', 64)");
    let error = client.batch_execute(&mismatch).await.unwrap_err();
    assert_eq!(error.code(), Some(&SqlState::CHECK_VIOLATION));

    let transaction = client.transaction().await.unwrap();
    transaction.batch_execute(INSERT_EVIDENCE).await.unwrap();
    transaction.rollback().await.unwrap();
    let count: i64 = client
        .query_one(
            "SELECT count(*) FROM catalog.package_upgrade_qualifications",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        count, 0,
        "evidence rolls back with its enclosing transaction"
    );
    client.batch_execute(INSERT_EVIDENCE).await.unwrap();
    let error = client.batch_execute(INSERT_EVIDENCE).await.unwrap_err();
    assert_eq!(error.code(), Some(&SqlState::UNIQUE_VIOLATION));
    for mutation in [
        "UPDATE catalog.package_upgrade_qualifications SET recorded_at = now()",
        "DELETE FROM catalog.package_upgrade_qualifications",
    ] {
        let error = client.batch_execute(mutation).await.unwrap_err();
        assert_eq!(
            error.code(),
            Some(&SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
        );
    }
    let app_access: bool = client
        .query_one(
            "SELECT has_table_privilege('wamn_app', \
             'catalog.package_upgrade_qualifications', 'SELECT,INSERT,UPDATE,DELETE')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        !app_access,
        "runtime applications cannot access upgrade evidence"
    );
    drop(client);
    drop(admin);
    server.stop().expect("clean up the owned PostgreSQL server");
}
