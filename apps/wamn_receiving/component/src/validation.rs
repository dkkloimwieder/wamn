//! Validation at the typed exports, before an operation acquires SQL authority.
use crate::exports::wamn_receiving::supplier::create;
use crate::wamn::node::types::NodeContext;
use std::future::Future as _;
use std::task::{Context, Poll, Waker};

fn context() -> NodeContext {
    NodeContext {
        wiring_id: String::new(),
        wiring_version: 0,
        node_id: String::new(),
        delivery_id: String::new(),
        input_port: None,
        occurrence: 0,
        traceparent: None,
        tracestate: None,
        deadline_ms: None,
        config: "null".into(),
    }
}

#[test]
fn invalid_supplier_values_refuse_before_the_action_runs() {
    for (id, name) in [
        ("00000000-0000-4000-8000-000000000001", None),
        ("00000000-0000-4000-8000-000000000001", Some(None)),
        (
            "00000000-0000-4000-8000-000000000001",
            Some(Some(String::new())),
        ),
        (
            "00000000-0000-4000-8000-000000000001",
            Some(Some(" \t ".into())),
        ),
        ("invalid", Some(Some("supplier".into()))),
    ] {
        let input = vec![create::CreateItem {
            request_id: "create".into(),
            input: Ok(create::CreateRequest {
                id: id.into(),
                idempotency_key: "k".into(),
                name,
            }),
        }];
        let call = <crate::Component as create::Guest>::run(context(), input);
        let Poll::Ready(Ok(outcomes)) =
            std::pin::pin!(call).poll(&mut Context::from_waker(Waker::noop()))
        else {
            panic!("invalid input must finish without SQL");
        };
        assert!(
            matches!(&outcomes[0].outcome, Err(create::CreateError::InvalidInput(detail)) if detail.field == if id == "invalid" { "id" } else { "name" })
        );
    }
}

#[test]
fn typed_call_refuses_missing_correlation_and_envelope_bounds_before_database_work() {
    let item = |request_id: &str| create::CreateItem {
        request_id: request_id.into(),
        input: Err(create::InvalidInputDetail {
            field: "name".into(),
        }),
    };
    for input in [
        Vec::new(),
        vec![item("")],
        (0..101).map(|_| item("create")).collect(),
    ] {
        let call = <crate::Component as create::Guest>::run(context(), input);
        let Poll::Ready(Err(crate::wamn::node::types::NodeError::InvalidInput(_))) =
            std::pin::pin!(call).poll(&mut Context::from_waker(Waker::noop()))
        else {
            panic!("invalid envelope must refuse before SQL");
        };
    }
}
