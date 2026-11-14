use std::collections::VecDeque;

use futures::channel::mpsc;
use futures::executor::block_on;
use futures::{FutureExt as _, StreamExt as _};
use serde_json::{Value, json};

use http_route::{
    AdapterLimits, AuthRejection, Backend, BodyReadError, BodyReader, DeliveryError,
    DeliveryFailure, DeliveryFailureType, DeliveryOutcome, DeliveryReport, DeliveryRequest, Header,
    ProviderError, RequestHead, RouteDefinition, SchemaInvalid, handle_request,
};

const AUTHENTICATED_USER_ID: &str = "11111111-1111-4111-8111-111111111111";

#[test]
fn health_head_needs_no_route_authentication_or_body() {
    let mut backend = FakeBackend::new(route());
    backend.fault = Fault::Routes;
    backend.auth = Err(AuthRejection {
        status: 401,
        code: "unauthenticated".to_string(),
    });
    let mut request = head();
    request.method = "HEAD".to_string();
    request.target = "/api/health".to_string();
    request.headers.clear();
    let mut body = Chunks::json(&[]);
    let response = block_on(handle_request(
        &mut backend,
        &mut body,
        &request,
        AdapterLimits::default(),
    ));
    assert_eq!(response.status, 204);
    assert!(response.body.is_empty());
    assert_eq!(
        response.cache_headers(),
        vec![("cache-control", "no-store".to_string())]
    );
    assert_eq!(body.reads, 0);
    assert!(backend.authenticated_attachments.is_empty());
    assert!(backend.deliveries.is_empty());

    backend.fault = Fault::None;
    for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        request.method = method.to_string();
        let response = block_on(handle_request(
            &mut backend,
            &mut body,
            &request,
            AdapterLimits::default(),
        ));
        assert_eq!(response.status, 404, "{method}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Routes,
    Deliver,
}

struct FakeBackend {
    routes: Vec<RouteDefinition>,
    auth: Result<Option<String>, AuthRejection>,
    delivery: Result<DeliveryOutcome, DeliveryError>,
    etag: Option<String>,
    fault: Fault,
    authenticated_attachments: Vec<String>,
    validated_inputs: Vec<(String, String)>,
    deliveries: Vec<DeliveryRequest<String>>,
}

impl FakeBackend {
    fn new(route: RouteDefinition) -> Self {
        Self {
            routes: vec![route],
            etag: None,
            auth: Ok(Some(AUTHENTICATED_USER_ID.to_string())),
            delivery: Ok(DeliveryOutcome::Respond(r#"{"ok":true}"#.to_string())),
            fault: Fault::None,
            authenticated_attachments: Vec::new(),
            validated_inputs: Vec::new(),
            deliveries: Vec::new(),
        }
    }
}

impl Backend for FakeBackend {
    type AuthenticatedCaller = String;

    fn routes(
        &mut self,
        _method: &str,
        _authority: &str,
    ) -> Result<Vec<RouteDefinition>, ProviderError> {
        (self.fault != Fault::Routes)
            .then(|| self.routes.clone())
            .ok_or(ProviderError)
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "record fixture effects when the asynchronous call is polled"
    )]
    async fn authenticate(
        &mut self,
        route: &RouteDefinition,
        _headers: &[Header],
        _body: &[u8],
    ) -> Result<Option<String>, AuthRejection> {
        self.authenticated_attachments
            .push(route.attachment_id.clone());
        self.auth.clone()
    }

    fn validate_input(
        &mut self,
        route: &RouteDefinition,
        payload: &str,
    ) -> Result<(), SchemaInvalid> {
        self.validated_inputs
            .push((route.attachment_id.clone(), payload.to_string()));
        Ok(())
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "record fixture effects when the asynchronous call is polled"
    )]
    async fn deliver(
        &mut self,
        _route: &RouteDefinition,
        request: DeliveryRequest<Self::AuthenticatedCaller>,
    ) -> DeliveryReport {
        self.deliveries.push(request);
        DeliveryReport {
            outcome: if self.fault == Fault::Deliver {
                Err(DeliveryError::ExecutionFailed)
            } else {
                self.delivery.clone()
            },
            etag: self.etag.clone(),
        }
    }
}

struct Chunks {
    chunks: VecDeque<Result<Option<Vec<u8>>, BodyReadError>>,
    reads: usize,
}

impl Chunks {
    fn json(chunks: &[&[u8]]) -> Self {
        let mut values = chunks
            .iter()
            .map(|chunk| Ok(Some(chunk.to_vec())))
            .collect::<VecDeque<_>>();
        values.push_back(Ok(None));
        Self {
            chunks: values,
            reads: 0,
        }
    }
}

impl BodyReader for Chunks {
    fn next_chunk(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<Vec<u8>>, BodyReadError>> {
        self.reads += 1;
        std::future::ready(self.chunks.pop_front().unwrap_or(Ok(None)))
    }
}

struct StreamedChunks {
    chunks: mpsc::UnboundedReceiver<Vec<u8>>,
    reads: usize,
}

impl BodyReader for StreamedChunks {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, BodyReadError> {
        self.reads += 1;
        Ok(self.chunks.next().await)
    }
}

fn route() -> RouteDefinition {
    RouteDefinition {
        attachment_id: "attachment-a".into(),
        path: "/receipts/42".into(),
        method: "POST".into(),
        body_limit: 1024,
        cache_control: None,
        sealed_operation_id: "test:receipt/create@1.0.0".into(),
        idempotency_rule: Some("request_id".into()),
        etag_inputs: None,
        operation_deadline_ms: 30_000,
        read_limits: None,
        auth_policy: json!({"modes":["none"]}),
    }
}

fn head() -> RequestHead {
    RequestHead {
        method: "post".to_string(),
        authority: "API.EXAMPLE.TEST".to_string(),
        target: "/receipts/42?tag=a&tag=b".to_string(),
        headers: vec![
            Header {
                name: "x-store".to_string(),
                value: "nyc".to_string(),
            },
            Header {
                name: "content-type".to_string(),
                value: "application/json".to_string(),
            },
            Header {
                name: "traceparent".to_string(),
                value: "00-abc-def-01".to_string(),
            },
        ],
    }
}

fn limits() -> AdapterLimits {
    AdapterLimits { body_bytes: 1024 }
}

fn request(
    backend: &mut FakeBackend,
    head: &RequestHead,
    bytes: &[u8],
) -> http_route::HttpResponse {
    let mut body = Chunks::json(&[bytes]);
    block_on(handle_request(backend, &mut body, head, limits()))
}

fn error_code(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .expect("JSON error")
        .pointer("/error/code")
        .and_then(Value::as_str)
        .expect("error code")
        .to_string()
}

fn error_operation(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .expect("JSON error")
        .pointer("/error/operation")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

#[test]
fn explicit_anonymous_admission_delivers_without_a_caller_identity() {
    let anonymous = route();
    let mut backend = FakeBackend::new(anonymous);
    backend.auth = Ok(None);

    let output = request(&mut backend, &head(), br#"{"amount":1}"#);

    assert_eq!(output.status, 200);
    assert_eq!(backend.authenticated_attachments, ["attachment-a"]);
    assert_eq!(backend.deliveries.len(), 1);
    assert_eq!(backend.deliveries[0].caller, None);
}

#[test]
fn an_authentication_refusal_never_reaches_delivery() {
    let anonymous = route();
    let mut backend = FakeBackend::new(anonymous);
    backend.auth = Err(AuthRejection {
        status: 401,
        code: "unauthorized".to_string(),
    });
    let mut body = Chunks::json(&[br#"{"amount":1}"#]);
    let output = block_on(handle_request(&mut backend, &mut body, &head(), limits()));

    assert_eq!(output.status, 401);
    assert_eq!(output.body, br#"{"error":{"code":"unauthorized"}}"#);
    assert_eq!(body.reads, 2, "authentication follows bounded body reads");
    assert!(backend.deliveries.is_empty());
}

#[test]
fn pending_body_resumes_under_the_selected_route_limit() {
    for (last_chunk, status, reads) in [(b"1}".as_slice(), 200, 3), (b"12}".as_slice(), 413, 2)] {
        let mut selected = route();
        selected.body_limit = 12;
        let mut backend = FakeBackend::new(selected);
        let (sender, chunks) = mpsc::unbounded();
        let mut body = StreamedChunks { chunks, reads: 0 };
        let request_head = head();
        sender.unbounded_send(br#"{"amount":"#.to_vec()).unwrap();

        let output = block_on(async {
            let mut request = Box::pin(handle_request(
                &mut backend,
                &mut body,
                &request_head,
                limits(),
            ));
            assert!(futures::poll!(request.as_mut()).is_pending());
            sender.unbounded_send(last_chunk.to_vec()).unwrap();
            drop(sender);
            request.await
        });

        assert_eq!(output.status, status);
        assert_eq!(body.reads, reads);
        if status == 200 {
            assert_eq!(backend.deliveries.len(), 1);
        } else {
            assert_eq!(output.body, b"request body exceeds 12-byte limit\n");
            assert!(backend.deliveries.is_empty());
        }
    }
}

#[test]
fn cancellation_while_awaiting_the_body_never_starts_delivery() {
    let mut backend = FakeBackend::new(route());
    let (sender, chunks) = mpsc::unbounded();
    let mut body = StreamedChunks { chunks, reads: 0 };
    sender.unbounded_send(br#"{"amount":"#.to_vec()).unwrap();

    assert!(
        handle_request(&mut backend, &mut body, &head(), limits())
            .now_or_never()
            .is_none()
    );

    assert_eq!(body.reads, 2, "the request awaited another body chunk");
    assert!(backend.authenticated_attachments.is_empty());
    assert!(backend.validated_inputs.is_empty());
    assert!(backend.deliveries.is_empty());
}

#[test]
fn malformed_oversize_and_auth_refusals_never_deliver() {
    let cases = [
        ("malformed", br#"{"amount":"#.as_slice(), 1024, None),
        ("oversize", br#"{"amount":123}"#.as_slice(), 4, None),
        (
            "auth",
            br#"{"amount":1}"#.as_slice(),
            1024,
            Some(AuthRejection {
                status: 401,
                code: "unauthorized".to_string(),
            }),
        ),
    ];
    for (name, bytes, body_limit, auth) in cases {
        let mut selected = route();
        selected.body_limit = body_limit;
        let mut backend = FakeBackend::new(selected);
        if let Some(rejection) = auth {
            backend.auth = Err(rejection);
        }
        let mut body = Chunks::json(&[bytes]);

        let output = block_on(handle_request(&mut backend, &mut body, &head(), limits()));

        assert!(matches!(output.status, 400 | 401 | 413), "{name}");
        assert!(
            backend.deliveries.is_empty(),
            "{name} unexpectedly delivered"
        );
        if name == "auth" {
            assert_eq!(body.reads, 2, "authentication follows bounded body reads");
        }
    }
}

#[test]
fn default_raw_body_ceiling_accepts_one_mebibyte_and_refuses_the_next_byte() {
    const BODY_LIMIT: usize = 1024 * 1024;

    let mut selected = route();
    selected.body_limit = usize::MAX;

    let mut accepted_body = vec![b' '; BODY_LIMIT];
    accepted_body[..4].copy_from_slice(b"null");
    let mut backend = FakeBackend::new(selected.clone());
    let mut body = Chunks::json(&[&accepted_body]);
    let output = block_on(handle_request(
        &mut backend,
        &mut body,
        &head(),
        AdapterLimits::default(),
    ));
    assert_eq!(output.status, 200);
    assert_eq!(backend.deliveries.len(), 1);

    let mut refused_body = accepted_body;
    refused_body.push(b' ');
    let mut backend = FakeBackend::new(selected);
    let mut body = Chunks::json(&[&refused_body]);
    let output = block_on(handle_request(
        &mut backend,
        &mut body,
        &head(),
        AdapterLimits::default(),
    ));
    assert_eq!(output.status, 413);
    assert_eq!(output.content_type, "text/plain; charset=utf-8");
    assert_eq!(output.body, b"request body exceeds 1048576-byte limit\n");
    assert!(backend.deliveries.is_empty());
}

#[test]
fn every_bridge_refusal_has_a_bounded_http_answer() {
    for (error, status, code) in [
        (DeliveryError::SourceNotFound, 404, "attachment-not-found"),
        (
            DeliveryError::InvalidRequest,
            400,
            "delivery-invalid-request",
        ),
        (
            DeliveryError::InvalidPayload,
            400,
            "delivery-invalid-payload",
        ),
        (DeliveryError::ExecutionFailed, 503, "execution-failed"),
    ] {
        let mut backend = FakeBackend::new(route());
        backend.delivery = Err(error);
        let output = request(&mut backend, &head(), br#"{"amount":1}"#);
        assert_eq!(
            (output.status, error_code(&output.body)),
            (status, code.to_string())
        );
    }
}

#[test]
fn missing_exact_operation_is_a_discoverable_forbidden_refusal() {
    const OPERATION: &str = "wamn-receiving:receipt/get@2.0.0";
    let mut backend = FakeBackend::new(route());
    backend.delivery = Err(DeliveryError::PermissionDenied {
        operation: OPERATION.to_string(),
    });

    let output = request(&mut backend, &head(), br#"{"amount":1}"#);

    assert_eq!(output.status, 403);
    assert_eq!(error_code(&output.body), "permission-denied");
    assert_eq!(error_operation(&output.body).as_deref(), Some(OPERATION));
    assert_eq!(backend.deliveries.len(), 1);
}

#[test]
fn authentication_backend_outage_has_one_generic_service_refusal() {
    let mut backend = FakeBackend::new(route());
    backend.auth = Err(AuthRejection {
        status: 503,
        code: "authentication-unavailable".to_string(),
    });

    let output = request(&mut backend, &head(), br#"{"amount":1}"#);

    assert_eq!(output.status, 503);
    assert_eq!(
        output.body,
        br#"{"error":{"code":"authentication-unavailable"}}"#
    );
    assert!(backend.deliveries.is_empty());
}

#[test]
fn provider_and_body_faults_are_bounded() {
    for (fault, code) in [
        (Fault::Routes, "routing-provider-failed"),
        (Fault::Permit, "route-limit-provider-failed"),
        (Fault::Deliver, "execution-failed"),
    ] {
        let mut backend = FakeBackend::new(route());
        backend.fault = fault;
        let output = request(&mut backend, &head(), br#"{"amount":1}"#);
        assert_eq!(output.status, 503);
        assert_eq!(error_code(&output.body), code);
    }

    let mut backend = FakeBackend::new(route());
    let mut body = Chunks {
        chunks: VecDeque::from([Err(BodyReadError)]),
        reads: 0,
    };
    let output = block_on(handle_request(&mut backend, &mut body, &head(), limits()));
    assert_eq!(error_code(&output.body), "body-read-failed");
    assert!(backend.deliveries.is_empty());
}

#[test]
fn a_read_authenticates_its_body_and_accepts_query_parameter_order() {
    let mut read = route();
    read.method = "GET".to_string();
    read.path = "/receipts/get".to_string();
    let request_element = json!({"filter": {"status": ["open"]}, "id": "r-1"});
    let query = read_query(request_element.as_object().unwrap());
    let mut get = head();
    get.method = "get".to_string();
    get.target = format!("/receipts/get?{query}");

    let mut backend = FakeBackend::new(read.clone());
    let mut body = Chunks::json(&[b"{\"ignored\":true}"]);
    let output = block_on(handle_request(&mut backend, &mut body, &get, limits()));
    assert_eq!(output.status, 200);
    assert_eq!(
        body.reads, 2,
        "a read bounds its body before authentication"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&backend.deliveries[0].payload).unwrap(),
        json!([request_element])
    );

    let mut reordered = get.clone();
    reordered.target = "/receipts/get?id=%22r-1%22&filter=%7b%7d".to_owned();
    let mut reordered_backend = FakeBackend::new(read.clone());
    assert_eq!(request(&mut reordered_backend, &reordered, b"").status, 200);

    for target in [
        "/receipts/get?id=r-1".to_string(),
        format!("/receipts/get?id=%22{}%22", "a".repeat(8 * 1024)),
    ] {
        let mut backend = FakeBackend::new(read.clone());
        let mut refused = get.clone();
        refused.target = target;
        let output = request(&mut backend, &refused, b"");
        assert_eq!(output.status, 400, "{}", refused.target);
        assert_eq!(error_code(&output.body), "invalid-target");
        assert!(backend.deliveries.is_empty());
    }
}

#[test]
fn a_query_read_answers_its_page_as_one_body() {
    let mut read = route();
    read.method = "GET".to_string();
    read.path = "/receipts/query".to_string();
    read.cache_control = Some("private, max-age=10, stale-while-revalidate=60".to_string());
    let request_element = json!({"limit": 10});
    let query = read_query(request_element.as_object().unwrap());
    let mut get = head();
    get.method = "get".to_string();
    get.target = format!("/receipts/query?{query}");

    let page = r#"[{"value":{"item":[{"id":"a"}],"next_cursor":null}}]"#;
    let mut backend = FakeBackend::new(read);
    backend.delivery = Ok(DeliveryOutcome::Respond(page.to_string()));
    backend.etag = Some("W/\"list\"".to_string());
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 200);
    assert_eq!(output.content_type, "application/json");
    assert_eq!(output.body, page.as_bytes());
    assert_eq!(
        output.cache_headers(),
        [
            (
                "cache-control",
                "private, max-age=10, stale-while-revalidate=60".to_string()
            ),
            ("vary", "Authorization, Cookie".to_string()),
            ("etag", "W/\"list\"".to_string()),
        ]
    );
    assert_eq!(
        serde_json::from_str::<Value>(&backend.deliveries[0].payload).unwrap(),
        json!([{"limit": 10}])
    );
}

#[test]
fn a_feed_read_is_revalidated_every_time_under_the_target_of_its_list() {
    let mut read = route();
    read.method = "GET".to_string();
    read.path = "/packaging/query".to_string();
    read.cache_control = Some("private, max-age=10, stale-while-revalidate=60".to_string());
    let request_element = json!({"filter": {"status": ["held"]}, "feed": {"cursor": "Y3Vyc29y"}});
    let query = read_query(request_element.as_object().unwrap());
    let mut get = head();
    get.method = "get".to_string();
    get.target = format!("/packaging/query?{query}");

    let changes = r#"[{"value":{"rows":[],"next_cursor":"Y3Vyc29y","complete":true}}]"#;
    let mut backend = FakeBackend::new(read);
    backend.delivery = Ok(DeliveryOutcome::Respond(changes.to_string()));
    backend.etag = Some("W/\"changes\"".to_string());
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 200);
    assert_eq!(
        output.cache_headers(),
        [
            ("cache-control", "private, no-cache".to_string()),
            ("vary", "Authorization, Cookie".to_string()),
            ("etag", "W/\"changes\"".to_string()),
        ]
    );
    // The query string decodes to the same request a body would carry.
    assert_eq!(
        serde_json::from_str::<Value>(&backend.deliveries[0].payload).unwrap(),
        json!([request_element])
    );
}

#[test]
fn only_a_successful_read_without_the_csrf_header_carries_its_cache_control() {
    const READ: &str = "private, max-age=10, stale-while-revalidate=60";
    let mut read = route();
    read.method = "GET".to_string();
    read.path = "/receipts/list".to_string();
    read.cache_control = Some(READ.to_string());
    let mut get = head();
    get.method = "get".to_string();
    get.target = "/receipts/list".to_string();

    let mut backend = FakeBackend::new(read.clone());
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 200);
    assert_eq!(
        output.cache_headers(),
        [
            ("cache-control", READ.to_string()),
            ("vary", "Authorization, Cookie".to_string())
        ]
    );

    let no_store = [("cache-control", "no-store".to_string())];
    let mut with_csrf = get.clone();
    with_csrf.headers.push(Header {
        name: "X-Wamn-Csrf".to_string(),
        value: "token".to_string(),
    });
    let mut backend = FakeBackend::new(read.clone());
    let output = request(&mut backend, &with_csrf, b"");
    assert_eq!(output.status, 200);
    assert_eq!(
        output.cache_headers(),
        no_store,
        "a request with the CSRF header"
    );

    let mut backend = FakeBackend::new(read.clone());
    backend.delivery = Err(DeliveryError::ExecutionFailed);
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 503);
    assert_eq!(output.cache_headers(), no_store, "a failed read");

    let mut backend = FakeBackend::new(read);
    backend.auth = Err(AuthRejection {
        status: 401,
        code: "unauthorized".to_string(),
    });
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 401);
    assert_eq!(output.cache_headers(), no_store, "a refused read");

    let mut backend = FakeBackend::new(route());
    let output = request(&mut backend, &head(), br#"{"amount":1}"#);
    assert_eq!(output.status, 200);
    assert_eq!(output.cache_headers(), no_store, "a write");
}

#[test]
fn a_read_forwards_if_none_match_and_answers_not_modified_with_its_tag_and_no_body() {
    const READ: &str = "private, max-age=10, stale-while-revalidate=60";
    const TAG: &str = "W/\"0123456789abcdef0123456789abcdef\"";
    let mut read = route();
    read.method = "GET".to_string();
    read.path = "/receipts/list".to_string();
    read.cache_control = Some(READ.to_string());
    let mut get = head();
    get.method = "get".to_string();
    get.target = "/receipts/list".to_string();
    get.headers.push(Header {
        name: "If-None-Match".to_string(),
        value: TAG.to_string(),
    });

    let mut backend = FakeBackend::new(read.clone());
    backend.etag = Some(TAG.to_string());
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 200);
    assert_eq!(
        backend.deliveries[0].if_none_match.as_deref(),
        Some(TAG),
        "the host compares the tag"
    );
    assert_eq!(
        output.cache_headers(),
        [
            ("cache-control", READ.to_string()),
            ("vary", "Authorization, Cookie".to_string()),
            ("etag", TAG.to_string()),
        ]
    );

    let mut backend = FakeBackend::new(read);
    backend.etag = Some(TAG.to_string());
    backend.delivery = Ok(DeliveryOutcome::NotModified);
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 304);
    assert!(output.body.is_empty(), "a 304 has no body");
    assert_eq!(
        output.cache_headers(),
        [
            ("cache-control", READ.to_string()),
            ("vary", "Authorization, Cookie".to_string()),
            ("etag", TAG.to_string()),
        ]
    );

    let mut write = head();
    write.headers.push(Header {
        name: "if-none-match".to_string(),
        value: TAG.to_string(),
    });
    let mut backend = FakeBackend::new(route());
    let output = request(&mut backend, &write, br#"{"amount":1}"#);
    assert_eq!(output.status, 200);
    assert_eq!(
        backend.deliveries[0].if_none_match, None,
        "a write is never conditional"
    );
}

fn read_query(request: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in request {
        query.append_pair(name, &value.to_string());
    }
    query.finish()
}
