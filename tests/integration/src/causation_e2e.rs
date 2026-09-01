//! M1 event-path proof: forward causation, durable deduplication, and tenant isolation.

use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use async_nats::header::NATS_MESSAGE_ID;
use clap::Args;
use futures_util::StreamExt as _;
use pg_walstream::{BaseBackupOptions, PgReplicationConnection};
use tokio_postgres::{Client, NoTls};

use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi::p2::bindings::CommandPre;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use crate::cdc_reader_process::{ReaderArgs, ReaderProcess};
use crate::release_fixture::{ReleaseFixture, load_release};
use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, SystemReader, WorkloadRoleFamily, WorkloadRoleScope,
    sql as provision_sql, system_reader_generation_role, workload_generation_role,
};
use wamn_control_registry::DurabilityClass;
use wamn_control_registry::identifiers::{doorbell_subject, mvp_execution_target_id};
use wamn_control_registry::sql as registry_sql;
use wamn_event_wire::Op;
use wamn_run_state::queue::mint_evt_run_id;
use wamn_runtime::engine::build_engine;
use wamn_runtime::plugins::wamn_jetstream::{
    self, WAMN_JETSTREAM_ID, WamnJetstream, WamnJetstreamConfig,
};
use wamn_runtime::plugins::wamn_postgres::{
    self, ClassCredentials, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig,
};

const SYSTEM_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");

const BROKER_DUP_WINDOW_SECS: u64 = 1;
const SIDECAR_POSTGRES_MAJOR: i64 = 18;
// Match the reader's generous graceful-shutdown window while remaining well
// inside the Pod's 180-second termination grace period for a bounded retry.
const CDC_BACKEND_TERMINATION_TIMEOUT_MS: i64 = 15_000;
// Long enough to expose fire-and-forget termination, while automatic resume
// keeps even a failed cleanup probe independently bounded.
const CDC_WITNESS_RESUME_DELAY_SECS: u64 = 1;
const M1_GENERATION_EXPIRES_AT: &str = "2100-01-01T00:00:00Z";

#[derive(Args)]
pub struct CausationE2eArgs {
    /// The compiled production materializer component.
    #[arg(long, default_value = "/bench/materializer.wasm")]
    pub component: PathBuf,

    /// Job-scoped application-generation URL, populated only after provisioning.
    #[arg(skip)]
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

#[derive(Clone, PartialEq, Eq)]
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
    app_generation: String,
    app_password: String,
    registry_reader_generation: String,
    registry_reader_password: String,
    cdc_name: String,
    cdc_password: String,
    stream: String,
    org: String,
    project: String,
    env: String,
    tenant: String,
    package_id: String,
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
        let project_database = format!("m1p_{suffix}");
        let system_database = format!("m1y_{suffix}");
        let org = format!("o-{}", &suffix[..10]);
        let project = format!("p-{}", &suffix[10..20]);
        let env = format!("e-{}", &suffix[20..]);
        let app_generation = workload_generation_role(
            WorkloadRoleFamily::App,
            WorkloadRoleScope::Tenant {
                tenant: &tenant,
                database: &project_database,
            },
            CredentialGeneration::A,
        )
        .context("derive the Job-scoped M1 application generation")?;
        let registry_reader_generation = system_reader_generation_role(
            SystemReader::Registry,
            &org,
            &project,
            &env,
            &system_database,
            CredentialGeneration::A,
        );
        let package_id = format!("p_{suffix}");
        let registration_id = format!("r-{suffix}");
        let durable = format!("mat_{tenant}_{package_id}_{registration_id}");
        let mut secret = [0u8; 72];
        std::fs::File::open("/dev/urandom")
            .context("open operating-system random source")?
            .read_exact(&mut secret)
            .context("read run-scoped PostgreSQL secrets")?;
        let resources = Self {
            job_name: args.job_name.clone(),
            job_uid: args.job_uid.clone(),
            gate_id: format!("wamn-m1-{suffix}"),
            project_database,
            system_database,
            schema: format!("m1s_{suffix}"),
            table: format!("receipts_{suffix}"),
            app_generation,
            app_password: hex::encode(&secret[..24]),
            registry_reader_generation,
            registry_reader_password: hex::encode(&secret[24..48]),
            cdc_name: format!("m1cdc_{suffix}"),
            cdc_password: hex::encode(&secret[48..]),
            stream: format!("M1_{suffix}"),
            org,
            project,
            env,
            tenant,
            package_id,
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
            (&self.app_generation, "application generation"),
            (
                &self.registry_reader_generation,
                "registry-reader generation",
            ),
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
            (&self.package_id, "package"),
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
                "app_generation": self.app_generation,
                "registry_reader_generation": self.registry_reader_generation,
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
                "package": self.package_id,
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

#[derive(Default)]
struct GateState {
    reader: Option<ReaderProcess>,
    cleanup_session_witness: Option<PgReplicationConnection>,
    ledger: SetupLedger,
}

#[derive(Debug, Default)]
struct SetupLedger {
    project_database: bool,
    system_database: bool,
    app_generation: bool,
    registry_reader_generation: bool,
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

fn registration_json(resources: &GateResources) -> String {
    wamn_event_reg::EventRegistration {
        schema_version: wamn_event_reg::SCHEMA_VERSION.into(),
        registration_id: resources.registration_id.clone(),
        package_id: resources.package_id.clone(),
        source_package_id: resources.package_id.clone(),
        entity: resources.entity_id.clone(),
        ops: vec![Op::Insert, Op::Delete],
        input: wamn_event_reg::RegistrationInput::default(),
        condition: None,
    }
    .to_json()
}

fn role_url(admin_url: &str, role: &str, password: &str) -> anyhow::Result<String> {
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
    Ok(format!("postgres://{role}:{password}@{host_and_path}"))
}

fn app_database_url(admin_url: &str, resources: &GateResources) -> anyhow::Result<String> {
    let plain = role_url(
        admin_url,
        &resources.app_generation,
        &resources.app_password,
    )?;
    Ok(format!(
        "{}?sslmode=disable",
        swap_database(&plain, &resources.project_database)?
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
        database_url: app_database_url(&args.admin_database_url, resources)?,
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
    let mut project_admin = connect(&args.admin_database_url).await?;
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

    // Production converges this cluster-wide floor before granting each
    // workload identity its exact database. The sibling database must start
    // behind the floor, while the positive M1 path grants CONNECT only to its
    // Job-scoped App generation. M-CONNECT-FLOOR: deleting the floor makes the
    // sibling logical-replication connection succeed and the named proof below
    // fail.
    project_admin
        .batch_execute(provision_sql::revoke_public_connect_floor_sql())
        .await
        .context("converge production PUBLIC CONNECT floor")?;
    let generation = project_admin.transaction().await?;
    generation
        .batch_execute(&provision_sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::App,
            &resources.project_database,
            &resources.app_generation,
            &resources.app_password,
            M1_GENERATION_EXPIRES_AT,
        ))
        .await
        .context("prepare the Job-scoped M1 application generation")?;
    generation
        .batch_execute(&format!(
            "COMMENT ON ROLE {} IS '{}'",
            resources.app_generation, resources.owner
        ))
        .await
        .context("mark the Job-scoped M1 application generation owner")?;
    generation.commit().await?;
    state.ledger.app_generation = true;
    require_stable_app_role(&project_admin, Some(&resources.app_generation)).await?;
    require_app_generation(&project_admin, resources).await?;
    let disposable = disposable_args(args, resources)?;
    require_loopback_url(&disposable.database_url, "application generation URL")?;
    Ok(disposable)
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect PostgreSQL")?;
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

fn expected_sidecar_hba() -> serde_json::Value {
    serde_json::json!([
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
            "type": "host", "database": ["all"], "user": ["all"],
            "address": "all", "netmask": null,
            "auth": "scram-sha-256", "error": null
        }
    ])
}

fn require_exact_sidecar_hba(observed: &serde_json::Value) -> anyhow::Result<()> {
    ensure!(
        observed == &expected_sidecar_hba(),
        "isolated HBA must be exactly the loopback fixture exceptions followed by the production host all all all scram-sha-256 rule, with no physical-replication admission"
    );
    Ok(())
}

async fn preflight_isolated_postgres(args: &CausationE2eArgs) -> anyhow::Result<Client> {
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
    require_exact_sidecar_hba(&hba)?;
    Ok(admin)
}

async fn setup_isolated_roles(admin: &mut Client, resources: &GateResources) -> anyhow::Result<()> {
    let app_role = provision_sql::ensure_app_acl_role_sql();
    let transaction = admin.transaction().await?;
    transaction
        .batch_execute(&format!(
            "{app_role} \
             DO $m1$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
                 CREATE ROLE wamn_system LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
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
    admin
        .batch_execute(&provision_sql::drain_app_role_sessions_sql())
        .await
        .context("drain retired shared application-role sessions")?;
    require_role(admin, "wamn_system", true, true).await?;
    require_stable_app_role(admin, None).await?;
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

async fn require_stable_app_role(
    client: &Client,
    expected_generation: Option<&str>,
) -> anyhow::Result<()> {
    let row = client
        .query_opt(provision_sql::workload_generation_state_sql(), &[&APP_ROLE])
        .await?
        .context("the stable wamn_app ACL role is absent")?;
    let expected_members = expected_generation
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ensure!(
        !row.get::<_, bool>("rolcanlogin")
            && !row.get::<_, bool>("rolsuper")
            && !row.get::<_, bool>("rolinherit")
            && !row.get::<_, bool>("rolcreaterole")
            && !row.get::<_, bool>("rolcreatedb")
            && !row.get::<_, bool>("rolreplication")
            && !row.get::<_, bool>("rolbypassrls")
            && !row.get::<_, bool>("password_set")
            && row.get::<_, Vec<String>>("memberships").is_empty()
            && row.get::<_, bool>("membership_options_exact")
            && row.get::<_, Vec<String>>("member_roles") == expected_members
            && row.get::<_, bool>("member_options_exact")
            && row.get::<_, bool>("generation_children_exact")
            && row.get::<_, Vec<String>>("connect_databases").is_empty()
            && row.get::<_, i64>("sessions") == 0
            && row.get::<_, i64>("owned_objects") == 0,
        "stable wamn_app is not the exact passwordless NOLOGIN NOINHERIT ACL carrier"
    );
    Ok(())
}

async fn require_app_generation(client: &Client, resources: &GateResources) -> anyhow::Result<()> {
    let row = client
        .query_opt(
            provision_sql::workload_generation_state_sql(),
            &[&resources.app_generation],
        )
        .await?
        .context("the Job-scoped M1 application generation is absent")?;
    ensure!(
        row.get::<_, bool>("rolcanlogin")
            && !row.get::<_, bool>("rolsuper")
            && row.get::<_, bool>("rolinherit")
            && !row.get::<_, bool>("rolcreaterole")
            && !row.get::<_, bool>("rolcreatedb")
            && !row.get::<_, bool>("rolreplication")
            && !row.get::<_, bool>("rolbypassrls")
            && row.get::<_, bool>("password_set")
            && row.get::<_, bool>("valid_until_finite")
            && row.get::<_, Option<String>>("valid_until").as_deref()
                == Some(M1_GENERATION_EXPIRES_AT)
            && row.get::<_, Vec<String>>("memberships") == vec![APP_ROLE.to_string()]
            && row.get::<_, bool>("membership_options_exact")
            && row.get::<_, Vec<String>>("member_roles").is_empty()
            && row.get::<_, Vec<String>>("connect_databases")
                == vec![resources.project_database.clone()]
            && row.get::<_, i64>("sessions") == 0
            && row.get::<_, i64>("owned_objects") == 0,
        "Job-scoped M1 application generation is not an exact active credential"
    );
    Ok(())
}

async fn setup_registry(
    system: &mut Client,
    resources: &GateResources,
    state: &mut GateState,
) -> anyhow::Result<()> {
    // The canonical owner lives only inside this Job's emptyDir-backed server.
    require_role(system, "wamn_system", true, true).await?;
    system.batch_execute(SYSTEM_SQL).await?;
    system
        .batch_execute(&provision_sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::RegistryReader,
            &resources.system_database,
            &resources.registry_reader_generation,
            &resources.registry_reader_password,
            M1_GENERATION_EXPIRES_AT,
        ))
        .await
        .context("prepare the Job-scoped M1 registry-reader generation")?;
    state.ledger.registry_reader_generation = true;
    system
        .batch_execute(&format!(
            "COMMENT ON ROLE {} IS '{}'",
            resources.registry_reader_generation, resources.owner
        ))
        .await
        .context("mark the Job-scoped M1 registry-reader generation owner")?;
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
                &"standard",
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
                &&resources.suffix[..8], // M-PROJECT-ENV-SUFFIX: omission is a 6-vs-5 prepare error.
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
    require_stable_app_role(&admin, Some(&resources.app_generation)).await?;
    require_app_generation(&admin, resources).await?;
    require_role(&admin, "wamn_scenario_author", false, false).await?;
    require_role(&admin, "wamn_effect_writer", false, false).await?;
    require_role(&admin, "wamn_run_projection_writer", false, false).await?;
    admin.batch_execute(CATALOG_SQL).await?;
    admin.batch_execute(RUN_STATE_SQL).await?;
    admin.batch_execute(RUN_QUEUE_SQL).await?;
    wamn_ctl::reconcile_run_plane::converge_environment_policy(
        &admin,
        &wamn_schema_control::BareSchemaName::new("wamn_run")?,
        &resources.tenant,
        &resources.env,
        DurabilityClass::Standard,
        true,
    )
    .await?;
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
               payload text NOT NULL, PRIMARY KEY (id)); \
             ALTER TABLE {schema}.{table} ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {schema}.{table} FORCE ROW LEVEL SECURITY; \
             CREATE POLICY receipts_tenant ON {schema}.{table} \
               USING (tenant_id=NULLIF(current_setting('app.tenant',true),'')) \
               WITH CHECK (tenant_id=NULLIF(current_setting('app.tenant',true),'')); \
             GRANT SELECT,INSERT,UPDATE,DELETE ON {schema}.{table} TO wamn_app; \
             CREATE TABLE {schema}.wamn_entities (relation_oid oid PRIMARY KEY, \
               package_id text NOT NULL, entity_id text NOT NULL, table_name text NOT NULL); \
             INSERT INTO {schema}.wamn_entities VALUES \
               ('{schema}.{table}'::regclass::oid,'{package_id}','{entity_id}','{table}');",
            schema = resources.schema,
            table = resources.table,
            package_id = resources.package_id,
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

    let registration = registration_json(resources);
    let transaction = admin.transaction().await?;
    transaction
        .execute(
            "INSERT INTO catalog.event_registrations \
           (tenant_id,package_id,registration_id,entity_id,registration) \
         VALUES ($1,$2,$3,$4,$5::text::jsonb)",
            &[
                &resources.tenant,
                &resources.package_id,
                &resources.registration_id,
                &resources.entity_id,
                &registration,
            ],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            status,trigger_source,event_source_run_id,event_root_run_id,event_depth) \
         VALUES \
           ($1,$2,$3,1,$4,1,$5,'completed','event',$2,$2,0), \
           ($1,$6,$3,1,$4,1,$5,'completed','event',$2,$2,1)",
            &[
                &resources.tenant,
                &resources.root_run_id,
                &resources.flow_id,
                &resources.package_id,
                &resources.env,
                &resources.source_run_id,
            ],
        )
        .await?;
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
        system_database_url: role_url(
            &args.system_database_url,
            &resources.registry_reader_generation,
            &resources.registry_reader_password,
        )?,
        cdc_url: role_url(
            &args.admin_database_url,
            &resources.cdc_name,
            &resources.cdc_password,
        )?,
        nats_url: args.nats_url.clone(),
        stream_replicas: 1,
    })
}

fn replication_url(plain_url: &str, mode: &str) -> String {
    debug_assert!(!plain_url.contains('?'));
    format!("{plain_url}?sslmode=disable&replication={mode}")
}

fn require_replication_startup_refusal(
    error: &pg_walstream::ReplicationError,
    sqlstate: &str,
    literal: &str,
    label: &str,
) -> anyhow::Result<()> {
    let rendered = error.to_string();
    ensure!(
        rendered.contains(&format!("SQLSTATE {sqlstate}")),
        "{label} returned the wrong SQLSTATE: {rendered}"
    );
    ensure!(
        rendered.contains(literal),
        "{label} returned the wrong refusal literal: {rendered}"
    );
    Ok(())
}

/// Prove the deployed CDC transport boundary before the production reader uses it.
fn prove_replication_protocol_confinement(
    args: &CausationE2eArgs,
    resources: &GateResources,
) -> anyhow::Result<PgReplicationConnection> {
    let own_plain = role_url(
        &args.admin_database_url,
        &resources.cdc_name,
        &resources.cdc_password,
    )?;
    let own_logical_url = replication_url(&own_plain, "database");

    // Positive control: the repository's production HBA rule admits a logical
    // walsender session to the database on which the role has explicit CONNECT.
    // The later reader run proves the same route remains usable for actual CDC.
    let mut own = PgReplicationConnection::connect(&own_logical_url)
        .context("open own-database logical replication connection")?;
    let identified = own
        .identify_system()
        .context("identify own-database logical replication system")?;
    ensure!(
        identified.ntuples() == 1
            && identified.get_value(0, 3).as_deref() == Some(resources.project_database.as_str()),
        "own logical replication identified the wrong database"
    );
    let physical_url = replication_url(&own_plain, "true");

    // M-PHYSICAL-HBA-START: adding a physical-replication HBA admission lets
    // this connection cross the boundary and fails the gate before the command
    // outcome can be mistaken for the HBA refusal.
    let start_result = match PgReplicationConnection::connect(&physical_url) {
        Err(error) => require_replication_startup_refusal(
            &error,
            "28000",
            "no pg_hba.conf entry for replication connection",
            "physical START_REPLICATION",
        ),
        Ok(mut physical) => {
            let command_accepted = physical.start_physical_replication(None, 0, None).is_ok();
            Err(anyhow::anyhow!(
                "physical START_REPLICATION crossed the production HBA boundary (command accepted: {command_accepted})"
            ))
        }
    };

    // M-PHYSICAL-HBA-BASE-BACKUP is a separate connection attempt because HBA
    // rejects before either replication command exists. Under a broadened HBA,
    // TARGET blackhole prevents the mutant probe from writing backup material.
    let backup_result = match PgReplicationConnection::connect(&physical_url) {
        Err(error) => require_replication_startup_refusal(
            &error,
            "28000",
            "no pg_hba.conf entry for replication connection",
            "BASE_BACKUP",
        ),
        Ok(mut physical) => {
            let command_accepted = physical
                .base_backup(&BaseBackupOptions {
                    target: Some("blackhole".into()),
                    checkpoint: Some("fast".into()),
                    manifest: Some("no".into()),
                    ..Default::default()
                })
                .is_ok();
            Err(anyhow::anyhow!(
                "BASE_BACKUP crossed the production HBA boundary (command accepted: {command_accepted})"
            ))
        }
    };
    match (start_result, backup_result) {
        (Ok(()), Ok(())) => {}
        (Err(start), Ok(())) => return Err(start),
        (Ok(()), Err(backup)) => return Err(backup),
        (Err(start), Err(backup)) => {
            anyhow::bail!("{start:#}; BASE_BACKUP also failed: {backup:#}")
        }
    }

    // The production HBA deliberately admits logical startup cluster-wide;
    // database CONNECT is the second, load-bearing boundary. M-CONNECT-FLOOR
    // removes the floor and makes this sibling connection succeed.
    let sibling_plain = swap_database(&own_plain, &resources.system_database)?;
    let sibling_url = replication_url(&sibling_plain, "database");
    let sibling_error = match PgReplicationConnection::connect(&sibling_url) {
        Err(error) => error,
        Ok(mut sibling) => {
            let command_accepted = sibling.identify_system().is_ok();
            anyhow::bail!(
                "sibling logical replication crossed the database CONNECT floor (IDENTIFY_SYSTEM accepted: {command_accepted})"
            );
        }
    };
    let sibling_literal = format!(
        "permission denied for database \"{}\"",
        resources.system_database
    );
    require_replication_startup_refusal(
        &sibling_error,
        "42501",
        &sibling_literal,
        "sibling logical replication",
    )?;
    ensure!(
        sibling_error
            .to_string()
            .contains("User does not have CONNECT privilege"),
        "sibling logical replication did not name the CONNECT floor: {sibling_error}"
    );
    // Retain the admitted logical session through cleanup so bounded backend
    // termination is a live proof, not a vacuous source-shape assertion.
    Ok(own)
}

/// Prove the CDC credential cannot use its ordinary SQL session for tenant DML.
async fn prove_cdc_dml_confinement(
    args: &CausationE2eArgs,
    resources: &GateResources,
) -> anyhow::Result<()> {
    let relation = format!("{}.{}", resources.schema, resources.table);
    let admin = connect(&args.admin_database_url).await?;
    let privileges = admin
        .query_one(
            "SELECT has_table_privilege($1, $2, 'INSERT'), \
                    has_table_privilege($1, $2, 'UPDATE'), \
                    has_table_privilege($1, $2, 'DELETE')",
            &[&resources.cdc_name, &relation],
        )
        .await
        .context("read effective CDC DML privileges")?;
    ensure!(
        !privileges.get::<_, bool>(0)
            && !privileges.get::<_, bool>(1)
            && !privileges.get::<_, bool>(2),
        "CDC role gained INSERT, UPDATE, or DELETE on the tenant relation"
    );

    // M-CDC-DML-GRANT: an INSERT grant flips both the effective inventory and
    // this actual boundary attempt; UPDATE/DELETE-only mutants fail inventory.
    let sql_url = format!(
        "{}?sslmode=disable",
        role_url(
            &args.admin_database_url,
            &resources.cdc_name,
            &resources.cdc_password,
        )?
    );
    let (client, connection) = tokio_postgres::connect(&sql_url, NoTls)
        .await
        .context("open CDC ordinary SQL connection")?;
    let connection_task = tokio::spawn(connection);
    let insert_sql = format!(
        "INSERT INTO {}.{} (tenant_id,id,payload) VALUES ($1,$2,$3)",
        resources.schema, resources.table
    );
    let probe_id = format!("cdc-dml-probe-{}", resources.suffix);
    let error = client
        .execute(&insert_sql, &[&resources.tenant, &probe_id, &"must-refuse"])
        .await
        .expect_err("CDC ordinary SQL must refuse tenant INSERT");
    let database_error = error
        .as_db_error()
        .context("CDC tenant INSERT refusal was not a PostgreSQL database error")?;
    ensure!(
        database_error.code().code() == "42501",
        "CDC tenant INSERT returned SQLSTATE {}, expected 42501",
        database_error.code().code()
    );
    let expected_literal = format!("permission denied for table {}", resources.table);
    ensure!(
        database_error.message() == expected_literal,
        "CDC tenant INSERT returned literal {:?}, expected {:?}",
        database_error.message(),
        expected_literal
    );
    drop(client);
    connection_task
        .await
        .context("join CDC ordinary SQL connection task")?
        .context("close CDC ordinary SQL connection")?;
    Ok(())
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
    let current_user: String = app
        .query_one("SELECT current_user::text", &[])
        .await?
        .get(0);
    ensure!(
        current_user == resources.app_generation,
        "tenant commit did not dial the Job-scoped M1 application generation"
    );
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

fn foreign_tenant(resources: &GateResources) -> String {
    format!("foreign-{}", resources.suffix)
}

async fn commit_tenant_isolation_events(
    args: &CausationE2eArgs,
    resources: &GateResources,
) -> anyhow::Result<()> {
    let foreign_tenant = foreign_tenant(resources);
    let mut admin = connect(&args.admin_database_url).await?;
    let foreign = admin.transaction().await?;
    foreign.batch_execute(&tenant_commit_sql(resources)).await?;
    let inserted = foreign
        .execute(
            &format!(
                "INSERT INTO {}.{} (tenant_id,id,payload) VALUES ($1,$2,$3)",
                resources.schema, resources.table
            ),
            &[&foreign_tenant, &"foreign-1", &"must-skip"],
        )
        .await?;
    ensure!(inserted == 1, "foreign tenant event was not committed once");
    foreign.commit().await?;

    let mut app = connect(&args.database_url).await?;
    app.batch_execute(&format!(
        "SET search_path TO {}; SET app.tenant TO '{}';",
        resources.schema, resources.tenant
    ))
    .await?;
    let unscopable = app.transaction().await?;
    unscopable
        .batch_execute(&tenant_commit_sql(resources))
        .await?;
    let deleted = unscopable
        .execute(
            &format!(
                "DELETE FROM {}.{} WHERE tenant_id=$1 AND id=$2",
                resources.schema, resources.table
            ),
            &[&resources.tenant, &"forward-1"],
        )
        .await?;
    ensure!(deleted == 1, "tenant event row was not deleted once");
    unscopable.commit().await?;
    Ok(())
}

async fn wait_for_stored_events(
    args: &CausationE2eArgs,
    resources: &GateResources,
    expected: u64,
) -> anyhow::Result<Vec<StoredEvent>> {
    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    loop {
        if let Ok(mut stream) = js.get_stream(&resources.stream).await {
            let info = stream.info().await?;
            let messages = info.state.messages;
            let first_sequence = info.state.first_sequence;
            if messages == expected {
                let mut events = Vec::with_capacity(expected as usize);
                for sequence in first_sequence..first_sequence + expected {
                    let message = stream.get_raw_message(sequence).await?;
                    events.push(StoredEvent {
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
                return Ok(events);
            }
            ensure!(
                messages <= expected,
                "expected {expected} CDC events, found {messages}"
            );
        }
        ensure!(
            Instant::now() < deadline,
            "CDC events did not reach {}",
            resources.stream
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_stored_event(
    args: &CausationE2eArgs,
    resources: &GateResources,
) -> anyhow::Result<StoredEvent> {
    let mut events = wait_for_stored_events(args, resources, 1).await?;
    events.pop().context("stored CDC event was not returned")
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
    // wamn-0h0g.22.16: the gate names the authority each credential belongs to.
    // No family is cut over, so its one url is written down for every class.
    pg_config.credentials = Some(ClassCredentials::every_class(args.database_url.clone()));
    let pg = Arc::new(WamnPostgres::new(pg_config)?);
    pg.set_tenant(&resources.gate_id, &resources.tenant)?;
    pg.set_schema(&resources.gate_id, "wamn_run")?;
    // The guest lifecycle now selects the credential BY TENANT
    // (wamn-0h0g.22.6.7), so the probe names the same tenant the gate binds.
    pg.probe_checkout(&resources.tenant).await?;
    // The production guest's durable-consumer bind is release-gated
    // (wamn-0h0g.15.95): a release-less host refuses the run-owned durable, so
    // no CDC event would ever be decided.
    let release = load_release(ReleaseFixture {
        tenant: resources.tenant.as_str(),
        package: resources.package_id.as_str(),
        environment: resources.env.as_str(),
        registration: resources.registration_id.as_str(),
        wiring: "causation-event-handler",
        entity: resources.entity_id.as_str(),
        ops: &["insert", "delete"],
    })?;
    let jetstream = Arc::new(
        WamnJetstream::new(WamnJetstreamConfig {
            nats_url: Some(args.nats_url.clone()),
        })
        .with_doorbell(async_nats::connect(&args.nats_url).await?)
        .with_release(Some(release)),
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

fn tenant_isolation_report_is_exact(report: &serde_json::Value) -> bool {
    counter(report, "skip-foreign-tenant") == 1
        && counter(report, "refuse-tenant-unscopable") == 1
        && [
            "fired",
            "duplicate",
            "skip-entity",
            "skip-op",
            "skip-condition-false",
            "refuse-depth",
            "refuse-old-image-absent",
            "refuse-condition-error",
            "refuse-seq",
            "held-registrations",
            "poison",
            "effect-retry",
            "doorbell-failed",
        ]
        .into_iter()
        .all(|name| counter(report, name) == 0)
}

fn delete_old_has_no_tenant_value(old: &serde_json::Map<String, serde_json::Value>) -> bool {
    matches!(old.get("tenant_id"), None | Some(serde_json::Value::Null))
}

async fn admission_counts(admin: &Client) -> anyhow::Result<(i64, i64)> {
    let row = admin
        .query_one(
            "SELECT (SELECT count(*) FROM wamn_run.runs), \
                    (SELECT count(*) FROM wamn_run.run_queue)",
            &[],
        )
        .await?;
    Ok((row.get(0), row.get(1)))
}

async fn wait_for_tenant_isolation_ack(
    args: &CausationE2eArgs,
    resources: &GateResources,
    final_sequence: u64,
) -> anyhow::Result<()> {
    let js = async_nats::jetstream::new(async_nats::connect(&args.nats_url).await?);
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    loop {
        let stream = js.get_stream(&resources.stream).await?;
        let info = stream.consumer_info(&resources.durable).await?;
        if info.num_pending == 0
            && info.num_ack_pending == 0
            && info.ack_floor.stream_sequence == final_sequence
        {
            ensure!(
                info.num_redelivered == 0,
                "tenant-isolation events redelivered {} time(s)",
                info.num_redelivered
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "tenant-isolation durable did not settle: pending={} ack-pending={} \
             ack-floor={} expected={final_sequence}",
            info.num_pending,
            info.num_ack_pending,
            info.ack_floor.stream_sequence,
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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
    let expected_hash = wamn_execution_contract::canonical_json_sha256(&registration);
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

async fn run_tenant_isolation(
    args: &CausationE2eArgs,
    resources: &GateResources,
    materializer: &MaterializerHarness,
    admin: &Client,
    first_delivery: &StoredEvent,
) -> anyhow::Result<()> {
    let before = admission_counts(admin).await?;
    let doorbell_nats = async_nats::connect(&args.nats_url).await?;
    let target = mvp_execution_target_id(&resources.tenant)?;
    let mut doorbells = doorbell_nats.subscribe(doorbell_subject(&target)).await?;
    doorbell_nats.flush().await?;

    commit_tenant_isolation_events(args, resources).await?;
    let stored = wait_for_stored_events(args, resources, 3).await?;
    ensure!(
        stored.first() == Some(first_delivery),
        "check 9 stored event changed during check 10"
    );
    ensure!(
        stored
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence),
        "tenant-isolation events did not preserve commit order"
    );

    let foreign_delivery = &stored[1];
    let foreign: wamn_event_wire::Envelope = serde_json::from_slice(&foreign_delivery.payload)?;
    ensure!(foreign.op == Op::Insert, "foreign event operation drifted");
    ensure!(
        foreign_delivery.subject
            == wamn_event_wire::subject(
                &resources.org,
                &resources.project,
                &resources.env,
                &resources.entity_id,
                Op::Insert,
            ),
        "foreign event subject drifted"
    );
    ensure!(
        foreign_delivery.message_id
            == wamn_event_wire::msg_id(&resources.project, &resources.env, foreign.lsn),
        "foreign event Nats-Msg-Id/LSN identity drifted"
    );
    ensure!(
        foreign.entity == resources.entity_id,
        "foreign event entity drifted"
    );
    ensure!(
        foreign.table == resources.table,
        "foreign event table drifted"
    );
    ensure!(
        foreign
            .new
            .as_ref()
            .and_then(|image| image.get("tenant_id"))
            .and_then(serde_json::Value::as_str)
            == Some(foreign_tenant(resources).as_str()),
        "foreign event did not carry the exact foreign tenant"
    );

    let unscopable_delivery = &stored[2];
    let unscopable: wamn_event_wire::Envelope =
        serde_json::from_slice(&unscopable_delivery.payload)?;
    ensure!(
        unscopable.op == Op::Delete,
        "unscopable event operation drifted"
    );
    ensure!(
        unscopable_delivery.subject
            == wamn_event_wire::subject(
                &resources.org,
                &resources.project,
                &resources.env,
                &resources.entity_id,
                Op::Delete,
            ),
        "unscopable event subject drifted"
    );
    ensure!(
        unscopable_delivery.message_id
            == wamn_event_wire::msg_id(&resources.project, &resources.env, unscopable.lsn),
        "unscopable event Nats-Msg-Id/LSN identity drifted"
    );
    ensure!(
        unscopable.entity == resources.entity_id,
        "unscopable event entity drifted"
    );
    ensure!(
        unscopable.table == resources.table,
        "unscopable event table drifted"
    );
    let old = unscopable
        .old
        .as_ref()
        .context("REPLICA IDENTITY DEFAULT delete lacks its key image")?;
    let id_match = old.get("id").and_then(serde_json::Value::as_str) == Some("forward-1");
    let tenant_value_absent = delete_old_has_no_tenant_value(old);
    let new_absent = unscopable.new.is_none();
    ensure!(
        id_match && tenant_value_absent && new_absent,
        "delete was not the tenant-unscopable key-only old image: \
         id_match={id_match} tenant_value_absent={tenant_value_absent} \
         new_absent={new_absent} old_field_count={}",
        old.len()
    );

    let report = materializer.run().await?;
    ensure!(
        tenant_isolation_report_is_exact(&report),
        "tenant-isolation materializer verdicts drifted: {report}"
    );
    wait_for_tenant_isolation_ack(args, resources, unscopable_delivery.sequence).await?;
    ensure!(
        tokio::time::timeout(Duration::from_millis(300), doorbells.next())
            .await
            .is_err(),
        "tenant-isolation events rang the admission doorbell"
    );
    ensure!(
        admission_counts(admin).await? == before,
        "tenant-isolation events changed run or queue rows"
    );
    assert_one_causal_run(
        admin,
        resources,
        first_delivery.sequence,
        &first_delivery.message_id,
    )
    .await?;
    ensure!(
        stream_message_count(args, resources).await? == 3,
        "tenant-isolation materializer changed the three-record stream"
    );
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

async fn cleanup_session_witness_pid(
    project_admin: &Client,
    resources: &GateResources,
) -> anyhow::Result<i32> {
    let rows = project_admin
        .query(
            "SELECT pid FROM pg_stat_activity \
             WHERE usename=$1 AND datname=$2 AND backend_type='walsender' \
             AND state='idle' AND pid <> pg_backend_pid() ORDER BY pid",
            &[&resources.cdc_name, &resources.project_database],
        )
        .await
        .with_context(|| {
            format!(
                "locate exact cleanup witness for slot {}",
                resources.cdc_name
            )
        })?;
    let pids = rows
        .iter()
        .map(|row| row.get::<_, i32>(0))
        .collect::<Vec<_>>();
    ensure!(
        pids.len() == 1,
        "cleanup witness cardinality mismatch: pid(s)={pids:?} slot={}",
        resources.cdc_name
    );
    Ok(pids[0])
}

async fn pause_cleanup_session_witness(
    project_admin: &Client,
    resources: &GateResources,
    pid: i32,
) -> anyhow::Result<()> {
    // This isolated sidecar-only control stops the live witness and arranges
    // its own resume. A one-argument pg_terminate_backend call then returns
    // before the PID disappears; the bounded two-argument call waits for it.
    let pause_sql = format!(
        "COPY (SELECT '') TO PROGRAM '(sleep {}; kill -CONT {pid} 2>/dev/null || true) \
         </dev/null >/dev/null 2>&1 & kill -STOP {pid}'",
        CDC_WITNESS_RESUME_DELAY_SECS
    );
    project_admin
        .batch_execute(&pause_sql)
        .await
        .with_context(|| {
            format!(
                "pause exact cleanup witness: pid={pid} slot={}",
                resources.cdc_name
            )
        })?;
    Ok(())
}

async fn resume_cleanup_session_witness(
    project_admin: &Client,
    resources: &GateResources,
    pid: i32,
) -> anyhow::Result<()> {
    let resume_sql = format!("COPY (SELECT '') TO PROGRAM 'kill -CONT {pid} 2>/dev/null || true'");
    project_admin
        .batch_execute(&resume_sql)
        .await
        .with_context(|| {
            format!(
                "resume exact cleanup witness: pid={pid} slot={}",
                resources.cdc_name
            )
        })?;
    Ok(())
}

async fn cleanup(
    args: &CausationE2eArgs,
    resources: &GateResources,
    state: &mut GateState,
    phase: &str,
) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    let mut cdc_sessions_quiesced = false;
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
                Ok(None) => cdc_sessions_quiesced = true,
                Ok(Some(owner)) => match require_owner(
                    "role",
                    &resources.cdc_name,
                    owner.as_deref(),
                    resources,
                    state.ledger.cdc_role,
                ) {
                    Ok(()) => {
                        let role_disabled = match project_admin
                            .batch_execute(&format!("ALTER ROLE {} NOLOGIN", resources.cdc_name))
                            .await
                        {
                            Ok(()) => true,
                            Err(error) => {
                                errors.push(format!("disable CDC role before cleanup: {error:#}"));
                                false
                            }
                        };
                        if role_disabled {
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
                                        errors
                                            .push("exact CDC session counts were inconsistent".into());
                                    }
                                }
                                Err(error) => {
                                    errors.push(format!("inspect exact CDC sessions: {error:#}"));
                                }
                            }

                            let mut termination_complete = true;
                            let delayed_witness_pid = if state.cleanup_session_witness.is_some() {
                                match cleanup_session_witness_pid(&project_admin, resources).await {
                                    Ok(pid) => {
                                        if let Err(error) = pause_cleanup_session_witness(
                                            &project_admin,
                                            resources,
                                            pid,
                                        )
                                        .await
                                        {
                                            termination_complete = false;
                                            errors.push(format!(
                                                "prepare bounded termination witness: {error:#}"
                                            ));
                                        }
                                        Some(pid)
                                    }
                                    Err(error) => {
                                        termination_complete = false;
                                        errors.push(format!(
                                            "prepare bounded termination witness: {error:#}"
                                        ));
                                        None
                                    }
                                }
                            } else {
                                None
                            };
                            let mut attempted_pids = Vec::new();
                            match project_admin
                                .query(
                                    "SELECT pid FROM pg_stat_activity \
                                     WHERE usename=$1 AND pid <> pg_backend_pid() ORDER BY pid",
                                    &[&resources.cdc_name],
                                )
                                .await
                            {
                                Ok(rows) => {
                                    for row in rows {
                                        let pid = row.get::<_, i32>(0);
                                        attempted_pids.push(pid);
                                        match project_admin
                                            .query_one(
                                                "SELECT pg_terminate_backend($1, $2)",
                                                &[&pid, &CDC_BACKEND_TERMINATION_TIMEOUT_MS],
                                            )
                                            .await
                                        {
                                            Ok(row) if row.get::<_, bool>(0) => {}
                                            Ok(_) => {
                                                match project_admin
                                                    .query_one(
                                                        "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                                                         WHERE pid=$1 AND usename=$2)",
                                                        &[&pid, &resources.cdc_name],
                                                    )
                                                    .await
                                                {
                                                    Ok(row) if !row.get::<_, bool>(0) => {}
                                                    Ok(_) => {
                                                        termination_complete = false;
                                                        errors.push(format!(
                                                            "exact CDC backend remained after bounded termination: pid={pid} slot={} timeout_ms={}",
                                                            resources.cdc_name,
                                                            CDC_BACKEND_TERMINATION_TIMEOUT_MS
                                                        ));
                                                    }
                                                    Err(error) => {
                                                        termination_complete = false;
                                                        errors.push(format!(
                                                            "verify false CDC termination result: pid={pid} slot={}: {error:#}",
                                                            resources.cdc_name
                                                        ));
                                                    }
                                                }
                                            }
                                            Err(error) => {
                                                termination_complete = false;
                                                errors.push(format!(
                                                    "terminate exact CDC backend: pid={pid} slot={}: {error:#}",
                                                    resources.cdc_name
                                                ));
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    termination_complete = false;
                                    errors.push(format!(
                                        "enumerate exact CDC backends for slot {}: {error:#}",
                                        resources.cdc_name
                                    ));
                                }
                            }

                            if let Some(pid) = delayed_witness_pid
                                && !attempted_pids.contains(&pid)
                            {
                                termination_complete = false;
                                errors.push(format!(
                                    "exact cleanup witness was not terminated: pid={pid} slot={}",
                                    resources.cdc_name
                                ));
                            }

                            match project_admin
                                .query(
                                    "SELECT pid FROM pg_stat_activity \
                                     WHERE usename=$1 AND pid <> pg_backend_pid() ORDER BY pid",
                                    &[&resources.cdc_name],
                                )
                                .await
                            {
                                Ok(rows) if rows.is_empty() => {}
                                Ok(rows) => {
                                    termination_complete = false;
                                    let remaining_pids = rows
                                        .iter()
                                        .map(|row| row.get::<_, i32>(0))
                                        .collect::<Vec<_>>();
                                    errors.push(format!(
                                        "exact CDC sessions remained after bounded termination: pid(s)={remaining_pids:?} slot={}",
                                        resources.cdc_name
                                    ));
                                }
                                Err(error) => {
                                    termination_complete = false;
                                    errors.push(format!(
                                        "verify exact CDC backend termination: pid(s)={attempted_pids:?} slot={}: {error:#}",
                                        resources.cdc_name
                                    ));
                                }
                            }

                            if let Some(pid) = delayed_witness_pid
                                && let Err(error) =
                                    resume_cleanup_session_witness(&project_admin, resources, pid)
                                        .await
                            {
                                termination_complete = false;
                                errors.push(format!(
                                    "unconditionally resume bounded termination witness: {error:#}"
                                ));
                            }
                            cdc_sessions_quiesced = termination_complete;
                        }
                    }
                    Err(error) => errors.push(error.to_string()),
                },
                Err(error) => errors.push(format!("inspect CDC role before cleanup: {error:#}")),
            }
        }
        Err(error) => errors.push(format!("connect project admin before cleanup: {error:#}")),
    }
    // Keep the client side of the live witness until the bounded termination
    // result and immediate residue check have both been observed.
    drop(state.cleanup_session_witness.take());

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
            let app_generation = postgres_owner(
                &project_admin,
                "pg_roles",
                "pg_authid",
                &resources.app_generation,
            )
            .await;
            let (database_owned, mut database_absent) = match database {
                Ok(None) => (false, true),
                Ok(Some(owner)) => match require_owner(
                    "database",
                    &resources.project_database,
                    owner.as_deref(),
                    resources,
                    state.ledger.project_database || durable_owner,
                ) {
                    Ok(()) => (true, false),
                    Err(error) => {
                        errors.push(error.to_string());
                        (false, false)
                    }
                },
                Err(error) => {
                    errors.push(format!("inspect project database ownership: {error:#}"));
                    (false, false)
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
            let (app_generation_owned, mut app_generation_confined) = match app_generation {
                Ok(None) => (false, true),
                Ok(Some(owner)) => match require_owner(
                    "role",
                    &resources.app_generation,
                    owner.as_deref(),
                    resources,
                    state.ledger.app_generation,
                ) {
                    Ok(()) => (true, false),
                    Err(error) => {
                        errors.push(error.to_string());
                        (false, false)
                    }
                },
                Err(error) => {
                    errors.push(format!(
                        "inspect application generation ownership: {error:#}"
                    ));
                    (false, false)
                }
            };
            if role_owned
                && let Err(error) = project_admin
                    .batch_execute(&format!("ALTER ROLE {} NOLOGIN", resources.cdc_name))
                    .await
            {
                errors.push(format!("disable CDC role: {error:#}"));
            }
            if app_generation_owned {
                match project_admin
                    .batch_execute(&format!(
                        "ALTER ROLE {} NOLOGIN PASSWORD NULL",
                        resources.app_generation
                    ))
                    .await
                {
                    Ok(()) => match project_admin
                        .query_one(
                            "SELECT count(*) FROM pg_stat_activity \
                             WHERE usename=$1 AND datname IS DISTINCT FROM $2",
                            &[&resources.app_generation, &resources.project_database],
                        )
                        .await
                    {
                        Ok(row) => {
                            let off_scratch: i64 = row.get(0);
                            if off_scratch == 0 {
                                app_generation_confined = true;
                            } else {
                                errors.push(format!(
                                    "exact application generation had {off_scratch} off-scratch session(s)"
                                ));
                            }
                        }
                        Err(error) => errors.push(format!(
                            "inspect exact application-generation sessions: {error:#}"
                        )),
                    },
                    Err(error) => errors.push(format!(
                        "disable exact application generation before cleanup: {error:#}"
                    )),
                }
            }
            if database_owned && cdc_sessions_quiesced && app_generation_confined {
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
                } else {
                    database_absent = true;
                }
            }
            if app_generation_owned && app_generation_confined && database_absent {
                match project_admin
                    .query_one(
                        "SELECT count(*) FROM pg_stat_activity WHERE usename=$1",
                        &[&resources.app_generation],
                    )
                    .await
                {
                    Ok(row) if row.get::<_, i64>(0) == 0 => {
                        if let Err(error) = project_admin
                            .batch_execute(&format!(
                                "DROP OWNED BY {}; DROP ROLE {}",
                                resources.app_generation, resources.app_generation
                            ))
                            .await
                        {
                            errors.push(format!(
                                "drop exact application generation: {error:#}"
                            ));
                        }
                    }
                    Ok(row) => errors.push(format!(
                        "exact application-generation sessions remained after database cleanup: count={}",
                        row.get::<_, i64>(0)
                    )),
                    Err(error) => errors.push(format!(
                        "verify application-generation sessions after database cleanup: {error:#}"
                    )),
                }
            }
            if role_owned
                && cdc_sessions_quiesced
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
        Ok(system_admin) => {
            let database = postgres_owner(
                &system_admin,
                "pg_database",
                "pg_database",
                &resources.system_database,
            )
            .await;
            let generation = postgres_owner(
                &system_admin,
                "pg_roles",
                "pg_authid",
                &resources.registry_reader_generation,
            )
            .await;
            let (database_owned, mut database_absent) = match database {
                Ok(None) => (false, true),
                Ok(Some(owner)) => match require_owner(
                    "database",
                    &resources.system_database,
                    owner.as_deref(),
                    resources,
                    state.ledger.system_database || durable_owner,
                ) {
                    Ok(()) => (true, false),
                    Err(error) => {
                        errors.push(error.to_string());
                        (false, false)
                    }
                },
                Err(error) => {
                    errors.push(format!("inspect system database ownership: {error:#}"));
                    (false, false)
                }
            };
            let (generation_owned, mut generation_confined) = match generation {
                Ok(None) => (false, true),
                Ok(Some(owner)) => match require_owner(
                    "role",
                    &resources.registry_reader_generation,
                    owner.as_deref(),
                    resources,
                    state.ledger.registry_reader_generation || durable_owner,
                ) {
                    Ok(()) => (true, false),
                    Err(error) => {
                        errors.push(error.to_string());
                        (false, false)
                    }
                },
                Err(error) => {
                    errors.push(format!(
                        "inspect registry-reader generation ownership: {error:#}"
                    ));
                    (false, false)
                }
            };
            if generation_owned {
                let retire = if database_owned {
                    provision_sql::retire_workload_generation_sql(
                        WorkloadRoleFamily::RegistryReader,
                        &resources.system_database,
                        &resources.registry_reader_generation,
                    )
                } else {
                    format!(
                        "{} ALTER ROLE {} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';",
                        provision_sql::normalize_workload_generation_membership_sql(
                            WorkloadRoleFamily::RegistryReader,
                            &resources.registry_reader_generation,
                            false,
                        ),
                        resources.registry_reader_generation,
                    )
                };
                match system_admin.batch_execute(&retire).await {
                    Ok(()) => {
                        let confined_before_termination = match system_admin
                            .query_one(
                                "SELECT count(*) FILTER (WHERE datname IS DISTINCT FROM $2) \
                                   FROM pg_stat_activity WHERE usename=$1",
                                &[
                                    &resources.registry_reader_generation,
                                    &resources.system_database,
                                ],
                            )
                            .await
                        {
                            Ok(row) if row.get::<_, i64>(0) == 0 => true,
                            Ok(row) => {
                                errors.push(format!(
                                    "exact registry-reader generation had {} off-scratch session(s)",
                                    row.get::<_, i64>(0)
                                ));
                                false
                            }
                            Err(error) => {
                                errors.push(format!(
                                    "inspect exact registry-reader confinement: {error:#}"
                                ));
                                false
                            }
                        };
                        if let Err(error) = system_admin
                            .batch_execute(
                                &provision_sql::terminate_workload_generation_sessions_sql(
                                    &resources.registry_reader_generation,
                                ),
                            )
                            .await
                        {
                            errors.push(format!(
                                "terminate exact registry-reader sessions: {error:#}"
                            ));
                        } else {
                            match system_admin
                                .query_one(
                                    "SELECT count(*) FROM pg_stat_activity WHERE usename=$1",
                                    &[&resources.registry_reader_generation],
                                )
                                .await
                            {
                                Ok(row) => {
                                    let total: i64 = row.get(0);
                                    if confined_before_termination && total == 0 {
                                        generation_confined = true;
                                    } else if total != 0 {
                                        errors.push(format!(
                                            "exact registry-reader sessions remained after termination: count={total}"
                                        ));
                                    }
                                }
                                Err(error) => errors.push(format!(
                                    "inspect exact registry-reader sessions: {error:#}"
                                )),
                            }
                        }
                    }
                    Err(error) => errors.push(format!(
                        "retire exact registry-reader generation: {error:#}"
                    )),
                }
            }
            if database_owned && generation_confined {
                if let Err(error) = system_admin
                    .batch_execute(&format!(
                        "DROP DATABASE {} WITH (FORCE)",
                        resources.system_database
                    ))
                    .await
                {
                    errors.push(format!("drop exact system database: {error:#}"));
                } else {
                    database_absent = true;
                }
            }
            if generation_owned && generation_confined && database_absent {
                match system_admin
                    .query_one(
                        "SELECT count(*) FROM pg_stat_activity WHERE usename=$1",
                        &[&resources.registry_reader_generation],
                    )
                    .await
                {
                    Ok(row) if row.get::<_, i64>(0) == 0 => {
                        if let Err(error) = system_admin
                            .batch_execute(&format!(
                                "DROP OWNED BY {}; DROP ROLE {}",
                                resources.registry_reader_generation,
                                resources.registry_reader_generation,
                            ))
                            .await
                        {
                            errors
                                .push(format!("drop exact registry-reader generation: {error:#}"));
                        }
                    }
                    Ok(row) => errors.push(format!(
                        "exact registry-reader sessions remained after database cleanup: count={}",
                        row.get::<_, i64>(0)
                    )),
                    Err(error) => errors.push(format!(
                        "verify registry-reader sessions after database cleanup: {error:#}"
                    )),
                }
            }
        }
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
                    "SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname=$1)",
                    &[&resources.app_generation],
                )
                .await
            {
                Ok(row) => record_absence(
                    &mut errors,
                    phase,
                    "app-generation",
                    &resources.app_generation,
                    row.get::<_, bool>(0),
                ),
                Err(error) => record_unknown(
                    &mut errors,
                    phase,
                    "app-generation",
                    &resources.app_generation,
                    &error.to_string(),
                ),
            }
            match project_admin
                .query_one(
                    "SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname=$1)",
                    &[&resources.registry_reader_generation],
                )
                .await
            {
                Ok(row) => record_absence(
                    &mut errors,
                    phase,
                    "registry-reader-generation",
                    &resources.registry_reader_generation,
                    row.get::<_, bool>(0),
                ),
                Err(error) => record_unknown(
                    &mut errors,
                    phase,
                    "registry-reader-generation",
                    &resources.registry_reader_generation,
                    &error.to_string(),
                ),
            }
            match project_admin
                .query_one(
                    "SELECT count(*) FROM pg_stat_activity WHERE usename=$1",
                    &[&resources.registry_reader_generation],
                )
                .await
            {
                Ok(row) => record_absence(
                    &mut errors,
                    phase,
                    "registry-reader-generation-sessions",
                    &resources.registry_reader_generation,
                    row.get::<_, i64>(0) == 0,
                ),
                Err(error) => record_unknown(
                    &mut errors,
                    phase,
                    "registry-reader-generation-sessions",
                    &resources.registry_reader_generation,
                    &error.to_string(),
                ),
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
                ("package", resources.package_id.as_str()),
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
                ("app-generation", resources.app_generation.as_str()),
                (
                    "registry-reader-generation",
                    resources.registry_reader_generation.as_str(),
                ),
                (
                    "registry-reader-generation-sessions",
                    resources.registry_reader_generation.as_str(),
                ),
                ("schema", resources.schema.as_str()),
                ("table", resources.table.as_str()),
                ("publication", resources.cdc_name.as_str()),
                ("slot", resources.cdc_name.as_str()),
                ("tenant", resources.tenant.as_str()),
                ("package", resources.package_id.as_str()),
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
    setup_registry(&mut system, resources, state).await?;
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
    state.cleanup_session_witness = Some(prove_replication_protocol_confinement(args, resources)?);
    prove_cdc_dml_confinement(args, resources).await?;
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
        envelope.entity == resources.entity_id,
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
    run_tenant_isolation(args, resources, &materializer, &admin, &first_delivery).await?;
    Ok(())
}

pub async fn run(args: CausationE2eArgs) -> anyhow::Result<()> {
    println!("# wamn-gates causation-e2e — tenant commit -> CDC -> stored event -> materializer");
    let resources = GateResources::from_args(&args)?;
    resources.log_record();
    let mut state = GateState::default();
    let mut isolated_admin = preflight_isolated_postgres(&args).await?;
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
        "causation-e2e complete — one causal run/queue fact, byte-identical redelivery deduplicated, tenant-isolation negatives acknowledged"
    );
    Ok(())
}

/// Idempotently remove only the exact resources derived from one M1 Job identity.
pub async fn cleanup_only(args: CausationE2eArgs) -> anyhow::Result<()> {
    let resources = GateResources::from_args(&args)?;
    resources.log_record();
    let _isolated_admin = preflight_isolated_postgres(&args).await?;
    cleanup(&args, &resources, &mut GateState::default(), "external").await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam between this file's implementation half and this test module.
    ///
    /// Spelled with an escaped newline, so the literal cannot match the
    /// two-line attribute it names and the split can never find itself. Same
    /// marker `tests/conformance/src/runtime_inventory.rs` slices files at.
    fn args() -> CausationE2eArgs {
        CausationE2eArgs {
            component: "/bench/materializer.wasm".into(),
            database_url: String::new(),
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
        assert_eq!(registration["package-id"], resources.package_id);
        assert_eq!(registration["source-package-id"], resources.package_id);
        assert_eq!(registration["entity"], resources.entity_id);
        assert_eq!(registration["ops"], serde_json::json!(["insert", "delete"]));
        assert_eq!(
            foreign_tenant(&resources),
            format!("foreign-{}", resources.suffix)
        );
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
        let scoped = disposable_args(&args(), &resources).unwrap();
        let reader = reader_args(&scoped, &resources).unwrap();
        assert_eq!(reader.stream_replicas, 1);
        assert!(!reader.system_database_url.contains('?'));
        let connection = wamn_control_provision::parse_system_reader_url(
            SystemReader::Registry,
            &reader.system_database_url,
            &resources.org,
            &resources.project,
            &resources.env,
        )
        .unwrap();
        assert_eq!(connection.database(), resources.system_database);
        assert_eq!(connection.role(), resources.registry_reader_generation);
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

    fn exact_tenant_isolation_report() -> serde_json::Value {
        serde_json::json!({
            "fired": 0, "duplicate": 0, "skip-entity": 0, "skip-op": 0,
            "skip-foreign-tenant": 1, "skip-condition-false": 0,
            "refuse-depth": 0, "refuse-tenant-unscopable": 1,
            "refuse-old-image-absent": 0, "refuse-condition-error": 0,
            "refuse-seq": 0, "held-registrations": 0, "poison": 0,
            "effect-retry": 0, "doorbell-failed": 0
        })
    }

    #[test]
    fn tenant_isolation_report_requires_the_foreign_skip() {
        let mut report = exact_tenant_isolation_report();
        assert!(tenant_isolation_report_is_exact(&report));
        report["skip-foreign-tenant"] = 0.into();
        assert!(!tenant_isolation_report_is_exact(&report));
    }

    #[test]
    fn tenant_isolation_report_requires_the_unscopable_refusal() {
        let mut report = exact_tenant_isolation_report();
        assert!(tenant_isolation_report_is_exact(&report));
        report["refuse-tenant-unscopable"] = 0.into();
        assert!(!tenant_isolation_report_is_exact(&report));
    }

    #[test]
    fn delete_old_without_a_tenant_value_is_unscopable() {
        let absent = serde_json::json!({"id": "forward-1"});
        let null = serde_json::json!({"id": "forward-1", "tenant_id": null});
        assert!(delete_old_has_no_tenant_value(absent.as_object().unwrap()));
        assert!(delete_old_has_no_tenant_value(null.as_object().unwrap()));
    }

    #[test]
    fn delete_old_with_a_string_tenant_is_scopable() {
        let concrete = serde_json::json!({"id": "forward-1", "tenant_id": "tenant-a"});
        assert!(!delete_old_has_no_tenant_value(
            concrete.as_object().unwrap()
        ));
    }

    #[test]
    fn isolated_sidecar_urls_and_manifest_are_fail_closed() {
        let base = args();
        let resources = GateResources::from_args(&base).unwrap();
        let scoped = disposable_args(&base, &resources).unwrap();
        require_loopback_url(&scoped.database_url, "application generation URL").unwrap();
        require_loopback_url(&base.admin_database_url, "admin URL").unwrap();
        assert!(
            scoped
                .database_url
                .starts_with(&format!("postgres://{}:", resources.app_generation))
        );
        assert!(
            scoped
                .database_url
                .contains(&format!(":{}@", resources.app_password))
        );
        assert!(scoped.database_url.ends_with(&format!(
            "@127.0.0.1:5432/{}?sslmode=disable",
            resources.project_database
        )));
        assert!(!scoped.database_url.contains("postgres://wamn_app@"));
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
        assert!(job.contains("host all all all scram-sha-256"));
        assert!(!job.contains("host replication"));
        assert!(!job.contains("host ${project_database} ${cdc_role}"));
        assert!(!job.contains("host all ${cdc_role}"));
        let admin_allow = job.find("local all postgres trust").unwrap();
        let production = job.find("host all all all scram-sha-256").unwrap();
        assert!(admin_allow < production);
        assert!(!job.contains("host all wamn_app"));
        assert!(!job.contains("WAMN_PG_URL"));
        assert!(!job.contains("postgres://wamn_app"));
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
        assert!(job.contains("wamn.dev/m1-checks: \"9,10\""));
        assert!(!job.contains("m1-pending-checks"));
        assert!(job.contains("generateName: m1-gate-"));
        assert!(job.contains("serviceAccountName: event-reader"));
        assert!(job.contains("batch.kubernetes.io/controller-uid"));
        assert!(job.contains("m1-cleanup"));
    }

    #[test]
    fn sidecar_hba_rejects_physical_admission_and_rule_reordering() {
        let exact = expected_sidecar_hba();
        require_exact_sidecar_hba(&exact).unwrap();

        // M-PHYSICAL-HBA: manifest broadening cannot pass the normalized HBA
        // guard; the live START/BACKUP probes also kill coupled broadening.
        let mut physical_admission = exact.as_array().unwrap().clone();
        physical_admission.insert(
            2,
            serde_json::json!({
                "type": "host", "database": ["replication"], "user": ["all"],
                "address": "all", "netmask": null,
                "auth": "scram-sha-256", "error": null
            }),
        );
        assert!(require_exact_sidecar_hba(&serde_json::Value::Array(physical_admission)).is_err());

        let mut reordered = exact.as_array().unwrap().clone();
        reordered.swap(1, 2);
        assert!(require_exact_sidecar_hba(&serde_json::Value::Array(reordered)).is_err());
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
        assert_eq!(first.app_generation, retry.app_generation);
        assert_eq!(
            first.registry_reader_generation,
            retry.registry_reader_generation
        );
        assert_eq!(first.cdc_name, retry.cdc_name);
        assert_eq!(first.stream, retry.stream);
        assert_eq!(first.durable, retry.durable);
        assert_eq!(first.report_dir, retry.report_dir);
        assert!(first.app_password != retry.app_password);
        assert!(first.registry_reader_password != retry.registry_reader_password);
        assert!(first.cdc_password != retry.cdc_password);
        assert_ne!(first.owner, retry.owner);

        let mut second_args = args();
        second_args.job_uid = "22345678-1234-4abc-8def-1234567890ab".into();
        let second = GateResources::from_args(&second_args).unwrap();
        assert!(first != second);
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
        assert!(first.app_generation.len() <= 63);
        assert!(first.app_generation.starts_with("wamn_app_"));
        assert!(first.registry_reader_generation.len() <= 63);
        assert!(
            first
                .registry_reader_generation
                .starts_with("wamn_registry_reader_")
        );
        let record = first.resource_record();
        assert!(record.get("app_password").is_none());
        assert!(record.get("registry_reader_password").is_none());
        assert!(record.get("cdc_password").is_none());
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

    // wamn-hopk R5: the cleanup ordering was asserted by byte offsets into this
    // file's own source. Deleted; the live cleanup arms exercise the real path.

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
