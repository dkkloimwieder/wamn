use wamn_env::registry::deployment;
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

    let mut transaction = provisioning_transaction(&mut project).await?;
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
        assert_eq!(summary.status, "active");
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
    let plain = json!({"project":"billing", "env":"dev", "name":"plain"});
    for (operation, expected) in [
        ("connection.disable", "disabled"),
        ("connection.enable", "active"),
    ] {
        configure(&mut administration, &org, operation, &plain).await?;
        let found = configure(&mut administration, &org, "connection.get", &plain).await?;
        assert_eq!(found["status"], expected);
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
    let mut transaction = provisioning_transaction(&mut client).await.unwrap();
    sqlx::raw_sql("INSERT INTO registry.org (id) VALUES ('demo')")
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

/// A project save uses the applied environment timings without replacing them.
#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn project_configuration_keeps_applied_env_timings() -> anyhow::Result<()> {
    use wamn_env::{env::document::EnvDocument, registry::configuration};

    let system = wamn_test_infrastructure::database::system().await?;
    let login = system.org_login(wamn_role::APPLY_ROLE)?;
    let url = system.login_url(login.login.as_str(), &login.password)?;
    let mut client = wamn_postgres::connect(url.as_str()).await?;
    let document = EnvDocument::from_json(
        br#"{"org":"acme","project":"inventory","env":"dev","target":"cloud",
             "readiness_budget_seconds":123,"drain_bound_seconds":234,"revocation_seconds":45,
             "apps":{}}"#,
    )?;
    let triple = document.triple();
    let mut transaction = provisioning_transaction(&mut client).await?;
    wamn_env::env_registry::record_project_env(
        &mut transaction,
        &triple,
        &document.settings.target,
        "fixture-secret",
        Some(triple.org.as_str()),
        &"abcd1234".parse()?,
        None,
    )
    .await?;
    let stored = (321, 432, 75);
    sqlx::query(
        "UPDATE registry.env
         SET readiness_budget_seconds=$3, drain_bound_seconds=$4, revocation_seconds=$5
         WHERE org=$1 AND env=$2",
    )
    .bind(triple.org.as_str())
    .bind(triple.env.as_str())
    .bind(stored.0)
    .bind(stored.1)
    .bind(stored.2)
    .execute(&mut *transaction)
    .await?;
    assert!(configuration::write(&mut transaction, &document.configuration()).await?);
    let applied = configuration::read(&mut transaction, &triple)
        .await?
        .expect("the project was configured");
    assert_eq!(
        (
            applied.readiness_budget_seconds,
            applied.drain_bound_seconds,
            i32::try_from(applied.revocation_seconds)?,
        ),
        stored
    );
    transaction.commit().await?;
    Ok(())
}
