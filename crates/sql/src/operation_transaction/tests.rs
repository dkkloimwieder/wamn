use sqlx::Row as _;
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::oneshot;

use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{GuestCall, GuestCallFuture};
use wash_runtime::host::HostRef;
use wash_runtime::host::http::{HostHandler, NullServer};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings};
use wash_runtime::types::{Component as WorkloadComponent, Workload};
use wash_runtime::wasmtime::component::{Accessor, Instance, Val};
use wash_runtime::wit::WitInterface;

use crate::session_identity::tests::{LIVE_PRINCIPAL, ensure_live_user_rows, live_guest_url};
use crate::{
    ClassCredentials, ReleaseIdentity, SessionIdentity, WAMN_POSTGRES_ID, WamnPostgres,
    WamnPostgresConfig,
};
use wamn_authority::connection::{
    ConnectionExecutionClosure, ConnectionInvocation, ConnectionOrigin,
};
use wamn_engine::engine::build_engine;

const OPERATION_OWNER: &str = "operation-transaction-owner";
const OWNER_OPERATION: &str = "base:widget/record@1.0.0";
const PROJECT_ENV_KEY: &str = "test--dev--operationtxn";

fn invocation(operation: &str, package_id: &str) -> ConnectionInvocation {
    ConnectionInvocation {
        origin: ConnectionOrigin {
            package_id: "overlay".into(),
            component_digest: format!("sha256:{}", "a".repeat(64))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            component: "overlay".into(),
            interface_version: "0.1.0".into(),
            operation: OWNER_OPERATION.into(),
        },
        entry: wamn_authority::connection::InvocationEntry::Wiring(
            wamn_authority::connection::WiringPosition {
                configuration: Default::default(),
                package_id: "overlay".into(),
                wiring_hash: format!("sha256:{}", "a".repeat(64))
                    .parse::<wamn_digest::Digest>()
                    .unwrap(),
                wiring_id: "widget".into(),
                wiring_version: 1,
                node_id: "record".into(),
                occurrence: 0,
            },
        ),
        package_id: package_id.into(),
        component_digest: format!("sha256:{}", "b".repeat(64))
            .parse::<wamn_digest::Digest>()
            .unwrap(),
        component: package_id.into(),
        operation: operation.into(),
        closure: ConnectionExecutionClosure::Released,
        effects: None,
    }
}

struct ActiveComponent<'a> {
    accessor: &'a Accessor<SharedCtx>,
    previous: Arc<str>,
}

impl Drop for ActiveComponent<'_> {
    fn drop(&mut self) {
        self.accessor.with(|mut access| {
            access.get().active_ctx.component_id = Arc::clone(&self.previous);
        });
    }
}

struct RunOwner {
    reply: oneshot::Sender<Val>,
    scope: &'static str,
}

impl GuestCall for RunOwner {
    fn describe(&self) -> &'static str {
        OWNER_OPERATION
    }

    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async move {
            let (run, previous) = accessor.with(|mut access| {
                let run = instance
                    .get_export_index(&mut access, None, "run")
                    .ok_or_else(|| wash_runtime::wasmtime::format_err!("missing owner run"))?;
                let run = instance.get_func(&mut access, run).ok_or_else(|| {
                    wash_runtime::wasmtime::format_err!("owner run is not a function")
                })?;
                let active = &mut access.get().active_ctx;
                let previous = std::mem::replace(&mut active.component_id, Arc::from(self.scope));
                Ok::<_, wash_runtime::wasmtime::Error>((run, previous))
            })?;
            let _active = ActiveComponent { accessor, previous };
            let mut results = [Val::Bool(false)];
            run.call_concurrent(accessor, &[], &mut results).await?;
            self.reply
                .send(results.into_iter().next().expect("one owner result"))
                .map_err(|_| wash_runtime::wasmtime::format_err!("owner reply abandoned"))?;
            Ok(None)
        })
    }
}

/// An owner at 0.3.0 that works in the host transaction: it takes the
/// operation transaction, sees `begin`, `run` and `run-stream` refused, runs
/// `statement` in the operation transaction, and sees its own `commit` refused. It returns true when
/// every step answered as expected, and drops its handle. Without a host
/// transaction it runs `statement` and begins a transaction, trapping if
/// either fails, and returns false.
fn operation_owner_component(statement: &str) -> String {
    let length = statement.len();
    format!(
        r#"(component
  (import "wamn:postgres/types@0.4.0" (instance $types
    (type $pg-error' (variant (case "serialization-failure") (case "connection-unavailable")
      (case "statement-timeout") (case "unique-violation" string)
      (case "foreign-key-violation" string) (case "check-violation" string)
      (case "exclusion-violation" string) (case "permission-denied")
      (case "query-error" (tuple string string))))
    (export "pg-error" (type $pg-error (eq $pg-error')))))
  (alias export $types "pg-error" (type $pg-error))
  (import "wamn:postgres/statements@0.4.0" (instance $statements
    (export "transaction" (type $transaction (sub resource)))
    (export "operation-transaction" (func async
      (result (result (own $transaction) (error $pg-error)))))
    (export "[method]transaction.query-batch" (func async (param "self" (borrow $transaction))
      (param "sql" string) (result (result (error $pg-error)))))))
  (alias export $statements "transaction" (type $transaction))
  (core module $memory
    (memory (export "memory") 1)
    (data (i32.const 512) "{statement}")
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 1024))
  (core instance $memory (instantiate $memory))
  (core func $transaction (canon lower (func $statements "operation-transaction")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $query (canon lower (func $statements "[method]transaction.query-batch")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $drop (canon resource.drop $transaction))
  (core func $return (canon task.return (result bool)))
  (core module $main
    (import "memory" "memory" (memory 1))
    (import "host" "transaction" (func $transaction (param i32)))
    (import "host" "query" (func $query (param i32 i32 i32 i32)))
    (import "host" "drop" (func $drop (param i32)))
    (import "host" "return" (func $return (param i32)))
    (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
    (func (export "run") (result i32) (local $handle i32)
      i32.const 64 call $transaction
      i32.const 64 i32.load8_u if
        i32.const 0 call $return i32.const 0 return end
      i32.const 68 i32.load local.set $handle
      local.get $handle i32.const 512 i32.const {length} i32.const 128 call $query
      i32.const 128 i32.load8_u if unreachable end
      local.get $handle call $drop
      i32.const 1 call $return i32.const 0))
  (core instance $main (instantiate $main
    (with "memory" (instance $memory))
    (with "host" (instance (export "transaction" (func $transaction))
      (export "query" (func $query)) (export "drop" (func $drop))
      (export "return" (func $return))))))
  (func (export "run") async (result bool)
    (canon lift (core func $main "run") (memory $memory "memory")
      (realloc (func $memory "realloc")) async (callback (func $main "callback")))))"#
    )
}

/// The operation transaction of `docs/history/host-transaction.md` 4.1: the host
/// begins it, the guest works in it, and only the host finishes it. A commit
/// makes the guest's update visible, and a rollback undoes it.
#[sqlx::test(migrations = false)]
async fn the_host_owns_the_operation_transaction_and_the_guest_works_in_it(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let guest_url = live_guest_url(database.as_str(), &PROJECT_ENV_KEY.parse().unwrap()).await;
    ensure_live_user_rows(database.as_str(), &[LIVE_PRINCIPAL]).await;
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(guest_url)),
            guest_pool_max_size: 2,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
        })
        .expect("live postgres plugin"),
    );
    let release = (format!("sha256:{}", "c".repeat(64)))
        .parse::<wamn_digest::Digest>()
        .unwrap();
    postgres
        .bind_session_identity(
            OPERATION_OWNER,
            &SessionIdentity {
                project_env_key: Some(PROJECT_ENV_KEY.parse().unwrap()),
                user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
                operation: Some(OWNER_OPERATION.into()),
                ..SessionIdentity::default()
            },
        )
        .await
        .expect("identity bind");
    postgres
        .set_release_identity(OPERATION_OWNER, ReleaseIdentity::for_test(release))
        .expect("release binds");
    postgres
        .bind_invocation(OPERATION_OWNER, invocation(OWNER_OPERATION, "base"))
        .expect("invocation binds");
    let sql = "UPDATE operation_transaction_roundtrip SET value = value + 1 RETURNING value";
    let mut admin = wamn_postgres::connect(database.as_str()).await.unwrap();

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE TABLE operation_transaction_roundtrip(value int NOT NULL); \
         INSERT INTO operation_transaction_roundtrip VALUES (1); \
         GRANT SELECT, UPDATE ON operation_transaction_roundtrip TO wamn_app")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .unwrap();
    let mut value = async || -> i32 {
        sqlx::query("SELECT value FROM operation_transaction_roundtrip")
            .fetch_one(&mut admin)
            .await
            .unwrap()
            .get(0)
    };

    let engine = build_engine(&[]).expect("engine");
    let workload = engine
        .initialize_workload(
            "operation-transaction-roundtrip",
            Workload {
                namespace: "test".into(),
                name: "operation-transaction-roundtrip".into(),
                annotations: HashMap::new(),
                service: None,
                components: vec![WorkloadComponent {
                    name: "owner".into(),
                    bytes: wat::parse_str(operation_owner_component(sql))
                        .unwrap()
                        .into(),
                    ..WorkloadComponent::default()
                }],
                host_interfaces: vec![
                    WitInterface::from("wamn:postgres/types@0.4.0"),
                    WitInterface::from("wamn:postgres/statements@0.4.0"),
                ],
                volumes: Vec::new(),
            },
        )
        .expect("native component compiles");
    let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> = HashMap::from([(
        WAMN_POSTGRES_ID,
        Arc::clone(&postgres) as Arc<dyn HostPlugin>,
    )]);
    let egress: Arc<dyn HostHandler> = Arc::new(NullServer::default());
    let workload = workload
        .resolve(
            Some(&plugins),
            &PluginBindings::new(),
            &HostRef::from_handler(&egress),
            &Meters::new(MeterKind::Off),
        )
        .await
        .expect("postgres links through native workload resolution");
    let component_id = workload
        .components()
        .read()
        .await
        .values()
        .find(|component| component.name() == "owner")
        .expect("owner component exists")
        .id()
        .to_owned();
    let target = workload
        .dispatch_target(&component_id, WAMN_POSTGRES_ID)
        .await
        .expect("native owner dispatch target");
    let run_owner = async || {
        let (reply, receive) = oneshot::channel();
        target
            .dispatch(RunOwner {
                reply,
                scope: OPERATION_OWNER,
            })
            .await
            .expect("native owner dispatch");
        receive.await.expect("owner reply")
    };

    let owner = run_owner().await;
    assert!(
        matches!(owner, Val::Bool(false)),
        "without a host transaction the owner gets none: {owner:?}"
    );
    assert_eq!(
        value().await,
        1,
        "without a host transaction SQL is refused"
    );

    for commit in [true, false] {
        let mut transaction = postgres
            .begin_operation_transaction(OPERATION_OWNER)
            .await
            .expect("the host begins the operation transaction");
        postgres
            .bind_operation_transaction(OPERATION_OWNER, &mut transaction)
            .expect("the transaction binds");
        let owner = run_owner().await;
        assert!(
            matches!(owner, Val::Bool(true)),
            "the owner works in the host transaction: {owner:?}"
        );
        postgres.revoke_operation_transaction(OPERATION_OWNER);
        assert_eq!(
            value().await,
            if commit { 1 } else { 2 },
            "nothing commits before the host"
        );
        if commit {
            transaction.commit().await.expect("the host commits");
            assert_eq!(value().await, 2, "the owner's update commits");
        } else {
            transaction.rollback().await.expect("the host rolls back");
            assert_eq!(value().await, 2, "the owner's update rolls back");
        }
    }
}
