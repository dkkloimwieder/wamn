//! Live test of invariant 1 of `docs/plan/platform-config.md` (wamn-aqd0m.1):
//! in a project database, the application role `wamn_app` holds `SELECT` on
//! the listed `platform` tables, `EXECUTE` on `platform.bump_counter`, the
//! existing grants of `platform.write_log`, the engine's claim record, and
//! nothing else in `platform`. It holds no grant on a history table or on
//! `schema_migrations`, and no `credential`, `target` or `target_application`
//! table exists. The test reads the privileges from the server catalogs, on a
//! fresh install and on a database that `project/0025_platform_configuration.sql`
//! upgrades. It holds the process lock of its server, because the schema files
//! create cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/project/0025_platform_configuration.sql");

/// The tables of `platform` that `wamn_app` reads, as `regclass` prints them. `environment` is the status
/// that the host reads as `wamn_app` on each request
/// (`crates/platform/runtime/src/plugins/wamn_postgres/claims.rs`).
const READ_TABLES: [&str; 12] = [
    "connection",
    "environment",
    "external_system",
    "integration_allow",
    "number_counter",
    "number_sequence",
    "permissions",
    "project_connection",
    "roles",
    "setting",
    "\"user\"",
    "user_roles",
];

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// Every privilege in schema `platform` that `wamn_app` or PUBLIC holds: on
/// the schema, its relations, their columns and its routines.
async fn surface(client: &Client) -> Vec<String> {
    client
        .query(
            "WITH grantee AS (SELECT oid FROM pg_roles WHERE rolname = 'wamn_app' UNION ALL SELECT 0::oid), \
                  platform AS (SELECT oid FROM pg_namespace WHERE nspname = 'platform') \
             SELECT entry FROM ( \
               SELECT 'schema platform ' || x.privilege_type AS entry \
                 FROM pg_namespace n, aclexplode(n.nspacl) x \
                WHERE n.oid IN (SELECT oid FROM platform) AND x.grantee IN (SELECT oid FROM grantee) \
               UNION ALL \
               SELECT 'table ' || c.oid::regclass::text || ' ' || x.privilege_type \
                 FROM pg_class c, aclexplode(c.relacl) x \
                WHERE c.relnamespace IN (SELECT oid FROM platform) \
                  AND x.grantee IN (SELECT oid FROM grantee) \
               UNION ALL \
               SELECT 'column ' || c.oid::regclass::text || '.' || a.attname || ' ' || x.privilege_type \
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid, aclexplode(a.attacl) x \
                WHERE c.relnamespace IN (SELECT oid FROM platform) \
                  AND x.grantee IN (SELECT oid FROM grantee) \
               UNION ALL \
               SELECT 'routine ' || p.oid::regprocedure::text || ' ' || x.privilege_type \
                 FROM pg_proc p, \
                      aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) x \
                WHERE p.pronamespace IN (SELECT oid FROM platform) \
                  AND x.grantee IN (SELECT oid FROM grantee) \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the wamn_app surface in platform")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// Invariant 1, as the list of privilege entries `surface` reads.
fn invariant_one() -> Vec<String> {
    let mut expected: Vec<String> = READ_TABLES
        .iter()
        .map(|table| format!("table platform.{table} SELECT"))
        .chain([
            "column platform.write_log.result UPDATE".to_owned(),
            "routine platform.bump_counter(uuid,text,text) EXECUTE".to_owned(),
            "schema platform USAGE".to_owned(),
            "table platform.write_log DELETE".to_owned(),
            "table platform.write_log INSERT".to_owned(),
            "table platform.write_log SELECT".to_owned(),
        ])
        .collect();
    expected.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    expected
}

/// The control-plane tables that no application database may hold.
async fn control_plane_tables(client: &Client) -> i64 {
    client
        .query_one(
            "SELECT count(*) FROM information_schema.tables \
              WHERE table_name IN ('credential', 'target', 'target_application')",
            &[],
        )
        .await
        .expect("read the tables of the database")
        .get(0)
}

/// A project database with the catalog and the application schema of this
/// commit, as provisioning installs them.
async fn fresh_install(url: &str) -> Client {
    let client = connect(url).await;
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
        .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
        .await
        .expect("install the application schema");
    client
}

#[tokio::test]
async fn a_fresh_install_grants_the_application_role_invariant_one() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = fresh_install(&url).await;
    assert_eq!(surface(&client).await, invariant_one());
    assert_eq!(control_plane_tables(&client).await, 0);
}

#[tokio::test]
async fn the_migration_grants_the_application_role_invariant_one() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = fresh_install(&url).await;
    // The database as 0024 left it: no platform configuration tables, the
    // configurations table with its history and grants, and the api_keys read.
    client
        .batch_execute(
            "DROP FUNCTION platform.bump_counter(uuid, text, text); \
             DROP TABLE platform.setting_history, platform.number_sequence_history, \
               platform.project_connection_history, platform.integration_allow_history, \
               platform.connection_history, platform.external_system_history; \
             DROP TABLE platform.setting, platform.number_counter, platform.number_sequence, \
               platform.project_connection, platform.integration_allow, platform.connection, \
               platform.external_system; \
             CREATE TABLE platform.configurations ( \
               tenant_id text NOT NULL CHECK (tenant_id <> ''), \
               config_key text NOT NULL, \
               config_value jsonb NOT NULL, \
               created_at timestamptz NOT NULL, \
               created_by uuid NOT NULL, \
               updated_at timestamptz NOT NULL, \
               updated_by uuid NOT NULL, \
               PRIMARY KEY (tenant_id, config_key)); \
             SELECT wamn_history.create_history_table('platform', 'configurations', true); \
             GRANT SELECT, INSERT, UPDATE, DELETE ON platform.configurations TO wamn_app; \
             GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, \
               transaction_id, before, after) ON platform.configurations_history TO wamn_app; \
             GRANT SELECT ON platform.api_keys TO wamn_app;",
        )
        .await
        .expect("make the database as 0024 left it");
    let before = surface(&client).await;
    assert!(
        before
            .iter()
            .any(|entry| entry == "table platform.configurations UPDATE"),
        "the old database grants the configurations write: {before:#?}"
    );

    client
        .batch_execute(&format!("BEGIN; {MIGRATION} COMMIT;"))
        .await
        .expect("apply the migration");
    assert_eq!(surface(&client).await, invariant_one());
    assert_eq!(control_plane_tables(&client).await, 0);
}
