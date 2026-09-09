//! Local HTTPS and fixed signing fixtures shared by session proofs.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use rustls::pki_types::PrivatePkcs8KeyDer;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;
use wamn_runtime::session_keys::{IssuerKeys, IssuerKeysConfig, TestClock};
use wamn_runtime::session_verifier::{SessionTestClock, SessionVerifier};

pub(super) const ISSUER: &str = "https://identity.example.test/issuer";
pub(super) const ORG: &str = "org-a";
pub(super) const AUDIENCE: &str = "urn:wamn:project-env:org-a:project:dev:instance-one";

fn pair() -> Ed25519KeyPair {
    // Fixed test-only seed. Production signing keys never enter this crate.
    Ed25519KeyPair::from_seed_unchecked(&[7; 32]).expect("fixture signing key")
}

fn jwks() -> Value {
    json!({"keys": [{"kid": "key-one", "kty": "OKP", "crv": "Ed25519",
        "alg": "Ed25519", "use": "sig",
        "x": URL_SAFE_NO_PAD.encode(pair().public_key().as_ref())}]})
}

pub(super) fn header() -> Value {
    json!({"alg": "Ed25519", "typ": "wamn-session+jwt", "kid": "key-one"})
}

pub(super) fn claims() -> Value {
    json!({"iss": ISSUER, "sub": "ed7056a9-5639-455f-9640-4678458794c0",
        "org": ORG, "aud": AUDIENCE, "roles": ["purchase-reader"],
        "iat": 1000, "exp": 1900, "jti": "session-test"})
}

pub(super) fn signed(header: &Value, claims: &Value) -> String {
    let message = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).expect("header bytes")),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claim bytes")),
    );
    let signature = pair().sign(message.as_bytes());
    format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
}

struct Hold {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

struct Response {
    body: Value,
    hold: Option<Hold>,
}

pub(super) struct Server {
    endpoint: String,
    ca_pem: Vec<u8>,
    response: Arc<Mutex<Response>>,
    requests: Arc<AtomicUsize>,
    task: Option<JoinHandle<()>>,
}

impl Server {
    pub(super) async fn start() -> Self {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_key = KeyPair::generate().expect("CA key");
        let ca = ca_params.self_signed(&ca_key).expect("CA certificate");
        let issuer = Issuer::new(ca_params, ca_key);
        let leaf_key = KeyPair::generate().expect("TLS key");
        let leaf = CertificateParams::new(vec!["127.0.0.1".into()])
            .expect("TLS parameters")
            .signed_by(&leaf_key, &issuer)
            .expect("TLS certificate");
        let config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::aws_lc_rs::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into(),
        )
        .expect("TLS configuration");
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let endpoint = format!(
            "https://{}/.well-known/jwks.json",
            listener.local_addr().expect("fixture address")
        );
        let response = Arc::new(Mutex::new(Response {
            body: jwks(),
            hold: None,
        }));
        let requests = Arc::new(AtomicUsize::new(0));
        let serving = response.clone();
        let request_count = requests.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (tcp, _) = accepted.expect("fixture connection");
                        let acceptor = acceptor.clone();
                        let response = serving.clone();
                        let requests = request_count.clone();
                        connections.spawn(async move {
                            let Ok(mut stream) = acceptor.accept(tcp).await else { return; };
                            let mut request = Vec::new();
                            while !request.ends_with(b"\r\n\r\n") {
                                let mut byte = [0];
                                if stream.read_exact(&mut byte).await.is_err() { return; }
                                request.push(byte[0]);
                                assert!(request.len() < 8192, "bounded fixture headers");
                            }
                            let request = String::from_utf8(request).expect("request text");
                            assert!(request.starts_with("GET /.well-known/jwks.json HTTP/1.1\r\n"));
                            assert!(!request.to_ascii_lowercase().contains("authorization:"));
                            requests.fetch_add(1, Ordering::SeqCst);
                            let (body, hold) = {
                                let mut response = response.lock().expect("response lock");
                                (serde_json::to_vec(&response.body).expect("JWKS bytes"), response.hold.take())
                            };
                            if let Some(hold) = hold {
                                let _ = hold.entered.send(());
                                let _ = hold.release.await;
                            }
                            let headers = format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len());
                            if stream.write_all(headers.as_bytes()).await.is_err() { return; }
                            let _ = stream.write_all(&body).await;
                            let _ = stream.shutdown().await;
                        });
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        completed.expect("connection task").expect("fixture did not panic");
                    }
                }
            }
        });
        Self {
            endpoint,
            ca_pem: ca.pem().into_bytes(),
            response,
            requests,
            task: Some(task),
        }
    }

    pub(super) fn cache(&self) -> (IssuerKeys, TestClock) {
        IssuerKeys::with_test_clock(
            IssuerKeysConfig::new(ISSUER, &self.endpoint, &self.ca_pem)
                .expect("trusted issuer configuration"),
        )
        .expect("issuer cache")
    }

    pub(super) fn verifier(&self) -> (SessionVerifier, SessionTestClock, TestClock) {
        let (keys, key_clock) = self.cache();
        let (verifier, token_clock) = SessionVerifier::with_test_clock(keys, ORG, AUDIENCE, 1000)
            .expect("trusted verifier scope");
        (verifier, token_clock, key_clock)
    }

    pub(super) fn hold(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (entered, received) = oneshot::channel();
        let (release, released) = oneshot::channel();
        self.response.lock().expect("response lock").hold = Some(Hold {
            entered,
            release: released,
        });
        (received, release)
    }

    pub(super) fn remove_keys(&self) {
        self.response.lock().expect("response lock").body = json!({"keys": []});
    }

    pub(super) fn count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    pub(super) async fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            assert!(task.await.expect_err("stopped listener").is_cancelled());
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
