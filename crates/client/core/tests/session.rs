//! Session exchanges, cache renewal, and explicit fresh credentials through public APIs.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::json;
use wamn_client::credentials::{CredentialError, SessionCredentials, SessionTarget};
use wamn_client::descriptor::{FieldDescriptor, FieldSchema};
use wamn_client::request::build_request;
use wamn_client::{
    ClientError, CredentialProvider, HttpRequest, HttpResponse, RouteMetadata, StaticPat,
    Transport, WamnClient,
};

const AUDIENCE: &str = "acme/receiving/dev";
const OPAQUE_SESSION: &str = "opaque-session-not-a-jwt";
const PRIVATE_PAT: &str = "private-pat-never-log";

#[derive(Debug, Default)]
struct PatSource {
    ordinary_calls: AtomicUsize,
    fresh_calls: AtomicUsize,
}

#[async_trait::async_trait]
impl CredentialProvider for PatSource {
    async fn bearer(&self) -> Result<String, CredentialError> {
        self.ordinary_calls.fetch_add(1, Ordering::Relaxed);
        Ok("ordinary-session-not-a-pat".to_owned())
    }

    async fn fresh_bearer(&self) -> Result<String, CredentialError> {
        let number = self.fresh_calls.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(format!("{PRIVATE_PAT}-{number}"))
    }
}

#[derive(Debug)]
struct SessionOnly;

#[async_trait::async_trait]
impl CredentialProvider for SessionOnly {
    async fn bearer(&self) -> Result<String, CredentialError> {
        Ok(OPAQUE_SESSION.to_owned())
    }
}

#[derive(Debug)]
struct FailingPat;

#[async_trait::async_trait]
impl CredentialProvider for FailingPat {
    async fn bearer(&self) -> Result<String, CredentialError> {
        Err(CredentialError::new(PRIVATE_PAT))
    }

    async fn fresh_bearer(&self) -> Result<String, CredentialError> {
        Err(CredentialError::new(PRIVATE_PAT))
    }
}

#[derive(Debug)]
struct RecordingTransport {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ClientError>>>,
}

#[async_trait::async_trait]
impl Transport for RecordingTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        self.requests.lock().expect("record request").push(request);
        // Make simultaneous cache misses overlap at the actual exchange boundary.
        tokio::task::yield_now().await;
        self.replies
            .lock()
            .expect("read reply")
            .pop_front()
            .expect("an unexpected extra request consumed the reply queue")
    }
}

fn transport(
    replies: impl IntoIterator<Item = Result<HttpResponse, ClientError>>,
) -> Arc<RecordingTransport> {
    Arc::new(RecordingTransport {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(replies.into_iter().collect()),
    })
}

fn requests(transport: &RecordingTransport) -> Vec<HttpRequest> {
    transport.requests.lock().expect("read requests").clone()
}

fn reply(status: u16, body: impl Into<String>) -> HttpResponse {
    HttpResponse {
        status,
        body: body.into(),
    }
}

fn session_reply(token: &str) -> HttpResponse {
    let expires_at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("the test clock follows the Unix epoch")
        .as_secs()
        + 300;
    reply(
        200,
        json!({"access_token":token,"token_type":"Bearer","expires_at":expires_at}).to_string(),
    )
}

fn session(
    pat: Arc<dyn CredentialProvider>,
    exchange: Arc<RecordingTransport>,
) -> SessionCredentials {
    SessionCredentials::new(
        SessionTarget::new("https://identity.example/identity/", AUDIENCE)
            .expect("explicit HTTPS issuer and audience"),
        pat,
        exchange,
    )
}

fn assert_redacted(error: &CredentialError) {
    for rendered in [error.to_string(), format!("{error:?}")] {
        for secret in [PRIVATE_PAT, OPAQUE_SESSION] {
            assert!(!rendered.contains(secret), "credential leaked: {rendered}");
        }
    }
}

fn route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/inventory/adjust".to_owned(),
    }
}

#[test]
fn an_unsafe_issuer_or_empty_audience_is_refused_without_echoing_secrets() {
    for issuer in [
        "http://identity.example",
        "https://private-pat-never-log@identity.example",
        "https://identity.example?token=private-pat-never-log",
        "https://identity.example#opaque-session-not-a-jwt",
        " https://identity.example",
        "not-a-url-private-pat-never-log",
    ] {
        let error = SessionTarget::new(issuer, AUDIENCE).expect_err("unsafe issuer");
        assert_redacted(&error);
    }
    for audience in ["", " \t"] {
        assert!(SessionTarget::new("https://identity.example", audience).is_err());
    }
}

#[test]
fn request_debug_hides_credential_headers_case_insensitively_and_omits_the_body() {
    let request = HttpRequest {
        url: "https://routes.example/inventory/adjust".to_owned(),
        method: "POST".to_owned(),
        headers: BTreeMap::from([
            ("AuThOrIzAtIoN".to_owned(), format!("Bearer {PRIVATE_PAT}")),
            (
                "PROXY-AUTHORIZATION".to_owned(),
                "Basic private-proxy-secret".to_owned(),
            ),
            ("content-type".to_owned(), "application/json".to_owned()),
        ]),
        body: format!("private-request-body {OPAQUE_SESSION}").into_bytes(),
    };

    let rendered = format!("{request:?}");
    for secret in [
        PRIVATE_PAT,
        "private-proxy-secret",
        "private-request-body",
        OPAQUE_SESSION,
    ] {
        assert!(
            !rendered.contains(secret),
            "request Debug leaked: {rendered}"
        );
    }
    assert!(
        !rendered.contains(&format!("{:?}", request.body)),
        "body bytes leaked: {rendered}"
    );
}

#[tokio::test]
async fn login_posts_to_the_configured_issuer_and_caches_an_opaque_bearer() {
    let pat = Arc::new(PatSource::default());
    let exchange = transport([Ok(session_reply(OPAQUE_SESSION))]);
    let credentials = session(pat.clone(), exchange.clone());

    credentials.login().await.expect("exchange succeeds");
    assert_eq!(
        credentials.bearer().await.expect("cached bearer"),
        OPAQUE_SESSION
    );
    credentials
        .login()
        .await
        .expect("login reuses a valid session");

    assert_eq!(
        requests(&exchange),
        [HttpRequest {
            url: "https://identity.example/identity/session".to_owned(),
            method: "POST".to_owned(),
            headers: BTreeMap::from([
                (
                    "authorization".to_owned(),
                    format!("Bearer {PRIVATE_PAT}-1")
                ),
                ("content-type".to_owned(), "application/json".to_owned()),
            ]),
            body: br#"{"aud":"acme/receiving/dev"}"#.to_vec(),
        }]
    );
    assert_eq!(pat.ordinary_calls.load(Ordering::Relaxed), 0);
    assert_eq!(pat.fresh_calls.load(Ordering::Relaxed), 1);

    let rendered = format!("{credentials:?}");
    assert!(!rendered.contains(PRIVATE_PAT), "{rendered}");
    assert!(!rendered.contains(OPAQUE_SESSION), "{rendered}");
}

#[tokio::test(start_paused = true)]
async fn concurrent_callers_share_one_exchange_and_renew_after_the_cached_deadline() {
    let pat = Arc::new(PatSource::default());
    let exchange = transport([
        Ok(session_reply(OPAQUE_SESSION)),
        Ok(session_reply("opaque-renewed-session")),
    ]);
    let credentials = session(pat.clone(), exchange.clone());

    let (login, first, second) = tokio::join!(
        credentials.login(),
        credentials.bearer(),
        credentials.bearer()
    );
    login.expect("concurrent login");
    assert_eq!(first.expect("first caller"), OPAQUE_SESSION);
    assert_eq!(second.expect("second caller"), OPAQUE_SESSION);
    assert_eq!(requests(&exchange).len(), 1);

    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        credentials.bearer().await.expect("unexpired cache"),
        OPAQUE_SESSION
    );
    assert_eq!(requests(&exchange).len(), 1);
    tokio::time::advance(Duration::from_secs(300)).await;

    let (first, second, third) = tokio::join!(
        credentials.bearer(),
        credentials.bearer(),
        credentials.bearer()
    );
    for result in [first, second, third] {
        assert_eq!(result.expect("renewed bearer"), "opaque-renewed-session");
    }
    let sent = requests(&exchange);
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[1].headers["authorization"],
        format!("Bearer {PRIVATE_PAT}-2")
    );
    assert_eq!(pat.ordinary_calls.load(Ordering::Relaxed), 0);
    assert_eq!(pat.fresh_calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn explicit_fresh_credentials_bypass_exchange_and_default_providers_refuse() {
    let pat = Arc::new(PatSource::default());
    let exchange = transport([]);
    let credentials = session(pat.clone(), exchange.clone());
    assert_eq!(
        credentials.fresh_bearer().await.expect("fresh PAT"),
        format!("{PRIVATE_PAT}-1")
    );
    assert_eq!(
        credentials.fresh_bearer().await.expect("new fresh PAT"),
        format!("{PRIVATE_PAT}-2")
    );
    assert!(requests(&exchange).is_empty());
    assert_eq!(pat.ordinary_calls.load(Ordering::Relaxed), 0);
    assert_eq!(pat.fresh_calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        StaticPat::new(PRIVATE_PAT)
            .expect("PAT")
            .fresh_bearer()
            .await
            .expect("static fresh PAT"),
        PRIVATE_PAT
    );

    assert!(SessionOnly.fresh_bearer().await.is_err());
    let credentials = session(Arc::new(SessionOnly), exchange.clone());
    assert!(credentials.login().await.is_err());
    assert!(credentials.fresh_bearer().await.is_err());
    assert!(requests(&exchange).is_empty());
}

#[tokio::test]
async fn malformed_refused_or_expired_exchange_replies_fail_closed() {
    let valid: serde_json::Value =
        serde_json::from_str(&session_reply(OPAQUE_SESSION).body).expect("fixture JSON");
    let mut cases = vec![
        reply(200, format!("not JSON {PRIVATE_PAT} {OPAQUE_SESSION}")),
        reply(200, "{}"),
        reply(401, format!("{PRIVATE_PAT} {OPAQUE_SESSION}")),
        reply(503, format!("{PRIVATE_PAT} {OPAQUE_SESSION}")),
    ];
    for (field, value) in [
        ("access_token", json!("")),
        (
            "access_token",
            json!("opaque-session-not-a-jwt\r\nheader: injected"),
        ),
        ("access_token", json!(null)),
        ("token_type", json!("Basic")),
        ("token_type", json!("bearer")),
        ("expires_at", json!(0)),
        ("expires_at", json!(-1)),
        ("expires_at", json!(OPAQUE_SESSION)),
    ] {
        let mut body = valid.clone();
        body[field] = value;
        cases.push(reply(200, body.to_string()));
    }

    for response in cases {
        let pat = Arc::new(PatSource::default());
        let exchange = transport([Ok(response), Ok(session_reply("recovered-session"))]);
        let credentials = session(pat.clone(), exchange.clone());
        assert_redacted(
            &credentials
                .login()
                .await
                .expect_err("invalid exchange refuses"),
        );
        assert_eq!(requests(&exchange).len(), 1, "no exchange retry");
        assert_eq!(
            pat.ordinary_calls.load(Ordering::Relaxed),
            0,
            "no PAT fallback"
        );
        assert_eq!(pat.fresh_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            credentials.bearer().await.expect("explicit next request"),
            "recovered-session"
        );
        assert_eq!(requests(&exchange).len(), 2, "invalid token was not cached");
    }
}

#[tokio::test(start_paused = true)]
async fn failed_renewal_does_not_return_the_expired_session() {
    let exchange = transport([
        Ok(session_reply(OPAQUE_SESSION)),
        Ok(reply(401, "{}")),
        Ok(session_reply("recovered-session")),
    ]);
    let credentials = session(Arc::new(PatSource::default()), exchange.clone());
    credentials.login().await.expect("initial login");
    tokio::time::advance(Duration::from_secs(301)).await;

    assert!(
        credentials.bearer().await.is_err(),
        "expired cache cannot hide renewal refusal"
    );
    assert_eq!(requests(&exchange).len(), 2);
    assert_eq!(
        credentials.bearer().await.expect("explicit recovery"),
        "recovered-session"
    );
    assert_eq!(requests(&exchange).len(), 3);
}

#[tokio::test]
async fn provider_and_transport_errors_do_not_expose_credentials() {
    let exchange = transport([]);
    let credentials = session(Arc::new(FailingPat), exchange.clone());
    assert_redacted(&credentials.login().await.expect_err("PAT source failed"));
    assert_redacted(
        &credentials
            .fresh_bearer()
            .await
            .expect_err("fresh source failed"),
    );
    assert!(requests(&exchange).is_empty());

    let exchange = transport([Err(ClientError::Transport {
        detail: format!("{PRIVATE_PAT} {OPAQUE_SESSION}"),
    })]);
    let credentials = session(Arc::new(PatSource::default()), exchange.clone());
    assert_redacted(&credentials.login().await.expect_err("transport failed"));
    assert_eq!(requests(&exchange).len(), 1);
}

#[tokio::test]
async fn fresh_invoke_before_login_sends_only_the_pat_operation() {
    let pat = Arc::new(PatSource::default());
    let exchange = transport([]);
    let operations = transport([Ok(reply(
        200,
        r#"[{"request_id":"r1","value":{"ok":true}}]"#,
    ))]);
    let client = WamnClient::new(
        "https://routes.example",
        None,
        Arc::new(session(pat.clone(), exchange.clone())),
        operations.clone(),
    );

    let outcomes = client
        .invoke_fresh(&route(), &BTreeMap::new(), &[json!({"request_id":"r1"})])
        .await
        .expect("fresh invocation needs no session login");

    assert_eq!(
        serde_json::to_value(outcomes).expect("outcomes"),
        json!([{"request_id":"r1","value":{"ok":true}}])
    );
    let sent = requests(&operations);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url, "https://routes.example/inventory/adjust");
    assert_eq!(
        sent[0].headers["authorization"],
        format!("Bearer {PRIVATE_PAT}-1")
    );
    assert_eq!(sent[0].body, br#"[{"request_id":"r1"}]"#);
    assert!(
        requests(&exchange).is_empty(),
        "no session exchange before the operation"
    );
    assert_eq!(pat.ordinary_calls.load(Ordering::Relaxed), 0);
    assert_eq!(pat.fresh_calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn fresh_invoke_and_submit_use_pats_without_replacing_the_cached_session() {
    let pat = Arc::new(PatSource::default());
    let exchange = transport([Ok(session_reply(OPAQUE_SESSION))]);
    let credentials = Arc::new(session(pat.clone(), exchange.clone()));
    credentials
        .login()
        .await
        .expect("cache the ordinary session");
    let raw = reply(503, "opaque response evidence\n");
    let operations = transport([
        Ok(reply(200, r#"[{"request_id":"r1","value":{"ok":true}}]"#)),
        Ok(raw.clone()),
    ]);
    let client = WamnClient::new(
        "https://routes.example/",
        Some("receiving.localhost".to_owned()),
        credentials.clone(),
        operations.clone(),
    );
    let item = json!({"request_id":"r1"});
    let built = build_request(
        &[FieldSchema {
            field: FieldDescriptor {
                path: "request_id",
                type_name: "string",
                nullable: false,
                values: &[],
            },
            required: true,
            children: &[],
            minimum: None,
            maximum: None,
        }],
        &item,
        None,
    )
    .expect("capture the request");

    let outcomes = client
        .invoke_fresh(&route(), &BTreeMap::new(), &[item])
        .await
        .expect("fresh invoke");
    assert_eq!(
        serde_json::to_value(outcomes).expect("outcomes"),
        json!([{"request_id":"r1","value":{"ok":true}}])
    );
    assert_eq!(
        client
            .submit_fresh(&route(), &BTreeMap::new(), &built)
            .await
            .expect("fresh submit preserves response"),
        raw
    );
    assert_eq!(
        credentials
            .bearer()
            .await
            .expect("ordinary session remains cached"),
        OPAQUE_SESSION
    );

    let sent = requests(&operations);
    assert_eq!(sent.len(), 2);
    for (request, suffix) in sent.iter().zip([2, 3]) {
        assert_eq!(request.url, "https://routes.example/inventory/adjust");
        assert_eq!(request.method, "POST");
        assert_eq!(request.body, br#"[{"request_id":"r1"}]"#);
        assert_eq!(
            request.headers,
            BTreeMap::from([
                (
                    "authorization".to_owned(),
                    format!("Bearer {PRIVATE_PAT}-{suffix}")
                ),
                ("content-type".to_owned(), "application/json".to_owned()),
                ("host".to_owned(), "receiving.localhost".to_owned()),
            ])
        );
    }
    assert_eq!(requests(&exchange).len(), 1);
    assert_eq!(pat.ordinary_calls.load(Ordering::Relaxed), 0);
    assert_eq!(pat.fresh_calls.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn nested_fresh_required_remains_visible_without_retrying_the_operation() {
    let pat = Arc::new(PatSource::default());
    let exchange = transport([Ok(session_reply(OPAQUE_SESSION))]);
    let credentials = Arc::new(session(pat.clone(), exchange.clone()));
    let operations = transport([
        Ok(reply(
            403,
            r#"{"error":{"code":"fresh-credential-required","operation":"x"}}"#,
        )),
        Ok(reply(200, r#"[{"request_id":"r1","value":{"ok":true}}]"#)),
    ]);
    let client = WamnClient::new(
        "https://routes.example",
        Some("receiving.localhost".to_owned()),
        credentials,
        operations.clone(),
    );
    let error = client
        .invoke(&route(), &BTreeMap::new(), &[json!({"request_id":"r1"})])
        .await
        .expect_err("fresh-only operation refuses a session");

    assert_eq!(error.code(), "fresh-credential-required");
    let sent = requests(&operations);
    assert_eq!(
        sent.len(),
        1,
        "the refused operation must not be replayed with a PAT"
    );
    assert_eq!(
        sent[0].headers["authorization"],
        format!("Bearer {OPAQUE_SESSION}")
    );
    assert_eq!(requests(&exchange).len(), 1);
    assert_eq!(pat.ordinary_calls.load(Ordering::Relaxed), 0);
    assert_eq!(pat.fresh_calls.load(Ordering::Relaxed), 1);
}
