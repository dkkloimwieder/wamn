use wamn_flow_invocation::{
    Admitted, BeginResult, Failure, FlowError, FlowInvocation, InvokeRequest, InvokeResult,
    Rejection, Response, TraceContext,
};

#[derive(Debug, Default)]
struct Stub {
    result: Option<InvokeResult>,
}

impl FlowInvocation for Stub {
    fn begin(&mut self, _request: InvokeRequest) -> BeginResult {
        BeginResult::Admitted(Admitted {
            run_id: "run-1".to_string(),
        })
    }

    fn wait(&mut self, _run_id: String, _timeout_ms: u32) -> Option<InvokeResult> {
        self.result.take()
    }
}

fn request() -> InvokeRequest {
    InvokeRequest {
        attachment_id: "orders-post".to_string(),
        expected_catalog_version: 7,
        expected_definition_hash: "sha256:def".to_string(),
        client_request_fingerprint: "sha256:req".to_string(),
        payload: r#"{"order":42}"#.to_string(),
        idempotency_key: Some("client-key".to_string()),
        principal: "customer:9".to_string(),
        deadline_override: Some(30_000),
        trace: Some(TraceContext {
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
            tracestate: None,
        }),
    }
}

fn flow_error(run_id: &str) -> FlowError {
    FlowError {
        code: "payment-declined".to_string(),
        message: None,
        run_id: run_id.to_string(),
        flow_id: "checkout".to_string(),
        flow_version: 3,
    }
}

#[test]
fn positive_begin_returns_the_durable_run_handle_before_wait() {
    let mut service = Stub::default();
    let BeginResult::Admitted(admitted) = service.begin(request()) else {
        panic!("stub admission must return the durable run handle");
    };

    assert_eq!(admitted.run_id, "run-1");
    assert_eq!(service.wait(admitted.run_id.clone(), 25), None);
}

#[test]
fn positive_wait_returns_each_stored_outcome_with_its_run_identity_and_status() {
    let cases = [
        InvokeResult::Responded(Response {
            run_id: "run-response".to_string(),
            body: r#"{"ok":true}"#.to_string(),
            status_hint: Some(202),
        }),
        InvokeResult::Failed(Failure {
            status: 400,
            error: flow_error("run-failed"),
        }),
    ];

    for expected in cases {
        let mut service = Stub {
            result: Some(expected.clone()),
        };
        assert_eq!(
            service.wait("requested-run".to_string(), u32::MAX),
            Some(expected)
        );
    }
}

#[test]
fn negative_pre_run_rejection_is_distinct_and_has_no_invented_run() {
    let result = BeginResult::Rejected(Rejection {
        status: 409,
        code: "idempotency-scope-changed".to_string(),
    });

    let BeginResult::Rejected(rejection) = result else {
        panic!("pre-run refusal must use the rejected arm");
    };
    assert_eq!(rejection.status, 409);
    assert_eq!(rejection.code, "idempotency-scope-changed");
}

#[test]
fn fault_rust_result_variants_are_exhaustive_and_unambiguous() {
    fn kind(result: InvokeResult) -> &'static str {
        match result {
            InvokeResult::Responded(_) => "responded",
            InvokeResult::Failed(_) => "failed",
        }
    }

    assert_eq!(
        kind(InvokeResult::Responded(Response {
            run_id: "run-1".to_string(),
            body: "{}".to_string(),
            status_hint: None,
        })),
        "responded"
    );
}

#[test]
fn fault_rust_begin_variants_are_exhaustive_and_unambiguous() {
    fn kind(result: BeginResult) -> &'static str {
        match result {
            BeginResult::Admitted(_) => "admitted",
            BeginResult::Rejected(_) => "rejected",
        }
    }

    assert_eq!(
        kind(BeginResult::Rejected(Rejection {
            status: 413,
            code: "payload-too-large".to_string(),
        })),
        "rejected"
    );
}
