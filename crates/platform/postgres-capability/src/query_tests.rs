//! Application dynamic queries through the guest interface and PostgreSQL grants.

use std::collections::HashMap;
use std::sync::Arc;

use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, Linker};

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
    let engine = wamn_engine::build_engine(&[]).expect("application engine");
    let component =
        Component::new(engine.inner(), wat::parse_str(QUERY_GUEST).unwrap()).expect("query guest");
    let mut linker = Linker::<SharedCtx>::new(engine.inner());
    crate::add_to_linker(&mut linker).expect("Postgres imports");
    let plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::from([(
        WAMN_POSTGRES_ID,
        postgres as Arc<dyn HostPlugin + Send + Sync>,
    )]);
    let ctx = Ctx::builder("application", "application")
        .with_plugins(plugins)
        .build();
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
    let query = instance
        .get_typed_func::<(String, Vec<SqlValue>), (QueryResponse,)>(&mut store, "query")
        .expect("guest query export");
    let (_, rows) = store.run_concurrent(async |accessor| {
        let (response,) = query.call_concurrent(accessor, (
            "SELECT $1::integer, current_user = $2 AND pg_has_role(current_user, 'wamn_query', 'MEMBER') AND NOT pg_has_role(current_user, 'wamn_app', 'MEMBER')".into(),
            vec![SqlValue::Int32(17), SqlValue::Text(query_login)],
        )).await?;
        collect(accessor, response).await
    }).await.unwrap().unwrap().expect("application reads through the query login");
    assert!(
        matches!(rows.as_slice(), [row] if matches!(row.as_slice(), [SqlValue::Int32(17), SqlValue::Boolean(true)]))
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
}

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
