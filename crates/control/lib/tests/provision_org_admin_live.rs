//! Live test of the org admin that `provision-org --admin` writes
//! (docs/architecture/overview.md#organization-membership-and-administration,
//! wamn-a40n.3, wamn-sjdxt). The test holds the process lock of its server,
//! because the installer creates cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_org::{OrgRecord, ProvisionOrgRequest, provision_org};
use wamn_control::provision_project_env::provisioning_transaction;
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_control_registry::Template;
use wamn_test_infrastructure::locked_database;

const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";
const FIRST_EMAIL: &str = "first@example.test";
const SECOND_EMAIL: &str = "second@example.test";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
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

async fn column(client: &Client, statement: &str) -> Vec<String> {
    client
        .query(statement, &[])
        .await
        .expect(statement)
        .iter()
        .map(|row| row.get(0))
        .collect()
}

async fn users(client: &Client) -> Vec<String> {
    column(
        client,
        "SELECT id::text || ' ' || email FROM identity.principals \
           WHERE type = 'user' ORDER BY email",
    )
    .await
}

async fn admin_rows(client: &Client, org: &str) -> (Vec<String>, Vec<String>) {
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
        system_database_url: url.clone(),
        platform_domain: "wamn.example.test".to_owned(),
    })
    .await
    .expect("install the control store");
    assert!(users(&client).await.is_empty(), "a fresh store has no user");

    // 1. A fresh store: the run creates the first user and makes it the admin.
    provision_org(request("acme", &url, FIRST_EMAIL))
        .await
        .expect("provision the first org with the first admin");
    for (relation, predicate) in [
        ("registry.orgs", "id = 'acme'"),
        ("registry.env_policies", "org = 'acme'"),
    ] {
        wamn_test_infrastructure::record_history::assert_stamp(
            &client,
            relation,
            predicate,
            PROVISIONING,
            PROVISIONING,
        )
        .await;
    }
    let created = users(&client).await;
    assert_eq!(created.len(), 1, "{created:?}");
    let first = created[0]
        .strip_suffix(&format!(" {FIRST_EMAIL}"))
        .expect("the first user has the admin email")
        .to_owned();
    assert_eq!(
        column(&client, "SELECT id FROM registry.orgs").await,
        ["acme"]
    );
    assert_eq!(admin_rows(&client, "acme").await, expected(&first));

    // 2. Users exist: `--admin` names an existing user, and no user is created.
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .expect("assume the control owner");
    let second = {
        let transaction = provisioning_transaction(&mut client)
            .await
            .expect("bind wamn:provisioning");
        let second =
            wamn_platform_identity::create_user(&transaction, "second", SECOND_EMAIL, "Second")
                .await
                .expect("create the second user");
        transaction.commit().await.expect("commit the second user");
        second
    };
    client
        .batch_execute("RESET ROLE")
        .await
        .expect("reset role");
    let before = users(&client).await;
    provision_org(request("beta", &url, SECOND_EMAIL))
        .await
        .expect("provision an org with an existing admin");
    assert_eq!(users(&client).await, before, "no user is created");
    assert_eq!(
        admin_rows(&client, "beta").await,
        expected(second.id().as_str())
    );

    // 3. Users exist: an unknown email is refused, and nothing is written.
    let error = provision_org(request("strangers", &url, "nobody@example.test"))
        .await
        .expect_err("an unknown admin email is refused");
    assert!(
        format!("{error:#}").contains("names no user principal"),
        "{error:#}"
    );
    assert_eq!(users(&client).await, before, "a refusal creates no user");
    assert_eq!(
        column(&client, "SELECT id FROM registry.orgs ORDER BY id").await,
        ["acme", "beta"],
        "a refusal leaves no org row"
    );
    assert_eq!(
        admin_rows(&client, "strangers").await,
        (Vec::new(), Vec::new())
    );
}
