//! Forward M1 causation proof: tenant commit -> CDC -> stored event -> materializer.

use std::io::Read as _;
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

const BROKER_DUP_WINDOW_SECS: u64 = 1;
const SIDECAR_POSTGRES_MAJOR: i64 = 18;
const ARTIFACT_HASH: &str =
    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

#[derive(Debug, Args)]
pub struct CausationE2eArgs {
    /// The compiled production materializer component.
    #[arg(long, default_value = "/bench/materializer.wasm")]
    pub component: PathBuf,

    /// Loopback application-role URL used as the template for the scratch project database.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// Loopback superuser URL used only to create/drop the scratch project database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// The same loopback superuser URL, used for the scratch system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Disposable JetStream-enabled NATS URL.
    #[arg(long, env = "WAMN_EVT_NATS_URL")]
    pub nats_url: String,

    /// Generated Kubernetes Job name from the Pod's automatic job-name label.
    #[arg(long, env = "WAMN_GATE_JOB_NAME")]
    pub job_name: String,

    /// Kubernetes Job UID from the Pod's automatic controller-uid label.
    #[arg(long, env = "WAMN_GATE_JOB_UID")]
    pub job_uid: String,

    /// Bound for CDC delivery.
    #[arg(long, default_value_t = 120)]
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GateResources {
    job_name: String,
    job_uid: String,
    suffix: String,
    owner: String,
    gate_id: String,
    project_database: String,
    system_database: String,
    schema: String,
    table: String,
    cdc_name: String,
    cdc_password: String,
    stream: String,
    org: String,
    project: String,
    env: String,
    tenant: String,
    catalog_id: String,
    flow_id: String,
    registration_id: String,
    entity_id: String,
    root_run_id: String,
    source_run_id: String,
    durable: String,
    report_dir: PathBuf,
}

impl GateResources {
    fn from_args(args: &CausationE2eArgs) -> anyhow::Result<Self> {
        ensure!(
            args.job_name.starts_with("m1-gate-")
                && args
                    .job_name
                    .chars()
                    .all(|character| character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || character == '-'),
            "M1 Job name must be a generated m1-gate-* DNS label"
        );
        let uid_parts = args.job_uid.split('-').collect::<Vec<_>>();
        ensure!(
            uid_parts.len() == 5
                && uid_parts.iter().map(|part| part.len()).eq([8, 4, 4, 4, 12])
                && uid_parts
                    .iter()
                    .all(|part| { part.chars().all(|character| character.is_ascii_hexdigit()) }),
            "M1 Job UID must be a canonical Kubernetes UID"
        );
        ensure!(
            args.job_name.len() <= 63
                && !args.job_name.ends_with('-')
                && !args.job_name.contains("--"),
            "M1 Job name must remain a bounded DNS label"
        );
        let uid = args.job_uid.replace('-', "").to_ascii_lowercase();
        ensure!(
            uid.len() == 32,
            "M1 Job UID must carry 32 hexadecimal digits"
        );
        let suffix = uid;
        let owner = format!("wamn-m1-job:{}/{}", args.job_name, args.job_uid);
        let tenant = format!("t-{suffix}");
        let catalog_id = format!("c-{suffix}");
        let registration_id = format!("r-{suffix}");
        let durable = format!("mat_{tenant}_{catalog_id}_{registration_id}");
        let mut secret = [0u8; 24];
        std::fs::File::open("/dev/urandom")
            .context("open operating-system random source")?
            .read_exact(&mut secret)
            .context("read run-scoped CDC secret")?;
        let resources = Self {
            job_name: args.job_name.clone(),
            job_uid: args.job_uid.clone(),
            gate_id: format!("wamn-m1-{suffix}"),
            project_database: format!("m1p_{suffix}"),
            system_database: format!("m1y_{suffix}"),
            schema: format!("m1s_{suffix}"),
            table: format!("receipts_{suffix}"),
            cdc_name: format!("m1cdc_{suffix}"),
            cdc_password: hex::encode(secret),
            stream: format!("M1_{suffix}"),
            org: format!("o-{}", &suffix[..10]),
            project: format!("p-{}", &suffix[10..20]),
            env: format!("e-{}", &suffix[20..]),
            tenant,
            catalog_id,
            flow_id: format!("f-{suffix}"),
            registration_id,
            entity_id: format!("x-{suffix}"),
            root_run_id: format!("root-{suffix}"),
            source_run_id: format!("source-{suffix}"),
            durable,
            report_dir: std::env::temp_dir().join(format!("wamn-m1-{suffix}")),
            suffix,
            owner,
        };
        resources.validate()?;
        Ok(resources)
    }

    fn validate(&self) -> anyhow::Result<()> {
        fn pg_identifier(name: &str, label: &str) -> anyhow::Result<()> {
            ensure!(
                !name.is_empty() && name.len() <= 63,
                "{label} exceeds PostgreSQL identifier bounds"
            );
            ensure!(
                name.starts_with(|character: char| character.is_ascii_lowercase())
                    && name.chars().all(|character| {
                        character.is_ascii_lowercase()
                            || character.is_ascii_digit()
                            || character == '_'
                    }),
                "{label} is not a conservative PostgreSQL identifier"
            );
            Ok(())
        }
        fn nats_name(name: &str, label: &str) -> anyhow::Result<()> {
            ensure!(
                !name.is_empty() && name.len() <= 255,
                "{label} exceeds JetStream name bounds"
            );
            ensure!(
                name.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '-' || character == '_'
                }),
                "{label} contains a JetStream-reserved character"
            );
            Ok(())
        }
        for (name, label) in [
            (&self.project_database, "project database"),
            (&self.system_database, "system database"),
            (&self.schema, "scratch schema"),
            (&self.table, "scratch table"),
            (&self.cdc_name, "CDC role/publication/slot"),
        ] {
            pg_identifier(name, label)?;
        }
        nats_name(&self.stream, "event stream")?;
        nats_name(&self.durable, "materializer durable")?;
        wamn_control_provision::validate_project_env(&self.org, &self.project, &self.env)
            .context("M1 org/project/environment identifiers")?;
        ensure!(
            wamn_control_registry::identifiers::valid_tenant(&self.tenant),
            "M1 tenant violates the canonical tenant identifier contract"
        );
        ensure!(
            format!(
                "{}{}{}",
                self.org.trim_start_matches("o-"),
                self.project.trim_start_matches("p-"),
                self.env.trim_start_matches("e-")
            ) == self.suffix,
            "M1 org/project/environment triple must reconstruct the exact run suffix"
        );
        for (value, label) in [
            (&self.gate_id, "gate id"),
            (&self.tenant, "tenant"),
            (&self.catalog_id, "catalog"),
            (&self.flow_id, "flow"),
            (&self.registration_id, "registration"),
            (&self.entity_id, "entity"),
            (&self.root_run_id, "root run"),
            (&self.source_run_id, "source run"),
        ] {
            ensure!(
                value.contains(&self.suffix) && value.len() <= 255,
                "{label} must contain the exact run suffix within its length bound"
            );
        }
        ensure!(
            self.report_dir.file_name().and_then(|name| name.to_str())
                == Some(self.gate_id.as_str()),
            "report directory must be exactly the run-scoped gate id"
        );
        Ok(())
    }

    fn resource_record(&self) -> serde_json::Value {
        serde_json::json!({
                "job_name": self.job_name,
                "job_uid": self.job_uid,
                "suffix": self.suffix,
                "project_database": self.project_database,
                "system_database": self.system_database,
                "cdc_role": self.cdc_name,
                "schema": self.schema,
                "table": self.table,
                "publication": self.cdc_name,
                "slot": self.cdc_name,
                "stream": self.stream,
                "durable": self.durable,
                "report_dir": self.report_dir,
                "org": self.org,
                "project": self.project,
                "environment": self.env,
                "tenant": self.tenant,
                "catalog": self.catalog_id,
                "flow": self.flow_id,
                "registration": self.registration_id,
                "entity": self.entity_id,
                "root_run": self.root_run_id,
                "source_run": self.source_run_id,
        })
    }

    fn log_record(&self) {
        println!("M1_RESOURCE_RECORD {}", self.resource_record());
    }
}

fn prepare_report_dir(resources: &GateResources, state: &mut GateState) -> anyhow::Result<()> {
    ensure!(
        !resources.report_dir.exists(),
        "exact report directory remained after preclean"
    );
    let setup_dir = resources.report_dir.with_file_name(format!(
        ".{}.setup-{}",
        resources.gate_id,
        std::process::id()
    ));
    ensure!(
        !setup_dir.exists(),
        "exact report setup directory already exists"
    );
    std::fs::create_dir(&setup_dir).context("create exact report setup directory")?;
    let prepared = (|| -> anyhow::Result<()> {
        std::fs::write(setup_dir.join(".wamn-m1-owner"), &resources.owner)
            .context("write exact report owner marker")?;
        std::fs::write(
            setup_dir.join("resource-record.json"),
            serde_json::to_vec(&resources.resource_record())?,
        )
        .context("write exact durable setup record")?;
        std::fs::rename(&setup_dir, &resources.report_dir)
            .context("atomically publish exact report setup record")?;
        Ok(())
    })();
    if prepared.is_err() && setup_dir.exists() {
        let _ = std::fs::remove_dir_all(&setup_dir);
    }
    prepared?;
    state.ledger.report_dir = true;
    Ok(())
}

#[derive(Debug, Default)]
struct GateState {
    reader: Option<ReaderProcess>,
    ledger: SetupLedger,
}

#[derive(Debug, Default)]
struct SetupLedger {
    project_database: bool,
    system_database: bool,
    cdc_role: bool,
    schema: bool,
    publication: bool,
    slot: bool,
    stream: bool,
    durable: bool,
    report_dir: bool,
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
    resources: GateResources,
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
                ("WAMN_MAT_STREAM", self.resources.stream.as_str()),
                ("WAMN_MAT_ORG", self.resources.org.as_str()),
                ("WAMN_MAT_PROJECT", self.resources.project.as_str()),
                ("WAMN_MAT_ENV", self.resources.env.as_str()),
                ("WAMN_MAT_TENANT", self.resources.tenant.as_str()),
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
        let ctx = Ctx::builder(
            self.resources.gate_id.clone(),
            self.resources.gate_id.clone(),
        )
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

fn flow_json(resources: &GateResources) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "flow-id": resources.flow_id,
        "version": 1,
        "nodes": [{"id": "event", "type": "event"}],
    })
    .to_string()
}

fn registration_json(resources: &GateResources) -> String {
    wamn_event_reg::EventRegistration {
        schema_version: wamn_event_reg::SCHEMA_VERSION.into(),
        registration_id: resources.registration_id.clone(),
        catalog_id: resources.catalog_id.clone(),
        flow_id: resources.flow_id.clone(),
        entity: wamn_schema_model::EntityId::from(resources.entity_id.as_str()),
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

fn role_url(admin_url: &str, resources: &GateResources) -> anyhow::Result<String> {
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
        "postgres://{}:{}@{host_and_path}",
        resources.cdc_name, resources.cdc_password
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

fn disposable_args(
    args: &CausationE2eArgs,
    resources: &GateResources,
) -> anyhow::Result<CausationE2eArgs> {
    ensure!(
        !args
            .admin_database_url
            .contains(&format!("/{}", resources.project_database)),
        "project admin URL must connect outside the disposable database"
    );
    ensure!(
        !args
            .system_database_url
            .contains(&format!("/{}", resources.system_database)),
        "system admin URL must connect outside the disposable database"
    );
    Ok(CausationE2eArgs {
        component: args.component.clone(),
        database_url: swap_database(&args.database_url, &resources.project_database)?,
        admin_database_url: swap_database(&args.admin_database_url, &resources.project_database)?,
        system_database_url: swap_database(&args.system_database_url, &resources.system_database)?,
        nats_url: args.nats_url.clone(),
        job_name: args.job_name.clone(),
        job_uid: args.job_uid.clone(),
        timeout_secs: args.timeout_secs,
    })
}

async fn provision_databases(
    args: &CausationE2eArgs,
    resources: &GateResources,
    state: &mut GateState,
) -> anyhow::Result<CausationE2eArgs> {
    let project_admin = connect(&args.admin_database_url).await?;
    project_admin
        .batch_execute(&format!("CREATE DATABASE {}", resources.project_database))
        .await
        .context("create disposable project database")?;
    state.ledger.project_database = true;
    project_admin
        .batch_execute(&format!(
            "COMMENT ON DATABASE {} IS '{}'",
            resources.project_database, resources.owner
        ))
        .await
        .context("mark disposable project database owner")?;
    let system_admin = connect(&args.system_database_url).await?;
    system_admin
        .batch_execute(&format!("CREATE DATABASE {}", resources.system_database))
        .await
        .context("create disposable system database")?;
    state.ledger.system_database = true;
    system_admin
        .batch_execute(&format!(
            "COMMENT ON DATABASE {} IS '{}'",
            resources.system_database, resources.owner
        ))
        .await
        .context("mark disposable system database owner")?;
    disposable_args(args, resources)
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

fn require_loopback_url(url: &str, label: &str) -> anyhow::Result<()> {
    let authority = url
        .strip_prefix("postgres://")
        .and_then(|value| value.split_once('@').map(|(_, tail)| tail))
        .context("PostgreSQL URL must carry postgres:// userinfo")?;
    ensure!(
        authority.starts_with("127.0.0.1:5432/"),
        "{label} must use the isolated loopback PostgreSQL sidecar"
    );
    ensure!(
        !url.contains(".svc") && !url.contains("wamn-pg") && !url.contains("wamn-sysdb"),
        "{label} must not resolve a shared PostgreSQL endpoint"
    );
    Ok(())
}

fn expected_sidecar_hba(resources: &GateResources) -> serde_json::Value {
    serde_json::json!([
        {
            "type": "host", "database": [resources.project_database],
            "user": [resources.cdc_name], "address": "127.0.0.1",
            "netmask": "255.255.255.255", "auth": "scram-sha-256", "error": null
        },
        {
            "type": "host", "database": ["all"], "user": [resources.cdc_name],
            "address": "127.0.0.1", "netmask": "255.255.255.255",
            "auth": "reject", "error": null
        },
        {
            "type": "local", "database": ["all"], "user": ["postgres"],
            "address": null, "netmask": null, "auth": "trust", "error": null
        },
        {
            "type": "host", "database": ["all"], "user": ["postgres"],
            "address": "127.0.0.1", "netmask": "255.255.255.255",
            "auth": "trust", "error": null
        },
        {
            "type": "host", "database": ["all"], "user": ["wamn_app"],
            "address": "127.0.0.1", "netmask": "255.255.255.255",
            "auth": "trust", "error": null
        },
        {
            "type": "host", "database": ["all"], "user": ["all"],
            "address": "127.0.0.1", "netmask": "255.255.255.255",
            "auth": "reject", "error": null
        },
        {
            "type": "host", "database": ["all"], "user": ["all"],
            "address": "0.0.0.0", "netmask": "0.0.0.0",
            "auth": "reject", "error": null
        },
        {
            "type": "host", "database": ["all"], "user": ["all"],
            "address": "::", "netmask": "::", "auth": "reject", "error": null
        }
    ])
}

fn require_exact_sidecar_hba(
    observed: &serde_json::Value,
    resources: &GateResources,
) -> anyhow::Result<()> {
    ensure!(
        observed == &expected_sidecar_hba(resources),
        "isolated HBA must be exactly the normalized UID-derived CDC allow/reject pair followed by the fixed loopback administration rules"
    );
    Ok(())
}

async fn preflight_isolated_postgres(
    args: &CausationE2eArgs,
    resources: &GateResources,
) -> anyhow::Result<Client> {
    require_loopback_url(&args.database_url, "application URL")?;
    require_loopback_url(&args.admin_database_url, "project admin URL")?;
    require_loopback_url(&args.system_database_url, "system admin URL")?;
    ensure!(
        args.admin_database_url == args.system_database_url,
        "M1 project and system scratch databases must share one isolated sidecar"
    );
    let admin = connect(&args.admin_database_url).await?;
    let version: String = admin
        .query_one("SHOW server_version_num", &[])
        .await?
        .get(0);
    let version = version
        .parse::<i64>()
        .context("parse isolated PostgreSQL server_version_num")?;
    ensure!(
        version / 10_000 == SIDECAR_POSTGRES_MAJOR,
        "M1 requires native PostgreSQL {SIDECAR_POSTGRES_MAJOR}, observed {version}"
    );
    let listen: String = admin.query_one("SHOW listen_addresses", &[]).await?.get(0);
    ensure!(
        listen == "127.0.0.1",
        "isolated PostgreSQL must listen only on IPv4 loopback"
    );
    let wal_level: String = admin.query_one("SHOW wal_level", &[]).await?.get(0);
    ensure!(
        wal_level == "logical",
        "isolated PostgreSQL must enable logical WAL"
    );
    let hba: String = admin
        .query_one(
            "SELECT COALESCE(jsonb_agg(jsonb_build_object( \
               'type', type, 'database', database, 'user', user_name, \
               'address', address, 'netmask', netmask, 'auth', auth_method, \
               'error', error) ORDER BY rule_number), '[]'::jsonb)::text \
             FROM pg_hba_file_rules",
            &[],
        )
        .await?
        .get(0);
    let hba: serde_json::Value =
        serde_json::from_str(&hba).context("parse normalized sidecar HBA")?;
    require_exact_sidecar_hba(&hba, resources)?;
    Ok(admin)
}

async fn setup_isolated_roles(admin: &mut Client, resources: &GateResources) -> anyhow::Result<()> {
    let transaction = admin.transaction().await?;
    transaction
        .batch_execute(&format!(
            "DO $m1$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
                 CREATE ROLE wamn_system LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_run_projection_writer') THEN \
                 CREATE ROLE wamn_run_projection_writer NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $m1$; \
             COMMENT ON ROLE wamn_system IS '{owner}'; \
             COMMENT ON ROLE wamn_app IS '{owner}'; \
             COMMENT ON ROLE wamn_scenario_author IS '{owner}'; \
             COMMENT ON ROLE wamn_effect_writer IS '{owner}'; \
             COMMENT ON ROLE wamn_run_projection_writer IS '{owner}';",
            owner = resources.owner,
        ))
        .await
        .context("create canonical roles inside isolated PostgreSQL")?;
    transaction.commit().await?;
    require_role(admin, "wamn_system", true, true).await?;
    require_role(admin, "wamn_app", true, true).await?;
    require_role(admin, "wamn_scenario_author", false, false).await?;
    require_role(admin, "wamn_effect_writer", false, false).await?;
    require_role(admin, "wamn_run_projection_writer", false, false).await?;
    Ok(())
}

async fn require_role(
    client: &Client,
    role: &str,
    expected_login: bool,
    expected_inherit: bool,
) -> anyhow::Result<()> {
    let row = client
        .query_opt(
            "SELECT rolcanlogin, rolinherit, rolsuper, rolcreatedb, rolcreaterole, \
                    rolreplication, rolbypassrls \
             FROM pg_roles WHERE rolname=$1",
            &[&role],
        )
        .await?
        .with_context(|| format!("required pre-existing PostgreSQL role {role} is absent"))?;
    ensure!(
        row.get::<_, bool>(0) == expected_login
            && row.get::<_, bool>(1) == expected_inherit
            && !row.get::<_, bool>(2)
            && !row.get::<_, bool>(3)
            && !row.get::<_, bool>(4)
            && !row.get::<_, bool>(5)
            && !row.get::<_, bool>(6),
        "pre-existing PostgreSQL role {role} has privilege drift"
    );
    Ok(())
}

async fn setup_registry(system: &mut Client, resources: &GateResources) -> anyhow::Result<()> {
    // The canonical owner lives only inside this Job's emptyDir-backed server.
    require_role(system, "wamn_system", true, true).await?;
    system.batch_execute(SYSTEM_SQL).await?;
    let transaction = system.transaction().await?;
    transaction
        .execute(
            registry_sql::upsert_org_sql(),
            &[&resources.org, &"dedicated", &Option::<&str>::None],
        )
        .await?;
    transaction
        .execute(
            registry_sql::stamp_env_policy_sql(),
            &[
                &resources.org,
                &resources.env,
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
        .execute(
            registry_sql::upsert_project_sql(),
            &[&resources.org, &resources.project],
        )
        .await?;
    transaction
        .execute(
            registry_sql::upsert_project_env_sql(),
            &[
                &resources.org,
                &resources.project,
                &resources.env,
                &resources.project_database,
                &Option::<&str>::None,
            ],
        )
        .await?;
    transaction
        .execute(
            registry_sql::upsert_event_reader_sql(),
            &[
                &resources.org,
                &resources.project,
                &resources.env,
                &resources.cdc_name,
                &resources.cdc_name,
                &resources.stream,
                &resources.cdc_name,
                &Option::<&str>::None,
                &true,
            ],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn setup_project(
    args: &CausationE2eArgs,
    resources: &GateResources,
    state: &mut GateState,
) -> anyhow::Result<()> {
    let mut admin = connect(&args.admin_database_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()", &[])
        .await?
        .get(0);
    require_role(&admin, "wamn_app", true, true).await?;
    require_role(&admin, "wamn_scenario_author", false, false).await?;
    require_role(&admin, "wamn_effect_writer", false, false).await?;
    require_role(&admin, "wamn_run_projection_writer", false, false).await?;
    admin.batch_execute(CATALOG_SQL).await?;
    admin.batch_execute(RUN_STATE_SQL).await?;
    admin.batch_execute(RUN_QUEUE_SQL).await?;
    admin
        .batch_execute(&provision_sql::ensure_schema_sql(&resources.schema))
        .await?;
    state.ledger.schema = true;
    admin
        .batch_execute(&format!(
            "COMMENT ON SCHEMA {} IS '{}';",
            resources.schema, resources.owner
        ))
        .await?;
    admin
        .batch_execute(&format!(
            "GRANT USAGE ON SCHEMA {schema} TO wamn_app; \
             CREATE TABLE {schema}.{table} (tenant_id text NOT NULL, id text NOT NULL, \
               payload text NOT NULL, PRIMARY KEY (tenant_id,id)); \
             ALTER TABLE {schema}.{table} ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {schema}.{table} FORCE ROW LEVEL SECURITY; \
             CREATE POLICY receipts_tenant ON {schema}.{table} \
               USING (tenant_id=NULLIF(current_setting('app.tenant',true),'')) \
               WITH CHECK (tenant_id=NULLIF(current_setting('app.tenant',true),'')); \
             GRANT SELECT,INSERT,UPDATE,DELETE ON {schema}.{table} TO wamn_app; \
             CREATE TABLE {schema}.wamn_entities (relation_oid oid PRIMARY KEY, \
               entity_id text NOT NULL, table_name text NOT NULL); \
             INSERT INTO {schema}.wamn_entities VALUES \
               ('{schema}.{table}'::regclass::oid,'{entity_id}','{table}');",
            schema = resources.schema,
            table = resources.table,
            entity_id = resources.entity_id,
        ))
        .await?;
    let role_transaction = admin.transaction().await?;
    role_transaction
        .batch_execute(&format!(
            "CREATE ROLE {} NOLOGIN REPLICATION PASSWORD '{}' \
             NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS; \
             ALTER ROLE {} CONNECTION LIMIT 1; COMMENT ON ROLE {} IS '{}';",
            resources.cdc_name,
            resources.cdc_password,
            resources.cdc_name,
            resources.cdc_name,
            resources.owner
        ))
        .await?;
    role_transaction.commit().await?;
    state.ledger.cdc_role = true;
    admin
        .batch_execute(&provision_sql::create_publication_sql(
            &resources.cdc_name,
            &resources.schema,
        ))
        .await?;
    state.ledger.publication = true;
    admin
        .batch_execute(&format!(
            "COMMENT ON PUBLICATION {} IS '{}';",
            resources.cdc_name, resources.owner
        ))
        .await?;
    admin
        .batch_execute(&provision_sql::grant_replication_access_sql(
            &database,
            &resources.cdc_name,
            &resources.schema,
        ))
        .await?;

    let graph = flow_json(resources);
    let registration = registration_json(resources);
    let (execution_plan, bundle_hash) = execution_plan_fixture()?;
    let members = serde_json::json!([{
        "flow-id": resources.flow_id, "flow-version": 1, "artifact-hash": ARTIFACT_HASH
    }]);
    let transaction = admin.transaction().await?;
    transaction
        .execute(
            "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version,state) \
         VALUES ($1,$2,1,$3,'0.1','applied')",
            &[&resources.tenant, &resources.catalog_id, &resources.env],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.flow_artifacts \
           (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash) \
         VALUES ($1,$2,1,'0.1',$3::text::jsonb,'wave8-11-9-graph',$4)",
            &[
                &resources.tenant,
                &resources.flow_id,
                &graph,
                &ARTIFACT_HASH,
            ],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.release_manifests \
           (tenant_id,catalog_id,catalog_version,members_json) VALUES ($1,$2,1,$3)",
            &[&resources.tenant, &resources.catalog_id, &members],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.execution_bundles \
           (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
         VALUES ($1,$2,'0.1',$3,$4)",
            &[
                &resources.tenant,
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
            &[
                &resources.tenant,
                &resources.catalog_id,
                &resources.flow_id,
                &bundle_hash,
            ],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.catalog_heads \
           (tenant_id,catalog_id,environment,applied_catalog_version) VALUES ($1,$2,$3,1)",
            &[&resources.tenant, &resources.catalog_id, &resources.env],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.event_registrations \
           (tenant_id,catalog_id,registration_id,flow_id,entity_id,registration) \
         VALUES ($1,$2,$3,$4,$5,$6::text::jsonb)",
            &[
                &resources.tenant,
                &resources.catalog_id,
                &resources.registration_id,
                &resources.flow_id,
                &resources.entity_id,
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
        &[
            &resources.tenant,
            &resources.root_run_id,
            &resources.flow_id,
            &resources.catalog_id,
            &resources.env,
            &bundle_hash,
            &resources.source_run_id,
        ],
    ).await?;
    transaction.commit().await?;

    // Capture starts after all fixture writes; only the matching tenant commit is observable.
    admin
        .batch_execute(&provision_sql::create_failover_slot_sql(
            &resources.cdc_name,
        ))
        .await?;
    state.ledger.slot = true;
    Ok(())
}

fn reader_args(args: &CausationE2eArgs, resources: &GateResources) -> anyhow::Result<ReaderArgs> {
    Ok(ReaderArgs {
        org: resources.org.clone(),
        project: resources.project.clone(),
        env: resources.env.clone(),
        system_database_url: args.system_database_url.clone(),
        cdc_url: role_url(&args.admin_database_url, resources)?,
        nats_url: args.nats_url.clone(),
        stream_replicas: 1,
    })
}

async fn setup_stream(
    args: &CausationE2eArgs,
    resources: &GateResources,
    state: &mut GateState,
) -> anyhow::Result<()> {
    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    js.create_stream(async_nats::jetstream::stream::Config {
        name: resources.stream.clone(),
        description: Some(resources.owner.clone()),
        subjects: vec![wamn_event_wire::stream_subjects(
            &resources.org,
            &resources.env,
        )],
        storage: async_nats::jetstream::stream::StorageType::File,
        num_replicas: 1,
        retention: async_nats::jetstream::stream::RetentionPolicy::Limits,
        duplicate_window: Duration::from_secs(BROKER_DUP_WINDOW_SECS),
        ..Default::default()
    })
    .await
    .context("create run-owned event stream")?;
    state.ledger.stream = true;
    Ok(())
}

async fn wait_for_stream(args: &CausationE2eArgs, resources: &GateResources) -> anyhow::Result<()> {
    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let deadline = Instant::now() + Duration::from_secs(10);
    while js.get_stream(&resources.stream).await.is_err() {
        ensure!(
            Instant::now() < deadline,
            "reader did not create {}",
            resources.stream
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

fn tenant_commit_sql(resources: &GateResources) -> String {
    format!(
        "SELECT pg_logical_emit_message(true, 'wamn.causation', \
           '{{\"run\":\"{source}\",\"root\":\"{root}\",\"depth\":1}}');",
        source = resources.source_run_id,
        root = resources.root_run_id,
    )
}

async fn commit_tenant_event(
    args: &CausationE2eArgs,
    resources: &GateResources,
) -> anyhow::Result<()> {
    let mut app = connect(&args.database_url).await?;
    app.batch_execute(&format!(
        "SET search_path TO {}; SET app.tenant TO '{}';",
        resources.schema, resources.tenant
    ))
    .await?;
    let transaction = app.transaction().await?;
    transaction
        .batch_execute(&tenant_commit_sql(resources))
        .await?;
    transaction
        .execute(
            &format!(
                "INSERT INTO {}.{} (tenant_id,id,payload) VALUES ($1,$2,$3)",
                resources.schema, resources.table
            ),
            &[&resources.tenant, &"forward-1", &"committed"],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn wait_for_stored_event(
    args: &CausationE2eArgs,
    resources: &GateResources,
) -> anyhow::Result<StoredEvent> {
    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    loop {
        if let Ok(mut stream) = js.get_stream(&resources.stream).await {
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
            "CDC event did not reach {}",
            resources.stream
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn stream_message_count(
    args: &CausationE2eArgs,
    resources: &GateResources,
) -> anyhow::Result<u64> {
    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let mut stream = js.get_stream(&resources.stream).await?;
    Ok(stream.info().await?.state.messages)
}

async fn build_materializer(
    args: &CausationE2eArgs,
    resources: &GateResources,
) -> anyhow::Result<MaterializerHarness> {
    wash_runtime::init_crypto();
    let guest = std::fs::read(&args.component)
        .with_context(|| format!("read {}", args.component.display()))?;
    let mut pg_config = WamnPostgresConfig::from_env();
    pg_config.database_url = Some(args.database_url.clone());
    let pg = Arc::new(WamnPostgres::new(pg_config)?);
    pg.set_tenant(&resources.gate_id, &resources.tenant)?;
    pg.set_schema(&resources.gate_id, "wamn_run")?;
    pg.probe_checkout().await?;
    let jetstream = Arc::new(
        WamnJetstream::new(WamnJetstreamConfig {
            nats_url: Some(args.nats_url.clone()),
        })
        .with_doorbell(async_nats::connect(&args.nats_url).await?),
    );
    jetstream.set_execution_target(
        &resources.gate_id,
        mvp_execution_target_id(&resources.tenant)?,
    );
    let engine = build_engine(&[])?;
    let raw: &RawEngine = engine.inner();
    let component = WasmtimeComponent::new(raw, &guest)
        .map_err(|error| anyhow::anyhow!("compile materializer: {error}"))?;
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wamn_postgres::add_to_linker(&mut linker)?;
    wamn_jetstream::add_to_linker(&mut linker)?;
    let pre = CommandPre::new(linker.instantiate_pre(&component)?)?;
    let report_dir = resources.report_dir.clone();
    let report_owner = std::fs::read_to_string(report_dir.join(".wamn-m1-owner"))
        .context("read materializer report owner record")?;
    ensure!(
        report_owner == resources.owner,
        "materializer report directory lacks the exact durable owner record"
    );
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    std::mem::forget(ticker);
    Ok(MaterializerHarness {
        engine,
        pre,
        pg,
        js: jetstream,
        report_dir,
        resources: resources.clone(),
    })
}

async fn observe_durable(
    args: &CausationE2eArgs,
    resources: &GateResources,
    state: &mut GateState,
) -> anyhow::Result<()> {
    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let stream = js.get_stream(&resources.stream).await?;
    stream
        .consumer_info(&resources.durable)
        .await
        .context("observe run-owned materializer durable")?;
    state.ledger.durable = true;
    Ok(())
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
    resources: &GateResources,
    sequence: u64,
    expected_source_event_id: &str,
) -> anyhow::Result<()> {
    let expected_run_id = mint_evt_run_id(
        &format!("{}:{}", resources.flow_id, resources.registration_id),
        sequence,
    );
    let registration: serde_json::Value = serde_json::from_str(&registration_json(resources))?;
    let (_, expected_hash) = registration_evidence(&registration);
    let run_count: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.runs \
             WHERE tenant_id=$1 AND registration_id=$2",
            &[&resources.tenant, &resources.registration_id],
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
            &[&resources.tenant, &resources.registration_id],
        )
        .await?;
    ensure!(
        row.get::<_, String>(0) == expected_run_id,
        "event run id drifted"
    );
    ensure!(
        row.get::<_, String>(1) == resources.tenant,
        "tenant identity drifted"
    );
    ensure!(
        row.get::<_, String>(2) == resources.registration_id,
        "registration identity drifted"
    );
    ensure!(
        row.get::<_, String>(3) == format!("evt:{}:{sequence}", resources.registration_id),
        "producer coordinate drifted"
    );
    ensure!(
        row.get::<_, String>(4) == expected_hash,
        "registration evidence hash drifted"
    );
    ensure!(
        row.get::<_, String>(5) == resources.entity_id,
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
        row.get::<_, String>(8) == resources.source_run_id,
        "source run lineage drifted"
    );
    ensure!(
        row.get::<_, String>(9) == resources.root_run_id,
        "root lineage drifted"
    );
    ensure!(row.get::<_, i32>(10) == 2, "depth lineage drifted");
    let queue_count: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.run_queue q \
             WHERE q.tenant_id=$1 AND q.run_id IN ( \
               SELECT r.run_id FROM wamn_run.runs r \
               WHERE r.tenant_id=$1 AND r.registration_id=$2)",
            &[&resources.tenant, &resources.registration_id],
        )
        .await?
        .get(0);
    ensure!(queue_count == 1, "expected exactly one causal queue row");
    let queue_sequence: i64 = admin
        .query_one(
            "SELECT stream_seq FROM wamn_run.run_queue WHERE tenant_id=$1 AND run_id=$2",
            &[&resources.tenant, &expected_run_id],
        )
        .await?
        .get(0);
    ensure!(queue_sequence == sequence as i64, "queue sequence drifted");
    Ok(())
}

async fn postgres_owner(
    client: &Client,
    catalog: &str,
    class: &str,
    name: &str,
) -> anyhow::Result<Option<Option<String>>> {
    let sql = format!(
        "SELECT shobj_description(oid, '{class}') FROM {catalog} WHERE {}=$1",
        if class == "pg_database" {
            "datname"
        } else {
            "rolname"
        }
    );
    Ok(client
        .query_opt(&sql, &[&name])
        .await?
        .map(|row| row.get::<_, Option<String>>(0)))
}

fn require_owner(
    kind: &str,
    name: &str,
    observed: Option<&str>,
    resources: &GateResources,
    created_in_process: bool,
) -> anyhow::Result<()> {
    ensure!(
        observed == Some(resources.owner.as_str()) || (observed.is_none() && created_in_process),
        "foreign or unowned {kind} {name} refuses exact M1 cleanup"
    );
    Ok(())
}

fn durable_setup_owned(resources: &GateResources) -> anyhow::Result<bool> {
    if !resources.report_dir.exists() {
        return Ok(false);
    }
    let marker = std::fs::read_to_string(resources.report_dir.join(".wamn-m1-owner"))
        .context("read exact durable owner marker")?;
    ensure!(
        marker == resources.owner,
        "foreign report directory refuses exact M1 cleanup"
    );
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(resources.report_dir.join("resource-record.json"))
            .context("read exact durable setup record")?,
    )
    .context("parse exact durable setup record")?;
    ensure!(
        record == resources.resource_record(),
        "mismatched durable setup record refuses exact M1 cleanup"
    );
    Ok(true)
}

async fn cleanup(
    args: &CausationE2eArgs,
    resources: &GateResources,
    state: &mut GateState,
    phase: &str,
) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    let durable_owner = match durable_setup_owned(resources) {
        Ok(owned) => owned,
        Err(error) => {
            errors.push(error.to_string());
            false
        }
    };
    if let Some(reader) = state.reader.take() {
        match reader.shutdown(Duration::from_secs(15)).await {
            Ok(true) => println!("M1_CLEANUP phase={phase} resource=cdc-reader verdict=absent"),
            Ok(false) => errors.push("CDC reader did not shut down successfully".to_string()),
            Err(error) => errors.push(format!("shut down CDC reader: {error:#}")),
        }
    }

    // PostgreSQL REPLICATION is inherently cluster-wide. Keep the run-owned
    // role disabled outside the shortest reader window, detect any off-scratch
    // use, then terminate every session belonging to this exact role.
    match connect(&args.admin_database_url).await {
        Ok(project_admin) => {
            match postgres_owner(&project_admin, "pg_roles", "pg_authid", &resources.cdc_name).await
            {
                Ok(None) => {}
                Ok(Some(owner)) => match require_owner(
                    "role",
                    &resources.cdc_name,
                    owner.as_deref(),
                    resources,
                    state.ledger.cdc_role,
                ) {
                    Ok(()) => {
                        if let Err(error) = project_admin
                            .batch_execute(&format!("ALTER ROLE {} NOLOGIN", resources.cdc_name))
                            .await
                        {
                            errors.push(format!("disable CDC role before cleanup: {error:#}"));
                        }
                        match project_admin
                            .query_one(
                                "SELECT count(*) FILTER (WHERE datname IS DISTINCT FROM $2), count(*) \
                                 FROM pg_stat_activity WHERE usename=$1 AND pid <> pg_backend_pid()",
                                &[&resources.cdc_name, &resources.project_database],
                            )
                            .await
                        {
                            Ok(row) => {
                                let off_scratch: i64 = row.get(0);
                                let total: i64 = row.get(1);
                                if off_scratch != 0 {
                                    errors.push(format!(
                                        "exact CDC role had {off_scratch} off-scratch session(s)"
                                    ));
                                }
                                if total < off_scratch {
                                    errors.push("exact CDC session counts were inconsistent".into());
                                }
                            }
                            Err(error) => {
                                errors.push(format!("inspect exact CDC sessions: {error:#}"));
                            }
                        }
                        match project_admin
                            .query(
                                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                             WHERE usename=$1 AND pid <> pg_backend_pid()",
                                &[&resources.cdc_name],
                            )
                            .await
                        {
                            Ok(rows) => {
                                if rows.iter().any(|row| !row.get::<_, bool>(0)) {
                                    errors.push(
                                        "one or more exact CDC sessions refused termination".into(),
                                    );
                                }
                            }
                            Err(error) => errors
                                .push(format!("terminate all exact CDC role sessions: {error:#}")),
                        }
                        match project_admin
                            .query_one(
                                "SELECT count(*) FROM pg_stat_activity \
                                 WHERE usename=$1 AND pid <> pg_backend_pid()",
                                &[&resources.cdc_name],
                            )
                            .await
                        {
                            Ok(row) if row.get::<_, i64>(0) == 0 => {}
                            Ok(row) => errors.push(format!(
                                "{} exact CDC role session(s) remained after termination",
                                row.get::<_, i64>(0)
                            )),
                            Err(error) => errors.push(format!(
                                "verify zero exact CDC sessions after termination: {error:#}"
                            )),
                        }
                    }
                    Err(error) => errors.push(error.to_string()),
                },
                Err(error) => errors.push(format!("inspect CDC role before cleanup: {error:#}")),
            }
        }
        Err(error) => errors.push(format!("connect project admin before cleanup: {error:#}")),
    }

    match async_nats::connect(&args.nats_url).await {
        Ok(nats) => {
            let js = async_nats::jetstream::new(nats);
            match js.get_stream(&resources.stream).await {
                Ok(stream) => {
                    let description = stream.cached_info().config.description.as_deref();
                    match require_owner(
                        "stream",
                        &resources.stream,
                        description,
                        resources,
                        state.ledger.stream,
                    ) {
                        Ok(()) => {
                            match stream.consumer_info(&resources.durable).await {
                                Ok(_) => {
                                    if let Err(error) = stream.delete_consumer(&resources.durable).await {
                                        errors.push(format!(
                                            "delete durable {}: {error:#}",
                                            resources.durable
                                        ));
                                    }
                                }
                                Err(error)
                                    if matches!(
                                        error.kind(),
                                        async_nats::jetstream::context::ConsumerInfoErrorKind::NotFound
                                    ) => {}
                                Err(error) => errors.push(format!(
                                    "inspect durable {}: {error:#}",
                                    resources.durable
                                )),
                            }
                            if let Err(error) = js.delete_stream(&resources.stream).await {
                                errors
                                    .push(format!("delete stream {}: {error:#}", resources.stream));
                            }
                        }
                        Err(error) => errors.push(error.to_string()),
                    }
                }
                Err(error) if stream_not_found(&error) => {}
                Err(error) => {
                    errors.push(format!("inspect stream {}: {error:#}", resources.stream))
                }
            }
        }
        Err(error) => errors.push(format!("connect NATS for cleanup: {error:#}")),
    }

    match connect(&args.admin_database_url).await {
        Ok(project_admin) => {
            let database = postgres_owner(
                &project_admin,
                "pg_database",
                "pg_database",
                &resources.project_database,
            )
            .await;
            let role =
                postgres_owner(&project_admin, "pg_roles", "pg_authid", &resources.cdc_name).await;
            let database_owned = match database {
                Ok(None) => false,
                Ok(Some(owner)) => match require_owner(
                    "database",
                    &resources.project_database,
                    owner.as_deref(),
                    resources,
                    state.ledger.project_database || durable_owner,
                ) {
                    Ok(()) => true,
                    Err(error) => {
                        errors.push(error.to_string());
                        false
                    }
                },
                Err(error) => {
                    errors.push(format!("inspect project database ownership: {error:#}"));
                    false
                }
            };
            let role_owned = match role {
                Ok(None) => false,
                Ok(Some(owner)) => match require_owner(
                    "role",
                    &resources.cdc_name,
                    owner.as_deref(),
                    resources,
                    state.ledger.cdc_role,
                ) {
                    Ok(()) => true,
                    Err(error) => {
                        errors.push(error.to_string());
                        false
                    }
                },
                Err(error) => {
                    errors.push(format!("inspect CDC role ownership: {error:#}"));
                    false
                }
            };
            if role_owned
                && let Err(error) = project_admin
                    .batch_execute(&format!("ALTER ROLE {} NOLOGIN", resources.cdc_name))
                    .await
            {
                errors.push(format!("disable CDC role: {error:#}"));
            }
            if database_owned {
                let scratch_url =
                    swap_database(&args.admin_database_url, &resources.project_database);
                match scratch_url {
                    Ok(scratch_url) => match connect(&scratch_url).await {
                        Ok(scratch) => {
                            let slot_database: Option<String> = scratch
                                .query_opt(
                                    "SELECT database FROM pg_replication_slots WHERE slot_name=$1",
                                    &[&resources.cdc_name],
                                )
                                .await
                                .map(|row| row.map(|row| row.get(0)))
                                .unwrap_or_else(|error| {
                                    errors.push(format!("inspect exact CDC slot: {error:#}"));
                                    None
                                });
                            if let Some(slot_database) = slot_database {
                                if slot_database == resources.project_database {
                                    if let Err(error) = scratch
                                        .execute(
                                            "SELECT pg_drop_replication_slot($1)",
                                            &[&resources.cdc_name],
                                        )
                                        .await
                                    {
                                        errors.push(format!("drop exact CDC slot: {error:#}"));
                                    }
                                } else {
                                    errors.push(format!(
                                        "exact slot {} belongs to foreign database {slot_database}",
                                        resources.cdc_name
                                    ));
                                }
                            }
                            if let Err(error) = scratch
                                .batch_execute(&format!(
                                    "DROP PUBLICATION IF EXISTS {}",
                                    resources.cdc_name
                                ))
                                .await
                            {
                                errors.push(format!("drop exact CDC publication: {error:#}"));
                            }
                        }
                        Err(error) => {
                            errors.push(format!("connect exact project database: {error:#}"))
                        }
                    },
                    Err(error) => {
                        errors.push(format!("derive exact project database URL: {error:#}"))
                    }
                };
                if let Err(error) = project_admin
                    .batch_execute(&format!(
                        "DROP DATABASE {} WITH (FORCE)",
                        resources.project_database
                    ))
                    .await
                {
                    errors.push(format!("drop exact project database: {error:#}"));
                }
            }
            if role_owned
                && let Err(error) = project_admin
                    .batch_execute(&format!("DROP ROLE {}", resources.cdc_name))
                    .await
            {
                errors.push(format!("drop exact CDC role: {error:#}"));
            }
        }
        Err(error) => errors.push(format!("connect project admin for cleanup: {error:#}")),
    }

    match connect(&args.system_database_url).await {
        Ok(system_admin) => match postgres_owner(
            &system_admin,
            "pg_database",
            "pg_database",
            &resources.system_database,
        )
        .await
        {
            Ok(None) => {}
            Ok(Some(owner)) => match require_owner(
                "database",
                &resources.system_database,
                owner.as_deref(),
                resources,
                state.ledger.system_database || durable_owner,
            ) {
                Ok(()) => {
                    if let Err(error) = system_admin
                        .batch_execute(&format!(
                            "DROP DATABASE {} WITH (FORCE)",
                            resources.system_database
                        ))
                        .await
                    {
                        errors.push(format!("drop exact system database: {error:#}"));
                    }
                }
                Err(error) => errors.push(error.to_string()),
            },
            Err(error) => errors.push(format!("inspect system database ownership: {error:#}")),
        },
        Err(error) => errors.push(format!("connect system admin for cleanup: {error:#}")),
    }

    // The durable record is cleanup authority for a database left between
    // CREATE DATABASE and COMMENT. Retain it until every other exact mutation
    // has succeeded so a retry can never be stranded without authorization.
    if errors.is_empty()
        && resources.report_dir.exists()
        && (durable_owner || state.ledger.report_dir)
        && let Err(error) = std::fs::remove_dir_all(&resources.report_dir)
    {
        errors.push(format!(
            "remove {}: {error}",
            resources.report_dir.display()
        ));
    }

    if let Err(error) = verify_clean(args, resources, phase).await {
        errors.push(format!("independent exact cleanup verification: {error:#}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(errors.join("; ")))
    }
}

fn record_absence(errors: &mut Vec<String>, phase: &str, kind: &str, name: &str, absent: bool) {
    println!(
        "M1_CLEANUP phase={phase} resource={kind} name={name} verdict={}",
        if absent { "absent" } else { "leaked" }
    );
    if !absent {
        errors.push(format!("{kind} {name} remains after cleanup"));
    }
}

fn record_unknown(errors: &mut Vec<String>, phase: &str, kind: &str, name: &str, reason: &str) {
    println!(
        "M1_CLEANUP phase={phase} resource={kind} name={name} verdict=unknown reason={reason}"
    );
    errors.push(format!("could not verify {kind} {name}: {reason}"));
}

async fn verify_clean(
    args: &CausationE2eArgs,
    resources: &GateResources,
    phase: &str,
) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    match async_nats::connect(&args.nats_url).await {
        Ok(nats) => {
            let js = async_nats::jetstream::new(nats);
            match js.get_stream(&resources.stream).await {
                Ok(_) => {
                    record_absence(&mut errors, phase, "stream", &resources.stream, false);
                    record_absence(&mut errors, phase, "durable", &resources.durable, false);
                }
                Err(error) if stream_not_found(&error) => {
                    record_absence(&mut errors, phase, "stream", &resources.stream, true);
                    record_absence(&mut errors, phase, "durable", &resources.durable, true);
                }
                Err(error) => {
                    let reason = format!("inspect stream: {error:#}");
                    record_unknown(&mut errors, phase, "stream", &resources.stream, &reason);
                    record_unknown(&mut errors, phase, "durable", &resources.durable, &reason);
                }
            }
        }
        Err(error) => {
            let reason = format!("connect NATS: {error:#}");
            record_unknown(&mut errors, phase, "stream", &resources.stream, &reason);
            record_unknown(&mut errors, phase, "durable", &resources.durable, &reason);
        }
    }
    record_absence(
        &mut errors,
        phase,
        "report-dir",
        &resources.report_dir.display().to_string(),
        !resources.report_dir.exists(),
    );

    match connect(&args.admin_database_url).await {
        Ok(project_admin) => {
            let database_absent = project_admin
                .query_one(
                    "SELECT NOT EXISTS (SELECT FROM pg_database WHERE datname=$1)",
                    &[&resources.project_database],
                )
                .await
                .map(|row| row.get::<_, bool>(0));
            match &database_absent {
                Ok(absent) => record_absence(
                    &mut errors,
                    phase,
                    "project-database",
                    &resources.project_database,
                    *absent,
                ),
                Err(error) => record_unknown(
                    &mut errors,
                    phase,
                    "project-database",
                    &resources.project_database,
                    &error.to_string(),
                ),
            }

            let role = project_admin
                .query_opt(
                    "SELECT rolcanlogin FROM pg_roles WHERE rolname=$1",
                    &[&resources.cdc_name],
                )
                .await;
            match role {
                Ok(None) => {
                    record_absence(&mut errors, phase, "cdc-role", &resources.cdc_name, true);
                    record_absence(
                        &mut errors,
                        phase,
                        "cdc-role-login",
                        &resources.cdc_name,
                        true,
                    );
                }
                Ok(Some(row)) => {
                    record_absence(&mut errors, phase, "cdc-role", &resources.cdc_name, false);
                    record_absence(
                        &mut errors,
                        phase,
                        "cdc-role-login",
                        &resources.cdc_name,
                        !row.get::<_, bool>(0),
                    );
                }
                Err(error) => {
                    for kind in ["cdc-role", "cdc-role-login"] {
                        record_unknown(
                            &mut errors,
                            phase,
                            kind,
                            &resources.cdc_name,
                            &error.to_string(),
                        );
                    }
                }
            }
            match project_admin
                .query_one(
                    "SELECT count(*) FROM pg_stat_activity WHERE usename=$1",
                    &[&resources.cdc_name],
                )
                .await
            {
                Ok(row) => record_absence(
                    &mut errors,
                    phase,
                    "cdc-role-sessions",
                    &resources.cdc_name,
                    row.get::<_, i64>(0) == 0,
                ),
                Err(error) => record_unknown(
                    &mut errors,
                    phase,
                    "cdc-role-sessions",
                    &resources.cdc_name,
                    &error.to_string(),
                ),
            }
            for (kind, name) in [
                ("schema", resources.schema.as_str()),
                ("table", resources.table.as_str()),
                ("publication", resources.cdc_name.as_str()),
                ("slot", resources.cdc_name.as_str()),
                ("tenant", resources.tenant.as_str()),
                ("catalog", resources.catalog_id.as_str()),
                ("flow", resources.flow_id.as_str()),
                ("registration", resources.registration_id.as_str()),
                ("entity", resources.entity_id.as_str()),
                ("root-run", resources.root_run_id.as_str()),
                ("source-run", resources.source_run_id.as_str()),
            ] {
                match &database_absent {
                    Ok(absent) => record_absence(&mut errors, phase, kind, name, *absent),
                    Err(error) => {
                        record_unknown(&mut errors, phase, kind, name, &error.to_string())
                    }
                }
            }
        }
        Err(error) => {
            let reason = format!("connect project admin: {error:#}");
            for (kind, name) in [
                ("project-database", resources.project_database.as_str()),
                ("cdc-role", resources.cdc_name.as_str()),
                ("cdc-role-login", resources.cdc_name.as_str()),
                ("cdc-role-sessions", resources.cdc_name.as_str()),
                ("schema", resources.schema.as_str()),
                ("table", resources.table.as_str()),
                ("publication", resources.cdc_name.as_str()),
                ("slot", resources.cdc_name.as_str()),
                ("tenant", resources.tenant.as_str()),
                ("catalog", resources.catalog_id.as_str()),
                ("flow", resources.flow_id.as_str()),
                ("registration", resources.registration_id.as_str()),
                ("entity", resources.entity_id.as_str()),
                ("root-run", resources.root_run_id.as_str()),
                ("source-run", resources.source_run_id.as_str()),
            ] {
                record_unknown(&mut errors, phase, kind, name, &reason);
            }
        }
    }
    match connect(&args.system_database_url).await {
        Ok(system_admin) => {
            let absent = system_admin
                .query_one(
                    "SELECT NOT EXISTS (SELECT FROM pg_database WHERE datname=$1)",
                    &[&resources.system_database],
                )
                .await
                .map(|row| row.get::<_, bool>(0));
            match &absent {
                Ok(absent) => record_absence(
                    &mut errors,
                    phase,
                    "system-database",
                    &resources.system_database,
                    *absent,
                ),
                Err(error) => record_unknown(
                    &mut errors,
                    phase,
                    "system-database",
                    &resources.system_database,
                    &error.to_string(),
                ),
            }
            for (kind, name) in [
                ("org", resources.org.as_str()),
                ("project", resources.project.as_str()),
                ("environment", resources.env.as_str()),
            ] {
                match &absent {
                    Ok(absent) => record_absence(&mut errors, phase, kind, name, *absent),
                    Err(error) => {
                        record_unknown(&mut errors, phase, kind, name, &error.to_string())
                    }
                }
            }
        }
        Err(error) => {
            let reason = format!("connect system admin: {error:#}");
            for (kind, name) in [
                ("system-database", resources.system_database.as_str()),
                ("org", resources.org.as_str()),
                ("project", resources.project.as_str()),
                ("environment", resources.env.as_str()),
            ] {
                record_unknown(&mut errors, phase, kind, name, &reason);
            }
        }
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

async fn run_forward(
    args: &CausationE2eArgs,
    resources: &GateResources,
    state: &mut GateState,
) -> anyhow::Result<()> {
    let mut system = connect(&args.system_database_url).await?;
    setup_registry(&mut system, resources).await?;
    setup_project(args, resources, state).await?;
    setup_stream(args, resources, state).await?;
    connect(&args.admin_database_url)
        .await?
        .batch_execute(&format!(
            "DO $m1$ BEGIN EXECUTE format(\
               'ALTER ROLE %I LOGIN VALID UNTIL %L', '{}', \
               clock_timestamp() + interval '10 minutes'); END $m1$;",
            resources.cdc_name
        ))
        .await
        .context("enable exact CDC role immediately before reader spawn")?;
    state.reader = Some(ReaderProcess::spawn_with_dup_window(
        reader_args(args, resources)?,
        BROKER_DUP_WINDOW_SECS,
    )?);
    wait_for_stream(args, resources).await?;
    commit_tenant_event(args, resources).await?;

    let first_delivery = wait_for_stored_event(args, resources).await?;
    let first_observed_at = Instant::now();
    let envelope: wamn_event_wire::Envelope = serde_json::from_slice(&first_delivery.payload)?;
    ensure!(
        first_delivery.subject
            == wamn_event_wire::subject(
                &resources.org,
                &resources.project,
                &resources.env,
                &resources.entity_id,
                Op::Insert,
            ),
        "stored source-event subject drifted"
    );
    ensure!(
        first_delivery.message_id
            == wamn_event_wire::msg_id(&resources.project, &resources.env, envelope.lsn),
        "stored source-event Nats-Msg-Id/LSN identity drifted"
    );
    ensure!(
        envelope.lsn != first_delivery.sequence,
        "source-event LSN was not independently distinguished from stream sequence"
    );
    ensure!(
        envelope.entity.as_deref() == Some(resources.entity_id.as_str()),
        "CDC entity identity drifted"
    );
    ensure!(
        envelope.causation.as_ref().map(|value| value.run.as_str())
            == Some(resources.source_run_id.as_str()),
        "CDC source-run stamp drifted"
    );
    ensure!(
        envelope.causation.as_ref().map(|value| value.root.as_str())
            == Some(resources.root_run_id.as_str()),
        "CDC root stamp drifted"
    );
    ensure!(
        envelope.causation.as_ref().map(|value| value.depth) == Some(1),
        "CDC depth stamp drifted"
    );

    let materializer = build_materializer(args, resources).await?;
    let first_report = materializer.run().await?;
    observe_durable(args, resources, state).await?;
    ensure!(
        counter(&first_report, "fired") == 1,
        "first delivery did not admit once: {first_report}"
    );
    ensure!(
        counter(&first_report, "duplicate") == 0,
        "first delivery was unexpectedly duplicate: {first_report}"
    );
    let admin = connect(&args.admin_database_url).await?;
    assert_one_causal_run(
        &admin,
        resources,
        first_delivery.sequence,
        &first_delivery.message_id,
    )
    .await?;
    ensure!(
        stream_message_count(args, resources).await? == 1,
        "materializer republished the source event"
    );

    // Cross the gate-specific broker dedup horizon before re-consuming the one
    // stored record. No publish occurs here: only the durable is recreated.
    tokio::time::sleep(Duration::from_secs(BROKER_DUP_WINDOW_SECS) + Duration::from_millis(100))
        .await;

    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let stream = js.get_stream(&resources.stream).await?;
    stream
        .delete_consumer(&resources.durable)
        .await
        .context("delete materializer durable to force stored redelivery")?;
    let second_delivery = wait_for_stored_event(args, resources).await?;
    let second_report = materializer.run().await?;
    let stored_messages = stream_message_count(args, resources).await?;
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
    assert_one_causal_run(
        &admin,
        resources,
        first_delivery.sequence,
        &first_delivery.message_id,
    )
    .await?;
    ensure!(stored_messages == 1, "redelivery republished the event");
    Ok(())
}

pub async fn run(args: CausationE2eArgs) -> anyhow::Result<()> {
    println!("# wamn-gates causation-e2e — tenant commit -> CDC -> stored event -> materializer");
    let resources = GateResources::from_args(&args)?;
    resources.log_record();
    let mut state = GateState::default();
    let mut isolated_admin = preflight_isolated_postgres(&args, &resources).await?;
    cleanup(&args, &resources, &mut state, "preclean").await?;
    setup_isolated_roles(&mut isolated_admin, &resources).await?;
    prepare_report_dir(&resources, &mut state)?;
    let result = async {
        let disposable = provision_databases(&args, &resources, &mut state).await?;
        run_forward(&disposable, &resources, &mut state).await
    }
    .await;
    let cleanup = cleanup(&args, &resources, &mut state, "final").await;
    finish_gate(result, cleanup)?;
    println!(
        "causation-e2e complete — one causal run/queue fact, byte-identical redelivery deduplicated"
    );
    Ok(())
}

/// Idempotently remove only the exact resources derived from one M1 Job identity.
pub async fn cleanup_only(args: CausationE2eArgs) -> anyhow::Result<()> {
    let resources = GateResources::from_args(&args)?;
    resources.log_record();
    let _isolated_admin = preflight_isolated_postgres(&args, &resources).await?;
    cleanup(&args, &resources, &mut GateState::default(), "external").await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> CausationE2eArgs {
        CausationE2eArgs {
            component: "/bench/materializer.wasm".into(),
            database_url: "postgres://wamn_app@127.0.0.1:5432/postgres?sslmode=disable".into(),
            admin_database_url: "postgres://postgres@127.0.0.1:5432/postgres?sslmode=disable"
                .into(),
            system_database_url: "postgres://postgres@127.0.0.1:5432/postgres?sslmode=disable"
                .into(),
            nats_url: "nats://nats:4222".into(),
            job_name: "m1-gate-proof1".into(),
            job_uid: "12345678-1234-4abc-8def-1234567890ab".into(),
            timeout_secs: 120,
        }
    }

    fn resources() -> GateResources {
        GateResources::from_args(&args()).unwrap()
    }

    #[test]
    fn forward_contract_pins_registration_and_durable_coordinate() {
        let resources = resources();
        let registration: serde_json::Value =
            serde_json::from_str(&registration_json(&resources)).unwrap();
        assert_eq!(registration["registration-id"], resources.registration_id);
        assert_eq!(registration["catalog-id"], resources.catalog_id);
        assert_eq!(registration["flow-id"], resources.flow_id);
        assert_eq!(registration["entity"], resources.entity_id);
        assert_eq!(registration["ops"], serde_json::json!(["insert"]));
        assert_eq!(
            mint_evt_run_id(
                &format!("{}:{}", resources.flow_id, resources.registration_id),
                7,
            ),
            format!(
                "{}:{}:evt:00000000000000000007",
                resources.flow_id, resources.registration_id
            )
        );
        assert_eq!(
            format!("evt:{}:7", resources.registration_id),
            format!("evt:r-{}:7", resources.suffix)
        );
    }

    #[test]
    fn tenant_commit_is_direct_transactional_and_reader_is_single_node() {
        let resources = resources();
        let sql = tenant_commit_sql(&resources);
        assert!(sql.contains("pg_logical_emit_message(true, 'wamn.causation'"));
        assert!(sql.contains(&resources.source_run_id));
        assert!(sql.contains(&resources.root_run_id));
        assert!(sql.contains("\"depth\":1"));
        assert!(!sql.contains("wamn_run"));
        assert!(!sql.contains("publish"));
        assert_eq!(reader_args(&args(), &resources).unwrap().stream_replicas, 1);
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
        assert_eq!(resources().stream, "M1_1234567812344abc8def1234567890ab");
    }

    #[test]
    fn isolated_sidecar_urls_and_manifest_are_fail_closed() {
        let base = args();
        let resources = GateResources::from_args(&base).unwrap();
        let scoped = disposable_args(&base, &resources).unwrap();
        require_loopback_url(&base.database_url, "application URL").unwrap();
        require_loopback_url(&base.admin_database_url, "admin URL").unwrap();
        assert_eq!(
            scoped.database_url,
            format!(
                "postgres://wamn_app@127.0.0.1:5432/{}?sslmode=disable",
                resources.project_database
            )
        );
        assert_eq!(
            scoped.admin_database_url,
            format!(
                "postgres://postgres@127.0.0.1:5432/{}?sslmode=disable",
                resources.project_database
            )
        );
        assert_eq!(
            scoped.system_database_url,
            format!(
                "postgres://postgres@127.0.0.1:5432/{}?sslmode=disable",
                resources.system_database
            )
        );
        assert_eq!(
            resources
                .report_dir
                .file_name()
                .and_then(|name| name.to_str()),
            Some(resources.gate_id.as_str())
        );

        let job = include_str!("../../../deploy/gates/m1-gate-job.yaml");
        let sidecar = include_str!("../../../deploy/gates/m1-postgres.Dockerfile");
        assert!(job.contains("restartPolicy: Always"));
        assert!(job.contains("emptyDir: {}"));
        assert!(job.contains("image: wamn-postgres:m1-pg18-720c455e"));
        assert!(!job.contains("m1-pg18-720c455e@sha256:"));
        assert!(job.contains("imagePullPolicy: Never"));
        assert!(job.contains("--sidecar-preflight-record"));
        assert!(!job.contains("ctr -n k8s.io images tag"));
        assert!(sidecar.contains("FROM --platform=linux/amd64 postgres:18.6-trixie@sha256:ae6c78831cbc35fa3a4aaf4d763ddacf6183d6004774cc2dc28b3920410d1d1a"));
        assert!(sidecar.contains("wamn.dev/upstream-child=\"sha256:cd78ca58eb75f929698e117a589488ccb2bd45107247fe02400b50ff6c418324\""));
        assert!(job.contains("listen_addresses=127.0.0.1"));
        assert!(job.contains("project_database=\"m1p_${suffix}\""));
        assert!(job.contains("cdc_role=\"m1cdc_${suffix}\""));
        assert!(job.contains("host ${project_database} ${cdc_role} 127.0.0.1/32 scram-sha-256"));
        assert!(job.contains("host all ${cdc_role} 127.0.0.1/32 reject"));
        let cdc_allow = job.find("host ${project_database} ${cdc_role}").unwrap();
        let cdc_reject = job.find("host all ${cdc_role}").unwrap();
        let admin_allow = job.find("local all postgres trust").unwrap();
        assert!(cdc_allow < cdc_reject && cdc_reject < admin_allow);
        assert!(job.contains("postgres://wamn_app@127.0.0.1:5432/postgres"));
        assert!(job.contains("value: sha256:POST_BUILD_MAIN_IMAGE_ID"));
        assert!(job.contains("M1_MAIN_IMAGE_ID=%s"));
        assert!(!job.contains("m1-388c99b3-debug"));
        assert!(!job.contains("secretKeyRef"));
        assert!(!job.contains("wamn-pg"));
        assert!(!job.contains("wamn-sysdb"));
        assert!(!job.contains("kind: Service"));
        assert!(!job.contains("kind: PersistentVolumeClaim"));
        assert!(!job.contains("kind: Role"));
        assert!(!job.contains("kind: ClusterRole"));
        assert!(job.contains("wamn.dev/m1-checks: \"9\""));
        assert!(job.contains("generateName: m1-gate-"));
        assert!(job.contains("serviceAccountName: event-reader"));
        assert!(job.contains("batch.kubernetes.io/controller-uid"));
        assert!(job.contains("m1-cleanup"));
        let whole_schema_drop = ["DROP", "SCHEMA"].join(" ");
        assert!(!include_str!("causation_e2e.rs").contains(&whole_schema_drop));
    }

    #[test]
    fn sidecar_hba_rejects_any_extra_or_preceding_cdc_rule() {
        let resources = resources();
        let exact = expected_sidecar_hba(&resources);
        require_exact_sidecar_hba(&exact, &resources).unwrap();

        let mut broad_first = exact.as_array().unwrap().clone();
        broad_first.insert(
            0,
            serde_json::json!({
                "type": "host", "database": ["all"], "user": ["all"],
                "address": "127.0.0.1", "netmask": "255.255.255.255",
                "auth": "trust", "error": null
            }),
        );
        assert!(
            require_exact_sidecar_hba(&serde_json::Value::Array(broad_first), &resources).is_err()
        );

        let mut reordered = exact.as_array().unwrap().clone();
        reordered.swap(0, 1);
        assert!(
            require_exact_sidecar_hba(&serde_json::Value::Array(reordered), &resources).is_err()
        );
    }

    #[test]
    fn run_identity_rejects_before_any_cleanup_name_exists() {
        let mut invalid = args();
        invalid.job_name = "foreign-job".into();
        assert!(GateResources::from_args(&invalid).is_err());
        invalid = args();
        invalid.job_uid = "not-a-kubernetes-uid".into();
        assert!(GateResources::from_args(&invalid).is_err());
    }

    #[test]
    fn resource_record_is_injective_bounded_and_domain_valid() {
        let first = resources();
        let mut retry_args = args();
        retry_args.job_name = "m1-gate-proof-retry".into();
        let retry = GateResources::from_args(&retry_args).unwrap();
        assert_eq!(first.suffix, retry.suffix);
        assert_eq!(first.project_database, retry.project_database);
        assert_eq!(first.system_database, retry.system_database);
        assert_eq!(first.schema, retry.schema);
        assert_eq!(first.table, retry.table);
        assert_eq!(first.cdc_name, retry.cdc_name);
        assert_eq!(first.stream, retry.stream);
        assert_eq!(first.durable, retry.durable);
        assert_eq!(first.report_dir, retry.report_dir);
        assert_ne!(first.cdc_password, retry.cdc_password);
        assert_ne!(first.owner, retry.owner);

        let mut second_args = args();
        second_args.job_uid = "22345678-1234-4abc-8def-1234567890ab".into();
        let second = GateResources::from_args(&second_args).unwrap();
        assert_ne!(first, second);
        for identifier in [
            &first.project_database,
            &first.system_database,
            &first.schema,
            &first.table,
            &first.cdc_name,
        ] {
            assert!(identifier.len() <= 63);
            assert!(identifier.contains(&first.suffix));
        }
        assert!(first.stream.len() <= 255);
        assert!(first.durable.len() <= 255);
        assert!(first.durable.contains(&first.suffix));
        wamn_control_provision::validate_project_env(&first.org, &first.project, &first.env)
            .unwrap();
    }

    #[test]
    fn cleanup_ownership_refuses_foreign_and_unrecorded_resources() {
        let resources = resources();
        assert!(
            require_owner(
                "stream",
                &resources.stream,
                Some("foreign-owner"),
                &resources,
                false,
            )
            .is_err()
        );
        assert!(require_owner("report directory", "report", None, &resources, false).is_err());
        assert!(require_owner("partial setup", "resource", None, &resources, true).is_ok());
        assert!(
            require_owner(
                "retry residue",
                "resource",
                Some(&resources.owner),
                &resources,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn durable_setup_record_authorizes_only_the_exact_uid_partial() {
        let mut owned_args = args();
        owned_args.job_name = format!("m1-gate-test-{}", std::process::id());
        owned_args.job_uid = format!(
            "12345678-1234-4abc-8def-{:012x}",
            u64::from(std::process::id())
        );
        let resources = GateResources::from_args(&owned_args).unwrap();
        assert!(
            !resources.report_dir.exists(),
            "stale test report directory"
        );
        let mut state = GateState::default();
        prepare_report_dir(&resources, &mut state).unwrap();
        assert!(state.ledger.report_dir);
        assert!(durable_setup_owned(&resources).unwrap());
        assert!(
            require_owner(
                "database",
                &resources.project_database,
                None,
                &resources,
                true
            )
            .is_ok()
        );
        std::fs::write(
            resources.report_dir.join("resource-record.json"),
            br#"{"foreign":true}"#,
        )
        .unwrap();
        assert!(durable_setup_owned(&resources).is_err());
        std::fs::remove_dir_all(&resources.report_dir).unwrap();
    }

    #[test]
    fn cleanup_source_pins_fail_closed_order_and_continuation() {
        let source = include_str!("causation_e2e.rs");
        let disable = source
            .find("ALTER ROLE {} NOLOGIN")
            .expect("cleanup disables role");
        let terminate = source
            .find("WHERE usename=$1 AND pid <> pg_backend_pid()")
            .expect("cleanup terminates exact-role sessions");
        let drop_role = source
            .find("DROP ROLE {}")
            .expect("cleanup drops exact role");
        let verify = source
            .find("verify_clean(args, resources, phase)")
            .expect("cleanup always verifies residue");
        let remove_authority = source
            .find("remove_dir_all(&resources.report_dir)")
            .expect("cleanup removes durable authority last");
        assert!(
            disable < terminate
                && terminate < drop_role
                && drop_role < remove_authority
                && remove_authority < verify
        );
        assert!(source.contains("if errors.is_empty()"));
        assert!(source.contains("if let Err(error) = project_admin"));
        for forbidden in [
            ["DROP", "SCHEMA"].join(" "),
            ["DROP", "OWNED"].join(" "),
            [" ", "LIKE", " "].join(""),
            ["CAS", "CADE"].join(""),
        ] {
            assert!(
                !source.contains(&forbidden),
                "broad cleanup token: {forbidden}"
            );
        }
        assert!(source.contains("phase={phase}"));
        assert!(source.contains("cdc-role-login"));
        assert!(source.contains("cdc-role-sessions"));
        let role_create = source.find("let role_transaction").unwrap();
        let role_comment = source[role_create..].find("COMMENT ON ROLE").unwrap() + role_create;
        let role_commit = source[role_comment..]
            .find("role_transaction.commit")
            .unwrap()
            + role_comment;
        let role_ledger = source[role_commit..]
            .find("ledger.cdc_role = true")
            .unwrap()
            + role_commit;
        assert!(
            role_create < role_comment && role_comment < role_commit && role_commit < role_ledger
        );
        assert!(source.contains("one or more exact CDC sessions refused termination"));
        assert!(source.contains("remained after termination"));
    }

    #[test]
    fn manifest_pins_signal_forwarding_and_single_exit_cleanup() {
        let job = include_str!("../../../deploy/gates/m1-gate-job.yaml");
        assert!(job.contains("terminationGracePeriodSeconds: 180"));
        assert!(job.contains("child_pid=$!"));
        assert!(job.contains("kill -\"$signal\" \"$child_pid\""));
        assert!(job.contains("wait \"$child_pid\""));
        assert!(job.contains("trap on_exit EXIT"));
        assert_eq!(job.matches("cleanup || cleanup_status=$?").count(), 1);
        assert!(!job.contains("kubectl apply"));
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
