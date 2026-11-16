//! HTTPS issuer and signing fixtures shared by authentication and SQL tests.

use std::fmt::Write as _;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use wamn_identity_protocol::verifier::{SessionTestClock, SessionVerifier};

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::pki_types::PrivatePkcs8KeyDer;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;
use wamn_authentication::session_keys::{IssuerKeys, IssuerKeysConfig, TestClock};

pub(super) const ISSUER: &str = "https://identity.example.test/issuer";
pub(super) const ORG: &str = "org-a";
pub(super) const AUDIENCE: &str = "urn:wamn:project-env:org-a:dev:project:instance-one";

fn pair() -> Ed25519KeyPair {
    // Fixed test-only seed. Production signing keys never enter this crate.
    Ed25519KeyPair::from_seed_unchecked(&[7; 32]).expect("fixture signing key")
}

pub(super) fn header() -> Value {
    json!({"alg": "EdDSA", "typ": "wamn-session+jwt", "kid": "key-one"})
}

pub(super) fn claims() -> Value {
    json!({"iss": ISSUER, "sub": "ed7056a9-5639-455f-9640-4678458794c0",
        "org": ORG, "aud": AUDIENCE, "roles": ["purchase-reader"],
        "iat": 1000, "exp": 1900, "authority": {"login": "ed7056a9-5639-455f-9640-4678458794c0"}, "jti": "session-test"})
}

pub(super) fn signed(header: &Value, claims: &Value) -> String {
    let message = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).expect("header bytes")),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("JWT claim bytes")),
    );
    let signature = pair().sign(message.as_bytes());
    format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
}

const JWKS_PATH: &str = "/.well-known/jwks.json?fixed=1";

pub(super) fn key(kid: &str) -> Value {
    json!({"kid": kid, "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA",
        "use": "sig", "x": URL_SAFE_NO_PAD.encode(pair().public_key().as_ref())})
}

fn document(kids: &[&str]) -> Vec<u8> {
    serde_json::to_vec(&json!({"keys": kids.iter().map(|kid| key(kid)).collect::<Vec<_>>()}))
        .expect("serialize public key fixture")
}

pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
    pub(super) chunked: bool,
    entered: Option<oneshot::Sender<()>>,
    release: Option<oneshot::Receiver<()>>,
}

impl Reply {
    pub(super) fn keys(kids: &[&str]) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: document(kids),
            chunked: false,
            entered: None,
            release: None,
        }
    }

    pub(super) fn age(mut self, age: &str) -> Self {
        self.headers.push(("Age".into(), age.into()));
        self
    }

    pub(super) fn held(mut self) -> (Self, oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        self.entered = Some(entered_tx);
        self.release = Some(release_rx);
        (self, entered_rx, release_tx)
    }
}

#[derive(Default)]
pub(super) struct Observed {
    replies: Mutex<VecDeque<Reply>>,
    requests: AtomicUsize,
    active: AtomicUsize,
    pub(super) peak: AtomicUsize,
}

struct ActiveRequest(Arc<Observed>);

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) struct Server {
    pub(super) endpoint: String,
    pub(super) ca_pem: Vec<u8>,
    pub(super) observed: Arc<Observed>,
    task: Option<JoinHandle<()>>,
}

impl Server {
    pub(super) async fn start() -> Self {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_key = KeyPair::generate().expect("CA key");
        let ca = ca_params.self_signed(&ca_key).expect("CA certificate");
        let issuer = Issuer::new(ca_params, ca_key);
        let leaf_key = KeyPair::generate().expect("TLS key");
        let leaf = CertificateParams::new(vec!["127.0.0.1".into()])
            .expect("TLS params")
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
            .expect("loopback listener");
        let endpoint = format!(
            "https://{}{JWKS_PATH}",
            listener.local_addr().expect("listener address")
        );
        let observed = Arc::new(Observed::default());
        let serving = Arc::clone(&observed);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (tcp, _) = accepted.expect("accept loopback connection");
                        let acceptor = acceptor.clone();
                        let observed = Arc::clone(&serving);
                        connections.spawn(async move {
                            let Ok(mut stream) = acceptor.accept(tcp).await else { return; };
                            let mut request = Vec::new();
                            while !request.ends_with(b"\r\n\r\n") {
                                let mut byte = [0];
                                if stream.read_exact(&mut byte).await.is_err() { return; }
                                request.push(byte[0]);
                                assert!(request.len() < 8192, "bounded fixture request headers");
                            }
                            observed.requests.fetch_add(1, Ordering::SeqCst);
                            let active = observed.active.fetch_add(1, Ordering::SeqCst) + 1;
                            observed.peak.fetch_max(active, Ordering::SeqCst);
                            let _active = ActiveRequest(Arc::clone(&observed));
                            let mut reply = observed.replies.lock().expect("reply lock")
                                .pop_front().unwrap_or_else(|| Reply::keys(&["key-one"]));
                            let mut headers = format!("HTTP/1.1 {} Fixture\r\nConnection: close\r\nContent-Type: application/json\r\n", reply.status);
                            for (name, value) in &reply.headers {
                                writeln!(headers, "{name}: {value}\r").expect("writing to a String cannot fail");
                            }
                            if reply.chunked {
                                headers.push_str("Transfer-Encoding: chunked\r\n\r\n");
                            } else {
                                writeln!(headers, "Content-Length: {}\r\n\r", reply.body.len()).expect("writing to a String cannot fail");
                            }
                            if stream.write_all(headers.as_bytes()).await.is_err() { return; }
                            if let Some(entered) = reply.entered.take() { let _ = entered.send(()); }
                            if let Some(release) = reply.release.take() { let _ = release.await; }
                            if reply.chunked {
                                for chunk in reply.body.chunks(1024) {
                                    if stream.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await.is_err()
                                        || stream.write_all(chunk).await.is_err()
                                        || stream.write_all(b"\r\n").await.is_err() { return; }
                                }
                                let _ = stream.write_all(b"0\r\n\r\n").await;
                            } else {
                                let _ = stream.write_all(&reply.body).await;
                            }
                            let _ = stream.shutdown().await;
                        });
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        completed.expect("connection task").expect("fixture connection did not panic");
                    }
                }
            }
        });
        Self {
            endpoint,
            ca_pem: ca.pem().into_bytes(),
            observed,
            task: Some(task),
        }
    }

    pub(super) fn queue(&self, reply: Reply) {
        self.observed
            .replies
            .lock()
            .expect("reply lock")
            .push_back(reply);
    }

    pub(super) fn cache(&self) -> (IssuerKeys, TestClock) {
        let config = IssuerKeysConfig::new(ISSUER, &self.endpoint, &self.ca_pem)
            .expect("configured HTTPS issuer");
        IssuerKeys::with_test_clock(config)
    }

    pub(super) fn verifier(&self) -> (SessionVerifier<IssuerKeys>, SessionTestClock, TestClock) {
        let (keys, key_clock) = self.cache();
        let (verifier, token_clock) = SessionVerifier::with_test_clock(keys, ORG, AUDIENCE, 1000)
            .expect("trusted verifier audience");
        (verifier, token_clock, key_clock)
    }

    pub(super) fn count(&self) -> usize {
        self.observed.requests.load(Ordering::SeqCst)
    }

    pub(super) async fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            assert!(task.await.expect_err("listener stopped").is_cancelled());
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

pub(super) async fn entered(receiver: oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(2), receiver)
        .await
        .expect("HTTPS request reached fixture")
        .expect("request barrier");
}
