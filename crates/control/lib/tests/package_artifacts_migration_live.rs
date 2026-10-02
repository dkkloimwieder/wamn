//! Live test of `system/0010_package_artifacts.sql` (wamn-zua8.3): on a
//! control database installed before it, the migration creates
//! `catalog.package_artifacts` as a fresh install has it. The test holds the
//! process lock of its server, because the installer creates cluster-wide
//! roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0010_package_artifacts.sql");

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// The columns, constraints, policies, triggers, RLS flags, owner and grants
/// of `catalog.package_artifacts`.
async fn definition(client: &Client) -> Vec<String> {
    client
        .query(
            "SELECT entry FROM ( \
               SELECT 'column ' || a.attname || ' ' || format_type(a.atttypid, a.atttypmod) \
                      || ' ' || a.attnotnull::text || ' ' \
                      || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS entry \
                 FROM pg_attribute a \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                WHERE a.attrelid = 'catalog.package_artifacts'::regclass \
                  AND a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL \
               SELECT 'constraint ' || conname || ' ' || pg_get_constraintdef(oid) \
                 FROM pg_constraint WHERE conrelid = 'catalog.package_artifacts'::regclass \
               UNION ALL \
               SELECT 'policy ' || policyname || ' ' || cmd || ' ' || qual || ' ' || with_check \
                 FROM pg_policies \
                WHERE schemaname = 'catalog' AND tablename = 'package_artifacts' \
               UNION ALL \
               SELECT 'trigger ' || pg_get_triggerdef(oid) \
                 FROM pg_trigger \
                WHERE tgrelid = 'catalog.package_artifacts'::regclass AND NOT tgisinternal \
               UNION ALL \
               SELECT 'table ' || relrowsecurity::text || ' ' || relforcerowsecurity::text \
                      || ' ' || pg_get_userbyid(relowner) || ' ' || coalesce(relacl::text, '') \
                 FROM pg_class WHERE oid = 'catalog.package_artifacts'::regclass \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the definition of catalog.package_artifacts")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn the_migration_creates_the_package_artifacts_table_as_a_fresh_install_has_it() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;",
        )
        .await
        .expect("create the wamn_system role");
    provision_system(&ProvisionSystemRequest {
        system_database_url: url.to_string(),
        platform_domain: "wamn.example.test".to_owned(),
    })
    .await
    .expect("install the control store");
    let fresh = definition(&client).await;
    assert!(
        fresh
            .iter()
            .any(|entry| entry
                == "constraint package_artifacts_pkey PRIMARY KEY (package_id, version)"),
        "a fresh install has the table: {fresh:?}"
    );

    client
        .batch_execute("DROP TABLE catalog.package_artifacts")
        .await
        .expect("make the catalog as 0009 left it");
    client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {MIGRATION} COMMIT;"
        ))
        .await
        .expect("apply system/0010 as wamn_system");
    assert_eq!(
        definition(&client).await,
        fresh,
        "the migration creates the table as a fresh install has it"
    );

    // A recorded artifact is an immutable fact.
    client
        .batch_execute(
            "SET ROLE wamn_system; \
             INSERT INTO catalog.package_artifacts (package_id, version, digest) \
             VALUES ('wamn_receiving', '2.1.0', 'sha256:' || repeat('a', 64));",
        )
        .await
        .expect("record one artifact");
    let refusal = client
        .batch_execute("UPDATE catalog.package_artifacts SET digest = 'sha256:' || repeat('b', 64)")
        .await
        .expect_err("the record refuses a change");
    assert_eq!(
        refusal.code(),
        Some(&tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
    );
}
