//! Shared schedule and event fixtures over private PostgreSQL.

pub use std::collections::BTreeSet;
pub use std::sync::Arc;
pub use std::time::Duration;

pub use chrono::{DateTime, Utc};
pub use serde_json::{Value, json};
pub use tokio_postgres::{Client, NoTls};
pub use wamn_catalog::{
    ComponentDeclaration, ComponentDescriptor, SERVING_MANIFEST_FORMAT_VERSION, ServingManifest,
    WiringDocument,
};
pub use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
pub use wamn_control_registry::Triple;
pub use wamn_engine::artifact_source::{LocalComponentSource, local_component_path};
pub use wamn_engine::component_admission::{
    ComponentAdmissionRequest, validate_component_admission,
};
pub use wamn_engine::engine::build_engine;
pub use wamn_engine::release_manifest::{LoadedRelease, ReleaseScope};
pub use wamn_environment_control::author_workflow::{
    AuthorWorkflowError, AuthorWorkflowErrorType, AuthorWorkflowRequest, WorkflowExecution,
    WorkflowTrigger, author_workflow_in_databases,
};
pub use wamn_environment_control::workflow_lifecycle::{
    WorkflowLifecycleError, WorkflowLifecycleErrorType, WorkflowVersion, disable_workflow,
    enable_workflow,
};
pub use wamn_execution_host::{OperationHost, OperationScope};
pub use wamn_project_state::{PlatformComponent, workflow_user_id};
pub use wamn_runtime::connection_credentials::ConnectionCredentials;
pub use wamn_runtime::plugins::connection_http::transport::HttpTransport;
pub use wamn_runtime::plugins::wamn_jetstream::WamnJetstream;
pub use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
pub use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, ClassCredentials, ScheduleRunAdmission, ScheduleRunAdmitted, WamnPostgres,
    WamnPostgresConfig,
};
pub use wamn_schema_control::BareSchemaName;
pub use wamn_workflow::{QueueService, QueueServiceConfig, RouterDriver};

pub const ENV_KEY: &str = "schedule-live--default--dev";
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
        "scope": {"env_key": ENV_KEY, "package-id": APPLICATION, "package-version": "1.0.0"},
        "component": "orders_data", "interface-version": "0.1.0",
        "operations": {(READ): {"registered-operation": READ,
            "input-ports": [{"name": "input", "schema": {}}],
            "output-ports": [{"name": "main", "schema": {}}],
            "parameters": []}},
        "connections": []
    })
}

/// The one-node workflow `id` over the read.
pub fn workflow(id: &str) -> WiringDocument {
    WiringDocument::parse(&json!({
        "format-version": "0.1", "wiring-id": id, "version": 1, "entry": "read",
        "nodes": {"read": {"component": "orders_data", "interface-version": "0.1.0",
            "operation": READ, "params": {}}},
        "edges": [], "cases": []
    }))
    .expect("the workflow graph is structurally valid")
}

pub fn due(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

pub async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

#[derive(Debug)]
pub struct Fixture {
    pub admin: Client,
    pub url: String,
    pub control: Client,
    pub control_url: String,
    pub engine: Arc<wash_runtime::engine::Engine>,
    pub postgres: Arc<WamnPostgres>,
    /// The executor login, which the host's schedule task connects with.
    pub executor_url: String,
    pub logging: Arc<WamnLogging>,
    pub scratch: wamn_test_infrastructure::scratch::ScratchRoot,
    pub read: wamn_catalog::AdmittedComponent,
    pub release: Arc<LoadedRelease>,
    pub digest: String,
    _databases: (wamn_test_postgres::Database, wamn_test_postgres::Database),
}

pub async fn fixture() -> Fixture {
    let database = wamn_test_postgres::database();
    let control_database = wamn_control_provision::test_database::system();
    let url = database.url().to_owned();
    let admin = connect(&url).await;
    let schema = BareSchemaName::new("wamn_run").unwrap();
    admin
        .batch_execute("CREATE SCHEMA application_data")
        .await
        .unwrap();
    wamn_environment_control::reconcile_run_plane::reconcile(&admin, &schema, true)
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
             INSERT INTO platform.role (name) VALUES \
               ('reader'), ('clerk'), ('admin');
             INSERT INTO platform.user_role (user_id, role_name) VALUES \
               ('{READER}', 'reader'), ('{ADMIN}', 'admin');
             INSERT INTO platform.permission (role_name, permission) VALUES \
               ('reader', '{READ_REFERENCE}');
             INSERT INTO platform.environment (status) VALUES ('active');
             INSERT INTO wamn_run.environment_policies (env_key, expected_environment, durability_class) \
               VALUES ('{ENV_KEY}', '{ENVIRONMENT}', 'standard');
             INSERT INTO catalog.packages (physical_schema_fingerprint, env_key, package_id, package_version, artifact_digest) \
               VALUES ('sha256:' || repeat('c', 64), '{ENV_KEY}', '{APPLICATION}', '1.0.0', '{ARTIFACT_DIGEST}');"
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
            ReleaseScope::new(ENV_KEY, ENVIRONMENT).with_applications(declared()),
        )
        .expect("the fixture release loads"),
    );
    let digest = release.release().manifest_digest.as_str().to_owned();
    admin
        .execute(
            "INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3)",
            &[&ENV_KEY, &digest, &served.canonical_bytes()],
        )
        .await
        .expect("install the serving release");

    let (postgres, executor_url) = scoped_postgres(&admin, &url).await;
    let (logging, _capture) = WamnLogging::new_with_capture(&WamnLoggingConfig::default()).unwrap();
    let control = connect(control_database.url()).await;
    control
        .execute(
            "SELECT set_config('app.env_key', $1, false), set_config('app.user_id', $2, false)",
            &[&ENV_KEY, &READER],
        )
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
pub async fn scoped_postgres(admin: &Client, url: &str) -> (Arc<WamnPostgres>, String) {
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
            WorkloadRoleScope::EnvKey {
                env_key: ENV_KEY,
                database: &database,
            }
        } else {
            WorkloadRoleScope::ProjectEnvironment {
                org: ENV_KEY,
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
    /// green gate report under its hash.
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
        self.control
            .execute(
                "INSERT INTO wamn_run.gate_reports (env_key, wiring_hash, passed, summary) \
                 VALUES ($1, $2, true, '{}'::jsonb) ON CONFLICT DO NOTHING",
                &[&ENV_KEY, &document.wiring_hash().as_str()],
            )
            .await
            .expect("record a green gate report");
        author_workflow_in_databases(
            &self.url,
            &self.control_url,
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
            &Triple::new("acme", APPLICATION, ENVIRONMENT),
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
            &Triple::new("acme", APPLICATION, ENVIRONMENT),
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
    pub async fn dues(&self, id: &str) -> Vec<DateTime<Utc>> {
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

    pub async fn grant_workflow_user(&self, role: &str) {
        self.admin
            .execute(
                "INSERT INTO platform.user_role (user_id, role_name) \
                 VALUES ($1::text::uuid, $2)",
                &[&workflow_user_id().to_string(), &role],
            )
            .await
            .expect("grant the workflow user a role");
    }

    pub async fn admit(&self, id: &str, at: &str) -> ScheduleRunAdmitted {
        self.postgres
            .admit_schedule_run(
                PROJECT,
                ENV_KEY,
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

    pub async fn queued(&self, id: &str, at: &str) -> String {
        match self.admit(id, at).await {
            ScheduleRunAdmitted::Queued { run_id } => run_id,
            other => panic!("the schedule admission queues a run: {other:?}"),
        }
    }

    pub async fn runs(&self) -> i64 {
        self.admin
            .query_one("SELECT count(*) FROM wamn_run.runs", &[])
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
                Arc::new(wamn_runtime::connection_facts::ConnectionFacts::new(
                    Arc::new(wamn_runtime::connection_memory::ConnectionMemory::default()),
                    wamn_platform_identity::connection_changes::ConnectionChanges::default(),
                    Arc::new(ConnectionCredentials::empty()),
                    Arc::clone(&self.postgres),
                    self.release.scope().env_key.as_str(),
                    PROJECT,
                    self.release.scope().environment.as_str(),
                )),
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
    pub async fn outcome(&self, run: &str) -> (String, Option<Value>) {
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

pub fn lifecycle_refusal(result: Result<(), WorkflowLifecycleError>) -> WorkflowLifecycleErrorType {
    result.expect_err("the enable refuses").error_type()
}

pub use wamn_runtime::plugins::wamn_postgres::EventBatchAdmission;

pub async fn event(fixture: &Fixture, id: &str, sequence: i64, role: Option<&str>) {
    let graph = workflow(id);
    let execution = if role.is_some() { "role" } else { "invoker" };
    fixture.admin.execute(
        "INSERT INTO catalog.workflows (env_key, environment, application, workflow_id, version, graph_json, wiring_hash, trigger, source_package, entity, ops, enable_sequence, execution_type, execution_role, enabled, created_by) VALUES ($1, $2, $3, $4, 1, $5::text::jsonb, $6, 'event', $7, 'order', ARRAY['insert', 'update', 'delete'], $8, $9, $10, true, $11::text::uuid)",
        &[&ENV_KEY, &ENVIRONMENT, &APP, &id, &serde_json::to_string(&graph).unwrap(), &graph.wiring_hash().as_str(), &APPLICATION, &sequence, &execution, &role, &ADMIN],
    ).await.unwrap();
}

pub fn envelope(op: &str, txid: i64, lsn: u64) -> String {
    let row = json!({"id": "7", "at": "2026-10-25 10:00:00+00", "status": "open"});
    let mut value = json!({
        "op": op, "package_id": APPLICATION, "entity": "order", "table": "orders",
        "lsn": lsn, "txid": txid, "commit_ts": "2026-10-25T10:00:00Z",
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
) -> Result<(), wamn_runtime::plugins::wamn_postgres::ProductionClaimError> {
    fixture
        .postgres
        .admit_event_batch(
            PROJECT,
            &fixture.release,
            &EventBatchAdmission {
                source_package: APPLICATION,
                entity: "order",
                envelope,
                sequence,
                source_event_id: &wamn_event_wire::msg_id(PROJECT, ENVIRONMENT, lsn),
            },
        )
        .await
}

pub async fn changed_order(fixture: &Fixture) -> i64 {
    let admin = &fixture.admin;
    admin.batch_execute(
        "CREATE TABLE application_data.orders (id bigint, at timestamptz, status text, PRIMARY KEY (id, at));
         SELECT wamn_history.create_history_table('application_data', 'orders', false);
         CREATE TRIGGER record_history AFTER INSERT OR UPDATE OR DELETE ON application_data.orders
         FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');"
    ).await.unwrap();
    admin
        .batch_execute(&sql::ensure_entity_map_sql("application_data"))
        .await
        .unwrap();
    admin
        .execute(
            &sql::upsert_entity_map_sql("application_data"),
            &[&APPLICATION, &"order", &"orders"],
        )
        .await
        .unwrap();
    admin
        .batch_execute(&sql::grant_executor_history_reads_sql())
        .await
        .unwrap();

    admin
        .batch_execute("BEGIN; SET LOCAL app.operation = 'admin:event-batch-fixture'")
        .await
        .unwrap();
    admin
        .execute("SELECT set_config('app.user_id', $1, true)", &[&READER])
        .await
        .unwrap();
    admin
        .batch_execute(
            "INSERT INTO application_data.orders VALUES (7, '2026-10-25T10:00:00Z', 'open')",
        )
        .await
        .unwrap();
    let txid: i64 = admin
        .query_one(
            "SELECT pg_current_xact_id()::text::bigint % 4294967296",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    admin.batch_execute("COMMIT").await.unwrap();
    txid
}

pub async fn release_variant(
    fixture: &Fixture,
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
        ReleaseScope::new(ENV_KEY, ENVIRONMENT)
            .with_applications([(application.to_owned(), declared().remove(APP).unwrap())].into()),
    )
    .unwrap();
    fixture.admin.execute(
            "INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            &[&ENV_KEY, &release.release().manifest_digest.as_str(), &manifest.canonical_bytes()],
        ).await.unwrap();
    release
}
