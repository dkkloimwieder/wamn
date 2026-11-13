//! Password requests cross the production TLS proxy with the forwarded address.

use super::*;

#[tokio::test]
async fn password_proxy_preserves_the_client_address_and_cookie() -> anyhow::Result<()> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
    )?;
    let identity = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("https://localhost:{}", identity.local_addr()?.port());
    let proxy = proxy(cert.pem().as_bytes())?;
    let identity_task = tokio::spawn(async move {
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        for _ in 0..3 {
            let (stream, _) = identity.accept().await.unwrap();
            let stream = acceptor.accept(stream).await.unwrap();
            let service = service_fn(
                |request: hyper::Request<hyper::body::Incoming>| async move {
                    let value = serde_json::json!({
                        "client": request.headers()["x-forwarded-for"].to_str().unwrap(),
                        "host": request.headers()[HOST].to_str().unwrap(),
                        "path": request.uri().path(),
                    });
                    request.into_body().collect().await.unwrap();
                    Ok::<_, Infallible>(
                        Response::builder()
                            .header("set-cookie", "session=opaque; Secure; HttpOnly")
                            .body(Full::new(Bytes::from(serde_json::to_vec(&value).unwrap())))
                            .unwrap(),
                    )
                },
            );
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
                .unwrap();
        }
    });
    let discovery = TcpListener::bind("127.0.0.1:0").await?;
    let discovery_url = format!("http://{}", discovery.local_addr()?);
    let discovery_task = tokio::spawn(async move {
        let (stream, _) = discovery.accept().await.unwrap();
        let service = service_fn(|_| async {
            let route = serde_json::json!({"host":"app.example", "release":"app", "api":"http://unused", "files":""});
            let objects = serde_json::json!({"items":[{"metadata":{"name":"app"},"data":{"route.json":route.to_string()}}]});
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                serde_json::to_vec(&objects).unwrap(),
            ))))
        });
        hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await
            .unwrap();
    });
    let releases = Releases {
        api: wamn_kubernetes::kubernetes_api::KubernetesApi::with_client(
            Client::builder(TokioExecutor::new()).build(
                hyper_rustls::HttpsConnectorBuilder::new()
                    .with_provider_and_native_roots(rustls::crypto::aws_lc_rs::default_provider())?
                    .https_or_http()
                    .enable_http1()
                    .build(),
            ),
            discovery_url,
            String::new(),
            "hosts".into(),
        ),
    };
    let web = Web { store: None };
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/password/session", listener.local_addr()?);
    let front = tokio::spawn(async move {
        for _ in 0..3 {
            let (stream, peer) = listener.accept().await.unwrap();
            let service = service_fn(|request| async {
                let mut response =
                    respond(&web, &releases, &proxy, &endpoint, peer.ip(), 2, request)
                        .await
                        .unwrap();
                response_headers(response.headers_mut());
                Ok::<_, Infallible>(response)
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
                .unwrap();
        }
    });
    let client: Client<HttpConnector, Full<Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    for address in [Some("198.51.100.1"), Some("198.51.100.2"), None] {
        let mut request = hyper::Request::builder()
            .uri(&url)
            .method(Method::POST)
            .header(HOST, "app.example")
            .header("connection", "x-forwarded-for, close");
        if let Some(address) = address {
            request = request.header(
                "x-forwarded-for",
                format!("192.0.2.99, {address}, 203.0.113.8"),
            );
        }
        let response = client
            .request(request.body(Full::new(Bytes::from_static(b"{}")))?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("set-cookie"));
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await?.to_bytes())?;
        assert_eq!(value["client"], address.unwrap_or("127.0.0.1"));
        assert_eq!(value["path"], "/password/session");
        assert!(value["host"].as_str().unwrap().starts_with("localhost:"));
    }
    front.abort();
    discovery_task.abort();
    identity_task.abort();
    Ok(())
}
