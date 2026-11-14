//! Authenticated org administration without a guest runtime.

mod execution;
pub mod platform;
pub mod retention;
mod workflow;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, ensure};
use http_body_util::BodyExt as _;
use serde::Deserialize;
use serde_json::{Value, json};
use wamn_engine::flow_http_routing::FlowHttpRouting;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_environment::environment::Platform;
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_identity_client::PatIssuerConfig;
use wamn_runtime::plugins::route_authentication::ControlRouteAuthenticator;

/// Public component configuration. Database passwords stay in CNPG Secrets.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub org: String,
    pub http_addr: SocketAddr,
    pub public_origin: String,
    pub database: wamn_postgres::ConnectionConfig,
    pub apply_database: wamn_postgres::ConnectionConfig,
    pub session_issuer: String,
    pub session_jwks_ca: PathBuf,
    pub administration_dir: PathBuf,
    pub worker: wamn_kubernetes::job::Worker,
    pub chart: PathBuf,
    pub release_artifact_base: String,
    pub package_artifact_base: String,
    pub component_artifact_base: String,
    pub integration_artifact_base: String,
    pub registry_auth_file: PathBuf,
    #[serde(default)]
    pub registry_token_metadata: bool,
    #[serde(default)]
    pub insecure_registry: bool,
    #[serde(default)]
    pub oci_ca_paths: Vec<PathBuf>,
    pub nats: Nats,
    pub events: Option<Events>,
    pub event_observer: Option<Nats>,
    #[serde(default)]
    pub identity: Identity,
    pub oauth_encryption: Option<wamn_runtime::oauth_encryption::Configuration>,
}

/// The platform broker connection, using mounted TLS credentials.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nats {
    pub url: String,
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
    pub server_name: Option<String>,
}
impl Nats {
    fn connection(&self) -> wamn_broker::nats::PlatformNats {
        wamn_broker::nats::PlatformNats {
            nats_url: self.url.clone(),
            nats_tls_ca: self.ca.clone(),
            nats_tls_cert: self.cert.clone(),
            nats_tls_key: self.key.clone(),
            nats_tls_server_name: self.server_name.clone(),
        }
    }
}

/// The installed event stream policy, using the platform broker credentials.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Events {
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
}

/// The identity operator connection. Private keys stay in mounted files.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub endpoint: Option<String>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
    pub server_ca: Option<PathBuf>,
}
impl Identity {
    fn connection(&self) -> PatIssuerConfig {
        PatIssuerConfig {
            endpoint: self.endpoint.clone(),
            client_cert: self.client_cert.clone(),
            client_key: self.client_key.clone(),
            server_ca: self.server_ca.clone(),
        }
    }
}

impl Configuration {
    /// Read one installer-owned configuration before accepting requests.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let config: Self = serde_json::from_slice(&std::fs::read(path)?)
            .context("read native control configuration")?;
        ensure!(
            config.database.credentials.namespace == config.org
                && config.apply_database.credentials.namespace == config.org,
            "control credentials must belong to the org namespace"
        );
        ensure!(
            config.apply_database.username == wamn_control_provision::apply::APPLY_ROLE,
            "operation configuration requires the apply role"
        );
        let expected_control = wamn_control_provision::workload_login(
            wamn_control_provision::WorkloadRoleFamily::Control,
            wamn_control_provision::WorkloadRoleIdentity::Org {
                org: &config.org,
                database: &config.database.database,
            },
        )?;
        ensure!(
            config.database.username == expected_control,
            "native control requires its org control login"
        );
        let (_, digest) = config
            .worker
            .image
            .rsplit_once('@')
            .context("worker image must be pinned")?;
        wamn_package_artifact::ensure_digest(digest)?;
        Ok(config)
    }

    fn platform(&self) -> anyhow::Result<Platform> {
        Ok(Platform {
            system_database_url: self.apply_database.to_json()?,
            target: wamn_kubernetes::Target {
                kubeconfig: "/work/kubeconfig".into(),
                context: "operation".into(),
                namespace: self.org.clone(),
            },
            chart: self.chart.clone(),
            chart_download: None,
            release_artifact_base: self.release_artifact_base.clone(),
            registry_auth_file: self.registry_auth_file.clone(),
            registry_token_metadata: self.registry_token_metadata,
            oci_ca_paths: self.oci_ca_paths.clone(),
            insecure_registry: self.insecure_registry,
            package_artifact_base: Some(self.package_artifact_base.clone()),
            component_artifact_base: Some(self.component_artifact_base.clone()),
            database_namespace: self.org.clone(),
            database_host: None,
            database_port: 5432,
            host_group: None,
            pat_issuer: self.identity.connection(),
            events: self.events.as_ref().map(|events| wamn_broker::EventBroker {
                nats: self.nats.connection(),
                stream_replicas: events.stream_replicas,
                dup_window_secs: events.dup_window_secs,
            }),
            auth_nats: Some(self.nats.connection()),
        })
    }
}

/// Only the service starts this worker with a server-selected actor and coordinate.
#[derive(Debug, clap::Args)]
pub struct Operation {
    #[arg(value_parser = ["deploy", "rollback", "stop", "diff", "apply", "reset", "retain"])]
    verb: String,
    #[arg(long)]
    org: String,
    #[arg(long)]
    project: String,
    #[arg(long)]
    env: String,
    #[arg(long)]
    application: Option<String>,
    #[arg(long)]
    release: Option<String>,
}

/// Execute one authorized operation in its Job, with no nested SQL Job.
pub async fn execute(config: &Configuration, operation: Operation) -> anyhow::Result<Value> {
    ensure!(
        operation.org == config.org,
        "operation belongs to another org"
    );
    let actor = std::env::var("WAMN_DEPLOY_ACTOR")
        .context("authenticated actor is absent")?
        .parse()?;
    let triple =
        wamn_registry::Triple::new(&operation.org, operation.env.as_str(), &operation.project);
    let platform = config.platform()?;
    write_kubeconfig(&platform.target.kubeconfig, &config.org)?;
    if operation.verb == "retain" {
        return Ok(
            json!({"removed":wamn_environment::environment::retention::prune(&platform, &triple).await?}),
        );
    }
    let credentials = if matches!(operation.verb.as_str(), "apply" | "reset") {
        Some({
            let instance = wamn_environment::provision_project_env::read_project_env_instance(
                &platform.system_database_url,
                &triple,
            )
            .await?;
            let database_name = wamn_control_provision::project_env_database_name(
                &operation.org,
                &operation.project,
                &operation.env,
                &instance,
            );
            let key = wamn_control_provision::control_administration_key(
                &operation.project,
                &operation.env,
            );
            let database = std::fs::read_to_string(config.administration_dir.join(key))?;
            ensure!(
                wamn_postgres::identity(&database)?.1 == database_name,
                "operation connection differs from the registry database"
            );
            wamn_environment::environment::sql::reset::Credentials {
                owner: wamn_control_provision::migrations::MigrationDatabase::Project
                    .configuration(&database)?,
                migration: wamn_control_provision::migration::configuration(
                    &database,
                    &operation.org,
                    &operation.project,
                    &operation.env,
                )?,
                capture: wamn_platform_install::environment::capture_connection(
                    &platform.system_database_url,
                    &triple,
                )
                .await?,
            }
        })
    } else {
        None
    };
    wamn_environment::environment::operation::execute(
        &platform,
        wamn_environment::environment::operation::Request {
            operation: &operation.verb,
            org: &operation.org,
            project: &operation.project,
            env: &operation.env,
            application: operation.application.as_deref(),
            release: operation.release.as_deref(),
        },
        &actor,
        credentials.as_ref(),
    )
    .await
}

fn write_kubeconfig(path: &Path, namespace: &str) -> anyhow::Result<()> {
    let host = std::env::var("KUBERNETES_SERVICE_HOST")?;
    let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let account = "/var/run/secrets/kubernetes.io/serviceaccount";
    let value = json!({"apiVersion":"v1","kind":"Config","current-context":"operation",
        "clusters":[{"name":"cluster","cluster":{"server":format!("https://{host}:{port}"),"certificate-authority":format!("{account}/ca.crt")}}],
        "users":[{"name":"worker","user":{"tokenFile":format!("{account}/token")}}],
        "contexts":[{"name":"operation","context":{"cluster":"cluster","user":"worker","namespace":namespace}}]});
    std::fs::write(path, serde_json::to_vec(&value)?)?;
    Ok(())
}

/// Serve org routes on native HTTP and drain active requests on shutdown.
pub async fn serve(mut config: Configuration) -> anyhow::Result<()> {
    config.worker.database_credential = config.apply_database.credentials.name.clone();
    config.worker.administration = config.administration_dir.clone();
    let platform = Arc::new(config.platform()?);
    write_kubeconfig(&platform.target.kubeconfig, &config.org)?;
    let integration_registry = wamn_package_artifact::PackageRegistry {
        artifact_base: config.integration_artifact_base.clone(),
        credentials: None,
        registry_auth_file: config.registry_auth_file.clone(),
        insecure_registry: config.insecure_registry,
        oci_ca_paths: config.oci_ca_paths.clone(),
    };
    let registry_base =
        wamn_catalog::parse_component_artifact_base(&config.integration_artifact_base)?;
    let registry_credentials = if config.registry_token_metadata {
        wamn_package_artifact::registry_credentials::RegistryCredentialSource::MetadataServer
    } else {
        wamn_package_artifact::registry_credentials::RegistryCredentialSource::Fixed(
            wamn_package_artifact::registry_credentials::read_registry_credentials(
                &config.registry_auth_file,
                registry_base.registry(),
            )?,
        )
    };
    write_kubeconfig(&platform.target.kubeconfig, &config.org)?;
    let database = config.database.to_json()?;
    let pool = wamn_postgres::pool(&database, 4).await?;
    let cleanup_pool = pool.clone();
    let mut cleanup_ticks = tokio::time::interval(std::time::Duration::from_secs(60));
    let keys = wamn_runtime::session_keys::IssuerKeys::new(
        wamn_runtime::session_keys::IssuerKeysConfig::new(
            &config.session_issuer,
            &format!(
                "{}/.well-known/jwks.json",
                config.session_issuer.trim_end_matches('/')
            ),
            &std::fs::read(&config.session_jwks_ca)?,
        )?,
    )?;
    let verifier = wamn_identity_protocol::verifier::SessionVerifier::new(
        keys,
        &config.org,
        &wamn_platform_identity::control::control_audience(&config.org)?,
    )?;
    let oauth = config
        .oauth_encryption
        .as_ref()
        .map(|configuration| {
            let envelope = Arc::new(wamn_runtime::oauth_encryption::Envelope::open(
                configuration,
            )?);
            wamn_runtime::oauth_authorization::Authorization::new(&config.public_origin, envelope)
                .map(Arc::new)
        })
        .transpose()?;
    let transport =
        Arc::new(wamn_runtime::plugins::connection_http::transport::HttpTransport::new()?);
    let changes = wamn_broker::Publisher::connect(&config.nats.connection()).await?;
    let handlers = Arc::new(
        HostRouteDelivery::new(
            Arc::new(LoadedRelease::control_root()),
            HostRouteHandlers::Control {
                control: pool.clone(),
                writer: Arc::new(tokio::sync::Mutex::new(
                    wamn_postgres::connect(&database).await?,
                )),
                identity: Some(config.identity.connection()),
                administration: Some(config.administration_dir.clone()),
                status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
                org: config.org.clone(),
                changes: changes.clone(),
            },
        )
        .with_control_authenticator(Arc::new(ControlRouteAuthenticator::new(verifier, pool)))
        .with_control_execution(Arc::new(execution::Execution {
            org: config.org.clone(),
            changes,
            worker: config.worker.clone(),
            platform_database: config.apply_database.clone(),
            event_observer: config.event_observer.as_ref().map(Nats::connection),
            administration: config.administration_dir.clone(),
            identity: config.identity.connection(),
            platform,
            system_database: wamn_control_provision::migrations::MigrationDatabase::System
                .configuration(&config.apply_database.to_json()?)?,
            integration_registry: integration_registry.clone(),
            registry_credentials: registry_credentials.clone(),
        }))
        .with_worker(config.worker)
        .with_operation_jobs(wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?)
        .with_registry_token_metadata(config.registry_token_metadata)
        .with_connection_health(Arc::new(
            wamn_runtime::connection_health::ConnectionHealth::new(transport)
                .with_registry(integration_registry, registry_credentials),
        ))
        .with_oauth(oauth.as_ref().map(Arc::clone)),
    );
    let database: Arc<str> = database.into();
    let org: Arc<str> = config.org.into();
    let routing = Arc::new(FlowHttpRouting::new(None));
    let listener = tokio::net::TcpListener::bind(config.http_addr).await?;
    let mut connections = tokio::task::JoinSet::new();
    let shutdown = shutdown();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            signal = &mut shutdown => { signal?; break; }
            _ = cleanup_ticks.tick() => {
                if let Err(error) = async { wamn_execution_host::credential_cleanup::collect(&mut *cleanup_pool.acquire().await?, &org).await }.await {
                    tracing::warn!(%error, "credential collection failed");
                }
            }
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = result { tracing::warn!(%error, "control connection failed"); }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let handlers = Arc::clone(&handlers);
                let routing = Arc::clone(&routing);
                let oauth = oauth.as_ref().map(Arc::clone);
                let database = Arc::clone(&database);
                let org = Arc::clone(&org);
                connections.spawn(async move {
                    let service = hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let handlers = Arc::clone(&handlers);
                        let routing = Arc::clone(&routing);
                        let oauth = oauth.as_ref().map(Arc::clone);
                        let database = Arc::clone(&database);
                        let org = Arc::clone(&org);
                        async move {
                            let request = request.map(|body| body.map_err(wasmtime_wasi_http::Error::from).boxed_unsync());
                            let response = if request.uri().path() == "/oauth/callback" && let Some(oauth) = oauth {
                                wamn_execution_host::oauth_callback::response(&oauth, &database, &org, request).await
                            } else { handlers.http(&routing, request).await };
                            Ok::<_, std::convert::Infallible>(response)
                        }
                    });
                    if let Err(error) = hyper::server::conn::http1::Builder::new().keep_alive(false)
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service).await {
                        tracing::debug!(%error, "control HTTP connection ended");
                    }
                });
            }
        }
    }
    let drain = async { while connections.join_next().await.is_some() {} };
    if tokio::time::timeout(wamn_engine::lifecycle::RUNTIME_SHUTDOWN_BUDGET, drain)
        .await
        .is_err()
    {
        connections.abort_all();
    }
    Ok(())
}

async fn shutdown() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! { result = tokio::signal::ctrl_c() => result, _ = terminate.recv() => Ok(()) }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
