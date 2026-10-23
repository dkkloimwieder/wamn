//! Application principals through apply's release stage, schedule admission, and authorization.

use serde_json::json;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ServingManifest, ServingWiring};
use wamn_control::environment::stage::{materialize_application_principals, write_release};
use wamn_engine::router_delivery::authorize_registered_operation;
use wamn_platform_identity::auth_changes::AuthChanges;
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
async fn apply_creates_one_integration_principal_and_grants_declared_dependencies()
-> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    system.execute(&["UPDATE registry.meta SET platform_domain = 'example.invalid'"])?;
    let project = wamn_test_postgres::database();
    let admin = connect(project.url()).await?;
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
    let mut manifest: ServingManifest = serde_json::from_value(json!({
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
    let mut descriptor = wamn_catalog::ComponentDescriptor::named(
        "consumer",
        "1.0.0",
        format!("sha256:{}", "a".repeat(64)),
    );
    descriptor.operations.insert(OPERATION.into(), serde_json::from_value(json!({
        "registered-operation": OPERATION, "input-ports": [], "output-ports": [], "parameters": []
    }))?);
    manifest.components.insert(wamn_catalog::ServingComponent {
        package_id: "consumer".into(),
        runtime_schema: None,
        descriptor,
        operations: std::collections::BTreeMap::from([(
            OPERATION.into(),
            wamn_catalog::ServingComponentOperation {
                registered_operation: Some(OPERATION.into()),
                permissions: [OPERATION.into()].into(),
                committed_result_schema: None,
                statements: std::collections::BTreeMap::new(),
            },
        )]),
        statement_instances: std::collections::BTreeMap::new(),
    });
    let mut package: wamn_schema_generator::PackageManifest =
        serde_json::from_str(include_str!("fixtures/observer_package/wamn.json"))?;
    package.package.id = "integration".into();
    package.package.version = "1.0.0".into();
    package.base_dependencies = serde_json::from_value(json!({
        "state": {"package":"wamn_integration", "version":"1.0.0", "digest":format!("sha256:{}", "b".repeat(64)), "operations":[]},
        "consumer": {"package":"consumer", "version":"1.0.0", "digest":format!("sha256:{}", "a".repeat(64)), "operations":["item.import"]}
    }))?;
    let mut application = manifest.components.iter().next().unwrap().clone();
    application.package_id = "integration".into();
    application.descriptor.component = "integration".into();
    application.descriptor.operations.clear();
    application.descriptor.operations.insert(
        "integration:sync/finish@1.0.0".into(),
        serde_json::from_value(json!({
            "input-ports": [], "output-ports": [], "parameters": []
        }))?,
    );
    application.operations.clear();
    application.operations.insert(
        "integration:sync/finish@1.0.0".into(),
        serde_json::from_value(json!({}))?,
    );
    application
        .descriptor
        .statement_instances
        .insert("state".into(), format!("sha256:{}", "b".repeat(64)));
    manifest.components.insert(application);
    let mut declaration_only = package.clone();
    declaration_only.package.id = "declaration_only".into();
    let mut consumer = package.clone();
    consumer.package.id = "consumer".into();
    consumer.base_dependencies.clear();
    let mut operation = consumer.custom_operations.values().next().unwrap().clone();
    operation.permission = Some("item.import".into());
    operation.visibility = wamn_schema_generator::OperationVisibility::Public;
    consumer.custom_operations.clear();
    consumer
        .custom_operations
        .insert("item.import".into(), operation);
    let packages = vec![package, declaration_only, consumer];
    // Composition alone must materialize the principal, before schedules exist.
    let mut unscheduled = manifest.clone();
    unscheduled.workflow.schedules.clear();
    materialize_application_principals(
        system.url(),
        project.url(),
        TENANT,
        &unscheduled,
        &packages,
        &AuthChanges::default(),
    )
    .await?;
    let (_, digest) = ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())?;
    for _ in 0..2 {
        write_release(project.url(), TENANT, digest.as_str(), &manifest).await?;
        materialize_application_principals(
            system.url(),
            project.url(),
            TENANT,
            &manifest,
            &packages,
            &AuthChanges::default(),
        )
        .await?;
    }
    let row = admin.query_one("SELECT count(*), min(id::text) FROM platform.\"user\" WHERE tenant_id=$1 AND type='service'", &[&TENANT]).await?;
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
            .query_one("SELECT count(*) FROM platform.user_roles", &[])
            .await?
            .get::<_, i64>(0),
        1
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
    authorize_registered_operation(Some(&caller), Some(OPERATION))?;
    let before: Vec<String> = admin.query("SELECT xmin::text FROM platform.\"user\" UNION ALL SELECT xmin::text FROM platform.roles UNION ALL SELECT xmin::text FROM platform.permissions UNION ALL SELECT xmin::text FROM platform.user_roles ORDER BY 1", &[]).await?.iter().map(|row| row.get(0)).collect();
    materialize_application_principals(
        system.url(),
        project.url(),
        TENANT,
        &manifest,
        &packages,
        &AuthChanges::default(),
    )
    .await?;
    let after: Vec<String> = admin.query("SELECT xmin::text FROM platform.\"user\" UNION ALL SELECT xmin::text FROM platform.roles UNION ALL SELECT xmin::text FROM platform.permissions UNION ALL SELECT xmin::text FROM platform.user_roles ORDER BY 1", &[]).await?.iter().map(|row| row.get(0)).collect();
    assert_eq!(before, after, "reapplying must write no application rows");
    let mut unscheduled = manifest.clone();
    unscheduled.workflow.schedules.clear();
    materialize_application_principals(
        system.url(),
        project.url(),
        TENANT,
        &unscheduled,
        &packages,
        &AuthChanges::default(),
    )
    .await?;
    let caller = queued_service_caller(&admin, TENANT, &principal).await?;
    authorize_registered_operation(Some(&caller), Some(OPERATION))?;
    assert!(
        authorize_registered_operation(Some(&caller), Some("consumer:item/delete@1.0.0")).is_err()
    );
    // A scheduled application needs no dependencies to retain its own private operations.
    let mut scheduled = packages[0].clone();
    scheduled.package.id = "scheduled".into();
    scheduled.base_dependencies.clear();
    let mut schedule_only = manifest.clone();
    schedule_only
        .release
        .packages
        .insert(wamn_catalog::PackageCoordinate::new("scheduled", "1.0.0")?);
    schedule_only.workflow.wirings.insert(ServingWiring::new(
        "scheduled",
        wamn_catalog::fixture_graph("pull", 1),
    ));
    let mut schedule = schedule_only
        .workflow
        .schedules
        .values()
        .next()
        .unwrap()
        .clone();
    schedule.package_id = "scheduled".into();
    schedule.workflow_id = "hourly".into();
    schedule_only
        .workflow
        .schedules
        .insert("scheduled::hourly".into(), schedule);
    admin.execute("INSERT INTO catalog.packages (tenant_id,package_id,package_version,artifact_digest) VALUES ($1,'scheduled','1.0.0',$2)", &[&TENANT, &format!("sha256:{}", "a".repeat(64))]).await?;
    let (_, scheduled_digest) =
        ServingManifest::from_canonical_bytes(&schedule_only.canonical_bytes())?;
    write_release(
        project.url(),
        TENANT,
        scheduled_digest.as_str(),
        &schedule_only,
    )
    .await?;
    materialize_application_principals(
        system.url(),
        project.url(),
        TENANT,
        &schedule_only,
        &[scheduled],
        &AuthChanges::default(),
    )
    .await?;
    let mut scheduled_request = request.clone();
    scheduled_request.manifest_digest = scheduled_digest.to_string();
    scheduled_request.package_id = "scheduled".into();
    scheduled_request.idempotency_key =
        wamn_workflow::schedule::idempotency_key("scheduled", "hourly", due);
    let scheduled_run = workflows.start(&scheduled_request).await?;
    let scheduled_principal: String = admin
        .query_one(
            "SELECT service_principal_id::text FROM wamn_run.runs WHERE run_id=$1",
            &[&scheduled_run],
        )
        .await?
        .get(0);
    let own_caller = queued_service_caller(&admin, TENANT, &scheduled_principal).await?;
    authorize_registered_operation(Some(&own_caller), None)?;
    assert_eq!(
        admin
            .query_one(
                "SELECT count(*) FROM platform.permissions WHERE role_name='application-scheduled'",
                &[]
            )
            .await?
            .get::<_, i64>(0),
        0
    );
    Ok(())
}
