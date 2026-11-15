//! Live PostgreSQL grants and credential isolation on fresh installations.

use sqlx::PgConnection as Client;
use sqlx::Row as _;

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to the disposable database");

    client
}

/// The control-plane tables that no application database may hold.
async fn control_plane_tables(client: &mut Client) -> i64 {
    sqlx::query("SELECT count(*) FROM information_schema.tables \
              WHERE table_name IN ('credential', 'oauth_token', 'oauth_authorization', 'target', 'target_application', 'integrations')").fetch_one(&mut *client)
        .await
        .expect("read the tables of the database")
        .get(0)
}

/// A project database with the catalog and the application schema of this
/// commit, as provisioning installs them.
async fn install(url: &str, schema: &str) -> Client {
    let mut client = connect(url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::ensure_app_acl_role_sql(),
    ))
    .execute(&mut client)
    .await
    .expect("install the product application roles");
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut client)
        .await
        .map(drop)
        .expect("install the catalog");
    sqlx::raw_sql(sqlx::AssertSqlSafe((schema).to_string()))
        .execute(&mut client)
        .await
        .map(drop)
        .expect("install the application schema");
    client
}

#[sqlx::test(migrations = false)]
async fn a_fresh_install_keeps_control_plane_tables_out(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let url = wamn_test_postgres::url(&pool).to_string();
    let mut client = install(&url, wamn_control_provision::APP_SCHEMA_SQL).await;
    assert_eq!(control_plane_tables(&mut client).await, 0);
}

#[tokio::test]
#[ignore = "requires: postgres"]
async fn integration_and_target_seeds_live_only_in_the_system_database() {
    let mut server = wamn_test_postgres::start(&[]).unwrap();
    let database = server.create_database("wamn_system").await.unwrap();
    let mut admin = connect(database.url()).await;
    sqlx::raw_sql("CREATE ROLE wamn_db_owner NOLOGIN NOINHERIT; CREATE ROLE wamn_system LOGIN PASSWORD 'c31-system-fixture';
        ALTER DATABASE wamn_system OWNER TO wamn_system; SET ROLE wamn_system").execute(&mut admin).await.map(drop)
        .unwrap();
    sqlx::raw_sql(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .execute(&mut admin)
        .await
        .map(drop)
        .unwrap();
    let expected = vec![
        "gcs:host:false".to_owned(),
        "http:host:false".to_owned(),
        "modbus:wasm:true".to_owned(),
        "s3:host:false".to_owned(),
    ];
    let mut seeds = async || -> Vec<String> {
        sqlx::query("SELECT code || ':' || runtime_type || ':' || exclusive FROM registry.integration_type ORDER BY code").fetch_all(&mut admin).await.unwrap().iter().map(|r|r.get(0)).collect()
    };
    assert_eq!(seeds().await, expected);
    let target: String=sqlx::query("SELECT name || ':' || type || ':' || status FROM registry.target WHERE last_seen_at IS NULL").fetch_one(&mut admin).await.unwrap().get(0);
    assert_eq!(target, "cloud:cloud:active");
    let app = server.create_database("c31_application").await.unwrap();
    let mut application = install(app.url(), wamn_control_provision::APP_SCHEMA_SQL).await;
    assert_eq!(control_plane_tables(&mut application).await, 0);
}

/// Effective PostgreSQL privilege tests include inherited roles and PUBLIC.
#[sqlx::test(migrations = false)]
async fn credentials_are_inaccessible_to_application_roles(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
    sqlx::raw_sql("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN CREATE ROLE wamn_app NOLOGIN NOINHERIT; END IF; END $$").execute(&mut client).await.map(drop).unwrap();
    let exposed: Vec<String> = sqlx::query("SELECT c.oid::regclass::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_%' AND n.nspname = 'identity' AND c.relkind IN ('r','p','v','m','f') AND (has_table_privilege('wamn_app',c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') OR has_any_column_privilege('wamn_app',c.oid,'SELECT,INSERT,UPDATE,REFERENCES')) ORDER BY 1").fetch_all(&mut client).await.unwrap().iter().map(|row| row.get(0)).collect();
    assert!(
        exposed.is_empty(),
        "credential relations exposed to wamn_app: {exposed:?}"
    );
    let routines: Vec<String> = sqlx::query("SELECT p.oid::regprocedure::text FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_%' AND (p.prosrc ~* '(credential|password|secret|token|identity[.]pats)' OR p.prorettype IN (SELECT c.reltype FROM pg_class c JOIN pg_namespace owner ON owner.oid=c.relnamespace WHERE owner.nspname='identity')) AND has_function_privilege('wamn_app',p.oid,'EXECUTE') ORDER BY 1").fetch_all(&mut client).await.unwrap().iter().map(|row| row.get(0)).collect();
    assert!(
        routines.is_empty(),
        "credential routines exposed through inherited or PUBLIC access: {routines:?}"
    );
}
