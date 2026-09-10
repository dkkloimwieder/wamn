//! Pinned HTTP transport with bounded admission, pooling, and response collection.

use std::{
    collections::HashMap,
    error::Error as StdError,
    fmt,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::http::uri::Authority;
use hyper::{HeaderMap, Request, Response, Uri, header};
use hyper_util::{
    client::legacy::{
        Client,
        connect::{Connected, Connection},
    },
    rt::{TokioIo, TokioTimer},
};
use rustls::pki_types::ServerName;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{Instant, timeout, timeout_at},
};
use tokio_rustls::TlsConnector;
use tower_service::Service;

use crate::connection_authority::{AuthorityDecision, HttpScheme, TlsIdentity, TransportDecision};

// Owner-approved limits apply across bindings and credential generations.
pub(crate) const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_HEADER_BYTES: usize = 32 * 1024;
pub(crate) const MAX_HEADERS: usize = 100;
const MAX_CLIENTS: usize = 128;
const MAX_SOCKETS: usize = 64;
const MAX_REQUESTS: usize = 32;
const MAX_SCOPE_SOCKETS: usize = 8;
const MAX_SCOPE_REQUESTS: usize = 8;
const MAX_IDLE_SOCKETS: usize = 2;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

type BoxError = Box<dyn StdError + Send + Sync>;
type HttpClient = Client<PinnedConnector, Full<Bytes>>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ConnectionScope {
    pub(crate) tenant: Box<str>,
    pub(crate) project: Box<str>,
    pub(crate) environment: Box<str>,
    pub(crate) instance: Box<str>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ClientScope {
    pub(crate) connection: ConnectionScope,
    pub(crate) package: Box<str>,
    pub(crate) component_digest: Box<str>,
    pub(crate) requirement: Box<str>,
    pub(crate) binding_hash: Box<str>,
    pub(crate) definition_hash: Box<str>,
    pub(crate) generation: i64,
}

/// Shares pinned clients and hard resource limits across host stores.
#[derive(Clone)]
pub struct HttpTransport {
    inner: Arc<Inner>,
}

impl fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpTransport").finish_non_exhaustive()
    }
}

struct Inner {
    state: Mutex<State>,
    clients: Arc<Semaphore>,
    sockets: Arc<Semaphore>,
    requests: Arc<Semaphore>,
    tls: Arc<rustls::ClientConfig>,
}

#[derive(Default)]
struct State {
    clients: HashMap<ClientKey, CachedClient>,
    scopes: HashMap<ConnectionScope, Weak<ScopeQuota>>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct ClientKey {
    scope: ClientScope,
    target: Target,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct Target {
    authority: Box<str>,
    peer: SocketAddr,
    tls_name: Option<Box<str>>,
}

struct CachedClient {
    http: HttpClient,
    life: Arc<ClientLife>,
    used: Instant,
}

struct ScopeQuota {
    sockets: Arc<Semaphore>,
    requests: Arc<Semaphore>,
}

struct ClientLife {
    // Connection tasks, connecting futures, and actual sockets retain this lease.
    _permit: OwnedSemaphorePermit,
    quota: Arc<ScopeQuota>,
}

impl HttpTransport {
    /// Uses the platform trust verifier and HTTP/2 or HTTP/1.1 TLS negotiation.
    ///
    /// # Errors
    /// Returns an error if the platform TLS verifier cannot initialize.
    pub fn new() -> anyhow::Result<Self> {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
        let verifier = rustls_platform_verifier::Verifier::new(Arc::clone(&provider))?;
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        Ok(Self::from_tls(tls))
    }

    fn from_tls(mut tls: rustls::ClientConfig) -> Self {
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                clients: Arc::new(Semaphore::new(MAX_CLIENTS)),
                sockets: Arc::new(Semaphore::new(MAX_SOCKETS)),
                requests: Arc::new(Semaphore::new(MAX_REQUESTS)),
                tls: Arc::new(tls),
            }),
        }
    }

    pub(crate) async fn execute(
        &self,
        scope: ClientScope,
        decision: &AuthorityDecision,
        mut request: Request<&[u8]>,
    ) -> Result<Response<Bytes>, TransportError> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let target = target(decision, request.uri())?;
        if request.body().len() > MAX_BODY_BYTES {
            return Err(input("HTTP request body limit exceeded"));
        }
        let host = decision
            .host_header
            .parse()
            .map_err(|error| input("Invalid HTTP host header").with_source(error))?;
        request.headers_mut().insert(header::HOST, host);
        let length = header::HeaderValue::from(request.body().len());
        request.headers_mut().insert(header::CONTENT_LENGTH, length);
        header_budget(request.headers(), (0, 0), Phase::BeforeDispatch)?;

        let _global = acquire(&self.inner.requests, "HTTP request capacity exhausted")?;
        let (http, life) = self.client(scope, target)?;
        let _scoped = acquire(
            &life.quota.requests,
            "HTTP connection request capacity exhausted",
        )?;
        // Admission precedes the only owned request-body copy.
        let request = request.map(|body| Full::new(Bytes::copy_from_slice(body)));
        let mut phase = Phase::AwaitingHead;
        let result = timeout_at(deadline, async {
            let response = http.request(request).await.map_err(head_error)?;
            phase = Phase::ResponseBody;
            let (parts, body) = response.into_parts();
            let mut budget = header_budget(&parts.headers, (0, 0), phase)?;
            let mut body = Limited::new(body, MAX_BODY_BYTES);
            let mut bytes = BytesMut::new();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|error| {
                    TransportError::new(ErrorKind::Transport, phase, "HTTP response body lost")
                        .with_source(error)
                })?;
                if let Some(data) = frame.data_ref() {
                    bytes.extend_from_slice(data);
                }
                if let Some(trailers) = frame.trailers_ref() {
                    budget = header_budget(trailers, budget, phase)?;
                }
            }
            Ok(Response::from_parts(parts, bytes.freeze()))
        })
        .await;
        match result {
            Ok(result) => result,
            Err(error) => Err(TransportError::new(
                ErrorKind::Timeout,
                phase,
                "HTTP transport deadline exceeded",
            )
            .with_source(error)),
        }
    }

    fn client(
        &self,
        scope: ClientScope,
        target: Target,
    ) -> Result<(HttpClient, Arc<ClientLife>), TransportError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| input("HTTP transport state unavailable"))?;
        let now = Instant::now();
        state
            .clients
            .retain(|_, client| now.duration_since(client.used) < IDLE_TIMEOUT);
        let key = ClientKey { scope, target };
        if let Some(client) = state.clients.get_mut(&key) {
            client.used = now;
            return Ok((client.http.clone(), Arc::clone(&client.life)));
        }
        if self.inner.clients.available_permits() == 0 {
            let oldest = state
                .clients
                .iter()
                .min_by_key(|(_, client)| client.used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                state.clients.remove(&oldest);
            }
        }
        // Eviction does not grant capacity if a retired client's tasks still live.
        let permit = acquire(&self.inner.clients, "HTTP client capacity exhausted")?;
        state.scopes.retain(|_, quota| quota.strong_count() != 0);
        let quota = state
            .scopes
            .get(&key.scope.connection)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let quota = Arc::new(ScopeQuota {
                    sockets: Arc::new(Semaphore::new(MAX_SCOPE_SOCKETS)),
                    requests: Arc::new(Semaphore::new(MAX_SCOPE_REQUESTS)),
                });
                state
                    .scopes
                    .insert(key.scope.connection.clone(), Arc::downgrade(&quota));
                quota
            });
        let life = Arc::new(ClientLife {
            _permit: permit,
            quota,
        });
        let connector = PinnedConnector {
            target: key.target.clone(),
            tls: Arc::clone(&self.inner.tls),
            sockets: Arc::clone(&self.inner.sockets),
            life: Arc::clone(&life),
        };
        let http = Client::builder(ClientExecutor {
            life: Arc::clone(&life),
        })
        .pool_max_idle_per_host(MAX_IDLE_SOCKETS)
        .pool_idle_timeout(IDLE_TIMEOUT)
        .pool_timer(TokioTimer::new())
        .timer(TokioTimer::new())
        .retry_canceled_requests(false)
        .http1_max_buf_size(MAX_HEADER_BYTES)
        .http1_max_headers(MAX_HEADERS)
        .http2_max_header_list_size(
            u32::try_from(MAX_HEADER_BYTES).expect("32 KiB header limit fits u32"),
        )
        .build(connector);
        state.clients.insert(
            key,
            CachedClient {
                http: http.clone(),
                life: Arc::clone(&life),
                used: now,
            },
        );
        Ok((http, life))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    BeforeDispatch,
    AwaitingHead,
    ResponseBody,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ErrorKind {
    Input,
    Limit,
    Timeout,
    Transport,
}

pub(crate) struct TransportError {
    kind: ErrorKind,
    phase: Phase,
    detail: &'static str,
    source: Option<BoxError>,
}

impl TransportError {
    fn new(kind: ErrorKind, phase: Phase, detail: &'static str) -> Self {
        Self {
            kind,
            phase,
            detail,
            source: None,
        }
    }

    fn with_source(mut self, source: impl Into<BoxError>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub(crate) fn is_before_dispatch(&self) -> bool {
        self.phase == Phase::BeforeDispatch
    }

    pub(crate) fn is_timeout(&self) -> bool {
        self.kind == ErrorKind::Timeout && self.phase != Phase::ResponseBody
    }

    pub(crate) fn is_response_lost(&self) -> bool {
        self.phase == Phase::ResponseBody
    }
}

impl fmt::Debug for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportError")
            .field("kind", &self.kind)
            .field("phase", &self.phase)
            .field("detail", &self.detail)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.detail)
    }
}

impl StdError for TransportError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

fn acquire(
    semaphore: &Arc<Semaphore>,
    detail: &'static str,
) -> Result<OwnedSemaphorePermit, TransportError> {
    Arc::clone(semaphore).try_acquire_owned().map_err(|error| {
        TransportError::new(ErrorKind::Limit, Phase::BeforeDispatch, detail).with_source(error)
    })
}

fn input(detail: &'static str) -> TransportError {
    TransportError::new(ErrorKind::Input, Phase::BeforeDispatch, detail)
}

fn head_error(error: hyper_util::client::legacy::Error) -> TransportError {
    let mut phase = Phase::AwaitingHead;
    let mut kind = ErrorKind::Transport;
    let mut source: Option<&(dyn StdError + 'static)> = Some(&error);
    while let Some(current) = source {
        if let Some(transport) = current.downcast_ref::<TransportError>() {
            if transport.is_before_dispatch() {
                phase = Phase::BeforeDispatch;
            }
            if transport.is_timeout() {
                kind = ErrorKind::Timeout;
            }
        }
        source = current.source();
    }
    TransportError::new(kind, phase, "HTTP request failed").with_source(error)
}

fn target(decision: &AuthorityDecision, uri: &Uri) -> Result<Target, TransportError> {
    let TransportDecision::Direct { origin } = &decision.transport else {
        return Err(input("HTTP proxy transport is not admitted"));
    };
    let logical_uri: Uri = decision
        .logical_url
        .parse()
        .map_err(|error| input("Invalid HTTP logical URL").with_source(error))?;
    if uri != &logical_uri {
        return Err(input("HTTP request does not match its authority decision"));
    }
    let (scheme, tls_name) = match (decision.logical_authority.scheme(), &origin.tls_identity) {
        (HttpScheme::Http, None) => ("http", None),
        (HttpScheme::Https, Some(TlsIdentity::Dns(name)))
            if name.as_ref() == decision.logical_authority.host() =>
        {
            ("https", Some(name.clone()))
        }
        (HttpScheme::Https, Some(TlsIdentity::Ip(ip)))
            if ip.to_string() == decision.logical_authority.host() =>
        {
            ("https", Some(decision.logical_authority.host().into()))
        }
        _ => return Err(input("HTTP TLS identity does not match its authority")),
    };
    if uri.scheme_str() != Some(scheme)
        || uri.authority().map(Authority::as_str) != Some(decision.host_header.as_ref())
        || decision.host_header.as_ref() != decision.logical_authority.http_authority()
    {
        return Err(input("HTTP logical authority is inconsistent"));
    }
    Ok(Target {
        authority: format!("{scheme}://{}", decision.host_header).into(),
        peer: origin.address,
        tls_name,
    })
}

fn header_budget(
    headers: &HeaderMap,
    (mut count, mut bytes): (usize, usize),
    phase: Phase,
) -> Result<(usize, usize), TransportError> {
    for (name, value) in headers {
        count += 1;
        bytes = bytes
            .saturating_add(name.as_str().len())
            .saturating_add(value.as_bytes().len())
            .saturating_add(4);
        if count > MAX_HEADERS || bytes > MAX_HEADER_BYTES {
            return Err(TransportError::new(
                ErrorKind::Limit,
                phase,
                "HTTP header limit exceeded",
            ));
        }
    }
    Ok((count, bytes))
}

#[derive(Clone)]
struct ClientExecutor {
    life: Arc<ClientLife>,
}

impl<F> hyper::rt::Executor<F> for ClientExecutor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, future: F) {
        let life = Arc::clone(&self.life);
        tokio::spawn(async move {
            let _life = life;
            future.await
        });
    }
}

#[derive(Clone)]
struct PinnedConnector {
    target: Target,
    tls: Arc<rustls::ClientConfig>,
    sockets: Arc<Semaphore>,
    life: Arc<ClientLife>,
}

impl Service<Uri> for PinnedConnector {
    type Response = SocketIo;
    type Error = TransportError;
    type Future = Pin<Box<dyn Future<Output = Result<SocketIo, TransportError>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let connector = self.clone();
        Box::pin(async move {
            let authority = format!(
                "{}://{}",
                uri.scheme_str().unwrap_or_default(),
                uri.authority().map(Authority::as_str).unwrap_or_default()
            );
            if authority != connector.target.authority.as_ref() {
                return Err(input("HTTP connector authority mismatch"));
            }
            let global = acquire(&connector.sockets, "HTTP socket capacity exhausted")?;
            let scoped = acquire(
                &connector.life.quota.sockets,
                "HTTP connection socket capacity exhausted",
            )?;
            // Hyper may retain a connecting future after its caller is cancelled.
            // It keeps both leases and a bounded lifetime of its own.
            let connected = timeout(REQUEST_TIMEOUT, async {
                let tcp = TcpStream::connect(connector.target.peer)
                    .await
                    .map_err(|error| {
                        TransportError::new(
                            ErrorKind::Transport,
                            Phase::AwaitingHead,
                            "HTTP connect failed",
                        )
                        .with_source(error)
                    })?;
                let peer = tcp.peer_addr().map_err(|error| {
                    TransportError::new(
                        ErrorKind::Transport,
                        Phase::AwaitingHead,
                        "HTTP peer unavailable",
                    )
                    .with_source(error)
                })?;
                if peer != connector.target.peer {
                    return Err(input("HTTP connected peer differs from the approved peer"));
                }
                let (stream, http2) = if let Some(name) = &connector.target.tls_name {
                    let name = ServerName::try_from(name.to_string())
                        .map_err(|error| input("Invalid HTTP TLS name").with_source(error))?;
                    let tls = TlsConnector::from(Arc::clone(&connector.tls))
                        .connect(name, tcp)
                        .await
                        .map_err(|error| {
                            TransportError::new(
                                ErrorKind::Transport,
                                Phase::AwaitingHead,
                                "HTTP TLS failed",
                            )
                            .with_source(error)
                        })?;
                    let http2 = tls.get_ref().1.alpn_protocol() == Some(b"h2".as_slice());
                    (Stream::Tls(Box::new(tls)), http2)
                } else {
                    (Stream::Plain(tcp), false)
                };
                Ok(SocketIo {
                    io: TokioIo::new(stream),
                    http2,
                    peer,
                    _global: global,
                    _scoped: scoped,
                    _life: connector.life,
                })
            })
            .await;
            connected.map_err(|error| {
                TransportError::new(
                    ErrorKind::Timeout,
                    Phase::AwaitingHead,
                    "HTTP connect deadline exceeded",
                )
                .with_source(error)
            })?
        })
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

struct SocketIo {
    // Fields drop in this order: actual socket, then permits, then client lease.
    io: TokioIo<Stream>,
    http2: bool,
    peer: SocketAddr,
    _global: OwnedSemaphorePermit,
    _scoped: OwnedSemaphorePermit,
    _life: Arc<ClientLife>,
}

impl hyper::rt::Read for SocketIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for SocketIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

impl Connection for SocketIo {
    fn connected(&self) -> Connected {
        let connected = Connected::new().extra(self.peer);
        if self.http2 {
            connected.negotiated_h2()
        } else {
            connected
        }
    }
}

#[cfg(test)]
mod tests;
