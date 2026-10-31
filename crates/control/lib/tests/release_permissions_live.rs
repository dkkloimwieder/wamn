//! Live test of authored role permissions as rows
//! (docs/architecture/execution.md#host-run-routes).
//!
//! A grant stores one row, served by a release or not, and a revoke removes
//! it. The test holds the process lock of its server, because the schema
//! installer creates cluster-wide roles.

use std::collections::BTreeSet;
use wamn_platform_identity::auth_changes::AuthDrops;

use tokio_postgres::{Client, NoTls};
use wamn_control::role_permission::{
    create_role, delete_role, grant_permission, revoke_permission,
};
use wamn_test_infrastructure::locked_database::{self, LockedDatabase};

async fn rows(client: &Client) -> BTreeSet<String> {
    client
        .query(
            "SELECT permission FROM platform.permission \
             WHERE role_name = 'clerk'",
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
    let url = locked_database::database(wamn_project_state::test_database::tenant_platform);
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    (url, client)
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
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
async fn permission_grants_and_revokes_store_rows() {
    let (_database, mut client) = install().await;

    let refused = change!(client, |tx| create_role(
        &tx,
        "t1",
        "admin",
        &mut AuthDrops::default()
    ))
    .unwrap_err();
    assert!(format!("{refused:#}").contains("built-in"), "{refused:#}");
    let refused = change!(client, |tx| create_role(
        &tx,
        "t1",
        "Clerk",
        &mut AuthDrops::default()
    ))
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("not a role name"),
        "{refused:#}"
    );
    assert!(
        change!(client, |tx| create_role(
            &tx,
            "t1",
            "clerk",
            &mut AuthDrops::default()
        ))
        .unwrap()
    );
    assert!(
        !change!(client, |tx| create_role(
            &tx,
            "t1",
            "clerk",
            &mut AuthDrops::default()
        ))
        .unwrap()
    );

    let refused = change!(client, |tx| grant_permission(
        &tx,
        "t1",
        "admin",
        "shop:line/list",
        &mut AuthDrops::default()
    ))
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("no permission"),
        "{refused:#}"
    );

    // A reference the release does not serve is stored.
    let dormant = change!(client, |tx| grant_permission(
        &tx,
        "t1",
        "clerk",
        "shop:order/delete",
        &mut AuthDrops::default()
    ))
    .unwrap();
    assert_eq!(dormant.rows_added, 1);

    let granted = change!(client, |tx| grant_permission(
        &tx,
        "t1",
        "clerk",
        "shop:report/run",
        &mut AuthDrops::default()
    ))
    .unwrap();
    assert_eq!(granted.rows_added, 1, "the grant alone is stored");
    let again = change!(client, |tx| grant_permission(
        &tx,
        "t1",
        "clerk",
        "shop:report/run",
        &mut AuthDrops::default()
    ))
    .unwrap();
    assert_eq!(again.rows_added, 0, "a second grant writes nothing");
    change!(client, |tx| grant_permission(
        &tx,
        "t1",
        "clerk",
        "shop:line/list",
        &mut AuthDrops::default()
    ))
    .unwrap();
    assert_eq!(
        rows(&client).await,
        set(&["shop:order/delete", "shop:report/run", "shop:line/list"]),
        "each grant is one row"
    );

    change!(client, |tx| revoke_permission(
        &tx,
        "t1",
        "clerk",
        "shop:line/list",
        &mut AuthDrops::default()
    ))
    .unwrap();
    assert_eq!(
        rows(&client).await,
        set(&["shop:order/delete", "shop:report/run"])
    );
    let refused = change!(client, |tx| revoke_permission(
        &tx,
        "t1",
        "clerk",
        "shop:line/list",
        &mut AuthDrops::default()
    ))
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("does not hold"),
        "{refused:#}"
    );

    let refused = change!(client, |tx| delete_role(
        &tx,
        "t1",
        "admin",
        &mut AuthDrops::default()
    ))
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("cannot be deleted"),
        "{refused:#}"
    );
    assert!(
        change!(client, |tx| delete_role(
            &tx,
            "t1",
            "clerk",
            &mut AuthDrops::default()
        ))
        .unwrap()
    );
    assert!(
        rows(&client).await.is_empty(),
        "the role took its rows with it"
    );
    assert!(
        !change!(client, |tx| delete_role(
            &tx,
            "t1",
            "clerk",
            &mut AuthDrops::default()
        ))
        .unwrap()
    );
}
