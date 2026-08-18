//! The per-delivery context document: whole-document replacement, no merge, and
//! atomic rejection of a non-object replacement.
//!
//! Ported from `crates/execution/flow-engine/tests/context.rs`. The engine's
//! fixtures carried a leading `{"id":"in","type":"event"}` node that the helper
//! acknowledged before the real walk began; the router has no reserved entry
//! type, so the first real node IS the entry and the acknowledgement step is
//! gone. Every assert is otherwise unchanged.

use serde_json::{Value, json};
use wamn_router::{
    ApplyErrorKind, Delivery, NodeCall, NodeOutcome, Step, Walk, Wiring, WiringEdge, WiringNode,
};

fn node(id: &str, component: &str) -> WiringNode {
    WiringNode {
        id: id.to_string(),
        component: component.to_string(),
        operation: "run".to_string(),
        config: Value::Null,
        connection: None,
    }
}

fn edge(from: &str, port: &str, to: &str) -> WiringEdge {
    WiringEdge {
        from: from.to_string(),
        from_port: port.to_string(),
        to: to.to_string(),
        ordinal: None,
    }
}

fn started(wiring: &Wiring, payload: Value) -> Walk {
    wiring.start(Delivery {
        id: "run".to_string(),
        payload,
    })
}

fn invoke(wiring: &Wiring, walk: &mut Walk) -> NodeCall {
    let Step::Invoke(call) = wiring.next(walk, 0) else {
        panic!("expected an invocation");
    };
    call
}

#[test]
fn successful_context_writes_replace_and_effects_observe_the_snapshot() {
    let w = Wiring::compile(
        "first",
        vec![
            node("first", "pure"),
            node("second", "pure"),
            node("effect", "effect"),
        ],
        vec![
            edge("first", "main", "second"),
            edge("second", "main", "effect"),
        ],
    )
    .unwrap();
    let mut walk = started(&w, json!({"tick": 1}));

    let first = invoke(&w, &mut walk);
    assert_eq!(first.context, json!({}));
    w.apply(
        &mut walk,
        &first,
        NodeOutcome::ok_with_context(json!({"output": 1}), "main", json!({"dropped": true})),
        0,
    )
    .unwrap();

    let second = invoke(&w, &mut walk);
    assert_eq!(second.context, json!({"dropped": true}));
    w.apply(
        &mut walk,
        &second,
        NodeOutcome::ok_with_context(json!({"output": 2}), "main", json!({"kept": true})),
        0,
    )
    .unwrap();

    let effect = invoke(&w, &mut walk);
    assert_eq!(effect.context, json!({"kept": true}));
    assert!(
        effect.context.get("dropped").is_none(),
        "writes must not merge"
    );
}

#[test]
fn context_is_byte_identical_through_a_loop() {
    let w = Wiring::compile(
        "loop-a",
        vec![
            node("loop-a", "pure"),
            node("loop-b", "pure"),
            node("effect", "effect"),
        ],
        vec![
            edge("loop-a", "again", "loop-b"),
            edge("loop-b", "main", "loop-a"),
            edge("loop-a", "main", "effect"),
        ],
    )
    .unwrap();
    let mut walk = started(&w, json!({"tick": 1}));
    for (port, payload, context) in [
        (
            "again",
            json!({"lap": 1}),
            json!({"cutoff": "2026-01-01T00:00:00Z", "lap": 1}),
        ),
        (
            "main",
            json!({"lap": 1}),
            json!({"cutoff": "2026-01-01T00:00:00Z", "lap": 1}),
        ),
        (
            "main",
            json!({"lap": 2}),
            json!({"cutoff": "2026-01-01T00:00:00Z", "lap": 2}),
        ),
    ] {
        let call = invoke(&w, &mut walk);
        w.apply(
            &mut walk,
            &call,
            NodeOutcome::ok_with_context(payload, port, context),
            0,
        )
        .unwrap();
    }
    let effect = invoke(&w, &mut walk);
    let expected = json!({"cutoff": "2026-01-01T00:00:00Z", "lap": 2});
    assert_eq!(effect.context, expected);
    assert_eq!(
        serde_json::to_vec(&effect.context).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
}

#[test]
fn invalid_replacement_is_atomic() {
    let w = Wiring::compile("pure", vec![node("pure", "pure")], vec![]).unwrap();
    let mut walk = started(&w, Value::Null);
    let call = invoke(&w, &mut walk);
    let error = w
        .apply(
            &mut walk,
            &call,
            NodeOutcome::ok_with_context(json!({"output": 1}), "main", json!(["merge"])),
            0,
        )
        .unwrap_err();
    assert!(matches!(error.kind(), ApplyErrorKind::InvalidContext(_)));
    assert_eq!(walk.context(), &json!({}));
    assert_eq!(invoke(&w, &mut walk), call);
}
