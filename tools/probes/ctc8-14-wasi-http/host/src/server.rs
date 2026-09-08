//! Local recording peers; they never echo credentials back to the guest.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::Full;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

pub const HOST_CREDENTIAL: &str = "Bearer ctc8-14-host-only-sentinel";
pub const GUEST_CREDENTIAL: &str = "Bearer ctc8-14-guest-forgery";

pub struct Server {
    pub address: SocketAddr,
    pub certificate: Option<CertificateDer<'static>>,
    pub requests: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn start(certificate_name: Option<&str>) -> anyhow::Result<Server> {
    let (acceptor, certificate) = if let Some(name) = certificate_name {
        let key = rcgen::generate_simple_self_signed(vec![name.to_owned()])?;
        let certificate = key.cert.der().clone();
        let key =
            PrivateKeyDer::try_from(key.signing_key.serialize_der()).map_err(anyhow::Error::msg)?;
        let mut config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate.clone()], key)?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        (Some(TlsAcceptor::from(Arc::new(config))), Some(certificate))
    } else {
        (None, None)
    };
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let collected = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, peer)) = accepted else { break };
                    let acceptor = acceptor.clone();
                    let collected = Arc::clone(&collected);
                    connections.spawn(async move {
                        if let Some(acceptor) = acceptor {
                            if let Ok(stream) = acceptor.accept(stream).await {
                                serve(stream, peer, address, collected).await;
                            }
                        } else {
                            serve(stream, peer, address, collected).await;
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    Ok(Server {
        address,
        certificate,
        requests,
        task,
    })
}

async fn serve<T>(stream: T, peer: SocketAddr, local: SocketAddr, requests: Arc<Mutex<Vec<Value>>>)
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service =
        hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
            let requests = Arc::clone(&requests);
            async move {
                let header = |name| {
                    request
                        .headers()
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                };
                requests.lock().expect("recording peer lock").push(json!({
                "connected_local": local.to_string(),
                "connected_peer": peer.to_string(),
                "protocol": format!("{:?}", request.version()),
                "path": request.uri().path(),
                "host": header("host").or_else(|| request.uri().authority().map(|a| a.as_str())),
                "content_type": header("content-type"),
                "host_credential": header("authorization") == Some(HOST_CREDENTIAL),
                "guest_credential": header("authorization") == Some(GUEST_CREDENTIAL),
                "traceparent": header("traceparent"),
            }));
                Ok::<_, Infallible>(
                    hyper::Response::builder()
                        .status(200)
                        .header("grpc-status", "0")
                        .body(Full::new(Bytes::new()))
                        .expect("fixed response"),
                )
            }
        });
    let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), service)
        .await;
}

pub fn last(server: &Server) -> anyhow::Result<Value> {
    server
        .requests
        .lock()
        .expect("recording peer lock")
        .last()
        .cloned()
        .context("the actual peer recorded no request")
}
