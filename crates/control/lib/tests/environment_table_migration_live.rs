//! Live test of `project/0007_environment_status.sql` (wamn-zua8.2): on a
//! project database installed before it, the migration creates
//! `app_system.environment` and its history table as a fresh install has
//! them. The test holds the process lock of its server, because the catalog
//! creates cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/project/0007_environment_status.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// The columns, constraints, triggers, policies, indexes and grants of the
/// environment table and its history table.
async fn definition(client: &Client) -> Vec<String> {
    client
        .query(
            "WITH relation AS (SELECT oid FROM pg_class WHERE oid IN \
               ('app_system.environment'::regclass, 'app_system.environment_history'::regclass)) \
             SELECT entry FROM ( \
               SELECT 'column ' || a.attrelid::regclass::text || '.' || a.attname || ' ' \
                      || format_type(a.atttypid, a.atttypmod) || ' ' || a.attnotnull::text \
                      || ' ' || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS entry \
                 FROM pg_attribute a JOIN relation r ON r.oid = a.attrelid \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                WHERE a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL \
               SELECT 'constraint ' || conrelid::regclass::text || ' ' || conname || ' ' \
                      || pg_get_constraintdef(c.oid) \
                 FROM pg_constraint c JOIN relation r ON r.oid = c.conrelid \
               UNION ALL \
               SELECT 'trigger ' || pg_get_triggerdef(t.oid) \
                 FROM pg_trigger t JOIN relation r ON r.oid = t.tgrelid WHERE NOT t.tgisinternal \
               UNION ALL \
               SELECT 'policy ' || p.polrelid::regclass::text || ' ' || p.polname || ' ' \
                      || p.polcmd::text || ' ' || p.polroles::regrole[]::text || ' ' \
                      || coalesce(pg_get_expr(p.polqual, p.polrelid), '') || ' ' \
                      || coalesce(pg_get_expr(p.polwithcheck, p.polrelid), '') \
                 FROM pg_policy p JOIN relation r ON r.oid = p.polrelid \
               UNION ALL \
               SELECT 'index ' || pg_get_indexdef(i.indexrelid) \
                 FROM pg_index i JOIN relation r ON r.oid = i.indrelid \
               UNION ALL \
               SELECT 'security ' || c.oid::regclass::text || ' ' || c.relrowsecurity::text \
                      || ' ' || c.relforcerowsecurity::text \
                 FROM pg_class c JOIN relation r ON r.oid = c.oid \
               UNION ALL \
               SELECT 'grant ' || c.oid::regclass::text || ' ' || x.grantee::regrole::text \
                      || ' ' || x.privilege_type \
                 FROM pg_class c JOIN relation r ON r.oid = c.oid, aclexplode(c.relacl) x \
                WHERE x.grantee <> c.relowner \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the definition of the environment tables")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn the_migration_creates_the_environment_table_as_a_fresh_install_has_it() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    client
        .batch_execute(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await
        .expect("create the platform roles");
    client
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .expect("install the catalog");
    client
        .batch_execute(APP_SCHEMA)
        .await
        .expect("install the application schema");
    let fresh = definition(&client).await;
    assert!(
        fresh
            .iter()
            .any(|entry| entry == "grant app_system.environment wamn_app SELECT"),
        "a fresh install grants wamn_app the read: {fresh:#?}"
    );

    client
        .batch_execute("DROP TABLE app_system.environment_history, app_system.environment")
        .await
        .expect("make the database as project migration 0006 left it");
    client
        .batch_execute(&format!("BEGIN; {MIGRATION} COMMIT;"))
        .await
        .expect("apply project/0007");
    assert_eq!(
        definition(&client).await,
        fresh,
        "the migration creates the environment tables as a fresh install has them"
    );
}
