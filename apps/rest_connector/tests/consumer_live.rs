//! Apply the release's data access and call its consumer through the native host.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use tokio_postgres::Client;
use wamn_control::apply_package::{ApplyPackageRequest, PackageRoot, apply_package};
use wamn_control::component_admission::ComponentAdmission;
use wamn_control::reconcile_package_data_access::{
    ReconcilePackageDataAccessRequest, reconcile_package_data_access,
};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_engine::operation::{OperationCall, OperationClosure, invoke_operation, node_types};
use wamn_execution_host::{NativeFacts, NodeAcquisition, OperationHost, OperationScope};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, ClassCredentials, ProjectConfig, StaticCredentialProvider, WamnPostgres,
};

const TENANT: &str = "rest--publication--test";
const OPERATION: &str = "rest-consumer-fixture:item/record@0.1.0";
const PASSWORD: &str = "rest-consumer-live-test";

pub async fn install(
    admin: &Client,
    database: &wamn_test_postgres::Database,
    roots: &[PathBuf],
    manifest: &wamn_catalog::ServingManifest,
) -> anyhow::Result<String> {
    admin
        .batch_execute("CREATE ROLE wamn_app NOLOGIN; CREATE ROLE wamn_scenario_author NOLOGIN;")
        .await?;
    admin.batch_execute(sql::ensure_db_owner_role_sql()).await?;
    admin.batch_execute("DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $$").await?;
    admin
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await?;
    admin
        .batch_execute(include_str!("../../../deploy/sql/app-schema.sql"))
        .await?;
    let packages = roots
        .iter()
        .map(|root| {
            Ok(PackageRoot {
                path: root.clone(),
                artifact_digest: Some(
                    wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(root)?,
                ),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for package in &packages {
        apply_package(ApplyPackageRequest {
            package: package.clone(),
            database_url: database.url().into(),
            tenant: TENANT.into(),
        })
        .await?;
    }
    reconcile_package_data_access(ReconcilePackageDataAccessRequest {
        packages,
        database_url: database.url().into(),
        tenant: TENANT.into(),
    })
    .await?;
    let role = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database: database.name(),
        },
        CredentialGeneration::A,
    )?;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::App,
            database.name(),
            &role,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await?;
    admin
        .batch_execute(&wamn_control_provision::platform_principals_sql(
            TENANT,
            "example.test",
        )?)
        .await?;
    let system = wamn_control_provision::test_database::system();
    system.execute(&["UPDATE registry.meta SET platform_domain = 'example.test'"])?;
    let manifests = roots
        .iter()
        .map(|root| {
            Ok(wamn_schema_generator::PackageManifest::from_slice(
                &std::fs::read(wamn_schema_generator::package_manifest_path(root))?,
            )?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    wamn_control::environment::stage::materialize_application_principals(
        system.url(),
        database.url(),
        TENANT,
        manifest,
        &manifests,
        &wamn_platform_identity::auth_changes::AuthChanges::default(),
    )
    .await?;
    let privileges = admin
        .query_one(
            "SELECT has_column_privilege($1, 'rest_consumer_fixture.item', 'key', 'SELECT'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'name', 'SELECT'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'key', 'INSERT'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'name', 'INSERT'), \
         has_table_privilege($1, 'platform.write_log', 'SELECT') \
           AND has_table_privilege($1, 'platform.write_log', 'INSERT') \
           AND has_table_privilege($1, 'platform.write_log', 'DELETE'), \
         has_column_privilege($1, 'platform.write_log', 'result', 'UPDATE')",
            &[&role],
        )
        .await?;
    assert_eq!(
        (0..6)
            .map(|index| privileges.get::<_, bool>(index))
            .collect::<Vec<_>>(),
        [true, false, true, true, true, true]
    );
    eprintln!(
        "consumer SQL role={role}, schema=rest_consumer_fixture; item SELECT(key), INSERT(key,name); write_log SELECT/INSERT/DELETE, UPDATE(result)"
    );
    let mut url = url::Url::parse(database.url())?;
    url.set_username(&role).unwrap();
    url.set_password(Some(PASSWORD)).unwrap();
    Ok(url.into())
}

pub async fn invoke(
    database: &wamn_test_postgres::Database,
    guest_url: &str,
    manifest: &wamn_catalog::ServingManifest,
    admissions: &[ComponentAdmission],
) -> anyhow::Result<()> {
    use wamn_engine::artifact_source::LocalComponentSource;
    use wamn_engine::operation::native_workload::NativeComponent;
    use wamn_engine::release_manifest::{LoadedRelease, ReleaseScope};
    use wamn_runtime::plugins::connection_http::transport::HttpTransport;
    use wamn_runtime::plugins::connection_http::{
        ConnectionExecutionClosure, InvocationEntry, WiringPosition,
    };
    use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
    use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
    use wamn_workflow::Workflows as _;

    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &manifest.canonical_bytes(),
        "consumer live",
        ReleaseScope::new(TENANT, "test"),
    )?);
    let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(
            HashMap::from([(
                "test".into(),
                ProjectConfig {
                    credentials: ClassCredentials::default()
                        .with_class(AuthorityClass::GuestSql, guest_url)
                        .with_class(AuthorityClass::ExecutorPlatform, database.url()),
                    guest_pool_max_size: 2,
                    platform_pool_max_size: 2,
                    wait_timeout_ms: 5000,
                    statement_timeout_ms: 10000,
                    row_limit: 1000,
                },
            )]),
            None,
        ),
    )));
    let host = OperationHost::new(
        Arc::new(wamn_engine::build_engine(&[])?),
        postgres,
        Arc::new(HttpTransport::new()?),
        Arc::new(WamnCredentials::empty()),
        Arc::new(WamnLogging::new(&WamnLoggingConfig::default())?),
        Arc::from([]),
        release,
        Arc::new(LocalComponentSource::new(PathBuf::new())),
        OperationScope {
            project: "test".into(),
            schema: Some("wamn_integration".into()),
            owner_prefix: "consumer-live".into(),
            warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
        },
    )?;
    let component = admissions
        .iter()
        .find(|admission| admission.component() == "rest_consumer_fixture")
        .unwrap()
        .facts();
    assert!(component.operations[OPERATION].host_transaction);
    let bytes = std::fs::read(
        wamn_schema_generator::output_root(
            &super::support::repository().join("apps/rest_consumer_fixture"),
        )
        .join("components/rest_consumer_fixture.wasm"),
    )?;
    let application = host
        .load_application(vec![NativeComponent {
            fact: component.clone(),
            bytes,
        }])
        .await?;
    let intents = host.call_intents(component, OPERATION)?;
    eprintln!(
        "consumer host transaction declared=true, intent context present={}",
        intents.is_some()
    );
    let (admin, connection) =
        tokio_postgres::connect(database.url(), tokio_postgres::NoTls).await?;
    tokio::spawn(connection);
    wamn_control::reconcile_run_plane::reconcile(
        &admin,
        &wamn_schema_control::BareSchemaName::new("wamn_run")?,
        true,
    )
    .await?;
    admin.execute("INSERT INTO wamn_run.environment_policies (tenant_id,expected_environment,durability_class) VALUES ($1,'test','standard')", &[&TENANT]).await?;
    let (_, digest) =
        wamn_catalog::ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())?;
    wamn_control::environment::stage::write_release(
        database.url(),
        TENANT,
        digest.as_str(),
        manifest,
    )
    .await?;
    let schedule = manifest
        .workflow
        .schedules
        .values()
        .find(|schedule| schedule.package_id == "rest_connector")
        .unwrap();
    let due =
        chrono::DateTime::parse_from_rfc3339("2026-10-21T12:02:00Z")?.with_timezone(&chrono::Utc);
    let (client, connection) =
        tokio_postgres::connect(database.url(), tokio_postgres::NoTls).await?;
    tokio::spawn(connection);
    let workflows = wamn_workflow::PostgresWorkflows::new(client, "wamn_run", TENANT, "test");
    let run = workflows
        .start(&wamn_workflow::StartRequest {
            manifest_digest: digest.to_string(),
            package_id: "rest_connector".into(),
            wiring_id: schedule.wiring_id.clone(),
            wiring_version: schedule.wiring_version,
            idempotency_key: wamn_workflow::schedule::idempotency_key(
                "rest_connector",
                &schedule.workflow_id,
                due,
            ),
            input: json!({"due": wamn_workflow::schedule::due_text(due)}),
            trigger: wamn_workflow::Trigger::Schedule {
                workflow_id: schedule.workflow_id.clone(),
                due,
            },
        })
        .await?;
    let principal: String = admin
        .query_one(
            "SELECT service_principal_id::text FROM wamn_run.runs WHERE run_id=$1",
            &[&run],
        )
        .await?
        .get(0);
    let caller = wamn_runtime::plugins::route_authentication::queued_service_caller(
        &admin, TENANT, &principal,
    )
    .await?;
    wamn_engine::router_delivery::authorize_registered_operation(Some(&caller), Some(OPERATION))?;
    let finish_operation = "rest-connector:sync/finish@0.1.0";
    let connector = admissions
        .iter()
        .find(|admission| admission.component() == "rest_connector")
        .unwrap()
        .facts();
    let connector_bytes = std::fs::read(
        wamn_schema_generator::output_root(
            &super::support::repository().join("apps/rest_connector"),
        )
        .join("components/rest_connector.wasm"),
    )?;
    let connector_application = host
        .load_application(vec![NativeComponent {
            fact: connector.clone(),
            bytes: connector_bytes,
        }])
        .await?;
    let mut failed_payload = serde_json::Value::Null;
    for request in [
        "live-request",
        "retry-request",
        "2026-10-21T12:02:00Z",
        "failed-finish",
    ] {
        let finishing = request == "failed-finish";
        let component = if finishing { connector } else { component };
        let operation = if finishing {
            finish_operation
        } else {
            OPERATION
        };
        let application = if finishing {
            &connector_application
        } else {
            &application
        };
        let input = if finishing {
            failed_payload.clone()
        } else {
            let (key, name) = if request == "2026-10-21T12:02:00Z" {
                ("failed-key", "")
            } else {
                ("live-key", "native name")
            };
            json!([{"request_id":request, "value":{"idempotency_key":key, "items":[{"key":"native-item", "name":name}]}}])
        };
        let intents = host.call_intents(component, operation)?;
        let call = OperationCall {
            closure: OperationClosure::Candidate(application),
            component,
            operation,
            context: node_types::NodeContext {
                wiring_id: "rest_item".into(),
                wiring_version: 1,
                node_id: if finishing { "finish" } else { "record" }.into(),
                delivery_id: run.clone(),
                input_port: None,
                occurrence: 0,
                traceparent: None,
                tracestate: None,
                deadline_ms: Some(10000),
                config: "null".into(),
            },
            input: &input,
            deadline_ms: 10000,
            facts: NativeFacts::entry(
                NodeAcquisition {
                    claims: host.claims(TENANT, Some(host.release_identity())),
                    invocation: wamn_execution_host::component_invocation(
                        component,
                        operation,
                        InvocationEntry::Wiring(WiringPosition {
                            package_id: "rest_connector".into(),
                            wiring_id: "rest_item".into(),
                            wiring_version: 1,
                            node_id: if finishing { "finish" } else { "record" }.into(),
                            occurrence: 0,
                        }),
                        ConnectionExecutionClosure::Released,
                        None,
                    ),
                    causation: None,
                    platform: None,
                },
                Some(caller.clone()),
            ),
        };
        let result = invoke_operation(
            &host,
            call,
            intents
                .as_ref()
                .map(wamn_execution_host::CallIntents::context),
        )
        .await?;
        if finishing {
            assert!(
                result.is_err(),
                "a failed consumer outcome must refuse checkpoint advancement"
            );
        } else {
            let emission = result
                .map_err(|error| anyhow::anyhow!("cross-package consumer failed: {error:?}"))?;
            let payload: serde_json::Value = serde_json::from_str(&emission.payload)?;
            if request == "2026-10-21T12:02:00Z" {
                assert!(
                    payload[0].get("error").is_some(),
                    "consumer must fail: {payload}"
                );
                failed_payload = payload;
            } else {
                assert_eq!(payload[0]["value"]["keys"], json!(["native-item"]));
            }
        }
    }
    connector_application.workload.unbind_all_plugins().await?;
    application.workload.unbind_all_plugins().await?;
    Ok(())
}
