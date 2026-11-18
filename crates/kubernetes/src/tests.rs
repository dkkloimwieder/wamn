//! Credential operations through the shared HTTP interface.

use crate::{
    CredentialSecret, Store as _,
    http_transport::{self, Protocols, Schemes},
    kubernetes::KubernetesStore,
    kubernetes_api::KubernetesApi,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::{Response, service::service_fn};
use hyper_util::{client::legacy::connect::HttpConnector, rt::TokioIo};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{Arc, Mutex},
};
use tokio::net::TcpListener;

struct Api {
    store: KubernetesStore,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Api {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn api(replies: Vec<(u16, Value)>) -> Api {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let received = Arc::clone(&requests);
    let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
    let task = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let replies = Arc::clone(&replies);
            let received = Arc::clone(&received);
            let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                let replies = Arc::clone(&replies);
                let received = Arc::clone(&received);
                async move {
                    let body = request.into_body().collect().await.unwrap().to_bytes();
                    received.lock().unwrap().push(if body.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_slice(&body).unwrap()
                    });
                    let (status, body) = replies
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected API request");
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(status)
                            .body(Full::new(Bytes::from(body.to_string())))
                            .unwrap(),
                    )
                }
            });
            tokio::spawn(async move {
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(socket), service)
                    .await
                    .unwrap();
            });
        }
    });
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(rustls::RootCertStore::empty())
    .with_no_client_auth();
    let connector = http_transport::tls_connector(
        tls,
        HttpConnector::new(),
        Schemes::HttpAndHttps,
        Protocols::Http1,
    );
    let api = KubernetesApi::with_client(
        http_transport::client(connector, |_| {}),
        format!("http://{address}"),
        String::new(),
        "hosts".into(),
    );
    Api {
        store: KubernetesStore::new(api),
        requests,
        task,
    }
}

#[tokio::test]
async fn credentials_use_a_key_scoped_patch() {
    let fixture = api(vec![(200, json!({}))]).await;
    let target = CredentialSecret {
        name: "wamn-credential-acme--dev--app".into(),
        org: "acme",
        env: "dev",
        project: "app",
    };
    let id = uuid::Uuid::new_v4();
    let stored = fixture.store.put(target, id, &[3]).await.unwrap();
    assert_eq!(uuid::Uuid::parse_str(&stored.version).unwrap(), id);
    let requests = fixture.requests.lock().unwrap();
    let write = requests.last().unwrap();
    assert!(write["metadata"].get("resourceVersion").is_none());
    assert!(
        write["metadata"]["labels"]
            .get("app.kubernetes.io/managed-by")
            .is_none()
    );
    assert_eq!(write["data"].as_object().unwrap().len(), 1);
    assert_eq!(
        STANDARD
            .decode(write["data"][id.to_string()].as_str().unwrap())
            .unwrap(),
        [3]
    );
}

#[tokio::test]
async fn failed_or_redirected_writes_never_return_a_reference() {
    for status in [302, 403, 404, 409, 500] {
        let fixture = api(vec![(status, json!({}))]).await;
        assert!(
            fixture
                .store
                .put(
                    CredentialSecret {
                        name: "wamn-credential-acme--dev--app".into(),
                        org: "acme",
                        env: "dev",
                        project: "app",
                    },
                    uuid::Uuid::new_v4(),
                    &[1]
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn credential_reads_and_deletes_refuse_invalid_references() {
    let fixture = api(Vec::new()).await;
    for reference in ["../foreign", "wamn-credential-/foreign", "foreign"] {
        assert!(fixture.store.get(reference, "entry").await.is_err());
        assert!(fixture.store.destroy(reference, "entry").await.is_err());
    }
    assert!(fixture.requests.lock().unwrap().is_empty());
}
