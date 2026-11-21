//! Least privilege through the authenticated login and catalog rules.

pub mod common;
use wamn_provision::WorkloadRoleFamily;

#[tokio::test]
#[ignore = "requires: docker"]
async fn query_refusals_and_catalog_rules() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let url = postgres.create_database("query_grants").await.unwrap();
    common::grants::family(url, WorkloadRoleFamily::Query).await;
}
