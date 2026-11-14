//! A manual run of an environment-authored workflow, live
//! (docs/architecture/execution.md#environment-workflows), and its negative
//! tests.
//!
//! The fixture release carries one application, `orders`, with one read
//! operation (a WAT component that answers fixed rows and writes nothing) and
//! the virtualized `jsonata` node.
//!
//! A workflow `read -> shape` is authored through the code path of
//! `wamn-ctl workflow author`, enabled with `execution_type = invoker`, and
//! started through the host route `workflow.start` by a session user whose
//! grants come from its role rows through the session permission reader.
//! The real queue service of the release drains it through the router, under
//! the targeted executor, application and admitter logins.
//!
//! The tests that `workflow_lifecycle_live.rs` already proves stay there:
//! changed bytes under an authored version (1), an enabler without the role
//! (4), a workflow user without the role (5), two enabled versions (6) and a
//! held lifecycle lock (11).

use sqlx::PgConnection as Client;
use sqlx::Row as _;
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use wamn_broker::Publisher;
use wamn_catalog::{
    ComponentDeclaration, ComponentDescriptor, HostRouteSet, SERVING_MANIFEST_FORMAT_VERSION,
    ServingManifest, WiringDocument,
};
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};
use wamn_engine::artifact_source::{LocalComponentSource, local_component_path};
use wamn_engine::component_admission::{ComponentAdmissionRequest, validate_component_admission};
use wamn_engine::engine::build_engine;
use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialType};
use wamn_engine::native_routes::{DeliveryRequest, Source};
use wamn_engine::release_manifest::{LoadedRelease, ReleaseTarget};
use wamn_environment::author_workflow::{
    AuthorWorkflowErrorType, AuthorWorkflowRequest, WorkflowExecution, WorkflowTrigger,
    author_workflow_in_databases,
};
use wamn_environment::workflow_lifecycle::{WorkflowVersion, enable_workflow};
use wamn_execution_host::{
    HostRouteDelivery, HostRouteHandlers, OperationHost, OperationHostConfig,
};
use wamn_identity_protocol::PlatformComponent;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::Triple;
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::wamn_jetstream::WamnJetstream;
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, ClassCredentials, ManualRunAdmission, ManualRunAdmitted, WamnPostgres,
    WamnPostgresConfig,
};
use wamn_workflow::{QueueService, QueueServiceConfig, RouterDriver};

const PROJECT_ENV_KEY: &str = "manual-live--dev--default";
const PROJECT: &str = "default";
const ENVIRONMENT: &str = "dev";
const APPLICATION: &str = "orders";
/// The application the environment declares, with the package `orders`. Its
/// name is no package (docs/architecture/execution.md#workflows).
const APP: &str = "erp";
const WORKFLOW: &str = "orders_report";
const PASSWORD: &str = "manual-live-test-only";
/// The read operation of the application, and the stable reference a role
/// stores for it.
const READ: &str = "orders:order/read@1.0.0";
const READ_REFERENCE: &str = "orders:order/read";
const START_REFERENCE: &str = "wamn-control:workflow/start";
const JSONATA_OPERATION: &str = "wamn:node/handler@0.1.0";
/// A user who holds one role, `reader`, granted the read and the start.
const READER: &str = "00000000-0000-4000-8000-000000000101";
/// A user who holds one role, `clerk`, granted the start only.
const CLERK: &str = "00000000-0000-4000-8000-000000000102";
const ARTIFACT_DIGEST: &str =
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The rows the read answers, and the application rows they mirror.
const ROWS: &str = r#"{"rows":[{"id":"o-1","total":1250,"status":"open"},{"id":"o-2","total":300,"status":"closed"}]}"#;
/// The shaping expression of the workflow's second node.
const SHAPE: &str = r#"[rows.{"order": id, "open": status = "open", "total": total}]"#;

/// The declaration template of the jsonata node, as publish renders it.
const JSONATA_DECLARATION: &str =
    include_str!("../../../../apps/platform/execution/jsonata/declaration.json.in");

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
        "target": {"project_env_key": PROJECT_ENV_KEY, "package-id": APPLICATION, "package-version": "1.0.0"},
        "component": "orders_data", "interface-version": "0.1.0",
        "operations": {(READ): {"registered-operation": READ,
            "input-ports": [{"name": "input", "schema": {}}],
            "output-ports": [{"name": "main", "schema": {}}],
            "parameters": []}},
        "connections": []
    })
}

fn jsonata_declaration() -> Value {
    serde_json::from_str(
        &JSONATA_DECLARATION
            .replace("__PROJECT_ENV_KEY__", PROJECT_ENV_KEY)
            .replace("__PACKAGE_ID__", APPLICATION)
            .replace("__PACKAGE_VERSION__", "1.0.0"),
    )
    .expect("the jsonata declaration template is JSON")
}

fn jsonata_bytes() -> Vec<u8> {
    let path = std::path::PathBuf::from(
        std::env::var_os("WAMN_JSONATA_COMPONENT")
            .expect("set WAMN_JSONATA_COMPONENT to the built jsonata component"),
    );
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "read {}; run wamn build apps/wamn_wms: {error}",
            path.display()
        )
    })
}

/// The workflow `read -> shape` at `version`, reading through `operation`.
fn workflow(version: u32, operation: &str) -> WiringDocument {
    WiringDocument::parse(&json!({
        "format-version": "0.1", "wiring-id": WORKFLOW, "version": version, "entry": "read",
        "nodes": {
            "read": {"component": "orders_data", "interface-version": "0.1.0",
                     "operation": operation, "params": {}},
            "shape": {"component": "jsonata", "interface-version": "0.1.0",
                      "operation": JSONATA_OPERATION, "params": {"expression": SHAPE}}
        },
        "edges": [{"from": "read", "to": "shape", "to-port": "input"}]
    }))
    .expect("the workflow graph is structurally valid")
}

/// A release of `orders` serving the application host routes. `read` adds
/// the read component.
fn manifest(
    read: Option<&wamn_catalog::AdmittedComponent>,
    jsonata: &wamn_catalog::AdmittedComponent,
) -> ServingManifest {
    let mut components = vec![json!({"package-id": APPLICATION,
        "descriptor": ComponentDescriptor::new(jsonata.clone(), Vec::new()),
        "operations": {(JSONATA_OPERATION): {"statements": {}}}})];
    if let Some(read) = read {
        components.push(json!({"package-id": APPLICATION,
            "descriptor": ComponentDescriptor::new(read.clone(), Vec::new()),
            "operations": {(READ): {"registered-operation": READ, "permissions": [READ],
                "statements": {}}}}));
    }
    serde_json::from_value(json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"packages": [{"package-id": APPLICATION, "package-version": "1.0.0"}]},
        "components": components,
        "routes": [], "attachments": {},
        "host-routes": [HostRouteSet::Application],
    }))
    .expect("the fixture release is valid")
}

/// The applications the fixture environment declares: the one package of
/// its release.
fn declared() -> wamn_catalog::EnvironmentApplications {
    [(
        APP.to_owned(),
        wamn_catalog::ApplicationDefinition {
            connections: std::collections::BTreeMap::new(),
            packages: [APPLICATION.to_owned()].into(),
        },
    )]
    .into()
}

fn load(manifest: &ServingManifest) -> Arc<LoadedRelease> {
    Arc::new(
        LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "manual-live",
            ReleaseTarget::new(PROJECT_ENV_KEY, ENVIRONMENT).with_applications(declared()),
        )
        .expect("the fixture release loads"),
    )
}

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to the disposable database");

    client
}

/// The attachment of `workflow.start` in the application host route set.
fn start_attachment() -> String {
    HostRouteSet::Application
        .attachments()
        .find(|(_, attachment)| attachment.reference == START_REFERENCE)
        .map(|(id, _)| id.to_owned())
        .expect("the application set serves workflow.start")
}

struct Fixture {
    admin: Client,
    observer: sqlx::PgPool,
    url: String,
    engine: Arc<wash_runtime::engine::Engine>,
    postgres: Arc<WamnPostgres>,
    logging: Arc<WamnLogging>,
    scratch: wamn_test_infrastructure::scratch::ScratchRoot,
    read: wamn_catalog::AdmittedComponent,
    jsonata: wamn_catalog::AdmittedComponent,
    release: Arc<LoadedRelease>,
    digest: String,
    delivery: HostRouteDelivery,
    _database: wamn_test_postgres::Database,
}

async fn fixture() -> Fixture {
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
        (&wamn_control_provision::platform_principals_sql("manual.invalid").unwrap()).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("write the platform principals and the workflow user");
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:manual-live-fixture', false)",
    )
    .bind(&PlatformComponent::Provisioning.principal_id().to_string())
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "INSERT INTO platform.\"user\" (id, type, email) VALUES \
               ('{READER}', 'user', 'reader@manual.invalid'), \
               ('{CLERK}', 'user', 'clerk@manual.invalid');
             INSERT INTO platform.role (name) VALUES ('reader'), ('clerk');
             INSERT INTO platform.user_role (user_id, role_name) VALUES \
               ('{READER}', 'reader'), ('{CLERK}', 'clerk');
             INSERT INTO platform.permission (role_name, permission) VALUES \
               ('reader', '{READ_REFERENCE}'), \
               ('reader', '{START_REFERENCE}'), \
               ('clerk', '{START_REFERENCE}');
             INSERT INTO platform.environment (status) VALUES ('active');
             INSERT INTO catalog.packages ( project_env_key, package_id, package_version, artifact_digest) VALUES ( '{PROJECT_ENV_KEY}', '{APPLICATION}', '1.0.0', '{ARTIFACT_DIGEST}');
             CREATE TABLE application_data.orders (id text PRIMARY KEY, total int NOT NULL, status text NOT NULL);
             INSERT INTO application_data.orders VALUES ('o-1', 1250, 'open'), ('o-2', 300, 'closed');"
        ))).execute(&mut admin).await.map(drop)
        .expect("seed the users, roles, permissions and application rows");

    let engine = Arc::new(build_engine(&[]).unwrap());
    let admitted = |bytes: &[u8], declaration: Value| {
        let declaration: ComponentDeclaration = serde_json::from_value(declaration).unwrap();
        validate_component_admission(
            &engine,
            bytes,
            ComponentAdmissionRequest {
                declaration,
                admitted_platform_packages: BTreeSet::from(["wamn:node".to_owned()]),
                base_digests: BTreeSet::new(),
            },
        )
        .expect("admit the fixture component")
        .component
    };
    let read_bytes = read_component();
    let jsonata_bytes = jsonata_bytes();
    let read = admitted(&read_bytes, read_declaration());
    let jsonata = admitted(&jsonata_bytes, jsonata_declaration());
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    for (component, bytes) in [(&read, &read_bytes), (&jsonata, &jsonata_bytes)] {
        std::fs::write(
            local_component_path(scratch.path(), &component.component_digest).unwrap(),
            bytes,
        )
        .unwrap();
    }

    let served = manifest(Some(&read), &jsonata);
    let release = load(&served);
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

    let postgres = targeted_postgres(&mut admin, &url).await;
    let (logging, _capture) = WamnLogging::new_with_capture(&WamnLoggingConfig::default()).unwrap();
    let delivery = host(&postgres, &release);

    let observer = wamn_postgres::pool(&url, 4).await.unwrap();
    Fixture {
        admin,
        observer,
        url,
        engine,
        postgres,
        logging: Arc::new(logging),
        scratch,
        read,
        jsonata,
        release,
        digest,
        delivery,
        _database: database,
    }
}

/// The host's database access under the targeted logins of its families: the
/// application login for guest SQL, the executor for admission and the
/// queue, and the admitter for the session permission reads.
async fn targeted_postgres(admin: &mut Client, url: &str) -> Arc<WamnPostgres> {
    let mut database: String = sqlx::query("SELECT current_database()")
        .fetch_one(&mut *admin)
        .await
        .unwrap()
        .get(0);
    let mut credentials = ClassCredentials::default();
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
        credentials = credentials.with_class(class, login.to_string());
    }
    Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(credentials),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 5000,
            statement_timeout_ms: 10000,
            row_limit: 100,
        })
        .unwrap(),
    )
}

/// The application host routes of `release`.
fn host(postgres: &Arc<WamnPostgres>, release: &Arc<LoadedRelease>) -> HostRouteDelivery {
    HostRouteDelivery::new(
        Arc::clone(release),
        HostRouteHandlers::Application {
            postgres: Arc::clone(postgres),
            project: PROJECT.to_owned(),
            identity: None,
            changes: Publisher::default(),
        },
    )
}

impl Fixture {
    async fn driver(&self, release: &Arc<LoadedRelease>) -> Arc<RouterDriver> {
        let operations = Arc::new(
            OperationHost::new(
                Arc::clone(&self.engine),
                Arc::clone(&self.postgres),
                Arc::new(HttpTransport::new().unwrap()),
                Arc::new(wamn_runtime::connection_facts::ConnectionFacts::new(
                    Arc::new(wamn_runtime::connection_memory::ConnectionMemory::default()),
                    wamn_broker::Publisher::default(),
                    Arc::new(ConnectionCredentials::empty()),
                    Arc::clone(&self.postgres),
                    release.target().project_env_key.as_str(),
                    PROJECT,
                    release.target().environment.as_str(),
                )),
                Arc::clone(&self.logging),
                Arc::from([]),
                Arc::clone(release),
                Arc::new(LocalComponentSource::new(self.scratch.path().to_owned())),
                OperationHostConfig {
                    project: PROJECT.to_owned(),
                    schema: Some("application_data".to_owned()),
                    owner_prefix: "manual-live".to_owned(),
                    warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
                },
            )
            .unwrap(),
        );
        // Compile and initialize before leasing a run. Cold JSONata compilation
        // otherwise shares the first node's 30-second deadline. If it expires,
        // cancelling the load leaves the run leased for another attempt.
        let preparing = std::time::Instant::now();
        operations
            .prepare_released(&Arc::from([self.read.clone(), self.jsonata.clone()]))
            .await
            .expect("prepare the fixture release before queue execution");
        eprintln!("manual-live release preparation: {:?}", preparing.elapsed());
        Arc::new(RouterDriver::new(operations))
    }

    /// Author `document` as `wamn-ctl workflow author` does: over its own
    /// project connection.
    async fn author(&self, document: &WiringDocument) -> anyhow::Result<()> {
        author_workflow_in_databases(
            &self.url,
            &AuthorWorkflowRequest {
                org: "manual-live",
                project: "default",
                environment: ENVIRONMENT,
                application: APP,
                declared: &declared(),
                document,
                trigger: WorkflowTrigger::Manual,
                execution: WorkflowExecution::Invoker,
                created_by: READER,
                manifest_digest: &self.digest,
            },
        )
        .await
        .map(|_| ())
    }

    async fn enable(&mut self, version: u32, enabler: &str) {
        enable_workflow(
            &self.url,
            &Triple::new("acme", ENVIRONMENT, APPLICATION),
            &mut self.admin,
            &WorkflowVersion {
                org: "manual-live",
                project: "default",
                environment: ENVIRONMENT,
                application: APP,
                workflow_id: WORKFLOW,
                version,
            },
            enabler,
            &self.digest,
            &declared(),
            None,
        )
        .await
        .expect("enable the workflow version");
    }

    /// Author and enable version 1 of the workflow.
    async fn enabled(&mut self) {
        self.author(&workflow(1, READ))
            .await
            .expect("author the workflow");
        self.enable(1, READER).await;
    }

    /// The caller a host admits for a session of `user`: the grants of the
    /// roles a session signs that the user still holds, read by the session
    /// permission reader, and the one active role they give.
    async fn session(&self, user: &str) -> AuthenticatedCaller {
        let signed: Vec<wamn_identity_protocol::RoleName> = sqlx::query(
            "SELECT role_name FROM platform.user_role \
                  WHERE user_id = $1::text::uuid",
        )
        .bind(&user)
        .fetch_all(&self.observer)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<String, _>(0).parse().unwrap())
        .collect();
        let principal: PrincipalId = user.parse().unwrap();
        let held = self
            .postgres
            .session_operation_permissions(PROJECT, PROJECT_ENV_KEY, &signed, &principal)
            .await
            .expect("read the session grants");
        AuthenticatedCaller::new(
            start_attachment(),
            user,
            CredentialType::Session,
            held.admin,
            held.references.iter().cloned().collect::<HashSet<_>>(),
            held.active_role(),
        )
    }

    /// Start the workflow through `delivery` as a session of `user`: the one
    /// outcome request element of the route, its `value` or its `error`.
    async fn start_on(&self, delivery: &HostRouteDelivery, user: &str, key: &str) -> Value {
        self.start_in(delivery, APP, user, key).await
    }

    /// [`Self::start_on`] naming the application `application`.
    async fn start_in(
        &self,
        delivery: &HostRouteDelivery,
        application: &str,
        user: &str,
        key: &str,
    ) -> Value {
        let caller = self.session(user).await;
        let report = delivery
            .deliver(
                DeliveryRequest {
                    source: Source::Attachment(start_attachment()),
                    delivery_id: format!("manual-live-{key}"),
                    payload: json!([{"request_id": key, "value": {
                        "application": application, "workflow_id": WORKFLOW, "input_json": {"day": 1}, "idempotency_key": key,
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
            panic!(
                "workflow.start answers one request element: {:?}",
                report.outcome
            );
        };
        let [request_element]: [Value; 1] =
            serde_json::from_str(&body).expect("one outcome request element");
        request_element
    }

    async fn start(&self, user: &str, key: &str) -> String {
        let request_element = self.start_on(&self.delivery, user, key).await;
        request_element["value"]["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("workflow.start admits a run: {request_element}"))
            .to_owned()
    }

    /// Bind the real queue service to `release` and drain the runs pinned
    /// to it: the service stops taking turns once no run of that release is
    /// dispatched or running.
    async fn drain(&self, release: &Arc<LoadedRelease>) {
        let service = QueueService::bind(
            self.driver(release).await,
            Arc::clone(&self.postgres),
            Arc::new(WamnJetstream::from_env()),
            release,
            QueueServiceConfig {
                project: PROJECT.to_owned(),
                runner: "manual-live".to_owned(),
                lease_ttl_ms: 30_000,
            },
        )
        .await
        .expect("bind the queue service");
        let (_stop, stopping) = tokio::sync::watch::channel(true);
        let draining = std::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(60), service.serve(stopping))
            .await
            .expect("the queue drains the release's backlog")
            .expect("queue service");
        eprintln!("manual-live queue drain: {:?}", draining.elapsed());
        service.revoke();
    }

    /// `(status, result_json, fail_type)` of a run.
    async fn outcome(&self, run: &str) -> (String, Option<Value>, Option<String>) {
        let row = sqlx::query(
            "SELECT status, result_json::text, fail_type FROM wamn_run.runs WHERE run_id = $1",
        )
        .bind(&run)
        .fetch_one(&self.observer)
        .await
        .unwrap();
        (
            row.get(0),
            row.get::<Option<String>, _>(1)
                .map(|text| serde_json::from_str(&text).unwrap()),
            row.get(2),
        )
    }

    /// The facts a run must not change: the release row, the environment
    /// row and the environment policy.
    async fn environment_facts(&self) -> Vec<String> {
        sqlx::query(
            "SELECT format('release %s %s', manifest_digest, md5(canonical_bytes)) \
                   FROM catalog.releases WHERE project_env_key = $1 \
                 UNION ALL SELECT format('environment %s %s %s', status, created_at, updated_at) \
                   FROM platform.environment \
                 ORDER BY 1",
        )
        .bind(&PROJECT_ENV_KEY)
        .fetch_all(&self.observer)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
    }

    async fn application_rows(&self) -> Vec<String> {
        sqlx::query(
            "SELECT format('%s %s %s', id, total, status) FROM application_data.orders \
                  ORDER BY id",
        )
        .fetch_all(&self.observer)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
    }

    /// A run refused at its node `read`: failed, with its refusal
    /// code as its result, never completed.
    async fn assert_permission_denied(&self, run: &str) {
        let (status, result, fail_type) = self.outcome(run).await;
        assert_eq!(status, "failed");
        assert_eq!(fail_type.as_deref(), Some("terminal"));
        let error = &result.expect("a failed run records its refusal")["error"];
        assert_eq!(error["code"], "permission_denied");
        assert_eq!(error["operation"], READ);
        assert_eq!(error["run-id"], run);
    }
}

/// The positive case: author, enable as `invoker`, start through
/// `workflow.start` as a session user with one role, drain with the real
/// queue, and read the run row.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT"]
async fn a_manual_run_reads_and_shapes_as_its_invoker_with_one_role() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT",
    );
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    let before = fixture.application_rows().await;
    fixture.enabled().await;

    let caller = fixture.session(READER).await;
    assert_eq!(caller.active_role(), Some("reader"));
    let run = fixture.start(READER, "first").await;
    assert_eq!(
        fixture.start(READER, "first").await,
        run,
        "the same key and input answer the first run"
    );
    fixture.drain(&fixture.release).await;

    let (status, result, fail_type) = fixture.outcome(&run).await;
    assert_eq!((status.as_str(), fail_type), ("completed", None));
    assert_eq!(
        result,
        Some(json!([
            {"order": "o-1", "open": true, "total": 1250},
            {"order": "o-2", "open": false, "total": 300}
        ])),
        "the jsonata node shaped the rows the read answered"
    );
    assert_eq!(
        fixture.application_rows().await,
        before,
        "the read changed nothing"
    );

    let row = sqlx::query(
        "SELECT trigger_source, execution_principal_id::text, execution_role, invoked_by, \
                    invoked_by_user_id::text, service_principal_id IS NULL, manifest_digest, \
                    node_digests_json::text, package_id, wiring_id, wiring_version \
               FROM wamn_run.runs WHERE run_id = $1",
    )
    .bind(&run)
    .fetch_one(&mut fixture.admin)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>(0), "manual");
    assert_eq!(row.get::<String, _>(1), READER);
    assert_eq!(row.get::<String, _>(2), "reader");
    assert_eq!(row.get::<String, _>(3), "manual");
    assert_eq!(row.get::<String, _>(4), READER);
    assert!(
        row.get::<bool, _>(5),
        "a manual run has no service principal"
    );
    assert_eq!(row.get::<String, _>(6), fixture.digest);
    assert_eq!(
        serde_json::from_str::<Value>(&row.get::<String, _>(7)).unwrap(),
        json!({"read": fixture.read.component_digest, "shape": fixture.jsonata.component_digest})
    );
    assert_eq!(
        (
            row.get::<String, _>(8),
            row.get::<String, _>(9),
            row.get::<i32, _>(10)
        ),
        (APP.to_owned(), WORKFLOW.to_owned(), 1)
    );
}

/// Test 2: an application the environment does not declare and a stored row
/// whose hash is not its graph's refuse at `workflow.start`, and a run whose hash is not the stored row's refuses at
/// `resolve_workflow`: the queue fails it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT"]
async fn a_wrong_hash_refuses_at_start_and_fails_the_run_at_resolution() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT",
    );
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;

    // An application the environment does not declare refuses before any
    // row is read.
    let request_element = fixture
        .start_in(&fixture.delivery, "billing", READER, "undeclared")
        .await;
    assert_eq!(
        request_element["error"]["code"], "application_not_declared",
        "{request_element}"
    );

    // A row written past authorship, enabled, under another graph's hash.
    let graph = serde_json::to_string(&workflow(1, READ)).unwrap();
    let wrong = format!("sha256:{}", "e".repeat(64));
    sqlx::query(
        "INSERT INTO catalog.workflows (project_env_key, environment, application, workflow_id, \
                version, graph_json, wiring_hash, trigger, execution_type, enabled, created_by) \
             VALUES ($1, $2, $3, $4, 1, $5::text::jsonb, $6, 'manual', 'invoker', true, \
                $7::text::uuid)",
    )
    .bind(&PROJECT_ENV_KEY)
    .bind(&ENVIRONMENT)
    .bind(&APP)
    .bind(&WORKFLOW)
    .bind(&graph)
    .bind(&wrong)
    .bind(&READER)
    .execute(&mut fixture.admin)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    let request_element = fixture
        .start_on(&fixture.delivery, READER, "wrong-row")
        .await;
    assert_eq!(
        request_element["error"]["code"], "workflow_not_served",
        "{request_element}"
    );

    // Version 2, authored and enabled, and a run of it admitted under the
    // executor with a hash the stored row does not carry.
    fixture
        .author(&workflow(2, READ))
        .await
        .expect("author version 2");
    fixture.enable(2, READER).await;
    let node_digests = json!({"read": fixture.read.component_digest,
        "shape": fixture.jsonata.component_digest});
    let input = json!({"day": 1});
    let ManualRunAdmitted::Queued { run_id: run } = fixture
        .postgres
        .admit_manual_run(
            PROJECT,
            PROJECT_ENV_KEY,
            &ManualRunAdmission {
                package_id: APP,
                manifest_digest: &fixture.digest,
                environment: ENVIRONMENT,
                workflow_id: WORKFLOW,
                workflow_version: 2,
                wiring_hash: &wrong,
                execution_principal_id: READER,
                execution_role: "reader",
                invoked_by_user_id: READER,
                node_digests: &node_digests,
                idempotency_key: "manual:wrong-run",
                input: &input,
            },
        )
        .await
        .expect("admit the run")
    else {
        panic!("a new key admits a run");
    };
    fixture.drain(&fixture.release).await;
    let (status, result, fail_type) = fixture.outcome(&run).await;
    assert_eq!(status, "failed");
    assert_eq!(fail_type.as_deref(), Some("hash-invalid-bytes"));
    assert_eq!(
        result.expect("a failed run records its refusal")["error"]["code"],
        "workflow_resolution_refused"
    );
}

/// Test 3: an operation the release does not serve refuses at author, and a
/// workflow enabled against a release refuses at admission once the serving
/// release no longer carries its operation.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT"]
async fn an_operation_not_in_the_release_refuses_at_author_and_at_admission() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT",
    );
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;

    let absent = workflow(2, "orders:order/absent@1.0.0");
    let refusal = fixture
        .author(&absent)
        .await
        .expect_err("an operation the release does not serve refuses");
    let refusal = refusal
        .downcast_ref::<wamn_environment::author_workflow::AuthorWorkflowError>()
        .expect("an authorship refusal");
    assert_eq!(refusal.error_type(), AuthorWorkflowErrorType::Condition);

    fixture.enabled().await;
    let without = load(&manifest(None, &fixture.jsonata));
    let request_element = fixture
        .start_on(&host(&fixture.postgres, &without), READER, "replaced")
        .await;
    assert_eq!(
        request_element["error"]["code"], "workflow_not_served",
        "{request_element}"
    );
    let runs: i64 = sqlx::query("SELECT count(*) FROM wamn_run.runs")
        .fetch_one(&mut fixture.admin)
        .await
        .unwrap()
        .get(0);
    assert_eq!(runs, 0, "a refused start admits no run");
}

/// Test 7: a user whose one role lacks the read starts the workflow, and the
/// run fails at its first node with `permission_denied`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT"]
async fn a_start_by_a_role_without_the_read_fails_at_the_first_node() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT",
    );
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.enabled().await;

    let caller = fixture.session(CLERK).await;
    assert_eq!(caller.active_role(), Some("clerk"));
    let run = fixture.start(CLERK, "clerk").await;
    fixture.drain(&fixture.release).await;
    fixture.assert_permission_denied(&run).await;
    let role: String = sqlx::query("SELECT execution_role FROM wamn_run.runs WHERE run_id = $1")
        .bind(&run)
        .fetch_one(&mut fixture.admin)
        .await
        .unwrap()
        .get(0);
    assert_eq!(role, "clerk");
}

/// Test 8: the role's grant on the read is revoked between enable and
/// start, and the run fails at its first node with `permission_denied`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT"]
async fn a_grant_revoked_between_enable_and_start_fails_the_first_node() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT",
    );
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.enabled().await;
    sqlx::query(
        "DELETE FROM platform.permission \
              WHERE role_name = 'reader' AND permission = $1",
    )
    .bind(&READ_REFERENCE)
    .execute(&mut fixture.admin)
    .await
    .map(|result| result.rows_affected())
    .unwrap();

    let run = fixture.start(READER, "revoked").await;
    fixture.drain(&fixture.release).await;
    fixture.assert_permission_denied(&run).await;
}

/// Test 9: a queue service of another release leaves the run queued, and
/// the service of the run's release takes it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT"]
async fn a_host_of_another_release_cannot_lease_the_run() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT",
    );
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.enabled().await;
    let run = fixture.start(READER, "pinned").await;

    let mut other_manifest = manifest(Some(&fixture.read), &fixture.jsonata);
    // Host routes are release content; environment-authored workflows are not.
    // Keep the same components so the release digest alone prevents the lease.
    other_manifest.host_routes.clear();
    let other = load(&other_manifest);
    assert_ne!(
        other.release().manifest_digest.as_str(),
        fixture.digest,
        "the other release has its own digest"
    );
    fixture.drain(&other).await;
    let queued = sqlx::query("SELECT r.status, q.attempts, q.lease_expires_at IS NULL \
               FROM wamn_run.runs AS r JOIN wamn_run.run_queue AS q USING (project_env_key, run_id) \
              WHERE r.run_id = $1").bind(&run).fetch_one(&mut fixture
        .admin)
        .await
        .unwrap();
    assert_eq!(
        (
            queued.get::<String, _>(0),
            queued.get::<i32, _>(1),
            queued.get::<bool, _>(2)
        ),
        ("dispatched".to_owned(), 0, true),
        "the run stays queued and unleased"
    );

    fixture.drain(&fixture.release).await;
    assert_eq!(fixture.outcome(&run).await.0, "completed");
}

/// Test 10: author, enable and start change neither the release row nor
/// the environment row nor its policy.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT"]
async fn author_enable_and_start_leave_the_release_and_the_environment_unchanged() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_JSONATA_COMPONENT",
    );
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    let before = fixture.environment_facts().await;
    assert_eq!(before.len(), 3, "{before:?}");

    fixture
        .author(&workflow(1, READ))
        .await
        .expect("author the workflow");
    assert_eq!(fixture.environment_facts().await, before, "after author");
    fixture.enable(1, READER).await;
    assert_eq!(fixture.environment_facts().await, before, "after enable");
    let run = fixture.start(READER, "facts").await;
    fixture.drain(&fixture.release).await;
    assert_eq!(fixture.outcome(&run).await.0, "completed");
    assert_eq!(fixture.environment_facts().await, before, "after start");
    assert_eq!(
        fixture.release.release().manifest_digest.as_str(),
        fixture.digest
    );
}
