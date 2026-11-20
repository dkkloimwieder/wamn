//! Shared Hyper transport, TLS trust, request deadlines and response bodies.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::body::{Body, Incoming};
use hyper::{Request, Response, StatusCode, header};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::{
    Builder, Client,
    connect::{
        Connect, HttpConnector,
        dns::{GaiResolver, Name},
    },
};
use hyper_util::rt::{TokioExecutor, TokioTimer};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};
use tokio::time::{Instant, timeout_at};
use tower_service::Service;
use url::Url;

fn builder() -> Builder {
    Client::builder(TokioExecutor::new())
}

/// The URL schemes a TLS connector accepts.
#[derive(Clone, Copy, Debug)]
pub enum Schemes {
    /// Accept HTTPS endpoints only.
    HttpsOnly,
    /// Accept both HTTP and HTTPS endpoints.
    HttpAndHttps,
}

/// The HTTP protocols a TLS connector negotiates.
#[derive(Clone, Copy, Debug)]
pub enum Protocols {
    /// Negotiate HTTP/1 only.
    Http1,
    /// Negotiate HTTP/1 and HTTP/2.
    Http1AndHttp2,
}

/// Wrap a TCP connector with the caller's TLS trust and protocol policy.
///
/// # Panics
/// Panics if `tls` already contains ALPN protocols.
pub fn tls_connector<R>(
    tls: rustls::ClientConfig,
    mut http: HttpConnector<R>,
    schemes: Schemes,
    protocols: Protocols,
) -> HttpsConnector<HttpConnector<R>> {
    http.enforce_http(false);
    let builder = HttpsConnectorBuilder::new().with_tls_config(tls);
    let builder = match schemes {
        Schemes::HttpsOnly => builder.https_only(),
        Schemes::HttpAndHttps => builder.https_or_http(),
    }
    .enable_http1();
    match protocols {
        Protocols::Http1 => builder.wrap_connector(http),
        Protocols::Http1AndHttp2 => builder.enable_http2().wrap_connector(http),
    }
}

/// Construct a pooled client without changing its request or response body type.
pub fn client<C, B>(connector: C, configure: impl FnOnce(&mut Builder)) -> Client<C, B>
where
    C: Connect + Clone,
    B: Body + Send + 'static,
    B::Data: Send,
{
    let mut builder = builder();
    configure(&mut builder);
    builder.build(connector)
}

/// Construct a plaintext client with Hyper's TCP keepalive and pool defaults.
pub fn http_client<B>() -> Client<HttpConnector, B>
where
    B: Body + Send + 'static,
    B::Data: Send,
{
    builder().build_http()
}

type Transport = Client<HttpsConnector<HttpConnector<Resolver>>, Full<Bytes>>;
type BoxError = Box<dyn StdError + Send + Sync>;

/// Whether a client follows HTTP redirects.
#[derive(Clone, Copy, Debug, Default)]
pub enum Redirects {
    /// Return the first response.
    None,
    /// Follow at most ten redirects, removing credentials on another origin.
    #[default]
    Follow,
}

/// A shared Hyper client, with trust and connection policy fixed at construction.
#[derive(Debug)]
pub struct HttpClient(Arc<ClientState>);

#[derive(Debug)]
struct ClientState {
    transport: Transport,
    timeout: Option<Duration>,
    headers: header::HeaderMap,
    https_only: bool,
    redirects: Redirects,
}

impl Clone for HttpClient {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

/// Configuration for callers that need their own trust or connection policy.
#[derive(Default)]
pub struct HttpClientBuilder {
    tls: Option<rustls::ClientConfig>,
    timeout: Option<Duration>,
    headers: header::HeaderMap,
    https_only: bool,
    redirects: Redirects,
    addresses: HashMap<String, SocketAddr>,
    idle_per_host: Option<usize>,
    idle_timeout: Option<Duration>,
}

impl fmt::Debug for HttpClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpClientBuilder").finish_non_exhaustive()
    }
}

/// The platform verifier shared by trusted endpoints and pinned connection calls.
pub fn platform_tls() -> anyhow::Result<rustls::ClientConfig> {
    tls_with_extra_roots(Vec::new())
}

fn tls_with_extra_roots(
    roots: Vec<CertificateDer<'static>>,
) -> anyhow::Result<rustls::ClientConfig> {
    let provider = rustls::crypto::CryptoProvider::get_default().map_or_else(
        || Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        Arc::clone,
    );
    let verifier =
        rustls_platform_verifier::Verifier::new_with_extra_roots(roots, Arc::clone(&provider))?;
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

fn certificates(pem: &[u8]) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let roots = CertificateDer::pem_slice_iter(pem).collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(!roots.is_empty(), "HTTPS trust bundle is empty");
    Ok(roots)
}

impl HttpClientBuilder {
    /// Bound the complete request, including response body reads.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Refuse plaintext endpoints when enabled.
    pub fn https_only(mut self, https_only: bool) -> Self {
        self.https_only = https_only;
        self
    }

    /// Set the redirect policy. Requests are never automatically retried.
    pub fn redirects(mut self, redirects: Redirects) -> Self {
        self.redirects = redirects;
        self
    }

    /// Add default request headers.
    pub fn default_headers(mut self, headers: header::HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    /// Set the maximum idle connections retained per host.
    pub fn pool_max_idle_per_host(mut self, count: usize) -> Self {
        self.idle_per_host = Some(count);
        self
    }

    /// Connect to an explicit address while retaining the URL's TLS identity.
    pub fn resolve(mut self, host: &str, address: SocketAddr) -> Self {
        self.addresses.insert(host.to_owned(), address);
        self
    }

    /// Add a PEM trust bundle to the platform roots.
    pub fn extra_roots(mut self, pem: &[u8]) -> anyhow::Result<Self> {
        self.tls = Some(tls_with_extra_roots(certificates(pem)?)?);
        Ok(self)
    }

    /// Replace ambient trust with a nonempty PEM bundle.
    pub fn roots(mut self, pem: &[u8]) -> anyhow::Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        for certificate in certificates(pem)? {
            roots.add(certificate)?;
        }
        let provider = rustls::crypto::CryptoProvider::get_default().map_or_else(
            || Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
            Arc::clone,
        );
        self.tls = Some(
            rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()?
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        Ok(self)
    }

    /// Use the certificate chain and private key in a PEM bundle for mutual TLS.
    pub fn identity_pem(mut self, pem: &[u8]) -> anyhow::Result<Self> {
        let chain = certificates(pem)?;
        let key = PrivateKeyDer::from_pem_slice(pem).context("read HTTPS client private key")?;
        let mut tls = self.tls.take().map_or_else(platform_tls, Ok)?;
        tls.client_auth_cert_resolver = Arc::new(rustls::sign::SingleCertAndKey::from(
            rustls::sign::CertifiedKey::from_der(chain, key, tls.crypto_provider())?,
        ));
        self.tls = Some(tls);
        Ok(self)
    }

    /// Construct a client. Proxy configuration is never read from the environment.
    pub fn build(self) -> anyhow::Result<HttpClient> {
        let tls = self.tls.map_or_else(platform_tls, Ok)?;
        let connector = tls_connector(
            tls,
            HttpConnector::new_with_resolver(Resolver(Arc::new(self.addresses))),
            Schemes::HttpAndHttps,
            Protocols::Http1AndHttp2,
        );
        let transport = client(connector, |builder| {
            builder
                .timer(TokioTimer::new())
                .pool_timer(TokioTimer::new())
                .retry_canceled_requests(false);
            if let Some(timeout) = self.idle_timeout {
                builder.pool_idle_timeout(timeout);
            }
            if let Some(count) = self.idle_per_host {
                builder.pool_max_idle_per_host(count);
            }
        });
        Ok(HttpClient(Arc::new(ClientState {
            transport,
            timeout: self.timeout,
            headers: self.headers,
            https_only: self.https_only,
            redirects: self.redirects,
        })))
    }
}

impl HttpClient {
    fn platform_builder() -> HttpClientBuilder {
        HttpClientBuilder {
            redirects: Redirects::None,
            idle_per_host: Some(2),
            idle_timeout: Some(Duration::from_secs(30)),
            ..Self::builder()
        }
    }

    /// Configure a client for an endpoint's trust and connection policy.
    pub fn builder() -> HttpClientBuilder {
        HttpClientBuilder::default()
    }

    /// Add registry trust roots to the platform roots through the shared transport.
    pub fn with_extra_roots(bundles: &[Vec<u8>]) -> anyhow::Result<Self> {
        if bundles.is_empty() {
            return Self::shared();
        }
        let roots = bundles
            .iter()
            .map(|pem| certificates(pem))
            .collect::<Result<Vec<_>, _>>()?;
        let builder = HttpClientBuilder {
            tls: Some(tls_with_extra_roots(roots.into_iter().flatten().collect())?),
            ..Self::platform_builder()
        };
        builder.build()
    }

    /// Reuse the process client for platform endpoints and metadata requests.
    pub fn shared() -> anyhow::Result<Self> {
        static CLIENT: OnceLock<HttpClient> = OnceLock::new();
        if let Some(client) = CLIENT.get() {
            return Ok(client.clone());
        }
        let client = Self::platform_builder().build()?;
        Ok(CLIENT.get_or_init(|| client).clone())
    }

    /// Trust only this nonempty PEM bundle, without ambient certificate roots.
    pub fn with_roots(pem: &[u8]) -> anyhow::Result<Self> {
        Self::platform_builder().roots(pem)?.build()
    }

    /// Send once and collect a bounded body within the complete request deadline.
    pub async fn send(
        &self,
        request: Request<Bytes>,
        timeout: Duration,
        body_limit: usize,
    ) -> anyhow::Result<Response<Bytes>> {
        let deadline = Some(Instant::now() + timeout);
        let response = self.send_head(request, deadline).await?;
        let (parts, body) = response.into_parts();
        let bytes = HttpResponse {
            parts,
            body,
            deadline,
        }
        .collect(body_limit)
        .await?;
        Ok(Response::from_parts(bytes.0, bytes.1))
    }

    /// Prepare a GET request.
    pub fn get(&self, url: impl AsRef<str>) -> HttpRequest {
        self.request(hyper::Method::GET, url)
    }

    /// Prepare a POST request.
    pub fn post(&self, url: impl AsRef<str>) -> HttpRequest {
        self.request(hyper::Method::POST, url)
    }

    /// Prepare a PUT request.
    pub fn put(&self, url: impl AsRef<str>) -> HttpRequest {
        self.request(hyper::Method::PUT, url)
    }

    /// Prepare a request. Invalid URLs and headers fail before dispatch.
    pub fn request(&self, method: hyper::Method, url: impl AsRef<str>) -> HttpRequest {
        let request = Url::parse(url.as_ref())
            .map_err(|_| HttpError::input())
            .and_then(|mut url| {
                if !matches!(url.scheme(), "http" | "https")
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    return Err(HttpError::input());
                }
                url.set_fragment(None);
                let request = Request::builder()
                    .method(method)
                    .uri(url.as_str())
                    .body(Bytes::new())
                    .map_err(|_| HttpError::input())?;
                Ok(request)
            });
        HttpRequest {
            client: self.clone(),
            request,
        }
    }

    async fn send_head(
        &self,
        request: Request<Bytes>,
        deadline: Option<Instant>,
    ) -> Result<Response<Incoming>, HttpError> {
        if self.0.https_only && request.uri().scheme_str() != Some("https") {
            return Err(HttpError::input());
        }
        within(deadline, self.0.transport.request(request.map(Full::new)))
            .await?
            .map_err(|source| {
                HttpError::new(
                    if source.is_connect() {
                        ErrorType::Connect
                    } else {
                        ErrorType::Transport
                    },
                    source,
                )
            })
    }
}

/// One prepared HTTP request.
pub struct HttpRequest {
    client: HttpClient,
    request: Result<Request<Bytes>, HttpError>,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest").finish_non_exhaustive()
    }
}

impl HttpRequest {
    /// Add request headers.
    pub fn headers(mut self, headers: header::HeaderMap) -> Self {
        if let Ok(request) = &mut self.request {
            request.headers_mut().extend(headers);
        }
        self
    }
    /// Add a request header.
    pub fn header<K, V>(mut self, name: K, value: V) -> Self
    where
        header::HeaderName: TryFrom<K>,
        header::HeaderValue: TryFrom<V>,
    {
        self.request = self.request.and_then(|mut request| {
            request.headers_mut().append(
                header::HeaderName::try_from(name).map_err(|_| HttpError::input())?,
                header::HeaderValue::try_from(value).map_err(|_| HttpError::input())?,
            );
            Ok(request)
        });
        self
    }

    /// Add a bearer credential without recording it in diagnostics.
    pub fn bearer_auth(self, token: impl fmt::Display) -> Self {
        self.header(header::AUTHORIZATION, format!("Bearer {token}"))
    }

    /// Set the request body.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        if let Ok(request) = &mut self.request {
            *request.body_mut() = body.into();
        }
        self
    }

    /// Encode JSON and set its media type.
    pub fn json(mut self, value: &(impl serde::Serialize + ?Sized)) -> Self {
        self.request = self.request.and_then(|mut request| {
            *request.body_mut() = serde_json::to_vec(value)
                .map_err(|_| HttpError::input())?
                .into();
            request
                .headers_mut()
                .entry(header::CONTENT_TYPE)
                .or_insert(header::HeaderValue::from_static("application/json"));
            Ok(request)
        });
        self
    }

    /// Send without retries. The same deadline covers redirects and body reads.
    pub async fn send(self) -> Result<HttpResponse, HttpError> {
        let mut request = self.request?;
        for name in self.client.0.headers.keys() {
            if !request.headers().contains_key(name) {
                for value in self.client.0.headers.get_all(name) {
                    request.headers_mut().append(name, value.clone());
                }
            }
        }
        request
            .headers_mut()
            .entry(header::ACCEPT)
            .or_insert(header::HeaderValue::from_static("*/*"));
        let deadline = self
            .client
            .0
            .timeout
            .map(|timeout| Instant::now() + timeout);
        let follow = matches!(self.client.0.redirects, Redirects::Follow);
        for redirects in 0..=10 {
            // Bytes shares the payload allocation only when a redirect can need it.
            let retry = follow.then(|| {
                (
                    request.method().clone(),
                    request.uri().clone(),
                    request.headers().clone(),
                    request.body().clone(),
                )
            });
            let response = self.client.send_head(request, deadline).await?;
            let status = response.status();
            let redirect = matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308);
            let Some((mut method, uri, mut headers, mut body)) =
                retry.filter(|_| redirect && response.headers().contains_key(header::LOCATION))
            else {
                let (parts, body) = response.into_parts();
                return Ok(HttpResponse {
                    parts,
                    body,
                    deadline,
                });
            };
            let mut previous = Url::parse(&uri.to_string()).map_err(|_| HttpError::input())?;
            let next = response.headers()[header::LOCATION]
                .to_str()
                .ok()
                .and_then(|location| previous.join(location).ok());
            let Some(mut next) = next else {
                let (parts, body) = response.into_parts();
                return Ok(HttpResponse {
                    parts,
                    body,
                    deadline,
                });
            };
            next.set_fragment(None);
            if redirects == 10 {
                return Err(HttpError::new(
                    ErrorType::Redirect,
                    std::io::Error::other("HTTP redirect limit exceeded"),
                ));
            }
            if !matches!(next.scheme(), "http" | "https")
                || !next.username().is_empty()
                || next.password().is_some()
            {
                return Err(HttpError::input());
            }
            if previous.origin() != next.origin() {
                headers.remove("cookie2");
                for name in [
                    header::AUTHORIZATION,
                    header::COOKIE,
                    header::PROXY_AUTHORIZATION,
                    header::WWW_AUTHENTICATE,
                ] {
                    headers.remove(name);
                }
            }
            headers.remove(header::HOST);
            if !(previous.scheme() == "https" && next.scheme() == "http") {
                previous.set_fragment(None);
                if let Ok(referer) = previous.as_str().parse() {
                    headers.insert(header::REFERER, referer);
                }
            }
            if status == StatusCode::SEE_OTHER
                || (matches!(status.as_u16(), 301 | 302) && method == hyper::Method::POST)
            {
                if method != hyper::Method::HEAD {
                    method = hyper::Method::GET;
                }
                body = Bytes::new();
                for name in [
                    header::CONTENT_TYPE,
                    header::CONTENT_LENGTH,
                    header::CONTENT_ENCODING,
                    header::TRANSFER_ENCODING,
                ] {
                    headers.remove(name);
                }
            }
            request = Request::builder()
                .method(method)
                .uri(next.as_str())
                .body(body)
                .map_err(|_| HttpError::input())?;
            *request.headers_mut() = headers;
        }
        unreachable!("the last redirect returns an error")
    }
}

/// A response whose body reads retain the request deadline.
#[derive(Debug)]
pub struct HttpResponse {
    parts: hyper::http::response::Parts,
    body: Incoming,
    deadline: Option<Instant>,
}

impl HttpResponse {
    /// The HTTP response status.
    pub fn status(&self) -> StatusCode {
        self.parts.status
    }
    /// The HTTP response headers.
    pub fn headers(&self) -> &header::HeaderMap {
        &self.parts.headers
    }
    /// The declared body length, when present.
    pub fn content_length(&self) -> Option<u64> {
        self.headers()
            .get(header::CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    }
    /// Refuse an unsuccessful HTTP status without retaining the URL or body.
    pub fn error_for_status(self) -> Result<Self, HttpError> {
        if self.status().is_client_error() || self.status().is_server_error() {
            Err(HttpError {
                type_: ErrorType::Status,
                source: None,
                status: Some(self.status()),
            })
        } else {
            Ok(self)
        }
    }
    /// Read the next data frame within the original deadline.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, HttpError> {
        loop {
            match within(self.deadline, self.body.frame()).await? {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        return Ok(Some(data));
                    }
                }
                Some(Err(source)) => return Err(HttpError::new(ErrorType::Body, source)),
                None => return Ok(None),
            }
        }
    }
    async fn collect(
        self,
        limit: usize,
    ) -> Result<(hyper::http::response::Parts, Bytes), HttpError> {
        if self
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(HttpError::new(
                ErrorType::Body,
                std::io::Error::other("HTTP response body limit exceeded"),
            ));
        }
        let body = within(self.deadline, Limited::new(self.body, limit).collect())
            .await?
            .map_err(|source| HttpError::new(ErrorType::Body, source))?
            .to_bytes();
        Ok((self.parts, body))
    }
    /// Collect the response body.
    pub async fn bytes(self) -> Result<Bytes, HttpError> {
        Ok(self.collect(usize::MAX).await?.1)
    }
    /// Read response text, replacing invalid UTF-8.
    pub async fn text(self) -> Result<String, HttpError> {
        Ok(String::from_utf8_lossy(&self.bytes().await?).into_owned())
    }
    /// Decode a JSON response body.
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, HttpError> {
        serde_json::from_slice(&self.bytes().await?).map_err(|_| HttpError {
            type_: ErrorType::Body,
            source: None,
            status: None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum ErrorType {
    Input,
    Connect,
    Transport,
    Timeout,
    Body,
    Status,
    Redirect,
}

/// A typed HTTP failure. Diagnostics never retain request URLs or credentials.
#[derive(Debug)]
pub struct HttpError {
    type_: ErrorType,
    source: Option<BoxError>,
    status: Option<StatusCode>,
}

impl HttpError {
    fn new(type_: ErrorType, source: impl Into<BoxError>) -> Self {
        Self {
            type_,
            source: Some(source.into()),
            status: None,
        }
    }
    fn input() -> Self {
        Self {
            type_: ErrorType::Input,
            source: None,
            status: None,
        }
    }
    /// Whether no HTTP connection was established.
    pub fn is_connect(&self) -> bool {
        matches!(self.type_, ErrorType::Connect)
    }
    /// Whether the request deadline elapsed.
    pub fn is_timeout(&self) -> bool {
        matches!(self.type_, ErrorType::Timeout)
    }
    /// Whether reading or decoding the response body failed.
    pub fn is_body(&self) -> bool {
        matches!(self.type_, ErrorType::Body)
    }
    /// The unsuccessful response status, if this is a status failure.
    pub fn status(&self) -> Option<StatusCode> {
        self.status
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP {:?} failure", self.type_)
    }
}
impl StdError for HttpError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

async fn within<T>(
    deadline: Option<Instant>,
    future: impl Future<Output = T>,
) -> Result<T, HttpError> {
    match deadline {
        Some(deadline) => timeout_at(deadline, future)
            .await
            .map_err(|source| HttpError::new(ErrorType::Timeout, source)),
        None => Ok(future.await),
    }
}

#[derive(Debug)]
struct Resolver(Arc<HashMap<String, SocketAddr>>);
impl Clone for Resolver {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl Service<Name> for Resolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, name: Name) -> Self::Future {
        let address = self.0.get(name.as_str()).copied();
        Box::pin(async move {
            match address {
                Some(address) => Ok(vec![address].into_iter()),
                None => Ok(GaiResolver::new()
                    .call(name)
                    .await?
                    .collect::<Vec<_>>()
                    .into_iter()),
            }
        })
    }
}
