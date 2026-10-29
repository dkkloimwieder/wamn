//! OAuth2 credentials exercise the host send path over a local listener.

use std::collections::VecDeque;
use std::convert::Infallible;

use http_body_util::{BodyExt as _, Full};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::*;

const CLIENT_ID: &str = "private client:id+";
const CLIENT_SECRET: &str = "private/secret&+";

struct Observed {
    path: String,
    method: String,
    headers: hyper::HeaderMap,
    body: Vec<u8>,
}

struct Fixture {
    base: String,
    observed: Arc<Mutex<Vec<Observed>>>,
    task: JoinHandle<()>,
    credentials: crate::connection_credentials::test_util::Fixture,
}

impl Fixture {
    async fn start(tokens: Vec<(u16, String)>, api: Vec<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/bound/", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let recorded = observed.clone();
        let tokens = Arc::new(Mutex::new(VecDeque::from(tokens)));
        let api = Arc::new(Mutex::new(VecDeque::from(api)));
        let task = tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let observed = recorded.clone();
                let tokens = tokens.clone();
                let api = api.clone();
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(tcp),
                        service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                            let observed = observed.clone();
                            let tokens = tokens.clone();
                            let api = api.clone();
                            async move {
                                let (parts, body) = request.into_parts();
                                let body = body.collect().await.unwrap().to_bytes().to_vec();
                                let token = matches!(parts.uri.path(), "/bound/token" | "/bound/");
                                observed.lock().unwrap().push(Observed {
                                    path: parts.uri.to_string(),
                                    method: parts.method.to_string(),
                                    headers: parts.headers,
                                    body,
                                });
                                let (status, body) = if token {
                                    tokens
                                        .lock()
                                        .unwrap()
                                        .pop_front()
                                        .unwrap_or((500, "unexpected token fetch".into()))
                                } else {
                                    let status = api.lock().unwrap().pop_front().unwrap_or(500);
                                    (status, format!("api-{status}"))
                                };
                                Ok::<_, Infallible>(
                                    hyper::Response::builder()
                                        .status(status)
                                        .header("connection", "close")
                                        .body(Full::new(bytes::Bytes::from(body)))
                                        .unwrap(),
                                )
                            }
                        }),
                    )
                    .await
                    .unwrap();
            }
        });
        Self {
            credentials: crate::connection_credentials::test_util::Fixture::new().await,
            base,
            observed,
            task,
        }
    }

    fn secret(&self) -> serde_json::Value {
        serde_json::json!({"oauth2": {
            "token-url": format!("{}token?audience=api", self.base),
            "client-id": CLIENT_ID,
            "client-secret": CLIENT_SECRET,
            "scope": "orders:read orders:write+"
        }})
    }

    async fn plugin(&mut self, secret: &serde_json::Value) -> ConnectionHttp {
        let mut plugin = offline_plugin();
        plugin.allowed_hosts =
            vec![self.base.strip_suffix("bound/").unwrap().parse().unwrap()].into();
        for name in ["handle", "other-handle"] {
            self.credentials
                .put(name, &secret.to_string(), &["project-a", "project-b"])
                .await;
        }
        plugin.vault = Arc::new(self.credentials.reader("project-a").await);
        plugin
    }

    async fn send(
        &self,
        plugin: &ConnectionHttp,
        generation: i64,
        handle: &str,
    ) -> Result<Response, ConnectionError> {
        let authority =
            parse_http_connection_authority(&self.base, TlsPolicy::Disabled, None).unwrap();
        let target = normalize_portable_http_target("/api").unwrap();
        let decision = resolve_http_request(
            &authority,
            &target,
            &plugin.allowed_hosts,
            &ConnectionNetworkPolicy,
            &TokioDnsResolver,
        )
        .await
        .unwrap();
        let scope = ClientScope {
            connection: ConnectionScope {
                env_key: plugin.env_key.clone(),
                project: plugin.project.clone(),
                environment: "dev".into(),
                instance: "instance".into(),
            },
            package: "package".into(),
            component_digest: "digest".into(),
            requirement: "manager".into(),
            binding_hash: "binding".into(),
            definition_hash: "definition".into(),
            generation,
        };
        let request = Request {
            path_and_query: "/api".into(),
            body: Some(b"guest-body".to_vec()),
            ..effect_request(vec![Header {
                name: "authorization".into(),
                value: b"guest-value".to_vec(),
            }])
        };
        let before = format!("{request:?}");
        let result = plugin
            .execute_authenticated(
                scope,
                &authority,
                decision,
                &request,
                &format!("org:acme:{handle}"),
            )
            .await;
        assert_eq!(
            format!("{request:?}"),
            before,
            "the guest request stays unchanged"
        );
        for value in [before, format!("{result:?}")] {
            assert!(!value.contains(CLIENT_ID));
            assert!(!value.contains(CLIENT_SECRET));
            assert!(!value.contains("fixture-token"));
        }
        result
    }

    fn counts(&self) -> (usize, usize) {
        let observed = self.observed.lock().unwrap();
        let tokens = observed
            .iter()
            .filter(|request| request.path.starts_with("/bound/token"))
            .count();
        (tokens, observed.len() - tokens)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn token(number: u8, expires_in: Option<u64>) -> (u16, String) {
    let mut value = serde_json::json!({"access_token": format!("fixture-token-{number}"), "token_type": "Bearer"});
    if let Some(seconds) = expires_in {
        value["expires_in"] = seconds.into();
    }
    (200, value.to_string())
}

#[tokio::test]
async fn fresh_and_cached_bearer_use_basic_form_authentication_without_guest_secrets() {
    use base64::Engine as _;

    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(vec![token(1, Some(3600))], vec![200, 200]).await;
    let plugin = fixture.plugin(&fixture.secret()).await;
    for _ in 0..2 {
        assert_eq!(
            fixture.send(&plugin, 7, "handle").await.unwrap().status,
            200
        );
    }
    assert_eq!(fixture.counts(), (1, 2));
    let observed = fixture.observed.lock().unwrap();
    let fetch = &observed[0];
    assert_eq!(fetch.method, "POST");
    assert_eq!(fetch.path, "/bound/token?audience=api");
    assert_eq!(
        fetch.headers["content-type"],
        "application/x-www-form-urlencoded"
    );
    let basic = fetch.headers["authorization"]
        .to_str()
        .unwrap()
        .strip_prefix("Basic ")
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(basic)
            .unwrap(),
        b"private+client%3Aid%2B:private%2Fsecret%26%2B"
    );
    assert_eq!(
        fetch.body,
        b"grant_type=client_credentials&scope=orders%3Aread+orders%3Awrite%2B"
    );
    for request in &observed[1..] {
        assert_eq!(request.headers["authorization"], "Bearer fixture-token-1");
        assert_eq!(request.body, b"guest-body");
        let wire = format!("{:?}{:?}", request.headers, request.body);
        assert!(!wire.contains(CLIENT_ID));
        assert!(!wire.contains(CLIENT_SECRET));
    }
}

#[tokio::test]
async fn token_endpoint_can_equal_the_bound_base_without_adding_a_query() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(vec![token(1, Some(3600))], vec![200]).await;
    let mut secret = fixture.secret();
    secret["oauth2"]["token-url"] = fixture.base.clone().into();
    let plugin = fixture.plugin(&secret).await;
    assert_eq!(
        fixture.send(&plugin, 7, "handle").await.unwrap().status,
        200
    );
    let observed = fixture.observed.lock().unwrap();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].path, "/bound/");
    assert_eq!(
        observed[1].headers["authorization"],
        "Bearer fixture-token-1"
    );
}

#[tokio::test]
async fn short_or_absent_expiry_never_caches_across_requests_and_scope_is_optional() {
    let _lock = wamn_test_postgres::lock();
    for expiry in [None, Some(0), Some(59), Some(60)] {
        let mut fixture =
            Fixture::start(vec![token(1, expiry), token(2, expiry)], vec![200, 200]).await;
        let mut secret = fixture.secret();
        secret["oauth2"].as_object_mut().unwrap().remove("scope");
        let plugin = fixture.plugin(&secret).await;
        for _ in 0..2 {
            fixture.send(&plugin, 7, "handle").await.unwrap();
        }
        assert_eq!(fixture.counts(), (2, 2));
        assert_eq!(
            fixture.observed.lock().unwrap()[0].body,
            b"grant_type=client_credentials"
        );
    }
}

#[tokio::test]
async fn clock_seam_expires_cached_token_at_the_sixty_second_margin() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(
        vec![token(1, Some(3600)), token(2, Some(3600))],
        vec![200, 200],
    )
    .await;
    let mut plugin = fixture.plugin(&fixture.secret()).await;
    fixture.send(&plugin, 7, "handle").await.unwrap();
    plugin.oauth2.now = || Instant::now() + Duration::from_mins(59);
    fixture.send(&plugin, 7, "handle").await.unwrap();
    assert_eq!(fixture.counts(), (2, 2));
    assert_eq!(
        fixture.observed.lock().unwrap()[3].headers["authorization"],
        "Bearer fixture-token-2"
    );
}

#[tokio::test]
async fn cache_separates_project_connection_and_generation() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(
        (1..=4).map(|n| token(n, Some(3600))).collect(),
        vec![200; 4],
    )
    .await;
    let mut plugin = fixture.plugin(&fixture.secret()).await;
    for (generation, handle) in [(7, "handle"), (8, "handle"), (8, "other-handle")] {
        fixture.send(&plugin, generation, handle).await.unwrap();
    }
    plugin.project = "project-b".into();
    plugin.vault = Arc::new(fixture.credentials.reader("project-b").await);
    fixture.send(&plugin, 8, "handle").await.unwrap();
    assert_eq!(fixture.counts(), (4, 4));
}

#[tokio::test]
async fn refused_token_never_dispatches_api_or_exposes_error_body() {
    for status in [400, 401, 302, 500] {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::start(
            vec![(status, format!("{CLIENT_ID} {CLIENT_SECRET}"))],
            vec![],
        )
        .await;
        let plugin = fixture.plugin(&fixture.secret()).await;
        assert!(matches!(
            fixture.send(&plugin, 7, "handle").await,
            Err(ConnectionError::CredentialUnavailable)
        ));
        assert_eq!(fixture.counts(), (1, 0));
    }
}

#[tokio::test]
async fn api_401_fetches_once_more_and_returns_the_second_response() {
    for second in [200, 401] {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::start(
            vec![token(1, Some(3600)), token(2, Some(3600))],
            vec![401, second],
        )
        .await;
        let plugin = fixture.plugin(&fixture.secret()).await;
        assert_eq!(
            fixture.send(&plugin, 7, "handle").await.unwrap().status,
            second
        );
        assert_eq!(fixture.counts(), (2, 2));
        let observed = fixture.observed.lock().unwrap();
        assert_eq!(
            observed[1].headers["authorization"],
            "Bearer fixture-token-1"
        );
        assert_eq!(
            observed[3].headers["authorization"],
            "Bearer fixture-token-2"
        );
        assert_eq!(observed[1].body, observed[3].body);
        if second == 401 {
            assert!(plugin.oauth2.entries.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn failed_refresh_returns_the_first_api_401_without_another_api_request() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(
        vec![
            token(1, Some(3600)),
            (400, format!("{CLIENT_ID} {CLIENT_SECRET}")),
        ],
        vec![401],
    )
    .await;
    let plugin = fixture.plugin(&fixture.secret()).await;
    let response = fixture.send(&plugin, 7, "handle").await.unwrap();
    assert_eq!(response.status, 401);
    assert_eq!(response.body, b"api-401");
    assert_eq!(fixture.counts(), (2, 1));
    assert!(plugin.oauth2.entries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn token_url_outside_binding_or_ambiguous_path_refuses_before_dispatch() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(vec![], vec![]).await;
    let origin = fixture.base.strip_suffix("bound/").unwrap();
    for endpoint in [
        "http://other.invalid/bound/token".into(),
        format!("{origin}outside/token"),
        format!("{origin}bound-other/token"),
        format!("{}../token", fixture.base),
        format!("{}%2e%2e/token", fixture.base),
        format!("{}token#fragment", fixture.base),
        "/bound/token".into(),
    ] {
        let mut secret = fixture.secret();
        secret["oauth2"]["token-url"] = endpoint.into();
        let plugin = fixture.plugin(&secret).await;
        assert!(matches!(
            fixture.send(&plugin, 7, "handle").await,
            Err(ConnectionError::CredentialUnavailable)
        ));
        assert_eq!(fixture.counts(), (0, 0));
    }
}

#[tokio::test]
async fn malformed_credentials_and_multiple_arms_refuse_before_dispatch() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(vec![], vec![]).await;
    let valid = fixture.secret();
    let mut invalid = vec![serde_json::json!({"oauth2": null})];
    for field in ["token-url", "client-id", "client-secret"] {
        let mut secret = valid.clone();
        secret["oauth2"].as_object_mut().unwrap().remove(field);
        invalid.push(secret);
        let mut secret = valid.clone();
        secret["oauth2"][field] = "".into();
        invalid.push(secret);
    }
    for arm in ["headers", "hmac", "oauth1"] {
        let mut secret = valid.clone();
        secret[arm] = serde_json::json!({});
        invalid.push(secret);
    }
    let mut unknown = valid.clone();
    unknown["oauth2"]["extra"] = true.into();
    invalid.push(unknown);
    for secret in invalid {
        let plugin = fixture.plugin(&secret).await;
        assert!(matches!(
            fixture.send(&plugin, 7, "handle").await,
            Err(ConnectionError::CredentialUnavailable)
        ));
    }
    assert_eq!(fixture.counts(), (0, 0));
}

#[tokio::test]
async fn malformed_token_response_refuses_without_leaking_its_body() {
    for body in [
        format!("{CLIENT_ID} {CLIENT_SECRET}"),
        r#"{"access_token":"ok","token_type":"MAC"}"#.into(),
        r#"{"access_token":"","token_type":"Bearer"}"#.into(),
        r#"{"access_token":"bad\r\nheader","token_type":"Bearer"}"#.into(),
        r#"{"access_token":"ok"}"#.into(),
        r#"{"access_token":"ok","token_type":"Bearer","expires_in":-1}"#.into(),
    ] {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::start(vec![(200, body)], vec![]).await;
        let plugin = fixture.plugin(&fixture.secret()).await;
        assert!(matches!(
            fixture.send(&plugin, 7, "handle").await,
            Err(ConnectionError::CredentialUnavailable)
        ));
        assert_eq!(fixture.counts(), (1, 0));
    }
}
