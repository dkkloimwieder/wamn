//! Live test of the org admin that `provision-org --admin` writes
//! (docs/architecture/overview.md#organization-membership-and-administration,
//! wamn-a40n.3, wamn-sjdxt). The test holds the process lock of its server,
//! because the installer creates cluster-wide roles.

use sqlx::PgConnection as Client;
use sqlx::Row as _;

use wamn_control_provision::Template;
use wamn_environment_control::provision_org::{OrgRecord, ProvisionOrgRequest, provision_org};
use wamn_environment_control::provision_project_env::provisioning_transaction;
use wamn_environment_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_test_infrastructure::locked_database;

const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";
const FIRST_EMAIL: &str = "first@example.test";
const SECOND_EMAIL: &str = "second@example.test";

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to the disposable database");

    client
}

fn request(org: &str, url: &str, admin: &str) -> ProvisionOrgRequest {
    ProvisionOrgRequest {
        org: org.to_owned(),
        template: Template::trials(),
        pool: "wamn-pg".to_owned(),
        record: Some(OrgRecord {
            system_database_url: url.to_owned(),
            admin: admin.to_owned(),
        }),
        cluster_namespace: "wamn-system".to_owned(),
    }
}

async fn column(client: &mut Client, statement: &str) -> Vec<String> {
    sqlx::query(sqlx::AssertSqlSafe((statement).to_string()))
        .fetch_all(&mut *client)
        .await
        .expect(statement)
        .iter()
        .map(|row| row.get(0))
        .collect()
}

async fn users(client: &mut Client) -> Vec<String> {
    column(
        client,
        "SELECT id::text || ' ' || email FROM identity.principals \
           WHERE type = 'user' ORDER BY email",
    )
    .await
}

async fn admin_rows(client: &mut Client, org: &str) -> (Vec<String>, Vec<String>) {
    (
        column(
            client,
            &format!(
                "SELECT principal_id::text || ' ' || status || ' ' || created_by::text \
                   FROM identity.org_memberships WHERE org = '{org}'"
            ),
        )
        .await,
        column(
            client,
            &format!(
                "SELECT principal_id::text || ' ' || role || ' ' || created_by::text \
                   FROM identity.org_roles WHERE org = '{org}'"
            ),
        )
        .await,
    )
}

fn expected(principal: &str) -> (Vec<String>, Vec<String>) {
    (
        vec![format!("{principal} active {PROVISIONING}")],
        vec![format!("{principal} org-admin {PROVISIONING}")],
    )
}

#[tokio::test]
async fn provision_org_creates_the_first_admin_then_names_existing_users_only() {
    let database = locked_database::database(wamn_test_postgres::database);
    let url = database.to_string();
    let mut client = connect(&url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("create the wamn_system role");
    provision_system(&ProvisionSystemRequest {
        migration_database_url: Some(
            wamn_control_provision::test_database::platform_migration_input(
                &url,
                wamn_control_provision::migrations::MigrationDatabase::System,
            )
            .await
            .unwrap(),
        ),
        system_database_url: url.clone(),
        platform_domain: "wamn.example.test".to_owned(),
    })
    .await
    .expect("install the control store");
    assert!(
        users(&mut client).await.is_empty(),
        "a fresh store has no user"
    );

    // 1. A fresh store: the run creates the first user and makes it the admin.
    provision_org(request("acme", &url, FIRST_EMAIL))
        .await
        .expect("provision the first org with the first admin");
    for (relation, predicate) in [
        ("registry.orgs", "id = 'acme'"),
        ("registry.env_policies", "org = 'acme'"),
    ] {
        wamn_test_infrastructure::record_history::assert_stamp(
            &mut client,
            relation,
            predicate,
            PROVISIONING,
            PROVISIONING,
        )
        .await;
    }
    let created = users(&mut client).await;
    assert_eq!(created.len(), 1, "{created:?}");
    let first = created[0]
        .strip_suffix(&format!(" {FIRST_EMAIL}"))
        .expect("the first user has the admin email")
        .to_owned();
    assert_eq!(
        column(&mut client, "SELECT id FROM registry.orgs").await,
        ["acme"]
    );
    assert_eq!(admin_rows(&mut client, "acme").await, expected(&first));

    // 2. Users exist: `--admin` names an existing user, and no user is created.
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .expect("assume the control owner");
    let second = {
        let mut transaction = provisioning_transaction(&mut client)
            .await
            .expect("bind wamn:provisioning");
        let second =
            wamn_platform_identity::create_user(&mut transaction, "second", SECOND_EMAIL, "Second")
                .await
                .expect("create the second user");
        transaction.commit().await.expect("commit the second user");
        second
    };
    sqlx::raw_sql("RESET ROLE")
        .execute(&mut client)
        .await
        .map(drop)
        .expect("reset role");
    let before = users(&mut client).await;
    provision_org(request("beta", &url, SECOND_EMAIL))
        .await
        .expect("provision an org with an existing admin");
    assert_eq!(users(&mut client).await, before, "no user is created");
    assert_eq!(
        admin_rows(&mut client, "beta").await,
        expected(second.id().to_string().as_str())
    );

    // 3. Users exist: an unknown email is refused, and nothing is written.
    let error = provision_org(request("strangers", &url, "nobody@example.test"))
        .await
        .expect_err("an unknown admin email is refused");
    assert!(
        format!("{error:#}").contains("names no user principal"),
        "{error:#}"
    );
    assert_eq!(
        users(&mut client).await,
        before,
        "a refusal creates no user"
    );
    assert_eq!(
        column(&mut client, "SELECT id FROM registry.orgs ORDER BY id").await,
        ["acme", "beta"],
        "a refusal leaves no org row"
    );
    assert_eq!(
        admin_rows(&mut client, "strangers").await,
        (Vec::new(), Vec::new())
    );
}
