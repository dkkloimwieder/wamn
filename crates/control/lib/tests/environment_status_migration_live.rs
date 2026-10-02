//! Live test of `system/0008_environment_status.sql` (wamn-zua8.2): on a
//! control database installed before it, the migration gives
//! `registry.project_envs` the status column as a fresh install has it. The
//! test holds the process lock of its server, because the installer creates
//! cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0008_environment_status.sql");

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// The columns and constraints of `registry.project_envs`.
async fn definition(client: &Client) -> Vec<String> {
    client
        .query(
            "SELECT entry FROM ( \
               SELECT 'column ' || a.attname || ' ' || format_type(a.atttypid, a.atttypmod) \
                      || ' ' || a.attnotnull::text || ' ' \
                      || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS entry \
                 FROM pg_attribute a \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                WHERE a.attrelid = 'registry.project_envs'::regclass \
                  AND a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL \
               SELECT 'constraint ' || conname || ' ' || pg_get_constraintdef(oid) \
                 FROM pg_constraint WHERE conrelid = 'registry.project_envs'::regclass \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the definition of registry.project_envs")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn the_migration_adds_the_status_column_as_a_fresh_install_has_it() {
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
            .any(|entry| entry == "column status text true 'active'::text"),
        "a fresh install has the status column: {fresh:?}"
    );

    client
        .batch_execute("ALTER TABLE registry.project_envs DROP COLUMN status")
        .await
        .expect("make the table as 0007 left it");
    client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {MIGRATION} COMMIT;"
        ))
        .await
        .expect("apply system/0008 as wamn_system");
    assert_eq!(
        definition(&client).await,
        fresh,
        "the migration adds the status column as a fresh install has it"
    );
}
