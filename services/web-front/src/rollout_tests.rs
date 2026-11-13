//! Route across a host replacement through the production proxy and native ingress.

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use wamn_engine::expected_router::expected_host_router;
use wash_runtime::host::http::{HostHandler as _, Ingress, IngressRoute, ServiceHttpJob};

use super::*;

#[tokio::test]
async fn host_replacement_routes_the_next_post_to_the_serving_host() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let host = "app.example.test";
        let mut tasks = tokio::task::JoinSet::new();
        let mut backends = Vec::new();
        for _ in 0..2 {
            let (stop, stopping) = watch::channel(false);
            let ingress = Ingress::new(
                expected_host_router(Some(host), stopping),
                "127.0.0.1:0".parse().unwrap(),
            ).await.unwrap();
            let (sender, mut jobs) = mpsc::channel::<ServiceHttpJob>(4);
            ingress.on_service_http_resolved("http", &[IngressRoute::ingress(host)], sender).await.unwrap();
            ingress.start().await.unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let recorded = Arc::clone(&calls);
            let address = ingress.addr();
            tasks.spawn(async move {
                while let Some(job) = jobs.recv().await {
                    recorded.fetch_add(1, Ordering::SeqCst);
                    job.resp_tx.send(Ok(Response::builder()
                        .header("x-backend", address.to_string())
                        .body(UnsyncBoxBody::default()).unwrap())).unwrap();
                }
            });
            backends.push((ingress, stop, calls));
        }

        // A Service selects an endpoint per TCP connection, not per request.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let service = listener.local_addr().unwrap();
        let selected = Arc::new(RwLock::new(backends[0].0.addr()));
        let current = Arc::clone(&selected);
        tasks.spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    Some(result) = connections.join_next(), if !connections.is_empty() => { result.unwrap(); }
                    accepted = listener.accept() => {
                        let (mut client, _) = accepted.unwrap();
                        let address = *current.read().await;
                        connections.spawn(async move {
                            let mut upstream = TcpStream::connect(address).await.unwrap();
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                        });
                    }
                }
            }
        });

        // Only the Kubernetes ConfigMap source is synthetic.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let route = serde_json::json!({"host":host,"release":"unused","api":format!("http://{service}"),"files":""});
        let objects = serde_json::json!({"items":[{"metadata":{"name":"route"},"data":{"route.json":route.to_string()}}]}).to_string();
        tasks.spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let objects = &objects;
                let service = service_fn(move |_| async move {
                    Ok::<_, Infallible>(Response::builder()
                        .header(hyper::header::CONNECTION, "close")
                        .body(Full::new(Bytes::from(objects.clone()))).unwrap())
                });
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service).await.unwrap();
            }
        });
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions().unwrap()
                .with_root_certificates(rustls::RootCertStore::empty()).with_no_client_auth())
            .https_or_http().enable_http1().build();
        let releases = Releases {
            api: wamn_kubernetes::kubernetes_api::KubernetesApi::with_client(
                Client::builder(TokioExecutor::new()).build(connector), endpoint, String::new(), "hosts".into()),
        };
        let web = Web { store: None };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let edge = listener.local_addr().unwrap();
        tasks.spawn(async move {
            let proxy = Client::builder(TokioExecutor::new()).build(hyper_rustls::HttpsConnectorBuilder::new().with_provider_and_native_roots(rustls::crypto::aws_lc_rs::default_provider()).unwrap().https_or_http().enable_http1().build());
            let (stream, _) = listener.accept().await.unwrap();
            let service = service_fn(|request| async {
                Ok::<_, Infallible>(respond(&web, &releases, &proxy, "https://identity.example", "127.0.0.1".parse().unwrap(), 0, request).await.unwrap())
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service).await.unwrap();
        });
        let client: Client<HttpConnector, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();
        for (index, (ingress, _, _)) in backends.iter().enumerate() {
            if index > 0 {
                *selected.write().await = ingress.addr();
                backends[0].1.send(true).unwrap();
                backends[0].0.stop().await.unwrap();
                backends[0].0.stopped().await;
            }
            let request = hyper::Request::builder()
                .method(Method::POST).uri(format!("http://{edge}/api/mutate"))
                .header(HOST, host).body(Full::new(Bytes::new())).unwrap();
            let response = client.request(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "host replacement must serve the next request");
            assert_eq!(response.headers()["x-backend"], ingress.addr().to_string());
            response.into_body().collect().await.unwrap();
        }
        for (ingress, _, calls) in &backends {
            assert_eq!(calls.load(Ordering::SeqCst), 1, "each POST executes once");
            ingress.stop().await.unwrap();
        }
        tasks.shutdown().await;
    }).await.expect("host replacement finishes within the test bound");
}
