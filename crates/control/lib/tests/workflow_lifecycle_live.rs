//! Live tests of environment workflows (docs/plan/workflows.md R3, R8, epic 1
//! issue 1): authorship against the serving release and a green gate report,
//! one enabled version, the lifecycle lock, the enable-role checks, the
//! immutability trigger and the `workflow` user row.
//!
//! The project database carries the production catalog and platform schemas
//! (`wamn_project_state::test_database::tenant_platform`), and the control
//! database the production control store, which holds the gate reports. The
//! lifecycle lock is an advisory lock, so the project database also serves as
//! the system database of the lock.

use std::collections::{BTreeMap, BTreeSet};

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ComponentPackageScope, PackageCoordinate,
    ServingComponent, ServingManifest, ServingRelease, WiringDocument, WiringNode, WiringTerminal,
};
use wamn_control::author_workflow::{
    AuthorWorkflowError, AuthorWorkflowErrorType, AuthorWorkflowRequest, WorkflowExecution,
    WorkflowTrigger, author_workflow,
};
use wamn_control::environment::lock::LifecycleLock;
use wamn_control::workflow_lifecycle::{
    APPLY_IN_PROGRESS, WorkflowLifecycleErrorType, WorkflowVersion, disable_workflow,
    enable_workflow,
};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};
use wamn_control_registry::Triple;

const TENANT: &str = "workflow-tenant";
const ENVIRONMENT: &str = "dev";
const APPLICATION: &str = "orders";

/// The applications the fixture environment declares.
fn declared() -> wamn_catalog::EnvironmentApplications {
    [(
        APPLICATION.to_owned(),
        wamn_catalog::ApplicationDefinition {
            packages: [APPLICATION.to_owned()].into(),
        },
    )]
    .into()
}
const OPERATION: &str = "orders:order/get@1.0.0";
const DOMAIN: &str = "example.invalid";
const ALICE: &str = "11111111-1111-1111-1111-111111111111";
const BOB: &str = "22222222-2222-2222-2222-222222222222";

struct Fixture {
    project: Client,
    control: Client,
    project_url: String,
    digest: String,
    _databases: (wamn_test_postgres::Database, wamn_test_postgres::Database),
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to a disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// One release whose application serves one registered operation.
fn release() -> ServingManifest {
    let fact = AdmittedComponent {
        scope: ComponentPackageScope {
            tenant_id: TENANT.to_owned(),
            package_id: APPLICATION.to_owned(),
            package_version: "1.0.0".to_owned(),
        },
        component: "orders_data".to_owned(),
        interface_version: "0.1".to_owned(),
        operations: BTreeMap::from([(
            OPERATION.to_owned(),
            AdmittedComponentOperation {
                committed_result_schema: None,
                registered_operation: Some(OPERATION.to_owned()),
                host_transaction: false,
                input_ports: Vec::new(),
                output_ports: Vec::new(),
                parameters: Vec::new(),
                statements: BTreeMap::new(),
            },
        )]),
        component_digest: format!("sha256:{}", "1".repeat(64)),
        imports: Vec::new(),
        imports_fingerprint: format!("sha256:{}", "6".repeat(64)),
        effects: Vec::new(),
    };
    ServingManifest::new(
        ServingRelease {
            packages: BTreeSet::from([PackageCoordinate::new(APPLICATION, "1.0.0").unwrap()]),
        },
        BTreeSet::from([ServingComponent::project(&fact, Vec::new()).unwrap()]),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .expect("the fixture release is valid")
}

/// A one-node workflow over the application's operation, or over `component`.
fn workflow(id: &str, version: u32, node: &str, component: &str) -> WiringDocument {
    WiringDocument::new(
        id,
        version,
        node,
        BTreeMap::from([(
            node.to_owned(),
            WiringNode {
                component: component.to_owned(),
                interface_version: "0.1".to_owned(),
                operation: OPERATION.to_owned(),
                operation_dependency: None,
                params: BTreeMap::new(),
                terminal: Some(WiringTerminal::Respond),
            },
        )]),
        Vec::new(),
        Vec::new(),
    )
    .expect("the fixture workflow is structurally valid")
}

async fn fixture() -> Fixture {
    let project_database = wamn_project_state::test_database::tenant_platform();
    let control_database = wamn_control_provision::test_database::system();
    let project_url = project_database.url().to_owned();
    let project = connect(&project_url).await;
    let control = connect(control_database.url()).await;
    control
        .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("scope the control seed session");

    let manifest = release();
    let bytes = manifest.canonical_bytes();
    let digest = manifest.digest().as_str().to_owned();
    project
        .execute(
            "INSERT INTO catalog.releases (tenant_id, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3)",
            &[&TENANT, &digest, &bytes],
        )
        .await
        .expect("install the serving release");

    // The platform rows twice: the second run writes nothing.
    let principals =
        wamn_control_provision::platform_principals_sql(DOMAIN).expect("a domain renders");
    for _ in 0..2 {
        project
            .batch_execute(&format!("BEGIN;\n{principals}COMMIT;"))
            .await
            .expect("write the platform principal rows");
    }
    project
        .batch_execute(&format!(
            "BEGIN;\n{}\
             INSERT INTO platform.\"user\" (id, type, email) VALUES \
               ('{ALICE}', 'user', 'alice@{DOMAIN}'), \
               ('{BOB}', 'user', 'bob@{DOMAIN}');\n\
             INSERT INTO platform.role (tenant_id, name) VALUES \
               ('{TENANT}', 'admin'), ('{TENANT}', 'clerk');\n\
             COMMIT;",
            bind_platform_principal_sql(PlatformComponent::Provisioning)
        ))
        .await
        .expect("seed the users and roles");
    Fixture {
        project,
        control,
        project_url,
        digest,
        _databases: (project_database, control_database),
    }
}

impl Fixture {
    async fn green(&self, document: &WiringDocument) {
        self.control
            .execute(
                "INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary) \
                 VALUES ($1, $2, true, '{}'::jsonb)",
                &[&TENANT, &document.wiring_hash().as_str()],
            )
            .await
            .expect("record a green gate report");
    }

    async fn author(
        &mut self,
        document: &WiringDocument,
        execution: WorkflowExecution<'_>,
    ) -> Result<(), AuthorWorkflowError> {
        self.author_trigger(document, execution, WorkflowTrigger::Manual)
            .await
    }

    async fn author_trigger(
        &mut self,
        document: &WiringDocument,
        execution: WorkflowExecution<'_>,
        trigger: WorkflowTrigger<'_>,
    ) -> Result<(), AuthorWorkflowError> {
        let transaction = self.project.transaction().await.unwrap();
        author_workflow(
            &self.control,
            &transaction,
            &AuthorWorkflowRequest {
                tenant_id: TENANT,
                environment: ENVIRONMENT,
                application: APPLICATION,
                declared: &declared(),
                document,
                trigger,
                execution,
                created_by: ALICE,
                manifest_digest: &self.digest,
            },
        )
        .await?;
        transaction.commit().await.unwrap();
        Ok(())
    }

    async fn grant(&self, user: &str, role: &str) {
        self.project
            .batch_execute(&format!(
                "BEGIN;\n{}\
                 INSERT INTO platform.user_role (tenant_id, user_id, role_name) \
                 VALUES ('{TENANT}', '{user}', '{role}');\nCOMMIT;",
                bind_platform_principal_sql(PlatformComponent::Provisioning)
            ))
            .await
            .expect("grant the role");
    }

    async fn enable(
        &mut self,
        id: &str,
        version: u32,
        enabler: &str,
    ) -> Result<(), wamn_control::workflow_lifecycle::WorkflowLifecycleError> {
        let digest = self.digest.clone();
        enable_workflow(
            &self.project_url,
            &triple(),
            &mut self.project,
            &key(id, version),
            enabler,
            &digest,
            &declared(),
            None,
        )
        .await
    }

    async fn enabled(&self, id: &str) -> Vec<i32> {
        self.project
            .query(
                "SELECT version FROM catalog.workflows \
                  WHERE tenant_id = $1 AND workflow_id = $2 AND enabled ORDER BY version",
                &[&TENANT, &id],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect()
    }
}

fn triple() -> Triple {
    Triple::new("acme", "orders", ENVIRONMENT)
}

fn key(id: &str, version: u32) -> WorkflowVersion<'_> {
    WorkflowVersion {
        tenant_id: TENANT,
        environment: ENVIRONMENT,
        application: APPLICATION,
        workflow_id: id,
        version,
    }
}

#[tokio::test]
async fn authorship_converges_and_refuses_what_the_release_or_the_report_does_not_cover() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;

    // No green report under the graph's hash.
    let ungated = workflow("nightly", 1, "read", "orders_data");
    let refusal = fixture
        .author(&ungated, WorkflowExecution::Invoker)
        .await
        .expect_err("an ungated graph refuses");
    assert_eq!(refusal.error_type(), AuthorWorkflowErrorType::Report);

    // Same bytes converge.
    fixture.green(&ungated).await;
    fixture
        .author(&ungated, WorkflowExecution::Invoker)
        .await
        .expect("a gated graph is authored");
    fixture
        .author(&ungated, WorkflowExecution::Invoker)
        .await
        .expect("the same bytes converge");
    let (count, enabled, created_by): (i64, bool, String) = {
        let row = fixture
            .project
            .query_one(
                "SELECT count(*), bool_or(enabled), min(created_by::text) \
                   FROM catalog.workflows WHERE workflow_id = 'nightly'",
                &[],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1), row.get(2))
    };
    assert_eq!((count, enabled, created_by.as_str()), (1, false, ALICE));

    // Changed bytes under the same version.
    let changed = workflow("nightly", 1, "moved", "orders_data");
    fixture.green(&changed).await;
    let refusal = fixture
        .author(&changed, WorkflowExecution::Invoker)
        .await
        .expect_err("changed bytes under an authored version refuse");
    assert_eq!(refusal.error_type(), AuthorWorkflowErrorType::Conflict);

    // A node the release does not carry, with a green report of its own.
    let absent = workflow("nightly", 2, "read", "absent_data");
    fixture.green(&absent).await;
    let refusal = fixture
        .author(&absent, WorkflowExecution::Invoker)
        .await
        .expect_err("a node absent from the release refuses");
    assert_eq!(refusal.error_type(), AuthorWorkflowErrorType::Gate);
}

#[tokio::test]
async fn one_version_is_enabled_and_the_lock_refuses_while_held() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    let first = workflow("nightly", 1, "read", "orders_data");
    let second = workflow("nightly", 2, "again", "orders_data");
    for document in [&first, &second] {
        fixture.green(document).await;
        fixture
            .author(document, WorkflowExecution::Invoker)
            .await
            .expect("author the version");
    }

    // An invoker workflow needs no role from its enabler.
    fixture.enable("nightly", 1, BOB).await.expect("enable 1");
    assert_eq!(fixture.enabled("nightly").await, [1]);
    fixture.enable("nightly", 2, BOB).await.expect("enable 2");
    assert_eq!(
        fixture.enabled("nightly").await,
        [2],
        "enabling 2 disabled 1"
    );

    // The partial index refuses a second enabled version written directly.
    let refused = fixture
        .project
        .execute(
            "UPDATE catalog.workflows SET enabled = true \
              WHERE workflow_id = 'nightly' AND version = 1",
            &[],
        )
        .await
        .expect_err("two enabled versions refuse");
    assert_eq!(
        refused.code(),
        Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    );

    // The trigger refuses a graph change and admits a change of `enabled`.
    let refused = fixture
        .project
        .execute(
            "UPDATE catalog.workflows SET graph_json = '{}'::jsonb \
              WHERE workflow_id = 'nightly' AND version = 1",
            &[],
        )
        .await
        .expect_err("a graph change refuses");
    let database = refused.as_db_error().expect("a database refusal");
    assert_eq!(database.code().code(), "55000");
    assert_eq!(database.message(), "workflow-version-immutable");

    // A held lock refuses enable and disable, and the state stays.
    let held = LifecycleLock::acquire(&fixture.project_url, &triple())
        .await
        .expect("hold the lifecycle lock");
    let refusal = fixture
        .enable("nightly", 1, BOB)
        .await
        .expect_err("enable refuses while the lock is held");
    assert_eq!(
        refusal.error_type(),
        WorkflowLifecycleErrorType::ApplyInProgress
    );
    assert!(
        refusal.to_string().starts_with(APPLY_IN_PROGRESS),
        "{refusal}"
    );
    assert!(refusal.to_string().contains("acme/orders/dev"), "{refusal}");
    held.release().await;
    assert_eq!(fixture.enabled("nightly").await, [2]);

    let project_url = fixture.project_url.clone();
    disable_workflow(
        &project_url,
        &triple(),
        &mut fixture.project,
        &key("nightly", 2),
    )
    .await
    .expect("disable 2");
    assert!(fixture.enabled("nightly").await.is_empty());
    let refusal = disable_workflow(
        &project_url,
        &triple(),
        &mut fixture.project,
        &key("nightly", 9),
    )
    .await
    .expect_err("an absent version refuses");
    assert_eq!(refusal.error_type(), WorkflowLifecycleErrorType::NotFound);
}

#[tokio::test]
async fn enable_requires_the_role_of_the_enabler_and_of_the_workflow_user() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;

    // The workflow user row is written once, as a service, with no role.
    let row = fixture
        .project
        .query_one(
            "SELECT count(*), min(type), min(email), min(display_name) FROM platform.\"user\" \
              WHERE id = $1::text::uuid",
            &[&wamn_project_state::workflow_user_id().to_string()],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            row.get::<_, i64>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, String>(3)
        ),
        (
            1,
            "service".to_owned(),
            format!("workflow@{DOMAIN}"),
            "workflow".to_owned()
        )
    );

    let first = workflow("approve", 1, "read", "orders_data");
    let second = workflow("approve", 2, "again", "orders_data");
    for document in [&first, &second] {
        fixture.green(document).await;
        fixture
            .author(document, WorkflowExecution::Role("clerk"))
            .await
            .expect("author the version");
    }

    let refusal = fixture
        .enable("approve", 1, BOB)
        .await
        .expect_err("an enabler with neither the role nor admin refuses");
    assert_eq!(
        refusal.error_type(),
        WorkflowLifecycleErrorType::RoleNotHeldByEnabler
    );

    fixture.grant(BOB, "clerk").await;
    let refusal = fixture
        .enable("approve", 1, BOB)
        .await
        .expect_err("a workflow user without the role refuses");
    assert_eq!(
        refusal.error_type(),
        WorkflowLifecycleErrorType::RoleNotHeldByWorkflowUser
    );
    assert!(fixture.enabled("approve").await.is_empty());

    fixture
        .grant(&wamn_project_state::workflow_user_id().to_string(), "clerk")
        .await;
    fixture
        .enable("approve", 1, BOB)
        .await
        .expect("an enabler holding the role enables");
    fixture.grant(ALICE, "admin").await;
    fixture
        .enable("approve", 2, ALICE)
        .await
        .expect("an admin enables");
    assert_eq!(fixture.enabled("approve").await, [2]);
}

/// `project/0032_workflows.sql` then `project/0033_workflow_schedules.sql`
/// create on an installed database the table that `catalog-schema.sql`
/// creates on a fresh one: the same columns, constraints, indexes, triggers,
/// tenant floor, grants and guard function.
#[tokio::test]
async fn the_migration_creates_the_table_of_the_full_schema() {
    const MIGRATION: &str =
        include_str!("../../../../deploy/sql/migrations/project/0032_workflows.sql");
    const SCHEDULES: &str =
        include_str!("../../../../deploy/sql/migrations/project/0033_workflow_schedules.sql");
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
    let client = connect(database.url()).await;
    let shape = async || -> Vec<String> {
        client
            .query(
                "SELECT entry FROM ( \
                   SELECT 'column ' || attname || ' ' || format_type(atttypid, atttypmod) \
                          || ' ' || attnotnull AS entry \
                     FROM pg_attribute WHERE attrelid = 'catalog.workflows'::regclass \
                      AND attnum > 0 AND NOT attisdropped \
                   UNION ALL SELECT 'constraint ' || conname || ' ' || pg_get_constraintdef(oid) \
                     FROM pg_constraint WHERE conrelid = 'catalog.workflows'::regclass \
                   UNION ALL SELECT 'index ' || indexdef FROM pg_indexes \
                     WHERE schemaname = 'catalog' AND tablename = 'workflows' \
                   UNION ALL SELECT 'trigger ' || pg_get_triggerdef(oid) FROM pg_trigger \
                     WHERE tgrelid = 'catalog.workflows'::regclass AND NOT tgisinternal \
                   UNION ALL SELECT 'policy ' || policyname || ' ' || roles::text || ' ' \
                          || coalesce(qual, '') FROM pg_policies \
                     WHERE schemaname = 'catalog' AND tablename = 'workflows' \
                   UNION ALL SELECT 'rls ' || relrowsecurity || ' ' || relforcerowsecurity \
                          || ' ' || coalesce(relacl::text, '') \
                     FROM pg_class WHERE oid = 'catalog.workflows'::regclass \
                   UNION ALL SELECT 'function ' || coalesce(proacl::text, '') || ' ' \
                          || prosrc FROM pg_proc \
                     WHERE oid = 'catalog.guard_workflow_update()'::regprocedure \
                 ) shape ORDER BY entry COLLATE \"C\"",
                &[],
            )
            .await
            .expect("read the shape of catalog.workflows")
            .iter()
            .map(|row| row.get(0))
            .collect()
    };
    let fresh = shape().await;
    client
        .batch_execute(
            "DROP TABLE catalog.workflows; DROP FUNCTION catalog.guard_workflow_update();",
        )
        .await
        .expect("remove the table of the full schema");
    client
        .batch_execute(&format!("BEGIN;\n{MIGRATION}\nCOMMIT;"))
        .await
        .expect("apply the migration");
    client
        .batch_execute(&format!("BEGIN;\n{SCHEDULES}\nCOMMIT;"))
        .await
        .expect("apply the schedule migration");
    client
        .batch_execute(include_str!(
            "../../../../deploy/sql/migrations/project/0035_workflow_events.sql"
        ))
        .await
        .expect("apply the event migration");
    assert_eq!(shape().await, fresh);
}

async fn capture_orders(fixture: &Fixture) {
    use wamn_control_provision::sql;
    fixture
        .project
        .batch_execute(&format!(
            "CREATE SCHEMA orders; CREATE TABLE orders.orders (id int PRIMARY KEY); {}; {} {}",
            sql::ensure_entity_map_sql("orders"),
            sql::create_publication_sql("workflow_events", "orders"),
            sql::upsert_entity_map_sql("orders")
                .replace("$1", "'orders'")
                .replace("$2", "'order'")
                .replace("$3", "'orders'")
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn event_authorship_requires_a_captured_application_model() {
    use wamn_event_wire::Op;
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    capture_orders(&fixture).await;
    let graph = workflow("changed", 1, "get", "orders_data");
    fixture.green(&graph).await;
    for (source_package, entity, ops, condition) in [
        ("other", "order", vec![Op::Insert], None),
        (APPLICATION, "missing", vec![Op::Insert], None),
        (APPLICATION, "order", vec![], None),
        (APPLICATION, "order", vec![Op::Insert, Op::Insert], None),
        (APPLICATION, "order", vec![Op::Update], Some("new.[")),
        (APPLICATION, "order", vec![Op::Update], Some(" ")),
    ] {
        let error = fixture
            .author_trigger(
                &graph,
                WorkflowExecution::Invoker,
                WorkflowTrigger::Event {
                    source_package,
                    entity,
                    ops: &ops,
                    condition,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.error_type(), AuthorWorkflowErrorType::Event);
    }
    let trigger = WorkflowTrigger::Event {
        source_package: APPLICATION,
        entity: "order",
        ops: &[Op::Update, Op::Delete],
        condition: Some("new.status != old.status"),
    };
    fixture
        .author_trigger(&graph, WorkflowExecution::Invoker, trigger)
        .await
        .unwrap();
    fixture
        .author_trigger(&graph, WorkflowExecution::Invoker, trigger)
        .await
        .unwrap();
    let row = fixture.project.query_one("SELECT source_package, entity, ops, condition, enable_sequence FROM catalog.workflows WHERE workflow_id = 'changed'", &[]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), APPLICATION);
    assert_eq!(row.get::<_, String>(1), "order");
    assert_eq!(row.get::<_, Vec<String>>(2), ["update", "delete"]);
    assert_eq!(row.get::<_, String>(3), "new.status != old.status");
    assert_eq!(row.get::<_, Option<i64>>(4), None);
    for update in [
        "condition = NULL",
        "entity = 'missing'",
        "ops = ARRAY['insert']",
        "enabled = true",
        "enable_sequence = 1",
    ] {
        assert!(
            fixture
                .project
                .batch_execute(&format!(
                    "UPDATE catalog.workflows SET {update} WHERE workflow_id = 'changed'"
                ))
                .await
                .is_err()
        );
    }
}

/// The supplied broker credential needs stream management and event publication.
#[tokio::test]
#[ignore = "requires: WAMN_EVENT_NATS_URL, WAMN_EVENT_NATS_USERNAME, WAMN_EVENT_NATS_PASSWORD_FILE"]
async fn event_enable_records_stream_tail_and_requires_admin() {
    use std::time::Duration;
    use wamn_control::environment::EventBroker;
    use wamn_control_provision::events::{materializer_consumer_config, source_stream_config};
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    capture_orders(&fixture).await;
    let graph = workflow("changed", 1, "get", "orders_data");
    fixture.green(&graph).await;
    fixture
        .author_trigger(
            &graph,
            WorkflowExecution::Invoker,
            WorkflowTrigger::Event {
                source_package: APPLICATION,
                entity: "order",
                ops: &[wamn_event_wire::Op::Update],
                condition: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .enable("changed", 1, ALICE)
            .await
            .unwrap_err()
            .error_type(),
        WorkflowLifecycleErrorType::RoleNotHeldByEnabler
    );
    fixture.grant(ALICE, "admin").await;
    fixture
        .grant(&wamn_project_state::workflow_user_id().to_string(), "admin")
        .await;
    let events = EventBroker {
        nats_url: std::env::var("WAMN_EVENT_NATS_URL").unwrap(),
        nats_username: std::env::var("WAMN_EVENT_NATS_USERNAME").unwrap(),
        nats_password_file: std::env::var_os("WAMN_EVENT_NATS_PASSWORD_FILE")
            .unwrap()
            .into(),
        stream_replicas: 1,
        dup_window_secs: 120,
    };
    let scope = triple();
    let broker = async_nats::jetstream::new(
        wamn_control::event_streams::connection_options(
            &events.nats_username,
            &events.nats_password_file,
        )
        .unwrap()
        .connect(&events.nats_url)
        .await
        .unwrap(),
    );
    let config = source_stream_config(&scope, 1, Duration::from_secs(120));
    let name = config.name.clone();
    let mut stream = broker.create_stream(config).await.unwrap();
    let subject = format!(
        "evt.{}.{}.{}.order.update",
        scope.org,
        scope.project,
        scope.env.as_str()
    );
    let consumer = materializer_consumer_config("unread", &subject, Duration::from_secs(30), 5);
    let mut consumer = stream.create_consumer(consumer).await.unwrap();
    let first = broker
        .publish(subject.clone(), "before".into())
        .await
        .unwrap()
        .await
        .unwrap();
    let second = broker
        .publish(subject.clone(), "before".into())
        .await
        .unwrap()
        .await
        .unwrap();
    assert!(second.sequence > first.sequence);
    assert_eq!(consumer.info().await.unwrap().delivered.stream_sequence, 0);
    enable_workflow(
        &fixture.project_url,
        &scope,
        &mut fixture.project,
        &key("changed", 1),
        ALICE,
        &fixture.digest,
        &declared(),
        Some(&events),
    )
    .await
    .unwrap();
    let sequence = async |project: &Client| -> i64 {
        project
            .query_one(
                "SELECT enable_sequence FROM catalog.workflows WHERE workflow_id = 'changed'",
                &[],
            )
            .await
            .unwrap()
            .get(0)
    };
    assert_eq!(
        sequence(&fixture.project).await,
        i64::try_from(second.sequence).unwrap()
    );
    assert_eq!(stream.info().await.unwrap().state.consumer_count, 1);
    disable_workflow(
        &fixture.project_url,
        &scope,
        &mut fixture.project,
        &key("changed", 1),
    )
    .await
    .unwrap();
    assert_eq!(
        sequence(&fixture.project).await,
        i64::try_from(second.sequence).unwrap()
    );
    let third = broker
        .publish(subject, "after".into())
        .await
        .unwrap()
        .await
        .unwrap();
    enable_workflow(
        &fixture.project_url,
        &scope,
        &mut fixture.project,
        &key("changed", 1),
        ALICE,
        &fixture.digest,
        &declared(),
        Some(&events),
    )
    .await
    .unwrap();
    assert_eq!(
        sequence(&fixture.project).await,
        i64::try_from(third.sequence).unwrap()
    );
    assert_eq!(stream.info().await.unwrap().state.consumer_count, 1);
    assert_eq!(consumer.info().await.unwrap().delivered.stream_sequence, 0);
    broker.delete_stream(&name).await.unwrap();
}
