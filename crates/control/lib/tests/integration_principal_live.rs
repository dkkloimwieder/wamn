//! Application principals through apply's release stage, schedule admission, and authorization.

use serde_json::json;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ServingManifest, ServingWiring};
use wamn_control::environment::stage::{materialize_application_principals, write_release};
use wamn_engine::router_delivery::authorize_registered_operation;
use wamn_platform_identity::auth_changes::{AuthChanges, AuthDrops};
use wamn_runtime::plugins::route_authentication::queued_service_caller;
use wamn_schema_control::BareSchemaName;
use wamn_workflow::{PostgresWorkflows, StartRequest, Trigger, Workflows as _};

const TENANT: &str = "acme--erp--dev";
const OPERATION: &str = "consumer:item/import@1.0.0";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(connection);
    Ok(client)
}

#[tokio::test]
async fn apply_creates_one_principal_and_schedules_use_its_authored_grants() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    system.execute(&["UPDATE registry.meta SET platform_domain = 'example.invalid'"])?;
    let project = wamn_test_postgres::database();
    let mut admin = connect(project.url()).await?;
    let schema = BareSchemaName::new("wamn_run")?;
    wamn_control::reconcile_run_plane::reconcile(&admin, &schema, true).await?;
    admin
        .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
        .await?;
    admin
        .batch_execute(&wamn_control_provision::platform_principals_sql(
            TENANT,
            "example.invalid",
        )?)
        .await?;
    // Simulate the old service check and exercise the actual schema transition.
    admin
        .batch_execute(
            "ALTER TABLE wamn_run.runs DROP CONSTRAINT runs_service_principal_check;
        ALTER TABLE wamn_run.runs ADD CONSTRAINT runs_service_principal_check CHECK (
          (trigger_source IS NOT DISTINCT FROM 'automation') = (service_principal_id IS NOT NULL));
        ALTER TABLE wamn_run.runs DROP CONSTRAINT runs_execution_grain_check;
        ALTER TABLE wamn_run.runs ADD CONSTRAINT runs_execution_grain_check CHECK (
          trigger_source IS DISTINCT FROM 'schedule' OR service_principal_id IS NULL)",
        )
        .await?;
    wamn_control::reconcile_run_plane::reconcile(&admin, &schema, true).await?;
    assert!(
        wamn_control::reconcile_run_plane::reconcile(&admin, &schema, false)
            .await?
            .actions
            .is_empty()
    );
    admin.execute("INSERT INTO wamn_run.environment_policies (tenant_id, expected_environment, durability_class) VALUES ($1, 'dev', 'standard')", &[&TENANT]).await?;
    for package in ["consumer", "integration"] {
        admin.execute("INSERT INTO catalog.packages (tenant_id,package_id,package_version,artifact_digest) VALUES ($1,$2,'1.0.0',$3)", &[&TENANT, &package, &format!("sha256:{}", "a".repeat(64))]).await?;
    }
    let manifest: ServingManifest = serde_json::from_value(json!({
        "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"packages": [
            {"package-id": "consumer", "package-version": "1.0.0"},
            {"package-id": "integration", "package-version": "1.0.0"}
        ]},
        "components": [], "routes": [], "attachments": {},
        "workflow": {"wirings": [ServingWiring::new("integration", wamn_catalog::fixture_graph("pull", 1))],
            "schedules": {
                "integration::hourly": {"package-id":"integration", "workflow-id":"hourly", "wiring-id":"pull", "wiring-version":1, "schedule":"0 * * * *"},
                "integration::daily": {"package-id":"integration", "workflow-id":"daily", "wiring-id":"pull", "wiring-version":1, "schedule":"0 0 * * *"}
            }}
    }))?;
    let (_, digest) = ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())?;
    for _ in 0..2 {
        write_release(project.url(), TENANT, digest.as_str(), &manifest).await?;
        materialize_application_principals(
            system.url(),
            project.url(),
            TENANT,
            &manifest,
            &AuthChanges::default(),
        )
        .await?;
    }
    let row = admin.query_one("SELECT count(*), min(id::text) FROM app_system.users WHERE tenant_id=$1 AND type='service'", &[&TENANT]).await?;
    assert_eq!(row.get::<_, i64>(0), 1);
    let principal: String = row.get(1);
    let identity = connect(system.url()).await?;
    assert_eq!(
        identity
            .query_one(
                "SELECT count(*) FROM identity.principals WHERE type='service'",
                &[]
            )
            .await?
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        identity
            .query_one("SELECT count(*) FROM identity.project_roles", &[])
            .await?
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        admin
            .query_one("SELECT count(*) FROM app_system.user_roles", &[])
            .await?
            .get::<_, i64>(0),
        0
    );
    let workflows =
        PostgresWorkflows::new(connect(project.url()).await?, "wamn_run", TENANT, "dev");
    let due =
        chrono::DateTime::parse_from_rfc3339("2026-10-21T12:00:00Z")?.with_timezone(&chrono::Utc);
    let request = StartRequest {
        manifest_digest: digest.to_string(),
        package_id: "integration".to_owned(),
        wiring_id: "pull".to_owned(),
        wiring_version: 1,
        idempotency_key: wamn_workflow::schedule::idempotency_key("integration", "hourly", due),
        input: json!({"due": wamn_workflow::schedule::due_text(due)}),
        trigger: Trigger::Schedule {
            workflow_id: "hourly".to_owned(),
            due,
        },
    };
    let run = workflows.start(&request).await?;
    assert_eq!(workflows.start(&request).await?, run);
    assert_eq!(
        admin
            .query_one(
                "SELECT service_principal_id::text FROM wamn_run.runs WHERE run_id=$1",
                &[&run]
            )
            .await?
            .get::<_, String>(0),
        principal
    );
    let caller = queued_service_caller(&admin, TENANT, &principal).await?;
    assert!(authorize_registered_operation(Some(&caller), Some(OPERATION)).is_err());
    let transaction = admin.transaction().await?;
    let mut drops = AuthDrops::default();
    wamn_control::role_permissions::create_role(&transaction, TENANT, "importer", &mut drops)
        .await?;
    wamn_control::role_permissions::grant_permission(
        &transaction,
        TENANT,
        "importer",
        "consumer:item/import",
        &mut drops,
    )
    .await?;
    wamn_platform_identity::application::grant_user_role(
        &transaction,
        TENANT,
        &principal,
        "importer",
        &mut drops,
    )
    .await?;
    transaction.commit().await?;
    materialize_application_principals(
        system.url(),
        project.url(),
        TENANT,
        &manifest,
        &AuthChanges::default(),
    )
    .await?;
    let caller = queued_service_caller(&admin, TENANT, &principal).await?;
    authorize_registered_operation(Some(&caller), Some(OPERATION))?;
    assert!(
        authorize_registered_operation(Some(&caller), Some("consumer:item/delete@1.0.0")).is_err()
    );
    Ok(())
}
