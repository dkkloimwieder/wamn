use super::{Delivery, FailureType, NodeCall, NodeOutcome, Outcome, Step, Verdict, WalkStatus};
use serde_json::json;

use super::wiring::Wiring;
use wamn_catalog::{WiringEdge, WiringNode, WiringTerminal as Terminal};

fn node(id: &str, terminal: Option<Terminal>) -> (String, WiringNode) {
    (
        id.to_owned(),
        WiringNode {
            interface_version: "0.1.0".into(),
            operation_dependency: None,
            component: "echo".to_string(),
            operation: "echo".to_string(),
            params: Default::default(),
            terminal,
        },
    )
}

fn edge_on(from: &str, port: &str, to: &str) -> WiringEdge {
    WiringEdge {
        from: from.to_string(),
        from_port: port.to_string(),
        to: to.to_string(),
        to_port: Some("input".to_string()),
    }
}

fn edge(from: &str, to: &str) -> WiringEdge {
    edge_on(from, "main", to)
}

fn wiring(entry: &str, nodes: Vec<(String, WiringNode)>, edges: Vec<WiringEdge>) -> Wiring {
    let document = wamn_catalog::WiringDocument::parse(&json!({
        "format-version": "0.1", "wiring-id": "test", "version": 1,
        "entry": entry, "nodes": nodes.into_iter().collect::<std::collections::BTreeMap<_, _>>(), "edges": edges,
    })).expect("catalog fixture admits");
    Wiring::compile(document, &std::collections::BTreeMap::new()).expect("graph indexes")
}

fn drive(
    w: &Wiring,
    caller_attached: bool,
    mut answer: impl FnMut(&NodeCall) -> NodeOutcome,
) -> Outcome {
    let mut walk = w.start(Delivery {
        payload: json!({}),
        caller_attached,
    });
    loop {
        match w.next(&mut walk, 0) {
            Step::Done(_) => return walk.into_outcome(),
            Step::Wait { .. } => panic!("no retry"),
            Step::Invoke(call) => w.apply(&mut walk, &call, answer(&call), 0).unwrap(),
        }
    }
}

fn failure(outcome: &Outcome) -> (&str, FailureType, Option<&str>) {
    let failure = outcome
        .failure
        .as_ref()
        .expect("a failed walk records its failure");
    (
        failure.node.as_str(),
        failure.type_,
        failure.detail.code.as_deref(),
    )
}

#[test]
fn a_respond_node_without_a_caller_fails_the_delivery_not_the_router() {
    let w = wiring("a", vec![node("a", Some(Terminal::Respond))], vec![]);

    let outcome = drive(&w, false, |_| NodeOutcome::ok(json!({"answer": 1})));

    assert_eq!(outcome.status, WalkStatus::Failed);
    assert_eq!(
        failure(&outcome),
        (
            "a",
            FailureType::RespondWithoutCaller,
            Some("respond-without-caller")
        )
    );
}

#[test]
fn a_second_terminal_fails_the_delivery_not_the_router() {
    let w = wiring(
        "fan",
        vec![
            node("fan", None),
            node("first", Some(Terminal::Respond)),
            node("second", Some(Terminal::Respond)),
        ],
        vec![edge("fan", "first"), edge("fan", "second")],
    );

    let outcome = drive(&w, true, |call| NodeOutcome::ok(json!({"at": call.node})));

    assert_eq!(outcome.status, WalkStatus::Failed);
    assert_eq!(
        failure(&outcome),
        ("second", FailureType::SecondVerdict, Some("second-verdict"))
    );
    assert_eq!(
        outcome.verdict,
        Some(Verdict::Respond {
            payload: json!({"at": "first"}),
            node_id: "first".to_string(),
        }),
        "the first verdict is the one this delivery recorded; the second is the defect"
    );
}
