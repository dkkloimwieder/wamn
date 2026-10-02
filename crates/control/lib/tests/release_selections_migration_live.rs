//! Live test of `system/0011_release_selections.sql` (wamn-zua8.3): on a
//! control database installed before it, the migration creates
//! `catalog.qualifications` and `catalog.release_selections` as a fresh
//! install has them. The test holds the process lock of its server, because
//! the installer creates cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0011_release_selections.sql");

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
/// of the two tables.
async fn definition(client: &Client) -> Vec<String> {
    client
        .query(
            "WITH t(oid) AS (VALUES ('catalog.qualifications'::regclass), \
                                    ('catalog.release_selections'::regclass)) \
             SELECT entry FROM ( \
               SELECT a.attrelid::regclass::text || ' column ' || a.attname || ' ' \
                      || format_type(a.atttypid, a.atttypmod) || ' ' || a.attnotnull::text \
                      || ' ' || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS entry \
                 FROM pg_attribute a \
                 JOIN t ON t.oid = a.attrelid \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                WHERE a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL \
               SELECT conrelid::regclass::text || ' constraint ' || conname || ' ' \
                      || pg_get_constraintdef(c.oid) \
                 FROM pg_constraint c JOIN t ON t.oid = c.conrelid \
               UNION ALL \
               SELECT p.polrelid::regclass::text || ' policy ' || p.polname || ' ' \
                      || coalesce(pg_get_expr(p.polqual, p.polrelid), '') || ' ' \
                      || coalesce(pg_get_expr(p.polwithcheck, p.polrelid), '') \
                 FROM pg_policy p JOIN t ON t.oid = p.polrelid \
               UNION ALL \
               SELECT 'trigger ' || pg_get_triggerdef(g.oid) \
                 FROM pg_trigger g JOIN t ON t.oid = g.tgrelid WHERE NOT g.tgisinternal \
               UNION ALL \
               SELECT c.oid::regclass::text || ' table ' || c.relrowsecurity::text || ' ' \
                      || c.relforcerowsecurity::text || ' ' || pg_get_userbyid(c.relowner) \
                      || ' ' || coalesce(c.relacl::text, '') \
                 FROM pg_class c JOIN t ON t.oid = c.oid \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the definition of the two tables")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn the_migration_creates_the_selection_tables_as_a_fresh_install_has_them() {
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
        fresh.iter().any(|entry| entry
            == "catalog.release_selections constraint release_selections_qualification_fkey \
                FOREIGN KEY (qualification_sha256) \
                REFERENCES catalog.qualifications(qualification_sha256)"),
        "a fresh install has the tables: {fresh:?}"
    );

    client
        .batch_execute("DROP TABLE catalog.release_selections, catalog.qualifications")
        .await
        .expect("make the catalog as 0010 left it");
    client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {MIGRATION} COMMIT;"
        ))
        .await
        .expect("apply system/0011 as wamn_system");
    assert_eq!(
        definition(&client).await,
        fresh,
        "the migration creates the tables as a fresh install has them"
    );
}
