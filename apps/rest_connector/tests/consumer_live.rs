//! Apply the release's data access and call its consumer through the native host.

use sqlx::Row as _;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use sqlx::PgConnection as Client;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};
use wamn_engine::operation::{OperationCall, OperationClosure, invoke_operation, node_types};
use wamn_environment::apply_package::{ApplyPackageRequest, PackageRoot, apply_package};
use wamn_execution_host::{NativeFacts, NodeAcquisition, OperationHost, OperationHostConfig};
use wamn_package_control::component_admission::ComponentAdmission;
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, ClassCredentials, ProjectConfig, StaticCredentialProvider, WamnPostgres,
};

const PROJECT_ENV_KEY: &str = "rest--test--publication";
const OPERATION: &str = "rest-consumer-fixture:item/record@0.1.1";
const PASSWORD: &str = "rest-consumer-live-test";
/// The execution role of the rest_connector item: exactly the operation its
/// graph calls that needs a grant.
const ROLE: &str = "rest-item";
/// The user who authors and enables a schedule workflow, as an environment
/// admin does. It holds the workflow's execution role.
const ENABLER: &str = "00000000-0000-4000-8000-0000000000e1";

/// The role administration an environment admin does before enabling a
/// workflow: `role` with exactly `operations`, held by the `workflow` user and
/// by the enabler. Returns the admin's project connection.
pub async fn execution_role(
    database: &wamn_test_postgres::Database,
    role: &str,
    operations: &[&str],
) -> anyhow::Result<Client> {
    let mut project = wamn_postgres::connect(database.url()).await?;

    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:schedule-fixture', false)",
    )
    .bind(
        &wamn_identity_protocol::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
    )
    .execute(&mut project)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query(
        "INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ($1::text::uuid, 'user', 'enabler@example.test') ON CONFLICT (id) DO NOTHING",
    )
    .bind(&ENABLER)
    .execute(&mut project)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO platform.role (name) VALUES ($1)")
        .bind(&role)
        .execute(&mut project)
        .await
        .map(|result| result.rows_affected())?;
    for operation in operations {
        sqlx::query(
            "INSERT INTO platform.permission (role_name, permission) \
                 VALUES ($1, $2)",
        )
        .bind(&role)
        .bind(operation)
        .execute(&mut project)
        .await
        .map(|result| result.rows_affected())?;
    }
    let workflow_user = wamn_identity_protocol::workflow_user_id().to_string();
    for user in [ENABLER, workflow_user.as_str()] {
        sqlx::query(
            "INSERT INTO platform.user_role (user_id, role_name) \
                 VALUES ($1::text::uuid, $2)",
        )
        .bind(&user)
        .bind(&role)
        .execute(&mut project)
        .await
        .map(|result| result.rows_affected())?;
    }
    Ok(project)
}

/// Install the run plane, the environment policy and `manifest` as the
/// serving release, and return the host's platform database access under the
/// targeted logins of its families: the executor for admission, and the
/// admitter for the per-node grant read. The host's credential test refuses
/// any other login, the superuser included.
pub async fn workflow_host(
    admin: &mut Client,
    database: &wamn_test_postgres::Database,
    manifest: &wamn_catalog::ServingManifest,
) -> anyhow::Result<Arc<WamnPostgres>> {
    wamn_environment::platform_migrations::install_project(admin).await?;
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(
            &wamn_identity_protocol::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .execute(&mut *admin)
        .await
        .map(|result| result.rows_affected())?;

    let (_, digest) =
        wamn_catalog::ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())?;
    wamn_environment::environment::stage::write_release(
        database.url(),
        PROJECT_ENV_KEY,
        digest.as_str(),
        manifest,
        &wamn_control_provision::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
    )
    .await?;
    let mut credentials = ClassCredentials::default();
    for (family, class) in [
        (
            WorkloadRoleFamily::ExecutorPlatform,
            AuthorityClass::ExecutorPlatform,
        ),
        (
            WorkloadRoleFamily::HttpAdmitter,
            AuthorityClass::CallableHttp,
        ),
    ] {
        let role = workload_login(
            family,
            WorkloadRoleIdentity::ProjectEnvironment {
                org: PROJECT_ENV_KEY,
                project: "test",
                environment: "test",
                database: database.name(),
            },
        )?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::login_sql(
                family,
                database.name(),
                &role,
                PASSWORD,
            ))
                .to_string(),
        ))
        .execute(&mut *admin)
        .await
        .map(drop)?;
        let mut login = url::Url::parse(database.url())?;
        login.set_username(&role).unwrap();
        login.set_password(Some(PASSWORD)).unwrap();
        credentials = credentials.with_class(class, login.to_string());
    }
    Ok(Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(
            HashMap::from([(
                "test".into(),
                ProjectConfig {
                    credentials,
                    guest_pool_max_size: 1,
                    platform_pool_max_size: 2,
                    wait_timeout_ms: 5000,
                    statement_timeout_ms: 10000,
                    row_limit: 1000,
                },
            )]),
            None,
        ),
    ))))
}

/// The applications an integration environment declares: `application`, with
/// every package of the release.
pub fn declared(
    application: &str,
    manifest: &wamn_catalog::ServingManifest,
) -> wamn_catalog::EnvironmentApplications {
    [(
        application.to_owned(),
        wamn_catalog::ApplicationDefinition {
            connections: std::collections::BTreeMap::new(),
            packages: manifest
                .release
                .packages
                .iter()
                .map(|package| package.package_id().to_owned())
                .collect(),
        },
    )]
    .into()
}

/// Author `document` as a minutely schedule workflow of `application` and
/// enable it, as an environment admin does with
/// `wamn-ctl workflow author --schedule '* * * * *'` and `enable`, then admit
/// its run due at `due` as the host's schedule task does, through `host` from
/// [`workflow_host`]. The run executes as the `workflow` user with `role`,
/// which holds exactly `operations`. Returns the run id.
#[expect(
    clippy::too_many_arguments,
    reason = "one fixture step over the project and control databases"
)]
pub async fn schedule_run(
    database: &wamn_test_postgres::Database,
    host: &WamnPostgres,
    manifest: &wamn_catalog::ServingManifest,
    application: &str,
    document: &wamn_catalog::WiringDocument,
    role: &str,
    operations: &[&str],
    due: &str,
) -> anyhow::Result<String> {
    use wamn_environment::author_workflow::{
        AuthorWorkflowRequest, WorkflowExecution, WorkflowTrigger, author_workflow_in_databases,
    };
    use wamn_environment::workflow_lifecycle::{WorkflowVersion, enable_workflow};
    use wamn_runtime::plugins::wamn_postgres::{ScheduleRunAdmission, ScheduleRunAdmitted};

    let (_, digest) =
        wamn_catalog::ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())?;
    let mut project = execution_role(database, role, operations).await?;

    let declared = declared(application, manifest);
    author_workflow_in_databases(
        database.url(),
        &AuthorWorkflowRequest {
            org: "rest",
            project: "publication",
            environment: "test",
            application,
            declared: &declared,
            document,
            trigger: WorkflowTrigger::Schedule("* * * * *"),
            execution: WorkflowExecution::Role(role),
            created_by: ENABLER,
            manifest_digest: digest.as_str(),
        },
    )
    .await?;
    enable_workflow(
        database.url(),
        &wamn_registry::Triple::new("rest", "test", application),
        &mut project,
        &WorkflowVersion {
            org: "rest",
            project: "publication",
            environment: "test",
            application,
            workflow_id: &document.wiring_id,
            version: document.version,
        },
        ENABLER,
        digest.as_str(),
        &declared,
        None,
    )
    .await?;
    match host
        .admit_schedule_run(
            "test",
            PROJECT_ENV_KEY,
            &ScheduleRunAdmission {
                environment: "test",
                application,
                application_packages: &declared[application].packages,
                workflow_id: &document.wiring_id,
                manifest_digest: digest.as_str(),
                due: chrono::DateTime::parse_from_rfc3339(due)?.with_timezone(&chrono::Utc),
            },
        )
        .await?
    {
        ScheduleRunAdmitted::Queued { run_id } => Ok(run_id),
        other => anyhow::bail!("the schedule admission queued no run: {other:?}"),
    }
}

/// The caller of one node of the schedule run `run`, which executes as the
/// `workflow` user with `role` active: the grants the role holds now, read
/// through `host` as the router reads them before each node.
pub async fn run_caller(
    admin: &mut Client,
    host: &WamnPostgres,
    run: &str,
    role: &str,
) -> anyhow::Result<wamn_engine::flow_http_routing::AuthenticatedCaller> {
    let row = sqlx::query(
        "SELECT execution_principal_id::text, execution_role FROM wamn_run.runs WHERE run_id=$1",
    )
    .bind(&run)
    .fetch_one(&mut *admin)
    .await?;
    let principal: String = row.get(0);
    let execution_role: String = row.get(1);
    anyhow::ensure!(
        principal == wamn_identity_protocol::workflow_user_id().to_string()
            && execution_role == role,
        "the schedule run executes as {principal} with {execution_role}, not the workflow user with {role}"
    );
    host.workflow_node_caller("test", PROJECT_ENV_KEY, &principal, &execution_role)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the workflow user does not hold {execution_role}"))
}

pub async fn install(
    admin: &mut Client,
    database: &wamn_test_postgres::Database,
    roots: &[PathBuf],
    manifest: &wamn_catalog::ServingManifest,
) -> anyhow::Result<String> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE wamn_app NOLOGIN; ").to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(("DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $$").to_string())).execute(&mut *admin).await.map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (wamn_catalog::CATALOG_SCHEMA_SQL).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (include_str!("../../../deploy/sql/app-schema.sql")).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
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
    let migration_database_url = wamn_control_provision::test_database::package_migration_input(
        database.url(),
        "rest",
        "publication",
        "test",
    )
    .await?;
    for package in &packages {
        apply_package(ApplyPackageRequest {
            migration_database_url: migration_database_url.clone(),
            component_source: wamn_environment::apply_package::component_source_from_env(None)
                .unwrap(),
            changes: wamn_broker::Publisher::default(),
            package: package.clone(),
            database_url: database.url().into(),
            org: "rest".into(),
            project: "publication".into(),
            environment: "test".into(),
        })
        .await?;
    }
    let role = workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: PROJECT_ENV_KEY,
            database: database.name(),
        },
    )?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::App,
            database.name(),
            &role,
            PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::platform_principals_sql("example.test")?).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    let system = wamn_control_provision::test_database::system();
    system.execute(&[
        "BEGIN",
        &wamn_control_provision::bind_platform_principal_sql(
            wamn_identity_protocol::PlatformComponent::Provisioning,
        ),
        "UPDATE registry.meta SET platform_domain = 'example.test'",
        "COMMIT",
    ])?;
    let manifests = roots
        .iter()
        .map(|root| {
            Ok(wamn_compiler::manifest::PackageManifest::from_slice(
                &std::fs::read(wamn_compiler::package_manifest_path(root))?,
            )?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    wamn_environment::environment::stage::materialize_application_principals(
        system.url(),
        database.url(),
        PROJECT_ENV_KEY,
        manifest,
        &manifests,
        &wamn_broker::Publisher::default(),
    )
    .await?;
    let privileges = sqlx::query("SELECT has_column_privilege($1, 'rest_consumer_fixture.item', 'key', 'SELECT'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'name', 'SELECT'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'key', 'INSERT'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'name', 'INSERT'), \
         has_table_privilege($1, 'platform.write_log', 'SELECT') \
           AND has_table_privilege($1, 'platform.write_log', 'INSERT') \
           AND has_table_privilege($1, 'platform.write_log', 'DELETE'), \
         has_column_privilege($1, 'platform.write_log', 'result', 'UPDATE'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'external_system_id', 'SELECT,INSERT'), \
         has_column_privilege($1, 'rest_consumer_fixture.item', 'external_id', 'SELECT,INSERT')").bind(&role).fetch_one(&mut *admin)
        .await?;
    assert_eq!(
        (0..8)
            .map(|index| privileges.get::<bool, _>(index))
            .collect::<Vec<_>>(),
        [true, false, true, true, true, true, true, true]
    );
    eprintln!(
        "consumer SQL role={role}, schema=rest_consumer_fixture; item SELECT(key,external_system_id,external_id), INSERT(key,name,external_system_id,external_id); write_log SELECT/INSERT/DELETE, UPDATE(result)"
    );
    let mut url = url::Url::parse(database.url())?;
    url.set_username(&role).unwrap();
    url.set_password(Some(PASSWORD)).unwrap();
    Ok(url.into())
}

pub async fn source(admin: &mut sqlx::PgConnection, name: &str) -> anyhow::Result<uuid::Uuid> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(("BEGIN").to_string()))
        .execute(&mut *admin)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    let id = sqlx::query(
        "INSERT INTO platform.external_system(id,name) VALUES (gen_random_uuid(),$1) RETURNING id",
    )
    .bind(&name)
    .fetch_one(&mut *admin)
    .await?
    .get(0);
    sqlx::raw_sql(sqlx::AssertSqlSafe(("COMMIT").to_string()))
        .execute(&mut *admin)
        .await
        .map(drop)?;
    Ok(id)
}

pub async fn invoke(
    database: &wamn_test_postgres::Database,
    guest_url: &str,
    manifest: &wamn_catalog::ServingManifest,
    admissions: &[ComponentAdmission],
) -> anyhow::Result<()> {
    use wamn_engine::artifact_source::LocalComponentSource;
    use wamn_engine::operation::native_workload::NativeComponent;
    use wamn_engine::release_manifest::{LoadedRelease, ReleaseTarget};
    use wamn_runtime::connection_credentials::ConnectionCredentials;
    use wamn_runtime::plugins::connection_http::transport::HttpTransport;
    use wamn_runtime::plugins::connection_http::{
        ConnectionExecutionClosure, InvocationEntry, WiringPosition,
    };
    use wash_runtime::plugin::wasi_logging::TracingLogger;

    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &manifest.canonical_bytes(),
        "consumer live",
        ReleaseTarget::new(PROJECT_ENV_KEY, "test")
            .with_applications(declared("rest_connector", manifest)),
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
        Arc::clone(&postgres),
        Arc::new(HttpTransport::new()?),
        Arc::new(wamn_runtime::connection_facts::ConnectionFacts::new(
            Arc::new(wamn_runtime::connection_memory::ConnectionMemory::default()),
            wamn_broker::Publisher::default(),
            Arc::new(ConnectionCredentials::empty()),
            postgres,
            PROJECT_ENV_KEY,
            "test",
            "test",
        )),
        Arc::new(TracingLogger::default()),
        Arc::from([]),
        release,
        Arc::new(LocalComponentSource::new(PathBuf::new())),
        OperationHostConfig {
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
        wamn_compiler::output_root(
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
    let mut admin = wamn_postgres::connect(database.url()).await?;

    let source_id = source(&mut admin, "rest-fixture").await?.to_string();
    // The rest_connector item is an environment workflow, authored, enabled and
    // admitted at one due moment (docs/architecture/execution.md#workflows).
    let document: wamn_catalog::WiringDocument = serde_json::from_slice(&std::fs::read(
        super::support::repository().join("apps/rest_connector/tests/workflows/rest_item.json"),
    )?)?;
    let workflow_host = workflow_host(&mut admin, database, manifest).await?;
    let run = schedule_run(
        database,
        &workflow_host,
        manifest,
        "rest_connector",
        &document,
        ROLE,
        &["rest-consumer-fixture:item/record"],
        "2026-10-21T12:02:00Z",
    )
    .await?;
    let caller = run_caller(&mut admin, &workflow_host, &run, ROLE).await?;
    wamn_engine::native_routes::authorize_registered_operation(Some(&caller), Some(OPERATION))?;
    let finish_operation = "rest-connector:sync/finish@0.1.1";
    let connector = admissions
        .iter()
        .find(|admission| admission.component() == "rest_connector")
        .unwrap()
        .facts();
    let connector_bytes = std::fs::read(
        wamn_compiler::output_root(&super::support::repository().join("apps/rest_connector"))
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
            json!([{"request_id":request, "value":{"external_system_id":source_id,"idempotency_key":key, "items":[{"key":"native-item", "name":name}]}}])
        };
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
                    identity: host.identity(PROJECT_ENV_KEY, Some(host.release_identity())),
                    invocation: wamn_execution_host::component_invocation(
                        component,
                        operation,
                        InvocationEntry::Wiring(WiringPosition {
                            configuration: Default::default(),
                            package_id: "rest_connector".into(),
                            wiring_hash: format!("sha256:{}", "a".repeat(64)),
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
        let result = invoke_operation(&host, call).await?;
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
