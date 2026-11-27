//! Route across a host replacement through the production proxy and native ingress.

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use wamn_engine::expected_router::expected_host_router;
use wamn_kubernetes::web_release::WEB_RELEASE_ANNOTATION;
use wash_runtime::host::http::{HostHandler as _, Ingress, IngressRoute, ServiceHttpJob};

use super::*;

#[tokio::test]
async fn host_replacement_routes_the_next_post_to_the_serving_host() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let host = "dev.acme.example.test";
        let upstream = "app-http.acme--dev.svc.cluster.local";
        let mount = "/shop/stock/";
        let mut tasks = tokio::task::JoinSet::new();
        let mut backends = Vec::new();
        for _ in 0..2 {
            let (stop, stopping) = watch::channel(false);
            let ingress = Ingress::new(
                expected_host_router(Some(upstream), stopping),
                "127.0.0.1:0".parse().unwrap(),
            ).await.unwrap();
            let (sender, mut jobs) = mpsc::channel::<ServiceHttpJob>(4);
            ingress.on_service_http_resolved("http", &[IngressRoute::ingress(upstream)], sender).await.unwrap();
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

        // Only the Kubernetes route discovery source is synthetic.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let route = serde_json::json!({
            "project":"shop", "app":"stock", "digest":wamn_digest::bytes_sha256(b"release")
        });
        let objects = serde_json::json!({"metadata":{"resourceVersion":"2"},"items":[
            {"metadata":{"name":"app-http","namespace":"acme--dev","annotations":{
                (WEB_RELEASE_ANNOTATION):route.to_string()
            }}}
        ]}).to_string();
        let namespaces = serde_json::json!({"metadata":{"resourceVersion":"1"},"items":[{"metadata":{"name":"acme--dev","labels":{
            "wamn.dev/env":"true","wamn.dev/org-workloads":"true","wamn.dev/org-name":"acme"
        }}}]}).to_string();
        tasks.spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let objects = &objects;
                let namespaces = &namespaces;
                let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| async move {
                    let watching = request.uri().query().is_some_and(|query| query.contains("watch=true"));
                    let body = match (request.uri().path(), watching) {
                        ("/api/v1/namespaces", false) => namespaces,
                        ("/api/v1/services", false) => objects,
                        ("/api/v1/namespaces" | "/api/v1/services", true) =>
                            "{\"type\":\"ERROR\",\"object\":{\"code\":410,\"reason\":\"Expired\"}}\n",
                        (path, _) => panic!("unexpected Kubernetes request {path}"),
                    };
                    Ok::<_, Infallible>(Response::builder()
                        .header(hyper::header::CONNECTION, "close")
                        .body(Full::new(Bytes::copy_from_slice(body.as_bytes()))).unwrap())
                });
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service).await.unwrap();
            }
        });
        let mut config = kube::Config::new(endpoint.parse().unwrap());
        config.default_namespace = "hosts".into();
        let releases = Arc::new(Releases::new(
            KubernetesApi::from_config(config).unwrap(),
            "example.test".parse().unwrap(),
        ));
        let watching = Arc::clone(&releases);
        tasks.spawn(async move { watching.watch().await });
        wait_for_route(&releases, host, mount).await;
        let web = Web { store: None };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let edge = listener.local_addr().unwrap();
        tasks.spawn(async move {
            let mut connector = Connector::new(native_connector().unwrap());
            connector.test_address = Some(service);
            let proxy = Proxy {
                client: http_transport::client(connector, |_| {}),
            };
            let (stream, _) = listener.accept().await.unwrap();
            let service = service_fn(|request| async {
                Ok::<_, Infallible>(respond(&web, &releases, &proxy, "https://identity.example", "127.0.0.1".parse().unwrap(), 0, request).await.unwrap())
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service).await.unwrap();
        });
        let client: Client<HttpConnector, Full<Bytes>> = http_transport::http_client();
        let redirect = hyper::Request::builder()
            .uri(format!("http://{edge}{}?tab=activity", mount.trim_end_matches('/')))
            .header(HOST, host).body(Full::new(Bytes::new())).unwrap();
        let response = client.request(redirect).await.unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(response.headers()[LOCATION], format!("{mount}?tab=activity"));
        response.into_body().collect().await.unwrap();
        for (index, (ingress, _, _)) in backends.iter().enumerate() {
            if index > 0 {
                *selected.write().await = ingress.addr();
                backends[0].1.send(true).unwrap();
                backends[0].0.stop().await.unwrap();
                backends[0].0.stopped().await;
            }
            let request = hyper::Request::builder()
                .method(Method::POST).uri(format!("http://{edge}{mount}api/mutate"))
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
