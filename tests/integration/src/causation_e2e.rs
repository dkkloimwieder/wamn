//! Forward M1 causation proof: tenant commit -> CDC -> stored event -> materializer.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use async_nats::header::NATS_MESSAGE_ID;
use clap::Args;
use tokio_postgres::{Client, NoTls};

use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi::p2::bindings::CommandPre;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use crate::cdc_reader_process::{ReaderArgs, ReaderProcess};
use wamn_control_provision::sql as provision_sql;
use wamn_control_registry::identifiers::mvp_execution_target_id;
use wamn_control_registry::sql as registry_sql;
use wamn_event_wire::Op;
use wamn_run_state::admission::registration_evidence;
use wamn_run_state::queue::mint_evt_run_id;
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::wamn_jetstream::{
    self, WAMN_JETSTREAM_ID, WamnJetstream, WamnJetstreamConfig,
};
use wamn_runtime::plugins::wamn_postgres::{
    self, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig,
};

const SYSTEM_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");

const GATE_ID: &str = "wamn-wave8-11-9-forward-causation";
const PROJECT_DATABASE: &str = "wamn_wave8_11_9_project";
const SYSTEM_DATABASE: &str = "wamn_wave8_11_9_system";
const ORG: &str = "wave8";
const PROJECT: &str = "forward";
const ENV: &str = "dev";
const TENANT: &str = "wave8-11-9-tenant";
const SCHEMA: &str = "wamn_wave8_11_9";
const CATALOG_ID: &str = "wave8-11-9-catalog";
const FLOW_ID: &str = "wave8-11-9-flow";
const REGISTRATION_ID: &str = "wave8-11-9-registration";
const ENTITY_ID: &str = "wave8-11-9-receipts";
const TABLE: &str = "receipts";
const CDC_NAME: &str = "wamn_cdc_wave8__forward__dev";
const CDC_PASSWORD: &str = "wave8_11_9_cdc_password";
const STREAM: &str = "EVT_wave8_dev";
const ROOT_RUN_ID: &str = "wave8-11-9-root";
const SOURCE_RUN_ID: &str = "wave8-11-9-source";
const BROKER_DUP_WINDOW_SECS: u64 = 1;
const ARTIFACT_HASH: &str =
    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

#[derive(Debug, Args)]
pub struct CausationE2eArgs {
    /// The compiled production materializer component.
    #[arg(long, default_value = "/bench/materializer.wasm")]
    pub component: PathBuf,

    /// Application-role URL used as the credential/host template for the disposable database.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// Project-cluster admin URL used only to create/drop the disposable database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// System-cluster admin URL used only to create/drop the disposable database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Disposable JetStream-enabled NATS URL.
    #[arg(long, env = "WAMN_EVT_NATS_URL")]
    pub nats_url: String,

    /// Bound for CDC delivery.
    #[arg(long, default_value_t = 120)]
    pub timeout_secs: u64,
}

#[derive(Debug, Default)]
struct GateState {
    reader: Option<ReaderProcess>,
}

#[derive(Debug, PartialEq, Eq)]
struct StoredEvent {
    sequence: u64,
    subject: String,
    message_id: String,
    payload: Vec<u8>,
}

struct MaterializerHarness {
    engine: wash_runtime::engine::Engine,
    pre: CommandPre<SharedCtx>,
    pg: Arc<WamnPostgres>,
    js: Arc<WamnJetstream>,
    report_dir: PathBuf,
}

impl Drop for MaterializerHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.report_dir);
    }
}

fn report_directory() -> PathBuf {
    std::env::temp_dir().join(GATE_ID)
}

impl MaterializerHarness {
    fn plugins(
        &self,
    ) -> std::collections::HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> {
        let mut plugins: std::collections::HashMap<
            &'static str,
            Arc<dyn HostPlugin + Send + Sync>,
        > = std::collections::HashMap::new();
        plugins.insert(WAMN_POSTGRES_ID, self.pg.clone());
        plugins.insert(WAMN_JETSTREAM_ID, self.js.clone());
        plugins
    }

    async fn run(&self) -> anyhow::Result<serde_json::Value> {
        let report_path = self.report_dir.join("counters.json");
        let _ = std::fs::remove_file(&report_path);
        let mut wasi = WasiCtxBuilder::new();
        wasi.args(&["materializer.wasm"])
            .inherit_stdout()
            .inherit_stderr()
            .envs(&[
                ("WAMN_MAT_STREAM", STREAM),
                ("WAMN_MAT_ORG", ORG),
                ("WAMN_MAT_PROJECT", PROJECT),
                ("WAMN_MAT_ENV", ENV),
                ("WAMN_MAT_TENANT", TENANT),
                ("WAMN_MAT_BATCH", "8"),
                ("WAMN_MAT_FETCH_MS", "1500"),
                ("WAMN_MAT_SWEEP_MS", "200"),
                ("WAMN_MAT_MAX_SWEEPS", "2"),
                ("WAMN_MAT_ACK_WAIT_MS", "30000"),
                ("WAMN_MAT_NACK_DELAY_MS", "500"),
                ("WAMN_MAT_REPORT_PATH", "/report/counters.json"),
            ])
            .preopened_dir(
                &self.report_dir,
                "/report",
                DirPerms::all(),
                FilePerms::all(),
            )
            .map_err(|error| anyhow::anyhow!("preopen report directory: {error}"))?;
        let ctx = Ctx::builder(GATE_ID.to_string(), GATE_ID.to_string())
            .with_plugins(self.plugins())
            .with_wasi_ctx(wasi.build())
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);
        let command = self
            .pre
            .instantiate_async(&mut store)
            .await
            .map_err(|error| anyhow::anyhow!("instantiate materializer: {error}"))?;
        let outcome = tokio::time::timeout(
            Duration::from_secs(120),
            command.wasi_cli_run().call_run(&mut store),
        )
        .await
        .context("materializer deadline exceeded")?
        .map_err(|error| anyhow::anyhow!("materializer trapped: {error}"))?;
        ensure!(outcome.is_ok(), "materializer returned an error status");
        let report = std::fs::read_to_string(&report_path)
            .with_context(|| format!("read {}", report_path.display()))?;
        serde_json::from_str(&report).context("parse materializer report")
    }
}

fn flow_json() -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "flow-id": FLOW_ID,
        "version": 1,
        "nodes": [{"id": "event", "type": "event"}],
    })
    .to_string()
}

fn registration_json() -> String {
    wamn_event_reg::EventRegistration {
        schema_version: wamn_event_reg::SCHEMA_VERSION.into(),
        registration_id: REGISTRATION_ID.into(),
        catalog_id: CATALOG_ID.into(),
        flow_id: FLOW_ID.into(),
        entity: wamn_schema_model::EntityId::from(ENTITY_ID),
        ops: vec![Op::Insert],
        condition: None,
    }
    .to_json()
}

fn execution_plan_fixture() -> anyhow::Result<(Vec<u8>, String)> {
    let entry = wamn_catalog::ExecutionNodeId::new("event")?;
    let plan = wamn_catalog::ExecutionPlanV2::new(
        wamn_catalog::ExecutionRuntimeRevision {
            flowrunner_component_digest:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            effect_provider_revision:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            host_effect_contract_version: wamn_catalog::HOST_EFFECT_CONTRACT_VERSION.into(),
        },
        ARTIFACT_HASH,
        wamn_catalog::ExecutionPlanBody {
            entry_instruction: entry.clone(),
            nodes: vec![wamn_catalog::ExecutionPlanNode {
                local_node_id: entry.clone(),
                source_node_id: "event".into(),
                node_type: "event".into(),
                config: serde_json::json!({}),
                effect_policy: wamn_catalog::ExecutionEffectPolicy::Pure,
                source_connection_requirement: None,
            }],
            edges: vec![],
            root_terminal_behavior: wamn_catalog::RootTerminalBehavior::FrontierExhaustion,
            entry_input_schema_guard: serde_json::Value::Bool(true),
            callable_contract: None,
            source_map: vec![wamn_catalog::ExecutionSourceMapEntry {
                local_node_id: entry,
                source_node_id: "event".into(),
            }],
        },
    )?;
    let exact_bytes = serde_json::to_vec(&plan)?;
    let bundle_hash = wamn_catalog::execution_bundle_hash(&exact_bytes);
    Ok((exact_bytes, bundle_hash))
}

fn role_url(admin_url: &str) -> anyhow::Result<String> {
    let plain = admin_url
        .split('?')
        .next()
        .context("empty PostgreSQL URL")?;
    let after_scheme = plain
        .strip_prefix("postgres://")
        .context("PostgreSQL URL must use postgres://")?;
    let (_, host_and_path) = after_scheme
        .rsplit_once('@')
        .context("PostgreSQL URL must carry userinfo")?;
    Ok(format!(
        "postgres://{CDC_NAME}:{CDC_PASSWORD}@{host_and_path}"
    ))
}

/// Replace the database path while preserving connection query parameters.
fn swap_database(url: &str, database: &str) -> anyhow::Result<String> {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (url, None),
    };
    let slash = base
        .rfind('/')
        .context("PostgreSQL URL has no database path")?;
    let mut scoped = format!("{}/{database}", &base[..slash]);
    if let Some(query) = query {
        scoped.push('?');
        scoped.push_str(query);
    }
    Ok(scoped)
}

fn disposable_args(args: &CausationE2eArgs) -> anyhow::Result<CausationE2eArgs> {
    ensure!(
        !args
            .admin_database_url
            .contains(&format!("/{PROJECT_DATABASE}")),
        "project admin URL must connect outside the disposable database"
    );
    ensure!(
        !args
            .system_database_url
            .contains(&format!("/{SYSTEM_DATABASE}")),
        "system admin URL must connect outside the disposable database"
    );
    Ok(CausationE2eArgs {
        component: args.component.clone(),
        database_url: swap_database(&args.database_url, PROJECT_DATABASE)?,
        admin_database_url: swap_database(&args.admin_database_url, PROJECT_DATABASE)?,
        system_database_url: swap_database(&args.system_database_url, SYSTEM_DATABASE)?,
        nats_url: args.nats_url.clone(),
        timeout_secs: args.timeout_secs,
    })
}

async fn provision_databases(args: &CausationE2eArgs) -> anyhow::Result<CausationE2eArgs> {
    let project_admin = connect(&args.admin_database_url).await?;
    project_admin
        .batch_execute(&format!("CREATE DATABASE {PROJECT_DATABASE}"))
        .await
        .context("create disposable project database")?;
    let system_admin = connect(&args.system_database_url).await?;
    system_admin
        .batch_execute(&format!("CREATE DATABASE {SYSTEM_DATABASE}"))
        .await
        .context("create disposable system database")?;
    disposable_args(args)
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| format!("connect PostgreSQL at {url}"))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

async fn setup_registry(system: &mut Client) -> anyhow::Result<()> {
    system
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN; END IF; END $$;",
        )
        .await?;
    system.batch_execute(SYSTEM_SQL).await?;
    let transaction = system.transaction().await?;
    transaction
        .execute(
            registry_sql::upsert_org_sql(),
            &[&ORG, &"dedicated", &Option::<&str>::None],
        )
        .await?;
    transaction
        .execute(
            registry_sql::stamp_env_policy_sql(),
            &[
                &ORG,
                &ENV,
                &r#"{"kind":"pool"}"#,
                &0i32,
                &1i32,
                &"1Gi",
                &"100m",
                &"128Mi",
                &"postgres:18",
                &"",
                &"",
                &"off",
            ],
        )
        .await?;
    transaction
        .execute(registry_sql::upsert_project_sql(), &[&ORG, &PROJECT])
        .await?;
    transaction
        .execute(
            registry_sql::upsert_project_env_sql(),
            &[
                &ORG,
                &PROJECT,
                &ENV,
                &"wamn-wave8-11-9-project-db",
                &Option::<&str>::None,
            ],
        )
        .await?;
    transaction
        .execute(
            registry_sql::upsert_event_reader_sql(),
            &[
                &ORG,
                &PROJECT,
                &ENV,
                &CDC_NAME,
                &CDC_NAME,
                &STREAM,
                &"wamn-wave8-11-9-cdc",
                &Option::<&str>::None,
                &true,
            ],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn setup_project(args: &CausationE2eArgs) -> anyhow::Result<()> {
    let mut admin = connect(&args.admin_database_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()", &[])
        .await?
        .get(0);
    admin
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
               CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
             END IF; END $$; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_scenario_author') THEN \
               CREATE ROLE wamn_scenario_author NOLOGIN; END IF; END $$; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_effect_writer') THEN \
               CREATE ROLE wamn_effect_writer NOLOGIN; END IF; END $$;",
        )
        .await?;
    admin.batch_execute(CATALOG_SQL).await?;
    admin.batch_execute(RUN_STATE_SQL).await?;
    admin.batch_execute(RUN_QUEUE_SQL).await?;
    admin
        .batch_execute(&provision_sql::ensure_schema_sql(SCHEMA))
        .await?;
    admin
        .batch_execute(&format!(
            "GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app; \
             CREATE TABLE {SCHEMA}.{TABLE} (tenant_id text NOT NULL, id text NOT NULL, \
               payload text NOT NULL, PRIMARY KEY (tenant_id,id)); \
             ALTER TABLE {SCHEMA}.{TABLE} ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {SCHEMA}.{TABLE} FORCE ROW LEVEL SECURITY; \
             CREATE POLICY receipts_tenant ON {SCHEMA}.{TABLE} \
               USING (tenant_id=NULLIF(current_setting('app.tenant',true),'')) \
               WITH CHECK (tenant_id=NULLIF(current_setting('app.tenant',true),'')); \
             GRANT SELECT,INSERT,UPDATE,DELETE ON {SCHEMA}.{TABLE} TO wamn_app; \
             CREATE TABLE {SCHEMA}.wamn_entities (relation_oid oid PRIMARY KEY, \
               entity_id text NOT NULL, table_name text NOT NULL); \
             INSERT INTO {SCHEMA}.wamn_entities VALUES \
               ('{SCHEMA}.{TABLE}'::regclass::oid,'{ENTITY_ID}','{TABLE}');"
        ))
        .await?;
    admin
        .batch_execute(&provision_sql::ensure_replication_role_sql(
            CDC_NAME,
            CDC_PASSWORD,
        ))
        .await?;
    admin
        .batch_execute(&provision_sql::create_publication_sql(CDC_NAME, SCHEMA))
        .await?;
    admin
        .batch_execute(&provision_sql::grant_replication_access_sql(
            &database, CDC_NAME, SCHEMA,
        ))
        .await?;

    let graph = flow_json();
    let registration = registration_json();
    let (execution_plan, bundle_hash) = execution_plan_fixture()?;
    let members = serde_json::json!([{
        "flow-id": FLOW_ID, "flow-version": 1, "artifact-hash": ARTIFACT_HASH
    }]);
    let transaction = admin.transaction().await?;
    transaction
        .execute(
            "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version,state) \
         VALUES ($1,$2,1,$3,'0.1','applied')",
            &[&TENANT, &CATALOG_ID, &ENV],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.flow_artifacts \
           (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash) \
         VALUES ($1,$2,1,'0.1',$3::text::jsonb,'wave8-11-9-graph',$4)",
            &[&TENANT, &FLOW_ID, &graph, &ARTIFACT_HASH],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.release_manifests \
           (tenant_id,catalog_id,catalog_version,members_json) VALUES ($1,$2,1,$3)",
            &[&TENANT, &CATALOG_ID, &members],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.execution_bundles \
           (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
         VALUES ($1,$2,'0.1',$3,$4)",
            &[
                &TENANT,
                &bundle_hash,
                &execution_plan,
                &(execution_plan.len() as i32),
            ],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.release_flows \
           (tenant_id,catalog_id,catalog_version,flow_id,flow_version,execution_bundle_hash) \
         VALUES ($1,$2,1,$3,1,$4)",
            &[&TENANT, &CATALOG_ID, &FLOW_ID, &bundle_hash],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.catalog_heads \
           (tenant_id,catalog_id,environment,applied_catalog_version) VALUES ($1,$2,$3,1)",
            &[&TENANT, &CATALOG_ID, &ENV],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.event_registrations \
           (tenant_id,catalog_id,registration_id,flow_id,entity_id,registration) \
         VALUES ($1,$2,$3,$4,$5,$6::text::jsonb)",
            &[
                &TENANT,
                &CATALOG_ID,
                &REGISTRATION_ID,
                &FLOW_ID,
                &ENTITY_ID,
                &registration,
            ],
        )
        .await?;
    transaction.execute(
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status,trigger_source,event_source_run_id,event_root_run_id,event_depth) \
         VALUES \
           ($1,$2,$3,1,$4,1,$5,$6,'completed','event',$2,$2,0), \
           ($1,$7,$3,1,$4,1,$5,$6,'completed','event',$2,$2,1)",
        &[&TENANT, &ROOT_RUN_ID, &FLOW_ID, &CATALOG_ID, &ENV, &bundle_hash, &SOURCE_RUN_ID],
    ).await?;
    transaction.commit().await?;

    // Capture starts after all fixture writes; only the matching tenant commit is observable.
    admin
        .batch_execute(&provision_sql::create_failover_slot_sql(CDC_NAME))
        .await?;
    Ok(())
}

fn reader_args(args: &CausationE2eArgs) -> anyhow::Result<ReaderArgs> {
    Ok(ReaderArgs {
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENV.into(),
        system_database_url: args.system_database_url.clone(),
        cdc_url: role_url(&args.admin_database_url)?,
        nats_url: args.nats_url.clone(),
        stream_replicas: 1,
    })
}

async fn wait_for_stream(args: &CausationE2eArgs) -> anyhow::Result<()> {
    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let deadline = Instant::now() + Duration::from_secs(10);
    while js.get_stream(STREAM).await.is_err() {
        ensure!(Instant::now() < deadline, "reader did not create {STREAM}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

fn tenant_commit_sql() -> String {
    format!(
        "SELECT pg_logical_emit_message(true, 'wamn.causation', \
           '{{\"run\":\"{SOURCE_RUN_ID}\",\"root\":\"{ROOT_RUN_ID}\",\"depth\":1}}');"
    )
}

async fn commit_tenant_event(args: &CausationE2eArgs) -> anyhow::Result<()> {
    let mut app = connect(&args.database_url).await?;
    app.batch_execute(&format!(
        "SET search_path TO {SCHEMA}; SET app.tenant TO '{TENANT}';"
    ))
    .await?;
    let transaction = app.transaction().await?;
    transaction.batch_execute(&tenant_commit_sql()).await?;
    transaction
        .execute(
            &format!("INSERT INTO {SCHEMA}.{TABLE} (tenant_id,id,payload) VALUES ($1,$2,$3)"),
            &[&TENANT, &"forward-1", &"committed"],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn wait_for_stored_event(args: &CausationE2eArgs) -> anyhow::Result<StoredEvent> {
    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    loop {
        if let Ok(mut stream) = js.get_stream(STREAM).await {
            let info = stream.info().await?;
            let messages = info.state.messages;
            let first_sequence = info.state.first_sequence;
            if messages == 1 {
                let message = stream.get_raw_message(first_sequence).await?;
                return Ok(StoredEvent {
                    sequence: message.sequence,
                    subject: message.subject.to_string(),
                    message_id: message
                        .headers
                        .get(NATS_MESSAGE_ID)
                        .map(ToString::to_string)
                        .context("stored CDC event lacks Nats-Msg-Id")?,
                    payload: message.payload.to_vec(),
                });
            }
            ensure!(messages <= 1, "expected one CDC event, found {}", messages);
        }
        ensure!(
            Instant::now() < deadline,
            "CDC event did not reach {STREAM}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn stream_message_count(args: &CausationE2eArgs) -> anyhow::Result<u64> {
    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let mut stream = js.get_stream(STREAM).await?;
    Ok(stream.info().await?.state.messages)
}

async fn build_materializer(args: &CausationE2eArgs) -> anyhow::Result<MaterializerHarness> {
    wash_runtime::init_crypto();
    let guest = std::fs::read(&args.component)
        .with_context(|| format!("read {}", args.component.display()))?;
    let mut pg_config = WamnPostgresConfig::from_env();
    pg_config.database_url = Some(args.database_url.clone());
    let pg = Arc::new(WamnPostgres::new(pg_config)?);
    pg.set_tenant(GATE_ID, TENANT)?;
    pg.set_schema(GATE_ID, "wamn_run")?;
    pg.probe_checkout().await?;
    let jetstream = Arc::new(
        WamnJetstream::new(WamnJetstreamConfig {
            nats_url: Some(args.nats_url.clone()),
        })
        .with_doorbell(async_nats::connect(&args.nats_url).await?),
    );
    jetstream.set_execution_target(GATE_ID, mvp_execution_target_id(TENANT)?);
    let engine = build_engine(&[])?;
    let raw: &RawEngine = engine.inner();
    let component = WasmtimeComponent::new(raw, &guest)
        .map_err(|error| anyhow::anyhow!("compile materializer: {error}"))?;
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wamn_postgres::add_to_linker(&mut linker)?;
    wamn_jetstream::add_to_linker(&mut linker)?;
    let pre = CommandPre::new(linker.instantiate_pre(&component)?)?;
    let report_dir = report_directory();
    std::fs::create_dir_all(&report_dir)?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    std::mem::forget(ticker);
    Ok(MaterializerHarness {
        engine,
        pre,
        pg,
        js: jetstream,
        report_dir,
    })
}

fn counter(report: &serde_json::Value, name: &str) -> i64 {
    report
        .get(name)
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(-1)
}

fn is_post_window_storage_duplicate(
    first: &StoredEvent,
    second: &StoredEvent,
    elapsed: Duration,
    stored_messages: u64,
    report: &serde_json::Value,
) -> bool {
    elapsed > Duration::from_secs(BROKER_DUP_WINDOW_SECS)
        && second == first
        && stored_messages == 1
        && counter(report, "fired") == 0
        && counter(report, "duplicate") == 1
}

async fn assert_one_causal_run(
    admin: &Client,
    sequence: u64,
    expected_source_event_id: &str,
) -> anyhow::Result<()> {
    let expected_run_id = mint_evt_run_id(&format!("{FLOW_ID}:{REGISTRATION_ID}"), sequence);
    let registration: serde_json::Value = serde_json::from_str(&registration_json())?;
    let (_, expected_hash) = registration_evidence(&registration);
    let run_count: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.runs \
             WHERE tenant_id=$1 AND registration_id=$2",
            &[&TENANT, &REGISTRATION_ID],
        )
        .await?
        .get(0);
    ensure!(run_count == 1, "expected exactly one registration run");
    let row = admin
        .query_one(
            "SELECT r.run_id, r.tenant_id, r.registration_id, r.idempotency_key, \
                r.invocation_context->'source'->>'registration-hash', \
                r.invocation_context->'source'->>'entity', \
                r.invocation_context->'source'->>'seq', \
                r.invocation_context->'source'->>'source-event-id', r.event_source_run_id, \
                r.event_root_run_id, r.event_depth \
         FROM wamn_run.runs r \
         WHERE r.tenant_id=$1 AND r.registration_id=$2",
            &[&TENANT, &REGISTRATION_ID],
        )
        .await?;
    ensure!(
        row.get::<_, String>(0) == expected_run_id,
        "event run id drifted"
    );
    ensure!(row.get::<_, String>(1) == TENANT, "tenant identity drifted");
    ensure!(
        row.get::<_, String>(2) == REGISTRATION_ID,
        "registration identity drifted"
    );
    ensure!(
        row.get::<_, String>(3) == format!("evt:{REGISTRATION_ID}:{sequence}"),
        "producer coordinate drifted"
    );
    ensure!(
        row.get::<_, String>(4) == expected_hash,
        "registration evidence hash drifted"
    );
    ensure!(
        row.get::<_, String>(5) == ENTITY_ID,
        "entity identity drifted"
    );
    ensure!(
        row.get::<_, String>(6) == sequence.to_string(),
        "source sequence drifted"
    );
    ensure!(
        row.get::<_, String>(7) == expected_source_event_id,
        "source-event identity drifted"
    );
    ensure!(
        row.get::<_, String>(8) == SOURCE_RUN_ID,
        "source run lineage drifted"
    );
    ensure!(
        row.get::<_, String>(9) == ROOT_RUN_ID,
        "root lineage drifted"
    );
    ensure!(row.get::<_, i32>(10) == 2, "depth lineage drifted");
    let queue_count: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.run_queue q \
             WHERE q.tenant_id=$1 AND q.run_id IN ( \
               SELECT r.run_id FROM wamn_run.runs r \
               WHERE r.tenant_id=$1 AND r.registration_id=$2)",
            &[&TENANT, &REGISTRATION_ID],
        )
        .await?
        .get(0);
    ensure!(queue_count == 1, "expected exactly one causal queue row");
    let queue_sequence: i64 = admin
        .query_one(
            "SELECT stream_seq FROM wamn_run.run_queue WHERE tenant_id=$1 AND run_id=$2",
            &[&TENANT, &expected_run_id],
        )
        .await?
        .get(0);
    ensure!(queue_sequence == sequence as i64, "queue sequence drifted");
    Ok(())
}

async fn cleanup(args: &CausationE2eArgs, state: &mut GateState) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    if let Some(reader) = state.reader.take() {
        match reader.shutdown(Duration::from_secs(15)).await {
            Ok(true) => {}
            Ok(false) => errors.push("CDC reader did not shut down successfully".to_string()),
            Err(error) => errors.push(format!("shut down CDC reader: {error:#}")),
        }
    }
    if state.reader.is_some() {
        errors.push("CDC reader handle remains after cleanup".to_string());
    }

    match async_nats::connect(&args.nats_url).await {
        Ok(nats) => {
            let js = async_nats::jetstream::new(nats);
            match js.get_stream(STREAM).await {
                Ok(_) => {
                    if let Err(error) = js.delete_stream(STREAM).await {
                        errors.push(format!("delete stream {STREAM}: {error:#}"));
                    }
                }
                Err(error) if stream_not_found(&error) => {}
                Err(error) => errors.push(format!("inspect stream {STREAM}: {error:#}")),
            }
            match js.get_stream(STREAM).await {
                Ok(_) => errors.push(format!("stream {STREAM} remains after cleanup")),
                Err(error) if stream_not_found(&error) => {}
                Err(error) => errors.push(format!("verify stream {STREAM} absence: {error:#}")),
            }
        }
        Err(error) => errors.push(format!("connect NATS for cleanup: {error:#}")),
    }

    let report_dir = report_directory();
    match std::fs::remove_dir_all(&report_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => errors.push(format!("remove {}: {error}", report_dir.display())),
    }
    if report_dir.exists() {
        errors.push(format!("report directory {} remains", report_dir.display()));
    }

    match connect(&args.admin_database_url).await {
        Ok(project_admin) => {
            if let Err(error) = project_admin
                .batch_execute(&format!(
                    "DROP DATABASE IF EXISTS {PROJECT_DATABASE} WITH (FORCE)"
                ))
                .await
            {
                errors.push(format!("drop disposable project database: {error:#}"));
            }
            if let Err(error) = project_admin
                .batch_execute(&format!("DROP ROLE IF EXISTS {CDC_NAME}"))
                .await
            {
                errors.push(format!("drop disposable CDC role: {error:#}"));
            }
            match project_admin
                .query_one(
                    "SELECT count(*) FROM pg_database WHERE datname=$1",
                    &[&PROJECT_DATABASE],
                )
                .await
            {
                Ok(row) if row.get::<_, i64>(0) == 0 => {}
                Ok(_) => errors.push(format!(
                    "disposable project database {PROJECT_DATABASE} remains"
                )),
                Err(error) => errors.push(format!("verify project database absence: {error:#}")),
            }
            match project_admin
                .query_one(
                    "SELECT count(*) FROM pg_roles WHERE rolname=$1",
                    &[&CDC_NAME],
                )
                .await
            {
                Ok(row) if row.get::<_, i64>(0) == 0 => {}
                Ok(_) => errors.push(format!("disposable CDC role {CDC_NAME} remains")),
                Err(error) => errors.push(format!("verify CDC role absence: {error:#}")),
            }
        }
        Err(error) => errors.push(format!("connect project admin for cleanup: {error:#}")),
    }

    match connect(&args.system_database_url).await {
        Ok(system_admin) => {
            if let Err(error) = system_admin
                .batch_execute(&format!(
                    "DROP DATABASE IF EXISTS {SYSTEM_DATABASE} WITH (FORCE)"
                ))
                .await
            {
                errors.push(format!("drop disposable system database: {error:#}"));
            }
            match system_admin
                .query_one(
                    "SELECT count(*) FROM pg_database WHERE datname=$1",
                    &[&SYSTEM_DATABASE],
                )
                .await
            {
                Ok(row) if row.get::<_, i64>(0) == 0 => {}
                Ok(_) => errors.push(format!(
                    "disposable system database {SYSTEM_DATABASE} remains"
                )),
                Err(error) => errors.push(format!("verify system database absence: {error:#}")),
            }
        }
        Err(error) => errors.push(format!("connect system admin for cleanup: {error:#}")),
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(errors.join("; ")))
    }
}

fn stream_not_found(error: &async_nats::jetstream::context::GetStreamError) -> bool {
    matches!(
        error.kind(),
        async_nats::jetstream::context::GetStreamErrorKind::JetStream(error)
            if error.error_code() == async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND
    )
}

fn finish_gate(run: anyhow::Result<()>, cleanup: anyhow::Result<()>) -> anyhow::Result<()> {
    match (run, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(anyhow::anyhow!(
            "{primary:#}; cleanup also failed: {cleanup:#}"
        )),
    }
}

async fn run_forward(args: &CausationE2eArgs, state: &mut GateState) -> anyhow::Result<()> {
    let mut system = connect(&args.system_database_url).await?;
    setup_registry(&mut system).await?;
    setup_project(args).await?;
    state.reader = Some(ReaderProcess::spawn_with_dup_window(
        reader_args(args)?,
        BROKER_DUP_WINDOW_SECS,
    )?);
    wait_for_stream(args).await?;
    commit_tenant_event(args).await?;

    let first_delivery = wait_for_stored_event(args).await?;
    let first_observed_at = Instant::now();
    let envelope: wamn_event_wire::Envelope = serde_json::from_slice(&first_delivery.payload)?;
    ensure!(
        first_delivery.subject
            == wamn_event_wire::subject(ORG, PROJECT, ENV, ENTITY_ID, Op::Insert),
        "stored source-event subject drifted"
    );
    ensure!(
        first_delivery.message_id == wamn_event_wire::msg_id(PROJECT, ENV, envelope.lsn),
        "stored source-event Nats-Msg-Id/LSN identity drifted"
    );
    ensure!(
        envelope.lsn != first_delivery.sequence,
        "source-event LSN was not independently distinguished from stream sequence"
    );
    ensure!(
        envelope.entity.as_deref() == Some(ENTITY_ID),
        "CDC entity identity drifted"
    );
    ensure!(
        envelope.causation.as_ref().map(|value| value.run.as_str()) == Some(SOURCE_RUN_ID),
        "CDC source-run stamp drifted"
    );
    ensure!(
        envelope.causation.as_ref().map(|value| value.root.as_str()) == Some(ROOT_RUN_ID),
        "CDC root stamp drifted"
    );
    ensure!(
        envelope.causation.as_ref().map(|value| value.depth) == Some(1),
        "CDC depth stamp drifted"
    );

    let materializer = build_materializer(args).await?;
    let first_report = materializer.run().await?;
    ensure!(
        counter(&first_report, "fired") == 1,
        "first delivery did not admit once: {first_report}"
    );
    ensure!(
        counter(&first_report, "duplicate") == 0,
        "first delivery was unexpectedly duplicate: {first_report}"
    );
    let admin = connect(&args.admin_database_url).await?;
    assert_one_causal_run(&admin, first_delivery.sequence, &first_delivery.message_id).await?;
    ensure!(
        stream_message_count(args).await? == 1,
        "materializer republished the source event"
    );

    // Cross the gate-specific broker dedup horizon before re-consuming the one
    // stored record. No publish occurs here: only the durable is recreated.
    tokio::time::sleep(Duration::from_secs(BROKER_DUP_WINDOW_SECS) + Duration::from_millis(100))
        .await;

    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let stream = js.get_stream(STREAM).await?;
    stream
        .delete_consumer(&format!("mat_{TENANT}_{CATALOG_ID}_{REGISTRATION_ID}"))
        .await
        .context("delete materializer durable to force stored redelivery")?;
    let second_delivery = wait_for_stored_event(args).await?;
    let second_report = materializer.run().await?;
    let stored_messages = stream_message_count(args).await?;
    ensure!(
        is_post_window_storage_duplicate(
            &first_delivery,
            &second_delivery,
            first_observed_at.elapsed(),
            stored_messages,
            &second_report,
        ),
        "post-window stored redelivery was not byte-exact and storage-deduplicated: \
         elapsed={:?} messages={stored_messages} report={second_report}",
        first_observed_at.elapsed()
    );
    assert_one_causal_run(&admin, first_delivery.sequence, &first_delivery.message_id).await?;
    ensure!(stored_messages == 1, "redelivery republished the event");
    Ok(())
}

pub async fn run(args: CausationE2eArgs) -> anyhow::Result<()> {
    println!("# wamn-gates causation-e2e — tenant commit -> CDC -> stored event -> materializer");
    let mut state = GateState::default();
    cleanup(&args, &mut state).await?;
    let result = async {
        let disposable = provision_databases(&args).await?;
        run_forward(&disposable, &mut state).await
    }
    .await;
    let cleanup = cleanup(&args, &mut state).await;
    finish_gate(result, cleanup)?;
    println!(
        "causation-e2e complete — one causal run/queue fact, byte-identical redelivery deduplicated"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> CausationE2eArgs {
        CausationE2eArgs {
            component: "/bench/materializer.wasm".into(),
            database_url: "postgres://wamn_app:wamn_app@postgres/wamn".into(),
            admin_database_url: "postgres://postgres:postgres@postgres/wamn".into(),
            system_database_url: "postgres://postgres:postgres@system/wamn".into(),
            nats_url: "nats://nats:4222".into(),
            timeout_secs: 120,
        }
    }

    #[test]
    fn forward_contract_pins_registration_and_durable_coordinate() {
        let registration: serde_json::Value = serde_json::from_str(&registration_json()).unwrap();
        assert_eq!(registration["registration-id"], REGISTRATION_ID);
        assert_eq!(registration["catalog-id"], CATALOG_ID);
        assert_eq!(registration["flow-id"], FLOW_ID);
        assert_eq!(registration["entity"], ENTITY_ID);
        assert_eq!(registration["ops"], serde_json::json!(["insert"]));
        assert_eq!(
            mint_evt_run_id(&format!("{FLOW_ID}:{REGISTRATION_ID}"), 7),
            "wave8-11-9-flow:wave8-11-9-registration:evt:00000000000000000007"
        );
        assert_eq!(
            format!("evt:{REGISTRATION_ID}:7"),
            "evt:wave8-11-9-registration:7"
        );
    }

    #[test]
    fn tenant_commit_is_direct_transactional_and_reader_is_single_node() {
        let sql = tenant_commit_sql();
        assert!(sql.contains("pg_logical_emit_message(true, 'wamn.causation'"));
        assert!(sql.contains(SOURCE_RUN_ID));
        assert!(sql.contains(ROOT_RUN_ID));
        assert!(sql.contains("\"depth\":1"));
        assert!(!sql.contains("wamn_run"));
        assert!(!sql.contains("publish"));
        assert_eq!(reader_args(&args()).unwrap().stream_replicas, 1);
    }

    #[test]
    fn stored_redelivery_contract_is_byte_and_sequence_exact() {
        let first = StoredEvent {
            sequence: 41,
            subject: "evt.wave8.forward.dev.wave8-11-9-receipts.insert".into(),
            message_id: "forward_dev:99".into(),
            payload: br#"{"exact":"stored-bytes"}"#.to_vec(),
        };
        let second = StoredEvent {
            sequence: 41,
            subject: first.subject.clone(),
            message_id: first.message_id.clone(),
            payload: first.payload.clone(),
        };
        let duplicate = serde_json::json!({"fired": 0, "duplicate": 1});
        let after_window = Duration::from_secs(BROKER_DUP_WINDOW_SECS) + Duration::from_millis(100);
        assert!(is_post_window_storage_duplicate(
            &first,
            &second,
            after_window,
            1,
            &duplicate,
        ));
        assert!(!is_post_window_storage_duplicate(
            &first,
            &second,
            Duration::from_secs(BROKER_DUP_WINDOW_SECS),
            1,
            &duplicate,
        ));
        assert!(!is_post_window_storage_duplicate(
            &first,
            &second,
            after_window,
            2,
            &duplicate,
        ));
        assert_eq!(STREAM, "EVT_wave8_dev");
    }

    #[test]
    fn shared_job_urls_are_only_seed_authorities_for_disposable_databases() {
        let mut base = args();
        base.database_url = "postgres://wamn_app:wamn_app@postgres/wamn?sslmode=disable".into();
        base.admin_database_url =
            "postgres://postgres:postgres@postgres/wamn?sslmode=disable".into();
        let scoped = disposable_args(&base).unwrap();
        assert_eq!(
            scoped.database_url,
            format!("postgres://wamn_app:wamn_app@postgres/{PROJECT_DATABASE}?sslmode=disable")
        );
        assert_eq!(
            scoped.admin_database_url,
            format!("postgres://postgres:postgres@postgres/{PROJECT_DATABASE}?sslmode=disable")
        );
        assert_eq!(
            scoped.system_database_url,
            format!("postgres://postgres:postgres@system/{SYSTEM_DATABASE}")
        );
        assert_eq!(
            report_directory()
                .file_name()
                .and_then(|name| name.to_str()),
            Some(GATE_ID)
        );

        let job = include_str!("../../../deploy/gates/causation-e2e-job.yaml");
        assert!(job.contains("wamn-runner-db-admin"));
        assert!(job.contains("WAMN_SYSTEM_ADMIN_URL"));
        let whole_schema_drop = ["DROP", "SCHEMA"].join(" ");
        assert!(!include_str!("causation_e2e.rs").contains(&whole_schema_drop));
    }

    #[test]
    fn cleanup_failure_never_masks_the_primary_gate_error() {
        let error = finish_gate(
            Err(anyhow::anyhow!("primary proof failure")),
            Err(anyhow::anyhow!("residue cleanup failure")),
        )
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("primary proof failure"));
        assert!(error.contains("cleanup also failed: residue cleanup failure"));
    }
}
