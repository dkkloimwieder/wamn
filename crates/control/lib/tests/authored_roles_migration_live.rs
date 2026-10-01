//! Live test of the two migrations of wamn-a40n.1 on databases installed
//! before them: `system/0005_admin_role.sql` and
//! `project/0002_authored_roles.sql` (docs/plan/platform-ui.md §6 issue 1).
//!
//! The project database takes the frozen `app-schema.sql` of `4495e0d20`, the
//! last commit before the authored-role model. Each test holds the process
//! lock of its server, because the installers create cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_test_infrastructure::locked_database;

const SYSTEM_MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0005_admin_role.sql");
const PROJECT_MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/project/0002_authored_roles.sql");
const RECORD_HISTORY: &str = include_str!("../../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS: &str =
    include_str!("../../../../deploy/sql/record-history-app-grants.sql");
const OLD_APP_SCHEMA: &str = include_str!("fixtures/app-schema-before-authored-roles.sql");

const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";
const SERVICE: &str = "00000000-0000-4000-8000-0000000000a1";
const BOTH: &str = "00000000-0000-4000-8000-0000000000a2";
const USER: &str = "00000000-0000-4000-8000-0000000000a3";

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

#[tokio::test]
async fn system_migration_moves_operator_services_to_admin() {
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
    // One service holds only operator, one holds operator and admin, and one
    // principal holds project-author, which the migration leaves alone.
    client
        .batch_execute(&format!(
            "BEGIN; SELECT set_config('app.user_id', '{PROVISIONING}', true); \
             INSERT INTO registry.orgs (id, placement_type) VALUES ('acme', 'dedicated'); \
             INSERT INTO registry.projects (org, id) VALUES ('acme', 'shop'); \
             INSERT INTO identity.principals (id, type, subject, display_name) VALUES \
               ('{SERVICE}', 'service', 'svc-a', 'A'), ('{BOTH}', 'service', 'svc-b', 'B'), \
               ('{USER}', 'service', 'svc-c', 'C'); \
             INSERT INTO identity.project_roles (principal_id, org, project, role) VALUES \
               ('{SERVICE}', 'acme', 'shop', 'operator'), ('{BOTH}', 'acme', 'shop', 'operator'), \
               ('{BOTH}', 'acme', 'shop', 'admin'), ('{USER}', 'acme', 'shop', 'project-author'); \
             COMMIT;"
        ))
        .await
        .expect("seed the pre-migration project roles");

    client
        .batch_execute(&format!("BEGIN; {SYSTEM_MIGRATION} COMMIT;"))
        .await
        .expect("apply system/0005");
    assert_eq!(
        column(
            &client,
            "SELECT principal_id::text || ' ' || role FROM identity.project_roles ORDER BY 1"
        )
        .await,
        [
            format!("{SERVICE} admin"),
            format!("{BOTH} admin"),
            format!("{USER} project-author"),
        ]
    );
}

#[tokio::test]
async fn project_migration_moves_roles_and_permissions_to_references() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    client
        .batch_execute(&format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
               CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOINHERIT NOREPLICATION NOBYPASSRLS; \
             END IF; END $$; \
             {RECORD_HISTORY}\n{RECORD_HISTORY_APP_GRANTS}\n{OLD_APP_SCHEMA}"
        ))
        .await
        .expect("install the application schema of 4495e0d20");
    // apply-package wrote operator and admin with every operation. The
    // authored role clerk holds one operation at two versions and one other.
    client
        .batch_execute(&format!(
            "BEGIN; SELECT set_config('app.user_id', '{PROVISIONING}', true), \
               set_config('app.operation', 'admin:migration-fixture', true); \
             INSERT INTO app_system.users (tenant_id, id, type, email) VALUES \
               ('t1', '{SERVICE}', 'service', 'a@example.invalid'), \
               ('t1', '{BOTH}', 'person', 'b@example.invalid'), \
               ('t1', '{USER}', 'person', 'c@example.invalid'); \
             INSERT INTO app_system.roles (tenant_id, name) VALUES \
               ('t1', 'operator'), ('t1', 'admin'), ('t1', 'clerk'); \
             INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES \
               ('t1', '{SERVICE}', 'operator'), ('t1', '{BOTH}', 'operator'), \
               ('t1', '{BOTH}', 'admin'), ('t1', '{USER}', 'clerk'); \
             INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES \
               ('t1', 'operator', 'shop:order/get@1.0.0'), ('t1', 'admin', 'shop:order/get@1.0.0'), \
               ('t1', 'clerk', 'shop:order/get@1.0.0'), ('t1', 'clerk', 'shop:order/get@1.1.0'), \
               ('t1', 'clerk', 'shop:order-line/create@1.1.0'); \
             COMMIT;"
        ))
        .await
        .expect("seed the pre-migration authority rows");

    client
        .batch_execute(&format!("BEGIN; {PROJECT_MIGRATION} COMMIT;"))
        .await
        .expect("apply project/0002");
    assert_eq!(
        column(&client, "SELECT name FROM app_system.roles ORDER BY name").await,
        ["admin", "clerk"]
    );
    assert_eq!(
        column(
            &client,
            "SELECT user_id::text || ' ' || role_name FROM app_system.user_roles ORDER BY 1"
        )
        .await,
        [
            format!("{SERVICE} admin"),
            format!("{BOTH} admin"),
            format!("{USER} clerk"),
        ]
    );
    assert_eq!(
        column(
            &client,
            "SELECT role_name || ' ' || permission || ' by ' || required_by \
             FROM app_system.permissions ORDER BY 1"
        )
        .await,
        [
            "clerk shop:order-line/create by shop:order-line/create",
            "clerk shop:order/get by shop:order/get",
        ]
    );
    // The new constraints hold on the migrated table.
    let refused = client
        .batch_execute(&format!(
            "BEGIN; SELECT set_config('app.user_id', '{PROVISIONING}', true), \
               set_config('app.operation', 'admin:migration-fixture', true); \
             INSERT INTO app_system.permissions (tenant_id, role_name, permission, required_by) \
             VALUES ('t1', 'clerk', 'shop:order/list', 'shop:order/query'); COMMIT;"
        ))
        .await
        .expect_err("a closure row without its root was stored");
    assert!(
        format!("{refused:?}").contains("permissions_required_by_fkey"),
        "{refused:?}"
    );
}
