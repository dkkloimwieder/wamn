//! Real HTTPS transport with independently controlled cache-evidence clocks.
#![cfg(feature = "test-util")]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::pki_types::PrivatePkcs8KeyDer;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;
use wamn_runtime::session_keys::{IssuerKeys, IssuerKeysConfig, TestClock};

const ISSUER: &str = "https://identity.example.test/issuer";
const JWKS_PATH: &str = "/.well-known/jwks.json?fixed=1";

fn key(kid: &str) -> Value {
    json!({"kid": kid, "kty": "OKP", "crv": "Ed25519", "alg": "Ed25519",
        "use": "sig", "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"})
}

fn document(kids: &[&str]) -> Vec<u8> {
    serde_json::to_vec(&json!({"keys": kids.iter().map(|kid| key(kid)).collect::<Vec<_>>()}))
        .expect("serialize public key fixture")
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    chunked: bool,
    entered: Option<oneshot::Sender<()>>,
    release: Option<oneshot::Receiver<()>>,
}

impl Reply {
    fn keys(kids: &[&str]) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: document(kids),
            chunked: false,
            entered: None,
            release: None,
        }
    }

    fn age(mut self, age: &str) -> Self {
        self.headers.push(("Age".into(), age.into()));
        self
    }

    fn held(mut self) -> (Self, oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        self.entered = Some(entered_tx);
        self.release = Some(release_rx);
        (self, entered_rx, release_tx)
    }
}

#[derive(Default)]
struct Observed {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<String>>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

struct ActiveRequest(Arc<Observed>);

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Server {
    endpoint: String,
    ca_pem: Vec<u8>,
    observed: Arc<Observed>,
    task: Option<JoinHandle<()>>,
}

impl Server {
    async fn start() -> Self {
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
        let serving = observed.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (tcp, _) = accepted.expect("accept loopback connection");
                        let acceptor = acceptor.clone();
                        let observed = serving.clone();
                        connections.spawn(async move {
                            let Ok(mut stream) = acceptor.accept(tcp).await else { return; };
                            let mut request = Vec::new();
                            while !request.ends_with(b"\r\n\r\n") {
                                let mut byte = [0];
                                if stream.read_exact(&mut byte).await.is_err() { return; }
                                request.push(byte[0]);
                                assert!(request.len() < 8192, "bounded fixture request headers");
                            }
                            observed.requests.lock().expect("requests lock")
                                .push(String::from_utf8(request).expect("HTTP request text"));
                            let active = observed.active.fetch_add(1, Ordering::SeqCst) + 1;
                            observed.peak.fetch_max(active, Ordering::SeqCst);
                            let _active = ActiveRequest(observed.clone());
                            let mut reply = observed.replies.lock().expect("reply lock")
                                .pop_front().expect("each HTTP request must have an explicit reply");
                            let mut headers = format!("HTTP/1.1 {} Fixture\r\nConnection: close\r\nContent-Type: application/json\r\n", reply.status);
                            for (name, value) in &reply.headers {
                                headers.push_str(&format!("{name}: {value}\r\n"));
                            }
                            if reply.chunked {
                                headers.push_str("Transfer-Encoding: chunked\r\n\r\n");
                            } else {
                                headers.push_str(&format!("Content-Length: {}\r\n\r\n", reply.body.len()));
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

    fn queue(&self, reply: Reply) {
        self.observed
            .replies
            .lock()
            .expect("reply lock")
            .push_back(reply);
    }

    fn cache(&self) -> (IssuerKeys, TestClock) {
        let config = IssuerKeysConfig::new(ISSUER, &self.endpoint, &self.ca_pem)
            .expect("configured HTTPS issuer");
        IssuerKeys::with_test_clock(config).expect("public key cache")
    }

    fn count(&self) -> usize {
        self.observed.requests.lock().expect("requests lock").len()
    }

    async fn stop(&mut self) {
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

async fn entered(receiver: oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(2), receiver)
        .await
        .expect("HTTPS request reached fixture")
        .expect("request barrier");
}

#[tokio::test]
async fn configured_https_endpoint_and_exclusive_ca_are_the_only_trust_source() {
    let server = Server::start().await;
    let other = Server::start().await;
    for endpoint in [
        "http://127.0.0.1/jwks",
        "https://user:pass@example.test/jwks",
        "https://example.test/jwks#fragment",
    ] {
        assert!(IssuerKeysConfig::new(ISSUER, endpoint, &server.ca_pem).is_err());
    }
    assert!(
        IssuerKeysConfig::new(
            "http://issuer.example.test",
            &server.endpoint,
            &server.ca_pem
        )
        .is_err()
    );
    assert!(IssuerKeysConfig::new(ISSUER, &server.endpoint, b"").is_err());
    let wrong_ca = IssuerKeys::new(
        IssuerKeysConfig::new(ISSUER, &server.endpoint, &other.ca_pem)
            .expect("other CA configuration"),
    )
    .expect("other CA client");
    assert!(wrong_ca.key("trusted").await.is_err());
    assert_eq!(server.count(), 0, "TLS refusal precedes HTTP");
    let wrong_name = server.endpoint.replace("127.0.0.1", "localhost");
    let wrong_name = IssuerKeys::new(
        IssuerKeysConfig::new(ISSUER, &wrong_name, &server.ca_pem).expect("name mismatch config"),
    )
    .expect("name mismatch client");
    assert!(wrong_name.key("trusted").await.is_err());
    assert_eq!(server.count(), 0, "hostname verification precedes HTTP");
    let (cache, _) = server.cache();
    server.queue(Reply::keys(&["trusted"]));
    assert!(
        cache
            .key("https://untrusted.invalid/.well-known/jwks.json")
            .await
            .is_err()
    );
    let evidence = cache
        .key("trusted")
        .await
        .expect("key from configured endpoint");
    assert_eq!(cache.issuer(), ISSUER);
    assert_eq!(evidence.issuer(), ISSUER);
    assert_eq!(evidence.public_key().kid, "trusted");
    assert_eq!(server.count(), 1);
    let requests = server.observed.requests.lock().expect("requests lock");
    assert!(requests[0].starts_with(&format!("GET {JWKS_PATH} HTTP/1.1\r\n")));
    assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
}

#[tokio::test]
async fn delayed_in_window_age_is_anchored_at_start_and_hits_do_not_extend_it() {
    let mut server = Server::start().await;
    let (cache, clock) = server.cache();
    let started = clock.now();
    let (reply, barrier, release) = Reply::keys(&["old"]).age("120").held();
    server.queue(reply);
    let caller = cache.clone();
    let fetch = tokio::spawn(async move { caller.key("old").await });
    entered(barrier).await;
    clock.advance(Duration::from_secs(2));
    release.send(()).expect("release body");
    let evidence = fetch
        .await
        .expect("fetch task")
        .expect("remaining evidence window");
    assert_eq!(evidence.deadline(), started + Duration::from_secs(180));
    clock.advance(Duration::from_secs(177));
    assert!(evidence.is_fresh());
    assert_eq!(
        cache.key("old").await.expect("fresh hit").deadline(),
        evidence.deadline()
    );
    assert_eq!(server.count(), 1);
    server.stop().await;
    clock.advance(Duration::from_secs(1));
    assert!(!evidence.is_fresh(), "equality expires held evidence too");
    assert!(
        cache.key("old").await.is_err(),
        "known key cannot outlive evidence"
    );
}

#[tokio::test]
async fn late_results_are_discarded_at_and_after_their_age_adjusted_deadline() {
    for elapsed in [4, 5] {
        let server = Server::start().await;
        let (cache, clock) = server.cache();
        let (reply, barrier, release) = Reply::keys(&["late"]).age("296").held();
        server.queue(reply);
        let caller = cache.clone();
        let fetch = tokio::spawn(async move { caller.key("late").await });
        entered(barrier).await;
        clock.advance(Duration::from_secs(elapsed));
        release.send(()).expect("release late body");
        assert!(fetch.await.expect("fetch task").is_err());
        server.queue(Reply::keys(&[]));
        assert!(cache.key("late").await.is_err());
        assert_eq!(server.count(), 2, "late response was not installed");
    }
}

#[tokio::test]
async fn expired_malformed_and_ambiguous_age_are_refused() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    for age in ["300", "301", "18446744073709551616", "-1", "", "1, 2"] {
        server.queue(Reply::keys(&["old"]).age(age));
        assert!(cache.key("old").await.is_err(), "Age={age:?}");
        clock.advance(Duration::from_secs(1));
    }
    server.queue(Reply::keys(&["old"]).age("0").age("0"));
    assert!(cache.key("old").await.is_err());
    assert_eq!(server.count(), 7);
}

#[tokio::test]
async fn complete_replacement_removes_absent_keys_including_an_empty_set() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    server.queue(Reply::keys(&["old"]));
    cache.key("old").await.expect("warm old key");
    clock.advance(Duration::from_secs(1));
    server.queue(Reply::keys(&["new"]));
    cache.key("new").await.expect("new set");
    assert!(
        cache.key("old").await.is_err(),
        "old set is not merged back"
    );
    clock.advance(Duration::from_secs(1));
    server.queue(Reply::keys(&[]));
    assert!(cache.key("unknown").await.is_err());
    assert!(
        cache.key("new").await.is_err(),
        "empty set removes every key"
    );
    assert_eq!(server.count(), 3);
}

#[tokio::test]
async fn varied_unknown_ids_share_one_inflight_slot_and_one_attempt_start_interval() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    server.queue(Reply::keys(&["known"]));
    cache.key("known").await.expect("warm known key");
    for i in 0..32 {
        assert!(cache.key(&format!("unknown-{i}")).await.is_err());
    }
    assert_eq!(
        server.count(),
        1,
        "different IDs do not bypass the interval"
    );
    clock.advance(Duration::from_millis(999));
    assert!(cache.key("still-limited").await.is_err());
    assert_eq!(server.count(), 1);
    clock.advance(Duration::from_millis(1));
    let (reply, barrier, release) = Reply::keys(&["known"]).held();
    server.queue(reply);
    let caller = cache.clone();
    let fetch = tokio::spawn(async move { caller.key("missing").await });
    entered(barrier).await;
    clock.advance(Duration::from_secs(2));
    let mut contenders = JoinSet::new();
    for i in 0..32 {
        let caller = cache.clone();
        contenders.spawn(async move { caller.key(&format!("other-{i}")).await.is_err() });
    }
    while let Some(result) = contenders.join_next().await {
        assert!(result.expect("lookup task"));
    }
    assert!(
        cache.key("known").await.is_ok(),
        "fresh hits do not wait for refresh"
    );
    assert_eq!(
        server.count(),
        2,
        "elapsed interval cannot overlap an active request"
    );
    assert_eq!(server.observed.peak.load(Ordering::SeqCst), 1);
    release.send(()).expect("release response");
    assert!(fetch.await.expect("refresh task").is_err());
    // The interval is measured from attempt start, not completion.
    server.queue(Reply::keys(&["known"]));
    assert!(cache.key("next-missing").await.is_err());
    assert_eq!(server.count(), 3);
    for i in 0..32 {
        assert!(cache.key(&format!("limited-{i}")).await.is_err());
    }
    assert_eq!(server.count(), 3);
}

#[tokio::test]
async fn cancelled_request_releases_slot_without_resetting_its_interval() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    let (reply, barrier, release) = Reply::keys(&["old"]).held();
    server.queue(reply);
    let caller = cache.clone();
    let fetch = tokio::spawn(async move { caller.key("old").await });
    entered(barrier).await;
    fetch.abort();
    assert!(fetch.await.expect_err("cancelled lookup").is_cancelled());
    assert!(cache.key("another").await.is_err());
    assert_eq!(server.count(), 1);
    release.send(()).expect("release abandoned response");
    clock.advance(Duration::from_secs(1));
    server.queue(Reply::keys(&["new"]));
    cache
        .key("new")
        .await
        .expect("slot reusable after cancellation");
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn two_warm_caches_refuse_removed_key_with_endpoint_available() {
    let server = Server::start().await;
    let (first, first_clock) = server.cache();
    let (second, second_clock) = server.cache();
    server.queue(Reply::keys(&["removed"]));
    let first_evidence = first.key("removed").await.expect("first warm cache");
    server.queue(Reply::keys(&["removed"]).age("30"));
    let second_evidence = second.key("removed").await.expect("second warm cache");
    first_clock.advance(Duration::from_secs(300));
    second_clock.advance(Duration::from_secs(270));
    for cache in [&first, &second] {
        server.queue(Reply::keys(&["replacement"]));
        assert!(
            cache.key("removed").await.is_err(),
            "known ID is refreshed at expiry"
        );
        assert!(cache.key("replacement").await.is_ok());
    }
    assert!(!first_evidence.is_fresh());
    assert!(!second_evidence.is_fresh());
    assert_eq!(server.count(), 4);
}

#[tokio::test]
async fn two_warm_caches_fail_closed_at_deadline_when_endpoint_is_unavailable() {
    let mut server = Server::start().await;
    let (first, first_clock) = server.cache();
    let (second, second_clock) = server.cache();
    server.queue(Reply::keys(&["removed"]));
    let first_evidence = first.key("removed").await.expect("first warm cache");
    server.queue(Reply::keys(&["removed"]).age("30"));
    let second_evidence = second.key("removed").await.expect("second warm cache");
    server.stop().await;
    first_clock.advance(Duration::from_secs(299));
    second_clock.advance(Duration::from_secs(269));
    for cache in [&first, &second] {
        assert!(
            cache.key("missing").await.is_err(),
            "refresh really encounters outage"
        );
        assert!(
            cache.key("removed").await.is_ok(),
            "failed refresh does not erase unexpired evidence"
        );
    }
    first_clock.advance(Duration::from_secs(1));
    second_clock.advance(Duration::from_secs(1));
    assert!(!first_evidence.is_fresh());
    assert!(!second_evidence.is_fresh());
    for cache in [&first, &second] {
        assert!(cache.key("removed").await.is_err());
    }
    first_clock.advance(Duration::from_secs(1));
    second_clock.advance(Duration::from_secs(1));
    for cache in [&first, &second] {
        assert!(cache.key("removed").await.is_err());
    }
}

#[tokio::test]
async fn invalid_or_private_key_documents_are_not_installed() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    let mut invalid = vec![
        json!({"keys": [key("old"), key("old")]}),
        json!({"keys": [key("old")], "private": "refuse"}),
    ];
    for (field, value) in [
        ("d", "private"),
        ("alg", "EdDSA"),
        ("kty", "RSA"),
        ("crv", "X25519"),
        ("use", "enc"),
        ("x", "short"),
        ("kid", ""),
    ] {
        let mut bad = key("old");
        bad[field] = json!(value);
        invalid.push(json!({"keys": [bad]}));
    }
    for body in invalid {
        let mut reply = Reply::keys(&[]);
        reply.body = serde_json::to_vec(&body).expect("invalid fixture bytes");
        server.queue(reply);
        assert!(cache.key("old").await.is_err());
        clock.advance(Duration::from_secs(1));
    }
    server.queue(Reply::keys(&["old"]));
    cache
        .key("old")
        .await
        .expect("valid replacement after refused documents");
}

#[tokio::test]
async fn redirects_and_server_failures_are_not_followed_or_retried() {
    let server = Server::start().await;
    let redirect_target = Server::start().await;
    let (cache, clock) = server.cache();
    let mut redirect = Reply::keys(&["old"]);
    redirect.status = 302;
    redirect
        .headers
        .push(("Location".into(), redirect_target.endpoint.clone()));
    server.queue(redirect);
    assert!(cache.key("old").await.is_err());
    assert_eq!(server.count(), 1);
    assert_eq!(redirect_target.count(), 0);
    clock.advance(Duration::from_secs(1));
    let mut unavailable = Reply::keys(&["old"]);
    unavailable.status = 503;
    server.queue(unavailable);
    assert!(cache.key("old").await.is_err());
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn streaming_body_limit_accepts_exact_bound_and_refuses_the_next_byte() {
    let server = Server::start().await;
    for (size, admitted) in [(65_536, true), (65_537, false)] {
        let (cache, _) = server.cache();
        let mut reply = Reply::keys(&["old"]);
        reply.body.resize(size, b' ');
        reply.chunked = true;
        server.queue(reply);
        assert_eq!(
            cache.key("old").await.is_ok(),
            admitted,
            "body bytes={size}"
        );
    }
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn body_stall_is_bounded_by_five_seconds_without_retry() {
    let server = Server::start().await;
    let (cache, _) = server.cache();
    let (reply, barrier, release) = Reply::keys(&["old"]).held();
    server.queue(reply);
    let caller = cache.clone();
    let fetch = tokio::spawn(async move { caller.key("old").await });
    entered(barrier).await;
    let result = tokio::time::timeout(Duration::from_secs(7), fetch)
        .await
        .expect("five-second total timeout bounds stalled body")
        .expect("lookup task");
    assert!(result.is_err());
    assert_eq!(server.count(), 1);
    drop(release);
}
