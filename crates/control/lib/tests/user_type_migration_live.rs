//! Live test of `project/0004_user_type.sql` (wamn-a40n.3): on a project
//! database installed before it, the migration changes the user type `person`
//! to `user` as a fresh install has it, and the changed row stamps
//! `wamn:provisioning`. The test holds the process lock of its server, because
//! the schema creates cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/project/0004_user_type.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");
const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn column(client: &Client, statement: &str) -> Vec<String> {
    client
        .query(statement, &[])
        .await
        .expect(statement)
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// The constraints of `app_system.users`.
async fn constraints(client: &Client) -> Vec<String> {
    column(
        client,
        "SELECT conname || ' ' || pg_get_constraintdef(oid) FROM pg_constraint \
          WHERE conrelid = 'app_system.users'::regclass ORDER BY 1",
    )
    .await
}

#[tokio::test]
async fn the_migration_changes_the_user_type_as_a_fresh_install_has_it() {
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
    let fresh = constraints(&client).await;

    // The database as 0003 left it, with one `person` row.
    client
        .batch_execute(
            "ALTER TABLE app_system.users DROP CONSTRAINT users_type_check, \
               ADD CONSTRAINT users_type_check CHECK (type IN ('person', 'service', 'platform')); \
             BEGIN; \
             SELECT pg_catalog.set_config('app.user_id', '00000000-0000-4000-8000-0000000000aa', true), \
                    pg_catalog.set_config('app.operation', 'admin:seed-user-type-fixture', true); \
             INSERT INTO app_system.users (tenant_id, id, type, email) \
               VALUES ('t1', '00000000-0000-4000-8000-000000000001', 'person', 'u1@example.test'); \
             COMMIT;",
        )
        .await
        .expect("make the database as 0003 left it");

    client
        .batch_execute(&format!("BEGIN; {MIGRATION} COMMIT;"))
        .await
        .expect("apply the migration");
    assert_eq!(
        constraints(&client).await,
        fresh,
        "the migration changes the check as a fresh install has it"
    );
    assert_eq!(
        column(
            &client,
            "SELECT type || ' ' || updated_by FROM app_system.users \
              WHERE id = '00000000-0000-4000-8000-000000000001'"
        )
        .await,
        [format!("user {PROVISIONING}")],
        "the row changes and stamps wamn:provisioning"
    );
}
