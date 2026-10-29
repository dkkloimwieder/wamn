//! Live PostgreSQL grants and credential isolation on fresh installations.

use tokio_postgres::{Client, NoTls};
use wamn_test_infrastructure::locked_database;

/// The tables of `platform` that `wamn_app` reads, as `regclass` prints them. `environment` is the status
/// that the host reads as `wamn_app` on each request
/// (`crates/platform/runtime/src/plugins/wamn_postgres/claims.rs`).
const READ_TABLES: [&str; 13] = [
    "connection",
    "environment",
    "external_system",
    "file",
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
              WHERE table_name IN ('credential', 'target', 'target_application', 'integrations')",
            &[],
        )
        .await
        .expect("read the tables of the database")
        .get(0)
}

/// A project database with the catalog and the application schema of this
/// commit, as provisioning installs them.
async fn install(url: &str, schema: &str) -> Client {
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
        .batch_execute(schema)
        .await
        .expect("install the application schema");
    client
}

#[tokio::test]
async fn a_fresh_install_grants_the_application_role_invariant_one() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = install(&url, wamn_control_provision::APP_SCHEMA_SQL).await;
    assert_eq!(surface(&client).await, invariant_one());
    assert_eq!(control_plane_tables(&client).await, 0);
}

#[tokio::test]
async fn integration_and_target_seeds_live_only_in_the_system_database() {
    use wamn_control_provision::schema_migrations::MigrationTarget;
    let mut server = wamn_test_postgres::start(&[]).unwrap();
    let database = server.create_database("wamn_system").unwrap();
    let admin = connect(database.url()).await;
    admin
        .batch_execute(
            "CREATE ROLE wamn_db_owner NOLOGIN; CREATE ROLE wamn_system LOGIN PASSWORD 'c31-system-fixture';
        ALTER DATABASE wamn_system OWNER TO wamn_system; SET ROLE wamn_system",
        )
        .await
        .unwrap();
    admin
        .batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .await
        .unwrap();
    wamn_control::upgrade_schema::record_fresh_install(&admin, MigrationTarget::System)
        .await
        .unwrap();
    let expected = vec![
        "gcs:host:false".to_owned(),
        "http:host:false".to_owned(),
        "modbus:wasm:false".to_owned(),
        "s3:host:false".to_owned(),
    ];
    let seeds = async || -> Vec<String> {
        admin.query("SELECT code || ':' || runtime_type || ':' || exclusive FROM registry.integration_type ORDER BY code",&[]).await.unwrap().iter().map(|r|r.get(0)).collect()
    };
    assert_eq!(seeds().await, expected);
    let target: String=admin.query_one("SELECT name || ':' || type || ':' || status FROM registry.target WHERE last_seen_at IS NULL",&[]).await.unwrap().get(0);
    assert_eq!(target, "cloud:cloud:active");
    let app = server.create_database("c31_application").unwrap();
    let application = install(app.url(), wamn_control_provision::APP_SCHEMA_SQL).await;
    assert_eq!(control_plane_tables(&application).await, 0);
}

/// Effective PostgreSQL privilege checks include inherited roles and PUBLIC.
#[tokio::test]
async fn credentials_are_inaccessible_to_application_roles() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let client = connect(database.url()).await;
    client.batch_execute("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN CREATE ROLE wamn_app NOLOGIN; END IF; END $$").await.unwrap();
    let exposed: Vec<String> = client.query(
        "SELECT c.oid::regclass::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_%' AND (c.relname LIKE '%credential%' OR c.relname IN ('pats','password_tokens','password_logins')) AND c.relkind IN ('r','p','v','m','f') AND (has_table_privilege('wamn_app',c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') OR has_any_column_privilege('wamn_app',c.oid,'SELECT,INSERT,UPDATE,REFERENCES')) ORDER BY 1",
        &[],
    ).await.unwrap().iter().map(|row| row.get(0)).collect();
    assert!(
        exposed.is_empty(),
        "credential relations exposed to wamn_app: {exposed:?}"
    );
    let routines: Vec<String> = client.query(
        "SELECT p.oid::regprocedure::text FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_%' AND (p.prosrc ~* '(credential|password|secret|token|identity[.]pats)' OR p.prorettype IN (SELECT reltype FROM pg_class WHERE relname IN ('credential','password_credentials','password_tokens','password_logins','pats'))) AND has_function_privilege('wamn_app',p.oid,'EXECUTE') ORDER BY 1",
        &[],
    ).await.unwrap().iter().map(|row| row.get(0)).collect();
    assert!(
        routines.is_empty(),
        "credential routines exposed through inherited or PUBLIC access: {routines:?}"
    );
}
