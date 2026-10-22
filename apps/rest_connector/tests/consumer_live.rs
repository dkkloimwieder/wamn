//! Apply the release's data access and call its consumer through the native host.

use std::collections::{HashMap, HashSet};
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

const TENANT: &str = "rest-publication";
const PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";
const OPERATION: &str = "rest-consumer-fixture:item/record@0.1.0";
const PASSWORD: &str = "rest-consumer-live-test";

pub async fn install(
    admin: &Client,
    database: &wamn_test_postgres::Database,
    roots: &[PathBuf],
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
    admin.execute("SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:consumer-live-fixture', false)", &[&PRINCIPAL]).await?;
    admin.execute("INSERT INTO app_system.users (tenant_id,id,type,email) VALUES ($1,$2::text::uuid,'service','connector@example.test')", &[&TENANT, &PRINCIPAL]).await?;
    admin
        .execute(
            "INSERT INTO app_system.roles (tenant_id,name) VALUES ($1,'consumer-writer')",
            &[&TENANT],
        )
        .await?;
    admin.execute("INSERT INTO app_system.permissions (tenant_id,role_name,permission) VALUES ($1,'consumer-writer','rest-consumer-fixture:item/record')", &[&TENANT]).await?;
    admin.execute("INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ($1,$2::text::uuid,'consumer-writer')", &[&TENANT,&PRINCIPAL]).await?;
    let privileges = admin
        .query_one(
            "SELECT has_column_privilege($1, 'rest_consumer_fixture.item', 'key', 'SELECT'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'name', 'SELECT'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'key', 'INSERT'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'name', 'INSERT'), \
         has_table_privilege($1, 'app_system.write_log', 'SELECT') \
           AND has_table_privilege($1, 'app_system.write_log', 'INSERT') \
           AND has_table_privilege($1, 'app_system.write_log', 'DELETE'), \
         has_column_privilege($1, 'app_system.write_log', 'result', 'UPDATE')",
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
    use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialType};
    use wamn_engine::operation::native_workload::NativeComponent;
    use wamn_engine::release_manifest::{LoadedRelease, ReleaseScope};
    use wamn_runtime::plugins::connection_http::transport::HttpTransport;
    use wamn_runtime::plugins::connection_http::{
        ConnectionExecutionClosure, InvocationEntry, WiringPosition,
    };
    use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
    use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};

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
    let caller = AuthenticatedCaller::new(
        "",
        PRINCIPAL,
        CredentialType::QueuedService,
        false,
        HashSet::from(["rest-consumer-fixture:item/record".into()]),
    );
    for request in ["live-request", "retry-request"] {
        let input = json!([{"request_id":request, "value":{"idempotency_key":"live-key", "items":[{"key":"native-item", "name":"native name"}]}}]);
        let call = OperationCall {
            closure: OperationClosure::Candidate(&application),
            component,
            operation: OPERATION,
            context: node_types::NodeContext {
                wiring_id: "rest_item".into(),
                wiring_version: 1,
                node_id: "record".into(),
                delivery_id: "consumer-live".into(),
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
                        OPERATION,
                        InvocationEntry::Wiring(WiringPosition {
                            package_id: "rest_connector".into(),
                            wiring_id: "rest_item".into(),
                            wiring_version: 1,
                            node_id: "record".into(),
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
        let emission =
            result.map_err(|error| anyhow::anyhow!("cross-package consumer failed: {error:?}"))?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&emission.payload)?[0]["value"]["keys"],
            json!(["native-item"])
        );
    }
    application.workload.unbind_all_plugins().await?;
    Ok(())
}
