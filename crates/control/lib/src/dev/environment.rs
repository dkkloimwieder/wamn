//! The disposable development environment the ten-stage `wamn dev` loop runs
//! against.
//!
//! `wamn dev` was testable before it was startable: every value its strict
//! configuration needs — five credential URLs, the verification database, the
//! Gate and its bearer token — only existed inside the live test. This module
//! is the argument-building layer over the platform verbs that mint them, so
//! `[WAMN-DEV-LIVE]`, `[RECEIVING-ROUTE-JOURNEY]` and the `wamn dev up`
//! operator command stand up one environment by one path (wamn-10yt.10.32).
//!
//! The control library owns this production path. CLI and test callers share
//! its provisioning operations without importing command-line wrappers.
//!
//! The verbs underneath are the shared truth. Nothing here reimplements
//! provisioning; it only names the arguments and the order.

use std::fmt::Write as _;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;

use crate::pat_client::PatIssuerConfig;
use crate::provision_project_env::{
    self, ProvisionProjectEnvRequest, ProvisionedRoute, WorkloadActionRequest, WorkloadActionVerb,
    WorkloadGenerationAction, read_json, secret_annotation, secret_value,
};
use crate::reconcile_run_plane::{self, ReconcileRunPlaneRequest};
use anyhow::Context as _;
use reqwest::Url;
use tokio_postgres::{Client, Config as PostgresConfig, NoTls};
use wamn_control_provision::{
    CONTROL_PORTABLE_STORE_SQL, CredentialGeneration, SYSTEM_SCHEMA_SQL, WorkloadRoleFamily,
    platform_principals_sql, sql as provision_sql,
};
use wamn_pg_core::Identifier;

use crate::dev::activation::DevActivationIdentity;
use crate::provision_org::{ProvisionOrgRequest, provision_org};
use wamn_control_registry::Template;

/// The deployment-owned inputs a standing development environment needs.
///
/// The live gate reads them from its harness environment and the operator
/// command takes them as flags; neither builds a second set of arguments.
#[derive(Debug)]
pub struct DevEnvironmentInputs {
    pub local_artifacts: super::config::LocalArtifacts,
    pub host_binary: PathBuf,
    pub nats_url: String,
    pub event_nats_url: String,
    pub event_nats_username: String,
    pub event_nats_password_file: PathBuf,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
    pub tempo_query_url: String,
    pub otel_exporter_otlp_endpoint: String,
    pub route_host: String,
    /// Domain of the platform principal emails, `<component>@<platform-domain>`.
    pub platform_domain: String,
    pub package_sources: Vec<PathBuf>,
}

/// Everything the strict `wamn dev` configuration is written from.
#[expect(
    missing_debug_implementations,
    reason = "carries minted PATs and password-bearing URLs; no derived formatter may print them"
)]
pub struct DevEnvironment {
    /// Pristine clone source for every run's target database.
    pub template: String,
    pub route: ProvisionedRoute,
    pub credentials: JourneyCredentials,
    pub identity: DevActivationIdentity,
    pub issuer: super::pat_issuer::Bootstrap,
}

/// Stand the environment up on a disposable PostgreSQL 18 cluster.
///
/// `root` holds the emitted Secrets and SQL and must survive the run: the
/// configuration written from it outlives the process that writes it.
pub async fn provision(
    system_url: &str,
    admin: &Client,
    root: &Path,
    platform_domain: &str,
) -> anyhow::Result<DevEnvironment> {
    wamn_control_provision::validate_platform_domain(platform_domain)?;
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

    anyhow::ensure!(
        !root.join("identity-process.json").exists(),
        "stop the existing development environment before provisioning again"
    );
    provision_journey_control(system_url, admin).await?;
    admin
        .execute(
            "UPDATE registry.meta SET platform_domain = $1",
            &[&platform_domain],
        )
        .await
        .context("record the disposable deployment platform domain")?;
    let route_secret = root.join("route-caller-pat.json");
    let management_secret = root.join("management-author-pat.json");
    let mut args = provisioning_args(system_url, root, &route_secret, Some(&management_secret));
    args.emit_management_author_pat_secret = None;
    args.emit_route_caller_pat_secret = None;
    provision_project_env::provision_project_env(&args).await?;
    let project_url = prepare_route_database(system_url, admin, root).await?;

    // Only the platform floor. The product command remains the sole owner of
    // both package migrations and their generated ACL union.
    let (project, project_task) = connect(&project_url).await?;
    install_journey_platform_floor(project.as_ref(), TENANT, platform_domain).await?;
    drop(project);
    project_task.abort();

    reconcile_journey_run_plane(system_url, &project_url).await?;
    let target_secret = root.join("session-role-reader.json");
    provision_project_env::run_workload_action(&generation_args(
        WorkloadRoleFamily::SessionRoleReader,
        system_url,
        Some(&project_url),
        &target_secret,
    ))
    .await?;
    let target = secret_value(&target_secret, "target.json")?;
    let target_path = root.join("identity-target.json");
    provision_project_env::write_secret_json(&target_path, &serde_json::from_str(&target)?)?;
    let issuer = super::pat_issuer::start_environment(system_url, root, &target_path).await?;
    args.emit_management_author_pat_secret = Some(management_secret.clone());
    args.emit_route_caller_pat_secret = Some(route_secret.clone());
    args.pat_issuer = issuer.args.clone();
    provision_project_env::provision_project_env(&args).await?;
    let route = route_credentials(project_url, root, Some(&management_secret))?;
    let target =
        wamn_control_provision::session_target::SessionTarget::from_json(target.as_bytes())?;
    provision_project_env::write_secret_json(
        &root.join("identity-trust.json"),
        &serde_json::json!({
            "issuer": issuer.args.endpoint,
            "ca": issuer.args.server_ca,
            "instance_suffix": target.instance_suffix(),
        }),
    )?;

    reconcile_journey_run_plane(system_url, &route.database_url).await?;
    let credentials =
        prepare_journey_credentials(system_url, &route.database_url, root, root, "wamn-system")
            .await?;
    // The template is taken HERE, while the project database is provisioned and
    // still pristine: the loop applies package migrations, the standup does not.
    // A clone of this is what every run starts from.
    let identity = dev_activation_identity();
    let template =
        prepare_target_template(admin, system_url, &route.database_url, root, &identity).await?;

    Ok(DevEnvironment {
        template,
        route,
        credentials,
        identity,
        issuer,
    })
}

pub const ORG: &str = "acme";

pub const PROJECT: &str = "receiving";

pub const ENVIRONMENT: &str = "dev";

pub const TENANT: &str = "receiving-route-auth";

pub const RELEASE_ID: u32 = 1;

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
) -> ProvisionProjectEnvRequest {
    ProvisionProjectEnvRequest {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        tenant: Some(TENANT.to_owned()),
        // The development target is disposable, and its registry row is where
        // that is written down (wamn-10yt.38). Admit reads the projection of
        // THIS row, so an author's re-run replaces its own component fact
        // instead of being locked out of a package version by a reformat.
        disposable: true,
        system_database_url: Some(system_url.to_owned()),
        cluster: Some("route-auth-pg18".to_owned()),
        connection_limit: None,
        namespace: "wamn-system".to_owned(),
        secret_namespace: None,
        emit_database: Some(root.join("database.json")),
        emit_role_sql: Some(root.join("roles.sql")),
        emit_privilege_sql: Some(root.join("privileges.sql")),
        emit_secret: root.join("database-secret.json"),
        emit_management_author_pat_secret: management_secret.map(Path::to_path_buf),
        emit_route_caller_pat_secret: Some(route_secret.to_path_buf()),
        pat_issuer: PatIssuerConfig::default(),
    }
}

pub fn generation_args(
    family: WorkloadRoleFamily,
    system_url: &str,
    target_admin_url: Option<&str>,
    secret: &Path,
) -> WorkloadActionRequest {
    WorkloadActionRequest {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        tenant: Some(TENANT.to_owned()),
        system_database_url: Some(system_url.to_owned()),
        target_admin_database_url: target_admin_url.map(str::to_owned),
        namespace: "wamn-system".to_owned(),
        action: WorkloadGenerationAction {
            family,
            verb: WorkloadActionVerb::Prepare,
            generation: CredentialGeneration::A,
        },
        secret: Some(secret.to_path_buf()),
        emit_role_sql: None,
    }
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
        .batch_execute(provision_sql::ensure_db_owner_role_sql())
        .await
        .context("ensure the database-owner role that the record history grants name")?;
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
    let issuer = super::pat_issuer::start(system_url, root).await?;
    let mut args = provisioning_args(system_url, root, &route_secret, management_secret);
    args.pat_issuer = issuer.args.clone();
    let provisioned = provision_project_env::provision_project_env(&args).await;
    let stopped = issuer.stop().await;
    if let Err(error) = provisioned {
        let context = if stopped.is_err() {
            "project-environment provisioning failed and identity bootstrap cleanup also failed"
        } else {
            "run production project-environment and route-PAT provisioning"
        };
        return Err(error.context(context));
    }
    stopped?;

    let database_url = prepare_route_database(system_url, admin, root).await?;
    route_credentials(database_url, root, management_secret)
}

async fn prepare_route_database(
    system_url: &str,
    admin: &Client,
    root: &Path,
) -> anyhow::Result<String> {
    let database = read_json(&root.join("database.json"))?["spec"]["name"]
        .as_str()
        .context("Database CR carries spec.name")?
        .to_owned();
    admin
        .batch_execute(
            std::fs::read_to_string(root.join("roles.sql"))
                .context("read emitted role SQL")?
                .as_str(),
        )
        .await
        .context("apply emitted role SQL")?;
    admin
        .batch_execute(wamn_schema_control::ensure_scenario_author_role_sql())
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

    database_url(system_url, &database)
}

fn route_credentials(
    database_url: String,
    root: &Path,
    management_secret: Option<&Path>,
) -> anyhow::Result<ProvisionedRoute> {
    let route_secret = root.join("route-caller-pat.json");
    Ok(ProvisionedRoute {
        database_url,
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
    super::pat_issuer::preflight(system_url)?;
    reset_control_store(admin).await?;
    provision_org(ProvisionOrgRequest {
        org: ORG.to_owned(),
        template: Template::trials(),
        pool: "route-auth-pg18".to_owned(),
        system_database_url: Some(system_url.to_owned()),
    })
    .await
    .context("stamp the journey org and environment policies through provision-org")
    .map(|_| ())
}

/// Snapshot the pristine project database and capture its database-level ACL.
///
/// `CREATE DATABASE ... TEMPLATE` copies every object and every object-level
/// privilege. It does not copy the ACL of the database itself, so that one
/// thing is read from the live database here, while it is healthy, and written
/// as SQL the loop replays after each clone.
///
/// Reading the ACL beats deriving it. A derived set would be this module's
/// opinion of what the standup granted; `datacl` is what it actually granted.
pub async fn prepare_target_template(
    admin: &Client,
    system_url: &str,
    target_url: &str,
    root: &Path,
    identity: &DevActivationIdentity,
) -> anyhow::Result<String> {
    let target = PostgresConfig::from_str(target_url)
        .context("parse the target database URL")?
        .get_dbname()
        .context("the target database URL names no database")?
        .to_owned();
    let template = format!("{target}--template");

    let acl = render_database_acl(admin, &target).await?;
    std::fs::write(root.join("database-acl.sql"), &acl)
        .context("write the captured database-level ACL")?;

    // CREATE DATABASE ... TEMPLATE refuses while another session is connected to
    // the source, so the copy is taken before the Gate opens.
    admin
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            Identifier::new(template.clone())?.quoted()
        ))
        .await
        .context("drop a previous target template")?;
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {} TEMPLATE {}",
            Identifier::new(template.clone())?.quoted(),
            Identifier::new(target.clone())?.quoted()
        ))
        .await
        .context("snapshot the pristine target database as a template")?;
    admin
        .batch_execute(&format!(
            "REVOKE CONNECT, TEMPORARY ON DATABASE {} FROM PUBLIC",
            Identifier::new(template.clone())?.quoted(),
        ))
        .await
        .context("preserve the cluster PUBLIC CONNECT floor on the template")?;

    // The fingerprint is stamped ON the template, so it cannot be separated
    // from the thing it describes. A run compares it before it drops anything.
    let system_database = super::target_database::database_name(system_url)
        .context("the system database URL names no database")?;
    let fingerprint =
        super::target_database::template_fingerprint(&target, &system_database, identity);
    admin
        .batch_execute(&format!(
            "COMMENT ON DATABASE {} IS {}",
            Identifier::new(template.clone())?.quoted(),
            wamn_pg_core::quote_literal(&fingerprint)
        ))
        .await
        .context("stamp the template with its standup fingerprint")?;
    Ok(template)
}

/// Render the database-level ACL of `database` as replayable SQL.
async fn render_database_acl(admin: &Client, database: &str) -> anyhow::Result<String> {
    let rows = admin
        .query(
            "SELECT grantee::regrole::text AS grantee,
                    privilege_type,
                    is_grantable
               FROM pg_catalog.pg_database d,
                    LATERAL pg_catalog.aclexplode(d.datacl)
              WHERE d.datname = $1
              ORDER BY 1, 2",
            &[&database],
        )
        .await
        .context("read the database-level ACL")?;
    let owner: String = admin
        .query_one(
            "SELECT pg_catalog.pg_get_userbyid(datdba)::text
               FROM pg_catalog.pg_database WHERE datname = $1",
            &[&database],
        )
        .await
        .context("read the database owner")?
        .get(0);

    let quoted = Identifier::new(database.to_owned())?.quoted();
    let mut sql = format!(
        "ALTER DATABASE {quoted} OWNER TO {};\n         REVOKE ALL ON DATABASE {quoted} FROM PUBLIC;\n",
        Identifier::new(owner)?.quoted()
    );
    for row in rows {
        let grantee: Option<String> = row.get("grantee");
        let privilege: String = row.get("privilege_type");
        let grantable: bool = row.get("is_grantable");
        // A NULL grantee is PUBLIC, which the blanket REVOKE above already
        // settled and which this loop must not hand back.
        let Some(grantee) = grantee else { continue };
        if grantee == "-" {
            continue;
        }
        writeln!(
            sql,
            "GRANT {privilege} ON DATABASE {quoted} TO {}{};",
            Identifier::new(grantee)?.quoted(),
            if grantable { " WITH GRANT OPTION" } else { "" }
        )
        .expect("writing to a String cannot fail");
    }
    Ok(sql)
}

/// Install the catalog, the application authorization schema, and the
/// platform principal rows of `tenant`, the tenant the project database serves.
pub async fn install_journey_platform_floor(
    project: &Client,
    tenant: &str,
    platform_domain: &str,
) -> anyhow::Result<()> {
    let platform_principals = platform_principals_sql(tenant, platform_domain)
        .context("render the platform principal rows")?;
    project
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .context("install the catalog schema")?;
    project
        .batch_execute(include_str!("../../../../../deploy/sql/app-schema.sql"))
        .await
        .context("install the application authorization schema")?;
    project
        .batch_execute(&platform_principals)
        .await
        .context("create the platform principal rows")
}

pub async fn reconcile_journey_run_plane(
    system_url: &str,
    project_url: &str,
) -> anyhow::Result<()> {
    reconcile_run_plane::reconcile_run_plane(ReconcileRunPlaneRequest {
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
    .context("reconcile the journey run plane")?;
    Ok(())
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
        provision_project_env::run_workload_action(&args)
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

/// One remote-boundary test Gate and the authority it listens on.
///
/// The local development coordinator validates authoring in-process. Retained
/// delivery and authentication tests use this real process to exercise the
/// authenticated HTTP boundary itself.
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

/// Settle the address a remote-boundary test Gate will listen on.
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

/// Spawn a remote-boundary `wamn-scenario-worker serve` test fixture.
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
            // A connect shows SOMETHING listens. Re-check the child so a Gate
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

pub fn write_dev_config(
    root: &Path,
    system_url: &str,
    template: &str,
    route: &ProvisionedRoute,
    credentials: &JourneyCredentials,
    inputs: &DevEnvironmentInputs,
    identity: &DevActivationIdentity,
) -> anyhow::Result<PathBuf> {
    let wasmtime_cache = root.join("dev-wasmtime-cache");
    // An operator reuses one environment directory across runs, and a warm
    // compilation cache is the point of keeping it.
    std::fs::create_dir_all(&wasmtime_cache)
        .context("create the product-command Wasmtime cache")?;
    let mut config = serde_json::json!({
        "target_database_url": route.database_url.as_str(),
        // The loop recreates the target when its schema inputs change, which
        // drops every per-database privilege with it. This is the file it
        // replays, and it is the file provision_route already applied, not a
        // second copy.
        "target_privileges_file": root.join("privileges.sql"),
        // The loop clones this template instead of re-provisioning. It carries
        // everything a drop destroys except the database-level ACL, which is
        // the file beside it.
        "target_template_database": template,
        "target_database_acl_file": root.join("database-acl.sql"),
        "system_database_url": system_url,
        "identity_database_url": credentials.identity_reader.as_str(),
        "guest_database_url": credentials.guest_sql.as_str(),
        "executor_platform_database_url": credentials.executor_platform.as_str(),
        "http_admitter_database_url": credentials.http_admitter.as_str(),
        "event_materializer_database_url": credentials.event_materializer.as_str(),
        "scheduler_nats_url": inputs.nats_url.as_str(),
        "event_nats_url": inputs.event_nats_url.as_str(),
        "event_nats_username": inputs.event_nats_username.as_str(),
        "event_nats_password_file": &inputs.event_nats_password_file,
        "stream_replicas": inputs.stream_replicas,
        "dup_window_secs": inputs.dup_window_secs,
        "tempo_query_url": inputs.tempo_query_url.as_str(),
        "otel_exporter_otlp_endpoint": inputs.otel_exporter_otlp_endpoint.as_str(),
        "gate_bearer_token": route
            .management_token
            .as_deref()
            .context("project provisioning emitted no management-author PAT")?,
        "operator_bearer_token": route.token.as_str(),
        "route_host": inputs.route_host.as_str(),
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
    // A separate insert keeps the literal above inside the json! recursion limit.
    config["platform_domain"] = inputs.platform_domain.as_str().into();
    config["local_artifacts"] = serde_json::to_value(&inputs.local_artifacts)?;
    if root.join("identity-trust.json").exists() {
        config["session_identity"] = read_json(&root.join("identity-trust.json"))?;
    }
    let path = root.join("dev.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)
        .context("write the strict product-command configuration")?;
    Ok(path)
}
