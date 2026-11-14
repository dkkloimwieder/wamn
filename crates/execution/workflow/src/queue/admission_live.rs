//! Manual and event workflow admission through the real queue.

use super::{QUEUE_LEASE_INVOCATION, QueueTarget, drain_one};
use serde_json::json;
use sqlx::Row as _;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use wamn_catalog::{ComponentDeclaration, ServingManifest, WiringDocument};
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};
use wamn_engine::artifact_source::{LocalComponentSource, local_component_path};
use wamn_engine::component_admission::{ComponentAdmissionRequest, validate_component_admission};
use wamn_engine::engine::build_engine;
use wamn_engine::flow_http_routing::CredentialType;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_postgres::schema::BareSchemaName;
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::wamn_jetstream::WamnJetstream;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, ClassCredentials, EventBatchAdmission, ProjectConfig, ReleaseIdentity,
    SessionIdentity, StaticCredentialProvider, WamnPostgres,
};
use wash_runtime::host::probes::Liveness;

use crate::{PostgresWorkflows, RouterDriver, WorkflowErrorType};
use wamn_execution_host::{OperationHost, OperationHostConfig};
use wamn_identity_protocol::PlatformComponent;

#[path = "admission_live/shutdown.rs"]
mod shutdown;

const PROJECT_ENV_KEY: &str = "automation-live";
const SERVICE: &str = "00000000-0000-4000-8000-000000000074";
const OPERATION: &str = "automation:echo/run@1.0.0";
/// The stable reference that an authored role stores for [`OPERATION`].
const REFERENCE: &str = "automation:echo/run";
const HASH: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

const NODE_TYPES: &str = r#"
      (import "wamn:node/types@0.1.0" (instance $node
        (type $json' string)
        (export "json" (type $json (eq $json')))
        (type $context' (record
          (field "wiring-id" string) (field "wiring-version" u32)
          (field "node-id" string) (field "delivery-id" string)
          (field "input-port" (option string)) (field "occurrence" u32)
          (field "traceparent" (option string)) (field "tracestate" (option string))
          (field "deadline-ms" (option u64)) (field "config" $json)))
        (export "node-context" (type $context (eq $context')))
        (type $detail' (record (field "message" string) (field "code" (option string))))
        (export "error-detail" (type $detail (eq $detail')))
        (type $rate' (record (field "detail" $detail) (field "retry-after-ms" (option u64))))
        (export "rate-limit-detail" (type $rate (eq $rate')))
        (type $error' (variant (case "retryable" $detail) (case "rate-limited" $rate)
          (case "terminal" $detail) (case "invalid-input" $detail) (case "cancelled")))
        (export "node-error" (type $error (eq $error')))
        (type $emission' (record (field "payload" $json) (field "port" (option string))))
        (export "emission" (type $emission (eq $emission')))))
      (alias export $node "json" (type $json))
      (alias export $node "node-context" (type $context))
      (alias export $node "node-error" (type $error))
      (alias export $node "emission" (type $emission))
"#;

fn component_bytes() -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component
      {NODE_TYPES}
      (core module $memory
        (memory (export "memory") 16)
        (global $next (mut i32) (i32.const 1024))
        (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
          (local $old i32) global.get $next local.tee $old
          local.get $size i32.add global.set $next local.get $old))
      (core instance $memory (instantiate $memory))
      (core module $main
        (import "memory" "memory" (memory 16))
        (func (export "run") (param $input i32) (result i32)
          i32.const 264 local.get $input i32.load offset=96 i32.store
          i32.const 268 local.get $input i32.load offset=100 i32.store
          i32.const 256))
      (core instance $main (instantiate $main (with "memory" (instance $memory))))
      (func $run (param "ctx" $context) (param "input" $json)
        (result (result $emission (error $error)))
        (canon lift (core func $main "run") (memory $memory "memory")
          (realloc (func $memory "realloc"))))
      (instance $handler
        (export "json" (type $json)) (export "node-context" (type $context))
        (export "node-error" (type $error)) (export "emission" (type $emission))
        (export "run" (func $run)))
      (export "{OPERATION}" (instance $handler)))"#
    ))
    .unwrap()
}

enum Mode {
    Shutdown,
    Event,
}

async fn run_current(mode: Mode) -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let mut admin = wamn_postgres::connect(database.url()).await?;

    let schema = BareSchemaName::new("wamn_run")?;
    sqlx::raw_sql("CREATE SCHEMA application_data")
        .execute(&mut admin)
        .await
        .map(drop)?;
    wamn_environment::platform_migrations::install_project(&mut admin).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::platform_principals_sql("automation.invalid")?).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::query("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:automation-fixture',false)").bind(&PlatformComponent::Provisioning.principal_id().to_string()).execute(&mut admin).await.map(|result| result.rows_affected())?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("INSERT INTO platform.\"user\" (id,type,email) VALUES ('{SERVICE}','service','automation@example.invalid');
      INSERT INTO platform.role (name) VALUES ('automation');
      INSERT INTO platform.user_role (user_id,role_name) VALUES ('{SERVICE}','automation');
      INSERT INTO platform.permission (role_name,permission) VALUES ('automation','{REFERENCE}'), ('automation','wamn-control:workflow/start');
      INSERT INTO catalog.packages ( project_env_key,package_id,package_version,artifact_digest) VALUES ( '{PROJECT_ENV_KEY}','automation','1.0.0','{HASH}');"))).execute(&mut admin).await.map(drop)?;
    let engine = Arc::new(build_engine(&[])?);
    let bytes = match mode {
        Mode::Shutdown => shutdown::component_bytes(),
        Mode::Event => component_bytes(),
    };
    let declaration: ComponentDeclaration = serde_json::from_value(json!({
        "target": {"project_env_key":PROJECT_ENV_KEY,"package-id":"automation","package-version":"1.0.0"},
        "component":"echo","interface-version":"0.1.0",
        "operations":{(OPERATION):{"registered-operation":OPERATION,
            "input-ports":[{"name":"input","schema":{}}],
            "output-ports":[{"name":"main","schema":{}}],
            "parameters":[{"name":"deadline-ms","schema":{"type":"integer"},"required":false}]}},
        "connections":[]
    }))?;
    let admitted = validate_component_admission(
        &engine,
        &bytes,
        ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages: BTreeSet::from(["wamn:node".to_owned()]),
            base_digests: BTreeSet::new(),
        },
    )?
    .component;
    let document = WiringDocument::parse(&json!({
        "format-version":"0.1", "wiring-id":"echo", "version":1,"entry":"echo",
        "nodes":{"echo":{"component":&admitted.component,"interface-version":"0.1.0","operation":OPERATION,"params":{"deadline-ms":60000}}},
        "edges":[]
    }))?;
    let graph_hash = document.wiring_hash();
    let manifest: ServingManifest = serde_json::from_value(json!({
        "format-version":wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        "release":{"packages":[{"package-id":"automation","package-version":"1.0.0"}]},
        "components":[{"package-id":"automation",
          "descriptor": wamn_catalog::ComponentDescriptor::new(admitted, Vec::new())}],
        "routes":[],"attachments":{},"host-routes":["application"]
    }))?;
    let workflow_user = wamn_identity_protocol::workflow_user_id().to_string();
    sqlx::query(
        "INSERT INTO platform.user_role (user_id, role_name) VALUES ($1::text::uuid, 'automation')",
    )
    .bind(&workflow_user)
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO catalog.workflows (project_env_key, environment, application, workflow_id, version, graph_json, wiring_hash, trigger, source_package, entity, ops, enable_sequence, execution_type, execution_role, enabled, created_by) VALUES ($1, 'test', 'automation', 'echo', 1, $2::text::jsonb, $3, 'event', 'automation', 'movement', ARRAY['insert'], 0, 'role', 'automation', true, $4::text::uuid)").bind(&PROJECT_ENV_KEY).bind(&serde_json::to_string(&document)?).bind(&graph_hash.as_str()).bind(&workflow_user).execute(&mut admin).await.map(|result| result.rows_affected())?;
    let mut manual = document;
    manual.wiring_id = "echo-manual".into();
    sqlx::query("INSERT INTO catalog.workflows (project_env_key, environment, application, workflow_id, version, graph_json, wiring_hash, trigger, execution_type, enabled, created_by) VALUES ($1, 'test', 'automation', 'echo-manual', 1, $2::text::jsonb, $3, 'manual', 'invoker', true, $4::text::uuid)").bind(&PROJECT_ENV_KEY).bind(&serde_json::to_string(&manual)?).bind(&manual.wiring_hash().as_str()).bind(&SERVICE).execute(&mut admin).await.map(|result| result.rows_affected())?;
    let canonical = manifest.canonical_bytes();
    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &canonical,
        "automation-live",
        wamn_engine::release_manifest::ReleaseTarget::new(PROJECT_ENV_KEY, "test")
            .with_applications(
                [(
                    "automation".to_owned(),
                    wamn_catalog::ApplicationDefinition {
                        connections: std::collections::BTreeMap::new(),
                        packages: ["automation".to_owned()].into(),
                    },
                )]
                .into(),
            ),
    )?);
    sqlx::query("INSERT INTO catalog.releases (project_env_key,manifest_digest,canonical_bytes) VALUES ($1,$2,$3)").bind(&PROJECT_ENV_KEY).bind(&release.release().manifest_digest.as_str()).bind(&canonical).execute(&mut admin).await.map(|result| result.rows_affected())?;
    let mut credentials = ClassCredentials::default();
    let db_name: String = sqlx::query("SELECT current_database()")
        .fetch_one(&mut admin)
        .await?
        .get(0);
    for (family, class) in [
        (WorkloadRoleFamily::App, AuthorityClass::GuestSql),
        (
            WorkloadRoleFamily::ExecutorPlatform,
            AuthorityClass::ExecutorPlatform,
        ),
        (
            WorkloadRoleFamily::HttpAdmitter,
            AuthorityClass::CallableHttp,
        ),
    ] {
        let role_target = if family == WorkloadRoleFamily::App {
            WorkloadRoleIdentity::ProjectEnvKey {
                project_env_key: PROJECT_ENV_KEY,
                database: &db_name,
            }
        } else {
            WorkloadRoleIdentity::ProjectEnvironment {
                org: PROJECT_ENV_KEY,
                project: "default",
                environment: "test",
                database: &db_name,
            }
        };
        let role = workload_login(family, role_target)?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::login_sql(
                family,
                &db_name,
                &role,
                "automation-test-only",
            ))
                .to_string(),
        ))
        .execute(&mut admin)
        .await
        .map(drop)?;
        // Ambient application lookup must not select the queue's storage.
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER ROLE \"{role}\" SET search_path = application_data"
        )))
        .execute(&mut admin)
        .await
        .map(drop)?;
        let mut url = url::Url::parse(database.url())?;
        url.set_username(&role).unwrap();
        url.set_password(Some("automation-test-only")).unwrap();
        credentials = credentials.with_class(class, url.to_string());
    }
    let provider = StaticCredentialProvider::new(
        std::collections::HashMap::new(),
        Some(ProjectConfig {
            credentials,
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 5000,
            statement_timeout_ms: 10000,
            row_limit: 100,
        }),
    );
    let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(provider)));
    postgres
        .bind_session_identity(
            QUEUE_LEASE_INVOCATION,
            &SessionIdentity {
                project_env_key: PROJECT_ENV_KEY.to_owned(),
                project: Some("default".to_owned()),
                schema: Some("wamn_run".to_owned()),
                runner: Some("automation-live".to_owned()),
                user_id: Some(PlatformComponent::Executor.principal_id().to_string()),
                operation: Some(PlatformComponent::Executor.principal_name()),
                release: Some(ReleaseIdentity::for_test(
                    release.release().manifest_digest.clone(),
                )),
                ..SessionIdentity::default()
            },
        )
        .await?;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    std::fs::write(
        local_component_path(
            scratch.path(),
            &release
                .manifest()
                .components
                .first()
                .expect("one admitted component")
                .descriptor
                .component_digest,
        )?,
        &bytes,
    )?;
    let (logging, capture) = WamnLogging::new_with_capture(
        &wamn_runtime::plugins::wamn_logging::WamnLoggingConfig::default(),
    )?;
    let logging = Arc::new(logging);
    let driver = Arc::new(RouterDriver::new(Arc::new(OperationHost::new(
        Arc::clone(&engine),
        Arc::clone(&postgres),
        Arc::new(HttpTransport::new()?),
        Arc::new(wamn_runtime::connection_facts::ConnectionFacts::new(
            Arc::new(wamn_runtime::connection_memory::ConnectionMemory::default()),
            wamn_broker::Publisher::default(),
            Arc::new(ConnectionCredentials::empty()),
            Arc::clone(&postgres),
            PROJECT_ENV_KEY,
            "default",
            "test",
        )),
        Arc::clone(&logging),
        Arc::from([]),
        Arc::clone(&release),
        Arc::new(LocalComponentSource::new(scratch.path().to_owned())),
        OperationHostConfig {
            project: "default".to_owned(),
            schema: Some("application_data".to_owned()),
            owner_prefix: "automation-live".to_owned(),
            warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
        },
    )?)));
    let target = QueueTarget {
        project_env_key: PROJECT_ENV_KEY.to_owned(),
        project: "default".to_owned(),
        package_ids: vec!["automation".to_owned()],
        environment: "test".to_owned(),
    };
    let jetstream = Arc::new(WamnJetstream::from_env());
    let client = wamn_postgres::connect(database.url()).await?;

    let workflows = WorkflowsFixture {
        runs: PostgresWorkflows::new(client, schema.as_str(), PROJECT_ENV_KEY, "test"),
        postgres: Arc::clone(&postgres),
        host: wamn_execution_host::HostRouteDelivery::new(
            Arc::clone(&release),
            wamn_execution_host::HostRouteHandlers::Application {
                postgres: Arc::clone(&postgres),
                project: "default".into(),
                identity: None,
                changes: wamn_broker::Publisher::default(),
            },
        ),
    };
    let request = StartInput {
        package_id: "automation".to_owned(),
        wiring_id: "echo-manual".to_owned(),
        idempotency_key: "shutdown".to_owned(),
        input: json!({"queued":true}),
    };
    match mode {
        Mode::Shutdown => {
            shutdown::run(
                &mut admin,
                &workflows,
                &request,
                shutdown::Execution {
                    driver: &driver,
                    postgres: &postgres,
                    engine: &engine,
                    logging: &logging,
                    capture: &capture,
                    target: &target,
                    jetstream: &jetstream,
                },
            )
            .await
        }
        Mode::Event => {
            event_run(
                &mut admin, &workflows, &release, &driver, &postgres, &target, &jetstream,
            )
            .await
        }
    }
}

struct StartInput {
    package_id: String,
    wiring_id: String,
    idempotency_key: String,
    input: serde_json::Value,
}

struct WorkflowsFixture {
    runs: PostgresWorkflows,
    postgres: Arc<WamnPostgres>,
    host: wamn_execution_host::HostRouteDelivery,
}

impl std::ops::Deref for WorkflowsFixture {
    type Target = PostgresWorkflows;
    fn deref(&self) -> &Self::Target {
        &self.runs
    }
}

impl WorkflowsFixture {
    async fn start(&self, request: &StartInput) -> anyhow::Result<String> {
        use wamn_engine::native_routes::{DeliveryRequest, Source};
        let attachment = wamn_catalog::HostRouteSet::Application
            .attachments()
            .find(|(_, attachment)| attachment.reference == "wamn-control:workflow/start")
            .map(|(id, _)| id.to_owned())
            .unwrap();
        let held = self
            .postgres
            .user_operation_permissions("default", PROJECT_ENV_KEY, &SERVICE.parse()?)
            .await?;
        let active_role = held.pat_role();
        let caller = wamn_engine::flow_http_routing::AuthenticatedCaller::new(
            attachment.clone(),
            SERVICE,
            CredentialType::Pat,
            held.admin,
            held.references.into_iter().collect(),
            active_role,
        );
        let report = self
            .host
            .deliver(
                DeliveryRequest {
                    source: Source::Attachment(attachment),
                    delivery_id: request.idempotency_key.clone(),
                    payload: json!([{"request_id": request.idempotency_key, "value": {
                        "application": request.package_id, "workflow_id": request.wiring_id,
                        "input_json": request.input, "idempotency_key": request.idempotency_key,
                    }}])
                    .to_string(),
                    caller: None,
                    trace: None,
                    parent_causation: None,
                    if_none_match: None,
                },
                Some(caller),
            )
            .await;
        let Ok(body) = report.outcome else {
            anyhow::bail!("workflow.start failed: {:?}", report.outcome);
        };
        let body: serde_json::Value = serde_json::from_str(&body)?;
        body[0]["value"]["run_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("workflow.start refused: {body}"))
    }
}

async fn event_run(
    admin: &mut sqlx::PgConnection,
    workflows: &WorkflowsFixture,
    release: &LoadedRelease,
    driver: &RouterDriver,
    postgres: &WamnPostgres,
    target: &QueueTarget,
    jetstream: &WamnJetstream,
) -> anyhow::Result<()> {
    let liveness = Liveness::new(Duration::from_secs(90));
    let envelope = json!({"op": "insert", "new": {"id": "m-2", "value": 7},
        "package_id": "automation", "entity": "movement", "table": "movement", "lsn": 7,
        "txid": 1, "commit_ts": "2026-10-25T00:00:00Z"})
    .to_string();
    let source_id = wamn_event_wire::msg_id(&target.project, "test", 7);
    let mut admission = EventBatchAdmission {
        source_package: "automation",
        entity: "movement",
        envelope: &envelope,
        sequence: 7,
        source_event_id: &source_id,
    };
    crate::event_admission::admit_event_batch(postgres, &target.project, release, &admission)
        .await?;
    crate::event_admission::admit_event_batch(postgres, &target.project, release, &admission)
        .await?;
    let runs = sqlx::query("SELECT run_id FROM wamn_run.runs WHERE trigger_source = 'event'")
        .fetch_all(&mut *admin)
        .await?;
    assert_eq!(runs.len(), 1, "redelivery admits no second run");
    let run: String = runs[0].get(0);
    let mut other: serde_json::Value = serde_json::from_str(&envelope)?;
    other["new"]["id"] = "m-3".into();
    let other = other.to_string();
    admission.envelope = &other;
    assert!(
        crate::event_admission::admit_event_batch(postgres, &target.project, release, &admission)
            .await
            .is_err(),
        "changed input must conflict"
    );

    let listed = workflows.list(10).await?;
    let entry = listed
        .iter()
        .find(|entry| entry.run_id == run)
        .expect("list shows the event run");
    assert_eq!(entry.trigger_source.as_deref(), Some("event"));
    assert!(entry.queued && !entry.parked);
    workflows.park(&run).await?;
    assert!(
        !drain_one(driver, postgres, jetstream, target, 30_000, &liveness).await?,
        "no lease takes a parked event run"
    );
    workflows.release(&run).await?;
    assert!(
        drain_one(driver, postgres, jetstream, target, 30_000, &liveness).await?,
        "the queue leases the released event run"
    );
    let stored = sqlx::query(
        "SELECT status, result_json::text, trigger_source, execution_principal_id::text FROM wamn_run.runs WHERE run_id = $1",
    )
    .bind(&run)
    .fetch_one(&mut *admin)
    .await?;
    assert_eq!(stored.get::<String, _>(0), "completed");
    let result: serde_json::Value = serde_json::from_str(&stored.get::<String, _>(1))?;
    assert_eq!(result["new"]["id"], json!("m-2"));
    assert_eq!(result["new"]["value"], json!(7));
    assert_eq!(stored.get::<String, _>(2), "event");
    assert_eq!(
        stored.get::<String, _>(3),
        wamn_identity_protocol::workflow_user_id().to_string()
    );
    assert!(
        stored.get::<bool, _>(4),
        "an event run has no service principal"
    );
    assert_eq!(
        workflows.park(&run).await.unwrap_err().error_type(),
        WorkflowErrorType::NotParkable
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn event_admission_is_idempotent_and_respects_park_and_release() -> anyhow::Result<()> {
    run_current(Mode::Event).await
}
