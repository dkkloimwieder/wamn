//! Live test of invariant 1 of `docs/plan/platform-config.md` (wamn-aqd0m.1):
//! in a project database, the application role `wamn_app` holds `SELECT` on
//! the listed `platform` tables, `EXECUTE` on `platform.bump_counter`,
//! `platform.assign_counter` and `platform.set_counter`, the
//! existing grants of `platform.write_log`, the engine's claim record, and
//! nothing else in `platform`. It holds no grant on a history table or on
//! `schema_migrations`, and no `credential`, `target` or `target_application`
//! table exists. The test reads the privileges from the server catalogs, on a
//! fresh install and on a database that `project/0025_platform_configuration.sql`
//! and `project/0028_platform_singular_tables.sql`,
//! `project/0029_number_assignment.sql`,
//! `project/0030_number_counter_set.sql` and `project/0031_org_boundary.sql`
//! upgrade. A last test applies 0031 to a database with rows in the old shape
//! (wamn-83eth.6). It holds the process lock of its server, because the
//! schema files create cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/project/0025_platform_configuration.sql");
const SINGULAR_TABLES: &str =
    include_str!("../../../../deploy/sql/migrations/project/0028_platform_singular_tables.sql");
const PLURAL_TABLES: &str = include_str!("fixtures/platform-singular-tables-before-0028.sql");
const NUMBER_ASSIGNMENT: &str =
    include_str!("../../../../deploy/sql/migrations/project/0029_number_assignment.sql");
const NUMBER_SETTING: &str =
    include_str!("../../../../deploy/sql/migrations/project/0030_number_counter_set.sql");
const ORG_BOUNDARY: &str =
    include_str!("../../../../deploy/sql/migrations/project/0031_org_boundary.sql");
const TENANT_FLOOR: &str = include_str!("fixtures/org-boundary-before-0031.sql");

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
    "permission",
    "project_connection",
    "role",
    "setting",
    "\"user\"",
    "user_role",
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
            "routine platform.assign_counter(uuid,text,text) EXECUTE".to_owned(),
            "routine platform.bump_counter(uuid,text,text) EXECUTE".to_owned(),
            "routine platform.set_counter(uuid,text,text,bigint) EXECUTE".to_owned(),
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
    // The database as 0024 left it: the plural table names, no platform
    // configuration tables, the configurations table with its history and
    // grants, and the api_keys read, and user under the tenant floor of
    // before 0031. 0028 and 0031 render the administration surface, which
    // names the run plane schema.
    client
        .batch_execute(&format!("SET wamn.fixture_tenant = 't1'; {TENANT_FLOOR}"))
        .await
        .expect("give user the tenant floor of before 0031");
    client
        .batch_execute(PLURAL_TABLES)
        .await
        .expect("name the role, user_role, permission and api_key tables as before 0028");
    client
        .batch_execute(
            "CREATE SCHEMA wamn_run; \
             DROP FUNCTION platform.set_counter(uuid, text, text, bigint); \
             DROP FUNCTION platform.assign_counter(uuid, text, text); \
             DROP FUNCTION platform.bump_counter(uuid, text, text); \
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
    client
        .batch_execute(&format!(
            "BEGIN; {SINGULAR_TABLES} {NUMBER_ASSIGNMENT} {NUMBER_SETTING} COMMIT;"
        ))
        .await
        .expect("apply the plural table renames, the number assignment and the number setting");
    client
        .batch_execute(&format!("BEGIN; {ORG_BOUNDARY} COMMIT;"))
        .await
        .expect("drop the tenant floor");
    assert_eq!(surface(&client).await, invariant_one());
    assert_eq!(control_plane_tables(&client).await, 0);
}

/// Project migration 0031 on a database with rows in the old shape: each row
/// stays, loses its tenant, and keeps its history under its new key. The
/// single-column keys hold, and the counter functions run on the sequence and
/// the application claim alone (wamn-83eth.6).
#[tokio::test]
async fn the_org_boundary_migration_keeps_the_rows_of_the_old_shape() {
    const USER: &str = "00000000-0000-4000-8000-000000000031";
    const SYSTEM: &str = "00000000-0000-4000-8000-000000000032";
    const CONNECTION: &str = "00000000-0000-4000-8000-000000000033";
    const SEQUENCE: &str = "00000000-0000-4000-8000-000000000034";
    const TABLES: [&str; 8] = [
        "user",
        "external_system",
        "connection",
        "integration_allow",
        "project_connection",
        "number_sequence",
        "number_counter",
        "setting",
    ];
    let url = locked_database::database(wamn_test_postgres::database);
    let client = fresh_install(&url).await;
    client
        .batch_execute(&format!(
            "CREATE SCHEMA wamn_run; SET wamn.fixture_tenant = 't1'; {TENANT_FLOOR}"
        ))
        .await
        .expect("give the tables the tenant floor of before 0031");
    client
        .batch_execute(&format!(
            "SET app.user_id = '{USER}'; SET app.operation = 'admin:org-boundary-fixture'; \
             INSERT INTO platform.\"user\" (tenant_id, id, type, email) \
               VALUES ('t1', '{USER}', 'user', 'clerk@example.invalid'); \
             INSERT INTO platform.role (tenant_id, name) VALUES ('t1', 'clerk'); \
             INSERT INTO platform.user_role (tenant_id, user_id, role_name) \
               VALUES ('t1', '{USER}', 'clerk'); \
             INSERT INTO platform.api_key (tenant_id, user_id, name, key_hash, prefix) \
               VALUES ('t1', '{USER}', 'ci', 'hash-1', 'wk_a'); \
             INSERT INTO platform.external_system (tenant_id, id, integration, name) \
               VALUES ('t1', '{SYSTEM}', 'netsuite', 'erp'); \
             INSERT INTO platform.connection \
               (tenant_id, id, integration, external_system_id, name, transport, exchange) \
               VALUES ('t1', '{CONNECTION}', 'netsuite', '{SYSTEM}', 'erp-rest', 'https', 'request'); \
             INSERT INTO platform.project_connection (tenant_id, project_id, connection_id) \
               VALUES ('t1', 'shop', '{CONNECTION}'); \
             INSERT INTO platform.integration_allow (tenant_id, scope_type, scope_id, integration) \
               VALUES ('t1', 'org', 'acme', 'netsuite'); \
             INSERT INTO platform.number_sequence (tenant_id, id, application, name, pattern) \
               VALUES ('t1', '{SEQUENCE}', 'receiving', 'po', 'P-{{seq:po}}'); \
             INSERT INTO platform.number_counter (tenant_id, sequence_id, counter, key_value, value) \
               VALUES ('t1', '{SEQUENCE}', 'po', '', 7); \
             INSERT INTO platform.setting (tenant_id, scope_type, scope_id, key, value) \
               VALUES ('t1', 'org', 'acme', 'locale', '\"en\"');"
        ))
        .await
        .expect("write one row of each table in the old shape");

    client
        .batch_execute(&format!("BEGIN; {ORG_BOUNDARY} COMMIT;"))
        .await
        .expect("drop the tenant floor");

    let relations: Vec<String> = TABLES
        .iter()
        .flat_map(|table| [(*table).to_owned(), format!("{table}_history")])
        .collect();
    let tenant_columns: i64 = client
        .query_one(
            "SELECT count(*) FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
              WHERE c.relnamespace = 'platform'::regnamespace AND a.attname = 'tenant_id' \
                AND NOT a.attisdropped AND c.relname = ANY($1)",
            &[&relations],
        )
        .await
        .expect("read the tenant columns")
        .get(0);
    assert_eq!(tenant_columns, 0, "no table of the scope keeps tenant_id");
    for table in TABLES {
        let rows: i64 = client
            .query_one(&format!("SELECT count(*) FROM platform.\"{table}\""), &[])
            .await
            .expect("count the rows")
            .get(0);
        assert_eq!(rows, 1, "platform.{table} keeps its row");
    }
    for table in TABLES.iter().filter(|table| **table != "number_counter") {
        let row = client
            .query_one(
                &format!(
                    "SELECT count(*), count(*) FILTER (WHERE row_key ? 'tenant_id') \
                       FROM platform.{table}_history"
                ),
                &[],
            )
            .await
            .expect("read the history entries");
        assert_eq!(
            (row.get::<_, i64>(0), row.get::<_, i64>(1)),
            (1, 0),
            "platform.{table}_history keeps its entry under the new key"
        );
    }
    assert_eq!(surface(&client).await, invariant_one());

    let bumped: i64 = client
        .query_one(
            &format!(
                "SELECT platform.bump_counter('{SEQUENCE}', 'po', '') \
                   FROM (SELECT set_config('app.application', 'receiving', false)) AS claim"
            ),
            &[],
        )
        .await
        .expect("bump the counter of the claimed application")
        .get(0);
    assert_eq!(bumped, 7);
    let refused = client
        .batch_execute(&format!(
            "SELECT set_config('app.application', 'wms', false); \
             SELECT platform.bump_counter('{SEQUENCE}', 'po', '')"
        ))
        .await
        .expect_err("a sequence of another application is refused");
    assert_eq!(
        refused.code(),
        Some(&tokio_postgres::error::SqlState::UNDEFINED_OBJECT),
        "{refused}"
    );
    client
        .batch_execute(&format!(
            "DELETE FROM platform.\"user\" WHERE id = '{USER}'"
        ))
        .await
        .expect("remove the user");
    let cascaded: i64 = client
        .query_one(
            "SELECT (SELECT count(*) FROM platform.user_role) \
                  + (SELECT count(*) FROM platform.api_key)",
            &[],
        )
        .await
        .expect("count the rows of the user")
        .get(0);
    assert_eq!(
        cascaded, 0,
        "the user's foreign keys remove its role and key rows"
    );
}
