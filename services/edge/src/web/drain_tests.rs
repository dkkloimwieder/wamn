//! Exercise connection retirement through the production edge and host ingress.

use std::sync::Mutex;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinSet;
use wamn_engine::expected_router::{ExpectedHostRouter, expected_host_router};
use wash_runtime::host::http::{HostHandler as _, Ingress, IngressRoute, ServiceHttpJob};

use super::*;

const ROUTE_HOST: &str = "api.example.test";

type Calls = Arc<Mutex<Vec<(Method, String)>>>;

async fn backend(
    id: &'static str,
    tasks: &mut JoinSet<()>,
    admitted: Arc<Notify>,
    release: Arc<Notify>,
) -> (Ingress<ExpectedHostRouter>, watch::Sender<bool>, Calls) {
    let (stop, stopping) = watch::channel(false);
    let ingress = Ingress::new(
        expected_host_router(Some(ROUTE_HOST), stopping),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let (sender, mut jobs) = mpsc::channel::<ServiceHttpJob>(4);
    ingress
        .on_service_http_resolved(id, &[IngressRoute::ingress(ROUTE_HOST)], sender)
        .await
        .unwrap();
    ingress.start().await.unwrap();
    tasks.spawn(async move {
        let mut responses = JoinSet::new();
        while let Some(job) = jobs.recv().await {
            recorded
                .lock()
                .unwrap()
                .push((job.req.method().clone(), job.req.uri().path().into()));
            let admitted = Arc::clone(&admitted);
            let release = Arc::clone(&release);
            responses.spawn(async move {
                if job.req.uri().path() == "/held" {
                    admitted.notify_one();
                    release.notified().await;
                }
                job.resp_tx
                    .send(Ok(Response::builder()
                        .header("x-backend", id)
                        .body(UnsyncBoxBody::default())
                        .unwrap()))
                    .unwrap();
            });
        }
        while let Some(result) = responses.join_next().await {
            result.unwrap();
        }
    });
    (ingress, stop, calls)
}

async fn edge_request(
    client: &Client<HttpConnector, Full<Bytes>>,
    edge: SocketAddr,
    method: Method,
    path: &str,
) -> (StatusCode, String) {
    let response = client
        .request(
            hyper::Request::builder()
                .method(method)
                .uri(format!("http://{edge}/api{path}"))
                .header(HOST, ROUTE_HOST)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let backend = response
        .headers()
        .get("x-backend")
        .map_or("none", |value| value.to_str().unwrap())
        .to_owned();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
    (status, backend)
}

#[tokio::test]
async fn pooled_edge_retires_draining_host_without_replaying_posts() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut tasks = JoinSet::new();
        let admitted = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (a, stop_a, calls_a) = backend("A", &mut tasks, Arc::clone(&admitted), Arc::clone(&release)).await;
        let (b, _stop_b, calls_b) = backend("B", &mut tasks, Arc::clone(&admitted), Arc::clone(&release)).await;

        // A TCP connection keeps its selected backend when the target changes.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let service_addr = listener.local_addr().unwrap();
        let target = Arc::new(Mutex::new(("A", a.addr())));
        let current = Arc::clone(&target);
        let connections = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&connections);
        let wire_requests = Arc::new(Mutex::new(BTreeMap::<usize, Vec<u8>>::new()));
        let recorded_requests = Arc::clone(&wire_requests);
        let (closed, mut closures) = mpsc::unbounded_channel();
        tasks.spawn(async move {
            let mut forwarding = JoinSet::new();
            loop {
                tokio::select! {
                    Some(result) = forwarding.join_next(), if !forwarding.is_empty() => { result.unwrap(); }
                    accepted = listener.accept() => {
                        let (mut client, _) = accepted.unwrap();
                        let (backend, addr) = *current.lock().unwrap();
                        let id = {
                            let mut connections = recorded.lock().unwrap();
                            let id = connections.len() + 1;
                            connections.push((id, backend));
                            id
                        };
                        let closed = closed.clone();
                        let recorded_requests = Arc::clone(&recorded_requests);
                        forwarding.spawn(async move {
                            let mut upstream = TcpStream::connect(addr).await.unwrap();
                            let (mut input, mut output) = client.split();
                            let (mut read, mut write) = upstream.split();
                            let upload = async {
                                let mut buffer = [0; 4096];
                                loop {
                                    let count = input.read(&mut buffer).await.unwrap();
                                    if count == 0 { break; }
                                    recorded_requests.lock().unwrap().entry(id).or_default()
                                        .extend_from_slice(&buffer[..count]);
                                    write.write_all(&buffer[..count]).await.unwrap();
                                }
                                write.shutdown().await.unwrap();
                            };
                            let download = async {
                                let mut response = Vec::new();
                                let mut buffer = [0; 4096];
                                loop {
                                    let count = read.read(&mut buffer).await.unwrap();
                                    if count == 0 { break; }
                                    response.extend_from_slice(&buffer[..count]);
                                    output.write_all(&buffer[..count]).await.unwrap();
                                }
                                output.shutdown().await.unwrap();
                                closed.send((id, backend, response)).unwrap();
                            };
                            tokio::join!(upload, download);
                        });
                    }
                }
            }
        });

        // Only the ConfigMap source is synthetic. Requests use production respond.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let route = serde_json::json!({"host":ROUTE_HOST,"release":"unused", "api":format!("http://{service_addr}"),"files":""});
        let objects = serde_json::json!({"items":[{"metadata":{"name":"route"},"data":{"route.json":route.to_string()}}]}).to_string();
        tasks.spawn(async move {
            let mut requests = JoinSet::new();
            loop {
                tokio::select! {
                    Some(result) = requests.join_next(), if !requests.is_empty() => { result.unwrap(); }
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let objects = objects.clone();
                        requests.spawn(async move {
                            let service = service_fn(move |_| {
                                let response = Response::new(Full::new(Bytes::from(objects.clone())));
                                async { Ok::<_, Infallible>(response) }
                            });
                            hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), service).await.unwrap();
                        });
                    }
                }
            }
        });
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions().unwrap()
                .with_root_certificates(rustls::RootCertStore::empty()).with_no_client_auth())
            .https_or_http().enable_http1().build();
        let releases = Releases {
            client: Client::builder(TokioExecutor::new()).build(connector),
            endpoint,
            token: "/dev/null".into(),
        };
        let web = Web { store: None, files: RwLock::new(BTreeMap::new()) };
        let proxy = proxy_client();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let edge = listener.local_addr().unwrap();
        tasks.spawn(async move {
            let mut requests = JoinSet::new();
            let state = Arc::new((web, releases, proxy));
            loop {
                tokio::select! {
                    Some(result) = requests.join_next(), if !requests.is_empty() => { result.unwrap(); }
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let state = Arc::clone(&state);
                        requests.spawn(async move {
                            let service = service_fn(|request| async {
                                Ok::<_, Infallible>(respond(&state.0, &state.1, &state.2, request).await.unwrap())
                            });
                            hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), service).await.unwrap();
                        });
                    }
                }
            }
        });
        let client: Client<HttpConnector, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();
        let held_client = client.clone();
        let held = tasks.spawn(async move {
            assert_eq!(edge_request(&held_client, edge, Method::POST, "/held").await, (StatusCode::OK, "A".into()));
        });
        admitted.notified().await;
        assert_eq!(edge_request(&client, edge, Method::GET, "/first").await, (StatusCode::OK, "A".into()));
        assert_eq!(*connections.lock().unwrap(), [(1, "A"), (2, "A")]);

        // Match probes.drain()/stopping followed by the host's handler.stop().
        let probes = wash_runtime::host::probes::ProbeState::default();
        probes.drain();
        stop_a.send(true).unwrap();
        a.stop().await.unwrap();
        a.stopped().await;
        *target.lock().unwrap() = ("B", b.addr());
        let after = edge_request(&client, edge, Method::POST, "/mutate").await;
        release.notify_one();
        while tasks.join_next_with_id().await.unwrap().unwrap().0 != held.id() {}
        let mutation_connections: Vec<_> = wire_requests.lock().unwrap().iter()
            .filter(|(_, bytes)| bytes.windows(b"POST /mutate ".len()).any(|part| part == b"POST /mutate "))
            .map(|(id, _)| *id).collect();
        eprintln!("request 2: status={}, backend={}, request_connection={mutation_connections:?}, connections={:?}, dispatches A={:?}, B={:?}",
            after.0, after.1, *connections.lock().unwrap(), *calls_a.lock().unwrap(), *calls_b.lock().unwrap());
        assert_eq!(after, (StatusCode::OK, "B".into()), "request 2 must use a new connection to B; connections={:?}", *connections.lock().unwrap());
        assert_eq!(*connections.lock().unwrap(), [(1, "A"), (2, "A"), (3, "B")]);
        assert_eq!(*calls_a.lock().unwrap(), [(Method::POST, "/held".into()), (Method::GET, "/first".into())], "A dispatches nothing after the cut, and the held POST runs once");
        assert_eq!(*calls_b.lock().unwrap(), [(Method::POST, "/mutate".into())], "the mutation runs exactly once on B");
        a.connection_limit().drained().await;
        for _ in 0..2 {
            let (id, backend, responses) = closures.recv().await.unwrap();
            assert_eq!(backend, "A");
            if id == 1 {
                assert!(String::from_utf8(responses).unwrap().to_ascii_lowercase().contains("connection: close\r\n"), "the in-flight response retires its connection");
            }
        }
        b.stop().await.unwrap();
    }).await.expect("routed requests and in-flight drain finish within the existing bound");
}
