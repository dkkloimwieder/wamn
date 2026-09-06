//! The disposable development environment the twelve-stage `wamn dev` loop runs
//! against.
//!
//! `wamn dev` was provable before it was startable: every value its strict
//! configuration needs — five credential URLs, the verification database, the
//! Gate and its bearer token — only existed inside the live proof. This module
//! is the argument-building layer over the platform verbs that mint them, so
//! `[WAMN-DEV-LIVE]`, `[RECEIVING-ROUTE-JOURNEY]` and the `wamn dev up`
//! operator command stand up one environment by one path (wamn-10yt.10.32).
//!
//! It lives in the product crate rather than in the proof crate because it
//! imports nothing test-only, and because a product command that starts its own
//! environment cannot reach into a test crate to build its configuration. Three
//! of its five `wamn` imports were already this crate's, so the move removed
//! dependency edges rather than adding any (wamn-10yt.10.32).
//!
//! The verbs underneath are the shared truth. Nothing here reimplements
//! provisioning; it only names the arguments and the order.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use reqwest::Url;
use serde_json::Value;
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::{
    CONTROL_PORTABLE_STORE_SQL, CredentialGeneration, SYSTEM_SCHEMA_SQL, WorkloadRoleFamily,
    management_admitter_generation_role, sql as provision_sql,
};

use crate::dev::activation::DevActivationIdentity;
use crate::provision_org::{self, ProvisionOrgArgs, TemplateArg};
use crate::provision_project_env::{
    self, ProvisionProjectEnvArgs, WorkloadActionVerb, WorkloadGenerationAction,
    WorkloadGenerationArgs,
};
use crate::reconcile_run_plane::{self, ReconcileRunPlaneArgs};

/// The deployment-owned inputs a standing development environment needs.
///
/// The live gate reads them from its harness environment and the operator
/// command takes them as flags; neither builds a second set of arguments.
#[derive(Debug)]
pub struct DevEnvironmentInputs {
    pub host_binary: PathBuf,
    pub nats_url: String,
    pub tempo_query_url: String,
    pub otel_exporter_otlp_endpoint: String,
    pub flow_http_workload_image: String,
    pub component_artifact_base: String,
    pub release_artifact_base: String,
    pub route_host: String,
    pub registry_auth_file: PathBuf,
    pub package_sources: Vec<PathBuf>,
}

/// Everything the strict `wamn dev` configuration is written from.
#[expect(
    missing_debug_implementations,
    reason = "carries minted PATs and password-bearing URLs; no derived formatter may print them"
)]
pub struct DevEnvironment {
    pub route: ProvisionedRoute,
    pub credentials: JourneyCredentials,
    pub verification: DevVerificationGate,
    pub identity: DevActivationIdentity,
}

/// Stand the environment up on a disposable PostgreSQL 18 cluster.
///
/// `root` holds the emitted Secrets and SQL and must survive the run: the
/// configuration written from it outlives the process that writes it.
pub async fn provision(
    system_url: &str,
    admin: &Client,
    root: &Path,
) -> anyhow::Result<DevEnvironment> {
    let version: i32 = admin
        .query_one("SHOW server_version_num", &[])
        .await
        .context("read PostgreSQL version")?
        .get::<_, String>(0)
        .parse()
        .context("parse PostgreSQL version")?;
    anyhow::ensure!(
        version >= 180_000,
        "the development environment requires PostgreSQL 18 or newer"
    );

    provision_journey_control(system_url, admin).await?;
    let route = provision_route(
        system_url,
        admin,
        root,
        Some(&root.join("management-author-pat.json")),
    )
    .await?;

    // Only the platform floor. The product command remains the sole owner of
    // both package migrations and their generated ACL union.
    let (project, project_task) = connect(&route.database_url).await?;
    install_journey_platform_floor(project.as_ref()).await?;
    drop(project);
    project_task.abort();

    reconcile_journey_run_plane(system_url, &route.database_url).await?;
    let credentials =
        prepare_journey_credentials(system_url, &route.database_url, root, root, "wamn-system")
            .await?;
    let identity = dev_activation_identity();
    let verification = prepare_dev_verification_gate(system_url, admin, &identity).await?;

    Ok(DevEnvironment {
        route,
        credentials,
        verification,
        identity,
    })
}

pub const ORG: &str = "acme";

pub const PROJECT: &str = "receiving";

pub const ENVIRONMENT: &str = "dev";

pub const TENANT: &str = "receiving-route-auth";

pub const RELEASE_ID: u32 = 1;

#[expect(
    missing_debug_implementations,
    reason = "carries minted PATs and password-bearing URLs; no derived formatter may print them"
)]
pub struct ProvisionedRoute {
    pub database_url: String,
    pub token: String,
    pub token_prefix: String,
    pub principal_subject: String,
    pub management_token: Option<String>,
    pub management_principal_subject: Option<String>,
}

pub async fn connect(url: &str) -> anyhow::Result<(Arc<Client>, tokio::task::JoinHandle<()>)> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to disposable PostgreSQL")?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((Arc::new(client), task))
}

fn database_url(admin_url: &str, database: &str) -> anyhow::Result<String> {
    let mut url = Url::parse(admin_url).context("parse disposable PostgreSQL URL")?;
    url.set_path(&format!("/{database}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

fn provisioning_args(
    system_url: &str,
    root: &Path,
    route_secret: &Path,
    management_secret: Option<&Path>,
) -> ProvisionProjectEnvArgs {
    ProvisionProjectEnvArgs {
        org: Some(ORG.to_owned()),
        project: Some(PROJECT.to_owned()),
        env: Some(ENVIRONMENT.to_owned()),
        tenant: Some(TENANT.to_owned()),
        system_database_url: Some(system_url.to_owned()),
        cluster: Some("route-auth-pg18".to_owned()),
        connection_limit: None,
        app_password: Some("unused-legacy-secret".to_owned()),
        app_host: Some("route-auth-pg18.invalid".to_owned()),
        app_port: 5432,
        namespace: "wamn-system".to_owned(),
        secret_namespace: None,
        target_admin_database_url: None,
        workload: WorkloadGenerationArgs::default(),
        emit_database: Some(root.join("database.json")),
        emit_role_sql: Some(root.join("roles.sql")),
        emit_privilege_sql: Some(root.join("privileges.sql")),
        emit_secret: Some(root.join("database-secret.json")),
        emit_management_author_pat_secret: management_secret.map(Path::to_path_buf),
        emit_route_caller_pat_secret: Some(route_secret.to_path_buf()),
        revoke_pat_prefix: None,
    }
}

pub fn generation_args(
    family: WorkloadRoleFamily,
    system_url: &str,
    target_admin_url: Option<&str>,
    secret: &Path,
) -> ProvisionProjectEnvArgs {
    ProvisionProjectEnvArgs {
        org: Some(ORG.to_owned()),
        project: Some(PROJECT.to_owned()),
        env: Some(ENVIRONMENT.to_owned()),
        tenant: Some(TENANT.to_owned()),
        system_database_url: Some(system_url.to_owned()),
        cluster: None,
        connection_limit: None,
        app_password: None,
        app_host: None,
        app_port: 5432,
        namespace: "wamn-system".to_owned(),
        secret_namespace: None,
        target_admin_database_url: target_admin_url.map(str::to_owned),
        workload: WorkloadGenerationArgs {
            action: Some(WorkloadGenerationAction {
                family,
                verb: WorkloadActionVerb::Prepare,
                generation: CredentialGeneration::A,
            }),
            secret: Some((family, secret.to_path_buf())),
        },
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
        emit_management_author_pat_secret: None,
        emit_route_caller_pat_secret: None,
        revoke_pat_prefix: None,
    }
}

pub fn read_json(path: &Path) -> anyhow::Result<Value> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

pub fn secret_value(path: &Path, key: &str) -> anyhow::Result<String> {
    read_json(path)?["stringData"][key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} carries stringData.{key}", path.display()))
}

fn secret_annotation(path: &Path, key: &str) -> anyhow::Result<String> {
    read_json(path)?["metadata"]["annotations"][key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} carries annotation {key}", path.display()))
}

pub async fn reset_control_store(admin: &Client) -> anyhow::Result<()> {
    let stale_databases = admin
        .query(
            "SELECT datname::text FROM pg_database \
             WHERE datname LIKE 'wamn-db-acme--%--%--%' ORDER BY datname",
            &[],
        )
        .await
        .context("list stale route-auth databases")?;
    for row in stale_databases {
        let database: String = row.get(0);
        admin
            .batch_execute(&provision_sql::drop_database_named_sql(&database))
            .await
            .with_context(|| format!("drop stale database {database}"))?;
    }
    admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS identity CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
                 CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$; \
             DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
                                        current_database()); END $$;",
        )
        .await
        .context("prepare the production control owner")?;
    admin
        .batch_execute(&provision_sql::ensure_control_author_acl_role_sql())
        .await
        .context("ensure the portable store's control-author ACL role")?;
    admin
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("assume the production control owner")?;
    admin
        .batch_execute(SYSTEM_SCHEMA_SQL)
        .await
        .context("install deploy/sql/system-schema.sql")?;
    admin
        .batch_execute(CONTROL_PORTABLE_STORE_SQL)
        .await
        .context("install the control portable store")?;
    admin
        .batch_execute("RESET ROLE")
        .await
        .context("release the production control owner before cluster ACL convergence")?;
    admin
        .batch_execute(provision_sql::revoke_public_connect_floor_sql())
        .await
        .context("converge the cluster PUBLIC CONNECT floor")?;
    admin
        .batch_execute(
            "DO $$ BEGIN EXECUTE format(\
               'REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()); END $$;",
        )
        .await
        .context("converge the control database PUBLIC TEMPORARY floor")?;
    Ok(())
}

pub async fn provision_route(
    system_url: &str,
    admin: &Client,
    root: &Path,
    management_secret: Option<&Path>,
) -> anyhow::Result<ProvisionedRoute> {
    let route_secret = root.join("route-caller-pat.json");
    provision_project_env::run(provisioning_args(
        system_url,
        root,
        &route_secret,
        management_secret,
    ))
    .await
    .context("run production project-environment and route-PAT provisioning")?;

    let database = read_json(&root.join("database.json"))?["spec"]["name"]
        .as_str()
        .context("Database CR carries spec.name")?
        .to_owned();
    admin
        .batch_execute(
            &std::fs::read_to_string(root.join("roles.sql")).context("read emitted role SQL")?,
        )
        .await
        .context("apply emitted role SQL")?;
    admin
        .batch_execute(&wamn_schema_control::ensure_scenario_author_role_sql())
        .await
        .context("ensure the catalog author role")?;
    admin
        .batch_execute(&provision_sql::create_database_named_sql(&database))
        .await
        .context("stand in for the emitted Database CR")?;
    admin
        .batch_execute(
            &std::fs::read_to_string(root.join("privileges.sql"))
                .context("read emitted privilege SQL")?,
        )
        .await
        .context("apply emitted privilege SQL")?;

    Ok(ProvisionedRoute {
        database_url: database_url(system_url, &database)?,
        token: secret_value(&route_secret, "token")?,
        token_prefix: secret_annotation(&route_secret, "wamn.io/pat-prefix")?,
        principal_subject: secret_annotation(&route_secret, "wamn.io/principal-subject")?,
        management_token: management_secret
            .map(|secret| secret_value(secret, "token"))
            .transpose()?,
        management_principal_subject: management_secret
            .map(|secret| secret_annotation(secret, "wamn.io/principal-subject"))
            .transpose()?,
    })
}

#[expect(
    missing_debug_implementations,
    reason = "carries minted PATs and password-bearing URLs; no derived formatter may print them"
)]
pub struct JourneyCredentials {
    pub guest_sql: String,
    pub executor_platform: String,
    pub event_materializer: String,
    pub http_admitter: String,
    pub identity_reader: String,
    pub control_author: String,
    pub management_admitter: String,
}

pub async fn provision_journey_control(system_url: &str, admin: &Client) -> anyhow::Result<()> {
    reset_control_store(admin).await?;
    provision_org::run(ProvisionOrgArgs {
        org: ORG.to_owned(),
        template: TemplateArg::Trials,
        pool: "route-auth-pg18".to_owned(),
        system_database_url: Some(system_url.to_owned()),
        emit_clusters: None,
        // The journey org is POOLED, so `provision_org::run` never reaches the
        // dedicated-org arm that reads these. They carry the same `cfg` as the
        // fields themselves, which are `ops`-only (wamn-0h0g.10.20).
        #[cfg(feature = "ops")]
        emit_object_store: None,
        #[cfg(feature = "ops")]
        emit_scheduled_backup: None,
    })
    .await
    .context("stamp the journey org and environment policies through provision-org")
}

pub async fn install_journey_platform_floor(project: &Client) -> anyhow::Result<()> {
    project
        .batch_execute(include_str!("../../../../deploy/sql/catalog-schema.sql"))
        .await
        .context("install the catalog schema")?;
    project
        .batch_execute(include_str!("../../../../deploy/sql/app-schema.sql"))
        .await
        .context("install the application authorization schema")
}

pub async fn reconcile_journey_run_plane(
    system_url: &str,
    project_url: &str,
) -> anyhow::Result<()> {
    reconcile_run_plane::run(ReconcileRunPlaneArgs {
        system_database_url: system_url.to_owned(),
        admin_database_url: project_url.to_owned(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        tenant: TENANT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        schema: "wamn_run".to_owned(),
        dry_run: false,
    })
    .await
    .context("reconcile the journey run plane")
}

pub async fn prepare_journey_credentials(
    system_url: &str,
    project_url: &str,
    root: &Path,
    host_secret_directory: &Path,
    host_secret_namespace: &str,
) -> anyhow::Result<JourneyCredentials> {
    async fn prepare(
        family: WorkloadRoleFamily,
        system_url: &str,
        target_url: Option<&str>,
        root: &Path,
        namespace: &str,
        name: &str,
    ) -> anyhow::Result<String> {
        let secret = root.join(format!("{name}.json"));
        let mut args = generation_args(family, system_url, target_url, &secret);
        args.namespace = namespace.to_owned();
        provision_project_env::run(args)
            .await
            .with_context(|| format!("prepare the production {name} generation"))?;
        secret_value(&secret, "url")
    }

    Ok(JourneyCredentials {
        guest_sql: prepare(
            WorkloadRoleFamily::App,
            system_url,
            Some(project_url),
            host_secret_directory,
            host_secret_namespace,
            "guest-sql",
        )
        .await?,
        executor_platform: prepare(
            WorkloadRoleFamily::ExecutorPlatform,
            system_url,
            Some(project_url),
            host_secret_directory,
            host_secret_namespace,
            "executor-platform",
        )
        .await?,
        event_materializer: prepare(
            WorkloadRoleFamily::EventMaterializer,
            system_url,
            Some(project_url),
            host_secret_directory,
            host_secret_namespace,
            "event-materializer",
        )
        .await?,
        http_admitter: prepare(
            WorkloadRoleFamily::HttpAdmitter,
            system_url,
            Some(project_url),
            host_secret_directory,
            host_secret_namespace,
            "http-admitter",
        )
        .await?,
        identity_reader: prepare(
            WorkloadRoleFamily::IdentityReader,
            system_url,
            None,
            host_secret_directory,
            host_secret_namespace,
            "identity-reader",
        )
        .await?,
        control_author: prepare(
            WorkloadRoleFamily::ControlAuthor,
            system_url,
            None,
            root,
            "wamn-system",
            "control-author",
        )
        .await?,
        management_admitter: prepare(
            WorkloadRoleFamily::ManagementAdmitter,
            system_url,
            Some(project_url),
            root,
            "wamn-system",
            "management-admitter",
        )
        .await?,
    })
}

/// How often readiness retries a connection to the spawned Gate.
const GATE_READINESS_INTERVAL: Duration = Duration::from_millis(250);

/// How many times readiness retries before it refuses and names the port.
///
/// The Gate settles three separate database connections before it listens, so
/// the bound is generous; what matters is that it is bounded.
const GATE_READINESS_ATTEMPTS: u32 = 120;

/// One `wamn-scenario-worker serve` child and the authority it listens on.
///
/// The Gate is a real process, not a task in this one (wamn-10yt.10.32): the
/// environment it serves outlives the command that stood it up, and a proof
/// that links the Gate in-process proves something the operator never runs.
#[derive(Debug)]
pub struct JourneyManagementGate {
    child: tokio::process::Child,
    bind: String,
}

impl JourneyManagementGate {
    /// The `host:port` authority the Gate was asked to listen on.
    ///
    /// It is what the operator named, not what the kernel picked: a fixed port
    /// is the whole reason the written configuration keeps working.
    #[must_use]
    pub fn bind(&self) -> &str {
        &self.bind
    }

    /// Wait for the Gate to exit on its own.
    pub async fn wait(&mut self) -> anyhow::Result<std::process::ExitStatus> {
        self.child
            .wait()
            .await
            .context("wait for the spawned management Gate")
    }

    /// Stop the Gate and reap it.
    pub async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.child
            .kill()
            .await
            .context("stop the spawned management Gate")
    }
}

/// Settle the address the Gate will listen on, before anything is provisioned.
///
/// A fixed nameable port is not a preference. The in-process launch could hand
/// the ephemeral port the kernel picked back to its caller; a spawned child
/// cannot, and the configuration written from that port outlives the process
/// that writes it. Port 0 is therefore a refusal, and it names the input.
pub fn gate_listen_address(bind: &str) -> anyhow::Result<SocketAddr> {
    let address: SocketAddr = bind
        .parse()
        .with_context(|| format!("the management Gate address {bind} is not host:port"))?;
    anyhow::ensure!(
        address.port() != 0,
        "the management Gate needs a fixed nameable port, and {bind} asks the kernel \
         for an ephemeral one: the configuration written from it outlives the process \
         that writes it"
    );
    Ok(address)
}

/// Spawn `wamn-scenario-worker serve` and wait until it accepts a connection.
///
/// Readiness is a bounded TCP connect against the port the caller named. The
/// management surface answers `POST /authoring` and 404s everything else, and
/// an unauthenticated health route added to a production service for a
/// development readiness poll would be a new attack surface bought with a
/// convenience — so the poll observes the listener itself (wamn-10yt.10.32).
///
/// Every credential-carrying input crosses as an environment variable, never as
/// an argument: `/proc/<pid>/cmdline` is world-readable and these values carry
/// passwords, while `/proc/<pid>/environ` is not.
pub async fn spawn_journey_management_gate(
    scenario_worker_binary: &Path,
    credentials: &JourneyCredentials,
    management_admission_database_url: &str,
    bind: &str,
) -> anyhow::Result<JourneyManagementGate> {
    let address = gate_listen_address(bind)?;

    // REFUSE A PORT SOMEONE ELSE HOLDS, BEFORE SPAWNING. Readiness below is a
    // bounded TCP connect, and a connect cannot tell this Gate from a stranger:
    // with the port held, the child dies on `Address already in use` while the
    // first poll still connects to the other listener and reports ready. The
    // run then continues past a dead Gate and fails later, somewhere else
    // (wamn-10yt.10.35).
    match std::net::TcpListener::bind(address) {
        Ok(probe) => drop(probe),
        Err(source) => {
            anyhow::bail!("the management Gate cannot take {bind}: {source}");
        }
    }

    let mut child = tokio::process::Command::new(scenario_worker_binary)
        .arg("serve")
        .env("WAMN_MANAGEMENT_BIND", bind)
        .env("WAMN_SYSTEM_URL", &credentials.identity_reader)
        .env("WAMN_CONTROL_AUTHORING_PG_URL", &credentials.control_author)
        .env(
            "WAMN_MANAGEMENT_ADMISSION_PG_URL",
            management_admission_database_url,
        )
        .env("WAMN_MANAGEMENT_ORG", ORG)
        .env("WAMN_MANAGEMENT_PROJECT", PROJECT)
        .env("WAMN_MANAGEMENT_ENVIRONMENT", ENVIRONMENT)
        .env("WAMN_MANAGEMENT_TENANT", TENANT)
        // A panicking caller must not leave a Gate holding the port.
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "spawn the management Gate from {}",
                scenario_worker_binary.display()
            )
        })?;

    for _ in 0..GATE_READINESS_ATTEMPTS {
        if let Some(status) = child
            .try_wait()
            .context("check whether the spawned management Gate is still running")?
        {
            anyhow::bail!("the management Gate stopped before listening on {bind}: {status}");
        }
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            // A connect proves SOMETHING listens. Re-check the child so a Gate
            // that died between the two reads is never reported ready.
            if let Some(status) = child
                .try_wait()
                .context("check whether the spawned management Gate is still running")?
            {
                anyhow::bail!("the management Gate stopped before listening on {bind}: {status}");
            }
            return Ok(JourneyManagementGate {
                child,
                bind: bind.to_owned(),
            });
        }
        tokio::time::sleep(GATE_READINESS_INTERVAL).await;
    }

    let _ = child.kill().await;
    anyhow::bail!(
        "the management Gate never accepted a connection on {bind} within {} seconds",
        (GATE_READINESS_INTERVAL * GATE_READINESS_ATTEMPTS).as_secs()
    )
}

#[expect(
    missing_debug_implementations,
    reason = "carries minted PATs and password-bearing URLs; no derived formatter may print them"
)]
pub struct DevVerificationGate {
    pub database: String,
    pub database_url: String,
    pub credential_url: String,
    pub generation_roles: [String; 2],
}

pub fn dev_activation_identity() -> DevActivationIdentity {
    let process = std::process::id();
    DevActivationIdentity {
        tenant: TENANT.to_owned(),
        catalog: "default".to_owned(),
        environment: ENVIRONMENT.to_owned(),
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        schema: "receiving".to_owned(),
        host_group: "wamn-dev-receiving".to_owned(),
        host_name: format!("wamn-dev-receiving-{process}"),
        runner: format!("wamn-dev-receiving-{process}"),
    }
}

fn quoted_generated_identifier(identifier: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        identifier
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
        "generated development fixture name is not a safe PostgreSQL identifier"
    );
    Ok(format!("\"{identifier}\""))
}

pub async fn prepare_dev_verification_gate(
    system_url: &str,
    admin: &Client,
    identity: &DevActivationIdentity,
) -> anyhow::Result<DevVerificationGate> {
    let database = format!("wamn_dev_verification_{}", std::process::id());
    admin
        .batch_execute(&provision_sql::drop_database_named_sql(&database))
        .await
        .context("remove stale disposable development verification database")?;
    admin
        .batch_execute(&provision_sql::create_database_named_sql(&database))
        .await
        .context("create the Gate's initial disposable verification database")?;
    let quoted_database = quoted_generated_identifier(&database)?;
    admin
        .batch_execute(&format!(
            "REVOKE CONNECT ON DATABASE {quoted_database} FROM PUBLIC"
        ))
        .await
        .context("revoke PUBLIC CONNECT on only the initial verification database")?;

    let generation_roles = [CredentialGeneration::A, CredentialGeneration::B].map(|generation| {
        management_admitter_generation_role(
            &identity.org,
            &identity.project,
            &identity.environment,
            &database,
            generation,
        )
    });
    for role in &generation_roles {
        let quoted = quoted_generated_identifier(role)?;
        admin
            .batch_execute(&format!(
                "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{role}') \
                 THEN CREATE ROLE {quoted} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 INHERIT NOREPLICATION NOBYPASSRLS; END IF; END $$;"
            ))
            .await
            .with_context(|| format!("ensure inactive generated Gate role {role}"))?;
    }

    let verification_url = database_url(system_url, &database)?;
    let (verification, verification_task) = connect(&verification_url).await?;
    for role in &generation_roles {
        verification
            .batch_execute(&provision_sql::retire_workload_generation_sql(
                WorkloadRoleFamily::ManagementAdmitter,
                &database,
                role,
            ))
            .await
            .with_context(|| format!("reset generated Gate role {role} to inactive"))?;
    }
    drop(verification);
    verification_task.abort();

    crate::dev::verification_world::bootstrap(&verification_url, identity)
        .await
        .context("bootstrap the Gate's initial disposable verification world")?;

    let password = format!("wamn-dev-gate-{}-a", std::process::id());
    let (verification, verification_task) = connect(&verification_url).await?;
    verification
        .batch_execute(&provision_sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::ManagementAdmitter,
            &database,
            &generation_roles[0],
            &password,
            "2099-01-01T00:00:00Z",
        ))
        .await
        .context("prepare the production-shaped verification Gate credential")?;
    drop(verification);
    verification_task.abort();

    let mut credential_url =
        Url::parse(&verification_url).context("parse the disposable verification database URL")?;
    credential_url
        .set_username(&generation_roles[0])
        .map_err(|_| anyhow::anyhow!("set the generated Gate role in its verification URL"))?;
    credential_url
        .set_password(Some(&password))
        .map_err(|_| anyhow::anyhow!("set the generated Gate password in its verification URL"))?;

    Ok(DevVerificationGate {
        database,
        database_url: verification_url,
        credential_url: credential_url.into(),
        generation_roles,
    })
}

pub async fn clean_dev_verification_gate_roles(
    admin: &Client,
    fixture: &DevVerificationGate,
) -> anyhow::Result<()> {
    for role in &fixture.generation_roles {
        let quoted = quoted_generated_identifier(role)?;
        admin
            .batch_execute(&format!(
                "REVOKE \"{}\" FROM {quoted}; DROP ROLE {quoted};",
                WorkloadRoleFamily::ManagementAdmitter.acl_role()
            ))
            .await
            .with_context(|| format!("remove generated Gate role {role}"))?;
    }
    Ok(())
}

pub fn write_dev_config(
    root: &Path,
    system_url: &str,
    route: &ProvisionedRoute,
    credentials: &JourneyCredentials,
    verification: &DevVerificationGate,
    gate_bind: &str,
    inputs: &DevEnvironmentInputs,
    identity: &DevActivationIdentity,
) -> anyhow::Result<PathBuf> {
    let wasmtime_cache = root.join("dev-wasmtime-cache");
    // An operator reuses one environment directory across runs, and a warm
    // compilation cache is the point of keeping it.
    std::fs::create_dir_all(&wasmtime_cache)
        .context("create the product-command Wasmtime cache")?;
    let config = serde_json::json!({
        "verification_database_url": verification.database_url.as_str(),
        "target_database_url": route.database_url.as_str(),
        "system_database_url": system_url,
        "identity_database_url": credentials.identity_reader.as_str(),
        "guest_database_url": credentials.guest_sql.as_str(),
        "executor_platform_database_url": credentials.executor_platform.as_str(),
        "http_admitter_database_url": credentials.http_admitter.as_str(),
        "event_materializer_database_url": credentials.event_materializer.as_str(),
        "scheduler_nats_url": inputs.nats_url.as_str(),
        "event_nats_url": inputs.nats_url.as_str(),
        "tempo_query_url": inputs.tempo_query_url.as_str(),
        "otel_exporter_otlp_endpoint": inputs.otel_exporter_otlp_endpoint.as_str(),
        "component_artifact_base": inputs.component_artifact_base.as_str(),
        "release_artifact_base": inputs.release_artifact_base.as_str(),
        "registry_auth_file": &inputs.registry_auth_file,
        "insecure_registry": true,
        "gate_url": format!("http://{gate_bind}/authoring"),
        "gate_bearer_token": route
            .management_token
            .as_deref()
            .context("project provisioning emitted no management-author PAT")?,
        "route_host": inputs.route_host.as_str(),
        "flow_http_workload_image": inputs.flow_http_workload_image.as_str(),
        "package_sources": inputs.package_sources.as_slice(),
        "effective_release_id": RELEASE_ID,
        "tenant": identity.tenant.as_str(),
        "catalog": identity.catalog.as_str(),
        "environment": identity.environment.as_str(),
        "org": identity.org.as_str(),
        "project": identity.project.as_str(),
        "schema": identity.schema.as_str(),
        "host_group": identity.host_group.as_str(),
        "host_name": identity.host_name.as_str(),
        "runner": identity.runner.as_str(),
        "host_binary": &inputs.host_binary,
        "wasmtime_cache_dir": wasmtime_cache,
    });
    let path = root.join("dev.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)
        .context("write the strict product-command configuration")?;
    Ok(path)
}
