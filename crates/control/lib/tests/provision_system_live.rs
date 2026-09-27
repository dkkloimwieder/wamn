//! Live test of `provision-system` against a disposable PostgreSQL server.
//!
//! The test holds the process lock of its server, because the verb changes
//! cluster-wide roles and the PUBLIC CONNECT floor of every database.

use tokio_postgres::NoTls;
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_test_infrastructure::locked_database;

#[tokio::test]
async fn provision_system_runs_once_and_records_the_platform_domain() {
    let url = locked_database::database(wamn_test_postgres::database);
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("connect to the disposable system database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    // CloudNativePG creates the owner role at bootstrap. The test does it here.
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;",
        )
        .await
        .expect("create the wamn_system role");
    let request = ProvisionSystemRequest {
        system_database_url: url.to_string(),
        platform_domain: "wamn.example.test".to_owned(),
    };

    provision_system(&request)
        .await
        .expect("the first run installs the control store");
    let domain: Option<String> = client
        .query_one("SELECT platform_domain FROM registry.meta", &[])
        .await
        .expect("read the platform domain")
        .get(0);
    assert_eq!(domain.as_deref(), Some("wamn.example.test"));

    let refusal = provision_system(&request)
        .await
        .expect_err("the second run refuses");
    assert!(
        format!("{refusal:#}").contains("already has the schema registry"),
        "{refusal:#}"
    );
}
