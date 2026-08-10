use std::collections::VecDeque;

use serde_json::{Value, json};
use wamn_flow_invocation::{
    Admitted, BeginResult, Failure, FlowError, InvokeRequest, InvokeResult, Rejection, Response,
};

use flow_http::{
    AdapterLimits, AdapterOutcome, Backend, BodyReadError, BodyReader, Cardinality, ClientLiveness,
    Header, Mapping, MappingSource, ProviderError, RequestHead, RouteDefinition, handle_request,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Routes,
    Begin,
    Wait,
}

struct FakeBackend {
    routes: Vec<RouteDefinition>,
    auth: Result<String, Rejection>,
    begin: BeginResult,
    waits: VecDeque<Option<InvokeResult>>,
    fault: Fault,
    auth_policies: Vec<String>,
    begins: Vec<InvokeRequest>,
    wait_timeouts: Vec<u32>,
}

impl FakeBackend {
    fn new(route: RouteDefinition) -> Self {
        Self {
            routes: vec![route],
            auth: Ok("principal:alice".to_string()),
            begin: BeginResult::Admitted(Admitted {
                run_id: "run-1".to_string(),
            }),
            waits: VecDeque::from([Some(responded(201, r#"{"ok":true}"#))]),
            fault: Fault::None,
            auth_policies: Vec::new(),
            begins: Vec::new(),
            wait_timeouts: Vec::new(),
        }
    }
}

impl Backend for FakeBackend {
    fn routes(
        &mut self,
        _method: &str,
        _authority: &str,
    ) -> Result<Vec<RouteDefinition>, ProviderError> {
        (self.fault != Fault::Routes)
            .then(|| self.routes.clone())
            .ok_or(ProviderError)
    }

    fn authenticate(&mut self, policy: &str, _headers: &[Header]) -> Result<String, Rejection> {
        self.auth_policies.push(policy.to_string());
        self.auth.clone()
    }

    fn begin(&mut self, request: InvokeRequest) -> Result<BeginResult, ProviderError> {
        self.begins.push(request);
        (self.fault != Fault::Begin)
            .then(|| self.begin.clone())
            .ok_or(ProviderError)
    }

    fn wait(
        &mut self,
        _run_id: &str,
        timeout_ms: u32,
    ) -> Result<Option<InvokeResult>, ProviderError> {
        self.wait_timeouts.push(timeout_ms);
        if self.fault == Fault::Wait {
            return Err(ProviderError);
        }
        Ok(self.waits.pop_front().unwrap_or(None))
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

struct Liveness {
    states: VecDeque<bool>,
}

impl Liveness {
    fn connected() -> Self {
        Self {
            states: VecDeque::from([true]),
        }
    }
}

impl ClientLiveness for Liveness {
    fn connected(&mut self) -> bool {
        self.states.pop_front().unwrap_or(true)
    }
}

fn route() -> RouteDefinition {
    RouteDefinition {
        attachment_id: "attachment-a".to_string(),
        catalog_version: 7,
        definition_hash: "definition-a".to_string(),
        host: "api.example.test".to_string(),
        path: "/receipts/{receipt}".to_string(),
        method: "POST".to_string(),
        enabled: true,
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
        idempotency_required: true,
        body_limit: 1024,
        mapped_limit: 1024,
        deadline_override: Some(5_000),
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
                name: "idempotency-key".to_string(),
                value: "key-1".to_string(),
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
        wait_slice_ms: 25,
        max_waits: 3,
    }
}

fn responded(status: u16, body: &str) -> InvokeResult {
    InvokeResult::Responded(Response {
        run_id: "run-1".to_string(),
        body: body.to_string(),
        status_hint: Some(status),
    })
}

fn failure(status: u16, code: &str) -> Failure {
    Failure {
        status,
        error: FlowError {
            code: code.to_string(),
            message: Some("authored detail".to_string()),
            run_id: "run-1".to_string(),
            flow_id: "flow-a".to_string(),
            flow_version: 3,
        },
    }
}

fn response(outcome: AdapterOutcome) -> flow_http::HttpResponse {
    match outcome {
        AdapterOutcome::Response(response) => response,
        AdapterOutcome::Disconnected { run_id } => panic!("unexpected disconnect: {run_id}"),
    }
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
fn partial_body_selected_policy_mapping_and_begin_identity() {
    let mut wildcard = route();
    wildcard.attachment_id = "wildcard".to_string();
    wildcard.host = "*".to_string();
    wildcard.auth_policy = "wrong-policy".to_string();
    let mut backend = FakeBackend::new(route());
    backend.routes.insert(0, wildcard);
    let mut body = Chunks::json(&[br#"{"am"#, br#"ount":12.50}"#]);
    let mut live = Liveness::connected();

    let output = response(handle_request(
        &mut backend,
        &mut body,
        &mut live,
        &head(),
        limits(),
    ));

    assert_eq!(output.status, 201);
    assert_eq!(output.body, br#"{"ok":true}"#);
    assert_eq!(backend.auth_policies, ["jwt:receipts"]);
    assert_eq!(backend.wait_timeouts, [25]);
    assert_eq!(body.reads, 3);
    let request = &backend.begins[0];
    assert_eq!(request.attachment_id, "attachment-a");
    assert_eq!(request.expected_catalog_version, 7);
    assert_eq!(request.expected_definition_hash, "definition-a");
    assert_eq!(request.idempotency_key.as_deref(), Some("key-1"));
    assert_eq!(request.principal, "principal:alice");
    assert_eq!(request.deadline_override, Some(5_000));
    assert_eq!(
        request
            .trace
            .as_ref()
            .map(|trace| trace.traceparent.as_str()),
        Some("00-abc-def-01")
    );
    assert_eq!(request.client_request_fingerprint.len(), 64);
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
fn route_precedence_is_static_then_param_then_catch_all_and_disabled_is_retained() {
    let mut static_route = route();
    static_route.path = "/receipts/special".to_string();
    static_route.attachment_id = "static".to_string();
    static_route.enabled = false;
    static_route.mappings.clear();
    static_route.input_schema = json!({});
    static_route.idempotency_required = false;
    let mut param = static_route.clone();
    param.path = "/receipts/{id}".to_string();
    param.attachment_id = "param".to_string();
    let mut catch_all = static_route.clone();
    catch_all.path = "/receipts/{*rest}".to_string();
    catch_all.attachment_id = "catch".to_string();
    let mut backend = FakeBackend::new(catch_all);
    backend.routes.extend([param, static_route]);
    let mut request = head();
    request.target = "/receipts/special".to_string();
    request.headers.clear();
    let mut body = Chunks::json(&[]);
    let mut live = Liveness::connected();

    let output = response(handle_request(
        &mut backend,
        &mut body,
        &mut live,
        &request,
        limits(),
    ));

    assert_eq!(output.status, 201);
    assert_eq!(backend.begins[0].attachment_id, "static");
}

#[test]
fn encoded_slash_stays_in_one_path_segment_and_maps_decoded() {
    let mut backend = FakeBackend::new(route());
    let mut request = head();
    request.target = "/receipts/r%2fpart?tag=a".to_string();
    let mut body = Chunks::json(&[br#"{"amount":1}"#]);
    let mut live = Liveness::connected();

    let output = response(handle_request(
        &mut backend,
        &mut body,
        &mut live,
        &request,
        limits(),
    ));

    assert_eq!(output.status, 201);
    let payload =
        serde_json::from_str::<Value>(&backend.begins[0].payload).expect("mapped payload");
    assert_eq!(payload["receipt"], "r/part");
}

#[test]
fn every_typed_rejection_is_adapted_without_a_run() {
    for (status, code) in [
        (400, "invalid-input"),
        (401, "unauthenticated"),
        (403, "forbidden"),
        (404, "attachment-not-found"),
        (409, "idempotency-key-reused"),
        (413, "payload-too-large"),
        (503, "admission-retry"),
        (409, "idempotency-scope-changed"),
        (410, "outcome-expired"),
    ] {
        let mut backend = FakeBackend::new(route());
        backend.begin = BeginResult::Rejected(Rejection {
            status,
            code: code.to_string(),
        });
        let mut body = Chunks::json(&[br#"{"amount":1}"#]);
        let mut live = Liveness::connected();

        let output = response(handle_request(
            &mut backend,
            &mut body,
            &mut live,
            &head(),
            limits(),
        ));

        assert_eq!(
            (output.status, error_code(&output.body)),
            (status, code.to_string())
        );
        assert!(backend.wait_timeouts.is_empty());
    }
}

#[test]
fn all_stored_outcomes_are_adapted_exactly() {
    let cases = [
        (responded(202, r#"{"queued":true}"#), 202, None),
        (
            InvokeResult::Failed(failure(400, "authored-fail")),
            400,
            Some("authored-fail"),
        ),
    ];
    for (result, status, code) in cases {
        let mut backend = FakeBackend::new(route());
        backend.waits = VecDeque::from([Some(result)]);
        let mut body = Chunks::json(&[br#"{"amount":1}"#]);
        let mut live = Liveness::connected();

        let output = response(handle_request(
            &mut backend,
            &mut body,
            &mut live,
            &head(),
            limits(),
        ));

        assert_eq!(output.status, status);
        if let Some(code) = code {
            assert_eq!(error_code(&output.body), code);
        }
    }
}

#[test]
fn malformed_oversize_mapping_schema_and_auth_refusals_never_begin() {
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
            Some(Rejection {
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
        let mut live = Liveness::connected();

        let output = response(handle_request(
            &mut backend,
            &mut body,
            &mut live,
            &head(),
            limits(),
        ));

        assert!(matches!(output.status, 400 | 403 | 413), "{name}");
        assert!(backend.begins.is_empty(), "{name} unexpectedly admitted");
        if name == "auth" {
            assert_eq!(body.reads, 0, "auth must precede body reads");
        }
    }

    let mut selected = route();
    selected.mappings[2].cardinality = Cardinality::One;
    let mut backend = FakeBackend::new(selected);
    let mut body = Chunks::json(&[br#"{"amount":1}"#]);
    let mut live = Liveness::connected();
    let output = response(handle_request(
        &mut backend,
        &mut body,
        &mut live,
        &head(),
        limits(),
    ));
    assert_eq!(error_code(&output.body), "mapping-cardinality");
    assert!(backend.begins.is_empty());
}

#[test]
fn invalid_percent_encoding_and_missing_idempotency_key_never_begin() {
    for mutate in [
        |head: &mut RequestHead| head.target = "/receipts/r-1?tag=%GG".to_string(),
        |head: &mut RequestHead| {
            head.headers
                .retain(|header| header.name != "idempotency-key");
        },
    ] {
        let mut request = head();
        mutate(&mut request);
        let mut backend = FakeBackend::new(route());
        let mut body = Chunks::json(&[br#"{"amount":1}"#]);
        let mut live = Liveness::connected();

        let output = response(handle_request(
            &mut backend,
            &mut body,
            &mut live,
            &request,
            limits(),
        ));

        assert_eq!(output.status, 400);
        assert!(backend.begins.is_empty());
    }
}

#[test]
fn mapped_payload_ceiling_is_enforced_before_begin() {
    let mut selected = route();
    selected.mapped_limit = 8;
    let mut backend = FakeBackend::new(selected);
    let mut body = Chunks::json(&[br#"{"amount":1}"#]);
    let mut live = Liveness::connected();

    let output = response(handle_request(
        &mut backend,
        &mut body,
        &mut live,
        &head(),
        limits(),
    ));

    assert_eq!(output.status, 413);
    assert_eq!(error_code(&output.body), "mapped-payload-too-large");
    assert!(backend.begins.is_empty());
}

#[test]
fn wait_is_finite_and_disconnect_detaches_without_mutating_the_run() {
    let mut backend = FakeBackend::new(route());
    backend.waits = VecDeque::from([None, None, None]);
    let mut body = Chunks::json(&[br#"{"amount":1}"#]);
    let mut live = Liveness::connected();
    let output = response(handle_request(
        &mut backend,
        &mut body,
        &mut live,
        &head(),
        limits(),
    ));
    assert_eq!(output.status, 504);
    assert_eq!(backend.wait_timeouts, [25, 25, 25]);

    let mut backend = FakeBackend::new(route());
    backend.waits = VecDeque::from([None]);
    let mut body = Chunks::json(&[br#"{"amount":1}"#]);
    let mut live = Liveness {
        states: VecDeque::from([true, false, false]),
    };
    let output = handle_request(&mut backend, &mut body, &mut live, &head(), limits());
    assert_eq!(
        output,
        AdapterOutcome::Disconnected {
            run_id: "run-1".to_string()
        }
    );
    assert_eq!(backend.wait_timeouts, [25]);
}

#[test]
fn routing_body_and_invocation_provider_faults_are_bounded() {
    for (fault, expected) in [
        (Fault::Routes, "routing-provider-failed"),
        (Fault::Begin, "invocation-provider-failed"),
        (Fault::Wait, "wait-provider-failed"),
    ] {
        let mut backend = FakeBackend::new(route());
        backend.fault = fault;
        let mut body = Chunks::json(&[br#"{"amount":1}"#]);
        let mut live = Liveness::connected();
        let output = response(handle_request(
            &mut backend,
            &mut body,
            &mut live,
            &head(),
            limits(),
        ));
        assert_eq!(output.status, 503);
        assert_eq!(error_code(&output.body), expected);
    }

    let mut backend = FakeBackend::new(route());
    let mut body = Chunks {
        chunks: VecDeque::from([Err(BodyReadError)]),
        reads: 0,
    };
    let mut live = Liveness::connected();
    let output = response(handle_request(
        &mut backend,
        &mut body,
        &mut live,
        &head(),
        limits(),
    ));
    assert_eq!(error_code(&output.body), "body-read-failed");
    assert!(backend.begins.is_empty());
}
