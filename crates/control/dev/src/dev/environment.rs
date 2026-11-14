//! The disposable development environment the `wamn dev` loop runs
//! against.
//!
//! `wamn dev` was testable before it was startable: every value its strict
//! configuration needs — five credential URLs, the verification database, the
//! Condition and its bearer token — only existed inside the live test. This module
//! is the argument-building layer over the platform verbs that mint them, so
//! `[WAMN-DEV-LIVE]`, `[RECEIVING-ROUTE-JOURNEY]` and the `wamn dev up`
//! operator command stand up one environment by one path (wamn-10yt.10.32).
//!
//! The control library owns this production path. CLI and test callers share
//! its provisioning operations without importing command-line wrappers.
//!
//! The verbs underneath are the shared truth. Nothing here reimplements
//! provisioning; it only names the arguments and the order.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};

use std::path::{Path, PathBuf};

use crate::identifier::Identifier;
use anyhow::Context as _;

use wamn_control_provision::sql as provision_sql;
use wamn_environment_control::provision_project_env::{
    self, ProvisionProjectEnvRequest, ProvisionedRoute, read_json, secret_annotation, secret_value,
};
use wamn_identity_client::PatIssuerConfig;

use crate::dev::activation::DevActivationIdentity;
use wamn_control_provision::Template;
use wamn_environment_control::provision_org::{OrgRecord, ProvisionOrgRequest, provision_org};

/// The deployment-owned inputs a standing development environment needs.
///
/// The live test reads them from its explicit inputs and the operator
/// command takes them as flags; neither builds a second set of arguments.
#[derive(Debug)]
pub struct DevEnvironmentInputs {
    pub local_artifacts: super::config::LocalArtifacts,
    pub host_binary: PathBuf,
    /// The one local NATS and the development certificate.
    pub nats: wamn_broker::nats::PlatformNats,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
    pub otel_exporter_otlp_endpoint: String,
    pub route_host: String,
    /// Domain of the platform principal emails, `<component>@<platform-domain>`.
    pub platform_domain: String,
    pub package_sources: Vec<PathBuf>,
    /// The CDC reader that `wamn dev up` enabled.
    pub cdc_reader: Option<super::config::CdcReader>,
}

/// The host configuration file inside the private root.
const HOST_CONFIG_FILE: &str = "host-config.yaml";

/// Write the host configuration file with the `plugins` entry that the
/// release chart declares (docs/plan/alignment.md R6). Its `events` binding
/// inherits the host's data connection, as in the cluster.
fn write_host_config(root: &Path, identity: &DevActivationIdentity) -> anyhow::Result<PathBuf> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let plugins = wamn_release_control::release_chart::events_plugin(
        &identity.org,
        &identity.project,
        identity.environment.as_str(),
    )?;
    let mut host = serde_yaml::Mapping::new();
    host.insert("plugins".into(), plugins);
    let mut config = serde_yaml::Mapping::new();
    config.insert("host".into(), serde_yaml::Value::Mapping(host));
    let path = root.join(HOST_CONFIG_FILE);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    // A file left by an earlier run keeps its old mode, so set it again.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(serde_yaml::to_string(&config)?.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Everything the strict `wamn dev` configuration is written from.
#[expect(
    missing_debug_implementations,
    reason = "carries minted PATs and password-bearing URLs; no derived formatter may print them"
)]
pub struct DevEnvironment {
    /// Pristine clone source for every run's target database.
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
    admin: &mut Client,
    root: &Path,
    platform_domain: &str,
    document: &wamn_environment_control::environment::document::EnvironmentDocument,
) -> anyhow::Result<DevEnvironment> {
    wamn_control_provision::validate_platform_domain(platform_domain)?;
    let identity = dev_activation_identity(document);
    let version: String = sqlx::query_scalar("SHOW server_version_num")
        .fetch_one(&mut *admin)
        .await?;
    anyhow::ensure!(
        version.parse::<u32>()? >= 180_000,
        "the development environment requires PostgreSQL 18 or newer"
    );
    anyhow::ensure!(
        !root.join("identity-process.json").exists(),
        "stop the existing development environment before provisioning again"
    );
    let coordinate = JourneyTarget {
        org: identity.org.clone(),
        project: identity.project.clone(),
        environment: identity.environment.clone(),
    };
    provision_journey_control(&coordinate, system_url, admin).await?;
    wamn_environment_control::provision_system::set_platform_domain(admin, platform_domain).await?;
    let route_secret = root.join("operator-pat.json");
    let management_secret = root.join("management-author-pat.json");
    let mut args = provisioning_args(
        &coordinate,
        system_url,
        root,
        &route_secret,
        Some(&management_secret),
    );
    args.emit_management_author_pat_secret = None;
    args.emit_operator_pat_secret = None;
    args.policy = Some(document.policy.clone());
    provision_project_env::provision_project_env(&args).await?;
    let project_url = prepare_route_database(system_url, admin, root).await?;
    let mut project = connect(&project_url).await?;
    install_journey_platform_base_grants(&mut project, platform_domain).await?;
    let mut tx = project.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ),
    ))
    .execute(&mut *tx)
    .await?;
    wamn_environment_control::environment::document::write_revocation_configuration(
        &mut tx, document,
    )
    .await?;
    tx.commit().await?;
    drop(project);
    initialize_journey_platform(&coordinate, system_url, &project_url).await?;
    let credentials = prepare_journey_credentials(&coordinate, system_url, &project_url).await?;
    let reader = super::credentials::workload(
        &project_url,
        &coordinate,
        wamn_control_provision::WorkloadRoleFamily::SessionRoleReader,
    )
    .await?;
    let instance =
        provision_project_env::read_project_env_instance(system_url, &document.triple()).await?;
    let target = wamn_control_provision::session_target::SessionTarget::new(
        &document.triple(),
        &instance,
        &coordinate.project_env_key(),
        &reader,
    )?;
    let target_path = root.join("identity-target.json");
    provision_project_env::write_secret_json(
        &target_path,
        &serde_json::from_str(&target.to_json()?)?,
    )?;
    let issuer = super::pat_issuer::start_environment(system_url, root, &target_path).await?;
    args.emit_management_author_pat_secret = Some(management_secret.clone());
    args.emit_operator_pat_secret = Some(route_secret);
    args.pat_issuer = issuer.args.clone();
    if let Err(error) = provision_project_env::provision_project_env(&args).await {
        issuer.stop().await?;
        return Err(error);
    }
    let route = route_credentials(project_url, root, Some(&management_secret))?;
    provision_project_env::write_secret_json(
        &root.join("identity-trust.json"),
        &serde_json::json!({"issuer":issuer.args.endpoint,"ca":issuer.args.server_ca,"instance_suffix":instance}),
    )?;
    let target = wamn_postgres::identity(&route.database_url)?.1;
    let instance: sqlx::postgres::types::Oid =
        sqlx::query_scalar("SELECT oid FROM pg_database WHERE datname=$1")
            .bind(&target)
            .fetch_one(&mut *admin)
            .await?;
    let marker = wamn_runtime::local_application::local_target_marker(
        &identity.project_env_key(),
        &identity.environment,
        instance.0,
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "COMMENT ON DATABASE {} IS {}",
        postgres_protocol::escape::escape_identifier(&target),
        postgres_protocol::escape::escape_literal(&marker)
    )))
    .execute(&mut *admin)
    .await?;
    Ok(DevEnvironment {
        route,
        credentials,
        identity,
        issuer,
    })
}

pub const ORG: &str = "acme";

pub const PROJECT: &str = "receiving";

pub const ENVIRONMENT: &str = "dev";

/// The `project_env_key` of the coordinate, as `project_env_key(ORG, PROJECT, ENVIRONMENT)`
/// derives it.
pub const PROJECT_ENV_KEY: &str = "acme--dev--receiving";

/// The org, project, and environment that one journey provisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JourneyTarget {
    pub org: String,
    pub project: String,
    pub environment: String,
}

impl JourneyTarget {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> String {
        wamn_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

impl JourneyTarget {
    /// The Receiving journey of the dev loop, and of a fixture with no candidate.
    pub fn receiving() -> Self {
        Self {
            org: ORG.to_owned(),
            project: PROJECT.to_owned(),
            environment: ENVIRONMENT.to_owned(),
        }
    }
}

pub async fn connect(input: &str) -> anyhow::Result<Client> {
    wamn_postgres::connect(input).await
}

fn database_url(admin_url: &str, database: &str) -> anyhow::Result<String> {
    wamn_postgres::database_configuration(admin_url, database)
}

fn provisioning_args(
    coordinate: &JourneyTarget,
    system_url: &str,
    root: &Path,
    route_secret: &Path,
    management_secret: Option<&Path>,
) -> ProvisionProjectEnvRequest {
    ProvisionProjectEnvRequest {
        changes: wamn_broker::Publisher::default(),
        org: coordinate.org.clone(),
        project: coordinate.project.clone(),
        env: coordinate.environment.clone(),
        policy: None,
        target: "cloud".to_owned(),
        // The development target is disposable, and its registry row is where
        // that is written down (wamn-10yt.38). Admit reads the projection of
        // THIS row, so an author's re-run replaces its own component fact
        // instead of being locked out of a package version by a reformat.
        disposable: Some(true),
        system_database_url: Some(system_url.to_owned()),
        cluster: Some("route-auth-pg18".to_owned()),
        connection_limit: None,
        cluster_namespace: "wamn-system".to_owned(),
        namespace: "wamn-system".to_owned(),
        secret_namespace: None,
        emit_database: Some(root.join("database.json")),
        emit_role_sql: Some(root.join("roles.sql")),
        emit_privilege_sql: Some(root.join("privileges.sql")),
        emit_secret: Some(root.join("database-secret.json")),
        emit_management_author_pat_secret: management_secret.map(Path::to_path_buf),
        emit_operator_pat_secret: Some(route_secret.to_path_buf()),
        pat_issuer: PatIssuerConfig::default(),
    }
}

pub async fn reset_control_store(admin: &mut Client) -> anyhow::Result<()> {
    let stale_databases = sqlx::query(
        "SELECT datname::text FROM pg_database \
             WHERE datname LIKE 'wamn-db-acme--%--%--%' ORDER BY datname",
    )
    .fetch_all(&mut *admin)
    .await
    .context("list stale route-auth databases")?;
    for row in stale_databases {
        let database: String = row.get(0);
        sqlx::raw_sql(sqlx::AssertSqlSafe(provision_sql::drop_database_named_sql(
            &database,
        )))
        .execute(&mut *admin)
        .await
        .map(drop)
        .with_context(|| format!("drop stale database {database}"))?;
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DROP SCHEMA IF EXISTS identity CASCADE; \
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
             END $$;")
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .context("prepare the production control owner")?;
    wamn_environment_control::provision_system::install_control_store(admin)
        .await
        .map(|_| ())
}

pub async fn provision_route(
    coordinate: &JourneyTarget,
    system_url: &str,
    admin: &mut Client,
    root: &Path,
    management_secret: Option<&Path>,
) -> anyhow::Result<ProvisionedRoute> {
    let route_secret = root.join("operator-pat.json");
    let issuer = super::pat_issuer::start(system_url, root).await?;
    let mut args = provisioning_args(
        coordinate,
        system_url,
        root,
        &route_secret,
        management_secret,
    );
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
    admin: &mut Client,
    root: &Path,
) -> anyhow::Result<String> {
    let database = read_json(&root.join("database.json"))?["spec"]["name"]
        .as_str()
        .context("Database CR carries spec.name")?
        .to_owned();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (std::fs::read_to_string(root.join("roles.sql"))
            .context("read emitted role SQL")?
            .as_str())
        .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .context("apply emitted role SQL")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "{} OWNER wamn_db_owner",
        provision_sql::create_database_named_sql(&database)
    )))
    .execute(&mut *admin)
    .await
    .map(drop)
    .context("stand in for the emitted Database CR")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        std::fs::read_to_string(root.join("privileges.sql"))
            .context("read emitted privilege SQL")?,
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .context("apply emitted privilege SQL")?;

    database_url(system_url, &database)
}

fn route_credentials(
    database_url: String,
    root: &Path,
    management_secret: Option<&Path>,
) -> anyhow::Result<ProvisionedRoute> {
    let route_secret = root.join("operator-pat.json");
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
    pub migration: String,
    pub executor_platform: String,
    pub event_materializer: String,
    pub http_admitter: String,
    pub administration: String,
    pub identity_reader: String,
    pub connection_reader: String,
}

/// Install the same stable role surfaces on the operator's disposable local database.
pub async fn prepare_journey_credentials(
    coordinate: &JourneyTarget,
    system_url: &str,
    project_url: &str,
) -> anyhow::Result<JourneyCredentials> {
    use wamn_control_provision::WorkloadRoleFamily as Family;
    let workload = super::credentials::workload;
    Ok(JourneyCredentials {
        guest_sql: workload(project_url, coordinate, Family::App).await?,
        migration: workload(project_url, coordinate, Family::Migration).await?,
        executor_platform: workload(project_url, coordinate, Family::ExecutorPlatform).await?,
        event_materializer: workload(project_url, coordinate, Family::EventMaterializer).await?,
        http_admitter: workload(project_url, coordinate, Family::HttpAdmitter).await?,
        administration: workload(project_url, coordinate, Family::Administration).await?,
        identity_reader: workload(system_url, coordinate, Family::IdentityReader).await?,
        connection_reader: workload(system_url, coordinate, Family::ConnectionReader).await?,
    })
}

/// The email of the journey org's admin, the first user of the store, who
/// signs in to the development environment.
pub const DEVELOPER_EMAIL: &str = "development@example.invalid";

pub async fn provision_journey_control(
    coordinate: &JourneyTarget,
    system_url: &str,
    admin: &mut Client,
) -> anyhow::Result<()> {
    super::pat_issuer::preflight(system_url)?;
    reset_control_store(admin).await?;
    provision_org(ProvisionOrgRequest {
        org: coordinate.org.clone(),
        template: Template::trials(),
        pool: "route-auth-pg18".to_owned(),
        record: Some(OrgRecord {
            system_database_url: system_url.to_owned(),
            admin: DEVELOPER_EMAIL.to_owned(),
        }),
        cluster_namespace: "wamn-system".to_owned(),
    })
    .await
    .context("stamp the journey org and environment policies through provision-org")
    .map(|_| ())
}

/// Install the catalog, the application authorization schema and the platform principal rows.
pub async fn install_journey_platform_base_grants(
    project: &mut Client,
    platform_domain: &str,
) -> anyhow::Result<()> {
    wamn_environment_control::platform_migrations::install_project(project).await?;
    let owner: String = sqlx::query(
        "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname=current_database()",
    )
    .fetch_one(&mut *project)
    .await
    .context("read the project database owner")?
    .get(0);
    let owner = Identifier::new(&owner)?.quoted();
    // As ensure does for env apply: the owner reaches the platform policies.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "GRANT wamn_platform TO {owner} WITH INHERIT TRUE, SET FALSE"
    )))
    .execute(&mut *project)
    .await
    .map(drop)
    .context("grant wamn_platform to the project database owner")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET ROLE {owner}")))
        .execute(&mut *project)
        .await
        .map(drop)
        .context("assume the project database owner")?;
    wamn_platform_identity::environment::fill_platform(project, platform_domain).await?;
    sqlx::raw_sql("RESET ROLE")
        .execute(&mut *project)
        .await
        .map(drop)
        .context("leave the project database owner")
}

/// Grant system project-admin to the operator service and project its admin row.
pub async fn grant_operator_admin_role(
    system_url: &str,
    project_url: &str,
    org: &str,
    project: &str,
    environment: &str,
    project_env_key: &str,
    platform_domain: &str,
) -> anyhow::Result<()> {
    let subject = wamn_platform_identity::operator_subject(org, project, environment)?;
    let mut system = wamn_postgres::connect(system_url).await?;

    let mut target = wamn_postgres::connect(project_url).await?;

    let result = async {
        let principal = wamn_platform_identity::resolve_subject(
            &mut system,
            wamn_platform_identity::PrincipalType::Service,
            &subject,
        )
        .await?
        .context("the operator service does not exist")?;
        let mut authority = provision_project_env::provisioning_transaction(&mut system).await?;
        let mut drops = wamn_broker::auth_changes::AuthDrops::default();
        wamn_platform_identity::assign_project_role(
            &mut authority,
            principal.id(),
            org,
            project,
            wamn_platform_identity::control::PROJECT_ADMIN_ROLE,
            &mut drops,
        )
        .await?;
        authority.commit().await?;
        let mut authority = system.begin().await?;
        let mut transaction = target.begin().await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ),
        ))
        .execute(&mut *transaction)
        .await
        .map(drop)?;
        let coordinate = wamn_platform_identity::application::ApplicationTarget {
            org,
            project,
            env: environment,
            project_env_key,
        };
        wamn_platform_identity::application::project_authority(
            &mut authority,
            &mut transaction,
            coordinate,
            principal.id(),
            &mut drops,
        )
        .await?;
        transaction.commit().await?;
        authority.commit().await?;
        anyhow::Ok(())
    }
    .await;
    drop(target);
    drop(system);
    result.with_context(|| {
        format!("grant system administration to {subject}@{platform_domain} in {project_env_key}")
    })
}

pub async fn initialize_journey_platform(
    coordinate: &JourneyTarget,
    system_url: &str,
    project_url: &str,
) -> anyhow::Result<()> {
    let mut project = wamn_postgres::connect(project_url).await?;
    wamn_environment_control::platform_migrations::install_project(&mut project).await?;
    let scope = wamn_control_provision::migrations::MigrationDatabase::Project;
    let (_, database) = wamn_postgres::identity(project_url)?;
    let migration_input = super::credentials::login(
        project_url,
        &scope.login(&database),
        Some(scope.owner_role()),
        "",
    )
    .await?;
    wamn_environment_control::platform_migrations::restore_project(
        &migration_input,
        &wamn_environment_control::platform_migrations::embedded_sources(scope),
    )
    .await?;
    let key = wamn_registry::project_env_key(
        &coordinate.org,
        &coordinate.project,
        &coordinate.environment,
    );
    wamn_platform_identity::environment::fill(
        system_url,
        project_url,
        wamn_platform_identity::application::ApplicationTarget {
            org: &coordinate.org,
            project: &coordinate.project,
            env: &coordinate.environment,
            project_env_key: &key,
        },
        &wamn_broker::Publisher::default(),
    )
    .await?;
    Ok(())
}

/// The identity declared by the local environment document.
pub fn dev_activation_identity(
    document: &wamn_environment_control::environment::document::EnvironmentDocument,
) -> DevActivationIdentity {
    let process = std::process::id();
    let key = wamn_registry::project_env_key(&document.org, &document.project, &document.env);
    let host_group = wamn_registry::name::dev_host_group(&key);
    DevActivationIdentity {
        catalog: "default".to_owned(),
        environment: document.env.clone(),
        org: document.org.clone(),
        project: document.project.clone(),
        host_name: format!("{host_group}-{process}"),
        runner: format!("{host_group}-{process}"),
        host_group,
    }
}

pub fn write_dev_config(
    root: &Path,
    system_url: &str,
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
    let host_config = write_host_config(root, identity)?;
    let mut config = serde_json::json!({
        "target_database_url": route.database_url.as_str(),
        "migration_database_url": credentials.migration.as_str(),
        "system_database_url": system_url,
        "identity_database_url": credentials.identity_reader.as_str(),
        "connection_database_url": credentials.connection_reader.as_str(),
        "guest_database_url": credentials.guest_sql.as_str(),
        "executor_platform_database_url": credentials.executor_platform.as_str(),
        "http_admitter_database_url": credentials.http_admitter.as_str(),
        "event_materializer_database_url": credentials.event_materializer.as_str(),
        "administration_database_url": credentials.administration.as_str(),
        "scheduler_nats_url": inputs.nats.nats_url.as_str(),
        "nats_tls_ca": &inputs.nats.nats_tls_ca,
        "nats_tls_cert": &inputs.nats.nats_tls_cert,
        "nats_tls_key": &inputs.nats.nats_tls_key,
        "stream_replicas": inputs.stream_replicas,
        "dup_window_secs": inputs.dup_window_secs,
        "otel_exporter_otlp_endpoint": inputs.otel_exporter_otlp_endpoint.as_str(),
        "condition_bearer_token": route
            .management_token
            .as_deref()
            .context("project provisioning emitted no management-author PAT")?,
        "route_host": inputs.route_host.as_str(),
        "package_sources": inputs.package_sources.as_slice(),
        "catalog": identity.catalog.as_str(),
        "environment": identity.environment.as_str(),
        "org": identity.org.as_str(),
        "project": identity.project.as_str(),
        "host_group": identity.host_group.as_str(),
        "host_name": identity.host_name.as_str(),
        "runner": identity.runner.as_str(),
        "host_binary": &inputs.host_binary,
        "wasmtime_cache_dir": wasmtime_cache,
        "host_config_file": host_config,
    });
    // A separate insert keeps the literal above inside the json! recursion limit.
    config["platform_domain"] = inputs.platform_domain.as_str().into();
    config["local_artifacts"] = serde_json::to_value(&inputs.local_artifacts)?;
    if let Some(reader) = &inputs.cdc_reader {
        config["cdc_reader"] = serde_json::to_value(reader)?;
    }
    if root.join("identity-trust.json").exists() {
        config["session_identity"] = read_json(&root.join("identity-trust.json"))?;
    }
    let path = root.join("dev.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)
        .context("write the strict product-command configuration")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_uses_the_authored_coordinate() {
        let document = serde_json::from_value(serde_json::json!({
            "org":"other", "project":"stock", "env":"preview", "target":"cloud", "policy":"dev","revocation_seconds":60,
            "applications": {"stock":{"current": "none", "revocation_seconds":5,"hostname": "stock.localhost", "packages":["inventory", "orders"]}}}))
        .unwrap();
        let identity = dev_activation_identity(&document);
        assert_eq!(identity.org, "other");
        assert_eq!(identity.project, "stock");
        assert_eq!(identity.environment, "preview");
        assert_eq!(
            identity.project_env_key(),
            wamn_registry::project_env_key(&document.org, &document.project, &document.env)
        );
    }
}
