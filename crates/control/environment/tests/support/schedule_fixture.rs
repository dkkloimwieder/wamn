//! Shared schedule and event fixtures over private PostgreSQL.

use sqlx::PgConnection as Client;
use sqlx::Row as _;
pub use std::collections::BTreeSet;
pub use std::sync::Arc;
pub use std::time::Duration;

pub use chrono::{DateTime, Utc};
pub use serde_json::{Value, json};
pub use wamn_catalog::{
    ComponentDeclaration, ComponentDescriptor, SERVING_MANIFEST_FORMAT_VERSION, ServingManifest,
    WiringDocument,
};
pub use wamn_connections::connection_credentials::ConnectionCredentials;
pub use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};
pub use wamn_engine::artifact_source::{LocalComponentSource, local_component_path};
pub use wamn_engine::component_admission::{
    ComponentAdmissionRequest, validate_component_admission,
};
pub use wamn_engine::engine::build_engine;
pub use wamn_engine::release_manifest::{LoadedRelease, ReleaseTarget};
pub use wamn_events_capability::WamnJetstream;
pub use wamn_execution_host::{OperationHost, OperationHostConfig};
pub use wamn_http_capability::transport::HttpTransport;
pub use wamn_identity_protocol::{PlatformComponent, workflow_user_id};
pub use wamn_postgres_capability::{
    AuthorityClass, ClassCredentials, WamnPostgres, WamnPostgresConfig,
};
pub use wamn_registry::Triple;
pub use wamn_workflow::author_workflow::{
    AuthorWorkflowError, AuthorWorkflowErrorType, AuthorWorkflowRequest, WorkflowExecution,
    WorkflowTrigger, author_workflow_in_databases,
};
pub use wamn_workflow::workflow_lifecycle::{
    WorkflowLifecycleError, WorkflowLifecycleErrorType, WorkflowVersion, disable_workflow,
    enable_workflow,
};
pub use wamn_workflow::{QueueService, QueueServiceConfig, RouterDriver};
pub use wash_runtime::plugin::wasi_logging::TracingLogger;

pub const PROJECT_ENV_KEY: &str = "schedule-live--dev--default";
pub const PROJECT: &str = "default";
pub const ENVIRONMENT: &str = "dev";
pub const APPLICATION: &str = "orders";
/// The application the environment declares, with the package `orders`. Its
/// name is no package (docs/architecture/execution.md#workflows).
pub const APP: &str = "erp";
pub const PASSWORD: &str = "schedule-live-test-only";
pub const READ: &str = "orders:order/read@1.0.0";
pub const READ_REFERENCE: &str = "orders:order/read";
pub const CRON: &str = "*/5 * * * *";
/// A user who holds one role, `reader`, granted the read.
pub const READER: &str = "00000000-0000-4000-8000-000000000201";
/// A user who holds `admin`.
pub const ADMIN: &str = "00000000-0000-4000-8000-000000000202";
pub const ARTIFACT_DIGEST: &str =
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The rows the read answers.
pub const ROWS: &str = r#"{"rows":[{"id":"o-1","total":1250,"status":"open"}]}"#;

pub const NODE_TYPES: &str = r#"
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
pub fn read_component() -> Vec<u8> {
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

pub fn read_declaration() -> Value {
    json!({
        "target": {"project_env_key": PROJECT_ENV_KEY, "package-id": APPLICATION, "package-version": "1.0.0"},
        "component": "orders_data", "interface-version": "0.1.0",
        "operations": {(READ): {"registered-operation": READ,
            "input-ports": [{"name": "input", "schema": {}}],
            "output-ports": [{"name": "main", "schema": {}}],
            "parameters": []}}
    })
}

/// The one-node workflow `id` over the read.
pub fn workflow(id: &str) -> WiringDocument {
    WiringDocument::parse(&json!({
        "format-version": "0.1", "wiring-id": id, "version": 1, "entry": "read",
        "nodes": {"read": {"component": "orders_data", "interface-version": "0.1.0",
            "operation": READ, "params": {}}},
        "edges": []
    }))
    .expect("the workflow graph is structurally valid")
}

pub fn due(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

pub async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to the disposable database");

    client
}

pub struct Fixture {
    pub admin: Client,
    pub url: String,
    pub engine: Arc<wash_runtime::engine::Engine>,
    pub postgres: Arc<WamnPostgres>,
    /// The executor login, which the host's schedule task connects with.
    pub executor_url: String,
    pub logging: Arc<TracingLogger>,
    pub scratch: wamn_test_infrastructure::scratch::ScratchRoot,
    pub read: wamn_catalog::AdmittedComponent,
    pub release: Arc<LoadedRelease>,
    pub digest: String,
    _database: wamn_test_postgres::Database,
}

impl std::fmt::Debug for Fixture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Fixture")
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

pub async fn fixture() -> Fixture {
    let database = wamn_test_postgres::database();
    let url = database.url().to_owned();
    let mut admin = connect(&url).await;
    sqlx::raw_sql("CREATE SCHEMA application_data")
        .execute(&mut admin)
        .await
        .map(drop)
        .unwrap();
    wamn_environment::platform_migrations::install_project(&mut admin)
        .await
        .expect("install the run plane and the catalog");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::platform_principals_sql("schedule.invalid").unwrap()).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("write the platform principals and the workflow user");
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:schedule-live-fixture', false)",
    )
    .bind(&PlatformComponent::Provisioning.principal_id().to_string())
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "INSERT INTO platform.\"user\" (id, type, email) VALUES \
               ('{READER}', 'user', 'reader@schedule.invalid'), \
               ('{ADMIN}', 'user', 'admin@schedule.invalid');
             INSERT INTO platform.role (name) VALUES \
               ('reader'), ('clerk'), ('admin');
             INSERT INTO platform.user_role (user_id, role_name) VALUES \
               ('{READER}', 'reader'), ('{ADMIN}', 'admin');
             INSERT INTO platform.permission (role_name, permission) VALUES \
               ('reader', '{READ_REFERENCE}');
             INSERT INTO platform.environment (status) VALUES ('active');
             INSERT INTO catalog.packages ( project_env_key, package_id, package_version, artifact_digest) VALUES ( '{PROJECT_ENV_KEY}', '{APPLICATION}', '1.0.0', '{ARTIFACT_DIGEST}');"
        ))).execute(&mut admin).await.map(drop)
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
            ReleaseTarget::new(PROJECT_ENV_KEY, ENVIRONMENT).with_applications(declared()),
        )
        .expect("the fixture release loads"),
    );
    let digest = release.release().manifest_digest.as_str().to_owned();
    sqlx::query(
        "INSERT INTO catalog.releases (project_env_key, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3)",
    )
    .bind(&PROJECT_ENV_KEY)
    .bind(&digest)
    .bind(&served.canonical_bytes())
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())
    .expect("install the serving release");

    let (postgres, executor_url) = targeted_postgres(&mut admin, &url).await;
    let logging = TracingLogger::default();
    Fixture {
        admin,
        url,
        engine,
        postgres,
        executor_url,
        logging: Arc::new(logging),
        scratch,
        read,
        release,
        digest,
        _database: database,
    }
}

/// The host's database access under the targeted logins of its families: the
/// application login for guest SQL, the executor for admission and the
/// queue, and the admitter for permission reads. Also the executor login.
pub async fn targeted_postgres(admin: &mut Client, url: &str) -> (Arc<WamnPostgres>, String) {
    let database: String = sqlx::query("SELECT current_database()")
        .fetch_one(&mut *admin)
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
        let target = if family == WorkloadRoleFamily::App {
            WorkloadRoleIdentity::ProjectEnvKey {
                project_env_key: PROJECT_ENV_KEY,
                database: &database,
            }
        } else {
            WorkloadRoleIdentity::ProjectEnvironment {
                org: PROJECT_ENV_KEY,
                project: PROJECT,
                environment: ENVIRONMENT,
                database: &database,
            }
        };
        let role = workload_login(family, target).unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::login_sql(family, &database, &role, PASSWORD))
                .to_string(),
        ))
        .execute(&mut *admin)
        .await
        .map(drop)
        .expect("prepare the targeted login");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER ROLE \"{role}\" SET search_path = application_data"
        )))
        .execute(&mut *admin)
        .await
        .map(drop)
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
pub fn declared() -> wamn_catalog::EnvironmentApplications {
    [(
        APP.to_owned(),
        wamn_catalog::ApplicationDefinition {
            connections: std::collections::BTreeMap::new(),
            packages: [APPLICATION.to_owned()].into(),
        },
    )]
    .into()
}

impl Fixture {
    /// Author version 1 of `id` as `wamn-ctl workflow author` does, with a
    /// green condition report under its hash.
    pub async fn author(
        &self,
        id: &str,
        trigger: WorkflowTrigger<'_>,
        execution: WorkflowExecution<'_>,
    ) -> anyhow::Result<()> {
        self.author_in(id, trigger, execution, &declared()).await
    }

    /// [`Self::author`] in an environment that declares `declared`.
    pub async fn author_in(
        &self,
        id: &str,
        trigger: WorkflowTrigger<'_>,
        execution: WorkflowExecution<'_>,
        declared: &wamn_catalog::EnvironmentApplications,
    ) -> anyhow::Result<()> {
        let document = workflow(id);

        author_workflow_in_databases(
            &self.url,
            &AuthorWorkflowRequest {
                org: "schedule-live",
                project: "default",
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

    pub async fn enable(&mut self, id: &str, enabler: &str) -> Result<(), WorkflowLifecycleError> {
        enable_workflow(
            &self.url,
            &Triple::new("acme", ENVIRONMENT, APPLICATION),
            &mut self.admin,
            &WorkflowVersion {
                org: "schedule-live",
                project: "default",
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

    pub async fn disable(&mut self, id: &str) -> Result<(), WorkflowLifecycleError> {
        disable_workflow(
            &self.url,
            &Triple::new("acme", ENVIRONMENT, APPLICATION),
            &mut self.admin,
            &WorkflowVersion {
                org: "schedule-live",
                project: "default",
                environment: ENVIRONMENT,
                application: APP,
                workflow_id: id,
                version: 1,
            },
        )
        .await
    }

    /// The due moments of the schedule runs of `id`, oldest first.
    pub async fn dues(&mut self, id: &str) -> Vec<DateTime<Utc>> {
        sqlx::query(
            "SELECT input_json->>'due' FROM wamn_run.runs \
                  WHERE trigger_source = 'schedule' AND wiring_id = $1 ORDER BY 1",
        )
        .bind(&id)
        .fetch_all(&mut self.admin)
        .await
        .unwrap()
        .iter()
        .map(|row| due(&row.get::<String, _>(0)))
        .collect()
    }

    pub async fn grant_workflow_user(&mut self, role: &str) {
        sqlx::query(
            "INSERT INTO platform.user_role (user_id, role_name) \
                 VALUES ($1::text::uuid, $2)",
        )
        .bind(&workflow_user_id().to_string())
        .bind(&role)
        .execute(&mut self.admin)
        .await
        .map(|result| result.rows_affected())
        .expect("grant the workflow user a role");
    }

    pub async fn admit(&self, id: &str, at: &str) -> ScheduleRunAdmitted {
        wamn_workflow::admission::admit_schedule_run(
            &self.postgres,
            PROJECT,
            &self.release,
            APP,
            id,
            due(at),
        )
        .await
        .expect("the schedule admission answers")
    }

    pub async fn queued(&self, id: &str, at: &str) -> String {
        match self.admit(id, at).await {
            ScheduleRunAdmitted::Queued { run_id } => run_id,
            other => panic!("the schedule admission queues a run: {other:?}"),
        }
    }

    pub async fn runs(&mut self) -> i64 {
        sqlx::query("SELECT count(*) FROM wamn_run.runs")
            .fetch_one(&mut self.admin)
            .await
            .unwrap()
            .get(0)
    }

    /// Bind the real queue service to the release and drain its runs.
    pub async fn drain(&self) {
        let driver = Arc::new(RouterDriver::new(Arc::new(
            OperationHost::new(
                Arc::clone(&self.engine),
                Arc::clone(&self.postgres),
                Arc::new(HttpTransport::new().unwrap()),
                Arc::new(wamn_connections::connection_facts::ConnectionFacts::new(
                    Arc::new(wamn_connections::connection_memory::ConnectionMemory::default()),
                    wamn_broker::Publisher::default(),
                    Arc::new(ConnectionCredentials::empty()),
                    &self.postgres,
                    self.release.target().project_env_key.as_str(),
                    PROJECT,
                    self.release.target().environment.as_str(),
                )),
                Arc::clone(&self.logging),
                Arc::from([]),
                Arc::clone(&self.release),
                Arc::new(LocalComponentSource::new(self.scratch.path().to_owned())),
                OperationHostConfig {
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
            Arc::new(
                WamnJetstream::new(wamn_events_capability::WamnJetstreamConfig::default())
                    .await
                    .expect("event transport initializes"),
            ),
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
    pub async fn outcome(&mut self, run: &str) -> (String, Option<Value>) {
        let row =
            sqlx::query("SELECT status, result_json::text FROM wamn_run.runs WHERE run_id = $1")
                .bind(&run)
                .fetch_one(&mut self.admin)
                .await
                .unwrap();
        (
            row.get(0),
            row.get::<Option<String>, _>(1)
                .map(|text| serde_json::from_str(&text).unwrap()),
        )
    }
}

pub fn lifecycle_refusal(result: Result<(), WorkflowLifecycleError>) -> WorkflowLifecycleErrorType {
    result.expect_err("the enable refuses").error_type()
}

pub use wamn_events_capability::EventBatchAdmission;
pub use wamn_workflow::admission::ScheduleRunAdmitted;

pub async fn event(fixture: &mut Fixture, id: &str, sequence: i64, role: Option<&str>) {
    let graph = workflow(id);
    let execution = if role.is_some() { "role" } else { "invoker" };
    sqlx::query("INSERT INTO catalog.workflows (project_env_key, environment, application, workflow_id, version, graph_json, wiring_hash, trigger, source_package, entity, ops, enable_sequence, execution_type, execution_role, enabled, created_by) VALUES ($1, $2, $3, $4, 1, $5::text::jsonb, $6, 'event', $7, 'order', ARRAY['insert', 'update', 'delete'], $8, $9, $10, true, $11::text::uuid)").bind(&PROJECT_ENV_KEY).bind(&ENVIRONMENT).bind(&APP).bind(&id).bind(&serde_json::to_string(&graph).unwrap()).bind(&graph.wiring_hash().as_str()).bind(&APPLICATION).bind(&sequence).bind(&execution).bind(&role).bind(&ADMIN).execute(&mut fixture.admin).await.map(|result| result.rows_affected()).unwrap();
}

pub fn envelope(op: &str, txid: i64, lsn: u64) -> String {
    let row = json!({"id": "7", "at": "2026-10-25 10:00:00+00", "status": "open"});
    let mut value = json!({
        "op": op, "package_id": APPLICATION, "entity": "order", "table": "orders",
        "type": "cdc", "identity": {"system_id": 1, "timeline": 1, "commit_lsn": lsn, "row_ordinal": 0}, "txid": txid, "commit_ts": "2026-10-25T10:00:00Z",
        "causation": {"run": "source-run", "root": "root-run", "depth": 2}
    });
    value[if op == "delete" { "old" } else { "new" }] = row;
    value.to_string()
}

pub async fn admit(
    fixture: &Fixture,
    envelope: &str,
    sequence: u64,
    lsn: u64,
) -> Result<(), wamn_postgres_capability::ProductionLeaseError> {
    wamn_workflow::event_admission::admit_event_batch(
        &fixture.postgres,
        PROJECT,
        &fixture.release,
        &EventBatchAdmission {
            application: APP,
            source_package: APPLICATION,
            entity: "order",
            envelope,
            sequence,
            source_event_id: &wamn_event_wire::msg_id(
                PROJECT,
                ENVIRONMENT,
                &wamn_event_wire::CdcIdentity {
                    system_id: 1,
                    timeline: 1,
                    commit_lsn: lsn,
                    row_ordinal: 0,
                },
            ),
        },
    )
    .await
}

pub async fn changed_order(fixture: &mut Fixture) -> i64 {
    let admin = &mut fixture.admin;
    sqlx::raw_sql("CREATE TABLE application_data.orders (id bigint, at timestamptz, status text, PRIMARY KEY (id, at));
         SELECT wamn_history.create_history_table('application_data', 'orders', false);
         CREATE TRIGGER record_history AFTER INSERT OR UPDATE OR DELETE ON application_data.orders
         FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');").execute(&mut *admin).await.map(drop).unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::ensure_entity_map_sql("application_data")).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(
        (&sql::upsert_entity_map_sql("application_data")).to_string(),
    ))
    .bind(&APPLICATION)
    .bind(&"order")
    .bind(&"orders")
    .execute(&mut *admin)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::grant_executor_history_reads_sql()).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .unwrap();

    sqlx::raw_sql("BEGIN; SET LOCAL app.operation = 'admin:event-batch-fixture'")
        .execute(&mut *admin)
        .await
        .map(drop)
        .unwrap();
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(&READER)
        .execute(&mut *admin)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("INSERT INTO application_data.orders VALUES (7, '2026-10-25T10:00:00Z', 'open')")
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .unwrap();
    let txid: i64 = sqlx::query("SELECT pg_current_xact_id()::text::bigint % 4294967296")
        .fetch_one(&mut *admin)
        .await
        .unwrap()
        .get(0);
    sqlx::raw_sql("COMMIT")
        .execute(&mut *admin)
        .await
        .map(drop)
        .unwrap();
    txid
}

pub async fn release_variant(
    fixture: &mut Fixture,
    application: &str,
    amount: &[u8; 4],
) -> LoadedRelease {
    let mut manifest = serde_json::to_value(fixture.release.manifest()).unwrap();
    let mut bytes = read_component();
    let offset = bytes.windows(4).position(|part| part == b"1250").unwrap();
    bytes[offset..offset + amount.len()].copy_from_slice(amount);
    let component = validate_component_admission(
        &fixture.engine,
        &bytes,
        ComponentAdmissionRequest {
            declaration: serde_json::from_value(read_declaration()).unwrap(),
            admitted_platform_packages: BTreeSet::from(["wamn:node".to_owned()]),
            base_digests: BTreeSet::new(),
        },
    )
    .unwrap()
    .component;
    manifest["components"][0]["descriptor"] =
        serde_json::to_value(ComponentDescriptor::new(component, Vec::new())).unwrap();
    let manifest: ServingManifest = serde_json::from_value(manifest).unwrap();
    let release = LoadedRelease::load_canonical_bytes(
        &manifest.canonical_bytes(),
        "event-live",
        ReleaseTarget::new(PROJECT_ENV_KEY, ENVIRONMENT)
            .with_applications([(application.to_owned(), declared().remove(APP).unwrap())].into()),
    )
    .unwrap();
    sqlx::query("INSERT INTO catalog.releases (project_env_key, manifest_digest, canonical_bytes) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING").bind(&PROJECT_ENV_KEY).bind(&release.release().manifest_digest.as_str()).bind(&manifest.canonical_bytes()).execute(&mut fixture.admin).await.map(|result| result.rows_affected()).unwrap();
    release
}
