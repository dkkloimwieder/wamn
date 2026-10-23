use std::collections::HashMap;
use std::sync::Arc;

use sha2::Digest as _;
use tokio::sync::oneshot;
use wamn_catalog::ManifestDigest;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{GuestCall, GuestCallFuture};
use wash_runtime::host::HostRef;
use wash_runtime::host::http::{HostHandler, NullServer};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings};
use wash_runtime::types::{Component as WorkloadComponent, Workload};
use wash_runtime::wasmtime::component::{Accessor, Instance, Val};
use wash_runtime::wit::WitInterface;

use crate::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionInvocation, ConnectionOrigin,
};
use crate::plugins::wamn_postgres::claims::tests::{
    LIVE_PRINCIPAL, ensure_live_users_rows, live_guest_url,
};
use crate::plugins::wamn_postgres::{
    ClassCredentials, ReleaseIdentity, SessionClaims, StatementField, StatementValueType,
    VerifiedStatement, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig,
};
use wamn_engine::engine::build_engine;

const OPERATION_OWNER: &str = "operation-transaction-owner";
const OWNER_OPERATION: &str = "base:widget/record@1.0.0";
const TENANT: &str = "operationtxn";

fn invocation(operation: &str, package_id: &str) -> ConnectionInvocation {
    ConnectionInvocation {
        origin: ConnectionOrigin {
            package_id: "overlay".into(),
            component_digest: format!("sha256:{}", "a".repeat(64)),
            component: "overlay".into(),
            interface_version: "0.1.0".into(),
            operation: OWNER_OPERATION.into(),
        },
        entry: crate::plugins::connection_http::InvocationEntry::Wiring(
            crate::plugins::connection_http::WiringPosition {
                package_id: "overlay".into(),
                wiring_id: "widget".into(),
                wiring_version: 1,
                node_id: "record".into(),
                occurrence: 0,
            },
        ),
        package_id: package_id.into(),
        component_digest: format!("sha256:{}", "b".repeat(64)),
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
        r#"
(component
  (import "wamn:postgres/types@0.4.0" (instance $types
    (type $sql-value' (variant (case "null") (case "boolean" bool) (case "int32" s32)
      (case "int64" s64) (case "float64" f64) (case "text" string) (case "bytes" (list u8))
      (case "numeric" string) (case "timestamptz" string) (case "json" string) (case "uuid" string)))
    (export "sql-value" (type $sql-value (eq $sql-value')))
    (type $column' (record (field "name" string) (field "type-name" string)))
    (export "column" (type $column (eq $column')))
    (type $row-set' (record (field "columns" (list $column)) (field "rows" (list (list $sql-value))) (field "wal-position" (option string))))
    (export "row-set" (type $row-set (eq $row-set')))
    (type $pg-error' (variant (case "serialization-failure") (case "connection-unavailable")
      (case "statement-timeout") (case "row-limit-exceeded" u64) (case "unique-violation" string)
      (case "foreign-key-violation" string) (case "check-violation" string)
      (case "exclusion-violation" string) (case "permission-denied")
      (case "query-error" (tuple string string))))
    (export "pg-error" (type $pg-error (eq $pg-error')))))
  (alias export $types "sql-value" (type $sql-value))
  (alias export $types "column" (type $column))
  (alias export $types "row-set" (type $row-set))
  (alias export $types "pg-error" (type $pg-error))
  (import "wamn:postgres/statements@0.4.0" (instance $statements
    (type $want-wal' (enum "current"))
    (export "want-wal-position" (type $want-wal (eq $want-wal')))
    (type $contract-part' (enum "binds" "columns"))
    (export "contract-part" (type $contract-part (eq $contract-part')))
    (type $value-shape' (record (field "count" u32) (field "types" (list string))))
    (export "value-shape" (type $value-shape (eq $value-shape')))
    (type $contract-mismatch' (record (field "statement" string) (field "part" $contract-part)
      (field "expected" $value-shape) (field "observed" $value-shape)))
    (export "contract-mismatch" (type $contract-mismatch (eq $contract-mismatch')))
    (type $statement-error' (variant (case "unknown-statement" string)
      (case "statement-contract-mismatch" $contract-mismatch) (case "postgres" $pg-error)))
    (export "statement-error" (type $statement-error (eq $statement-error')))
    (export "transaction" (type $transaction (sub resource)))
    (export "operation-transaction" (func async
      (result (result (own $transaction) (error $statement-error)))))
    (export "begin" (func async
      (result (result (own $transaction) (error $statement-error)))))
    (export "run" (func async (param "statement" string) (param "binds" (list $sql-value)) (param "wal" (option $want-wal))
      (result (result $row-set (error $statement-error)))))
    (export "run-stream" (func async (param "statement" string) (param "binds" (list $sql-value)) (param "wal" (option $want-wal))
      (result (result (tuple (list $column) (option string) (stream (list $sql-value))
        (future (result (error $statement-error)))) (error $statement-error)))))
    (export "[method]transaction.run" (func async (param "self" (borrow $transaction))
      (param "statement" string) (param "binds" (list $sql-value))
      (result (result $row-set (error $statement-error)))))
    (export "[method]transaction.commit" (func async
      (param "self" (borrow $transaction)) (result (result (error $statement-error)))))))
  (alias export $statements "transaction" (type $transaction))
  (core module $memory
    (memory (export "memory") 1)
    (data (i32.const 512) "{statement}")
    (global $next (mut i32) (i32.const 1024))
    (func (export "realloc") (param i32 i32) (param $align i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $next
      local.get $align i32.const 1 i32.sub i32.add
      i32.const 0 local.get $align i32.sub i32.and
      local.tee $ptr local.get $size i32.add global.set $next
      local.get $ptr))
  (core instance $memory (instantiate $memory))
  (core func $operation-transaction (canon lower (func $statements "operation-transaction")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $begin (canon lower (func $statements "begin")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $run (canon lower (func $statements "run")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $run-stream (canon lower (func $statements "run-stream")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $transaction-run (canon lower (func $statements "[method]transaction.run")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $commit (canon lower (func $statements "[method]transaction.commit")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $drop (canon resource.drop $transaction))
  (core func $return (canon task.return (result bool)))
  (core module $main
    (import "memory" "memory" (memory 1))
    (import "host" "operation-transaction" (func $operation-transaction (param i32)))
    (import "host" "begin" (func $begin (param i32)))
    (import "host" "run" (func $run (param i32 i32 i32 i32 i32 i32 i32)))
    (import "host" "run-stream" (func $run-stream (param i32 i32 i32 i32 i32 i32 i32)))
    (import "host" "transaction-run" (func $transaction-run (param i32 i32 i32 i32 i32 i32)))
    (import "host" "commit" (func $commit (param i32 i32)))
    (import "host" "drop" (func $drop (param i32)))
    (import "host" "return" (func $return (param i32)))
    ;; Whether the result at $at is err(postgres(query-error((state, _)))) with
    ;; a five-byte SQLSTATE whose first four bytes are $state.
    (func $refused (param $at i32) (param $state i32) (result i32)
      local.get $at i32.load8_u i32.const 1 i32.eq
      local.get $at i32.const 8 i32.add i32.load8_u i32.const 2 i32.eq i32.and
      local.get $at i32.const 16 i32.add i32.load8_u i32.const 9 i32.eq i32.and
      local.get $at i32.const 28 i32.add i32.load i32.const 5 i32.eq i32.and
      local.get $at i32.const 24 i32.add i32.load i32.load local.get $state i32.eq i32.and)
    (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
    (func (export "run") (result i32) (local $transaction i32)
      i32.const 64 call $operation-transaction
      i32.const 64 i32.load8_u if
        ;; Outside a host transaction, run and begin work as before.
        i32.const 512 i32.const {length} i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 320 call $run
        i32.const 320 i32.load8_u if unreachable end
        i32.const 448 call $begin
        i32.const 448 i32.load8_u if unreachable end
        i32.const 456 i32.load call $drop
        i32.const 0 call $return i32.const 0 return end
      i32.const 72 i32.load local.set $transaction
      ;; "2500" of 25001, active_sql_transaction.
      i32.const 256 call $begin
      i32.const 256 i32.const 0x30303532 call $refused
      i32.eqz if i32.const 0 call $return i32.const 0 return end
      ;; run and run-stream would each commit on a connection of their own.
      i32.const 512 i32.const {length} i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 320 call $run
      i32.const 320 i32.const 0x30303532 call $refused
      i32.eqz if i32.const 0 call $return i32.const 0 return end
      i32.const 512 i32.const {length} i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 384 call $run-stream
      i32.const 384 i32.const 0x30303532 call $refused
      i32.eqz if i32.const 0 call $return i32.const 0 return end
      ;; The same statement runs in the host transaction.
      local.get $transaction i32.const 512 i32.const {length} i32.const 0 i32.const 0
      i32.const 448 call $transaction-run
      i32.const 448 i32.load8_u if i32.const 0 call $return i32.const 0 return end
      ;; "2D00" of 2D000, invalid_transaction_termination.
      local.get $transaction i32.const 192 call $commit
      i32.const 192 i32.const 0x30304432 call $refused
      i32.eqz if i32.const 0 call $return i32.const 0 return end
      local.get $transaction call $drop
      i32.const 1 call $return
      i32.const 0))
  (core instance $main (instantiate $main
    (with "memory" (instance $memory))
    (with "host" (instance
      (export "operation-transaction" (func $operation-transaction))
      (export "begin" (func $begin))
      (export "run" (func $run)) (export "run-stream" (func $run-stream))
      (export "transaction-run" (func $transaction-run))
      (export "commit" (func $commit))
      (export "drop" (func $drop)) (export "return" (func $return))))))
  (func (export "run") async (result bool)
    (canon lift (core func $main "run") (memory $memory "memory")
      (realloc (func $memory "realloc")) async (callback (func $main "callback")))))
"#
    )
}

/// The operation transaction of `docs/history/host-transaction.md` 4.1: the host
/// begins it, the guest works in it, and only the host finishes it. A commit
/// makes the guest's update visible, and a rollback undoes it.
#[tokio::test]
async fn the_host_owns_the_operation_transaction_and_the_guest_works_in_it() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let guest_url = live_guest_url(database.url(), TENANT).await;
    ensure_live_users_rows(database.url(), TENANT, &[LIVE_PRINCIPAL]).await;
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(guest_url)),
            guest_pool_max_size: 2,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 10,
        })
        .expect("live postgres plugin"),
    );
    let release = ManifestDigest::parse(format!("sha256:{}", "c".repeat(64))).unwrap();
    postgres
        .bind_session_claims(
            OPERATION_OWNER,
            &SessionClaims {
                tenant: TENANT.into(),
                user_id: Some(LIVE_PRINCIPAL.into()),
                operation: Some(OWNER_OPERATION.into()),
                ..SessionClaims::default()
            },
        )
        .await
        .expect("claims bind");
    postgres
        .set_release_identity(OPERATION_OWNER, ReleaseIdentity::for_test(release))
        .expect("release binds");
    postgres
        .bind_invocation(OPERATION_OWNER, invocation(OWNER_OPERATION, "base"))
        .expect("invocation binds");
    let sql = "UPDATE operation_transaction_roundtrip SET value = value + 1 RETURNING value";
    let digest = format!(
        "sha256:{}",
        hex::encode(sha2::Sha256::digest(sql.as_bytes()))
    );
    postgres
        .bind_statement_operation(
            OPERATION_OWNER,
            OWNER_OPERATION,
            [(
                digest.clone(),
                VerifiedStatement {
                    exact_sql: sql.into(),
                    binds: Box::new([]),
                    columns: Box::new([StatementField {
                        value_type: StatementValueType::Int32,
                        nullable: false,
                    }]),
                    transactional: true,
                },
            )]
            .into(),
        )
        .expect("owner statement binds");
    postgres
        .activate_statement_operation(OPERATION_OWNER, OWNER_OPERATION)
        .expect("owner statement activates");
    let (admin, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    admin
        .batch_execute(
            "CREATE TABLE operation_transaction_roundtrip(value int NOT NULL); \
         INSERT INTO operation_transaction_roundtrip VALUES (1); \
         GRANT SELECT, UPDATE ON operation_transaction_roundtrip TO wamn_app",
        )
        .await
        .unwrap();
    let value = async || -> i32 {
        admin
            .query_one("SELECT value FROM operation_transaction_roundtrip", &[])
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
                    bytes: wat::parse_str(operation_owner_component(&digest))
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
    let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> =
        HashMap::from([(WAMN_POSTGRES_ID, postgres.clone() as Arc<dyn HostPlugin>)]);
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
    assert_eq!(value().await, 2, "without a host transaction run commits");

    for commit in [true, false] {
        let transaction = postgres
            .begin_operation_transaction(OPERATION_OWNER)
            .await
            .expect("the host begins the operation transaction");
        postgres
            .bind_operation_transaction(OPERATION_OWNER, &transaction)
            .expect("the transaction binds");
        let owner = run_owner().await;
        assert!(
            matches!(owner, Val::Bool(true)),
            "the owner works in the host transaction: {owner:?}"
        );
        assert!(
            postgres
                .refuse_in_host_transaction(OPERATION_OWNER, "run-stream")
                .is_err()
        );
        postgres.revoke_operation_transaction(OPERATION_OWNER);
        assert_eq!(
            value().await,
            if commit { 2 } else { 3 },
            "nothing commits before the host"
        );
        if commit {
            transaction.commit().await.expect("the host commits");
            assert_eq!(value().await, 3, "the owner's update commits");
        } else {
            transaction.rollback().await.expect("the host rolls back");
            assert_eq!(value().await, 3, "the owner's update rolls back");
        }
    }
}
