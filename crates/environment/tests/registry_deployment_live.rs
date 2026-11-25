use sqlx::Connection as _;
use wamn_environment::registry::deployment;
use wamn_platform_install::provision_system::install_control_store;
use wamn_postgres::provisioning_transaction::provisioning_transaction;
use wamn_registry::{Triple, deployment::Registration};

/// The native connection summaries decode the complete live query rows.
#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn native_connection_summary_decodes_query_rows() -> anyhow::Result<()> {
    use serde_json::{Value, json};
    use wamn_connection::{administration::configure, project_connections::Administration};
    use wamn_routes::host_wire::ConnectionSummary;
    use wamn_test_infrastructure::database;

    let project = database::project().await?;
    let system = database::system().await?;
    let mut project = project.admin().await?;
    let mut system = system.admin().await?;
    let mut transaction = provisioning_transaction(&mut project).await?;
    sqlx::raw_sql(
        "INSERT INTO platform.connection
             (id, name, integration_type, auth_type, target_id, active_generation, service_principal_id)
         VALUES ('00000000-0000-0000-0000-000000000001', 'plain', 'http', 'none', gen_random_uuid(), 1, gen_random_uuid()),
                ('00000000-0000-0000-0000-000000000002', 'authenticated', 'http', 'api_key', gen_random_uuid(), 1, gen_random_uuid());
         INSERT INTO platform.credential (id, connection_id, store, ref, version, hint)
         VALUES ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000002', 'k8s', 'fixture-only', '1', 'hint');
         INSERT INTO platform.connection_generation (connection_id, generation, parameters, credential_id)
         VALUES ('00000000-0000-0000-0000-000000000001', 1, '{}', NULL),
                ('00000000-0000-0000-0000-000000000002', 1, '{}', '00000000-0000-0000-0000-000000000003');",
    )
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    let mut transaction = project.begin().await?;
    let mut administration = Administration::new(&mut transaction, &mut system, None, None);
    let org = "acme".parse()?;
    let listed = configure(
        &mut administration,
        &org,
        "connection.list",
        &json!({"project":"billing", "env":"dev"}),
    )
    .await?;
    let rows = listed["connections"].as_array().expect("connection rows");
    assert!(rows.iter().any(|row| row["name"] == "plain"));
    assert!(rows.iter().any(|row| row["name"] == "authenticated"));
    for row in rows {
        let summary: ConnectionSummary = serde_json::from_value(row.clone())?;
        assert_eq!(serde_json::to_value(&summary)?, *row);
        let found: Value = configure(
            &mut administration,
            &org,
            "connection.get",
            &json!({"project":"billing", "env":"dev", "name":summary.name}),
        )
        .await?;
        let summary: ConnectionSummary = serde_json::from_value(found.clone())?;
        assert_eq!(serde_json::to_value(summary)?, found);
    }
    Ok(())
}

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
