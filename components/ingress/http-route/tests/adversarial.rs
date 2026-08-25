use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use flow_http::{
    AdapterLimits, AuthRejection, Backend, BodyReadError, BodyReader, Cardinality, DeliveryError,
    DeliveryFailure, DeliveryFailureKind, DeliveryOutcome, DeliveryRequest, Emission, Header,
    Mapping, MappingSource, ProviderError, RequestHead, RouteDefinition, handle_request,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Routes,
    Permit,
    Deliver,
}

struct TestPermit(Arc<AtomicUsize>);

impl Drop for TestPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct FakeBackend {
    routes: Vec<RouteDefinition>,
    auth: Result<String, AuthRejection>,
    delivery: Result<DeliveryOutcome, DeliveryError>,
    fault: Fault,
    auth_policies: Vec<String>,
    deliveries: Vec<DeliveryRequest>,
    next_delivery_id: u64,
    permit_available: bool,
    permits: Arc<AtomicUsize>,
    acquired_routes: Vec<String>,
}

impl FakeBackend {
    fn new(route: RouteDefinition) -> Self {
        Self {
            routes: vec![route],
            auth: Ok("principal:alice".to_string()),
            delivery: Ok(DeliveryOutcome::Respond(r#"{"ok":true}"#.to_string())),
            fault: Fault::None,
            auth_policies: Vec::new(),
            deliveries: Vec::new(),
            next_delivery_id: 1,
            permit_available: true,
            permits: Arc::new(AtomicUsize::new(0)),
            acquired_routes: Vec::new(),
        }
    }
}

impl Backend for FakeBackend {
    type RoutePermit = TestPermit;

    fn routes(
        &mut self,
        _method: &str,
        _authority: &str,
    ) -> Result<Vec<RouteDefinition>, ProviderError> {
        (self.fault != Fault::Routes)
            .then(|| self.routes.clone())
            .ok_or(ProviderError)
    }

    fn authenticate(&mut self, policy: &str, _headers: &[Header]) -> Result<String, AuthRejection> {
        self.auth_policies.push(policy.to_string());
        self.auth.clone()
    }

    fn try_acquire_route(
        &mut self,
        attachment_id: &str,
    ) -> Result<Option<Self::RoutePermit>, ProviderError> {
        self.acquired_routes.push(attachment_id.to_string());
        if self.fault == Fault::Permit {
            return Err(ProviderError);
        }
        if !self.permit_available {
            return Ok(None);
        }
        self.permits.fetch_add(1, Ordering::SeqCst);
        Ok(Some(TestPermit(Arc::clone(&self.permits))))
    }

    fn new_delivery_id(&mut self) -> String {
        let id = self.next_delivery_id;
        self.next_delivery_id = self
            .next_delivery_id
            .checked_add(1)
            .expect("fixture delivery id space exhausted");
        format!("{id:032x}")
    }

    fn deliver(&mut self, request: DeliveryRequest) -> Result<DeliveryOutcome, DeliveryError> {
        self.deliveries.push(request);
        if self.fault == Fault::Deliver {
            return Err(DeliveryError::ExecutionFailed);
        }
        self.delivery.clone()
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
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, BodyReadError> {
        self.reads += 1;
        self.chunks.pop_front().unwrap_or(Ok(None))
    }
}

fn route() -> RouteDefinition {
    RouteDefinition {
        attachment_id: "attachment-a".to_string(),
        host: "api.example.test".to_string(),
        path: "/receipts/{receipt}".to_string(),
        method: "POST".to_string(),
        auth_policy: "jwt:receipts".to_string(),
        mappings: vec![
            Mapping {
                from: MappingSource::Body,
                name: "amount".to_string(),
                to: "/amount".to_string(),
                optional: false,
                cardinality: Cardinality::One,
            },
            Mapping {
                from: MappingSource::Path,
                name: "receipt".to_string(),
                to: "/receipt".to_string(),
                optional: false,
                cardinality: Cardinality::One,
            },
            Mapping {
                from: MappingSource::Query,
                name: "tag".to_string(),
                to: "/tags".to_string(),
                optional: false,
                cardinality: Cardinality::Many,
            },
            Mapping {
                from: MappingSource::Header,
                name: "x-store".to_string(),
                to: "/store".to_string(),
                optional: false,
                cardinality: Cardinality::One,
            },
        ],
        input_schema: json!({
            "type": "object",
            "required": ["amount", "receipt", "tags", "store"],
            "properties": {
                "amount": {"type": "number"},
                "receipt": {"type": "string"},
                "tags": {"type": "array"},
                "store": {"type": "string"}
            },
            "additionalProperties": false
        }),
        body_limit: 1024,
        mapped_limit: 1024,
    }
}

fn head() -> RequestHead {
    RequestHead {
        method: "post".to_string(),
        authority: "API.EXAMPLE.TEST".to_string(),
        target: "/receipts/r-1/?tag=a&tag=b".to_string(),
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
    AdapterLimits {
        body_bytes: 1024,
        mapped_bytes: 1024,
    }
}

fn request(backend: &mut FakeBackend, head: &RequestHead, bytes: &[u8]) -> flow_http::HttpResponse {
    let mut body = Chunks::json(&[bytes]);
    handle_request(backend, &mut body, head, limits())
}

fn error_code(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .expect("JSON error")
        .pointer("/error/code")
        .and_then(Value::as_str)
        .expect("error code")
        .to_string()
}

#[test]
fn partial_body_selected_policy_mapping_and_attachment_delivery() {
    let mut wildcard = route();
    wildcard.attachment_id = "wildcard".to_string();
    wildcard.host = "*".to_string();
    wildcard.auth_policy = "wrong-policy".to_string();
    let mut backend = FakeBackend::new(route());
    backend.routes.insert(0, wildcard);
    let mut body = Chunks::json(&[br#"{"am"#, br#"ount":12.50}"#]);

    let output = handle_request(&mut backend, &mut body, &head(), limits());

    assert_eq!(output.status, 200);
    assert_eq!(output.body, br#"{"ok":true}"#);
    assert_eq!(backend.auth_policies, ["jwt:receipts"]);
    assert_eq!(body.reads, 3);
    let request = &backend.deliveries[0];
    assert_eq!(request.attachment_id, "attachment-a");
    assert_eq!(request.delivery_id.len(), 32);
    assert_eq!(
        request
            .caller
            .as_ref()
            .and_then(|caller| caller.user_id.as_deref()),
        Some("principal:alice")
    );
    assert_eq!(
        request
            .caller
            .as_ref()
            .and_then(|caller| caller.role.as_deref()),
        None
    );
    assert_eq!(
        request
            .trace
            .as_ref()
            .map(|trace| trace.traceparent.as_str()),
        Some("00-abc-def-01")
    );
    assert_eq!(
        serde_json::from_str::<Value>(&request.payload).expect("mapped payload"),
        json!({
            "amount": 12.50,
            "receipt": "r-1",
            "tags": ["a", "b"],
            "store": "nyc"
        })
    );
}

#[test]
fn identical_requests_receive_distinct_delivery_ids() {
    let mut backend = FakeBackend::new(route());

    for _ in 0..2 {
        let output = request(&mut backend, &head(), br#"{"amount":1}"#);
        assert_eq!(output.status, 200);
    }

    assert_ne!(
        backend.deliveries[0].delivery_id, backend.deliveries[1].delivery_id,
        "delivery identity is per request, not a deterministic payload fingerprint"
    );
}

#[test]
fn saturated_selected_route_sheds_immediately_without_delivery() {
    let mut backend = FakeBackend::new(route());
    backend.permit_available = false;

    let output = request(&mut backend, &head(), br#"{"amount":1}"#);

    assert_eq!(output.status, 429);
    assert_eq!(error_code(&output.body), "route-capacity-exhausted");
    assert_eq!(backend.acquired_routes, ["attachment-a"]);
    assert!(backend.deliveries.is_empty());
    assert_eq!(
        backend.next_delivery_id, 1,
        "shed work mints no delivery id"
    );
}

#[test]
fn route_permit_is_released_after_a_typed_delivery_error() {
    let mut backend = FakeBackend::new(route());
    backend.delivery = Err(DeliveryError::ExecutionFailed);

    let output = request(&mut backend, &head(), br#"{"amount":1}"#);

    assert_eq!(output.status, 503);
    assert_eq!(backend.permits.load(Ordering::SeqCst), 0);
}

#[test]
fn route_precedence_is_static_then_parameter_then_catch_all() {
    let mut static_route = route();
    static_route.path = "/receipts/special".to_string();
    static_route.attachment_id = "static".to_string();
    static_route.mappings.clear();
    static_route.input_schema = json!({});
    let mut parameter = static_route.clone();
    parameter.path = "/receipts/{id}".to_string();
    parameter.attachment_id = "parameter".to_string();
    let mut catch_all = static_route.clone();
    catch_all.path = "/receipts/{*rest}".to_string();
    catch_all.attachment_id = "catch".to_string();
    let mut backend = FakeBackend::new(catch_all);
    backend.routes.extend([parameter, static_route]);
    let mut request_head = head();
    request_head.target = "/receipts/special".to_string();
    request_head.headers.clear();

    let output = request(&mut backend, &request_head, b"");

    assert_eq!(output.status, 200);
    assert_eq!(backend.deliveries[0].attachment_id, "static");
}

#[test]
fn encoded_slash_stays_in_one_path_segment_and_maps_decoded() {
    let mut backend = FakeBackend::new(route());
    let mut request_head = head();
    request_head.target = "/receipts/r%2fpart?tag=a".to_string();

    let output = request(&mut backend, &request_head, br#"{"amount":1}"#);

    assert_eq!(output.status, 200);
    let payload =
        serde_json::from_str::<Value>(&backend.deliveries[0].payload).expect("mapped payload");
    assert_eq!(payload["receipt"], "r/part");
}

#[test]
fn malformed_oversize_mapping_schema_and_auth_refusals_never_deliver() {
    let cases = [
        ("malformed", br#"{"amount":"#.as_slice(), 1024, None),
        ("oversize", br#"{"amount":123}"#.as_slice(), 4, None),
        (
            "schema",
            br#"{"amount":"not-a-number"}"#.as_slice(),
            1024,
            None,
        ),
        (
            "auth",
            br#"{"amount":1}"#.as_slice(),
            1024,
            Some(AuthRejection {
                status: 403,
                code: "forbidden".to_string(),
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

        let output = handle_request(&mut backend, &mut body, &head(), limits());

        assert!(matches!(output.status, 400 | 403 | 413), "{name}");
        assert!(
            backend.deliveries.is_empty(),
            "{name} unexpectedly delivered"
        );
        if name == "auth" {
            assert_eq!(body.reads, 0, "auth must precede body reads");
        }
    }

    let mut selected = route();
    selected.mappings[2].cardinality = Cardinality::One;
    let mut backend = FakeBackend::new(selected);
    let output = request(&mut backend, &head(), br#"{"amount":1}"#);
    assert_eq!(error_code(&output.body), "mapping-cardinality");
    assert!(backend.deliveries.is_empty());
}

#[test]
fn invalid_percent_encoding_and_payload_ceiling_refuse_before_delivery() {
    let mut invalid = head();
    invalid.target = "/receipts/r-1?tag=%GG".to_string();
    let mut backend = FakeBackend::new(route());
    let output = request(&mut backend, &invalid, br#"{"amount":1}"#);
    assert_eq!(output.status, 400);
    assert!(backend.deliveries.is_empty());

    let mut selected = route();
    selected.mapped_limit = 8;
    let mut backend = FakeBackend::new(selected);
    let output = request(&mut backend, &head(), br#"{"amount":1}"#);
    assert_eq!(output.status, 413);
    assert_eq!(error_code(&output.body), "mapped-payload-too-large");
    assert!(backend.deliveries.is_empty());
}

#[test]
fn every_router_outcome_is_mapped_exhaustively() {
    let cases = [
        (
            DeliveryOutcome::Respond(r#"{"accepted":true}"#.to_string()),
            200,
            None,
        ),
        (
            DeliveryOutcome::Failed(DeliveryFailure {
                kind: DeliveryFailureKind::InvalidInput,
                code: Some("authored-invalid".to_string()),
                message: "bad input".to_string(),
            }),
            400,
            Some("authored-invalid"),
        ),
        (
            DeliveryOutcome::Failed(DeliveryFailure {
                kind: DeliveryFailureKind::UnreleasedCaller,
                code: None,
                message: "no response terminal".to_string(),
            }),
            500,
            Some("unreleased-caller"),
        ),
        (
            DeliveryOutcome::Emit(Emission {
                event: "{}".to_string(),
                dedup_id: "d1".to_string(),
            }),
            500,
            Some("http-route-emitted"),
        ),
        (DeliveryOutcome::Discard, 500, Some("http-route-discarded")),
        (DeliveryOutcome::Cancelled, 503, Some("execution-cancelled")),
    ];

    for (outcome, status, code) in cases {
        let mut backend = FakeBackend::new(route());
        backend.delivery = Ok(outcome);
        let output = request(&mut backend, &head(), br#"{"amount":1}"#);
        assert_eq!(output.status, status);
        if let Some(code) = code {
            assert_eq!(error_code(&output.body), code);
        }
        assert_eq!(backend.deliveries.len(), 1);
    }
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
        (
            DeliveryError::WiringNotPreloaded,
            503,
            "wiring-not-preloaded",
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
    let output = handle_request(&mut backend, &mut body, &head(), limits());
    assert_eq!(error_code(&output.body), "body-read-failed");
    assert!(backend.deliveries.is_empty());
}
