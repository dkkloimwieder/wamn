//! Live test of authored role permissions as rows
//! (docs/architecture/execution.md#host-run-routes).
//!
//! A grant stores one row, served by a release or not, and a revoke removes
//! it.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use std::collections::BTreeSet;
use std::sync::Arc;

use wamn_authentication::administration::{
    AdministrationError, AdministrationRefusal, GrantAuthority, GrantChange, activate_role,
    create_role, deactivate_role, mutate_grant,
};
use wamn_authentication::authorization::Authorization;
use wamn_identity_protocol::AuthorityClass;
use wamn_postgres::{ClassCredentials, WamnPostgres};

use wamn_test_infrastructure::identity as common;

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
            (&wamn_postgres::bind_platform_principal_sql(
                wamn_principal::PlatformComponent::Provisioning,
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

#[tokio::test]
#[ignore = "requires: docker"]
async fn permission_grants_and_revokes_store_rows() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = common::project(&postgres, "release_permissions").await;
    let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();
    let project_env_key = "t1--dev--project".parse().unwrap();

    change!(client, |tx| create_role(&mut tx, "admin")).unwrap_err();
    change!(client, |tx| create_role(&mut tx, "Clerk")).unwrap_err();
    assert!(change!(client, |tx| create_role(&mut tx, "clerk")).unwrap());
    assert!(!change!(client, |tx| create_role(&mut tx, "clerk")).unwrap());

    change!(client, |tx| {
        wamn_authentication::administration::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_authentication::administration::GrantAuthority::Provisioning,
            wamn_authentication::administration::GrantChange::Permission {
                role: "admin",
                reference: "shop:line/list",
                grant: true,
            },
        )
    })
    .unwrap_err();

    // A reference the release does not serve is stored.
    let dormant = change!(client, |tx| {
        wamn_authentication::administration::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_authentication::administration::GrantAuthority::Provisioning,
            wamn_authentication::administration::GrantChange::Permission {
                role: "clerk",
                reference: "shop:order/delete",
                grant: true,
            },
        )
    })
    .unwrap();
    assert_eq!(dormant.rows_added, 1);

    let granted = change!(client, |tx| {
        wamn_authentication::administration::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_authentication::administration::GrantAuthority::Provisioning,
            wamn_authentication::administration::GrantChange::Permission {
                role: "clerk",
                reference: "shop:report/run",
                grant: true,
            },
        )
    })
    .unwrap();
    assert_eq!(granted.rows_added, 1, "the grant alone is stored");
    let again = change!(client, |tx| {
        wamn_authentication::administration::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_authentication::administration::GrantAuthority::Provisioning,
            wamn_authentication::administration::GrantChange::Permission {
                role: "clerk",
                reference: "shop:report/run",
                grant: true,
            },
        )
    })
    .unwrap();
    assert_eq!(again.rows_added, 0, "a second grant writes nothing");
    change!(client, |tx| {
        wamn_authentication::administration::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_authentication::administration::GrantAuthority::Provisioning,
            wamn_authentication::administration::GrantChange::Permission {
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
        wamn_authentication::administration::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_authentication::administration::GrantAuthority::Provisioning,
            wamn_authentication::administration::GrantChange::Permission {
                role: "clerk",
                reference: "shop:line/list",
                grant: false,
            },
        )
    })
    .unwrap();
    let remaining = rows(&mut client).await;
    assert_eq!(remaining, set(&["shop:order/delete", "shop:report/run"]));
    change!(client, |tx| {
        wamn_authentication::administration::mutate_grant(
            &mut tx,
            &project_env_key,
            wamn_authentication::administration::GrantAuthority::Provisioning,
            wamn_authentication::administration::GrantChange::Permission {
                role: "clerk",
                reference: "shop:line/list",
                grant: false,
            },
        )
    })
    .unwrap_err();

    assert!(matches!(
        change!(client, |tx| deactivate_role(&mut tx, "admin")),
        Err(AdministrationError::Refused(
            AdministrationRefusal::AdminStatusFixed
        ))
    ));
    assert!(change!(client, |tx| deactivate_role(&mut tx, "clerk")).unwrap());
    assert_eq!(
        rows(&mut client).await,
        remaining,
        "deactivation keeps the stored permissions"
    );
    assert!(!change!(client, |tx| deactivate_role(&mut tx, "clerk")).unwrap());
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn role_deactivation_and_activation_apply_on_the_next_grant_read() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = common::project(&postgres, "role_activity").await;
    let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();
    sqlx::raw_sql(
        "CREATE ROLE wamn_platform_login LOGIN INHERIT PASSWORD 'test';
         GRANT wamn_platform TO wamn_platform_login;",
    )
    .execute(&mut client)
    .await
    .unwrap();
    let platform_url = postgres
        .database_url("role_activity", "wamn_platform_login", "test")
        .unwrap();
    let credentials =
        ClassCredentials::default().with_class(AuthorityClass::Platform, platform_url);
    let authorization =
        Authorization::new(Arc::new(WamnPostgres::from_env(Some(credentials)).unwrap()));
    let app = wamn_app::app_config::AppConfig {
        org: "t1".parse().unwrap(),
        project: wamn_registry::DEFAULT_PROJECT.to_owned(),
        env: "dev".parse().unwrap(),
        app: "shop".parse().unwrap(),
        grant_cache_seconds: 3600,
        components: Vec::new(),
    };
    authorization.configure_app(&app).unwrap();
    let key = wamn_registry::project_env_key(&app.org, &app.project, &app.env);
    let user = change!(client, |tx| sqlx::query_scalar::<_, uuid::Uuid>(
        r#"INSERT INTO platform."user" (id, issuer, sub, type, email)
           VALUES (uuidv7(), 'https://role-test.invalid', 'clerk', 'user', 'clerk@example.invalid')
           RETURNING id"#,
    )
    .fetch_one(&mut *tx))
    .unwrap()
    .into();
    let role = "clerk";
    let reference = "shop:line/list";
    change!(client, |tx| create_role(&mut tx, role)).unwrap();
    change!(client, |tx| mutate_grant(
        &mut tx,
        &key,
        GrantAuthority::Provisioning,
        GrantChange::Permission {
            role,
            reference,
            grant: true
        },
    ))
    .unwrap();
    change!(client, |tx| mutate_grant(
        &mut tx,
        &key,
        GrantAuthority::Provisioning,
        GrantChange::UserRole {
            user_id: &user,
            role,
            grant: true
        },
    ))
    .unwrap();

    let read_grants = || authorization.user_operation_permissions(&app.project, &key, &user);
    let before = read_grants().await.unwrap();
    assert!(before.references.contains(reference));
    assert_eq!(before.active_role().as_deref(), Some(role));

    change!(client, |tx| deactivate_role(&mut tx, role)).unwrap();
    let inactive = read_grants().await.unwrap();
    assert!(inactive.user, "the principal remains an active user");
    assert!(inactive.roles.is_empty());
    assert!(inactive.references.is_empty());

    assert!(change!(client, |tx| activate_role(&mut tx, role)).unwrap());
    assert_eq!(read_grants().await.unwrap(), before);
}
