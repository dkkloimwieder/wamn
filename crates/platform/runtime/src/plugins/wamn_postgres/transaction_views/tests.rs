use std::collections::HashMap;
use std::sync::Arc;

use sha2::Digest as _;
use tokio::sync::oneshot;
use tokio::time::{Duration, Instant};
use wamn_catalog::ManifestDigest;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{GuestCall, GuestCallFuture};
use wash_runtime::host::http::NullServer;
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings};
use wash_runtime::types::{Component as WorkloadComponent, Workload};
use wash_runtime::wasmtime::component::{Accessor, Instance, Val};
use wash_runtime::wit::WitInterface;

use super::{PgError, StatementError, statement_wit};
use crate::engine::build_engine;
use crate::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionInvocation, ConnectionOrigin,
};
use crate::plugins::wamn_postgres::claims::tests::{
    LIVE_PRINCIPAL, ensure_live_users_rows, live_guest_url,
};
use crate::plugins::wamn_postgres::resources::{
    PgStatementTransaction, begin_statement_transaction, finish_statement_txn,
};
use crate::plugins::wamn_postgres::{
    ClassCredentials, SessionClaims, StatementField, StatementValueType, VerifiedStatement,
    WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig,
};

const CANCEL_OWNER: &str = "transaction-view-cancel-owner";
const CANCEL_PARTICIPANT: &str = "transaction-view-cancel-participant";
const CONTROL_OWNER: &str = "transaction-view-control-owner";
const OWNER: &str = "transaction-view-owner";
const PARTICIPANT: &str = "transaction-view-participant";
const WRONG_PARTICIPANT: &str = "transaction-view-wrong-participant";
const OWNER_OPERATION: &str = "base:receipt/record@1.0.0";
const PARTICIPANT_OPERATION: &str = "acme:receipt/participate@1.0.0";
const TENANT: &str = "transactionview";

fn invocation(operation: &str, package_id: &str) -> ConnectionInvocation {
    ConnectionInvocation {
        origin: ConnectionOrigin {
            wiring_package_id: "overlay".into(),
            package_id: "overlay".into(),
            component_digest: format!("sha256:{}", "a".repeat(64)),
            component: "overlay".into(),
            interface_version: "0.1.0".into(),
            operation: OWNER_OPERATION.into(),
        },
        package_id: package_id.into(),
        wiring_id: "receipt".into(),
        wiring_version: 1,
        node_id: "record".into(),
        occurrence: 0,
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
    (type $transaction-view' (record (field "opaque" string)))
    (export "transaction-view" (type $transaction-view (eq $transaction-view')))
    (export "run-view" (func async (param "view" $transaction-view)
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
  (core func $run-view (canon lower (func $statements "run-view")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $return (canon task.return (result bool)))
  (core module $main
    (import "memory" "memory" (memory 1))
    (import "host" "run-view" (func $run-view (param i32 i32 i32 i32 i32 i32 i32)))
    (import "host" "return" (func $return (param i32)))
    (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
    (func (export "run-view") (param i32 i32 i32 i32 i32 i32) (result i32)
      local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 local.get 5
      i32.const 128 call $run-view
      i32.const 128 i32.load8_u i32.eqz call $return
      i32.const 0))
  (core instance $main (instantiate $main
    (with "memory" (instance $memory))
    (with "host" (instance (export "run-view" (func $run-view)) (export "return" (func $return))))))
  (func (export "run-view") async
    (param "view" $transaction-view) (param "statement-digest" string) (param "binds" (list $sql-value))
    (result bool)
    (canon lift (core func $main "run-view") (memory $memory "memory")
      (realloc (func $memory "realloc")) async (callback (func $main "callback")))))
"#
}

struct RunView {
    token: String,
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
                let previous = std::mem::replace(&mut active.component_id, Arc::from(PARTICIPANT));
                Ok::<_, wash_runtime::wasmtime::Error>((run, previous))
            })?;
            let params = [
                Val::Record(vec![("opaque".into(), Val::String(self.token))]),
                Val::String(self.digest),
                Val::List(Vec::new()),
            ];
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
        (PARTICIPANT, PARTICIPANT_OPERATION, "acme"),
        (WRONG_PARTICIPANT, PARTICIPANT_OPERATION, "acme"),
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
    postgres
        .bind_statement_operation(
            PARTICIPANT,
            PARTICIPANT_OPERATION,
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
        .activate_statement_operation(PARTICIPANT, PARTICIPANT_OPERATION)
        .expect("participant statement activates");
    let deadline = Instant::now() + Duration::from_secs(10);
    postgres
        .bind_transaction_scope(OWNER, deadline, None)
        .unwrap();
    let transaction = begin_statement_transaction(&postgres, OWNER, "default")
        .await
        .expect("owner transaction begins");
    let transaction = PgStatementTransaction {
        transaction,
        statements: None,
        owner_scope: OWNER.into(),
    };
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
    let token = postgres
        .issue_transaction_view(OWNER, &transaction, PARTICIPANT_OPERATION.into())
        .expect("owner issues view");
    let opaque = token.opaque.clone();
    let participation = postgres
        .prepare_transaction_participation(OWNER, PARTICIPANT_OPERATION)
        .unwrap();
    postgres
        .bind_transaction_scope(PARTICIPANT, deadline, participation.as_ref())
        .expect("participant binds view");
    postgres
        .bind_transaction_scope(WRONG_PARTICIPANT, deadline, None)
        .expect("different invocation binds without the view");

    assert!(
        postgres
            .permit_transaction_nested_call(PARTICIPANT)
            .is_err()
    );
    let independent = postgres
        .one_shot_statement(
            PARTICIPANT,
            &digest,
            &VerifiedStatement {
                exact_sql: sql.into(),
                binds: Box::new([]),
                columns: Box::new([StatementField {
                    value_type: StatementValueType::Int32,
                    nullable: false,
                }]),
                transactional: true,
            },
            &[],
        )
        .await;
    assert!(matches!(
        independent,
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));
    assert!(matches!(
        postgres.issue_transaction_view(PARTICIPANT, &transaction, OWNER_OPERATION.into()),
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));

    let engine = build_engine(&[]).expect("engine");
    let workload = engine
        .initialize_workload(
            "transaction-view-roundtrip",
            Workload {
                namespace: "test".into(),
                name: "transaction-view-roundtrip".into(),
                annotations: HashMap::new(),
                service: None,
                components: vec![WorkloadComponent {
                    name: "participant".into(),
                    bytes: wat::parse_str(run_view_component()).unwrap().into(),
                    ..WorkloadComponent::default()
                }],
                host_interfaces: vec![
                    WitInterface::from("wamn:postgres/types@0.1.0"),
                    WitInterface::from("wamn:postgres/statements@0.1.0"),
                ],
                volumes: Vec::new(),
            },
        )
        .expect("native component compiles");
    let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> =
        HashMap::from([(WAMN_POSTGRES_ID, postgres.clone() as Arc<dyn HostPlugin>)]);
    let workload = workload
        .resolve(
            Some(&plugins),
            &PluginBindings::new(),
            Arc::new(NullServer::default()),
            &Meters::new(MeterKind::Off),
        )
        .await
        .expect("postgres links through native workload resolution");
    let component_id = workload
        .components()
        .read()
        .await
        .values()
        .find(|component| component.name() == "participant")
        .expect("participant component exists")
        .id()
        .to_owned();
    let target = workload
        .dispatch_target(&component_id, WAMN_POSTGRES_ID)
        .await
        .expect("native participant dispatch target");
    let (reply, receive) = oneshot::channel();
    target
        .dispatch(RunView {
            token: token.opaque,
            digest: digest.clone(),
            reply,
        })
        .await
        .expect("native run-view dispatch");
    let result = receive.await.expect("run-view reply");
    assert!(
        matches!(result, Val::Bool(true)),
        "run-view returned {result:?}"
    );

    let fabricated = statement_wit::TransactionView {
        opaque: "fabricated".into(),
    };
    assert!(matches!(
        postgres
            .run_transaction_view(PARTICIPANT, &fabricated, &digest, &[])
            .await,
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));
    assert!(matches!(
        postgres
            .run_transaction_view(
                WRONG_PARTICIPANT,
                &statement_wit::TransactionView {
                    opaque: opaque.clone(),
                },
                &digest,
                &[]
            )
            .await,
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));
    postgres.revoke_transaction_scope(PARTICIPANT);
    assert!(matches!(
        postgres
            .run_transaction_view(
                PARTICIPANT,
                &statement_wit::TransactionView {
                    opaque: opaque.clone(),
                },
                &digest,
                &[]
            )
            .await,
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));
    assert!(
        postgres
            .bind_transaction_scope(PARTICIPANT, deadline, participation.as_ref())
            .is_err(),
        "a revoked selection cannot activate as an ordinary SQL invocation"
    );
    let expiring = postgres
        .issue_transaction_view(OWNER, &transaction, PARTICIPANT_OPERATION.into())
        .expect("owner issues a second view after participant return");
    postgres
        .bind_transaction_scope(
            PARTICIPANT,
            deadline,
            postgres
                .prepare_transaction_participation(OWNER, PARTICIPANT_OPERATION)
                .unwrap()
                .as_ref(),
        )
        .expect("participant binds the expiring view");
    tokio::time::sleep_until(deadline).await;
    assert!(matches!(
        postgres
            .run_transaction_view(PARTICIPANT, &expiring, &digest, &[])
            .await,
        Err(StatementError::Postgres(PgError::PermissionDenied))
    ));
    postgres.revoke_transaction_scope(PARTICIPANT);
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

    for (scope, operation, package) in [
        (CANCEL_OWNER, OWNER_OPERATION, "base"),
        (CANCEL_PARTICIPANT, PARTICIPANT_OPERATION, "acme"),
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
            CANCEL_PARTICIPANT,
            PARTICIPANT_OPERATION,
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
        .activate_statement_operation(CANCEL_PARTICIPANT, PARTICIPANT_OPERATION)
        .unwrap();
    let cancel_deadline = Instant::now() + Duration::from_secs(5);
    postgres
        .bind_transaction_scope(CANCEL_OWNER, cancel_deadline, None)
        .unwrap();
    let cancel_transaction = PgStatementTransaction {
        transaction: begin_statement_transaction(&postgres, CANCEL_OWNER, "default")
            .await
            .unwrap(),
        statements: None,
        owner_scope: CANCEL_OWNER.into(),
    };
    let cancel_token = postgres
        .issue_transaction_view(
            CANCEL_OWNER,
            &cancel_transaction,
            PARTICIPANT_OPERATION.into(),
        )
        .unwrap();
    postgres
        .bind_transaction_scope(
            CANCEL_PARTICIPANT,
            cancel_deadline,
            postgres
                .prepare_transaction_participation(CANCEL_OWNER, PARTICIPANT_OPERATION)
                .unwrap()
                .as_ref(),
        )
        .unwrap();
    admin
        .batch_execute("BEGIN; UPDATE transaction_view_roundtrip SET value = value")
        .await
        .unwrap();
    let running_postgres = Arc::clone(&postgres);
    let running_digest = digest.clone();
    let running = tokio::spawn(async move {
        running_postgres
            .run_transaction_view(CANCEL_PARTICIPANT, &cancel_token, &running_digest, &[])
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
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
