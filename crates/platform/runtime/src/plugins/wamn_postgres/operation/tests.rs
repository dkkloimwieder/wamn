//! The entry boundary through a hand-written guest and private PostgreSQL.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde_json::json;
use tokio::sync::oneshot;
use wamn_catalog::{
    AttachmentTarget, AttachmentType, ComponentDescriptor, ComponentSqlField,
    ComponentSqlStatement, ComponentSqlValueType, DefinitionHash, PackageCoordinate,
    ServingAttachment, ServingComponent, ServingComponentOperation, ServingManifest,
    ServingRelease, ServingWiring,
};
use wamn_engine::engine::build_engine;
use wamn_engine::flow_http_routing::{
    AuthenticatedCaller, CredentialType, FLOW_HTTP_ROUTING_ID, FlowHttpRouting, RouteInFlightLimit,
};
use wamn_engine::release_manifest::{LoadedRelease, ReleaseScope};
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{GuestCall, GuestCallFuture};
use wash_runtime::host::HostRef;
use wash_runtime::host::http::{HostHandler, NullServer};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings};
use wash_runtime::types::{Component as WorkloadComponent, Workload};
use wash_runtime::wasmtime::component::{Accessor, Instance, ResourceAny, Val};
use wash_runtime::wit::WitInterface;

use crate::plugins::wamn_postgres::claims::tests::{
    LIVE_PRINCIPAL, ensure_live_users_rows, live_guest_url,
};
use crate::plugins::wamn_postgres::{
    ClassCredentials, ReleaseIdentity, SessionClaims, WAMN_POSTGRES_ID, WamnPostgres,
    WamnPostgresConfig,
};

const WORKLOAD: &str = "entry-fixture";
const TENANT: &str = "entryfixture";
const OPERATION: &str = "base:widget/update@1.0.0";
const RESULT: &str = "{ \"value\" : 7 }";

fn release() -> Arc<LoadedRelease> {
    let digest = |c: char| format!("sha256:{}", c.to_string().repeat(64));
    let statement = ComponentSqlStatement {
        name: "update".into(),
        path: "update.sql".into(),
        sql: "UPDATE widget SET value = value + 1 WHERE id = 1 RETURNING value".into(),
        binds: Vec::new(),
        columns: vec![ComponentSqlField {
            name: "value".into(),
            value_type: ComponentSqlValueType::Int32,
            nullable: false,
        }],
        transactional: true,
    };
    let manifest = ServingManifest::new(
        ServingRelease { packages: BTreeSet::from([PackageCoordinate::new("base", "1.0.0").unwrap()]) },
        BTreeSet::from([ServingComponent {
            package_id: "base".into(),
            descriptor: ComponentDescriptor::named("entry", "0.1", digest('a')),
            statement_instances: BTreeMap::new(),
            operations: BTreeMap::from([(OPERATION.into(), ServingComponentOperation {
                registered_operation: Some(OPERATION.into()),
                permissions: BTreeSet::from([OPERATION.into()]),
                committed_result_schema: None,
                statements: BTreeMap::from([("update.sql".into(), statement)]),
            })]),
        }]),
        BTreeSet::new(),
        BTreeSet::from([ServingWiring::new("base", wamn_catalog::fixture_graph("entry", 1))]),
        BTreeMap::from([("entry".into(), ServingAttachment {
            type_: AttachmentType::Http, package_id: "base".into(),
            target: AttachmentTarget::Wiring { wiring_id: "entry".into(), wiring_version: 1 },
            definition_hash: DefinitionHash::parse(digest('c')).unwrap(),
            definition: json!({"route": {"host": "entry.localhost", "path": "/entry", "method": "POST"}}),
                auth_policy: json!({"modes": ["pat"]}),
            registered_operation: None,
        })]),
        BTreeMap::new(),
    ).unwrap();
    Arc::new(
        LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "entry fixture",
            ReleaseScope::new(TENANT, "test"),
        )
        .unwrap(),
    )
}

struct Run {
    mode: u32,
    reply: oneshot::Sender<()>,
}

impl GuestCall for Run {
    fn describe(&self) -> &'static str {
        "operation entry fixture"
    }

    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async move {
            let (run, caller) = accessor.with(|mut access| {
                access.get().active_ctx.component_id = Arc::from(WORKLOAD);
                let caller = access.get().table.push(AuthenticatedCaller::new(
                    "entry",
                    LIVE_PRINCIPAL,
                    CredentialType::Pat,
                    false,
                    if self.mode == 3 {
                        HashSet::new()
                    } else {
                        HashSet::from(["base:widget/update".into()])
                    },
                ))?;
                let caller = ResourceAny::try_from_resource(caller, &mut access)?;
                let export = instance.get_export_index(&mut access, None, "run").unwrap();
                let run = instance.get_func(&mut access, export).unwrap();
                Ok::<_, wash_runtime::wasmtime::Error>((run, caller))
            })?;
            let mut results = [Val::Bool(false)];
            run.call_concurrent(
                accessor,
                &[Val::Resource(caller), Val::U32(self.mode)],
                &mut results,
            )
            .await?;
            assert!(matches!(results[0], Val::Bool(true)));
            let dropped = accessor.with(|mut access| access.get().dropped.token());
            tokio::spawn(async move {
                dropped.cancelled().await;
                self.reply.send(()).expect("fixture receiver exists");
            });
            Ok(None)
        })
    }
}

async fn fixture() -> (
    wamn_test_postgres::Database,
    Arc<WamnPostgres>,
    Arc<LoadedRelease>,
) {
    let database = wamn_test_postgres::database();
    let url = live_guest_url(database.url(), TENANT).await;
    ensure_live_users_rows(database.url(), TENANT, &[LIVE_PRINCIPAL]).await;
    let schema = include_str!("../../../../../../../deploy/sql/app-schema.sql");
    let start = schema
        .find("CREATE TABLE IF NOT EXISTS app_system.write_log")
        .unwrap();
    let end = schema
        .find("GRANT DELETE ON app_system.write_log TO wamn_app;")
        .unwrap()
        + "GRANT DELETE ON app_system.write_log TO wamn_app;".len();
    database.execute(&[
        &schema[start..end],
        "DO $$ BEGIN CREATE ROLE wamn_db_owner; EXCEPTION WHEN duplicate_object THEN NULL; END $$;",
        include_str!("../../../../../../../deploy/sql/record-history.sql"),
        include_str!("../../../../../../../deploy/sql/record-history-app-grants.sql"),
        "CREATE TABLE widget(id int PRIMARY KEY, value int NOT NULL); \
         INSERT INTO widget VALUES (1, 1); \
         SELECT wamn_history.create_history_table('public', 'widget', false); \
         CREATE TRIGGER widget_history AFTER UPDATE ON widget FOR EACH ROW \
           EXECUTE FUNCTION wamn_history.log_row_change(); \
         GRANT SELECT, UPDATE ON widget TO wamn_app; \
         GRANT INSERT ON widget_history TO wamn_app; \
         GRANT USAGE ON SEQUENCE widget_history_position_seq TO wamn_app;",
    ]).unwrap();
    let release = release();
    let mut postgres = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(url)),
        guest_pool_max_size: 2,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 10,
    })
    .unwrap();
    postgres
        .bind_operation_workload(
            WORKLOAD.into(),
            Arc::clone(&release),
            "base",
            "entry",
            &format!("sha256:{}", "a".repeat(64)),
            SessionClaims {
                tenant: TENANT.into(),
                release: Some(ReleaseIdentity::from_loaded(&release)),
                ..SessionClaims::default()
            },
        )
        .unwrap();
    (database, Arc::new(postgres), release)
}

async fn run_guest(postgres: Arc<WamnPostgres>, release: Arc<LoadedRelease>, mode: u32) {
    let engine = build_engine(&[]).unwrap();
    let workload = engine
        .initialize_workload(
            "entry-boundary",
            Workload {
                namespace: "test".into(),
                name: "entry-boundary".into(),
                components: vec![WorkloadComponent {
                    name: "entry".into(),
                    pool_size: 0,
                    bytes: wat::parse_str(include_str!("fixture.wat")).unwrap().into(),
                    ..WorkloadComponent::default()
                }],
                host_interfaces: vec![
                    WitInterface::from("wamn:postgres/types@0.4.0"),
                    WitInterface::from("wamn:postgres/statements@0.4.0"),
                    WitInterface::from("wamn:postgres/operation@0.4.0"),
                    WitInterface::from("wamn:flow-http-routing/routing@0.1.0"),
                ],
                annotations: HashMap::new(),
                service: None,
                volumes: Vec::new(),
            },
        )
        .unwrap();
    let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> = HashMap::from([
        (WAMN_POSTGRES_ID, postgres.clone() as Arc<dyn HostPlugin>),
        (
            FLOW_HTTP_ROUTING_ID,
            Arc::new(FlowHttpRouting::new(
                Some(release),
                RouteInFlightLimit::default(),
            )) as Arc<dyn HostPlugin>,
        ),
    ]);
    let egress: Arc<dyn HostHandler> = Arc::new(NullServer::default());
    let workload = workload
        .resolve(
            Some(&plugins),
            &PluginBindings::new(),
            &HostRef::from_handler(&egress),
            &Meters::new(MeterKind::Off),
        )
        .await
        .unwrap();
    let id = workload
        .components()
        .read()
        .await
        .values()
        .find(|component| component.name() == "entry")
        .unwrap()
        .id()
        .to_owned();
    let target = workload
        .dispatch_target(&id, WAMN_POSTGRES_ID)
        .await
        .unwrap();
    let (reply, receive) = oneshot::channel();
    target.dispatch(Run { mode, reply }).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(120), receive)
        .await
        .expect("fixture Store drops")
        .unwrap();
}

async fn connect(database: &wamn_test_postgres::Database) -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn value(database: &wamn_test_postgres::Database) -> i32 {
    connect(database)
        .await
        .query_one("SELECT value FROM widget", &[])
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn commit_without_begin_refuses() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 0).await;
    assert_eq!(value(&database).await, 1);
}

#[tokio::test]
async fn a_second_begin_on_the_store_refuses() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 1).await;
    assert_eq!(value(&database).await, 2);
}

#[tokio::test]
async fn store_drop_rolls_back_the_statement_and_claim() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(Arc::clone(&postgres), release, 2).await;
    assert_eq!(value(&database).await, 1);
    assert!(postgres.operation_transactions.lock().unwrap().is_empty());
    let count: i64 = connect(&database)
        .await
        .query_one("SELECT count(*) FROM app_system.write_log", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
}

#[tokio::test]
async fn a_caller_without_the_operation_grant_refuses() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 3).await;
    assert_eq!(value(&database).await, 1);
}

#[tokio::test]
async fn guest_begin_statement_commit_writes_the_log_and_history() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(Arc::clone(&postgres), Arc::clone(&release), 4).await;
    let (client, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let row = client
        .query_one(
            "SELECT operation, idempotency_key, request, result FROM app_system.write_log",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "base:widget/update");
    assert_eq!(row.get::<_, String>(1), "key");
    assert_eq!(row.get::<_, Vec<u8>>(2), [255, 0, 128]);
    assert_eq!(row.get::<_, String>(3), RESULT);
    let row = client
        .query_one(
            "SELECT operation, changed_by::text, after->>'value' FROM widget_history",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), OPERATION);
    assert_eq!(row.get::<_, String>(1), LIVE_PRINCIPAL);
    assert_eq!(row.get::<_, String>(2), "2");
    run_guest(Arc::clone(&postgres), Arc::clone(&release), 5).await;
    assert_eq!(value(&database).await, 2);
    client
        .execute(
            "UPDATE app_system.write_log SET request = 'different'::bytea",
            &[],
        )
        .await
        .unwrap();
    run_guest(Arc::clone(&postgres), Arc::clone(&release), 6).await;
    client
        .execute(
            "UPDATE app_system.write_log SET request = $1, result = NULL",
            &[&vec![255_u8, 0, 128]],
        )
        .await
        .unwrap();
    run_guest(postgres, release, 7).await;
    assert_eq!(value(&database).await, 2);
}
