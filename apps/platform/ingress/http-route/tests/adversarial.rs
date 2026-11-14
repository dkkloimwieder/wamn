use std::collections::VecDeque;

use futures::channel::mpsc;
use futures::executor::block_on;
use futures::{FutureExt as _, StreamExt as _};
use serde_json::{Value, json};

use http_route::{
    AdapterLimits, AuthRejection, Backend, BodyReadError, BodyReader, Header, HttpResponse,
    OperationRequest, ProviderError, ReadCache, RequestHead, RouteDefinition, SchemaInvalid,
    handle_request,
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
    assert_eq!(body.reads, 0);
    assert!(backend.authenticated_attachments.is_empty());
    assert!(backend.invocations.is_empty());

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
}

struct FakeBackend {
    routes: Vec<RouteDefinition>,
    auth: Result<Option<String>, AuthRejection>,
    response: HttpResponse,
    etag: Option<String>,
    fault: Fault,
    authenticated_attachments: Vec<String>,
    validated_inputs: Vec<(String, String)>,
    invocations: Vec<OperationRequest<String>>,
}

impl FakeBackend {
    fn new(route: RouteDefinition) -> Self {
        Self {
            routes: vec![route],
            etag: None,
            auth: Ok(Some(AUTHENTICATED_USER_ID.to_string())),
            response: response(200, r#"{"ok":true}"#),
            fault: Fault::None,
            authenticated_attachments: Vec::new(),
            validated_inputs: Vec::new(),
            invocations: Vec::new(),
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
    async fn invoke(
        &mut self,
        _route: &RouteDefinition,
        request: OperationRequest<Self::AuthenticatedCaller>,
    ) -> HttpResponse {
        self.invocations.push(request);
        let mut response = self.response.clone();
        response.read = Some(Box::new(ReadCache {
            cache_control: None,
            etag: self.etag.clone(),
        }));
        response
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

#[test]
fn explicit_anonymous_admission_invokes_without_a_caller_identity() {
    let anonymous = route();
    let mut backend = FakeBackend::new(anonymous);
    backend.auth = Ok(None);

    let output = request(&mut backend, &head(), br#"{"amount":1}"#);

    assert_eq!(output.status, 200);
    assert!(!backend.authenticated_attachments.is_empty());
    assert_eq!(backend.invocations.len(), 1);
    assert_eq!(backend.invocations[0].caller, None);
}

#[test]
fn an_authentication_refusal_never_reaches_invocation() {
    let anonymous = route();
    let mut backend = FakeBackend::new(anonymous);
    backend.auth = Err(AuthRejection {
        status: 401,
        code: "unauthorized".to_string(),
    });
    let mut body = Chunks::json(&[br#"{"amount":1}"#]);
    let output = block_on(handle_request(&mut backend, &mut body, &head(), limits()));

    assert_eq!(output.status, 401);
    assert_eq!(body.reads, 2, "authentication follows bounded body reads");
    assert!(backend.invocations.is_empty());
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
            assert_eq!(backend.invocations.len(), 1);
        } else {
            assert!(backend.invocations.is_empty());
        }
    }
}

#[test]
fn cancellation_while_awaiting_the_body_never_starts_invocation() {
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
    assert!(backend.invocations.is_empty());
}

#[test]
fn malformed_oversize_and_auth_refusals_never_invoke() {
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
            backend.invocations.is_empty(),
            "{name} unexpectedly invoked"
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
    selected.body_limit = u64::MAX;

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
    assert_eq!(backend.invocations.len(), 1);

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
    assert!(backend.invocations.is_empty());
}

#[test]
fn missing_exact_operation_is_a_discoverable_forbidden_refusal() {
    let mut backend = FakeBackend::new(route());
    backend.response = response(
        403,
        &json!({"error": {"operation": backend.routes[0].sealed_operation_id}}).to_string(),
    );
    backend.etag = Some("W/\"stale\"".into());

    let output = request(&mut backend, &head(), br#"{"amount":1}"#);

    assert_eq!(output.status, 403);
    let body: Value = serde_json::from_slice(&output.body).unwrap();
    assert_eq!(
        body["error"]["operation"].as_str(),
        Some(backend.routes[0].sealed_operation_id.as_str())
    );
    assert_eq!(backend.invocations.len(), 1);
    assert!(output.read.is_none());
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
    assert!(backend.invocations.is_empty());
}

#[test]
fn provider_and_body_faults_are_bounded() {
    let mut backend = FakeBackend::new(route());
    backend.fault = Fault::Routes;
    let mut body = Chunks::json(&[br#"{"amount":1}"#]);
    let output = block_on(handle_request(&mut backend, &mut body, &head(), limits()));
    assert_eq!(output.status, 503);
    assert!(output.read.is_none());
    assert_eq!(body.reads, 0);
    assert!(backend.authenticated_attachments.is_empty());
    assert!(backend.invocations.is_empty());

    let mut backend = FakeBackend::new(route());
    let mut body = Chunks {
        chunks: VecDeque::from([Err(BodyReadError)]),
        reads: 0,
    };
    let output = block_on(handle_request(&mut backend, &mut body, &head(), limits()));
    assert_eq!(output.status, 400);
    assert!(backend.invocations.is_empty());
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
    assert!(
        serde_json::from_str::<Value>(&backend.invocations[0].payload)
            .unwrap()
            .is_array()
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
        assert!(backend.invocations.is_empty());
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
    backend.response = response(200, page);
    backend.etag = Some("W/\"list\"".to_string());
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 200);
    assert!(
        serde_json::from_slice::<Value>(&output.body)
            .unwrap()
            .is_array()
    );
    assert!(
        serde_json::from_str::<Value>(&backend.invocations[0].payload)
            .unwrap()
            .is_array()
    );
}

#[test]
fn a_feed_read_is_revalidated_every_time_under_the_target_of_its_list() {
    for (policy, is_public) in [
        ("private, max-age=10, stale-while-revalidate=60", false),
        ("public, max-age=10, stale-while-revalidate=60", true),
    ] {
        let mut read = route();
        read.method = "GET".into();
        read.cache_control = Some(policy.into());
        let mut get = head();
        get.method = "GET".into();
        let element = json!({"feed": {"cursor": "Y3Vyc29y"}});
        get.target = format!("{}?{}", read.path, read_query(element.as_object().unwrap()));
        let mut backend = FakeBackend::new(read);

        let output = request(&mut backend, &get, b"");

        assert_eq!(output.status, 200);
        assert_eq!(backend.invocations.len(), 1);
        let headers = output.cache_headers();
        let control = headers
            .iter()
            .find_map(|(name, value)| name.eq_ignore_ascii_case("cache-control").then_some(value))
            .unwrap();
        let mut visibility = None;
        let mut revalidate = false;
        let mut no_store = false;
        let mut fresh_for = None;
        let mut stale_for = None;
        for directive in control.split(',').map(str::trim) {
            match directive.split_once('=') {
                Some(("max-age", value)) => fresh_for = Some(value.parse::<u64>().unwrap()),
                Some(("stale-while-revalidate", value)) => {
                    stale_for = Some(value.parse::<u64>().unwrap())
                }
                _ if directive.eq_ignore_ascii_case("public") => visibility = Some(true),
                _ if directive.eq_ignore_ascii_case("private") => visibility = Some(false),
                _ if directive.eq_ignore_ascii_case("no-cache") => revalidate = true,
                _ if directive.eq_ignore_ascii_case("no-store") => no_store = true,
                _ => {}
            }
        }
        assert_eq!(visibility, Some(is_public));
        assert!(revalidate);
        assert!(!no_store);
        assert!(fresh_for.is_none());
        assert!(stale_for.is_none());
    }
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
    assert!(output.read.as_ref().unwrap().cache_control.is_some());

    let mut with_csrf = get.clone();
    with_csrf.headers.push(Header {
        name: "X-Wamn-Csrf".to_string(),
        value: "token".to_string(),
    });
    let mut backend = FakeBackend::new(read.clone());
    let output = request(&mut backend, &with_csrf, b"");
    assert_eq!(output.status, 200);
    assert!(output.read.as_ref().unwrap().cache_control.is_none());

    let mut backend = FakeBackend::new(read.clone());
    backend.response = response(503, "");
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 503);
    assert!(output.read.is_none());

    let mut backend = FakeBackend::new(read);
    backend.auth = Err(AuthRejection {
        status: 401,
        code: "unauthorized".to_string(),
    });
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 401);
    assert!(output.read.is_none());

    let mut backend = FakeBackend::new(route());
    let output = request(&mut backend, &head(), br#"{"amount":1}"#);
    assert_eq!(output.status, 200);
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
    assert!(backend.invocations[0].if_none_match.is_some());

    let mut backend = FakeBackend::new(read);
    backend.etag = Some(TAG.to_string());
    backend.response = response(304, "");
    let output = request(&mut backend, &get, b"");
    assert_eq!(output.status, 304);
    assert!(output.read.as_ref().unwrap().etag.is_some());
    assert!(output.body.is_empty(), "a 304 has no body");

    let mut write = head();
    write.headers.push(Header {
        name: "if-none-match".to_string(),
        value: TAG.to_string(),
    });
    let mut backend = FakeBackend::new(route());
    let output = request(&mut backend, &write, br#"{"amount":1}"#);
    assert_eq!(output.status, 200);
    assert_eq!(
        backend.invocations[0].if_none_match, None,
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

fn response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        content_type: "application/json",
        body: body.as_bytes().to_vec(),
        read: None,
    }
}
