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

/// Public component configuration. Database passwords stay in CNPG Secrets.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub org: wamn_registry::OrgId,
    pub http_addr: SocketAddr,
    pub public_origin: String,
    pub database: wamn_postgres::ConnectionConfig,
    pub apply_database: wamn_postgres::ConnectionConfig,
    pub session_issuer: String,
    pub session_jwks_url: String,
    pub session_audience: String,
    pub session_algorithms: Vec<String>,
    pub session_jwks_ca: PathBuf,
    pub administration_dir: PathBuf,
    pub worker: wamn_kubernetes::job::Worker,
    pub chart: PathBuf,
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
    pub events: Option<Events>,
    #[serde(default)]
    pub identity: Identity,
    pub oauth_encryption: Option<wamn_envelope::encryption::Configuration>,
}

/// The installed event stream policy.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Events {
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
}

/// The identity endpoint and its server trust roots.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub endpoint: Option<String>,
    pub server_ca: Option<PathBuf>,
}
impl Identity {
    fn users(&self) -> anyhow::Result<Option<UserClient>> {
        let Some(endpoint) = self.endpoint.as_deref() else {
            return Ok(None);
        };
        UserClient::new(
            endpoint,
            self.server_ca
                .as_deref()
                .context("identity server CA is required")?,
        )
        .map(Some)
    }
}

impl Configuration {
    /// Read one installer-owned configuration before accepting requests.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let config: Self = serde_json::from_slice(&std::fs::read(path)?)
            .context("read native control configuration")?;
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
            .worker
            .image
            .rsplit_once('@')
            .context("worker image must be pinned")?;
        digest.parse::<wamn_digest::Digest>()?;
        Ok(config)
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
            registry_auth_file: self.registry_auth_file.clone(),
            registry_token_metadata: self.registry_token_metadata,
            oci_ca_paths: self.oci_ca_paths.clone(),
            insecure_registry: self.insecure_registry,
            package_artifact_base: Some(self.package_artifact_base.clone()),
            host_group: None,
            identity_issuer: self.session_issuer.clone(),
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

async fn wait_for_system_schema(database: &str) -> anyhow::Result<sqlx::PgConnection> {
    use std::time::Duration;

    tokio::time::timeout(Duration::from_mins(5), async {
        loop {
            let ready = tokio::time::timeout(Duration::from_secs(5), async {
                let mut connection = wamn_postgres::connect(database).await?;
                let domain: Option<String> =
                    sqlx::query_scalar(wamn_registry::sql::PLATFORM_DOMAIN_SQL)
                        .fetch_one(&mut connection)
                        .await?;
                Ok::<_, anyhow::Error>(domain.map(|_| connection))
            })
            .await;
            if let Ok(Ok(Some(connection))) = ready {
                return connection;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("wait for system schema timed out"))
}

/// Serve org routes on native HTTP and drain active requests on shutdown.
pub async fn serve(config: Configuration) -> anyhow::Result<()> {
    let platform = config.platform()?;
    write_kubeconfig(&platform.target.kubeconfig, config.org.as_str())?;
    let integration_registry = wamn_oci::package_artifact::PackageRegistry {
        artifact_base: config.integration_artifact_base.clone(),
        credentials: None,
        registry_auth_file: config.registry_auth_file.clone(),
        insecure_registry: config.insecure_registry,
        oci_ca_paths: config.oci_ca_paths.clone(),
    };
    let registry_base =
        wamn_catalog::parse_component_artifact_base(&config.integration_artifact_base)?;
    let registry_credentials = if config.registry_token_metadata {
        wamn_oci::package_artifact::registry_credentials::RegistryCredentialSource::MetadataServer
    } else {
        wamn_oci::package_artifact::registry_credentials::RegistryCredentialSource::Fixed(
            wamn_oci::package_artifact::registry_credentials::read_registry_credentials(
                &config.registry_auth_file,
                registry_base.registry(),
            )?,
        )
    };
    write_kubeconfig(&platform.target.kubeconfig, config.org.as_str())?;
    let database = config.database.to_json()?;
    let writer = wait_for_system_schema(&database).await?;
    let pool = wamn_postgres::pool(&database, 4).await?;
    let identity_databases = config.administration_dir.clone();
    let identity_system = wamn_provision::migrations::MigrationDatabase::System
        .configuration(&config.apply_database.to_json()?)?;
    let verifier = wamn_authentication::access_token::AccessTokenVerifier::new(
        &config.session_issuer,
        &config.session_jwks_url,
        &config.session_audience,
        &config.session_algorithms,
        &config.session_jwks_ca,
    )?;
    let oauth = config
        .oauth_encryption
        .as_ref()
        .map(|configuration| {
            let envelope = Arc::new(wamn_envelope::encryption::Envelope::open(configuration)?);
            wamn_http::oauth_authorization::Authorization::new(&config.public_origin, envelope)
                .map(Arc::new)
        })
        .transpose()?;
    let transport = Arc::new(wamn_http::transport::HttpTransport::new()?);
    let handlers = Arc::new(
        HostRouteDelivery::new(HostRouteHandlers {
            control: pool.clone(),
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            identity: config.identity.users()?,
            issuer: config.session_issuer,
            administration: Some(config.administration_dir.clone()),
            status: Arc::new(crate::environment_status::KubernetesSource),
            org: config.org.clone(),
        })
        .with_control_authenticator(Arc::new(ControlRouteAuthenticator::new(verifier)))
        .with_control_execution(Arc::new(execution::Execution {
            org: config.org.clone(),
            worker: config.worker.clone(),
            administration: config.administration_dir.clone(),
            control_database: database,
            registry_token_metadata: config.registry_token_metadata,
            system_database: identity_system.clone(),
            integration_registry,
            registry_credentials,
        }))
        .with_worker(config.worker)
        .with_operation_jobs(wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?)
        .with_registry_token_metadata(config.registry_token_metadata)
        .with_connection_health(Arc::new(crate::connection_health::ConnectionHealth::new(
            transport,
        )))
        .with_oauth(oauth.as_ref().map(Arc::clone)),
    );
    let callback_databases = Arc::new(config.administration_dir);
    let listener = tokio::net::TcpListener::bind(config.http_addr).await?;
    let mut maintenance = tokio::task::JoinSet::new();
    maintenance.spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(60));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            if let Err(error) = retention::identities(&identity_databases, &identity_system).await {
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
