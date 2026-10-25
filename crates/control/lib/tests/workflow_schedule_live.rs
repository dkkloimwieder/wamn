//! Schedule workflows from rows, live (docs/plan/workflows.md R3, R8, epic 2
//! issues 1 and 2): a schedule trigger authored with its cron, the enable rule
//! of a schedule, the host-owned admission of a due moment, and the host's
//! schedule task, which reads the enabled rows at each tick.
//!
//! The fixture release carries one package, `orders`, with one read
//! operation (a WAT component that answers fixed rows and writes nothing). A
//! one-node workflow over the read is authored through the code path of
//! `wamn-ctl workflow author --schedule`, enabled, and admitted with
//! `WamnPostgres::admit_schedule_run` as the host's schedule task will admit
//! it. The real queue service of the release drains it through the router,
//! under the scoped executor and application logins.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{
    ComponentDeclaration, ComponentDescriptor, SERVING_MANIFEST_FORMAT_VERSION, ServingManifest,
    WiringDocument,
};
use wamn_control::author_workflow::{
    AuthorWorkflowError, AuthorWorkflowErrorType, AuthorWorkflowRequest, WorkflowExecution,
    WorkflowTrigger, author_workflow_in_databases,
};
use wamn_control::workflow_lifecycle::{
    WorkflowLifecycleError, WorkflowLifecycleErrorType, WorkflowVersion, disable_workflow,
    enable_workflow,
};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_control_registry::Triple;
use wamn_engine::artifact_source::{LocalComponentSource, local_component_path};
use wamn_engine::component_admission::{ComponentAdmissionRequest, validate_component_admission};
use wamn_engine::engine::build_engine;
use wamn_engine::release_manifest::{LoadedRelease, ReleaseScope};
use wamn_execution_host::{OperationHost, OperationScope};
use wamn_project_state::{PlatformComponent, workflow_user_id};
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_jetstream::WamnJetstream;
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, ClassCredentials, ScheduleRunAdmission, ScheduleRunAdmitted, WamnPostgres,
    WamnPostgresConfig,
};
use wamn_schema_control::BareSchemaName;
use wamn_workflow::{QueueService, QueueServiceConfig, RouterDriver};

#[path = "workflow_schedule_live/event_batch.rs"]
mod event_batch;

const TENANT: &str = "schedule-live";
const PROJECT: &str = "default";
const ENVIRONMENT: &str = "dev";
const APPLICATION: &str = "orders";
/// The application the environment declares, with the package `orders`. Its
/// name is no package (docs/plan/platform-config.md).
const APP: &str = "erp";
const PASSWORD: &str = "schedule-live-test-only";
const READ: &str = "orders:order/read@1.0.0";
const READ_REFERENCE: &str = "orders:order/read";
const CRON: &str = "*/5 * * * *";
/// A user who holds one role, `reader`, granted the read.
const READER: &str = "00000000-0000-4000-8000-000000000201";
/// A user who holds `admin`.
const ADMIN: &str = "00000000-0000-4000-8000-000000000202";
const ARTIFACT_DIGEST: &str =
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The rows the read answers.
const ROWS: &str = r#"{"rows":[{"id":"o-1","total":1250,"status":"open"}]}"#;

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

/// The read: a node that ignores its input and emits [`ROWS`] from a data
/// segment. It imports nothing, so it can write nothing.
fn read_component() -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component
      {NODE_TYPES}
      (core module $memory
        (memory (export "memory") 16)
        (data (i32.const 512) "{data}")
        (global $next (mut i32) (i32.const 1024))
        (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
          (local $old i32) global.get $next local.tee $old
          local.get $size i32.add global.set $next local.get $old))
      (core instance $memory (instantiate $memory))
      (core module $main
        (import "memory" "memory" (memory 16))
        (func (export "run") (param $input i32) (result i32)
          i32.const 264 i32.const 512 i32.store
          i32.const 268 i32.const {length} i32.store
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
      (export "{READ}" (instance $handler)))"#,
        data = ROWS.replace('"', "\\\""),
        length = ROWS.len(),
    ))
    .expect("the read component is valid WAT")
}

fn read_declaration() -> Value {
    json!({
        "scope": {"tenant-id": TENANT, "package-id": APPLICATION, "package-version": "1.0.0"},
        "component": "orders_data", "interface-version": "0.1.0",
        "operations": {(READ): {"registered-operation": READ,
            "input-ports": [{"name": "input", "schema": {}}],
            "output-ports": [{"name": "main", "schema": {}}],
            "parameters": []}},
        "connections": []
    })
}

/// The one-node workflow `id` over the read.
fn workflow(id: &str) -> WiringDocument {
    WiringDocument::parse(&json!({
        "format-version": "0.1", "wiring-id": id, "version": 1, "entry": "read",
        "nodes": {"read": {"component": "orders_data", "interface-version": "0.1.0",
            "operation": READ, "params": {}}},
        "edges": [], "cases": []
    }))
    .expect("the workflow graph is structurally valid")
}

fn due(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

struct Fixture {
    admin: Client,
    url: String,
    control: Client,
    control_url: String,
    engine: Arc<wash_runtime::engine::Engine>,
    postgres: Arc<WamnPostgres>,
    /// The executor login, which the host's schedule task connects with.
    executor_url: String,
    logging: Arc<WamnLogging>,
    scratch: wamn_test_infrastructure::scratch::ScratchRoot,
    read: wamn_catalog::AdmittedComponent,
    release: Arc<LoadedRelease>,
    digest: String,
    _databases: (wamn_test_postgres::Database, wamn_test_postgres::Database),
}

async fn fixture() -> Fixture {
    let database = wamn_test_postgres::database();
    let control_database = wamn_control_provision::test_database::system();
    let url = database.url().to_owned();
    let admin = connect(&url).await;
    let schema = BareSchemaName::new("wamn_run").unwrap();
    admin
        .batch_execute("CREATE SCHEMA application_data")
        .await
        .unwrap();
    wamn_control::reconcile_run_plane::reconcile(&admin, &schema, true)
        .await
        .expect("install the run plane and the catalog");
    admin
        .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
        .await
        .unwrap();
    admin
        .batch_execute(
            &wamn_control_provision::platform_principals_sql("schedule.invalid").unwrap(),
        )
        .await
        .expect("write the platform principals and the workflow user");
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:schedule-live-fixture', false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await
        .unwrap();
    admin
        .batch_execute(&format!(
            "INSERT INTO platform.\"user\" (id, type, email) VALUES \
               ('{READER}', 'user', 'reader@schedule.invalid'), \
               ('{ADMIN}', 'user', 'admin@schedule.invalid');
             INSERT INTO platform.role (tenant_id, name) VALUES \
               ('{TENANT}', 'reader'), ('{TENANT}', 'clerk'), ('{TENANT}', 'admin');
             INSERT INTO platform.user_role (tenant_id, user_id, role_name) VALUES \
               ('{TENANT}', '{READER}', 'reader'), ('{TENANT}', '{ADMIN}', 'admin');
             INSERT INTO platform.permission (tenant_id, role_name, permission) VALUES \
               ('{TENANT}', 'reader', '{READ_REFERENCE}');
             INSERT INTO platform.environment (tenant_id, status) VALUES ('{TENANT}', 'active');
             INSERT INTO wamn_run.environment_policies (tenant_id, expected_environment, durability_class) \
               VALUES ('{TENANT}', '{ENVIRONMENT}', 'standard');
             INSERT INTO catalog.packages (tenant_id, package_id, package_version, artifact_digest) \
               VALUES ('{TENANT}', '{APPLICATION}', '1.0.0', '{ARTIFACT_DIGEST}');"
        ))
        .await
        .expect("seed the users, roles, permissions and the environment");

    let engine = Arc::new(build_engine(&[]).unwrap());
    let read_bytes = read_component();
    let declaration: ComponentDeclaration = serde_json::from_value(read_declaration()).unwrap();
    let read = validate_component_admission(
        &engine,
        &read_bytes,
        ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages: BTreeSet::from(["wamn:node".to_owned()]),
            base_digests: BTreeSet::new(),
        },
    )
    .expect("admit the read component")
    .component;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    std::fs::write(
        local_component_path(scratch.path(), &read.component_digest).unwrap(),
        &read_bytes,
    )
    .unwrap();

    let served: ServingManifest = serde_json::from_value(json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"packages": [{"package-id": APPLICATION, "package-version": "1.0.0"}]},
        "components": [{"package-id": APPLICATION,
            "descriptor": ComponentDescriptor::new(read.clone(), Vec::new()),
            "operations": {(READ): {"registered-operation": READ, "permissions": [READ],
                "statements": {}}}}],
        "routes": [], "attachments": {},
    }))
    .expect("the fixture release is valid");
    let release = Arc::new(
        LoadedRelease::load_canonical_bytes(
            &served.canonical_bytes(),
            "schedule-live",
            ReleaseScope::new(TENANT, ENVIRONMENT).with_applications(declared()),
        )
        .expect("the fixture release loads"),
    );
    let digest = release.release().manifest_digest.as_str().to_owned();
    admin
        .execute(
            "INSERT INTO catalog.releases (tenant_id, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3)",
            &[&TENANT, &digest, &served.canonical_bytes()],
        )
        .await
        .expect("install the serving release");

    let (postgres, executor_url) = scoped_postgres(&admin, &url).await;
    let (logging, _capture) = WamnLogging::new_with_capture(&WamnLoggingConfig::default()).unwrap();
    let control = connect(control_database.url()).await;
    control
        .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .unwrap();

    Fixture {
        admin,
        url,
        control,
        control_url: control_database.url().to_owned(),
        engine,
        postgres,
        executor_url,
        logging: Arc::new(logging),
        scratch,
        read,
        release,
        digest,
        _databases: (database, control_database),
    }
}

/// The host's database access under the scoped logins of its families: the
/// application login for guest SQL, the executor for admission and the
/// queue, and the admitter for permission reads. Also the executor login.
async fn scoped_postgres(admin: &Client, url: &str) -> (Arc<WamnPostgres>, String) {
    let database: String = admin
        .query_one("SELECT current_database()", &[])
        .await
        .unwrap()
        .get(0);
    let mut credentials = ClassCredentials::default();
    let mut executor_url = String::new();
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
        let scope = if family == WorkloadRoleFamily::App {
            WorkloadRoleScope::Tenant {
                tenant: TENANT,
                database: &database,
            }
        } else {
            WorkloadRoleScope::ProjectEnvironment {
                org: TENANT,
                project: PROJECT,
                environment: ENVIRONMENT,
                database: &database,
            }
        };
        let role = workload_generation_role(family, scope, CredentialGeneration::A).unwrap();
        admin
            .batch_execute(&sql::prepare_workload_generation_sql(
                family,
                &database,
                &role,
                PASSWORD,
                "2099-01-01T00:00:00Z",
            ))
            .await
            .expect("prepare the scoped login");
        admin
            .batch_execute(&format!(
                "ALTER ROLE \"{role}\" SET search_path = application_data"
            ))
            .await
            .unwrap();
        let mut login = url::Url::parse(url).unwrap();
        login.set_username(&role).unwrap();
        login.set_password(Some(PASSWORD)).unwrap();
        if family == WorkloadRoleFamily::ExecutorPlatform {
            executor_url = login.to_string();
        }
        credentials = credentials.with_class(class, login.to_string());
    }
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(credentials),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 5000,
            statement_timeout_ms: 10000,
            row_limit: 100,
        })
        .unwrap(),
    );
    (postgres, executor_url)
}

/// The applications the fixture environment declares: `erp`, with the one
/// package of its release.
fn declared() -> wamn_catalog::EnvironmentApplications {
    [(
        APP.to_owned(),
        wamn_catalog::ApplicationDefinition {
            packages: [APPLICATION.to_owned()].into(),
        },
    )]
    .into()
}

impl Fixture {
    /// Author version 1 of `id` as `wamn-ctl workflow author` does, with a
    /// green gate report under its hash.
    async fn author(
        &self,
        id: &str,
        trigger: WorkflowTrigger<'_>,
        execution: WorkflowExecution<'_>,
    ) -> anyhow::Result<()> {
        self.author_in(id, trigger, execution, &declared()).await
    }

    /// [`Self::author`] in an environment that declares `declared`.
    async fn author_in(
        &self,
        id: &str,
        trigger: WorkflowTrigger<'_>,
        execution: WorkflowExecution<'_>,
        declared: &wamn_catalog::EnvironmentApplications,
    ) -> anyhow::Result<()> {
        let document = workflow(id);
        self.control
            .execute(
                "INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary) \
                 VALUES ($1, $2, true, '{}'::jsonb) ON CONFLICT DO NOTHING",
                &[&TENANT, &document.wiring_hash().as_str()],
            )
            .await
            .expect("record a green gate report");
        author_workflow_in_databases(
            &self.url,
            &self.control_url,
            &AuthorWorkflowRequest {
                tenant_id: TENANT,
                environment: ENVIRONMENT,
                application: APP,
                declared,
                document: &document,
                trigger,
                execution,
                created_by: READER,
                manifest_digest: &self.digest,
            },
        )
        .await
        .map(|_| ())
    }

    async fn enable(&mut self, id: &str, enabler: &str) -> Result<(), WorkflowLifecycleError> {
        enable_workflow(
            &self.url,
            &Triple::new("acme", APPLICATION, ENVIRONMENT),
            &mut self.admin,
            &WorkflowVersion {
                tenant_id: TENANT,
                environment: ENVIRONMENT,
                application: APP,
                workflow_id: id,
                version: 1,
            },
            enabler,
            &self.digest,
            &declared(),
            None,
        )
        .await
    }

    async fn disable(&mut self, id: &str) -> Result<(), WorkflowLifecycleError> {
        disable_workflow(
            &self.url,
            &Triple::new("acme", APPLICATION, ENVIRONMENT),
            &mut self.admin,
            &WorkflowVersion {
                tenant_id: TENANT,
                environment: ENVIRONMENT,
                application: APP,
                workflow_id: id,
                version: 1,
            },
        )
        .await
    }

    /// The due moments of the schedule runs of `id`, oldest first.
    async fn dues(&self, id: &str) -> Vec<DateTime<Utc>> {
        self.admin
            .query(
                "SELECT input_json->>'due' FROM wamn_run.runs \
                  WHERE trigger_source = 'schedule' AND wiring_id = $1 ORDER BY 1",
                &[&id],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| due(&row.get::<_, String>(0)))
            .collect()
    }

    async fn grant_workflow_user(&self, role: &str) {
        self.admin
            .execute(
                "INSERT INTO platform.user_role (tenant_id, user_id, role_name) \
                 VALUES ($1, $2::text::uuid, $3)",
                &[&TENANT, &workflow_user_id().to_string(), &role],
            )
            .await
            .expect("grant the workflow user a role");
    }

    async fn admit(&self, id: &str, at: &str) -> ScheduleRunAdmitted {
        self.postgres
            .admit_schedule_run(
                PROJECT,
                TENANT,
                &ScheduleRunAdmission {
                    environment: ENVIRONMENT,
                    application: APP,
                    application_packages: &declared()[APP].packages,
                    workflow_id: id,
                    manifest_digest: &self.digest,
                    due: due(at),
                },
            )
            .await
            .expect("the schedule admission answers")
    }

    async fn queued(&self, id: &str, at: &str) -> String {
        match self.admit(id, at).await {
            ScheduleRunAdmitted::Queued { run_id } => run_id,
            other => panic!("the schedule admission queues a run: {other:?}"),
        }
    }

    async fn runs(&self) -> i64 {
        self.admin
            .query_one("SELECT count(*) FROM wamn_run.runs", &[])
            .await
            .unwrap()
            .get(0)
    }

    /// Bind the real queue service to the release and drain its runs.
    async fn drain(&self) {
        let driver = Arc::new(RouterDriver::new(Arc::new(
            OperationHost::new(
                Arc::clone(&self.engine),
                Arc::clone(&self.postgres),
                Arc::new(HttpTransport::new().unwrap()),
                Arc::new(WamnCredentials::empty()),
                Arc::clone(&self.logging),
                Arc::from([]),
                Arc::clone(&self.release),
                Arc::new(LocalComponentSource::new(self.scratch.path().to_owned())),
                OperationScope {
                    project: PROJECT.to_owned(),
                    schema: Some("application_data".to_owned()),
                    owner_prefix: "schedule-live".to_owned(),
                    warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
                },
            )
            .unwrap(),
        )));
        let service = QueueService::bind(
            driver,
            Arc::clone(&self.postgres),
            Arc::new(WamnJetstream::from_env()),
            &self.release,
            QueueServiceConfig {
                project: PROJECT.to_owned(),
                runner: "schedule-live".to_owned(),
                lease_ttl_ms: 30_000,
            },
        )
        .await
        .expect("bind the queue service");
        let (_stop, stopping) = tokio::sync::watch::channel(true);
        tokio::time::timeout(Duration::from_secs(60), service.serve(stopping))
            .await
            .expect("the queue drains the release's backlog")
            .expect("queue service");
        service.revoke();
    }

    /// `(status, result_json)` of a run.
    async fn outcome(&self, run: &str) -> (String, Option<Value>) {
        let row = self
            .admin
            .query_one(
                "SELECT status, result_json::text FROM wamn_run.runs WHERE run_id = $1",
                &[&run],
            )
            .await
            .unwrap();
        (
            row.get(0),
            row.get::<_, Option<String>>(1)
                .map(|text| serde_json::from_str(&text).unwrap()),
        )
    }
}

fn lifecycle_refusal(result: Result<(), WorkflowLifecycleError>) -> WorkflowLifecycleErrorType {
    result.expect_err("the enable refuses").error_type()
}

/// An invalid cron refuses at author with the stable schedule refusal, and no
/// row is written.
#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_cron_refuses_at_author() {
    let _lock = wamn_test_postgres::lock();
    let fixture = fixture().await;
    for cron in ["* * * *", "* * * * * *", "61 * * * *", "every minute"] {
        let refusal = fixture
            .author(
                "nightly",
                WorkflowTrigger::Schedule(cron),
                WorkflowExecution::Role("reader"),
            )
            .await
            .expect_err("an invalid cron refuses");
        let refusal = refusal
            .downcast_ref::<AuthorWorkflowError>()
            .expect("an authorship refusal");
        assert_eq!(
            refusal.error_type(),
            AuthorWorkflowErrorType::Schedule,
            "{cron}"
        );
    }
    let rows: i64 = fixture
        .admin
        .query_one("SELECT count(*) FROM catalog.workflows", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(rows, 0);

    // The same version under another trigger or cron is another definition.
    fixture
        .author(
            "nightly",
            WorkflowTrigger::Schedule(CRON),
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("a valid cron is authored");
    fixture
        .author(
            "nightly",
            WorkflowTrigger::Schedule(CRON),
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("the same definition converges");
    for trigger in [
        WorkflowTrigger::Manual,
        WorkflowTrigger::Schedule("0 * * * *"),
    ] {
        let refusal = fixture
            .author("nightly", trigger, WorkflowExecution::Role("reader"))
            .await
            .expect_err("another trigger under an authored version refuses");
        assert_eq!(
            refusal
                .downcast_ref::<AuthorWorkflowError>()
                .expect("an authorship refusal")
                .error_type(),
            AuthorWorkflowErrorType::Conflict,
        );
    }
}

/// An application the environment does not declare refuses at author with
/// the stable application refusal, and no row is written.
#[tokio::test(flavor = "multi_thread")]
async fn an_undeclared_application_refuses_at_author() {
    let _lock = wamn_test_postgres::lock();
    let fixture = fixture().await;
    let other: wamn_catalog::EnvironmentApplications = [(
        "billing".to_owned(),
        wamn_catalog::ApplicationDefinition {
            packages: [APPLICATION.to_owned()].into(),
        },
    )]
    .into();
    for declared in [wamn_catalog::EnvironmentApplications::new(), other] {
        let refusal = fixture
            .author_in(
                "nightly",
                WorkflowTrigger::Manual,
                WorkflowExecution::Role("reader"),
                &declared,
            )
            .await
            .expect_err("an undeclared application refuses");
        assert_eq!(
            refusal
                .downcast_ref::<AuthorWorkflowError>()
                .expect("an authorship refusal")
                .error_type(),
            AuthorWorkflowErrorType::Application,
        );
    }
    let rows: i64 = fixture
        .admin
        .query_one("SELECT count(*) FROM catalog.workflows", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(rows, 0);
}

/// A `role` schedule workflow: two admissions of one due moment answer one
/// run of the schedule workflow grain, and the queue executes it as the
/// `workflow` user with the one role. A workflow whose role lacks the read
/// fails at its node, although the `workflow` user holds a role that has it.
#[tokio::test(flavor = "multi_thread")]
async fn a_role_schedule_run_is_admitted_once_per_due_and_runs_as_the_workflow_user() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    fixture.grant_workflow_user("clerk").await;
    for (id, role) in [("nightly", "reader"), ("clerkly", "clerk")] {
        fixture
            .author(
                id,
                WorkflowTrigger::Schedule(CRON),
                WorkflowExecution::Role(role),
            )
            .await
            .expect("author the schedule workflow");
    }
    fixture
        .enable("nightly", READER)
        .await
        .expect("an enabler holding the role enables");
    fixture
        .enable("clerkly", ADMIN)
        .await
        .expect("an admin enables");

    let run = fixture.queued("nightly", "2026-10-20T10:15:00Z").await;
    assert_eq!(
        fixture.queued("nightly", "2026-10-20T10:15:00Z").await,
        run,
        "a second admission of the same due moment answers the first run"
    );
    assert_eq!(fixture.runs().await, 1);
    let denied = fixture.queued("clerkly", "2026-10-20T10:15:00Z").await;

    let row = fixture
        .admin
        .query_one(
            "SELECT trigger_source, service_principal_id IS NULL, registration_id IS NULL, \
                    wiring_id, wiring_version, wiring_hash, manifest_digest, \
                    node_digests_json::text, execution_principal_id::text, execution_role, \
                    invoked_by, invoked_by_user_id IS NULL, idempotency_key, input_json::text, \
                    package_id \
               FROM wamn_run.runs WHERE run_id = $1",
            &[&run],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "schedule");
    assert!(row.get::<_, bool>(1), "no service principal");
    assert!(row.get::<_, bool>(2), "no registration");
    assert_eq!(
        (row.get::<_, String>(3), row.get::<_, i32>(4)),
        ("nightly".to_owned(), 1)
    );
    assert_eq!(
        row.get::<_, String>(5),
        workflow("nightly").wiring_hash().as_str()
    );
    assert_eq!(row.get::<_, String>(6), fixture.digest);
    assert_eq!(
        serde_json::from_str::<Value>(&row.get::<_, String>(7)).unwrap(),
        json!({"read": fixture.read.component_digest})
    );
    assert_eq!(row.get::<_, String>(8), workflow_user_id().to_string());
    assert_eq!(row.get::<_, String>(9), "reader");
    assert_eq!(row.get::<_, String>(10), "schedule");
    assert!(row.get::<_, bool>(11), "no invoking user");
    assert_eq!(
        row.get::<_, String>(12),
        "schedule:erp/nightly:2026-10-20T10:15:00Z"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&row.get::<_, String>(13)).unwrap(),
        json!({"due": "2026-10-20T10:15:00Z"})
    );
    assert_eq!(row.get::<_, String>(14), APP);

    fixture.drain().await;
    let (status, result) = fixture.outcome(&run).await;
    assert_eq!(status, "completed", "{result:?}");
    assert_eq!(result, Some(serde_json::from_str(ROWS).unwrap()));
    let (status, result) = fixture.outcome(&denied).await;
    assert_eq!(status, "failed");
    let error = &result.expect("a failed run records its refusal")["error"];
    assert_eq!(error["code"], "permission_denied");
    assert_eq!(error["operation"], READ);
}

/// An `invoker` schedule workflow assigns `admin`: a non-admin enabler
/// refuses, an admin refuses while the `workflow` user lacks `admin`, and is
/// allowed once it holds it. The run executes as the `workflow` user with
/// `admin` active.
#[tokio::test(flavor = "multi_thread")]
async fn an_invoker_schedule_needs_admin_to_enable() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture
        .author(
            "nightly",
            WorkflowTrigger::Schedule(CRON),
            WorkflowExecution::Invoker,
        )
        .await
        .expect("author the schedule workflow");

    assert_eq!(
        lifecycle_refusal(fixture.enable("nightly", READER).await),
        WorkflowLifecycleErrorType::RoleNotHeldByEnabler
    );
    assert_eq!(
        lifecycle_refusal(fixture.enable("nightly", ADMIN).await),
        WorkflowLifecycleErrorType::RoleNotHeldByWorkflowUser
    );
    assert_eq!(
        fixture.admit("nightly", "2026-10-20T10:15:00Z").await,
        ScheduleRunAdmitted::NotEnabled
    );
    fixture.grant_workflow_user("admin").await;
    assert_eq!(
        lifecycle_refusal(fixture.enable("nightly", READER).await),
        WorkflowLifecycleErrorType::RoleNotHeldByEnabler,
        "a non-admin still refuses"
    );
    fixture
        .enable("nightly", ADMIN)
        .await
        .expect("an admin enables while the workflow user holds admin");

    let run = fixture.queued("nightly", "2026-10-20T10:20:00Z").await;
    let row = fixture
        .admin
        .query_one(
            "SELECT execution_principal_id::text, execution_role, invoked_by \
               FROM wamn_run.runs WHERE run_id = $1",
            &[&run],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2)
        ),
        (
            workflow_user_id().to_string(),
            "admin".to_owned(),
            "schedule".to_owned()
        )
    );
    fixture.drain().await;
    assert_eq!(fixture.outcome(&run).await.0, "completed");
}

/// No enabled schedule version refuses with no row: a workflow never
/// enabled, a manual workflow, and a schedule workflow after its disable.
#[tokio::test(flavor = "multi_thread")]
async fn no_enabled_schedule_version_refuses_with_no_row() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    assert_eq!(
        fixture.admit("nightly", "2026-10-20T10:15:00Z").await,
        ScheduleRunAdmitted::NotEnabled
    );
    fixture
        .author(
            "manual",
            WorkflowTrigger::Manual,
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("author a manual workflow");
    fixture
        .enable("manual", READER)
        .await
        .expect("enable the manual workflow");
    assert_eq!(
        fixture.admit("manual", "2026-10-20T10:15:00Z").await,
        ScheduleRunAdmitted::NotEnabled,
        "a manual workflow has no schedule run"
    );
    fixture
        .author(
            "nightly",
            WorkflowTrigger::Schedule(CRON),
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("author the schedule workflow");
    assert_eq!(
        fixture.admit("nightly", "2026-10-20T10:15:00Z").await,
        ScheduleRunAdmitted::NotEnabled,
        "an authored version is disabled"
    );
    fixture
        .enable("nightly", READER)
        .await
        .expect("enable the schedule workflow");
    wamn_control::workflow_lifecycle::disable_workflow(
        &fixture.url,
        &Triple::new("acme", APPLICATION, ENVIRONMENT),
        &mut fixture.admin,
        &WorkflowVersion {
            tenant_id: TENANT,
            environment: ENVIRONMENT,
            application: APP,
            workflow_id: "nightly",
            version: 1,
        },
    )
    .await
    .expect("disable the schedule workflow");
    assert_eq!(
        fixture.admit("nightly", "2026-10-20T10:20:00Z").await,
        ScheduleRunAdmitted::NotEnabled,
        "a disabled version has no schedule run"
    );
    assert_eq!(fixture.runs().await, 0);
}

/// The first minute boundary strictly after `after`.
fn next_minute(after: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(after.timestamp().div_euclid(60) * 60 + 60, 0).unwrap()
}

/// The host's schedule task reads the enabled rows at each tick: a row
/// enabled after the task started fires at its next tick, and a row disabled
/// fires at no later tick. It runs in real time, one or two minute ticks.
#[tokio::test(flavor = "multi_thread")]
async fn the_schedule_task_fires_a_row_enabled_after_start_and_stops_after_disable() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    fixture
        .author(
            "minutely",
            WorkflowTrigger::Schedule("* * * * *"),
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("author the schedule workflow");

    let schedules = wamn_workflow::schedule::PostgresSchedules::new(
        connect(&fixture.executor_url).await,
        Arc::clone(&fixture.postgres),
        PROJECT,
        TENANT,
        ENVIRONMENT,
        declared(),
        &fixture.digest,
    );
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let task =
        tokio::spawn(async move { wamn_workflow::schedule::serve(&schedules, stopping).await });

    fixture
        .enable("minutely", READER)
        .await
        .expect("an enabler holding the role enables");
    let enabled = Utc::now();
    let fired = tokio::time::timeout(Duration::from_secs(130), async {
        loop {
            let dues = fixture.dues("minutely").await;
            if !dues.is_empty() {
                return dues;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .expect("the enabled row fires within two ticks");
    assert_eq!(fired.len(), 1, "{fired:?}");
    assert!(
        fired[0] > enabled - chrono::Duration::seconds(5)
            && fired[0] <= next_minute(next_minute(enabled)),
        "the first run is due at a tick after the enable: {fired:?}, enabled at {enabled}"
    );

    fixture
        .disable("minutely")
        .await
        .expect("the enabled version disables");
    let disabled = Utc::now();
    let quiet = next_minute(disabled) + chrono::Duration::seconds(10);
    tokio::time::sleep((quiet - Utc::now()).to_std().unwrap_or_default()).await;
    let dues = fixture.dues("minutely").await;
    assert!(
        dues.iter().all(|due| *due <= disabled),
        "a disabled row fires at no later tick: {dues:?}, disabled at {disabled}"
    );
    assert!(!task.is_finished(), "the schedule task keeps serving");

    stop.send(true).unwrap();
    task.await
        .unwrap()
        .expect("the schedule task stops at shutdown");
    fixture.drain().await;
    let run: String = fixture
        .admin
        .query_one(
            "SELECT run_id FROM wamn_run.runs WHERE wiring_id = 'minutely' ORDER BY created_at LIMIT 1",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(fixture.outcome(&run).await.0, "completed");
}

/// Schedules run from rows (docs/plan/workflows.md, Invariants and checks):
/// the serving release declares no schedule, an authored and enabled row
/// fires, and a tick missed while the task waits on the database is skipped,
/// not caught up. The enable after start and the disable are the case above.
/// A missed tick during a slow admission is the schedule task's unit test
/// `ticks_missed_during_a_slow_admission_are_skipped`. Here the task's read
/// of the rows waits on a lock across a whole minute, in real time: about
/// three minutes.
#[tokio::test(flavor = "multi_thread")]
async fn schedules_run_from_rows_and_a_missed_tick_is_skipped() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    let stored: Vec<u8> = fixture
        .admin
        .query_one(
            "SELECT canonical_bytes FROM catalog.releases WHERE manifest_digest = $1",
            &[&fixture.digest],
        )
        .await
        .unwrap()
        .get(0);
    let stored: Value = serde_json::from_slice(&stored).unwrap();
    assert!(
        stored.pointer("/workflow/schedules").is_none(),
        "the serving release declares no schedule: {stored}"
    );
    fixture.grant_workflow_user("reader").await;
    fixture
        .author(
            "minutely",
            WorkflowTrigger::Schedule("* * * * *"),
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("author the schedule workflow");
    fixture
        .enable("minutely", READER)
        .await
        .expect("an enabler holding the role enables");

    // Start clear of a minute boundary, so the task's first tick is `first`.
    if Utc::now().timestamp().rem_euclid(60) > 50 {
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
    // Hold the rows from before the first tick until after the second.
    let mut locker = connect(&fixture.url).await;
    let hold = locker.transaction().await.unwrap();
    hold.batch_execute("LOCK TABLE catalog.workflows IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let first = next_minute(Utc::now());
    let schedules = wamn_workflow::schedule::PostgresSchedules::new(
        connect(&fixture.executor_url).await,
        Arc::clone(&fixture.postgres),
        PROJECT,
        TENANT,
        ENVIRONMENT,
        declared(),
        &fixture.digest,
    );
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let task =
        tokio::spawn(async move { wamn_workflow::schedule::serve(&schedules, stopping).await });
    let second = next_minute(first);
    let third = next_minute(second);
    let until = |moment: DateTime<Utc>| (moment - Utc::now()).to_std().unwrap_or_default();
    tokio::time::sleep(until(second + chrono::Duration::seconds(5))).await;
    assert!(
        fixture.dues("minutely").await.is_empty(),
        "no run is admitted while the rows are locked"
    );
    hold.commit().await.unwrap();
    tokio::time::sleep(until(third + chrono::Duration::seconds(15))).await;
    stop.send(true).unwrap();
    task.await
        .unwrap()
        .expect("the schedule task stops at shutdown");
    assert_eq!(
        fixture.dues("minutely").await,
        [first, third],
        "the late first tick admits its own due, the missed second tick is skipped, \
         and the third fires"
    );
}
