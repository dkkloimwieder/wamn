//! Live test of authored role permissions as rows
//! (docs/architecture/execution.md#host-run-routes).
//!
//! A grant stores one row, served by a release or not, and a revoke removes
//! it. The test holds the process lock of its server, because the schema
//! installer creates cluster-wide roles.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use std::collections::BTreeSet;

use wamn_environment::application::{create_role, delete_role};

async fn rows(client: &mut Client) -> BTreeSet<String> {
    sqlx::query(
        "SELECT permission FROM platform.permission \
             WHERE role_name = 'clerk'",
    )
    .fetch_all(&mut *client)
    .await
    .unwrap()
    .iter()
    .map(|row| row.get(0))
    .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

/// Run one change in its own transaction and return its result.
macro_rules! change {
    ($client:expr, |$tx:ident| $body:expr) => {{
        let mut $tx = $client.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_provision::bind_platform_principal_sql(
                wamn_provision::PlatformComponent::Provisioning,
            ))
                .to_string(),
        ))
        .execute(&mut *$tx)
        .await
        .map(drop)
        .unwrap();
        let result = $body.await;
        if result.is_ok() {
            $tx.commit().await.unwrap();
        }
        result
    }};
}

#[sqlx::test(migrations = false)]
async fn permission_grants_and_revokes_store_rows(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_identity::test_database::project_platform(database.as_str()).await;
    let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();
    let project_env_key = "t1--dev--project".parse().unwrap();

    change!(client, |tx| create_role(&mut tx, "admin")).unwrap_err();
    change!(client, |tx| create_role(&mut tx, "Clerk")).unwrap_err();
    assert!(change!(client, |tx| create_role(&mut tx, "clerk")).unwrap());
    assert!(!change!(client, |tx| create_role(&mut tx, "clerk")).unwrap());

    change!(client, |tx| {
        wamn_environment::application::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_environment::application::GrantAuthority::Provisioning,
            wamn_environment::application::GrantChange::Permission {
                role: "admin",
                reference: "shop:line/list",
                grant: true,
            },
        )
    })
    .unwrap_err();

    // A reference the release does not serve is stored.
    let dormant = change!(client, |tx| {
        wamn_environment::application::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_environment::application::GrantAuthority::Provisioning,
            wamn_environment::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:order/delete",
                grant: true,
            },
        )
    })
    .unwrap();
    assert_eq!(dormant.rows_added, 1);

    let granted = change!(client, |tx| {
        wamn_environment::application::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_environment::application::GrantAuthority::Provisioning,
            wamn_environment::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:report/run",
                grant: true,
            },
        )
    })
    .unwrap();
    assert_eq!(granted.rows_added, 1, "the grant alone is stored");
    let again = change!(client, |tx| {
        wamn_environment::application::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_environment::application::GrantAuthority::Provisioning,
            wamn_environment::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:report/run",
                grant: true,
            },
        )
    })
    .unwrap();
    assert_eq!(again.rows_added, 0, "a second grant writes nothing");
    change!(client, |tx| {
        wamn_environment::application::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_environment::application::GrantAuthority::Provisioning,
            wamn_environment::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:line/list",
                grant: true,
            },
        )
    })
    .unwrap();
    assert_eq!(
        rows(&mut client).await,
        set(&["shop:order/delete", "shop:report/run", "shop:line/list"]),
        "each grant is one row"
    );

    change!(client, |tx| {
        wamn_environment::application::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_environment::application::GrantAuthority::Provisioning,
            wamn_environment::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:line/list",
                grant: false,
            },
        )
    })
    .unwrap();
    assert_eq!(
        rows(&mut client).await,
        set(&["shop:order/delete", "shop:report/run"])
    );
    change!(client, |tx| {
        wamn_environment::application::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_environment::application::GrantAuthority::Provisioning,
            wamn_environment::application::GrantChange::Permission {
                role: "clerk",
                reference: "shop:line/list",
                grant: false,
            },
        )
    })
    .unwrap_err();

    change!(client, |tx| delete_role(&mut tx, "admin")).unwrap_err();
    assert!(change!(client, |tx| delete_role(&mut tx, "clerk")).unwrap());
    assert!(
        rows(&mut client).await.is_empty(),
        "the role took its rows with it"
    );
    assert!(!change!(client, |tx| delete_role(&mut tx, "clerk")).unwrap());
}
