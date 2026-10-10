//! Live test of authored role permissions as roots
//! (docs/plan/platform-ui.md §4.1, docs/plan/platform-deploy.md R18).
//!
//! A grant stores the root, served by a release or not, and no row holds a
//! closure. Project migration 0011 removes the closure rows an installed
//! database still holds. The test holds the process lock of its server,
//! because the schema installer creates cluster-wide roles.

use std::collections::{BTreeMap, BTreeSet};

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ArtifactHash, ServingComponent, ServingComponentOperation};
use wamn_control::role_permissions::{
    ReleaseClosures, create_role, delete_role, grant_permission, revoke_permission,
};
use wamn_test_infrastructure::locked_database::{self, LockedDatabase};

const RECORD_HISTORY: &str = include_str!("../../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS: &str =
    include_str!("../../../../deploy/sql/record-history-app-grants.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");
const PERMISSION_ROOTS: &str =
    include_str!("../../../../deploy/sql/migrations/project/0011_permission_roots.sql");
const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";

/// One component of `package` whose exports register the given operations,
/// each with its folded call-graph permissions.
fn component(package: &str, operations: &[(&str, &[&str])]) -> ServingComponent {
    ServingComponent {
        package_id: package.to_owned(),
        component: package.to_owned(),
        interface_version: "1".to_owned(),
        digest: ArtifactHash::parse(format!("sha256:{}", "0".repeat(64))).unwrap(),
        operations: operations
            .iter()
            .enumerate()
            .map(|(index, (sealed, required))| {
                (
                    format!("export-{index}"),
                    ServingComponentOperation {
                        pre_commit: None,
                        registered_operation: Some((*sealed).to_owned()),
                        permissions: std::iter::once(*sealed)
                            .chain(required.iter().copied())
                            .map(str::to_owned)
                            .collect(),
                        fresh_only: false,
                        committed_result_schema: None,
                        participant: None,
                        statements: BTreeMap::new(),
                    },
                )
            })
            .collect(),
        descriptor: wamn_catalog::ComponentDescriptor::named(
            package,
            "1",
            format!("sha256:{}", "0".repeat(64)),
        ),
    }
}

async fn rows(client: &Client) -> BTreeSet<String> {
    client
        .query(
            "SELECT permission || ' by ' || required_by FROM app_system.permissions \
             WHERE tenant_id = 't1' AND role_name = 'clerk'",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// Connect to a disposable database with the application schema installed.
async fn install() -> (LockedDatabase, Client) {
    let url = locked_database::database(wamn_test_postgres::database);
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(&format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
               CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOINHERIT NOREPLICATION NOBYPASSRLS; \
             END IF; END $$; \
             {RECORD_HISTORY}\n{RECORD_HISTORY_APP_GRANTS}\n{APP_SCHEMA}"
        ))
        .await
        .expect("install the application schema");
    (url, client)
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[tokio::test]
async fn migration_0011_removes_the_closure_rows_and_keeps_the_roots() {
    let (_database, client) = install().await;
    client
        .batch_execute(&format!(
            "BEGIN; SELECT set_config('app.user_id', '{PROVISIONING}', true), \
               set_config('app.operation', 'admin:release-permission-fixture', true); \
             INSERT INTO app_system.roles (tenant_id, name) VALUES ('t1', 'clerk'); \
             INSERT INTO app_system.permissions (tenant_id, role_name, permission, required_by) \
             VALUES ('t1', 'clerk', 'shop:order/create', 'shop:order/create'), \
                    ('t1', 'clerk', 'shop:order/get', 'shop:order/create'), \
                    ('t1', 'clerk', 'shop:line/list', 'shop:line/list'); \
             COMMIT;"
        ))
        .await
        .expect("seed a root with its closure row");
    client
        .batch_execute(&format!("BEGIN; {PERMISSION_ROOTS} COMMIT;"))
        .await
        .expect("apply migration 0011");
    assert_eq!(
        rows(&client).await,
        set(&[
            "shop:order/create by shop:order/create",
            "shop:line/list by shop:line/list",
        ]),
        "only the roots remain"
    );
}

/// Run one change in its own transaction and return its result.
macro_rules! change {
    ($client:expr, |$tx:ident| $body:expr) => {{
        let $tx = $client.transaction().await.unwrap();
        let result = $body.await;
        if result.is_ok() {
            $tx.commit().await.unwrap();
        }
        result
    }};
}

#[tokio::test]
async fn permission_grants_and_revokes_store_roots_only() {
    let (_database, mut client) = install().await;
    // report/run requires line/list, and order/create requires line/list too.
    let current = ReleaseClosures::from_components(&[component(
        "shop",
        &[
            ("shop:report/run@1.0.0", &["shop:line/list@1.0.0"]),
            ("shop:order/create@1.0.0", &["shop:line/list@1.0.0"]),
            ("shop:line/list@1.0.0", &[]),
        ],
    )]);

    let refused = change!(client, |tx| create_role(&tx, "t1", "admin")).unwrap_err();
    assert!(format!("{refused:#}").contains("built-in"), "{refused:#}");
    let refused = change!(client, |tx| create_role(&tx, "t1", "Clerk")).unwrap_err();
    assert!(
        format!("{refused:#}").contains("not a role name"),
        "{refused:#}"
    );
    assert!(change!(client, |tx| create_role(&tx, "t1", "clerk")).unwrap());
    assert!(!change!(client, |tx| create_role(&tx, "t1", "clerk")).unwrap());

    let refused = change!(client, |tx| grant_permission(
        &tx,
        "t1",
        "admin",
        "shop:line/list",
        &current
    ))
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("no permission"),
        "{refused:#}"
    );

    // R18 (4): a reference the release does not serve is stored as a root.
    let dormant = change!(client, |tx| grant_permission(
        &tx,
        "t1",
        "clerk",
        "shop:order/delete",
        &current
    ))
    .unwrap();
    assert_eq!(dormant.rows_added, 1);
    assert!(dormant.closure.is_empty());

    let granted = change!(client, |tx| grant_permission(
        &tx,
        "t1",
        "clerk",
        "shop:report/run",
        &current
    ))
    .unwrap();
    assert_eq!(granted.rows_added, 1, "the root alone is stored");
    assert_eq!(
        granted.closure,
        set(&["shop:report/run", "shop:line/list"]),
        "the outcome reports the closure under the given release"
    );
    let again = change!(client, |tx| grant_permission(
        &tx,
        "t1",
        "clerk",
        "shop:report/run",
        &current
    ))
    .unwrap();
    assert_eq!(again.rows_added, 0, "a second grant writes nothing");
    change!(client, |tx| grant_permission(
        &tx,
        "t1",
        "clerk",
        "shop:line/list",
        &current
    ))
    .unwrap();
    assert_eq!(
        rows(&client).await,
        set(&[
            "shop:order/delete by shop:order/delete",
            "shop:report/run by shop:report/run",
            "shop:line/list by shop:line/list",
        ]),
        "no row holds a closure"
    );

    let revoked = change!(client, |tx| revoke_permission(
        &tx,
        "t1",
        "clerk",
        "shop:line/list"
    ))
    .unwrap();
    assert!(revoked.still_required_by.is_empty());
    assert_eq!(
        rows(&client).await,
        set(&[
            "shop:order/delete by shop:order/delete",
            "shop:report/run by shop:report/run",
        ])
    );
    let refused = change!(client, |tx| revoke_permission(
        &tx,
        "t1",
        "clerk",
        "shop:line/list"
    ))
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("does not hold"),
        "{refused:#}"
    );

    let refused = change!(client, |tx| delete_role(&tx, "t1", "admin")).unwrap_err();
    assert!(
        format!("{refused:#}").contains("cannot be deleted"),
        "{refused:#}"
    );
    assert!(change!(client, |tx| delete_role(&tx, "t1", "clerk")).unwrap());
    assert!(
        rows(&client).await.is_empty(),
        "the role took its rows with it"
    );
    assert!(!change!(client, |tx| delete_role(&tx, "t1", "clerk")).unwrap());
}
