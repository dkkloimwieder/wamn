//! Recording loopback servers for the disposable native transport experiment.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{Notify, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

type Body = http_body_util::combinators::UnsyncBoxBody<Bytes, std::io::Error>;

#[derive(Default)]
pub(crate) struct Observed {
    pub accepted: AtomicUsize,
    pub active: AtomicUsize,
    pub requests: Mutex<Vec<Value>>,
    changed: Notify,
}

pub(crate) struct Server {
    pub address: SocketAddr,
    pub observed: Arc<Observed>,
    pub release: Arc<Semaphore>,
    pub client_tls: Arc<rustls::ClientConfig>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        // Dropping the accept task drops its JoinSet and aborts its peers.
        self.task.abort();
    }
}

struct Active(Arc<Observed>);

impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_one();
    }
}

pub(crate) async fn start(tls: bool, h2: bool) -> Result<Server> {
    let certificate =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])?;
    let der = certificate.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::try_from(certificate.signing_key.serialize_der())
        .map_err(anyhow::Error::msg)?;
    let mut server_tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key)?;
    server_tls.alpn_protocols = vec![if h2 {
        b"h2".to_vec()
    } else {
        b"http/1.1".to_vec()
    }];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_tls));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der)?;
    let client_tls = Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let observed = Arc::new(Observed::default());
    let state = Arc::clone(&observed);
    let release = Arc::new(Semaphore::new(0));
    let held = Arc::clone(&release);
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, peer)) = accepted else { break };
                    let id = state.accepted.fetch_add(1, Ordering::SeqCst) + 1;
                    state.active.fetch_add(1, Ordering::SeqCst);
                    let active = Active(Arc::clone(&state));
                    let state = Arc::clone(&state);
                    let held = Arc::clone(&held);
                    let acceptor = acceptor.clone();
                    connections.spawn(async move {
                        let _active = active;
                        if tls {
                            if let Ok(stream) = acceptor.accept(stream).await {
                                serve(stream, h2, id, peer, state, held).await;
                            }
                        } else { serve(stream, h2, id, peer, state, held).await; }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    Ok(Server {
        address,
        observed,
        release,
        client_tls,
        task,
    })
}

async fn serve<S>(
    stream: S,
    h2: bool,
    id: usize,
    peer: SocketAddr,
    state: Arc<Observed>,
    held: Arc<Semaphore>,
) where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let service = service_fn(move |request| {
        respond(request, id, peer, Arc::clone(&state), Arc::clone(&held))
    });
    if h2 {
        let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
            .serve_connection(TokioIo::new(stream), service)
            .await;
    } else {
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await;
    }
}

fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

async fn respond(
    request: Request<Incoming>,
    id: usize,
    peer: SocketAddr,
    state: Arc<Observed>,
    held: Arc<Semaphore>,
) -> Result<Response<Body>, std::io::Error> {
    let path = request.uri().path().to_owned();
    let credential = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let header_bytes: usize = request
        .headers()
        .iter()
        .map(|(key, value)| key.as_str().len() + value.as_bytes().len())
        .sum();
    let version = format!("{:?}", request.version());
    let body = request
        .into_body()
        .collect()
        .await
        .map_err(std::io::Error::other)?
        .to_bytes();
    state.requests.lock().expect("fixture request lock").push(json!({
        "connection":id,"client_peer":peer.to_string(),"path":path,"credential_marker":credential,
        "version":version,"request_bytes":body.len(),"header_bytes":header_bytes
    }));
    state.changed.notify_one();
    if path == "/hold" {
        held.acquire()
            .await
            .map_err(std::io::Error::other)?
            .forget();
    }
    if path == "/slow-head" {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let body = match path.as_str() {
        "/large-body" => full(vec![b'x'; 128 * 1024]),
        "/slow-body" => StreamBody::new(futures_util::stream::unfold(0, |index| async move {
            if index == 5 {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
            Some((
                Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(b"x"))),
                index + 1,
            ))
        }))
        .boxed_unsync(),
        "/truncate" => StreamBody::new(futures_util::stream::iter([
            Ok(Frame::data(Bytes::from_static(b"committed"))),
            Err(std::io::Error::other("fixture closes after the mutation")),
        ]))
        .boxed_unsync(),
        _ => full("ok"),
    };
    let mut response = Response::builder().header("x-probe-connection", id.to_string());
    if path == "/large-header" {
        response = response.header("x-probe-padding", "x".repeat(4096));
    }
    if version == "HTTP/2.0" {
        response = response.header("grpc-status", "0");
    }
    response.body(body).map_err(std::io::Error::other)
}

pub(crate) fn requests(server: &Server) -> Vec<Value> {
    server
        .observed
        .requests
        .lock()
        .expect("fixture request lock")
        .clone()
}

pub(crate) async fn wait_requests(server: &Server, count: usize) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let changed = server.observed.changed.notified();
            if server
                .observed
                .requests
                .lock()
                .expect("fixture request lock")
                .len()
                >= count
            {
                return;
            }
            changed.await;
        }
    })
    .await
    .context("fixture did not observe the expected requests")
}

pub(crate) async fn wait_closed(server: &Server) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let changed = server.observed.changed.notified();
            if server.observed.active.load(Ordering::SeqCst) == 0 {
                return;
            }
            changed.await;
        }
    })
    .await
    .context("fixture connection did not drain")
}
