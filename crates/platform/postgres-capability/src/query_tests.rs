//! Application dynamic queries through the guest interface and PostgreSQL grants.

use std::collections::HashMap;
use std::sync::Arc;

use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, Instance, Linker, ResourceAny};

use crate::{
    ClassCredentials, PgError, SqlValue, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig,
};

#[sqlx::test(migrations = false)]
async fn application_query_uses_the_read_login_and_database_grants_refuse_a_write(
    pool: sqlx::PgPool,
) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let app_url =
        crate::session_identity::tests::live_guest_url(database.as_str(), "test--dev--dynamic")
            .await;
    sqlx::raw_sql("CREATE SEQUENCE public.dynamic_query_write; GRANT USAGE ON SEQUENCE public.dynamic_query_write TO wamn_app")
        .execute(&pool).await.expect("sequence writable by the application login");
    sqlx::raw_sql("CREATE TABLE public.dynamic_query_probe(value integer); GRANT INSERT ON public.dynamic_query_probe TO wamn_query")
        .execute(&pool).await.unwrap();
    let query_login =
        url::Url::parse(&app_url)
            .unwrap()
            .username()
            .replacen("wamn_app_", "wamn_query_", 1);
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(app_url)),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2000,
            statement_timeout_ms: 5000,
        })
        .expect("Postgres capability"),
    );
    postgres
        .set_project_env_key("application", "test--dev--dynamic")
        .unwrap();
    postgres.set_application("application", "dynamic").unwrap();
    let user = "00000000-0000-4000-8000-000000000031";
    postgres.set_user_id("application", user).unwrap();
    let (mut store, instance) = guest(Arc::clone(&postgres), "application", QUERY_GUEST).await;
    let query = instance
        .get_typed_func::<(String, Vec<SqlValue>), (QueryResponse,)>(&mut store, "query")
        .expect("guest query export");
    let (_, rows) = store.run_concurrent(async |accessor| {
        let (response,) = query.call_concurrent(accessor, (
            "SELECT $1::integer, current_user = $2 AND pg_has_role(current_user, 'wamn_query', 'MEMBER') AND NOT pg_has_role(current_user, 'wamn_app', 'MEMBER'), (wamn_authority.current_principal_id())::text".into(),
            vec![SqlValue::Int32(17), SqlValue::Text(query_login)],
        )).await?;
        collect(accessor, response).await
    }).await.unwrap().unwrap().expect("application reads through the query login");
    assert!(
        matches!(rows.as_slice(), [row] if matches!(row.as_slice(), [SqlValue::Int32(17), SqlValue::Boolean(true), SqlValue::Text(caller)] if caller == user))
    );
    let write = store
        .run_concurrent(async |accessor| {
            let (response,) = query
                .call_concurrent(
                    accessor,
                    (
                        "SELECT nextval('public.dynamic_query_write')".into(),
                        Vec::new(),
                    ),
                )
                .await?;
            collect(accessor, response).await
        })
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(write, Err(PgError::PermissionDenied)),
        "database grants refuse the sequence write: {write:?}"
    );
    let write = store
        .run_concurrent(async |accessor| {
            let (response,) = query
                .call_concurrent(
                    accessor,
                    (
                        "INSERT INTO public.dynamic_query_probe VALUES(1)".into(),
                        Vec::new(),
                    ),
                )
                .await?;
            collect(accessor, response).await
        })
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(write, Err(PgError::QueryError((code, _))) if code == "25006"));
    let destroyed = postgres
        .destroyed
        .load(std::sync::atomic::Ordering::Relaxed);
    let commit = store
        .run_concurrent(async |accessor| {
            let (response,) = query
                .call_concurrent(accessor, ("COMMIT".into(), Vec::new()))
                .await?;
            collect(accessor, response).await
        })
        .await;
    assert!(
        commit.is_err(),
        "ending the dynamic read transaction fails its invocation"
    );
    assert!(
        postgres
            .destroyed
            .load(std::sync::atomic::Ordering::Relaxed)
            > destroyed
    );
}

async fn guest(
    postgres: Arc<WamnPostgres>,
    scope: &str,
    wat: &str,
) -> (Store<SharedCtx>, Instance) {
    let engine = wamn_engine::build_engine(&[]).expect("application engine");
    let component =
        Component::new(engine.inner(), wat::parse_str(wat).unwrap()).expect("query guest");
    let mut linker = Linker::<SharedCtx>::new(engine.inner());
    crate::add_to_linker(&mut linker).expect("Postgres imports");
    let plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::from([(
        WAMN_POSTGRES_ID,
        postgres as Arc<dyn HostPlugin + Send + Sync>,
    )]);
    let ctx = Ctx::builder(scope, scope).with_plugins(plugins).build();
    let mut store = Store::new(
        engine.inner(),
        SharedCtx::new(ctx).with_guest_memory(engine.guest_memory()),
    );
    wash_runtime::engine::guest_memory::install_memory_limiter(&mut store);
    store.set_epoch_deadline(u64::MAX / 2);
    let instance = linker
        .instantiate_async(&mut store, &component)
        .await
        .expect("application guest links");
    (store, instance)
}

#[sqlx::test(migrations = false)]
async fn guest_commit_fails_host_finish_and_discards_the_connection(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    const SCOPE: &str = "transaction-owner";
    const PROJECT: &str = "test--dev--transaction-owner";
    const OPERATION: &str = "fixture:transaction/write";
    let app_url = crate::session_identity::tests::live_guest_url(database.as_str(), PROJECT).await;
    sqlx::raw_sql("CREATE TABLE public.transaction_owner(value integer); \
        ALTER TABLE public.transaction_owner ENABLE ROW LEVEL SECURITY; \
        CREATE POLICY caller ON public.transaction_owner USING ((SELECT (wamn_authority.current_identity()).xid) IS NOT NULL); \
        GRANT SELECT, INSERT ON public.transaction_owner TO wamn_app; \
        CREATE TABLE platform.write_log(operation text, idempotency_key text, request bytea, result text, created_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY(operation,idempotency_key)); \
        GRANT SELECT, INSERT, UPDATE ON platform.write_log TO wamn_app")
        .execute(&pool).await.unwrap();
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(app_url)),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2000,
            statement_timeout_ms: 5000,
        })
        .unwrap(),
    );
    postgres.set_project_env_key(SCOPE, PROJECT).unwrap();
    postgres
        .set_user_id(SCOPE, crate::session_identity::tests::LIVE_PRINCIPAL)
        .unwrap();
    let (mut store, instance) = guest(Arc::clone(&postgres), SCOPE, TRANSACTION_GUEST).await;
    let open = instance
        .get_typed_func::<(), (Result<ResourceAny, PgError>,)>(&mut store, "open")
        .unwrap();
    let batch = instance
        .get_typed_func::<(ResourceAny, String), (Result<(), PgError>,)>(&mut store, "batch")
        .unwrap();
    for known in [false, true] {
        let key = if known { "known" } else { "ordinary" };
        let transaction = postgres.begin_operation_transaction(SCOPE).await.unwrap();
        let transaction = if known {
            transaction.with_known_result("null".into())
        } else {
            transaction
        };
        let backend = transaction
            .with_host_connection(|mut connection| async move {
                let result = sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
                    .fetch_one(connection.connection())
                    .await;
                (connection, result)
            })
            .await
            .unwrap();
        assert!(matches!(
            transaction
                .begin_opaque(OPERATION, key, vec![1])
                .await
                .unwrap(),
            crate::write_log::Claim::New
        ));
        postgres
            .bind_operation_transaction(SCOPE, &transaction)
            .unwrap();
        let (handle,) = store
            .run_concurrent(async |accessor| open.call_concurrent(accessor, ()).await)
            .await
            .unwrap()
            .unwrap();
        let handle = handle.unwrap();
        for sql in ["INSERT INTO public.transaction_owner VALUES(1)", "COMMIT"] {
            let (result,) = store
                .run_concurrent(async |accessor| {
                    batch.call_concurrent(accessor, (handle, sql.into())).await
                })
                .await
                .unwrap()
                .unwrap();
            result.unwrap();
        }
        let (result,) = store
            .run_concurrent(async |accessor| {
                batch
                    .call_concurrent(
                        accessor,
                        (
                            handle,
                            "INSERT INTO public.transaction_owner VALUES(2)".into(),
                        ),
                    )
                    .await
            })
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, Err(PgError::PermissionDenied)));
        transaction.finish_opaque(OPERATION, key, "null".into());
        assert!(matches!(
            transaction.commit().await,
            Err(crate::PgCommitFailure::Uncertain(_))
        ));
        postgres.revoke_operation_transaction(SCOPE);
        handle.resource_drop_async(&mut store).await.unwrap();
        assert_ne!(
            backend,
            postgres.probe_checkout(PROJECT).await.unwrap().backend_pid
        );
        let result: Option<String> = sqlx::query_scalar(
            "SELECT result FROM platform.write_log WHERE operation=$1 AND idempotency_key=$2",
        )
        .bind(OPERATION)
        .bind(key)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(result.is_none());
        sqlx::query("UPDATE platform.write_log SET created_at=now()-interval '1 minute' WHERE operation=$1 AND idempotency_key=$2")
            .bind(OPERATION).bind(key).execute(&pool).await.unwrap();
        let retry = postgres.begin_operation_transaction(SCOPE).await.unwrap();
        assert!(matches!(
            retry.begin_opaque(OPERATION, key, vec![1]).await.unwrap(),
            crate::write_log::Claim::OutcomeUnknown
        ));
        retry.rollback().await.unwrap();
    }
    let values: Vec<i32> = sqlx::query_scalar("SELECT value FROM public.transaction_owner")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(values.iter().all(|value| *value == 1));
    assert!(!values.is_empty());
}

const TRANSACTION_GUEST: &str = r#"(component
    (import "wamn:postgres/types@0.4.0" (instance $types
      (type $error' (variant (case "serialization-failure") (case "connection-unavailable")
        (case "statement-timeout") (case "unique-violation" string) (case "foreign-key-violation" string)
        (case "check-violation" string) (case "exclusion-violation" string) (case "permission-denied")
        (case "query-error" (tuple string string))))
      (export "pg-error" (type $error (eq $error')))))
    (alias export $types "pg-error" (type $error))
    (import "wamn:postgres/statements@0.4.0" (instance $sql
      (export "transaction" (type $tx (sub resource)))
      (export "operation-transaction" (func async (result (result (own $tx) (error $error)))))
      (export "[method]transaction.query-batch" (func async (param "self" (borrow $tx))
        (param "sql" string) (result (result (error $error)))))))
    (alias export $sql "transaction" (type $tx))
    (export "transaction" (type $tx))
    (core module $memory
      (memory (export "memory") 1)
      (global $next (mut i32) (i32.const 1024))
      (func (export "realloc") (param i32 i32) (param $align i32) (param $size i32) (result i32)
        (local $ptr i32) global.get $next local.get $align i32.const 1 i32.sub i32.add
        i32.const 0 local.get $align i32.sub i32.and local.tee $ptr
        local.get $size i32.add global.set $next local.get $ptr))
    (core instance $memory (instantiate $memory))
    (core func $open (canon lower (func $sql "operation-transaction")
      (memory $memory "memory") (realloc (func $memory "realloc"))))
    (core func $batch (canon lower (func $sql "[method]transaction.query-batch")
      (memory $memory "memory") (realloc (func $memory "realloc"))))
    (core func $drop (canon resource.drop $tx))
    (core func $return-open (canon task.return (result (result (own $tx) (error $error))) (memory $memory "memory")))
    (core func $return-batch (canon task.return (result (result (error $error))) (memory $memory "memory")))
    (core module $main
      (import "memory" "memory" (memory 1))
      (import "host" "open" (func $open (param i32)))
      (import "host" "batch" (func $batch (param i32 i32 i32 i32)))
      (import "host" "drop" (func $drop (param i32)))
      (import "host" "return-open" (func $return-open (param i32 i32 i32 i32 i32 i32)))
      (import "host" "return-batch" (func $return-batch (param i32 i32 i32 i32 i32 i32)))
      (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
      (func (export "open") (result i32)
        i32.const 512 call $open
        i32.const 512 i32.load i32.const 516 i32.load i32.const 520 i32.load
        i32.const 524 i32.load i32.const 528 i32.load i32.const 532 i32.load call $return-open
        i32.const 0)
      (func (export "batch") (param $self i32) (param $sql i32) (param $len i32) (result i32)
        local.get $self local.get $sql local.get $len i32.const 512 call $batch
        local.get $self call $drop
        i32.const 512 i32.load i32.const 516 i32.load i32.const 520 i32.load
        i32.const 524 i32.load i32.const 528 i32.load i32.const 532 i32.load call $return-batch
        i32.const 0))
    (core instance $main (instantiate $main (with "memory" (instance $memory))
      (with "host" (instance (export "open" (func $open)) (export "batch" (func $batch)) (export "drop" (func $drop))
        (export "return-open" (func $return-open)) (export "return-batch" (func $return-batch))))))
    (func (export "open") async (result (result (own $tx) (error $error)))
      (canon lift (core func $main "open") (memory $memory "memory")
        (realloc (func $memory "realloc")) async (callback (func $main "callback"))))
    (func (export "batch") async (param "self" (borrow $tx)) (param "sql" string)
      (result (result (error $error)))
      (canon lift (core func $main "batch") (memory $memory "memory")
        (realloc (func $memory "realloc")) async (callback (func $main "callback"))))
)"#;

type QueryResponse = Result<
    (
        Vec<String>,
        wash_runtime::wasmtime::component::StreamReader<Vec<SqlValue>>,
        wash_runtime::wasmtime::component::FutureReader<Result<(), PgError>>,
    ),
    PgError,
>;

async fn collect(
    accessor: &wash_runtime::wasmtime::component::Accessor<SharedCtx>,
    response: QueryResponse,
) -> wash_runtime::wasmtime::Result<Result<(Vec<String>, Vec<Vec<SqlValue>>), PgError>> {
    let (columns, rows, end) = match response {
        Ok(response) => response,
        Err(error) => return Ok(Err(error)),
    };
    let (done, received) = tokio::sync::oneshot::channel();
    let (closed, completion) = tokio::sync::oneshot::channel();
    accessor.with(|mut access| {
        rows.pipe(
            &mut access,
            Rows {
                values: Vec::new(),
                done: Some(done),
            },
        )?;
        end.pipe(&mut access, Completion(Some(closed)))
    })?;
    let rows = received.await?;
    Ok(completion.await?.map(|()| (columns, rows)))
}

struct Rows {
    values: Vec<Vec<SqlValue>>,
    done: Option<tokio::sync::oneshot::Sender<Vec<Vec<SqlValue>>>>,
}
impl Drop for Rows {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(std::mem::take(&mut self.values));
        }
    }
}
impl<D> wash_runtime::wasmtime::component::StreamConsumer<D> for Rows {
    type Item = Vec<SqlValue>;
    fn poll_consume(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        mut store: wash_runtime::wasmtime::StoreContextMut<D>,
        mut source: wash_runtime::wasmtime::component::Source<Self::Item>,
        finish: bool,
    ) -> std::task::Poll<
        wash_runtime::wasmtime::Result<wash_runtime::wasmtime::component::StreamResult>,
    > {
        let count = source.remaining(&mut store);
        let this = self.get_mut();
        this.values.reserve(count);
        source.read(&mut store, &mut this.values)?;
        std::task::Poll::Ready(Ok(if count == 0 && finish {
            wash_runtime::wasmtime::component::StreamResult::Cancelled
        } else {
            wash_runtime::wasmtime::component::StreamResult::Completed
        }))
    }
}
struct Completion(Option<tokio::sync::oneshot::Sender<Result<(), PgError>>>);
impl<D> wash_runtime::wasmtime::component::FutureConsumer<D> for Completion {
    type Item = Result<(), PgError>;
    fn poll_consume(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        store: wash_runtime::wasmtime::StoreContextMut<D>,
        mut source: wash_runtime::wasmtime::component::Source<Self::Item>,
        _: bool,
    ) -> std::task::Poll<wash_runtime::wasmtime::Result<()>> {
        let mut value = None;
        source.read(store, &mut value)?;
        if let Some(done) = self.get_mut().0.take() {
            let _ = done.send(value.unwrap_or(Err(PgError::ConnectionUnavailable)));
        }
        std::task::Poll::Ready(Ok(()))
    }
}

const QUERY_GUEST: &str = r#"(component
  (import "wamn:postgres/types@0.4.0" (instance $types
    (type $sql-value' (variant
      (case "null")
      (case "boolean" bool)
      (case "int32" s32)
      (case "int64" s64)
      (case "float64" f64)
      (case "text" string)
      (case "bytes" (list u8))
      (case "numeric" string)
      (case "timestamptz" string)
      (case "json" string)
      (case "uuid" string)))
    (export "sql-value" (type $sql-value (eq $sql-value')))
    (type $pg-error' (variant
      (case "serialization-failure")
      (case "connection-unavailable")
      (case "statement-timeout")
      (case "unique-violation" string)
      (case "foreign-key-violation" string)
      (case "check-violation" string)
      (case "exclusion-violation" string)
      (case "permission-denied")
      (case "query-error" (tuple string string))))
    (export "pg-error" (type $pg-error (eq $pg-error')))))
  (alias export $types "sql-value" (type $sql-value))
  (alias export $types "pg-error" (type $pg-error))
  (import "wamn:postgres/client@0.4.0" (instance $client
    (export "sql-value" (type (eq $sql-value)))
    (export "pg-error" (type (eq $pg-error)))
    (export "query" (func async
      (param "sql" string)
      (param "params" (list $sql-value))
      (result (result (tuple (list string) (stream (list $sql-value)) (future (result (error $pg-error)))) (error $pg-error)))))))
  (core module $memory
    (memory (export "memory") 1)
    (global $next (mut i32) (i32.const 1024))
    (func (export "realloc") (param i32 i32) (param $align i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $next local.get $align i32.const 1 i32.sub i32.add
      i32.const 0 local.get $align i32.sub i32.and local.tee $ptr
      local.get $size i32.add global.set $next local.get $ptr))
  (core instance $memory (instantiate $memory))
  (core func $query (canon lower (func $client "query")
    (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core func $return (canon task.return (result (result (tuple (list string) (stream (list $sql-value)) (future (result (error $pg-error)))) (error $pg-error)))
    (memory $memory "memory")))
  (core module $main
    (import "memory" "memory" (memory 1))
    (import "host" "query" (func $query (param i32 i32 i32 i32 i32)))
    (import "host" "return" (func $return (param i32 i32 i32 i32 i32 i32)))
    (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
    (func (export "query") (param $sql i32) (param $len i32) (param $params i32) (param $count i32) (result i32)
      local.get $sql local.get $len local.get $params local.get $count
      i32.const 512 call $query
      i32.const 512 i32.load8_u i32.const 516 i32.load
      i32.const 520 i32.load i32.const 524 i32.load
      i32.const 528 i32.load i32.const 532 i32.load call $return
      i32.const 0))
  (core instance $main (instantiate $main (with "memory" (instance $memory))
    (with "host" (instance (export "query" (func $query)) (export "return" (func $return))))))
  (func (export "query") async (param "sql" string) (param "params" (list $sql-value))
    (result (result (tuple (list string) (stream (list $sql-value)) (future (result (error $pg-error)))) (error $pg-error)))
    (canon lift (core func $main "query") (memory $memory "memory")
      (realloc (func $memory "realloc")) async (callback (func $main "callback"))))
)"#;
