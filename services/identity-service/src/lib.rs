//! The identity authority serves public keys, user login, and operator user management.

use sqlx::Connection as _;
use sqlx::PgConnection as Client;
pub mod cli;
pub mod mail;
mod password;
mod session;

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
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use tokio_rustls::TlsAcceptor;
use wamn_identity::session_keys::session_jwks;
use wamn_provision::identity_role::identity_issuer_login;

// Match the public-key client's total fetch bound. No retry or stale response
// is introduced when TLS, request headers, or the authoritative read stalls.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// The default signing key rotation interval: 30 days.
pub const DEFAULT_KEY_ROTATION: Duration = Duration::from_hours(30 * 24);

/// Validated exact issuer and its narrow database credential.
#[derive(Clone)]
pub struct IdentityConfig {
    issuer: String,
    key_rotation: Duration,
    trusted_proxy_hops: usize,
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
            key_rotation: DEFAULT_KEY_ROTATION,
            trusted_proxy_hops: 0,
            connection,
            mail: None,
        })
    }

    /// Trust this many proxy addresses at the end of the forwarded chain. Zero uses the peer.
    pub fn with_trusted_proxy_hops(mut self, hops: usize) -> Self {
        self.trusted_proxy_hops = hops;
        self
    }

    /// Set the signing key schedule. The previous public key retains its token window.
    pub fn with_key_rotation(mut self, interval: Duration) -> Result<Self, IdentityServiceError> {
        if interval.as_secs() == 0 || i64::try_from(interval.as_secs()).is_err() {
            return Err(IdentityServiceError::new(
                "signing key interval must be positive",
            ));
        }
        self.key_rotation = interval;
        Ok(self)
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
    key_rotation: Duration,
    trusted_proxy_hops: usize,
    // Every held connection opens again with this connection string after Postgres
    // closes it, for example when the server restarts.
    connection: String,
    passwords: Option<password::State>,
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
            self.key_rotation,
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
    pub async fn connect(config: IdentityConfig) -> Result<Self, IdentityServiceError> {
        let envelope = signing_envelope()?;
        let database = connect(&config).await?;
        let passwords = if let Some(mail) = config.mail {
            Some(password::State::new(mail, &config.connection).await?)
        } else {
            None
        };
        Ok(Self {
            inner: Arc::new(Inner {
                issuer: config.issuer,
                key_rotation: config.key_rotation,
                trusted_proxy_hops: config.trusted_proxy_hops,
                envelope,
                connection: config.connection,
                passwords,
                database: Mutex::new(database),
            }),
        })
    }

    async fn respond(
        &self,
        request: Request<Incoming>,
        operator: bool,
        source: std::net::IpAddr,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let source = if self.inner.trusted_proxy_hops == 0
            || !request.headers().contains_key("x-forwarded-for")
        {
            source
        } else {
            match request
                .headers()
                .get_all("x-forwarded-for")
                .iter()
                .rev()
                .flat_map(|value| value.as_bytes().rsplit(|byte| *byte == b','))
                .nth(self.inner.trusted_proxy_hops - 1)
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(|value| value.trim().parse::<std::net::IpAddr>().ok())
            {
                Some(address) => address,
                None => {
                    return Ok(response(
                        StatusCode::BAD_REQUEST,
                        "application/json",
                        b"{\"error\":\"invalid client address\"}".to_vec(),
                    ));
                }
            }
        };
        if request.method() == Method::POST
            && request.uri().query().is_none()
            && matches!(
                request.uri().path(),
                "/invitations"
                    | "/users"
                    | "/password/enroll"
                    | "/password/session"
                    | "/password/renew"
                    | "/password/logout"
                    | "/password/logout-all"
                    | "/password/recover"
                    | "/password/reset"
            )
            && let Some(state) = &self.inner.passwords
        {
            return Ok(password::respond(&self.inner, state, request, operator, source).await);
        }
        let response = if request.method() != Method::GET || request.uri().query().is_some() {
            response(StatusCode::NOT_FOUND, "text/plain", b"not found\n".to_vec())
        } else {
            match request.uri().path() {
                "/healthz" => match self.inner.database().await {
                    Ok(_) => response(StatusCode::OK, "text/plain", b"ok\n".to_vec()),
                    Err(_) => unavailable(),
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

fn signing_envelope() -> Result<wamn_envelope::encryption::Envelope, IdentityServiceError> {
    let configuration = std::env::var("WAMN_OAUTH_ENCRYPTION")
        .map_err(|_| IdentityServiceError::new("signing key envelope configuration is required"))?;
    let configuration = serde_json::from_str(&configuration)
        .map_err(|_| IdentityServiceError::new("signing key envelope configuration refused"))?;
    wamn_envelope::encryption::Envelope::open(&configuration)
        .map_err(|_| IdentityServiceError::new("open signing key envelope failed"))
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

/// Parse mounted PEM material into the service's TLS-only listener configuration.
pub fn tls_config(
    certificate_pem: &[u8],
    private_key_pem: &[u8],
) -> Result<rustls::ServerConfig, IdentityServiceError> {
    listener_tls_config(certificate_pem, private_key_pem, None)
}

/// Trust only the dedicated operator CA for optional TLS client authentication.
///
/// Anonymous clients can read public keys and use password login.
/// Only a verified client certificate authorizes PAT issuance.
pub fn tls_config_with_operator_ca(
    certificate_pem: &[u8],
    private_key_pem: &[u8],
    operator_ca_pem: &[u8],
) -> Result<rustls::ServerConfig, IdentityServiceError> {
    listener_tls_config(certificate_pem, private_key_pem, Some(operator_ca_pem))
}

fn listener_tls_config(
    certificate_pem: &[u8],
    private_key_pem: &[u8],
    operator_ca_pem: Option<&[u8]>,
) -> Result<rustls::ServerConfig, IdentityServiceError> {
    let certificates = CertificateDer::pem_slice_iter(certificate_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| IdentityServiceError::new("identity TLS certificate refused"))?;
    let key = PrivateKeyDer::from_pem_slice(private_key_pem)
        .map_err(|_| IdentityServiceError::new("identity TLS private key refused"))?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier =
        match operator_ca_pem {
            Some(pem) => {
                let mut roots = rustls::RootCertStore::empty();
                for certificate in CertificateDer::pem_slice_iter(pem) {
                    roots
                        .add(certificate.map_err(|_| {
                            IdentityServiceError::new("identity operator CA refused")
                        })?)
                        .map_err(|_| IdentityServiceError::new("identity operator CA refused"))?;
                }
                rustls::server::WebPkiClientVerifier::builder_with_provider(
                    Arc::new(roots),
                    Arc::clone(&provider),
                )
                .allow_unauthenticated()
                .build()
                .map_err(|_| IdentityServiceError::new("identity operator CA refused"))?
            }
            None => rustls::server::WebPkiClientVerifier::no_client_auth(),
        };
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| IdentityServiceError::new("identity TLS versions refused"))?
        .with_client_cert_verifier(verifier)
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
                    // Only the accepted TLS session supplies operator authority.
                    // Certificate headers and bearer credentials cannot set it.
                    let operator = tls.get_ref().1.peer_certificates()
                        .is_some_and(|certificates| !certificates.is_empty());
                    let handler = service_fn(move |request| {
                        let service = service.clone();
                        async move { service.respond(request, operator, peer.ip()).await }
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
