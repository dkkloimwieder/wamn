//! Live test of authored role permissions as rows
//! (docs/architecture/execution.md#host-run-routes).
//!
//! A grant stores one row, served by a release or not, and a revoke removes
//! it. The test holds the process lock of its server, because the schema
//! installer creates cluster-wide roles.

use std::collections::BTreeSet;
use wamn_platform_identity::auth_changes::AuthDrops;

use tokio_postgres::{Client, NoTls};
use wamn_platform_identity::application::{create_role, delete_role};
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
        $tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
        .unwrap();
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
    let mut drops = AuthDrops::default();

    let refused = change!(client, |tx| create_role(&tx, "t1", "admin", &mut drops)).unwrap_err();
    assert!(format!("{refused:#}").contains("built-in"), "{refused:#}");
    let refused = change!(client, |tx| create_role(&tx, "t1", "Clerk", &mut drops)).unwrap_err();
    assert!(
        format!("{refused:#}").contains("not a role name"),
        "{refused:#}"
    );
    assert!(change!(client, |tx| create_role(&tx, "t1", "clerk", &mut drops)).unwrap());
    assert!(!change!(client, |tx| create_role(&tx, "t1", "clerk", &mut drops)).unwrap());

    let refused = change!(client, |tx| {
        wamn_platform_identity::application::mutate_grant(
            &tx,
            "t1",
            wamn_platform_identity::application::GrantAuthority::Provisioning,
            wamn_platform_identity::application::GrantChange::Permission {
                role: "admin",
                reference: "shop:line/list",
                grant: true,
            },
            &mut drops,
        )
    })
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("no permission"),
        "{refused:#}"
    );

    // A reference the release does not serve is stored.
    let dormant = change!(client, |tx| {
        wamn_platform_identity::application::mutate_grant(
            &tx,
            "t1",
            wamn_platform_identity::application::GrantAuthority::Provisioning,
            wamn_platform_identity::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:order/delete",
                grant: true,
            },
            &mut drops,
        )
    })
    .unwrap();
    assert_eq!(dormant.rows_added, 1);

    let granted = change!(client, |tx| {
        wamn_platform_identity::application::mutate_grant(
            &tx,
            "t1",
            wamn_platform_identity::application::GrantAuthority::Provisioning,
            wamn_platform_identity::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:report/run",
                grant: true,
            },
            &mut drops,
        )
    })
    .unwrap();
    assert_eq!(granted.rows_added, 1, "the grant alone is stored");
    let again = change!(client, |tx| {
        wamn_platform_identity::application::mutate_grant(
            &tx,
            "t1",
            wamn_platform_identity::application::GrantAuthority::Provisioning,
            wamn_platform_identity::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:report/run",
                grant: true,
            },
            &mut drops,
        )
    })
    .unwrap();
    assert_eq!(again.rows_added, 0, "a second grant writes nothing");
    change!(client, |tx| {
        wamn_platform_identity::application::mutate_grant(
            &tx,
            "t1",
            wamn_platform_identity::application::GrantAuthority::Provisioning,
            wamn_platform_identity::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:line/list",
                grant: true,
            },
            &mut drops,
        )
    })
    .unwrap();
    assert_eq!(
        rows(&client).await,
        set(&["shop:order/delete", "shop:report/run", "shop:line/list"]),
        "each grant is one row"
    );

    change!(client, |tx| {
        wamn_platform_identity::application::mutate_grant(
            &tx,
            "t1",
            wamn_platform_identity::application::GrantAuthority::Provisioning,
            wamn_platform_identity::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:line/list",
                grant: false,
            },
            &mut drops,
        )
    })
    .unwrap();
    assert_eq!(
        rows(&client).await,
        set(&["shop:order/delete", "shop:report/run"])
    );
    let refused = change!(client, |tx| {
        wamn_platform_identity::application::mutate_grant(
            &tx,
            "t1",
            wamn_platform_identity::application::GrantAuthority::Provisioning,
            wamn_platform_identity::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:line/list",
                grant: false,
            },
            &mut drops,
        )
    })
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("does not hold"),
        "{refused:#}"
    );

    let refused = change!(client, |tx| delete_role(&tx, "t1", "admin", &mut drops)).unwrap_err();
    assert!(
        format!("{refused:#}").contains("cannot be deleted"),
        "{refused:#}"
    );
    assert!(change!(client, |tx| delete_role(&tx, "t1", "clerk", &mut drops)).unwrap());
    assert!(
        rows(&client).await.is_empty(),
        "the role took its rows with it"
    );
    assert!(!change!(client, |tx| delete_role(&tx, "t1", "clerk", &mut drops)).unwrap());
}
