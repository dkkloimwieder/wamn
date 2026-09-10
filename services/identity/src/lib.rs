//! The identity authority serves public keys, PAT exchanges, and operator PAT issuance.

pub mod cli;
mod pat;
mod session;

use std::collections::BTreeMap;
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
use tokio::task::{JoinHandle, JoinSet};
use tokio_postgres::{Client, NoTls};
use tokio_rustls::TlsAcceptor;
use wamn_control_provision::identity_issuer::{
    IdentityIssuerConnection, parse_identity_issuer_url,
};
use wamn_control_provision::session_target::SessionTarget;
use wamn_platform_identity::session_keys::session_jwks;

// Match the public-key client's total fetch bound. No retry or stale response
// is introduced when TLS, request headers, or the authoritative read stalls.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Validated exact issuer and its narrow database credential.
#[derive(Clone, Debug)]
pub struct IdentityConfig {
    issuer: String,
    connection: IdentityIssuerConnection,
    targets: BTreeMap<String, SessionTarget>,
}

impl IdentityConfig {
    /// Validate the scoped credential before any database socket is opened.
    pub fn new(issuer: &str, database_url: &str) -> Result<Self, IdentityServiceError> {
        let connection = parse_identity_issuer_url(database_url, issuer)
            .map_err(|_| IdentityServiceError::new("identity database configuration refused"))?;
        Ok(Self {
            issuer: issuer.to_owned(),
            connection,
            targets: BTreeMap::new(),
        })
    }

    /// Enable exchanges only for explicitly provisioned, nonduplicated audiences.
    pub fn with_session_targets(
        mut self,
        targets: Vec<SessionTarget>,
    ) -> Result<Self, IdentityServiceError> {
        for target in targets {
            if self
                .targets
                .insert(target.audience().to_owned(), target)
                .is_some()
            {
                return Err(IdentityServiceError::new(
                    "duplicate identity session audience",
                ));
            }
        }
        Ok(self)
    }
}

/// Shared service state containing no exported private key material.
#[derive(Clone, Debug)]
pub struct IdentityService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    issuer: String,
    database: Database,
    // Key reads remain available while a signer waits on the rotation barrier.
    signing: Option<Mutex<Database>>,
    targets: BTreeMap<String, ConfiguredTarget>,
}

#[derive(Debug)]
struct ConfiguredTarget {
    binding: SessionTarget,
    // One connection per used environment supports the existing live-successor
    // retirement rule. Unused environments consume no database connections.
    reader: Mutex<Option<Database>>,
}

pub(crate) struct Database {
    pub(crate) client: Client,
    driver: JoinHandle<()>,
}

impl fmt::Debug for Database {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Database")
            .field("connected", &!self.client.is_closed())
            .finish_non_exhaustive()
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

pub(crate) async fn connect(config: &IdentityConfig) -> Result<Database, IdentityServiceError> {
    // The parsed capability is the only URL passed to the driver. NoTls is the
    // repository's existing internal system-database transport contract.
    connect_database(config.connection.url()).await
}

// Callers supply only an already parsed issuer or session-reader capability.
pub(crate) async fn connect_database(url: &str) -> Result<Database, IdentityServiceError> {
    let (client, connection) =
        tokio::time::timeout(IO_TIMEOUT, tokio_postgres::connect(url, NoTls))
            .await
            .map_err(|_| IdentityServiceError::new("identity database connection timed out"))?
            .map_err(|_| IdentityServiceError::new("identity database connection failed"))?;
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(Database { client, driver })
}

impl IdentityService {
    /// Connect using already validated issuer-scoped authority.
    pub async fn connect(config: IdentityConfig) -> Result<Self, IdentityServiceError> {
        let database = connect(&config).await?;
        let signing = if config.targets.is_empty() {
            None
        } else {
            Some(Mutex::new(connect(&config).await?))
        };
        Ok(Self {
            inner: Arc::new(Inner {
                issuer: config.issuer,
                database,
                signing,
                targets: config
                    .targets
                    .into_iter()
                    .map(|(audience, binding)| {
                        (
                            audience,
                            ConfiguredTarget {
                                binding,
                                reader: Mutex::new(None),
                            },
                        )
                    })
                    .collect(),
            }),
        })
    }

    async fn respond(
        &self,
        request: Request<Incoming>,
        operator: bool,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        if request.method() == Method::POST
            && request.uri().path() == "/pats"
            && request.uri().query().is_none()
        {
            return Ok(pat::respond(&self.inner, request, operator).await);
        }
        if request.method() == Method::POST
            && request.uri().path() == "/session"
            && request.uri().query().is_none()
            && !self.inner.targets.is_empty()
        {
            return Ok(session::respond(&self.inner, request).await);
        }
        let response = if request.method() != Method::GET || request.uri().query().is_some() {
            response(StatusCode::NOT_FOUND, "text/plain", b"not found\n".to_vec())
        } else {
            match request.uri().path() {
                "/healthz" if !self.inner.database.client.is_closed() => {
                    response(StatusCode::OK, "text/plain", b"ok\n".to_vec())
                }
                "/healthz" => unavailable(),
                "/.well-known/jwks.json" => {
                    match tokio::time::timeout(
                        IO_TIMEOUT,
                        session_jwks(&self.inner.database.client, &self.inner.issuer),
                    )
                    .await
                    {
                        Ok(Ok(keys)) => match serde_json::to_vec(&keys) {
                            Ok(bytes) => {
                                let mut response =
                                    response(StatusCode::OK, "application/json", bytes);
                                response.headers_mut().insert(
                                    "cache-control",
                                    "public, max-age=300"
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
                        Ok(Err(_)) | Err(_) => unavailable(),
                    }
                }
                _ => response(StatusCode::NOT_FOUND, "text/plain", b"not found\n".to_vec()),
            }
        };
        Ok(response)
    }
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
/// Anonymous clients retain access to public keys and configured PAT exchanges.
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
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (tcp, _) = accepted.map_err(|_| IdentityServiceError::new("identity HTTPS accept failed"))?;
                let acceptor = acceptor.clone();
                let service = service.clone();
                connections.spawn(async move {
                    let Ok(Ok(tls)) = tokio::time::timeout(IO_TIMEOUT, acceptor.accept(tcp)).await else { return; };
                    // Only the accepted TLS session supplies operator authority.
                    // Certificate headers and bearer credentials cannot set it.
                    let operator = tls.get_ref().1.peer_certificates()
                        .is_some_and(|certificates| !certificates.is_empty());
                    let handler = service_fn(move |request| {
                        let service = service.clone();
                        async move { service.respond(request, operator).await }
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
