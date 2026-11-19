//! Authenticated org administration without a guest runtime.

pub mod connection_health;
mod control_route;
pub mod environment_status;
mod execution;
mod host_route;
pub mod platform;
pub mod retention;
mod worker;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, ensure};
pub use host_route::{ControlExecution, HostRouteDelivery, HostRouteHandlers};
use http_body_util::BodyExt as _;
use serde::Deserialize;
use serde_json::json;
use wamn_authentication::route_authentication::ControlRouteAuthenticator;
use wamn_environment::environment::Platform;
use wamn_identity_client::users::UserClient;

const HTTP_PORT: u16 = 80;
const ADMINISTRATION_DIR: &str = "/etc/wamn/control-administration";
const REGISTRY_AUTH_FILE: &str = "/credentials/registry/config.json";
const IDENTITY_CA: &str = "/etc/identity-ca/ca.crt";
const IDENTITY_CLIENT_CERT: &str = "/etc/identity-control/tls.crt";
const IDENTITY_CLIENT_KEY: &str = "/etc/identity-control/tls.key";
const OAUTH_KEY: &str = "/etc/wamn/oauth/key";
const SETTINGS_FILE: &str = "/etc/wamn/control/settings.json";

/// Public component configuration. Database passwords stay in CNPG Secrets.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub org: wamn_registry::OrgId,
    pub public_origin: String,
    pub database: wamn_postgres::ConnectionConfig,
    pub apply_database: wamn_postgres::ConnectionConfig,
    pub identity_issuer: String,
    pub nats_url: String,
    pub registry: String,
    pub worker_image: String,
    pub chart: PathBuf,
    #[serde(default)]
    pub registry_token_metadata: bool,
    #[serde(default)]
    pub oci_ca_paths: Vec<PathBuf>,
    pub events: Option<Events>,
    pub oauth_encryption: Option<wamn_envelope::encryption::Configuration>,
}

/// The installed event stream policy.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Events {
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
}

impl Configuration {
    /// Read one installer-owned configuration before accepting requests.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("read control settings {}", path.display()))?;
        let config: Self = serde_json::from_slice(&bytes).context("decode control settings")?;
        ensure!(
            config.database.credentials.namespace == config.org
                && config.apply_database.credentials.namespace == config.org
                && config.database.server_ca.namespace == config.org
                && config.apply_database.server_ca.namespace == config.org,
            "control credentials must belong to the org namespace"
        );
        ensure!(
            config.apply_database.username.as_str() == wamn_role::APPLY_ROLE,
            "operation configuration requires the apply role"
        );
        let (_, digest) = config
            .worker_image
            .rsplit_once('@')
            .context("worker image must be pinned")?;
        digest.parse::<wamn_digest::Digest>()?;
        Ok(config)
    }

    fn worker(&self) -> wamn_kubernetes::job::Worker {
        wamn_kubernetes::job::Worker {
            image: self.worker_image.clone(),
            configuration: format!(
                "{}-settings",
                wamn_registry::name::control_resource(&self.org)
            ),
            service_account: format!("wamn-operation-{}", self.org),
        }
    }

    fn platform(&self) -> anyhow::Result<Platform> {
        Ok(Platform {
            system_database_url: self.apply_database.to_json()?,
            database_files: None,
            target: wamn_kubernetes::Target {
                kubeconfig: "/work/kubeconfig".into(),
                context: "operation".into(),
                namespace: self.org.to_string(),
            },
            chart: self.chart.clone(),
            chart_download: None,
            registry_auth_file: REGISTRY_AUTH_FILE.into(),
            registry_token_metadata: self.registry_token_metadata,
            oci_ca_paths: self.oci_ca_paths.clone(),
            insecure_registry: wamn_registry::registry_is_loopback(&self.registry),
            package_artifact_base: Some(format!("{}/wamn/packages", self.registry)),
            host_group: None,
            identity_issuer: self.identity_issuer.clone(),
            nats_url: self.nats_url.clone(),
            oauth_encryption: self.oauth_encryption.clone(),
            events: self.events.as_ref().map(|events| wamn_broker::EventBroker {
                stream_replicas: events.stream_replicas,
                dup_window_secs: events.dup_window_secs,
            }),
        })
    }
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
pub async fn serve(config: Configuration) -> anyhow::Result<()> {
    let platform = config.platform()?;
    write_kubeconfig(&platform.target.kubeconfig, config.org.as_str())?;
    let integration_artifact_base = format!("{}/wamn/integrations", config.registry);
    let registry_base = wamn_catalog::parse_component_artifact_base(&integration_artifact_base)?;
    let registry_credentials = if config.registry_token_metadata {
        wamn_oci::package_artifact::registry_credentials::RegistryCredentialSource::MetadataServer
    } else {
        wamn_oci::package_artifact::registry_credentials::RegistryCredentialSource::Fixed(
            wamn_oci::package_artifact::registry_credentials::read_registry_credentials(
                Path::new(REGISTRY_AUTH_FILE),
                registry_base.registry(),
            )?,
        )
    };
    let integration_registry = wamn_oci::package_artifact::PackageRegistry {
        artifact_base: integration_artifact_base,
        credentials: None,
        registry_auth_file: REGISTRY_AUTH_FILE.into(),
        insecure_registry: platform.insecure_registry,
        oci_ca_paths: platform.oci_ca_paths,
    };
    let database = config.database.to_json()?;
    let pool = wamn_postgres::pool(&database, 4).await?;
    let identity_databases = Path::new(ADMINISTRATION_DIR);
    let identity_system = wamn_provision::migrations::MigrationDatabase::System
        .configuration(&platform.system_database_url)?;
    let verifier = wamn_authentication::access_token::AccessTokenVerifier::new(
        &config.identity_issuer,
        &format!(
            "{}/.well-known/jwks.json",
            config.identity_issuer.trim_end_matches('/')
        ),
        &wamn_registry::name::org_audience(&config.org),
        &["EdDSA".into()],
        Path::new(IDENTITY_CA),
    )?;
    let oauth = config
        .oauth_encryption
        .as_ref()
        .map(|configuration| {
            let envelope = match configuration {
                wamn_envelope::encryption::Configuration::Local => {
                    wamn_envelope::encryption::Envelope::from_file(Path::new(OAUTH_KEY))
                }
                wamn_envelope::encryption::Configuration::Gcp { .. } => {
                    wamn_envelope::encryption::Envelope::open(configuration)
                }
            }?;
            let envelope = Arc::new(envelope);
            wamn_http::oauth_authorization::Authorization::new(&config.public_origin, envelope)
                .map(Arc::new)
        })
        .transpose()?;
    let transport = Arc::new(wamn_http::transport::HttpTransport::new()?);
    let worker = config.worker();
    let handlers = Arc::new(
        HostRouteDelivery::new(HostRouteHandlers {
            control: pool.clone(),
            writer: Arc::new(tokio::sync::Mutex::new(
                wamn_postgres::connect(&database).await?,
            )),
            identity: Some(UserClient::new(
                &config.identity_issuer,
                Path::new(IDENTITY_CLIENT_CERT),
                Path::new(IDENTITY_CLIENT_KEY),
                Path::new(IDENTITY_CA),
            )?),
            issuer: config.identity_issuer,
            administration: Some(ADMINISTRATION_DIR.into()),
            status: Arc::new(crate::environment_status::KubernetesSource),
            org: config.org.clone(),
        })
        .with_control_authenticator(Arc::new(ControlRouteAuthenticator::new(verifier)))
        .with_control_execution(Arc::new(execution::Execution {
            org: config.org.clone(),
            worker: worker.clone(),
            administration: ADMINISTRATION_DIR.into(),
            control_database: database,
            registry_token_metadata: config.registry_token_metadata,
            system_database: identity_system.clone(),
            integration_registry,
            registry_credentials,
        }))
        .with_worker(worker)
        .with_operation_jobs(wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?)
        .with_registry_token_metadata(config.registry_token_metadata)
        .with_connection_health(Arc::new(crate::connection_health::ConnectionHealth::new(
            transport,
        )))
        .with_oauth(oauth.as_ref().map(Arc::clone)),
    );
    let callback_databases = Arc::new(PathBuf::from(ADMINISTRATION_DIR));
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], HTTP_PORT))).await?;
    let mut maintenance = tokio::task::JoinSet::new();
    maintenance.spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(60));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            if let Err(error) = retention::identities(identity_databases, &identity_system).await {
                tracing::warn!(%error, "transaction identity retention failed");
            }
        }
    });
    let mut connections = tokio::task::JoinSet::new();
    let shutdown = shutdown();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            signal = &mut shutdown => { signal?; break; }
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = result { tracing::warn!(%error, "control connection failed"); }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let handlers = Arc::clone(&handlers);
                let oauth = oauth.as_ref().map(Arc::clone);
                let callback_databases = Arc::clone(&callback_databases);
                connections.spawn(async move {
                    let service = hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let handlers = Arc::clone(&handlers);
                        let oauth = oauth.as_ref().map(Arc::clone);
                        let callback_databases = Arc::clone(&callback_databases);
                        async move {
                            let request = request.map(|body| body.map_err(wasmtime_wasi_http::Error::from).boxed_unsync());
                            let response = if request.uri().path() == "/oauth/callback" && let Some(oauth) = oauth {
                                wamn_http::oauth_callback::response(&oauth, &callback_databases, request).await
                            } else { handlers.http(request).await };
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
    maintenance.shutdown().await;
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
