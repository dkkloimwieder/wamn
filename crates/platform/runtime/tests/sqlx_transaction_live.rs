//! Real-transport acceptance gate for `wamn-postgres-sqlx` (wamn-0h0g.22.2a).
//!
//! The separately built WASI command uses SQLx's generic API, while this host
//! supplies the production `wamn:postgres` plugin and a real PostgreSQL 18
//! identity. See `docs/operations/build-and-test.md` for the arming recipe.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, ensure};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{
    InMemorySpanExporter, InMemorySpanExporterBuilder, SdkTracerProvider, SpanData,
};
use tokio_postgres::{Client, NoTls};
use tracing_subscriber::layer::SubscriberExt as _;
use wamn_runtime::engine::build_engine;
use wamn_runtime::plugins::wamn_postgres::{
    self, ClassCredentials, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig,
};
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::p2::bindings::CommandPre;

const ADMIN_URL_ENV: &str = "WAMN_SQLX_TRANSACTION_PG_URL";
const COMPONENT_ENV: &str = "WAMN_SQLX_TRANSACTION_COMPONENT";
const COMPONENT_ID: &str = "sqlx-command-live";
const SCHEMA: &str = "sqlx_command_gate";
const TENANT: &str = "sqlx-command-tenant";
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
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to the SQLx transaction database")?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!(%error, "SQLx transaction database connection failed");
        }
    });
    Ok(client)
}

fn generation_role(database: &str) -> String {
    format!(
        "wamn_app_{}_a",
        wamn_run_state::app_scope_hash(TENANT, database)
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

async fn install(admin: &Client, role: &str) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
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
        .await
        .context("install the SQLx transaction fixture")
}

async fn fixture_is_clean(admin: &Client, role: &str) -> anyhow::Result<bool> {
    Ok(admin
        .query_one(
            "SELECT NOT EXISTS (SELECT FROM pg_roles \
                                 WHERE rolname::text IN ('wamn_app', $1::text)) \
                    AND to_regnamespace($2::text) IS NULL",
            &[&role, &SCHEMA],
        )
        .await
        .context("check the SQLx transaction fixture is fresh")?
        .get(0))
}

async fn cleanup(admin: &Client, role: &str) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
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
        .await
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
    postgres.set_tenant(COMPONENT_ID, TENANT)?;
    postgres.set_schema(COMPONENT_ID, SCHEMA)?;

    let engine = build_engine(&[])?;
    let raw = engine.inner();
    let component = WasmtimeComponent::new(raw, &guest)
        .map_err(|error| anyhow::anyhow!("compile sqlx-command component: {error}"))?;
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wamn_postgres::add_to_linker(&mut linker)?;
    let pre = CommandPre::new(linker.instantiate_pre(&component)?)?;

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
    let mut store = Store::new(raw, SharedCtx::new(ctx));
    store.set_epoch_deadline(u64::MAX / 2);

    let traces = TraceHarness::install();
    let command = pre
        .instantiate_async(&mut store)
        .await
        .map_err(|error| anyhow::anyhow!("instantiate sqlx-command: {error}"))?;
    let outcome = command
        .wasi_cli_run()
        .call_run(&mut store)
        .await
        .map_err(|error| anyhow::anyhow!("run sqlx-command: {error}"))?;
    ensure!(outcome.is_ok(), "sqlx-command returned an error status");
    drop(command);
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

async fn acceptance(admin: &Client, admin_url: &str, component: &Path) -> anyhow::Result<()> {
    let version: i32 = admin
        .query_one("SELECT current_setting('server_version_num')::integer", &[])
        .await
        .context("read PostgreSQL version")?
        .get(0);
    ensure!(
        version / 10_000 == 18,
        "gate requires PostgreSQL 18, got {version}"
    );
    let database: String = admin
        .query_one("SELECT current_database()", &[])
        .await
        .context("read current database")?
        .get(0);
    let role = generation_role(&database);
    install(admin, &role).await?;

    let spans = run_component(component, guest_url(admin_url, &role)?).await?;
    let rows = admin
        .query(
            &format!("SELECT id, label, owner_name FROM {SCHEMA}.effects ORDER BY id"),
            &[],
        )
        .await
        .context("inspect SQLx transaction effects")?;
    ensure!(
        rows.len() == 1,
        "only the committed row may exist, got {}",
        rows.len()
    );
    ensure!(
        rows[0].get::<_, i32>(0) == 1
            && rows[0].get::<_, String>(1) == "committed"
            && rows[0].get::<_, String>(2) == role,
        "the surviving row must be the committed row owned by the guest login"
    );
    let operations = effect_operations(&spans);
    ensure!(
        operations.contains("txn.query") && operations.contains("txn.execute"),
        "wamn.postgres spans must include txn.query and txn.execute; got {operations:?}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires WAMN_SQLX_TRANSACTION_PG_URL and WAMN_SQLX_TRANSACTION_COMPONENT"]
async fn sqlx_command_commits_rolls_back_and_obeys_current_user_rls() {
    let admin_url = std::env::var(ADMIN_URL_ENV)
        .unwrap_or_else(|_| panic!("set {ADMIN_URL_ENV} to a fresh PostgreSQL 18 superuser URL"));
    let component = std::env::var(COMPONENT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| panic!("set {COMPONENT_ENV} to the built sqlx-command component"));
    let admin = connect(&admin_url)
        .await
        .expect("connect SQLx transaction admin");
    let database: String = admin
        .query_one("SELECT current_database()", &[])
        .await
        .expect("read SQLx transaction database")
        .get(0);
    let role = generation_role(&database);
    assert!(
        fixture_is_clean(&admin, &role)
            .await
            .expect("inspect SQLx transaction fixture"),
        "SQLx transaction gate requires a fresh cluster: wamn_app, {role}, or {SCHEMA} exists"
    );

    let result = acceptance(&admin, &admin_url, &component).await;
    let teardown = cleanup(&admin, &role).await;
    result.expect("SQLx transaction acceptance");
    teardown.expect("SQLx transaction cleanup");
}
