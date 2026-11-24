//! Live PostgreSQL grants and credential isolation on fresh installations.

use sqlx::PgConnection as Client;
use sqlx::Row as _;

async fn connect(url: &str) -> Client {
    wamn_postgres::connect(url)
        .await
        .expect("connect to the disposable database")
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn a_fresh_install_keeps_control_plane_tables_out() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    postgres.create_system_role().await.unwrap();
    let database = postgres.create_project_database("project").await.unwrap();
    let mut client = connect(database.as_str()).await;
    wamn_postgres::environment_schema::install_project(&mut client)
        .await
        .unwrap();
    let system_relations: Vec<String> = sqlx::query_scalar(
        "SELECT c.oid::regclass::text FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_roles owner ON owner.oid=c.relowner \
         WHERE owner.rolname='wamn_system'",
    )
    .fetch_all(&mut client)
    .await
    .unwrap();
    assert!(
        system_relations.is_empty(),
        "system storage in a project database: {system_relations:?}"
    );
}

/// Effective PostgreSQL privilege tests include inherited roles and PUBLIC.
#[tokio::test]
#[ignore = "requires: docker"]
async fn credentials_are_inaccessible_to_application_roles() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    postgres.create_system_role().await.unwrap();
    let database = postgres.create_database("wamn_system").await.unwrap();
    let mut client = connect(database.as_str()).await;
    wamn_provision::provision_system::install_control_store(&mut client)
        .await
        .unwrap();
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
