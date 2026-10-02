//! Live test of `project/0006_administration_release_head.sql` (wamn-a40n.2,
//! wamn-a40n.9): on an installed project database, the latest migration of the
//! administration surface gives `wamn_administration` the exact surface that
//! provisioning grants. The test holds the process lock of
//! its server, because the file creates a cluster-wide role.

use tokio_postgres::{Client, NoTls};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/project/0006_administration_release_head.sql");
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

/// Every privilege that names `wamn_administration` in this database: on
/// schemas, relations, columns and routines.
async fn surface(client: &Client) -> Vec<String> {
    client
        .query(
            "WITH grantee AS (SELECT oid FROM pg_roles WHERE rolname = 'wamn_administration') \
             SELECT entry FROM ( \
               SELECT 'schema ' || n.nspname || ' ' || x.privilege_type AS entry \
                 FROM pg_namespace n, aclexplode(n.nspacl) x, grantee g WHERE x.grantee = g.oid \
               UNION ALL \
               SELECT 'table ' || c.oid::regclass::text || ' ' || x.privilege_type \
                 FROM pg_class c, aclexplode(c.relacl) x, grantee g WHERE x.grantee = g.oid \
               UNION ALL \
               SELECT 'column ' || c.oid::regclass::text || '.' || a.attname || ' ' || x.privilege_type \
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid, \
                      aclexplode(a.attacl) x, grantee g WHERE x.grantee = g.oid \
               UNION ALL \
               SELECT 'routine ' || p.oid::regprocedure::text || ' ' || x.privilege_type \
                 FROM pg_proc p, aclexplode(p.proacl) x, grantee g WHERE x.grantee = g.oid \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the administration surface")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn the_migration_grants_the_provisioned_administration_surface() {
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
             END $$; \
             CREATE SCHEMA wamn_run;",
        )
        .await
        .expect("create the platform roles and the run plane schema");
    client
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .expect("install the catalog");
    client
        .batch_execute(APP_SCHEMA)
        .await
        .expect("install the application schema");

    client
        .batch_execute(&format!("BEGIN; {MIGRATION} COMMIT;"))
        .await
        .expect("apply the migration");
    let migrated = surface(&client).await;
    for expected in [
        "column app_system.roles_history.operation INSERT",
        "routine wamn_authority.tenant_key(text) EXECUTE",
        "routine wamn_history.row_image(record) EXECUTE",
        "schema app_system USAGE",
        "schema catalog USAGE",
        "table catalog.effective_release_heads SELECT",
        "table app_system.permissions DELETE",
        "table app_system.user_roles INSERT",
        "table app_system.users UPDATE",
    ] {
        assert!(
            migrated.iter().any(|entry| entry == expected),
            "the migration grants {expected}: {migrated:#?}"
        );
    }

    client
        .batch_execute(&wamn_control_provision::sql::grant_administration_surface_sql("wamn_run"))
        .await
        .expect("apply the provisioned surface");
    assert_eq!(
        surface(&client).await,
        migrated,
        "the migration and provisioning grant one surface"
    );
}
