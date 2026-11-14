//! Application principals through apply's release stage and authorization.

use serde_json::json;
use sqlx::PgConnection as Client;
use sqlx::Row as _;

use wamn_broker::Publisher;
use wamn_catalog::ServingManifest;
use wamn_environment::environment::stage::{materialize_application_principals, write_release};
use wamn_postgres::schema::BareSchemaName;

const PROJECT_ENV_KEY: &str = "acme--dev--erp";
const OPERATION: &str = "consumer:item/import@1.0.0";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let client = wamn_postgres::connect(url).await?;

    Ok(client)
}

#[tokio::test]
async fn apply_creates_one_integration_principal_and_grants_declared_dependencies()
-> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    system.execute(&["SET app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'; UPDATE registry.meta SET platform_domain = 'example.invalid'"])?;
    let project = wamn_test_postgres::database();
    let mut admin = connect(project.url()).await?;
    let schema = BareSchemaName::new("wamn_run")?;
    wamn_environment::platform_migrations::install_project(&mut admin).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::platform_principals_sql("example.invalid")?).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;

    for package in ["consumer", "integration"] {
        sqlx::query("INSERT INTO catalog.packages ( project_env_key,package_id,package_version,artifact_digest) VALUES ( $1,$2,'1.0.0',$3)").bind(&PROJECT_ENV_KEY).bind(&package).bind(&format!("sha256:{}", "a".repeat(64))).execute(&mut admin).await.map(|result| result.rows_affected())?;
    }
    let mut manifest: ServingManifest = serde_json::from_value(json!({
        "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"packages": [
            {"package-id": "consumer", "package-version": "1.0.0"},
            {"package-id": "integration", "package-version": "1.0.0"}
        ]},
        "components": [], "routes": [], "attachments": {},
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
        descriptor: {
            let mut descriptor = descriptor;
            descriptor.operations = std::collections::BTreeMap::from([(
                OPERATION.into(),
                wamn_catalog::AdmittedComponentOperation {
                    connections: Vec::new(),
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    registered_operation: Some(OPERATION.into()),
                    committed_result_schema: None,
                    statements: std::collections::BTreeMap::new(),
                },
            )]);
            descriptor
        },
        statement_instances: std::collections::BTreeMap::new(),
    });
    let mut package: wamn_compiler::manifest::PackageManifest = serde_json::from_str(
        include_str!("../../publish/tests/fixtures/observer_package/generated/wamn.json"),
    )?;
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
    operation.visibility = wamn_compiler::manifest::OperationVisibility::Public;
    consumer.custom_operations.clear();
    consumer
        .custom_operations
        .insert("item.import".into(), operation);
    let packages = vec![package, declaration_only, consumer];
    // Composition alone materializes the principal, before the release is written.
    materialize_application_principals(
        system.url(),
        project.url(),
        PROJECT_ENV_KEY,
        &manifest,
        &packages,
        &Publisher::default(),
    )
    .await?;
    let (_, digest) = ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())?;
    for _ in 0..2 {
        write_release(
            project.url(),
            PROJECT_ENV_KEY,
            digest.as_str(),
            &manifest,
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await?;
        materialize_application_principals(
            system.url(),
            project.url(),
            PROJECT_ENV_KEY,
            &manifest,
            &packages,
            &Publisher::default(),
        )
        .await?;
    }
    // The workflow user is the other service row
    // (docs/architecture/execution.md#environment-workflows).
    let workflow_user = wamn_identity_protocol::workflow_user_id().to_string();
    let row = sqlx::query(
        "SELECT count(*), min(id::text) FROM platform.\"user\" \
             WHERE type='service' AND id::text <> $1",
    )
    .bind(&workflow_user)
    .fetch_one(&mut admin)
    .await?;
    assert_eq!(row.get::<i64, _>(0), 1);
    let principal: String = row.get(1);
    let mut identity = connect(system.url()).await?;
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.principals WHERE type='service'")
            .fetch_one(&mut identity)
            .await?
            .get::<i64, _>(0),
        1
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.project_roles")
            .fetch_one(&mut identity)
            .await?
            .get::<i64, _>(0),
        0
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM platform.user_role")
            .fetch_one(&mut admin)
            .await?
            .get::<i64, _>(0),
        1
    );
    let caller = permissions(&mut admin, &principal).await?;
    assert!(caller.contains(OPERATION.rsplit_once('@').unwrap().0));
    let before: Vec<String> = sqlx::query("SELECT xmin::text FROM platform.\"user\" UNION ALL SELECT xmin::text FROM platform.role UNION ALL SELECT xmin::text FROM platform.permission UNION ALL SELECT xmin::text FROM platform.user_role ORDER BY 1").fetch_all(&mut admin).await?.iter().map(|row| row.get(0)).collect();
    materialize_application_principals(
        system.url(),
        project.url(),
        PROJECT_ENV_KEY,
        &manifest,
        &packages,
        &Publisher::default(),
    )
    .await?;
    let after: Vec<String> = sqlx::query("SELECT xmin::text FROM platform.\"user\" UNION ALL SELECT xmin::text FROM platform.role UNION ALL SELECT xmin::text FROM platform.permission UNION ALL SELECT xmin::text FROM platform.user_role ORDER BY 1").fetch_all(&mut admin).await?.iter().map(|row| row.get(0)).collect();
    assert_eq!(before, after, "reapplying must write no application rows");
    let caller = permissions(&mut admin, &principal).await?;
    assert!(caller.contains(OPERATION.rsplit_once('@').unwrap().0));
    assert!(!caller.contains("consumer:item/delete"));
    Ok(())
}

async fn permissions(
    client: &mut sqlx::PgConnection,
    principal: &str,
) -> anyhow::Result<std::collections::BTreeSet<String>> {
    Ok(sqlx::query("SELECT p.permission FROM platform.user_role r JOIN platform.permission p USING(role_name) WHERE r.user_id=$1::text::uuid")
        .bind(principal).fetch_all(client).await?.into_iter().map(|row| row.get(0)).collect())
}
