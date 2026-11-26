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

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn project_deactivation_frees_capacity_and_readd_starts_a_new_generation()
-> anyhow::Result<()> {
    use wamn_env::{env::document::EnvDocument, registry::configuration};
    use wamn_identity::identity::{
        create_user, grant_project_env_membership, has_project_env_membership,
        org::activate_org_membership,
    };
    let system = wamn_test_infrastructure::database::system().await?;
    let mut admin = system.admin().await?;
    let login = system.org_login(wamn_role::APPLY_ROLE)?;
    let url = system.login_url(login.login.as_str(), &login.password)?;
    let mut apply = wamn_postgres::connect(url.as_str()).await?;
    let document = EnvDocument::from_json(
        br#"{"org":"acme","project":"inventory","env":"dev","target":"cloud",
             "revocation_seconds":60,"integrations":{"shared-erp":{"version":1}},
             "apps":{"inventory":{"label":"inventory","connections":{"erp":"shared-erp"}}}}"#,
    )?;
    let triple = document.triple();
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let original = "abcd1234".parse()?;
    let candidate = "efgh5678".parse()?;
    let target = deployment::target_id(&mut apply, &document.settings.target)
        .await?
        .expect("fixture deployment target");
    let mut registration = Registration {
        triple: &triple,
        secret_name: "fixture-secret",
        secret_namespace: None,
        instance_suffix: &original,
        disposable: None,
        target_id: &target,
    };
    let mut tx = provisioning_transaction(&mut apply).await?;
    deployment::record_project(&mut tx, org, project).await?;
    deployment::record(&mut tx, &registration).await?;
    assert!(configuration::write(&mut tx, &document.configuration()).await?);
    sqlx::raw_sql(
        "INSERT INTO registry.event_reader
           (org, project, env, publication, slot, stream, replication_secret_name)
         VALUES ('acme', 'inventory', 'dev', 'capture', 'capture', 'stream', 'secret');
         INSERT INTO registry.capture_gap (org, project, env, slot, start_at, reason, end_lsn)
         VALUES ('acme', 'inventory', 'dev', 'capture', now(), 'lost', '0/1');",
    )
    .execute(&mut *tx)
    .await?;
    registration.instance_suffix = &candidate;
    assert_eq!(deployment::record(&mut tx, &registration).await?, original);
    tx.commit().await?;

    let mut tx = provisioning_transaction(&mut admin).await?;
    let email = "member@example.com";
    let member = create_user(&mut tx, email, email, "Member").await?;
    activate_org_membership(&mut tx, member.id(), org).await?;
    grant_project_env_membership(&mut tx, member.id(), org, project, env).await?;
    tx.commit().await?;

    let available = || {
        sqlx::query_scalar::<_, bool>(
            "SELECT registry.project_capacity_available(max_projects::int) FROM registry.meta",
        )
    };
    let reader = || {
        sqlx::query(wamn_env::env_sql::select_event_reader_origin_sql())
            .bind(org.as_str())
            .bind(project.as_str())
            .bind(env.as_str())
    };
    assert!(!available().fetch_one(&mut apply).await?);
    assert!(configuration::read(&mut apply, &triple).await?.is_some());
    assert!(reader().fetch_optional(&mut apply).await?.is_some());
    assert!(has_project_env_membership(&mut admin, member.id(), org, project, env).await?);

    let mut tx = provisioning_transaction(&mut apply).await?;
    deployment::deactivate(&mut tx, &triple).await?;
    tx.commit().await?;
    let active: bool = sqlx::query_scalar(
        "SELECT active FROM registry.project_env WHERE org='acme' AND project='inventory' AND env='dev'",
    )
    .fetch_one(&mut apply)
    .await?;
    assert!(!active);
    assert!(available().fetch_one(&mut apply).await?);
    assert!(configuration::read(&mut apply, &triple).await?.is_none());
    assert!(reader().fetch_optional(&mut apply).await?.is_none());
    let revoked: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT FROM identity.project_env_membership_history
         WHERE type='delete' AND row_key->>'principal_id'=$1
         AND row_key->>'org'='acme' AND row_key->>'project'='inventory' AND row_key->>'env'='dev')",
    )
    .bind(member.id().to_string())
    .fetch_one(&mut admin)
    .await?;
    assert!(
        revoked,
        "deactivation records the revoked membership in history"
    );

    let mut tx = provisioning_transaction(&mut admin).await?;
    let denied = grant_project_env_membership(&mut tx, member.id(), org, project, env)
        .await
        .expect_err("inactive deployments refuse new memberships");
    assert_eq!(
        denied.error_type(),
        wamn_authentication::identity_error::IdentityErrorType::NotFound,
    );
    tx.rollback().await?;

    let mut tx = provisioning_transaction(&mut apply).await?;
    configuration::retain_integrations(&mut tx, org, env, &[]).await?;
    let reserved = deployment::record(&mut tx, &registration).await?;
    tx.commit().await?;
    assert_eq!(reserved, candidate);
    assert!(!available().fetch_one(&mut apply).await?);
    assert!(configuration::read(&mut apply, &triple).await?.is_none());
    assert!(
        reader().fetch_optional(&mut apply).await?.is_none(),
        "re-add discards the old reader registration"
    );
    assert!(!has_project_env_membership(&mut admin, member.id(), org, project, env).await?);
    Ok(())
}

/// Applying an edited env file deactivates the omitted app and can reactivate it.
#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn env_apply_deactivates_and_reactivates_an_app() -> anyhow::Result<()> {
    use wamn_env::{
        env::document::{EnvDocument, EnvFile},
        registry::configuration,
    };
    use wamn_test_infrastructure::{database, scratch::ScratchRoot};

    let scratch = ScratchRoot::create()?;
    let path = scratch.path().join("env.yaml");
    let declared = "org: acme
env: dev
target: cloud
revocation_seconds: 45
database: {instances: 1, storage: 1Gi, cpu: '1', memory: 256Mi}
connections: {erp: {version: 1}}
projects:
  - name: inventory
    apps:
      stock: {label: stock-web, connections: {backend: erp}}
";
    std::fs::write(&path, declared)?;
    let mut omitted = EnvFile::compile(&path)?;
    let (app, selection) = omitted.projects[0]
        .apps
        .pop_first()
        .expect("the file declares one app");
    let triple = omitted.triple(&omitted.projects[0].name);
    let omitted = omitted.to_yaml()?;

    let system = database::system().await?;
    let login = system.org_login(wamn_role::APPLY_ROLE)?;
    let url = system.login_url(login.login.as_str(), &login.password)?;
    let mut client = wamn_postgres::connect(url.as_str()).await?;
    let mut transaction = provisioning_transaction(&mut client).await?;
    wamn_env::env_registry::record_project_env(
        &mut transaction,
        &triple,
        "cloud",
        "fixture-secret",
        Some(triple.org.as_str()),
        &"abcd1234".parse()?,
        None,
    )
    .await?;
    transaction.commit().await?;

    for (file, active) in [
        (declared, true),
        (omitted.as_str(), false),
        (declared, true),
    ] {
        std::fs::write(&path, file)?;
        let document = EnvDocument::compile(&path)?;
        let mut transaction = provisioning_transaction(&mut client).await?;
        assert!(configuration::write(&mut transaction, &document.configuration()).await?);
        transaction.commit().await?;

        let (stored_active, label): (bool, String) = sqlx::query_as(
            "SELECT active, label FROM registry.env_app
             WHERE org=$1 AND env=$2 AND project=$3 AND app=$4",
        )
        .bind(triple.org.as_str())
        .bind(triple.env.as_str())
        .bind(triple.project.as_str())
        .bind(app.as_str())
        .fetch_one(&mut client)
        .await?;
        assert_eq!(stored_active, active);
        assert_eq!(label, selection.label);
        let applied = configuration::read(&mut client, &triple)
            .await?
            .expect("the project was configured");
        assert_eq!(
            applied.apps.get(&app).map(|app| &app.connections),
            active.then_some(&selection.connections)
        );
    }
    Ok(())
}
