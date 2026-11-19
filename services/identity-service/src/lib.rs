//! The identity authority serves public keys, user login, and operator user management.

use sqlx::Connection as _;
use sqlx::PgConnection as Client;
pub mod cli;
pub mod mail;
mod nats;
mod password;
mod session;
mod users;
mod workload;

use std::convert::Infallible;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use tokio_rustls::TlsAcceptor;
use wamn_identity::session_keys::{
    active_session_key_exists, initialize_session_key, session_jwks,
};
use wamn_kubernetes::kubernetes_api::KubernetesApi;
use wamn_provision::identity_role::identity_issuer_login;

// Match the public-key client's total fetch bound. No retry or stale response
// is introduced when TLS, request headers, or the authoritative read stalls.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// The default signing key rotation interval: 30 days.
const DEFAULT_KEY_ROTATION: Duration = Duration::from_hours(30 * 24);

/// Validated exact issuer and its narrow database credential.
#[derive(Clone)]
pub struct IdentityConfig {
    issuer: String,
    nats: Option<nats::Settings>,
    connection: String,
    mail: Option<mail::MailConfig>,
}

impl IdentityConfig {
    /// Validate the targeted credential before any database socket is opened.
    pub fn new(issuer: &str, database_url: &str) -> Result<Self, IdentityServiceError> {
        let (username, database) = wamn_postgres::identity(database_url)
            .map_err(|_| IdentityServiceError::new("identity database configuration refused"))?;
        if username
            != identity_issuer_login(issuer)
                .map_err(|_| IdentityServiceError::new("invalid issuer"))?
            || database != "wamn_system"
        {
            return Err(IdentityServiceError::new(
                "identity database authority refused",
            ));
        }
        let connection = database_url.to_owned();
        Ok(Self {
            issuer: issuer.to_owned(),
            nats: None,
            connection,
            mail: None,
        })
    }

    /// Enable invitation and password endpoints with explicit mail configuration.
    pub fn with_resend(mut self, mail: mail::ResendConfig) -> Self {
        self.mail = Some(mail::MailConfig::Resend(mail));
        self
    }

    /// Record local identity mail in a private file without a network sender.
    pub fn with_local_mail(mut self, path: std::path::PathBuf) -> Self {
        self.mail = Some(mail::MailConfig::Local(path));
        self
    }
}

/// Shared service state containing no exported private key material.
#[derive(Clone, Debug)]
pub struct IdentityService {
    inner: Arc<Inner>,
}

struct Inner {
    issuer: String,
    // Every held connection opens again with this connection string after Postgres
    // closes it, for example when the server restarts.
    connection: String,
    passwords: Option<password::State>,
    nats: Option<nats::State>,
    kubernetes: Option<KubernetesApi>,
    // Password, user management, and NATS signing share eight places per replica.
    request_slots: Semaphore,
    envelope: wamn_envelope::encryption::Envelope,
    database: Mutex<Database>,
}

pub(crate) struct Database {
    pub(crate) client: Client,
}

impl fmt::Debug for Database {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Database")
            .field("connected", &true)
            .finish_non_exhaustive()
    }
}

pub(crate) async fn connect(config: &IdentityConfig) -> Result<Database, IdentityServiceError> {
    connect_database(&config.connection).await
}

pub(crate) async fn connect_database(input: &str) -> Result<Database, IdentityServiceError> {
    let client = tokio::time::timeout(IO_TIMEOUT, wamn_postgres::connect(input))
        .await
        .map_err(|_| IdentityServiceError::new("identity database connection timed out"))?
        .map_err(|_| IdentityServiceError::new("identity database connection failed"))?;
    Ok(Database { client })
}

impl Inner {
    async fn rotate_keys(&self) -> Result<(), IdentityServiceError> {
        let mut database = connect_database(&self.connection).await?;
        wamn_identity::session_keys::rotate_session_keys(
            &mut database.client,
            &self.issuer,
            DEFAULT_KEY_ROTATION,
            &self.envelope,
        )
        .await
        .map_err(|_| IdentityServiceError::new("rotate signing keys failed"))
    }

    async fn database(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Database>, IdentityServiceError> {
        let mut database = self.database.lock().await;
        self.reopen(&mut database).await?;
        Ok(database)
    }

    async fn reopen(&self, database: &mut Database) -> Result<(), IdentityServiceError> {
        if database.client.ping().await.is_err() {
            *database = connect_database(&self.connection).await?;
        }
        Ok(())
    }
}

impl fmt::Debug for IdentityConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdentityConfig")
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl IdentityService {
    /// Connect using already validated issuer-targeted authority.
    pub async fn connect(
        config: IdentityConfig,
        envelope: wamn_envelope::encryption::Envelope,
    ) -> Result<Self, IdentityServiceError> {
        let mut database = wait_for_schema(&config).await?;
        initialize_session_key(&mut database.client, &config.issuer, &envelope)
            .await
            .map_err(|_| IdentityServiceError::new("initialize session signing key failed"))?;
        let kubernetes = std::env::var_os("KUBERNETES_SERVICE_HOST")
            .map(|_| KubernetesApi::in_cluster())
            .transpose()
            .map_err(|_| {
                IdentityServiceError::new("configure identity Kubernetes client failed")
            })?;
        let nats = nats::start(
            &mut database.client,
            &config.issuer,
            &envelope,
            kubernetes.as_ref(),
            config.nats,
        )
        .await?;
        let passwords = if let Some(mail) = config.mail {
            Some(password::State::new(mail, &config.connection).await?)
        } else {
            None
        };
        Ok(Self {
            inner: Arc::new(Inner {
                issuer: config.issuer,
                envelope,
                connection: config.connection,
                passwords,
                nats,
                kubernetes,
                request_slots: Semaphore::new(8),
                database: Mutex::new(database),
            }),
        })
    }

    async fn respond(
        &self,
        request: Request<Incoming>,
        source: std::net::IpAddr,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        if request.method() == Method::POST
            && request.uri().query().is_none()
            && matches!(
                request.uri().path(),
                "/users"
                    | "/invitations"
                    | "/users/disable"
                    | wamn_identity_client::nats::ENSURE_ACCOUNT_PATH
                    | wamn_identity_client::nats::SIGN_USER_PATH
            )
        {
            return Ok(users::respond(&self.inner, request, source).await);
        }
        let source = if let Some(value) = request
            .headers()
            .get_all("x-forwarded-for")
            .iter()
            .next_back()
        {
            let address = value
                .as_bytes()
                .rsplit(|byte| *byte == b',')
                .next()
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(|value| value.trim().parse::<std::net::IpAddr>().ok());
            let Some(address) = address else {
                return Ok(response(
                    StatusCode::BAD_REQUEST,
                    "application/json",
                    b"{\"error\":\"invalid client address\"}".to_vec(),
                ));
            };
            address
        } else {
            source
        };
        if request.method() == Method::POST
            && request.uri().query().is_none()
            && matches!(
                request.uri().path(),
                "/password/enroll"
                    | "/password/session"
                    | "/password/renew"
                    | "/password/logout"
                    | "/password/logout-all"
                    | "/password/recover"
                    | "/password/reset"
            )
            && let Some(state) = &self.inner.passwords
        {
            return Ok(password::respond(&self.inner, state, request, source).await);
        }
        let response = if request.method() != Method::GET || request.uri().query().is_some() {
            response(StatusCode::NOT_FOUND, "text/plain", b"not found\n".to_vec())
        } else {
            match request.uri().path() {
                "/healthz" => match self.inner.database().await {
                    Ok(_) => response(StatusCode::OK, "text/plain", b"ok\n".to_vec()),
                    Err(_) => unavailable(),
                },
                "/readyz" => match tokio::time::timeout(IO_TIMEOUT, async {
                    let mut database = self.inner.database().await.map_err(|_| ())?;
                    active_session_key_exists(&mut database.client, &self.inner.issuer)
                        .await
                        .map_err(|_| ())
                })
                .await
                {
                    Ok(Ok(true)) => response(StatusCode::OK, "text/plain", b"ok\n".to_vec()),
                    _ => unavailable(),
                },
                "/.well-known/jwks.json" => {
                    match tokio::time::timeout(IO_TIMEOUT, async {
                        let mut database = self.inner.database().await.map_err(|_| ())?;
                        session_jwks(&mut database.client, &self.inner.issuer)
                            .await
                            .map_err(|_| ())
                    })
                    .await
                    {
                        Ok(Ok(keys)) => match serde_json::to_vec(&keys) {
                            Ok(bytes) => {
                                let mut response =
                                    response(StatusCode::OK, "application/json", bytes);
                                response.headers_mut().insert(
                                    "cache-control",
                                    format!(
                                        "public, max-age={}",
                                        wamn_identity::session_keys::JWKS_CACHE_SECONDS
                                    )
                                    .parse()
                                    .expect("constant cache policy"),
                                );
                                response
                                    .headers_mut()
                                    .insert("age", "0".parse().expect("constant origin age"));
                                response
                            }
                            Err(_) => unavailable(),
                        },
                        Ok(Err(())) | Err(_) => unavailable(),
                    }
                }
                _ => response(StatusCode::NOT_FOUND, "text/plain", b"not found\n".to_vec()),
            }
        };
        Ok(response)
    }
}

async fn wait_for_schema(config: &IdentityConfig) -> Result<Database, IdentityServiceError> {
    tokio::time::timeout(Duration::from_mins(5), async {
        loop {
            if let Ok(mut database) = connect(config).await
                && matches!(
                    tokio::time::timeout(
                        IO_TIMEOUT,
                        active_session_key_exists(&mut database.client, &config.issuer),
                    )
                    .await,
                    Ok(Ok(_))
                )
            {
                return database;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .map_err(|_| IdentityServiceError::new("wait for identity schema timed out"))
}

fn unavailable() -> Response<Full<Bytes>> {
    response(
        StatusCode::SERVICE_UNAVAILABLE,
        "application/json",
        b"{\"error\":\"identity unavailable\"}".to_vec(),
    )
}

fn response(
    status: StatusCode,
    content_type: &'static str,
    body: Vec<u8>,
) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .header("cache-control", "no-store")
        .body(Full::new(Bytes::from(body)))
        .expect("constant HTTP response metadata")
}

/// Parse the serving TLS certificate and private key.
pub fn tls_config(
    certificate_pem: &[u8],
    private_key_pem: &[u8],
) -> Result<rustls::ServerConfig, IdentityServiceError> {
    let certificates = CertificateDer::pem_slice_iter(certificate_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| IdentityServiceError::new("identity TLS certificate refused"))?;
    let key = PrivateKeyDer::from_pem_slice(private_key_pem)
        .map_err(|_| IdentityServiceError::new("identity TLS private key refused"))?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| IdentityServiceError::new("identity TLS versions refused"))?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| IdentityServiceError::new("identity TLS key pair refused"))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// Serve HTTPS identity routes; dropping this future closes connections.
pub async fn serve(
    listener: TcpListener,
    service: IdentityService,
    tls: rustls::ServerConfig,
) -> Result<(), IdentityServiceError> {
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let mut connections = JoinSet::new();
    let slots = Arc::new(tokio::sync::Semaphore::new(128));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (tcp, peer) = accepted.map_err(|_| IdentityServiceError::new("identity HTTPS accept failed"))?;
                let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else { drop(tcp); continue; };
                let acceptor = acceptor.clone();
                let service = service.clone();
                connections.spawn(async move {
                    let _slot = slot;
                    let Ok(Ok(tls)) = tokio::time::timeout(IO_TIMEOUT, acceptor.accept(tcp)).await else { return; };
                    let handler = service_fn(move |request| {
                        let service = service.clone();
                        async move { service.respond(request, peer.ip()).await }
                    });
                    let _ = http1::Builder::new().timer(TokioTimer::new())
                        .header_read_timeout(IO_TIMEOUT)
                        .serve_connection(TokioIo::new(tls), handler).await;
                });
            }
            completed = connections.join_next(), if !connections.is_empty() => {
                if completed.is_some_and(|result| result.is_err()) {
                    return Err(IdentityServiceError::new("identity HTTPS connection task failed"));
                }
            }
        }
    }
}

/// Fixed diagnostic text that cannot carry URLs, keys, or database error detail.
#[derive(Debug)]
pub struct IdentityServiceError {
    context: &'static str,
}

impl IdentityServiceError {
    pub(crate) fn new(context: &'static str) -> Self {
        Self { context }
    }
}

impl fmt::Display for IdentityServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.context)
    }
}

impl std::error::Error for IdentityServiceError {}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}
