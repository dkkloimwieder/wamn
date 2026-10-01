//! Live test of the org owner that `provision-org` writes (wamn-a40n.3,
//! docs/plan/platform-ui.md §4.4). The test holds the process lock of its
//! server, because the installer creates cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_org::{ProvisionOrgRequest, provision_org};
use wamn_control::provision_project_env::provisioning_transaction;
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_control_registry::Template;
use wamn_test_infrastructure::locked_database;

const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";
const OWNER_EMAIL: &str = "owner@example.test";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

fn request(org: &str, url: Option<&str>, owner: Option<&str>) -> ProvisionOrgRequest {
    ProvisionOrgRequest {
        org: org.to_owned(),
        template: Template::trials(),
        pool: "wamn-pg".to_owned(),
        system_database_url: url.map(str::to_owned),
        cluster_namespace: "wamn-system".to_owned(),
        owner_email: owner.map(str::to_owned),
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

#[tokio::test]
async fn provision_org_writes_the_owner_of_a_known_email_and_refuses_an_unknown_one() {
    let error = provision_org(request("planonly", None, Some(OWNER_EMAIL)))
        .await
        .expect_err("an owner without a system database is refused");
    assert_eq!(
        error.to_string(),
        "--owner-email needs --system-database-url"
    );

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
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .expect("assume the control owner");
    let owner = {
        let transaction = provisioning_transaction(&mut client)
            .await
            .expect("bind wamn:provisioning");
        let owner =
            wamn_platform_identity::create_user(&transaction, "owner", OWNER_EMAIL, "Org Owner")
                .await
                .expect("create the owner");
        transaction.commit().await.expect("commit the owner");
        owner
    };
    client
        .batch_execute("RESET ROLE")
        .await
        .expect("reset role");

    let error = provision_org(request(
        "strangers",
        Some(&url),
        Some("nobody@example.test"),
    ))
    .await
    .expect_err("an unknown owner email is refused");
    assert!(
        format!("{error:#}").contains("names no user principal"),
        "{error:#}"
    );
    assert!(
        column(
            &client,
            "SELECT id FROM registry.orgs WHERE id = 'strangers'"
        )
        .await
        .is_empty(),
        "a refused owner leaves no org row"
    );

    for _ in 0..2 {
        provision_org(request("acme", Some(&url), Some(OWNER_EMAIL)))
            .await
            .expect("provision the org with its owner");
    }
    assert_eq!(
        column(
            &client,
            "SELECT principal_id::text || ' ' || status || ' ' || created_by::text \
               FROM identity.org_memberships WHERE org = 'acme'"
        )
        .await,
        [format!("{} active {PROVISIONING}", owner.id().as_str())]
    );
    assert_eq!(
        column(
            &client,
            "SELECT principal_id::text || ' ' || role || ' ' || created_by::text \
               FROM identity.org_roles WHERE org = 'acme'"
        )
        .await,
        [format!("{} org-admin {PROVISIONING}", owner.id().as_str())]
    );
}
