//! Socket-level admission, pinning, and protocol tests.

use std::sync::atomic::{AtomicUsize, Ordering};

use hyper::{StatusCode, Version, service::service_fn};
use hyper_util::rt::TokioExecutor;
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::pki_types::PrivatePkcs8KeyDer;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::TlsAcceptor;

use super::*;
use crate::connection_authority::{PinnedEndpoint, TlsPolicy, parse_http_connection_authority};

#[derive(Clone, Copy)]
enum Reply {
    Ok,
    HeldBody,
    Disconnect,
    Redirect,
    Unavailable,
    LargeBody,
    LargeHeaders,
    ManyHeaders,
    ManyTrailers,
}

struct Observed {
    host: String,
    method: String,
    version: Version,
    authorization: Option<String>,
}

struct Server {
    peer: SocketAddr,
    accepts: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    observed: Arc<Mutex<Vec<Observed>>>,
    gate: Arc<Semaphore>,
    task: JoinHandle<()>,
    tls: Option<rustls::ClientConfig>,
}

impl Server {
    async fn start(reply: Reply, tls_http2: Option<bool>) -> Self {
        let (acceptor, tls) = match tls_http2 {
            Some(http2) => {
                let (server, client) = tls_configs(http2);
                (Some(TlsAcceptor::from(Arc::new(server))), Some(client))
            }
            None => (None, None),
        };
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener");
        let peer = listener.local_addr().expect("test address");
        let accepts = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Semaphore::new(0));
        let counted_accepts = Arc::clone(&accepts);
        let counted_requests = Arc::clone(&requests);
        let recorded = Arc::clone(&observed);
        let held = Arc::clone(&gate);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (tcp, _) = accepted.expect("test accept");
                        counted_accepts.fetch_add(1, Ordering::SeqCst);
                        let requests = Arc::clone(&counted_requests);
                        let observed = Arc::clone(&recorded);
                        let gate = Arc::clone(&held);
                        let acceptor = acceptor.clone();
                        connections.spawn(async move {
                            if let Some(acceptor) = acceptor {
                                if let Ok(tls) = acceptor.accept(tcp).await {
                                    assert_eq!(tls.get_ref().1.server_name(), Some("logical.invalid"));
                                    if tls_http2 == Some(true) {
                                        serve_h2(tls, requests, observed, gate, reply).await;
                                    } else {
                                        serve_h1(tls, requests, observed, gate, reply).await;
                                    }
                                }
                            } else {
                                serve_h1(tcp, requests, observed, gate, reply).await;
                            }
                        });
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        completed.expect("test connection present").expect("test connection task");
                    }
                }
            }
        });
        Self {
            peer,
            accepts,
            requests,
            observed,
            gate,
            task,
            tls,
        }
    }

    fn transport(&self) -> HttpTransport {
        self.tls
            .clone()
            .map(HttpTransport::from_tls)
            .unwrap_or_else(|| HttpTransport::new().expect("platform TLS"))
    }

    fn decision(&self) -> AuthorityDecision {
        decision(self.peer, self.tls.is_some())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn tls_configs(http2: bool) -> (rustls::ServerConfig, rustls::ClientConfig) {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca_key = KeyPair::generate().expect("CA key");
    let ca = ca_params.self_signed(&ca_key).expect("CA certificate");
    let issuer = Issuer::new(ca_params, ca_key);
    let leaf_key = KeyPair::generate().expect("TLS key");
    let leaf = CertificateParams::new(vec!["logical.invalid".into()])
        .expect("TLS parameters")
        .signed_by(&leaf_key, &issuer)
        .expect("TLS certificate");
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut server = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into(),
        )
        .expect("server TLS config");
    server.alpn_protocols = vec![if http2 {
        b"h2".to_vec()
    } else {
        b"http/1.1".to_vec()
    }];
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.der().clone()).expect("trust test CA");
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    (server, client)
}

async fn serve_h1<I: AsyncRead + AsyncWrite + Unpin>(
    io: I,
    requests: Arc<AtomicUsize>,
    observed: Arc<Mutex<Vec<Observed>>>,
    gate: Arc<Semaphore>,
    reply: Reply,
) {
    let mut io = BufReader::new(io);
    loop {
        let mut line = String::new();
        if io.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let method = line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        let mut host = String::new();
        let mut authorization = None;
        let mut length = 0;
        loop {
            line.clear();
            if io.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                match name.to_ascii_lowercase().as_str() {
                    "host" => host = value.trim().to_owned(),
                    "authorization" => authorization = Some(value.trim().to_owned()),
                    "content-length" => length = value.trim().parse().expect("body length"),
                    _ => {}
                }
            }
        }
        let mut body = vec![0; length];
        if io.read_exact(&mut body).await.is_err() {
            return;
        }
        observed.lock().expect("observed lock").push(Observed {
            host,
            method,
            version: Version::HTTP_11,
            authorization,
        });
        requests.fetch_add(1, Ordering::SeqCst);
        let io = io.get_mut();
        let response = match reply {
            Reply::Ok => b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".as_slice(),
            Reply::HeldBody => b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n".as_slice(),
            Reply::Disconnect => return,
            Reply::Redirect => b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://different.invalid/\r\nContent-Length: 0\r\n\r\n".as_slice(),
            Reply::Unavailable => b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n".as_slice(),
            Reply::LargeBody => b"HTTP/1.1 200 OK\r\nContent-Length: 8388609\r\n\r\n".as_slice(),
            Reply::LargeHeaders => b"HTTP/1.1 200 OK\r\nX-Long: ".as_slice(),
            Reply::ManyHeaders => b"HTTP/1.1 200 OK\r\n".as_slice(),
            Reply::ManyTrailers => b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n".as_slice(),
        };
        if io.write_all(response).await.is_err() {
            return;
        }
        match reply {
            Reply::HeldBody => {
                if io.flush().await.is_err() {
                    return;
                }
                gate.acquire().await.expect("body gate").forget();
                if io.write_all(b"ok").await.is_err() {
                    return;
                }
            }
            Reply::LargeBody => {
                let chunk = [b'x'; 8192];
                for _ in 0..1024 {
                    if io.write_all(&chunk).await.is_err() {
                        return;
                    }
                }
                let _ = io.write_all(b"x").await;
                return;
            }
            Reply::LargeHeaders => {
                let _ = io.write_all(&vec![b'x'; MAX_HEADER_BYTES]).await;
                let _ = io.write_all(b"\r\nContent-Length: 0\r\n\r\n").await;
                return;
            }
            Reply::ManyHeaders | Reply::ManyTrailers => {
                for _ in 0..MAX_HEADERS + 1 {
                    if io.write_all(b"X-Repeated: x\r\n").await.is_err() {
                        return;
                    }
                }
                let _ = io.write_all(b"\r\n").await;
                return;
            }
            _ => {}
        }
        if io.flush().await.is_err() {
            return;
        }
    }
}

async fn serve_h2<I: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    io: I,
    requests: Arc<AtomicUsize>,
    observed: Arc<Mutex<Vec<Observed>>>,
    gate: Arc<Semaphore>,
    reply: Reply,
) {
    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
        let requests = Arc::clone(&requests);
        let observed = Arc::clone(&observed);
        let gate = Arc::clone(&gate);
        async move {
            observed.lock().expect("observed lock").push(Observed {
                host: request
                    .uri()
                    .authority()
                    .expect("H2 authority")
                    .as_str()
                    .into(),
                method: request.method().to_string(),
                version: request.version(),
                authorization: request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .map(|value| value.to_str().expect("test auth").to_owned()),
            });
            request.into_body().collect().await.expect("request body");
            requests.fetch_add(1, Ordering::SeqCst);
            if matches!(reply, Reply::Disconnect) {
                return Err(std::io::Error::other(
                    "discard response after accepting mutation",
                ));
            }
            if matches!(reply, Reply::HeldBody) {
                gate.acquire().await.expect("H2 head gate").forget();
            }
            let body = if matches!(reply, Reply::LargeBody) {
                Bytes::from(vec![b'x'; MAX_BODY_BYTES + 1])
            } else {
                Bytes::from_static(b"ok")
            };
            let mut response = Response::new(Full::new(body));
            if matches!(reply, Reply::Redirect) {
                *response.status_mut() = StatusCode::TEMPORARY_REDIRECT;
                response.headers_mut().insert(
                    header::LOCATION,
                    header::HeaderValue::from_static("http://different.invalid/"),
                );
            }
            if matches!(reply, Reply::Unavailable) {
                *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
            }
            if matches!(reply, Reply::LargeHeaders) {
                response.headers_mut().insert(
                    "x-long",
                    header::HeaderValue::from_bytes(&vec![b'x'; MAX_HEADER_BYTES])
                        .expect("large test header"),
                );
            }
            if matches!(reply, Reply::ManyHeaders) {
                for _ in 0..MAX_HEADERS + 1 {
                    response
                        .headers_mut()
                        .append("x-repeated", header::HeaderValue::from_static("x"));
                }
            }
            Ok::<_, std::io::Error>(response)
        }
    });
    let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(io), service)
        .await;
}

fn scope(generation: i64) -> ClientScope {
    ClientScope {
        connection: ConnectionScope {
            tenant: "tenant".into(),
            project: "project".into(),
            environment: "environment".into(),
            instance: "connection".into(),
        },
        package: "package".into(),
        component_digest: "component".into(),
        requirement: "requirement".into(),
        binding_hash: "binding".into(),
        definition_hash: "definition".into(),
        generation,
    }
}

fn decision(peer: SocketAddr, tls: bool) -> AuthorityDecision {
    let scheme = if tls { "https" } else { "http" };
    let url = format!("{scheme}://logical.invalid:{}/resource", peer.port());
    let parsed = parse_http_connection_authority(
        &url,
        if tls {
            TlsPolicy::VerifyAuthority
        } else {
            TlsPolicy::Disabled
        },
        None,
    )
    .expect("test authority");
    AuthorityDecision {
        logical_url: url.into(),
        logical_authority: parsed.authority().clone(),
        host_header: parsed.authority().http_authority().into(),
        transport: TransportDecision::Direct {
            origin: PinnedEndpoint {
                address: peer,
                tls_identity: tls.then(|| TlsIdentity::Dns("logical.invalid".into())),
            },
        },
    }
}

fn request(decision: &AuthorityDecision) -> Request<&'static [u8]> {
    Request::builder()
        .method("POST")
        .uri(decision.logical_url.as_ref())
        .body(b"mutation".as_slice())
        .expect("test request")
}

async fn wait_for(mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("test state reached");
}

#[tokio::test]
async fn warm_http_reuses_only_the_pinned_peer_and_never_caches_credentials() {
    let server = Server::start(Reply::Ok, None).await;
    let transport = server.transport();
    let decision = server.decision();
    for auth in ["Bearer first", "Bearer second"] {
        let mut request = request(&decision);
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, auth.parse().expect("test auth"));
        let response = transport
            .execute(scope(1), &decision, request)
            .await
            .expect("pinned HTTP");
        assert_eq!(response.body(), "ok");
        assert_eq!(
            response.extensions().get::<SocketAddr>(),
            Some(&server.peer)
        );
    }
    assert_eq!(server.accepts.load(Ordering::SeqCst), 1);
    let observed = server.observed.lock().expect("observed lock");
    assert_eq!(observed.len(), 2);
    assert!(
        observed
            .iter()
            .all(|request| request.host == decision.host_header.as_ref()
                && request.method == "POST"
                && request.version == Version::HTTP_11)
    );
    assert_eq!(observed[0].authorization.as_deref(), Some("Bearer first"));
    assert_eq!(observed[1].authorization.as_deref(), Some("Bearer second"));
}

#[tokio::test]
async fn tls_http1_and_http2_keep_logical_identity_and_reuse_connections() {
    for http2 in [false, true] {
        let server = Server::start(Reply::Ok, Some(http2)).await;
        let transport = server.transport();
        let decision = server.decision();
        for _ in 0..2 {
            let response = transport
                .execute(scope(1), &decision, request(&decision))
                .await
                .expect("verified TLS");
            assert_eq!(
                response.version(),
                if http2 {
                    Version::HTTP_2
                } else {
                    Version::HTTP_11
                }
            );
            assert_eq!(
                response.extensions().get::<SocketAddr>(),
                Some(&server.peer)
            );
        }
        assert_eq!(server.accepts.load(Ordering::SeqCst), 1);
        assert!(
            server
                .observed
                .lock()
                .expect("observed lock")
                .iter()
                .all(|request| request.host == decision.host_header.as_ref())
        );
        let untrusted = HttpTransport::new().expect("platform verifier");
        assert!(
            untrusted
                .execute(scope(1), &decision, request(&decision))
                .await
                .is_err()
        );
        assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn trusted_certificate_for_a_different_logical_name_is_rejected() {
    let server = Server::start(Reply::Ok, Some(true)).await;
    let transport = server.transport();
    let url = format!("https://different.invalid:{}/resource", server.peer.port());
    let parsed = parse_http_connection_authority(&url, TlsPolicy::VerifyAuthority, None)
        .expect("test authority");
    let decision = AuthorityDecision {
        logical_url: url.into(),
        logical_authority: parsed.authority().clone(),
        host_header: parsed.authority().http_authority().into(),
        transport: TransportDecision::Direct {
            origin: PinnedEndpoint {
                address: server.peer,
                tls_identity: Some(TlsIdentity::Dns("different.invalid".into())),
            },
        },
    };
    assert!(
        transport
            .execute(scope(1), &decision, request(&decision))
            .await
            .is_err()
    );
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn http2_multiplexes_eight_requests_on_one_socket_and_still_refuses_the_ninth() {
    let server = Server::start(Reply::HeldBody, Some(true)).await;
    let transport = server.transport();
    let decision = server.decision();
    server.gate.add_permits(1);
    transport
        .execute(scope(1), &decision, request(&decision))
        .await
        .expect("warm H2");
    let mut requests = JoinSet::new();
    for _ in 0..MAX_SCOPE_REQUESTS {
        let transport = transport.clone();
        let decision = decision.clone();
        requests.spawn(async move {
            transport
                .execute(scope(1), &decision, request(&decision))
                .await
        });
    }
    wait_for(|| server.requests.load(Ordering::SeqCst) == MAX_SCOPE_REQUESTS + 1).await;
    assert_eq!(server.accepts.load(Ordering::SeqCst), 1);
    assert_eq!(transport.inner.sockets.available_permits(), MAX_SOCKETS - 1);
    assert!(
        transport
            .execute(scope(1), &decision, request(&decision))
            .await
            .expect_err("scoped H2 requests full")
            .is_before_dispatch()
    );
    server.gate.add_permits(MAX_SCOPE_REQUESTS);
    while let Some(result) = requests.join_next().await {
        result.expect("request task").expect("H2 response");
    }
    assert_eq!(transport.inner.requests.available_permits(), MAX_REQUESTS);
}

#[tokio::test]
async fn deadline_while_waiting_for_a_response_head_refunds_request_admission() {
    let server = Server::start(Reply::HeldBody, Some(true)).await;
    let transport = server.transport();
    let decision = server.decision();
    let task_transport = transport.clone();
    let task_decision = decision.clone();
    let pending = tokio::spawn(async move {
        task_transport
            .execute(scope(1), &task_decision, request(&task_decision))
            .await
    });
    wait_for(|| server.requests.load(Ordering::SeqCst) == 1).await;
    tokio::time::pause();
    tokio::time::advance(REQUEST_TIMEOUT).await;
    let error = pending
        .await
        .expect("request task")
        .expect_err("response head deadline");
    tokio::time::resume();
    assert!(error.is_timeout());
    assert!(!error.is_response_lost());
    assert!(!error.is_before_dispatch());
    assert_eq!(transport.inner.requests.available_permits(), MAX_REQUESTS);
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn every_client_identity_field_and_peer_separates_pools() {
    let server = Server::start(Reply::Ok, None).await;
    let second_server = Server::start(Reply::Ok, None).await;
    let transport = server.transport();
    let mut decision = server.decision();
    let initial = scope(1);
    let mut variants = vec![initial.clone()];
    let mut changed = initial.clone();
    changed.generation = 2;
    variants.push(changed);
    let mut changed = initial.clone();
    changed.package = "other".into();
    variants.push(changed);
    let mut changed = initial.clone();
    changed.component_digest = "other".into();
    variants.push(changed);
    let mut changed = initial.clone();
    changed.requirement = "other".into();
    variants.push(changed);
    let mut changed = initial.clone();
    changed.binding_hash = "other".into();
    variants.push(changed);
    let mut changed = initial.clone();
    changed.definition_hash = "other".into();
    variants.push(changed);
    let mut changed = initial.clone();
    changed.connection.instance = "other".into();
    variants.push(changed);
    let mut changed = initial.clone();
    changed.connection.tenant = "other".into();
    variants.push(changed);
    let mut changed = initial.clone();
    changed.connection.project = "other".into();
    variants.push(changed);
    let mut changed = initial.clone();
    changed.connection.environment = "other".into();
    variants.push(changed);
    for variant in &variants {
        transport
            .execute(variant.clone(), &decision, request(&decision))
            .await
            .expect("isolated client");
    }
    assert_eq!(server.accepts.load(Ordering::SeqCst), variants.len());
    let TransportDecision::Direct { origin } = &mut decision.transport else {
        panic!("test direct transport");
    };
    origin.address = second_server.peer;
    transport
        .execute(initial, &decision, request(&decision))
        .await
        .expect("new pinned peer");
    assert_eq!(second_server.accepts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mutation_disconnect_status_and_redirect_never_replay() {
    for protocol in [None, Some(false), Some(true)] {
        for reply in [Reply::Disconnect, Reply::Redirect, Reply::Unavailable] {
            let server = Server::start(reply, protocol).await;
            let transport = server.transport();
            let decision = server.decision();
            let result = transport
                .execute(scope(1), &decision, request(&decision))
                .await;
            match reply {
                Reply::Disconnect => {
                    let error = result.expect_err("lost response");
                    assert!(!error.is_before_dispatch());
                    assert!(!error.is_timeout());
                    assert!(!error.is_response_lost());
                }
                Reply::Redirect => assert_eq!(
                    result.expect("redirect returned").status(),
                    StatusCode::TEMPORARY_REDIRECT
                ),
                Reply::Unavailable => assert_eq!(
                    result.expect("status returned").status(),
                    StatusCode::SERVICE_UNAVAILABLE
                ),
                _ => unreachable!(),
            }
            assert_eq!(server.accepts.load(Ordering::SeqCst), 1);
            assert_eq!(server.requests.load(Ordering::SeqCst), 1);
            let observed = server.observed.lock().expect("observed lock");
            assert_eq!(observed.len(), 1);
            assert_eq!(observed[0].method, "POST");
            assert_eq!(
                observed[0].version,
                if protocol == Some(true) {
                    Version::HTTP_2
                } else {
                    Version::HTTP_11
                }
            );
        }
    }
}

#[tokio::test]
async fn oversized_requests_refuse_before_connecting() {
    let server = Server::start(Reply::Ok, None).await;
    let transport = server.transport();
    let decision = server.decision();
    let bytes = vec![0; MAX_BODY_BYTES + 1];
    let oversized = Request::builder()
        .uri(decision.logical_url.as_ref())
        .body(bytes.as_slice())
        .expect("test request");
    assert!(
        transport
            .execute(scope(1), &decision, oversized)
            .await
            .expect_err("body limit")
            .is_before_dispatch()
    );
    for many in [false, true] {
        let mut request = request(&decision);
        if many {
            for _ in 0..MAX_HEADERS {
                request
                    .headers_mut()
                    .append("x-repeated", header::HeaderValue::from_static("x"));
            }
        } else {
            request.headers_mut().insert(
                "x-long",
                header::HeaderValue::from_bytes(&vec![b'x'; MAX_HEADER_BYTES])
                    .expect("large header"),
            );
        }
        assert!(
            transport
                .execute(scope(1), &decision, request)
                .await
                .expect_err("header limit")
                .is_before_dispatch()
        );
    }
    assert_eq!(server.accepts.load(Ordering::SeqCst), 0);
    assert_eq!(transport.inner.clients.available_permits(), MAX_CLIENTS);
}

#[tokio::test]
async fn response_body_headers_and_trailers_are_bounded_while_reading() {
    for tls in [None, Some(true)] {
        for reply in [Reply::LargeBody, Reply::LargeHeaders, Reply::ManyHeaders] {
            let server = Server::start(reply, tls).await;
            let transport = server.transport();
            let decision = server.decision();
            let error = transport
                .execute(scope(1), &decision, request(&decision))
                .await
                .expect_err("response limit");
            assert!(!error.is_before_dispatch());
            if matches!(reply, Reply::LargeBody) {
                assert!(error.is_response_lost());
            }
        }
    }
    let server = Server::start(Reply::ManyTrailers, None).await;
    let transport = server.transport();
    let decision = server.decision();
    assert!(
        transport
            .execute(scope(1), &decision, request(&decision))
            .await
            .expect_err("trailer limit")
            .is_response_lost()
    );
}

#[tokio::test]
async fn scoped_requests_are_shared_across_generations_until_bodies_finish() {
    let server = Server::start(Reply::HeldBody, None).await;
    let transport = server.transport();
    let decision = server.decision();
    let mut requests = JoinSet::new();
    for generation in 0..MAX_SCOPE_REQUESTS {
        let transport = transport.clone();
        let decision = decision.clone();
        requests.spawn(async move {
            transport
                .execute(scope(generation as i64), &decision, request(&decision))
                .await
        });
    }
    wait_for(|| server.requests.load(Ordering::SeqCst) == MAX_SCOPE_REQUESTS).await;
    let error = transport
        .execute(scope(99), &decision, request(&decision))
        .await
        .expect_err("scoped requests full");
    assert!(error.is_before_dispatch());
    assert_eq!(server.accepts.load(Ordering::SeqCst), MAX_SCOPE_REQUESTS);
    assert_eq!(
        transport.inner.requests.available_permits(),
        MAX_REQUESTS - MAX_SCOPE_REQUESTS
    );
    server.gate.add_permits(MAX_SCOPE_REQUESTS);
    while let Some(result) = requests.join_next().await {
        result.expect("task").expect("body complete");
    }
    assert_eq!(transport.inner.requests.available_permits(), MAX_REQUESTS);
}

#[tokio::test]
async fn global_requests_refuse_without_queued_work_or_client_creation() {
    let server = Server::start(Reply::HeldBody, None).await;
    let transport = server.transport();
    let decision = server.decision();
    let mut requests = JoinSet::new();
    for index in 0..MAX_REQUESTS {
        let mut scope = scope(index as i64);
        scope.connection.instance = format!("connection-{}", index / MAX_SCOPE_REQUESTS).into();
        let transport = transport.clone();
        let decision = decision.clone();
        requests.spawn(async move {
            transport
                .execute(scope, &decision, request(&decision))
                .await
        });
    }
    wait_for(|| server.requests.load(Ordering::SeqCst) == MAX_REQUESTS).await;
    let retained = transport.inner.clients.available_permits();
    assert!(
        transport
            .execute(scope(99), &decision, request(&decision))
            .await
            .expect_err("global requests full")
            .is_before_dispatch()
    );
    assert_eq!(transport.inner.clients.available_permits(), retained);
    assert_eq!(server.accepts.load(Ordering::SeqCst), MAX_REQUESTS);
    requests.abort_all();
    while requests.join_next().await.is_some() {}
    assert_eq!(transport.inner.requests.available_permits(), MAX_REQUESTS);
}

#[tokio::test]
async fn socket_limits_include_idle_connections_and_all_generations() {
    let server = Server::start(Reply::Ok, None).await;
    let transport = server.transport();
    let decision = server.decision();
    for index in 0..MAX_SOCKETS {
        let mut client_scope = scope(index as i64);
        client_scope.connection.instance =
            format!("connection-{}", index / MAX_SCOPE_SOCKETS).into();
        transport
            .execute(client_scope, &decision, request(&decision))
            .await
            .expect("socket admitted");
        if index + 1 == MAX_SCOPE_SOCKETS {
            let mut scope = scope(1000);
            scope.connection.instance = "connection-0".into();
            assert!(
                transport
                    .execute(scope, &decision, request(&decision))
                    .await
                    .expect_err("scoped sockets full")
                    .is_before_dispatch()
            );
        }
    }
    assert_eq!(transport.inner.sockets.available_permits(), 0);
    assert!(
        transport
            .execute(scope(2000), &decision, request(&decision))
            .await
            .expect_err("global sockets full")
            .is_before_dispatch()
    );
    assert_eq!(server.accepts.load(Ordering::SeqCst), MAX_SOCKETS);
}

#[tokio::test]
async fn idle_pool_retains_two_sockets_then_expires_them() {
    let server = Server::start(Reply::HeldBody, None).await;
    let transport = server.transport();
    let decision = server.decision();
    let mut requests = JoinSet::new();
    for _ in 0..3 {
        let transport = transport.clone();
        let decision = decision.clone();
        requests.spawn(async move {
            transport
                .execute(scope(1), &decision, request(&decision))
                .await
        });
    }
    wait_for(|| server.requests.load(Ordering::SeqCst) == 3).await;
    server.gate.add_permits(3);
    while let Some(result) = requests.join_next().await {
        result.expect("task").expect("body complete");
    }
    wait_for(|| transport.inner.sockets.available_permits() == MAX_SOCKETS - MAX_IDLE_SOCKETS)
        .await;
    tokio::time::pause();
    tokio::time::advance(IDLE_TIMEOUT + Duration::from_secs(1)).await;
    wait_for(|| transport.inner.sockets.available_permits() == MAX_SOCKETS).await;
    tokio::time::resume();
}

#[tokio::test]
async fn retired_client_leases_outlive_cache_entries_and_body_cancellation() {
    let server = Server::start(Reply::HeldBody, Some(true)).await;
    let transport = server.transport();
    let decision = server.decision();
    let task_transport = transport.clone();
    let task_decision = decision.clone();
    let request_task = tokio::spawn(async move {
        task_transport
            .execute(scope(0), &task_decision, request(&task_decision))
            .await
    });
    wait_for(|| server.requests.load(Ordering::SeqCst) == 1).await;
    let target = target(&decision, &decision.logical_url.parse().expect("URI")).expect("target");
    for generation in 1..MAX_CLIENTS {
        transport
            .client(scope(generation as i64), target.clone())
            .expect("retain client");
    }
    assert_eq!(transport.inner.clients.available_permits(), 0);
    assert!(transport.client(scope(200), target.clone()).is_err());
    assert_eq!(
        transport.inner.state.lock().expect("state").clients.len(),
        MAX_CLIENTS - 1
    );
    assert_eq!(transport.inner.sockets.available_permits(), MAX_SOCKETS - 1);
    // The oldest entry was evicted, but its active HTTP/2 task still owns a lease.
    request_task.abort();
    let _ = request_task.await;
    wait_for(|| transport.inner.sockets.available_permits() == MAX_SOCKETS).await;
    wait_for(|| transport.inner.clients.available_permits() == 1).await;
    transport
        .client(scope(200), target)
        .expect("admit after actual drain");
    let state = transport.inner.state.lock().expect("state");
    assert!(state.clients.len() <= MAX_CLIENTS);
    assert!(state.scopes.len() <= MAX_CLIENTS);
}

#[test]
fn error_display_and_debug_do_not_expose_sources() {
    let error = input("HTTP request refused")
        .with_source(std::io::Error::other("Bearer secret; private.internal"));
    assert_eq!(error.to_string(), "HTTP request refused");
    assert!(!format!("{error:?}").contains("secret"));
    assert!(error.source().is_some());
    assert!(error.is_before_dispatch());
    let timeout = TransportError::new(ErrorKind::Timeout, Phase::AwaitingHead, "deadline");
    assert!(timeout.is_timeout());
    let body_timeout = TransportError::new(ErrorKind::Timeout, Phase::ResponseBody, "deadline");
    assert!(body_timeout.is_response_lost());
    assert!(!body_timeout.is_timeout());
}
