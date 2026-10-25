//! The entry boundary through a hand-written guest and private PostgreSQL.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde_json::json;
use tokio::sync::oneshot;
use wamn_catalog::{
    AttachmentTarget, AttachmentType, ComponentDescriptor, ComponentSqlField,
    ComponentSqlStatement, ComponentSqlValueType, DefinitionHash, OperationType, PackageCoordinate,
    ServingAttachment, ServingComponent, ServingComponentOperation, ServingManifest,
    ServingRelease, ServingRoute, ServingWiring,
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
    LIVE_PRINCIPAL, ensure_live_user_rows, live_guest_url,
};
use crate::plugins::wamn_postgres::{
    ClassCredentials, ReleaseIdentity, SessionClaims, WAMN_POSTGRES_ID, WamnPostgres,
    WamnPostgresConfig,
};

const WORKLOAD: &str = "entry-fixture";
const TENANT: &str = "entryfixture";
const OPERATION: &str = "base:widget/update@1.0.0";
const READ_OPERATION: &str = "base:widget/get@1.0.0";
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
    let mut manifest = ServingManifest::new(
        ServingRelease { packages: BTreeSet::from([PackageCoordinate::new("base", "1.0.0").unwrap()]) },
        BTreeSet::from([ServingComponent {
            runtime_schema: None,
            package_id: "base".into(),
            descriptor: ComponentDescriptor::named("entry", "0.1", digest('a')),
            statement_instances: BTreeMap::new(),
            operations: BTreeMap::from([(READ_OPERATION.into(), ServingComponentOperation {
                registered_operation: None,
                permissions: BTreeSet::from([READ_OPERATION.into()]),
                committed_result_schema: None,
                statements: BTreeMap::from([("read.sql".into(), ComponentSqlStatement {
                    name: "read".into(), path: "read.sql".into(),
                    sql: "SELECT value FROM widget WHERE id = 1".into(),
                    transactional: false, ..statement.clone()
                })]),
            }), (OPERATION.into(), ServingComponentOperation {
                registered_operation: Some(OPERATION.into()),
                permissions: BTreeSet::from([OPERATION.into()]),
                committed_result_schema: None,
                statements: BTreeMap::from([("update.sql".into(), statement)]),
            })]),
        }]),
        BTreeSet::from([ServingRoute {
            package_id: "base".into(), component: "entry".into(),
            operation: READ_OPERATION.into(), type_: OperationType::Get,
            reads: BTreeSet::new(), revision: None,
            idempotency: None, canonicalization: None,
        }]),
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
    manifest.composed_entry = Some(wamn_catalog::ServingComposedEntry {
        reference: format!("entries@{}", digest('e')),
        route_digest: digest('f'),
        members: BTreeSet::from([wamn_catalog::ServingEntryMember {
            package_id: "base".into(),
            component: "entry".into(),
            digest: digest('a'),
        }]),
    });
    Arc::new(
        LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "entry fixture",
            ReleaseScope::new(TENANT, "test").with_applications(
                serde_json::from_value(json!({"receiving": {"packages": ["base"]}})).unwrap(),
            ),
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
            if self.mode == 22 {
                let resolved = accessor.with(|mut access| {
                    let export = instance
                        .get_export_index(&mut access, None, "resolved-refusal")
                        .unwrap();
                    let function = instance.get_func(&mut access, export).unwrap();
                    function.typed::<(), (super::Refusal,)>(&access).unwrap();
                    function
                });
                let mut results = [Val::Bool(false)];
                resolved
                    .call_concurrent(accessor, &[], &mut results)
                    .await?;
                assert_eq!(
                    results[0],
                    Val::Variant(
                        "resolved".into(),
                        Some(Box::new(Val::String("external-evidence".into()))),
                    ),
                );
                let dropped = accessor.with(|mut access| access.get().dropped.token());
                tokio::spawn(async move {
                    dropped.cancelled().await;
                    self.reply.send(()).expect("fixture receiver exists");
                });
                return Ok(None);
            }
            let modes = if self.mode == 8 {
                vec![8, 9, 10]
            } else {
                vec![self.mode]
            };
            let mut deliveries = Vec::new();
            for mode in modes {
                let (run, caller) = accessor.with(|mut access| {
                    let caller = access.get().table.push(AuthenticatedCaller::new(
                        "entry",
                        LIVE_PRINCIPAL,
                        CredentialType::Pat,
                        false,
                        if self.mode == 3 {
                            HashSet::new()
                        } else {
                            HashSet::from(["base:widget/update".into(), "base:widget/get".into()])
                        },
                        None,
                    ))?;
                    let caller = ResourceAny::try_from_resource(caller, &mut access)?;
                    let export = instance.get_export_index(&mut access, None, "run").unwrap();
                    let run = instance.get_func(&mut access, export).unwrap();
                    Ok::<_, wash_runtime::wasmtime::Error>((run, caller))
                })?;
                let mut results = [Val::Bool(false)];
                run.call_concurrent(
                    accessor,
                    &[Val::Resource(caller), Val::U32(mode)],
                    &mut results,
                )
                .await?;
                assert!(matches!(results[0], Val::Bool(true)));
                if mode == 8 || mode == 9 {
                    let delivery = accessor.with(|mut access| {
                        let ctx = &access.get().active_ctx;
                        assert_eq!(
                            ctx.get_plugin::<WamnPostgres>(WAMN_POSTGRES_ID)
                                .operation_transaction_for(&ctx.component_id)
                                .expect("open operation has a host transaction")
                                .application
                                .as_deref(),
                            Some("receiving"),
                        );
                        ctx.get_plugin::<WamnPostgres>(WAMN_POSTGRES_ID)
                            .current_run_for(&ctx.component_id)
                            .expect("open operation has a delivery")
                    });
                    let read_id = accessor.with(|mut access| {
                        let export = instance
                            .get_export_index(&mut access, None, "delivery-id")
                            .unwrap();
                        instance.get_func(&mut access, export).unwrap()
                    });
                    let mut result = [Val::String(String::new())];
                    read_id.call_concurrent(accessor, &[], &mut result).await?;
                    let Val::String(id) = &result[0] else {
                        panic!("fixture returns the delivery id as a string");
                    };
                    assert_eq!(id, &delivery.run, "guest reads the host-minted id");
                    deliveries.push(delivery);
                }
            }
            if self.mode == 8 {
                assert_ne!(deliveries[0].run, deliveries[1].run);
                for delivery in deliveries {
                    uuid::Uuid::parse_str(&delivery.run).unwrap();
                    assert_eq!(delivery.root, delivery.run);
                    assert_eq!(delivery.depth, 0);
                }
            }
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
    ensure_live_user_rows(database.url(), &[LIVE_PRINCIPAL]).await;
    let schema = include_str!("../../../../../../../deploy/sql/app-schema.sql");
    let start = schema
        .find("CREATE TABLE IF NOT EXISTS platform.write_log")
        .unwrap();
    let end = schema
        .find("GRANT DELETE ON platform.write_log TO wamn_app;")
        .unwrap()
        + "GRANT DELETE ON platform.write_log TO wamn_app;".len();
    database.execute(&[
        &schema[start..end],
        "DO $$ BEGIN CREATE ROLE wamn_db_owner; EXCEPTION WHEN duplicate_object THEN NULL; END $$;",
        include_str!("../../../../../../../deploy/sql/record-history.sql"),
        include_str!("../../../../../../../deploy/sql/record-history-app-grants.sql"),
        "CREATE TABLE widget(id int PRIMARY KEY, value int NOT NULL, \
           CONSTRAINT application_claim CHECK (value = 1 OR current_setting('app.application', true) IS NOT DISTINCT FROM 'receiving')); \
         INSERT INTO widget VALUES (1, 1); \
         SELECT wamn_history.create_history_table('public', 'widget', false); \
         CREATE TRIGGER widget_history AFTER UPDATE ON widget FOR EACH ROW \
           EXECUTE FUNCTION wamn_history.log_row_change(); \
         GRANT SELECT, UPDATE ON widget TO wamn_app; \
         GRANT INSERT ON widget_history TO wamn_app; \
         GRANT USAGE ON SEQUENCE widget_history_position_seq TO wamn_app;",
    ]).unwrap();
    let release = release();
    let postgres = WamnPostgres::new(WamnPostgresConfig {
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
            WORKLOAD,
            Arc::clone(&release),
            &entry_start(&release),
            "entry",
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
            WORKLOAD,
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
                    WitInterface::from("wamn:operation/operation@0.1.0"),
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
async fn resolved_refusal_carries_its_basis_across_wit() {
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 1000,
            statement_timeout_ms: 1000,
            row_limit: 10,
        })
        .unwrap(),
    );
    run_guest(postgres, Arc::new(LoadedRelease::control_root()), 22).await;
}

#[tokio::test]
async fn anonymous_read_authorization_needs_no_database_credentials() {
    let release = release();
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 1000,
            statement_timeout_ms: 1000,
            row_limit: 10,
        })
        .unwrap(),
    );
    postgres
        .bind_operation_workload(
            WORKLOAD,
            Arc::clone(&release),
            &entry_start(&release),
            "entry",
            SessionClaims {
                tenant: TENANT.into(),
                release: Some(ReleaseIdentity::from_loaded(&release)),
                ..SessionClaims::default()
            },
        )
        .unwrap();
    run_guest(Arc::clone(&postgres), release, 19).await;
    assert!(postgres.operation_transactions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reads_authorize_without_a_transaction_and_refuse_begin() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    for _ in 0..2 {
        run_guest(Arc::clone(&postgres), Arc::clone(&release), 18).await;
    }
    let count: i64 = connect(&database)
        .await
        .query_one("SELECT count(*) FROM platform.write_log", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
    assert!(postgres.operation_transactions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unbound_control_entry_links_but_refuses_every_operation_call() {
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 1000,
            statement_timeout_ms: 1000,
            row_limit: 10,
        })
        .unwrap(),
    );
    run_guest(
        Arc::clone(&postgres),
        Arc::new(LoadedRelease::control_root()),
        21,
    )
    .await;
    assert!(postgres.operation_workloads.read().unwrap().is_empty());
    assert!(postgres.operation_transactions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn wal_position_without_begin_refuses() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 16).await;
    assert_eq!(value(&database).await, 1);
}

#[tokio::test]
async fn wal_position_inside_an_open_operation_preserves_its_transaction() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 17).await;
    assert_eq!(value(&database).await, 2);
}

#[tokio::test]
async fn rollback_without_begin_refuses() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 13).await;
    assert_eq!(value(&database).await, 1);
}

#[tokio::test]
async fn rollback_removes_the_claim_and_statement_effects() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(Arc::clone(&postgres), release, 14).await;
    assert_eq!(value(&database).await, 1);
    assert!(postgres.operation_transactions.lock().unwrap().is_empty());
    let count: i64 = connect(&database)
        .await
        .query_one("SELECT count(*) FROM platform.write_log", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
}

#[tokio::test]
async fn rollback_then_begin_on_the_same_store_runs_the_unclaimed_key() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 15).await;
    assert_eq!(value(&database).await, 2);
    let result: String = connect(&database)
        .await
        .query_one("SELECT result FROM platform.write_log", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(result, RESULT);
}

#[tokio::test]
async fn commit_without_begin_refuses() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 0).await;
    assert_eq!(value(&database).await, 1);
}

#[tokio::test]
async fn begin_while_open_refuses_and_leaves_the_open_operation() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 1).await;
    assert_eq!(value(&database).await, 4);
}

#[tokio::test]
async fn begin_commit_begin_on_the_same_store_uses_new_transaction_and_delivery() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 8).await;
    assert_eq!(value(&database).await, 3);
    let client = connect(&database).await;
    let transactions: i64 = client
        .query_one(
            "SELECT count(DISTINCT transaction_id) FROM widget_history",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(transactions, 2);
    let keys: Vec<String> = client
        .query(
            "SELECT idempotency_key FROM platform.write_log ORDER BY idempotency_key",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(keys, ["key", "ney"]);
}

#[tokio::test]
async fn refused_begin_then_begin_on_the_same_store_is_accepted() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 11).await;
    assert_eq!(value(&database).await, 2);
}

#[tokio::test]
async fn refused_commit_then_begin_on_the_same_store_is_accepted() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    database
        .execute(&[
            "ALTER TABLE platform.write_log ADD CONSTRAINT refuse_first_finish \
         CHECK (result IS NULL OR idempotency_key <> 'key')",
        ])
        .unwrap();
    run_guest(postgres, release, 12).await;
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
        .query_one("SELECT count(*) FROM platform.write_log", &[])
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
            "SELECT operation, idempotency_key, request, result FROM platform.write_log",
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
            "UPDATE platform.write_log SET request = 'different'::bytea",
            &[],
        )
        .await
        .unwrap();
    run_guest(Arc::clone(&postgres), Arc::clone(&release), 6).await;
    client
        .execute(
            "UPDATE platform.write_log SET request = $1, result = NULL",
            &[&vec![255_u8, 0, 128]],
        )
        .await
        .unwrap();
    run_guest(postgres, release, 7).await;
    assert_eq!(value(&database).await, 2);
}

#[tokio::test]
async fn anonymous_caller_begins_an_operation_without_a_grant() {
    let _lock = wamn_test_postgres::lock();
    let (_database, postgres, release) = fixture().await;
    run_guest(postgres, release, 19).await;
}

#[tokio::test]
async fn anonymous_caller_on_a_granted_operation_refuses() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres, release) = fixture().await;
    run_guest(postgres, release, 20).await;
    assert_eq!(value(&database).await, 1);
}

/// The entry reference a host with the fixture component base starts.
fn entry_start(release: &LoadedRelease) -> String {
    wamn_catalog::composed_entry_start_reference(
        Some("registry.example/components"),
        &release
            .manifest()
            .composed_entry
            .as_ref()
            .unwrap()
            .reference,
    )
    .unwrap()
}

#[test]
fn entry_operations_use_the_application_of_their_own_package() {
    let original = release();
    let mut manifest = original.manifest().clone();
    let mut other = manifest.components.first().unwrap().clone();
    other.package_id = "other".into();
    let mut operation = other.operations.remove(OPERATION).unwrap();
    other.operations.clear();
    let other_operation = "other:widget/update@1.0.0";
    operation.registered_operation = Some(other_operation.into());
    operation.permissions = BTreeSet::from([other_operation.into()]);
    other.operations.insert(other_operation.into(), operation);
    manifest
        .release
        .packages
        .insert(PackageCoordinate::new("other", "1.0.0").unwrap());
    manifest
        .composed_entry
        .as_mut()
        .unwrap()
        .members
        .insert(wamn_catalog::ServingEntryMember {
            package_id: "other".into(),
            component: other.descriptor.component.clone(),
            digest: other.descriptor.component_digest.clone(),
        });
    manifest.components.insert(other);
    let release = Arc::new(
        LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "two applications",
            ReleaseScope::new(TENANT, "test").with_applications(
                serde_json::from_value(json!({
                    "receiving": {"packages": ["base"]}, "orders": {"packages": ["other"]}
                }))
                .unwrap(),
            ),
        )
        .unwrap(),
    );
    let postgres = WamnPostgres::with_provider(Arc::new(
        super::super::StaticCredentialProvider::new(HashMap::new(), None),
    ));
    postgres
        .bind_operation_workload(
            WORKLOAD,
            Arc::clone(&release),
            &entry_start(&release),
            "entry",
            SessionClaims {
                tenant: TENANT.into(),
                release: Some(ReleaseIdentity::from_loaded(&release)),
                ..SessionClaims::default()
            },
        )
        .unwrap();
    let workloads = postgres.operation_workloads.read().unwrap();
    assert_eq!(workloads[WORKLOAD].applications[OPERATION], "receiving");
    assert_eq!(workloads[WORKLOAD].applications[other_operation], "orders");
}

#[test]
fn entry_binding_requires_the_started_reference_and_component_name() {
    let release = release();
    let postgres = WamnPostgres::new(WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 1000,
        statement_timeout_ms: 1000,
        row_limit: 10,
    })
    .unwrap();
    let claims = SessionClaims {
        tenant: TENANT.into(),
        release: Some(ReleaseIdentity::from_loaded(&release)),
        ..SessionClaims::default()
    };
    assert!(
        postgres
            .bind_operation_workload(
                WORKLOAD,
                Arc::clone(&release),
                "registry.example/other@sha256:bad",
                "entry",
                claims.clone()
            )
            .is_err()
    );
    postgres
        .bind_operation_workload(
            WORKLOAD,
            Arc::clone(&release),
            &entry_start(&release),
            "entry",
            claims,
        )
        .unwrap();
    // Every operation binds its package's name from the environment map.
    assert_eq!(
        postgres
            .operation_workloads
            .read()
            .unwrap()
            .get(WORKLOAD)
            .unwrap()
            .applications[OPERATION],
        "receiving"
    );
    assert!(
        postgres
            .bind_entry_instance(WORKLOAD, "member", "member")
            .is_err()
    );
    postgres
        .bind_entry_instance(WORKLOAD, "entry-instance", "entry")
        .unwrap();
    postgres.forget_operation_start(WORKLOAD);
    assert!(
        postgres
            .bind_entry_instance(WORKLOAD, "late-instance", "entry")
            .is_err()
    );
}
