//! Real-transport acceptance test for the private `sqlx-command` driver (wamn-0h0g.22.2a).
//!
//! The separately built WASI command uses SQLx's generic API, while this host
//! supplies the production `wamn:postgres` plugin and a real PostgreSQL 18
//! identity. See `docs/operations/running-tests.md#live-prerequisites-and-troubleshooting` for the arming recipe.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _, Statement as _};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, ensure};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{
    InMemorySpanExporter, InMemorySpanExporterBuilder, SdkTracerProvider, SpanData,
};

use tracing_subscriber::layer::SubscriberExt as _;
use wamn_engine::engine::build_engine;
use wamn_postgres_capability::{
    self, ClassCredentials, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig,
};
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::p3::bindings::Command;

const COMPONENT_ENV: &str = "WAMN_SQLX_TRANSACTION_COMPONENT";
const COMPONENT_ID: &str = "sqlx-command-live";
const SCHEMA: &str = "sqlx_command_acceptance_test";
const PROJECT_ENV_KEY: &str = "sqlx-command-project";
const PASSWORD: &str = "sqlx-command-live";

struct TraceHarness {
    exporter: InMemorySpanExporter,
    provider: SdkTracerProvider,
    _guard: tracing::subscriber::DefaultGuard,
}

impl TraceHarness {
    fn install() -> Self {
        let exporter = InMemorySpanExporterBuilder::new().build();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer().with_tracer(provider.tracer("sqlx-transaction-live")),
        );
        let guard = tracing::subscriber::set_default(subscriber);
        Self {
            exporter,
            provider,
            _guard: guard,
        }
    }

    fn spans(&self) -> anyhow::Result<Vec<SpanData>> {
        self.provider
            .force_flush()
            .context("flush SQLx transaction spans")?;
        self.exporter
            .get_finished_spans()
            .context("read SQLx transaction spans")
    }
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let mut client = ::wamn_postgres::connect(url)
        .await
        .context("connect to the SQLx transaction database")?;

    Ok(client)
}

fn generation_role(database: &str) -> String {
    format!(
        "wamn_app_{}",
        wamn_identity_protocol::app_identity_hash(PROJECT_ENV_KEY, database)
    )
}

fn guest_url(admin_url: &str, role: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(admin_url).context("parse the SQLx transaction URL")?;
    url.set_username(role)
        .map_err(|()| anyhow::anyhow!("set SQLx guest role in URL"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set SQLx guest password in URL"))?;
    Ok(url.to_string())
}

async fn install(admin: &mut Client, role: &str) -> anyhow::Result<()> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE \"{role}\" LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOCREATEDB \
               NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
             GRANT wamn_app TO \"{role}\"; \
             CREATE SCHEMA {SCHEMA}; \
             CREATE TABLE {SCHEMA}.effects ( \
               id integer PRIMARY KEY, \
               label text NOT NULL UNIQUE, \
               owner_name text NOT NULL \
             ); \
             ALTER TABLE {SCHEMA}.effects ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {SCHEMA}.effects FORCE ROW LEVEL SECURITY; \
             CREATE POLICY current_login_only ON {SCHEMA}.effects TO wamn_app \
               USING (owner_name = current_user::text) \
               WITH CHECK (owner_name = current_user::text); \
             GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app; \
             GRANT SELECT, INSERT ON {SCHEMA}.effects TO wamn_app;"
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .context("install the SQLx transaction fixture")
}

async fn fixture_is_clean(admin: &mut Client, role: &str) -> anyhow::Result<bool> {
    Ok(sqlx::query(
        "SELECT NOT EXISTS (SELECT FROM pg_roles \
                                 WHERE rolname::text IN ('wamn_app', $1::text)) \
                    AND to_regnamespace($2::text) IS NULL",
    )
    .bind(&role)
    .bind(&SCHEMA)
    .fetch_one(&mut *admin)
    .await
    .context("make sure the SQLx transaction fixture is fresh")?
    .get(0))
}

async fn cleanup(admin: &mut Client, role: &str) -> anyhow::Result<()> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DO $cleanup$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 DROP OWNED BY wamn_app; \
                 DROP ROLE wamn_app; \
               END IF; \
             END $cleanup$;"
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .context("remove the SQLx transaction fixture")
}

async fn run_component(component_path: &Path, guest_url: String) -> anyhow::Result<Vec<SpanData>> {
    let guest = std::fs::read(component_path)
        .with_context(|| format!("read {}", component_path.display()))?;
    let postgres = Arc::new(WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(guest_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 100,
    })?);
    postgres.set_project_env_key(COMPONENT_ID, PROJECT_ENV_KEY)?;
    postgres.set_schema(COMPONENT_ID, SCHEMA)?;

    let engine = build_engine(&[])?;
    let raw = engine.inner();
    let component = WasmtimeComponent::new(raw, &guest)
        .map_err(|error| anyhow::anyhow!("compile sqlx-command component: {error}"))?;
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;
    wamn_postgres_capability::add_to_linker(&mut linker)?;

    let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
    plugins.insert(WAMN_POSTGRES_ID, postgres);
    let mut wasi = WasiCtxBuilder::new();
    wasi.args(&["sqlx-command.wasm"])
        .inherit_stdout()
        .inherit_stderr();
    let ctx = Ctx::builder(COMPONENT_ID.to_string(), COMPONENT_ID.to_string())
        .with_plugins(plugins)
        .with_wasi_ctx(wasi.build())
        .build();
    let mut store = Store::new(
        raw,
        SharedCtx::new(ctx).with_guest_memory(engine.guest_memory()),
    );
    wash_runtime::engine::guest_memory::install_memory_limiter(&mut store);
    store.set_epoch_deadline(u64::MAX / 2);

    let traces = TraceHarness::install();
    let command = Command::instantiate_async(&mut store, &component, &linker)
        .await
        .map_err(|error| anyhow::anyhow!("instantiate sqlx-command: {error}"))?;
    let outcome = store
        .run_concurrent(async move |accessor| command.wasi_cli_run().call_run(accessor).await)
        .await
        .map_err(|error| anyhow::anyhow!("run sqlx-command: {error}"))??;
    ensure!(outcome.is_ok(), "sqlx-command returned an error status");
    drop(store);
    traces.spans()
}

fn effect_operations(spans: &[SpanData]) -> HashSet<String> {
    spans
        .iter()
        .filter(|span| span.name == "wamn.postgres")
        .flat_map(|span| &span.attributes)
        .filter(|attribute| attribute.key.as_str() == "db.operation")
        .map(|attribute| attribute.value.to_string())
        .collect()
}

async fn acceptance(admin: &mut Client, admin_url: &str, component: &Path) -> anyhow::Result<()> {
    let version: i32 = sqlx::query("SELECT current_setting('server_version_num')::integer")
        .fetch_one(&mut *admin)
        .await
        .context("read PostgreSQL version")?
        .get(0);
    ensure!(
        version / 10_000 == 18,
        "condition requires PostgreSQL 18, got {version}"
    );
    let database: String = sqlx::query("SELECT current_database()")
        .fetch_one(&mut *admin)
        .await
        .context("read current database")?
        .get(0);
    let role = generation_role(&database);
    install(admin, &role).await?;

    let spans = run_component(component, guest_url(admin_url, &role)?).await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT id, label, owner_name FROM {SCHEMA}.effects ORDER BY id"
    )))
    .fetch_all(&mut *admin)
    .await
    .context("inspect SQLx transaction effects")?;
    ensure!(
        rows.len() == 1,
        "only the committed row may exist, got {}",
        rows.len()
    );
    ensure!(
        rows[0].get::<i32, _>(0) == 1
            && rows[0].get::<String, _>(1) == "committed"
            && rows[0].get::<String, _>(2) == role,
        "the surviving row must be the committed row owned by the guest login"
    );
    let operations = effect_operations(&spans);
    ensure!(
        operations.contains("txn.query") && operations.contains("txn.execute"),
        "wamn.postgres spans must include txn.query and txn.execute; got {operations:?}"
    );
    Ok(())
}

#[sqlx::test(migrations = false)]
#[ignore = "requires: env:WAMN_SQLX_TRANSACTION_COMPONENT"]
async fn sqlx_command_commits_rolls_back_and_obeys_current_user_rls(pool: sqlx::PgPool) {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_SQLX_TRANSACTION_COMPONENT",
    );
    let component = std::env::var(COMPONENT_ENV).map_or_else(
        |_| panic!("set {COMPONENT_ENV} to the built sqlx-command component"),
        PathBuf::from,
    );
    let _lock = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let admin_url = test_database.as_str();
    let mut admin = connect(admin_url)
        .await
        .expect("connect SQLx transaction admin");
    let database: String = sqlx::query("SELECT current_database()")
        .fetch_one(&mut admin)
        .await
        .expect("read SQLx transaction database")
        .get(0);
    let role = generation_role(&database);
    assert!(
        fixture_is_clean(&mut admin, &role)
            .await
            .expect("inspect SQLx transaction fixture"),
        "SQLx transaction condition requires a fresh cluster: wamn_app, {role}, or {SCHEMA} exists"
    );

    let result = acceptance(&mut admin, admin_url, &component).await;
    let teardown = cleanup(&mut admin, &role).await;
    result.expect("SQLx transaction acceptance");
    teardown.expect("SQLx transaction cleanup");
}
