//! The separate identity authority exposes only public JWKS and HTTPS health.

pub mod cli;

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
use tokio::task::{JoinHandle, JoinSet};
use tokio_postgres::{Client, NoTls};
use tokio_rustls::TlsAcceptor;
use wamn_control_provision::identity_issuer::{
    IdentityIssuerConnection, parse_identity_issuer_url,
};
use wamn_platform_identity::session_keys::session_jwks;

// Match the public-key client's total fetch bound. No retry or stale response
// is introduced when TLS, request headers, or the authoritative read stalls.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Validated exact issuer and its narrow database credential.
#[derive(Clone, Debug)]
pub struct IdentityConfig {
    issuer: String,
    connection: IdentityIssuerConnection,
}

impl IdentityConfig {
    /// Validate the scoped credential before any database socket is opened.
    pub fn new(issuer: &str, database_url: &str) -> Result<Self, IdentityServiceError> {
        let connection = parse_identity_issuer_url(database_url, issuer)
            .map_err(|_| IdentityServiceError::new("identity database configuration refused"))?;
        Ok(Self {
            issuer: issuer.to_owned(),
            connection,
        })
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
    let (client, connection) = tokio::time::timeout(
        IO_TIMEOUT,
        tokio_postgres::connect(config.connection.url(), NoTls),
    )
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
        Ok(Self {
            inner: Arc::new(Inner {
                issuer: config.issuer,
                database,
            }),
        })
    }

    async fn respond(
        &self,
        request: Request<Incoming>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
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
    let certificates = CertificateDer::pem_slice_iter(certificate_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| IdentityServiceError::new("identity TLS certificate refused"))?;
    let key = PrivateKeyDer::from_pem_slice(private_key_pem)
        .map_err(|_| IdentityServiceError::new("identity TLS private key refused"))?;
    let mut config = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::aws_lc_rs::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .map_err(|_| IdentityServiceError::new("identity TLS versions refused"))?
    .with_no_client_auth()
    .with_single_cert(certificates, key)
    .map_err(|_| IdentityServiceError::new("identity TLS key pair refused"))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// Serve the two public routes; dropping this future closes owned connections.
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
                    let handler = service_fn(move |request| {
                        let service = service.clone();
                        async move { service.respond(request).await }
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
