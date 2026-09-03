//! The exit gate's assertions, driven through the whole client.
//!
//! BELOW THE TERMINAL LAYER, as the slice requires: a fake transport stands in
//! for the network, so envelope, error and paging semantics are proven without
//! a live server and without a rendered frame.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use wamn_client::{
    ClientError, CredentialProvider, HttpRequest, HttpResponse, RouteMetadata, StaticPat,
    Transport, WamnClient,
};

/// Records what the client sent and replays a canned response.
#[derive(Debug)]
struct FakeTransport {
    response: Mutex<HttpResponse>,
    seen: Mutex<Option<HttpRequest>>,
}

impl FakeTransport {
    fn replying(status: u16, body: &str) -> Arc<Self> {
        Arc::new(Self {
            response: Mutex::new(HttpResponse {
                status,
                body: body.to_owned(),
            }),
            seen: Mutex::new(None),
        })
    }

    fn request(&self) -> HttpRequest {
        self.seen
            .lock()
            .expect("lock")
            .clone()
            .expect("a request was sent")
    }
}

#[async_trait::async_trait]
impl Transport for FakeTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        *self.seen.lock().expect("lock") = Some(request);
        Ok(self.response.lock().expect("lock").clone())
    }
}

fn client(transport: Arc<FakeTransport>) -> WamnClient {
    WamnClient::new(
        "http://flow-http.wamn-system.svc/",
        Some("receiving.localhost".to_owned()),
        Arc::new(StaticPat::new("pat-abc").expect("token")) as Arc<dyn CredentialProvider>,
        transport as Arc<dyn Transport>,
    )
}

fn route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/purchase_order/update".to_owned(),
    }
}

fn item(request_id: &str) -> serde_json::Value {
    serde_json::json!({ "request_id": request_id, "id": "3f8e", "row_version": 4 })
}

#[tokio::test]
async fn the_request_carries_the_bearer_the_host_and_a_canonical_envelope() {
    let transport = FakeTransport::replying(200, r#"[{"request_id":"r1","value":{"ok":true}}]"#);
    client(Arc::clone(&transport))
        .invoke(&route(), &BTreeMap::new(), &[item("r1")])
        .await
        .expect("the call succeeds");

    let sent = transport.request();
    assert_eq!(
        sent.url,
        "http://flow-http.wamn-system.svc/purchase_order/update"
    );
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.headers["authorization"], "Bearer pat-abc");
    assert_eq!(sent.headers["host"], "receiving.localhost");
    assert_eq!(sent.headers["content-type"], "application/json");

    // The host header comes from the CLIENT's deployment config, not from the
    // route: a release records no host, so a route could not supply one.
    // The body is an ARRAY envelope, and canonical — the same bytes the
    // platform hashes elsewhere.
    let body: serde_json::Value = serde_json::from_slice(&sent.body).expect("body is JSON");
    assert!(body.is_array(), "the envelope is an array: {body}");
    assert_eq!(body[0]["request_id"], "r1");
}

/// The base URL is joined without doubling the separator: a trailing slash on
/// the deployment URL and a leading slash on the route must not produce `//`,
/// which some gateways route differently.
#[tokio::test]
async fn the_base_url_and_route_join_without_doubling() {
    let transport = FakeTransport::replying(200, r#"[{"request_id":"r1","value":null}]"#);
    client(Arc::clone(&transport))
        .invoke(&route(), &BTreeMap::new(), &[item("r1")])
        .await
        .expect("call");
    assert!(
        !transport.request().url.contains("svc//"),
        "{}",
        transport.request().url
    );
}

/// EXIT GATE: a stale row_version surfaces as concurrency_conflict carrying
/// BOTH revisions, through the whole client rather than in isolation.
#[tokio::test]
async fn a_stale_write_surfaces_both_revisions_through_the_client() {
    let transport = FakeTransport::replying(
        200,
        r#"[{"request_id":"r1","error":{"code":"concurrency_conflict",
             "detail":{"expected_row_version":4,"observed_row_version":7}}}]"#,
    );
    let outcomes = client(transport)
        .invoke(&route(), &BTreeMap::new(), &[item("r1")])
        .await
        .expect("the envelope itself succeeded");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].request_id, "r1");
    match outcomes[0].clone().into_result() {
        Err(ClientError::ConcurrencyConflict {
            expected_row_version,
            observed_row_version,
        }) => {
            assert_eq!((expected_row_version, observed_row_version), (4, 7));
        }
        other => panic!("expected a concurrency conflict, got {other:?}"),
    }
}

/// EXIT GATE: 401 is indistinguishable; 403 names the operation.
#[tokio::test]
async fn authentication_and_authorization_failures_differ_through_the_client() {
    let unauthenticated = client(FakeTransport::replying(401, r#"{"reason":"expired"}"#))
        .invoke(&route(), &BTreeMap::new(), &[item("r1")])
        .await
        .expect_err("401 refuses");
    assert_eq!(unauthenticated, ClientError::Unauthenticated);
    assert!(!unauthenticated.to_string().contains("expired"));

    let forbidden = client(FakeTransport::replying(
        403,
        r#"{"operation":"purchase_order.update"}"#,
    ))
    .invoke(&route(), &BTreeMap::new(), &[item("r1")])
    .await
    .expect_err("403 refuses");
    assert_eq!(
        forbidden,
        ClientError::PermissionDenied {
            operation: "purchase_order.update".to_owned(),
        }
    );
}

/// An outcome count that does not match the request is malformed. Silently
/// truncating would hand a caller fewer results than items it sent, and they
/// would read the missing ones as never attempted.
#[tokio::test]
async fn a_short_outcome_array_is_refused_rather_than_truncated() {
    let error = client(FakeTransport::replying(200, "[]"))
        .invoke(&route(), &BTreeMap::new(), &[item("r1"), item("r2")])
        .await
        .expect_err("a short response refuses");
    match error {
        ClientError::MalformedResponse { detail } => {
            assert!(detail.contains('2') && detail.contains('0'), "{detail}");
        }
        other => panic!("expected MalformedResponse, got {other:?}"),
    }
}

/// A route parameter the caller did not supply refuses before anything is
/// sent — the transport must never see a half-built URL.
#[tokio::test]
async fn a_missing_route_parameter_refuses_before_sending() {
    let transport = FakeTransport::replying(200, "[]");
    let parameterised = RouteMetadata {
        method: "POST".to_owned(),
        template: "/purchase_order/{id}".to_owned(),
    };
    let error = client(Arc::clone(&transport))
        .invoke(&parameterised, &BTreeMap::new(), &[item("r1")])
        .await
        .expect_err("a missing parameter refuses");
    assert_eq!(error.code(), "route_missing_parameter");
    assert!(
        transport.seen.lock().expect("lock").is_none(),
        "nothing may be sent when the route could not be built"
    );
}

/// The host is CLIENT config, not route config.
///
/// Two clients pointed at different deployments send the same release-derived
/// route to different hosts. This is the seam ruling 3 requires: a release
/// records no host — publication refuses an authored `route.host` — so a route
/// cannot supply one, and only the client can.
#[tokio::test]
async fn the_same_route_reaches_different_hosts_from_different_clients() {
    let mut seen = Vec::new();
    for host in ["receiving.localhost", "receiving.staging.internal"] {
        let transport = FakeTransport::replying(200, r#"[{"request_id":"r1","value":null}]"#);
        WamnClient::new(
            "http://flow-http.wamn-system.svc",
            Some(host.to_owned()),
            Arc::new(StaticPat::new("pat-abc").expect("token")) as Arc<dyn CredentialProvider>,
            Arc::clone(&transport) as Arc<dyn Transport>,
        )
        .invoke(&route(), &BTreeMap::new(), &[item("r1")])
        .await
        .expect("call");
        seen.push(transport.request().headers["host"].clone());
    }
    assert_eq!(seen, ["receiving.localhost", "receiving.staging.internal"]);
}

/// A client with no host sends none. A deployment that routes by path alone
/// must not receive a fabricated header.
#[tokio::test]
async fn a_client_with_no_host_sends_no_host_header() {
    let transport = FakeTransport::replying(200, r#"[{"request_id":"r1","value":null}]"#);
    WamnClient::new(
        "http://flow-http.wamn-system.svc",
        None,
        Arc::new(StaticPat::new("pat-abc").expect("token")) as Arc<dyn CredentialProvider>,
        Arc::clone(&transport) as Arc<dyn Transport>,
    )
    .invoke(&route(), &BTreeMap::new(), &[item("r1")])
    .await
    .expect("call");
    assert!(
        !transport.request().headers.contains_key("host"),
        "a host header was fabricated: {:?}",
        transport.request().headers
    );
}
