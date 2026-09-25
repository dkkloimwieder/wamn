use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use sha2::Digest as _;
use tokio::sync::oneshot;
use tokio::time::{Duration, Instant};
use wamn_catalog::ManifestDigest;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{DispatchTarget, GuestCall, GuestCallFuture};
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::host::HostRef;
use wash_runtime::host::http::{HostHandler, NullServer};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings, WitInterfaces};
use wash_runtime::types::{Component as WorkloadComponent, Workload};
use wash_runtime::wasmtime::component::{Accessor, Instance, Val};
use wash_runtime::wit::{WitInterface, WitWorld};

use super::{PgError, StatementError};
use crate::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionInvocation, ConnectionOrigin,
};
use crate::plugins::wamn_postgres::claims::tests::{
    LIVE_PRINCIPAL, ensure_live_users_rows, live_guest_url,
};
use crate::plugins::wamn_postgres::resources::{
    PgStatementTransaction, begin_statement_transaction, finish_statement_txn,
};
use crate::plugins::wamn_postgres::transaction_views::PgTransactionView;
use crate::plugins::wamn_postgres::{
    ClassCredentials, SessionClaims, StatementField, StatementValueType, VerifiedStatement,
    WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig,
};
use wamn_engine::engine::build_engine;

/// Bounds a wait for an observed backend state. A wait ends when PostgreSQL
/// shows that state, never after a fixed delay. This bound only stops a test
/// that would otherwise hang because of a defect, so machine load cannot reach it.
const HANG_GUARD: Duration = Duration::from_secs(120);

const CANCEL_OWNER: &str = "transaction-view-cancel-owner";
const CONTROL_OWNER: &str = "transaction-view-control-owner";
const OWNER: &str = "transaction-view-owner";
const OTHER: &str = "transaction-view-other-invocation";
const OWNER_OPERATION: &str = "base:widget/record@1.0.0";
const PARTICIPANT_OPERATION: &str = "peer:widgets/participate@1.0.0";
const PARTICIPANT_CALL: &str = "test:transaction-view/participant@1.0.0";
const DISPATCH_ID: &str = "transaction-view-test-dispatch";
const TENANT: &str = "transactionview";

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

fn run_view_component() -> &'static str {
    r#"
(component
  (import "wamn:postgres/types@0.1.0" (instance $types
    (type $sql-value' (variant (case "null") (case "boolean" bool) (case "int32" s32)
      (case "int64" s64) (case "float64" f64) (case "text" string) (case "bytes" (list u8))
      (case "numeric" string) (case "timestamptz" string) (case "json" string) (case "uuid" string)))
    (export "sql-value" (type $sql-value (eq $sql-value')))
    (type $column' (record (field "name" string) (field "type-name" string)))
    (export "column" (type $column (eq $column')))
    (type $row-set' (record (field "columns" (list $column)) (field "rows" (list (list $sql-value)))))
    (export "row-set" (type $row-set (eq $row-set')))
    (type $pg-error' (variant (case "serialization-failure") (case "connection-unavailable")
      (case "statement-timeout") (case "row-limit-exceeded" u64) (case "unique-violation" string)
      (case "foreign-key-violation" string) (case "check-violation" string)
      (case "exclusion-violation" string) (case "permission-denied")
      (case "query-error" (tuple string string))))
    (export "pg-error" (type $pg-error (eq $pg-error')))))
  (alias export $types "sql-value" (type $sql-value))
  (alias export $types "row-set" (type $row-set))
  (alias export $types "pg-error" (type $pg-error))
  (import "wamn:postgres/statements@0.1.0" (instance $statements
    (type $contract-part' (enum "binds" "columns"))
    (export "contract-part" (type $contract-part (eq $contract-part')))
    (type $value-shape' (record (field "count" u32) (field "types" (list string))))
    (export "value-shape" (type $value-shape (eq $value-shape')))
    (type $contract-mismatch' (record (field "statement-digest" string) (field "part" $contract-part)
      (field "expected" $value-shape) (field "observed" $value-shape)))
    (export "contract-mismatch" (type $contract-mismatch (eq $contract-mismatch')))
    (type $statement-error' (variant (case "unknown-statement" string)
      (case "statement-contract-mismatch" $contract-mismatch) (case "postgres" $pg-error)))
    (export "statement-error" (type $statement-error (eq $statement-error')))
    (export "transaction-view" (type $transaction-view (sub resource)))
    (export "participant-view" (func async
      (result (result (own $transaction-view) (error $statement-error)))))
    (export "[method]transaction-view.run" (func async (param "self" (borrow $transaction-view))
      (param "statement-digest" string) (param "binds" (list $sql-value))
      (result (result $row-set (error $statement-error)))))))
  (alias export $statements "transaction-view" (type $transaction-view))
  (core module $memory
    (memory (export "memory") 1)
    (global $next (mut i32) (i32.const 1024))
    (func (export "realloc") (param i32 i32) (param $align i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $next
      local.get $align i32.const 1 i32.sub i32.add
      i32.const 0 local.get $align i32.sub i32.and
      local.tee $ptr local.get $size i32.add global.set $next
      local.get $ptr))
  (core instance $memory (instantiate $memory))
  (core func $participant-view (canon lower (func $statements "participant-view")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $run-view (canon lower (func $statements "[method]transaction-view.run")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $return (canon task.return (result bool)))
  (core module $main
    (import "memory" "memory" (memory 1))
    (import "host" "participant-view" (func $participant-view (param i32)))
    (import "host" "run-view" (func $run-view (param i32 i32 i32 i32 i32 i32)))
    (import "host" "return" (func $return (param i32)))
    (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
    (func (export "run-view") (param i32 i32 i32 i32) (result i32)
      i32.const 64 call $participant-view
      i32.const 64 i32.load8_u if i32.const 0 call $return end
      i32.const 72 i32.load
      local.get 0 local.get 1 local.get 2 local.get 3
      i32.const 128 call $run-view
      i32.const 128 i32.load8_u i32.eqz call $return
      i32.const 0))
  (core instance $main (instantiate $main
    (with "memory" (instance $memory))
    (with "host" (instance (export "participant-view" (func $participant-view))
      (export "run-view" (func $run-view)) (export "return" (func $return))))))
  (func (export "run-view") async
    (param "statement-digest" string) (param "binds" (list $sql-value))
    (result bool)
    (canon lift (core func $main "run-view") (memory $memory "memory")
      (realloc (func $memory "realloc")) async (callback (func $main "callback")))))
"#
}

fn owner_component() -> &'static str {
    r#"
(component
  (import "wamn:postgres/types@0.1.0" (instance $types
    (type $sql-value' (variant (case "null") (case "boolean" bool) (case "int32" s32)
      (case "int64" s64) (case "float64" f64) (case "text" string) (case "bytes" (list u8))
      (case "numeric" string) (case "timestamptz" string) (case "json" string) (case "uuid" string)))
    (export "sql-value" (type $sql-value (eq $sql-value')))
    (type $column' (record (field "name" string) (field "type-name" string)))
    (export "column" (type $column (eq $column')))
    (type $row-set' (record (field "columns" (list $column)) (field "rows" (list (list $sql-value)))))
    (export "row-set" (type $row-set (eq $row-set')))
    (type $pg-error' (variant (case "serialization-failure") (case "connection-unavailable")
      (case "statement-timeout") (case "row-limit-exceeded" u64) (case "unique-violation" string)
      (case "foreign-key-violation" string) (case "check-violation" string)
      (case "exclusion-violation" string) (case "permission-denied")
      (case "query-error" (tuple string string))))
    (export "pg-error" (type $pg-error (eq $pg-error')))))
  (alias export $types "sql-value" (type $sql-value))
  (alias export $types "row-set" (type $row-set))
  (alias export $types "pg-error" (type $pg-error))
  (import "wamn:postgres/statements@0.1.0" (instance $statements
    (type $contract-part' (enum "binds" "columns"))
    (export "contract-part" (type $contract-part (eq $contract-part')))
    (type $value-shape' (record (field "count" u32) (field "types" (list string))))
    (export "value-shape" (type $value-shape (eq $value-shape')))
    (type $contract-mismatch' (record (field "statement-digest" string) (field "part" $contract-part)
      (field "expected" $value-shape) (field "observed" $value-shape)))
    (export "contract-mismatch" (type $contract-mismatch (eq $contract-mismatch')))
    (type $statement-error' (variant (case "unknown-statement" string)
      (case "statement-contract-mismatch" $contract-mismatch) (case "postgres" $pg-error)))
    (export "statement-error" (type $statement-error (eq $statement-error')))
    (export "transaction" (type $transaction (sub resource)))
    (export "begin" (func async
      (result (result (own $transaction) (error $statement-error)))))
    (export "[method]transaction.select-participant" (func async
      (param "self" (borrow $transaction)) (param "participant-operation" string)
      (result (result (error $statement-error)))))
    (export "[method]transaction.rollback" (func async
      (param "self" (borrow $transaction)) (result (result (error $statement-error)))))))
  (alias export $statements "transaction" (type $transaction))
  (import "test:transaction-view/participant@1.0.0" (instance $participant
    (export "run" (func async (result bool)))))
  (core module $memory
    (memory (export "memory") 1)
    (data (i32.const 16) "peer:widgets/participate@1.0.0")
    (global $next (mut i32) (i32.const 1024))
    (func (export "realloc") (param i32 i32) (param $align i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $next
      local.get $align i32.const 1 i32.sub i32.add
      i32.const 0 local.get $align i32.sub i32.and
      local.tee $ptr local.get $size i32.add global.set $next
      local.get $ptr))
  (core instance $memory (instantiate $memory))
  (core func $begin (canon lower (func $statements "begin")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $select (canon lower (func $statements "[method]transaction.select-participant")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $rollback (canon lower (func $statements "[method]transaction.rollback")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $participant-run (canon lower (func $participant "run")))
  (core func $return (canon task.return (result bool)))
  (core module $main
    (import "memory" "memory" (memory 1))
    (import "host" "begin" (func $begin (param i32)))
    (import "host" "select" (func $select (param i32 i32 i32 i32)))
    (import "host" "participant-run" (func $participant-run (result i32)))
    (import "host" "rollback" (func $rollback (param i32 i32)))
    (import "host" "return" (func $return (param i32)))
    (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
    (func (export "run") (result i32) (local $transaction i32) (local $nested i32)
      i32.const 64 call $begin
      i32.const 64 i32.load8_u if i32.const 0 call $return i32.const 0 return end
      i32.const 72 i32.load local.set $transaction
      local.get $transaction i32.const 16 i32.const 30 i32.const 128 call $select
      i32.const 128 i32.load8_u if i32.const 0 call $return i32.const 0 return end
      call $participant-run local.set $nested
      local.get $transaction i32.const 192 call $rollback
      i32.const 192 i32.load8_u if i32.const 0 call $return i32.const 0 return end
      local.get $nested call $return
      i32.const 0))
  (core instance $main (instantiate $main
    (with "memory" (instance $memory))
    (with "host" (instance (export "begin" (func $begin)) (export "select" (func $select))
      (export "participant-run" (func $participant-run)) (export "rollback" (func $rollback))
      (export "return" (func $return))))))
  (func (export "run") async (result bool)
    (canon lift (core func $main "run") (memory $memory "memory")
      (realloc (func $memory "realloc")) async (callback (func $main "callback")))))
"#
}

struct RunView {
    digest: String,
    reply: oneshot::Sender<Val>,
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

impl GuestCall for RunView {
    fn describe(&self) -> &'static str {
        "transaction-view-roundtrip"
    }

    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async move {
            let (run, previous) = accessor.with(|mut access| {
                let run = instance
                    .get_export_index(&mut access, None, "run-view")
                    .ok_or_else(|| wash_runtime::wasmtime::format_err!("missing run-view"))?;
                let run = instance.get_func(&mut access, run).ok_or_else(|| {
                    wash_runtime::wasmtime::format_err!("run-view is not a function")
                })?;
                let active = &mut access.get().active_ctx;
                // A composed participant runs in the scope of the base that calls it.
                let previous = std::mem::replace(&mut active.component_id, Arc::from(OWNER));
                Ok::<_, wash_runtime::wasmtime::Error>((run, previous))
            })?;
            let params = [Val::String(self.digest), Val::List(Vec::new())];
            let _active = ActiveComponent { accessor, previous };
            let mut results = [Val::Bool(false)];
            run.call_concurrent(accessor, &params, &mut results).await?;
            self.reply
                .send(results.into_iter().next().expect("one run-view result"))
                .map_err(|_| wash_runtime::wasmtime::format_err!("reply abandoned"))?;
            Ok(None)
        })
    }
}

struct RunOwner {
    reply: oneshot::Sender<Val>,
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
                let previous = std::mem::replace(&mut active.component_id, Arc::from(OWNER));
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

/// This fixture stands in for build composition: it plugs the participant
/// into the owner's import, in the owner's invocation scope. It does not run
/// execution-host's production NativePolicy.
struct TestNativeDispatch {
    digest: String,
    participant: Arc<Mutex<Option<DispatchTarget>>>,
}

#[async_trait::async_trait]
impl HostPlugin for TestNativeDispatch {
    fn id(&self) -> &'static str {
        DISPATCH_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from(PARTICIPANT_CALL)]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let digest = self.digest.clone();
        let participant = Arc::clone(&self.participant);
        item.linker()
            .instance(PARTICIPANT_CALL)?
            .func_new_concurrent("run", move |_accessor, _ty, params, results| {
                let participant = participant.clone();
                let digest = digest.clone();
                Box::pin(async move {
                    let [] = params else {
                        wash_runtime::wasmtime::bail!("participant run takes no input");
                    };
                    let [result] = results else {
                        wash_runtime::wasmtime::bail!("participant run returns one value");
                    };
                    let target = participant
                        .lock()
                        .expect("participant target lock poisoned")
                        .clone()
                        .ok_or_else(|| {
                            wash_runtime::wasmtime::format_err!("participant target unresolved")
                        })?;
                    let (reply, receive) = oneshot::channel();
                    target
                        .dispatch(RunView { digest, reply })
                        .await
                        .map_err(wash_runtime::wasmtime::Error::msg)?;
                    *result = receive.await.map_err(|_| {
                        wash_runtime::wasmtime::format_err!("participant abandoned")
                    })?;
                    Ok(())
                })
            })?;
        Ok(())
    }

    async fn on_workload_resolved(
        &self,
        workload: &ResolvedWorkload,
        _component_id: &str,
    ) -> anyhow::Result<()> {
        let components = workload.components();
        let participant_id = components
            .read()
            .await
            .values()
            .find(|component| component.name() == "participant")
            .ok_or_else(|| anyhow::anyhow!("participant component absent"))?
            .id()
            .to_owned();
        let target = workload.dispatch_target(&participant_id, self.id()).await?;
        *self
            .participant
            .lock()
            .expect("participant target lock poisoned") = Some(target);
        Ok(())
    }
}

#[tokio::test]
async fn typed_native_participant_runs_inside_the_owner_transaction() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let guest_url = live_guest_url(database.url(), TENANT).await;
    ensure_live_users_rows(database.url(), TENANT, &[LIVE_PRINCIPAL]).await;
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(guest_url)),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 10,
        })
        .expect("live postgres plugin"),
    );
    let release = ManifestDigest::parse(format!("sha256:{}", "c".repeat(64))).unwrap();
    for (scope, operation, package) in [
        (OWNER, OWNER_OPERATION, "base"),
        (OTHER, OWNER_OPERATION, "base"),
    ] {
        postgres
            .bind_session_claims(
                scope,
                &SessionClaims {
                    tenant: TENANT.into(),
                    user_id: Some(LIVE_PRINCIPAL.into()),
                    operation: Some(operation.into()),
                    ..SessionClaims::default()
                },
            )
            .await
            .expect("claims bind");
        postgres
            .set_release_identity(scope, 1, release.clone())
            .expect("release binds");
        postgres
            .bind_invocation(scope, invocation(operation, package))
            .expect("invocation binds");
    }

    let sql = "UPDATE transaction_view_roundtrip SET value = 2 RETURNING value";
    let digest = format!(
        "sha256:{}",
        hex::encode(sha2::Sha256::digest(sql.as_bytes()))
    );
    // Publish folds the participant's statements into the entry's set.
    postgres
        .bind_statement_operation(
            OWNER,
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
        .expect("participant statement binds");
    postgres
        .activate_statement_operation(OWNER, OWNER_OPERATION)
        .expect("participant statement activates");
    let deadline = Instant::now() + Duration::from_secs(10);
    postgres.bind_transaction_scope(OWNER, deadline).unwrap();
    postgres
        .bind_selected_participant(OWNER, PARTICIPANT_OPERATION.into(), "intent".into())
        .expect("the published participant binds");
    let admin = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let (admin, connection) = admin;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    admin
        .batch_execute(
            "CREATE TABLE transaction_view_roundtrip(value int NOT NULL); \
         INSERT INTO transaction_view_roundtrip VALUES (1); \
         GRANT SELECT, UPDATE ON transaction_view_roundtrip TO wamn_app",
        )
        .await
        .unwrap();
    let engine = build_engine(&[]).expect("engine");
    let workload = engine
        .initialize_workload(
            "transaction-view-roundtrip",
            Workload {
                namespace: "test".into(),
                name: "transaction-view-roundtrip".into(),
                annotations: HashMap::new(),
                service: None,
                components: vec![
                    WorkloadComponent {
                        name: "owner".into(),
                        bytes: wat::parse_str(owner_component()).unwrap().into(),
                        ..WorkloadComponent::default()
                    },
                    WorkloadComponent {
                        name: "participant".into(),
                        bytes: wat::parse_str(run_view_component()).unwrap().into(),
                        ..WorkloadComponent::default()
                    },
                ],
                host_interfaces: vec![
                    WitInterface::from("wamn:postgres/types@0.1.0"),
                    WitInterface::from("wamn:postgres/statements@0.1.0"),
                    WitInterface::from(PARTICIPANT_CALL),
                ],
                volumes: Vec::new(),
            },
        )
        .expect("native component compiles");
    let dispatch = Arc::new(TestNativeDispatch {
        digest: digest.clone(),
        participant: Arc::default(),
    });
    let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> = HashMap::from([
        (WAMN_POSTGRES_ID, postgres.clone() as Arc<dyn HostPlugin>),
        (DISPATCH_ID, dispatch as Arc<dyn HostPlugin>),
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
        .dispatch_target(&component_id, DISPATCH_ID)
        .await
        .expect("native owner dispatch target");
    let (reply, receive) = oneshot::channel();
    target
        .dispatch(RunOwner { reply })
        .await
        .expect("native owner dispatch");
    let result = receive.await.expect("owner reply");
    assert!(
        matches!(result, Val::Bool(true)),
        "owner transaction returned {result:?}"
    );
    let value: i32 = admin
        .query_one("SELECT value FROM transaction_view_roundtrip", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(value, 1, "participant update belonged to owner rollback");

    let transaction = PgStatementTransaction {
        transaction: begin_statement_transaction(&postgres, OWNER, "default")
            .await
            .expect("owner transaction begins for authority checks"),
        statements: None,
        owner_scope: OWNER.into(),
    };
    assert!(
        matches!(
            postgres.select_transaction_participant(OWNER, &transaction, OWNER_OPERATION.into()),
            Err(StatementError::Postgres(PgError::PermissionDenied))
        ),
        "only the published participant is selected"
    );
    postgres
        .select_transaction_participant(OWNER, &transaction, PARTICIPANT_OPERATION.into())
        .expect("owner selects participant for authority checks");
    postgres
        .bind_transaction_scope(OTHER, deadline)
        .expect("a different invocation binds without the view");
    assert!(matches!(
        postgres.acquire_transaction_view(OTHER),
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));
    assert!(matches!(
        postgres.select_transaction_participant(OTHER, &transaction, PARTICIPANT_OPERATION.into()),
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));

    let view = postgres
        .acquire_transaction_view(OWNER)
        .expect("the participant acquires the view in its own scope");
    assert!(
        matches!(
            postgres.acquire_transaction_view(OWNER),
            Err(StatementError::Postgres(PgError::PermissionDenied))
        ),
        "one view per selection"
    );
    let fabricated = PgTransactionView {
        lease: Arc::clone(&view.lease),
        scope: OTHER.into(),
    };
    assert!(matches!(
        postgres
            .run_transaction_view(OWNER, &fabricated, &digest, &[])
            .await,
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));
    assert!(matches!(
        postgres
            .run_transaction_view(OTHER, &view, &digest, &[])
            .await,
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));
    tokio::time::sleep_until(deadline).await;
    assert!(matches!(
        postgres
            .run_transaction_view(OWNER, &view, &digest, &[])
            .await,
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));
    postgres.revoke_transaction_scope(OWNER);
    finish_statement_txn(
        &transaction.transaction.state,
        &transaction.transaction.destroyed,
        "ROLLBACK",
    )
    .await
    .expect("owner rolls back");
    let value: i32 = admin
        .query_one("SELECT value FROM transaction_view_roundtrip", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(value, 1, "participant update belonged to owner rollback");

    {
        let (scope, operation, package) = (CANCEL_OWNER, OWNER_OPERATION, "base");
        postgres
            .bind_session_claims(
                scope,
                &SessionClaims {
                    tenant: TENANT.into(),
                    user_id: Some(LIVE_PRINCIPAL.into()),
                    operation: Some(operation.into()),
                    ..SessionClaims::default()
                },
            )
            .await
            .unwrap();
        postgres
            .set_release_identity(scope, 1, release.clone())
            .unwrap();
        postgres
            .bind_invocation(scope, invocation(operation, package))
            .unwrap();
    }
    postgres
        .bind_statement_operation(
            CANCEL_OWNER,
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
        .unwrap();
    postgres
        .activate_statement_operation(CANCEL_OWNER, OWNER_OPERATION)
        .unwrap();
    // The finishing rollback, not this deadline, ends the participant. The
    // deadline only stops a defect from hanging the test, so machine load
    // cannot reach it before the participant reaches its lock wait.
    let cancel_deadline = Instant::now() + HANG_GUARD;
    postgres
        .bind_transaction_scope(CANCEL_OWNER, cancel_deadline)
        .unwrap();
    postgres
        .bind_selected_participant(CANCEL_OWNER, PARTICIPANT_OPERATION.into(), "intent".into())
        .unwrap();
    let cancel_transaction = PgStatementTransaction {
        transaction: begin_statement_transaction(&postgres, CANCEL_OWNER, "default")
            .await
            .unwrap(),
        statements: None,
        owner_scope: CANCEL_OWNER.into(),
    };
    postgres
        .select_transaction_participant(
            CANCEL_OWNER,
            &cancel_transaction,
            PARTICIPANT_OPERATION.into(),
        )
        .unwrap();
    let cancel_view = postgres.acquire_transaction_view(CANCEL_OWNER).unwrap();
    admin
        .batch_execute("BEGIN; UPDATE transaction_view_roundtrip SET value = value")
        .await
        .unwrap();
    let running_postgres = Arc::clone(&postgres);
    let running_digest = digest.clone();
    let running = tokio::spawn(async move {
        running_postgres
            .run_transaction_view(CANCEL_OWNER, &cancel_view, &running_digest, &[])
            .await
    });
    tokio::time::timeout(HANG_GUARD, async {
        loop {
            let waiting: bool = admin
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                     WHERE wait_event_type = 'Lock' AND query LIKE 'UPDATE transaction_view_roundtrip%')",
                    &[],
                )
                .await
                .unwrap()
                .get(0);
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("participant reaches the database lock wait");
    let finish_state = Arc::clone(&cancel_transaction.transaction.state);
    let finish_destroyed = Arc::clone(&cancel_transaction.transaction.destroyed);
    let finishing = tokio::spawn(async move {
        finish_statement_txn(&finish_state, &finish_destroyed, "ROLLBACK").await
    });
    assert!(matches!(
        running.await.unwrap(),
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));
    assert!(finishing.await.unwrap().is_err());
    admin.batch_execute("ROLLBACK").await.unwrap();

    postgres
        .bind_session_claims(
            CONTROL_OWNER,
            &SessionClaims {
                tenant: TENANT.into(),
                user_id: Some(LIVE_PRINCIPAL.into()),
                operation: Some(OWNER_OPERATION.into()),
                ..SessionClaims::default()
            },
        )
        .await
        .unwrap();
    let control = begin_statement_transaction(&postgres, CONTROL_OWNER, "default")
        .await
        .unwrap();
    finish_statement_txn(&control.state, &control.destroyed, "COMMIT")
        .await
        .expect("normal owner transaction commits");
}
