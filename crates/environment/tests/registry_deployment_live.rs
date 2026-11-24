use sqlx::Connection as _;
use wamn_environment::registry::deployment;
use wamn_platform_install::provision_system::install_control_store;
use wamn_postgres::provisioning_transaction::provisioning_transaction;
use wamn_registry::{Triple, deployment::Registration};

/// A deployment update with no marker preserves the stored disposable setting.
#[tokio::test]
#[ignore = "requires: docker"]
async fn a_record_without_a_marker_keeps_the_disposable_row() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    postgres.create_system_role().await.unwrap();
    let database = postgres.create_database("wamn_system").await.unwrap();
    let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();
    install_control_store(&mut client).await.unwrap();

    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let mut transaction = client.begin().await.unwrap();
    sqlx::raw_sql("INSERT INTO platform.tx_identity(principal_id,role,application) VALUES('770df186-ac15-579e-b46b-c297cae2011b','','fixture')")
        .execute(&mut *transaction)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql("INSERT INTO registry.orgs (id) VALUES ('demo')")
        .execute(&mut *transaction)
        .await
        .map(drop)
        .unwrap();
    transaction.commit().await.unwrap();
    let triple = Triple::new(
        "demo".parse().unwrap(),
        "dev".parse().unwrap(),
        "inventory".parse().unwrap(),
    );
    let mut transaction = provisioning_transaction(&mut client).await.unwrap();
    deployment::record_project(&mut transaction, &triple.org, &triple.project)
        .await
        .unwrap();
    let target = deployment::target_id(&mut transaction, "cloud")
        .await
        .unwrap()
        .unwrap();
    let mut registration = Registration {
        triple: &triple,
        secret_name: "db-first",
        secret_namespace: Some("system"),
        instance_suffix: &"abcd1234".parse().unwrap(),
        disposable: Some(true),
        readiness_budget_seconds: 600,
        drain_bound_seconds: 300,
        target_id: &target,
    };
    deployment::record(&mut transaction, &registration)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    registration.disposable = None;
    let mut transaction = provisioning_transaction(&mut client).await.unwrap();
    deployment::record(&mut transaction, &registration)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let disposable = deployment::disposable(&mut client, &triple).await.unwrap();
    assert!(
        disposable,
        "a record without a marker keeps the registry row"
    );
}
