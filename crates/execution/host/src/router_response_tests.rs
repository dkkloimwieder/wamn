use std::io;

use serde_json::{Value, json};
use wamn_catalog::WiringResponse;
use wamn_execution_contract::EffectOutcome;
use wamn_router::{ErrorDetail, Failure, FailureKind, NodeError, NodeOutcome, Verdict, WalkStatus};

use super::{InterruptedResponse, PreparedResponse, PreparedSchema, ResponseState};

fn committed_schema() -> Value {
    json!({
        "type": "array",
        "minItems": 1,
        "items": {
            "type": "object",
            "required": ["request_id", "value"],
            "additionalProperties": false,
            "properties": {
                "request_id": {"type": "string"},
                "value": {
                    "type": "object",
                    "required": ["movement_id", "revision"],
                    "additionalProperties": false,
                    "properties": {
                        "movement_id": {"type": "string"},
                        "revision": {"type": "integer"}
                    }
                }
            }
        }
    })
}

fn normal_schema() -> Value {
    let mut schema = committed_schema();
    let value = &mut schema["items"]["properties"]["value"];
    value["required"] = json!(["movement_id", "revision", "zpl", "stored"]);
    value["properties"]["zpl"] = json!({"type": "string"});
    value["properties"]["stored"] = json!({
        "type": "object",
        "required": ["container", "key"],
        "additionalProperties": false,
        "properties": {"container": {"type": "string"}, "key": {"type": "string"}}
    });
    schema
}

fn contract() -> PreparedResponse {
    let schema = normal_schema();
    PreparedResponse {
        declaration: WiringResponse {
            node: "store".to_owned(),
            schema: schema.clone(),
            committed_result: Some("move".to_owned()),
        },
        normal: PreparedSchema::new(schema).unwrap(),
        committed: Some(PreparedSchema::new(committed_schema()).unwrap()),
    }
}

fn committed() -> Value {
    json!([{"request_id": "request-1", "value": {"movement_id": "movement-1", "revision": 2}}])
}

fn stored() -> Value {
    let mut value = committed();
    value[0]["value"]["zpl"] = json!("^XA^XZ");
    value[0]["value"]["stored"] = json!({"container": "labels", "key": "movement-1"});
    value
}

fn failed() -> NodeOutcome {
    NodeOutcome::Error(NodeError::Terminal(ErrorDetail::coded(
        "write_failed",
        "label storage failed",
    )))
}

fn failure(node: &str, kind: FailureKind) -> Failure {
    Failure {
        node: node.to_owned(),
        kind,
        detail: ErrorDetail::coded("write_failed", "label storage failed"),
    }
}

#[test]
fn selected_committed_result_survives_arbitrary_success_and_label_enrichment() {
    let contract = contract();
    let mut state = ResponseState::new(Some(&contract), true);
    assert!(state.effect_evidence().is_none());
    state
        .observe("unselected", &NodeOutcome::ok(committed()), None)
        .unwrap();
    assert!(
        state
            .evidence(
                WalkStatus::Failed,
                Some(&failure("store", FailureKind::Terminal)),
                None
            )
            .is_none()
    );

    let original = committed();
    state
        .observe("move", &NodeOutcome::ok(original.clone()), None)
        .unwrap();
    assert!(state.effect_evidence().is_some());
    let mut other = original.clone();
    other[0]["value"]["movement_id"] = json!("unselected-movement");
    state
        .observe("unselected", &NodeOutcome::ok(other), None)
        .unwrap();
    state
        .observe("label", &NodeOutcome::ok(stored()), None)
        .unwrap();
    state.observe("store", &failed(), None).unwrap();
    let evidence = state
        .evidence(
            WalkStatus::Failed,
            Some(&failure("store", FailureKind::Terminal)),
            None,
        )
        .unwrap();
    assert_eq!(evidence.committed_result, original);
    assert_eq!(evidence.effect_outcome, None);
}

#[test]
fn malformed_error_and_mixed_envelopes_cannot_prove_commitment() {
    let contract = contract();
    let error_item = json!({"request_id": "request-1", "error": {"code": "stale_revision"}});
    for payload in [
        Value::Null,
        json!({"movement_id": "movement-1"}),
        json!([]),
        json!([error_item.clone()]),
        json!([committed()[0].clone(), error_item]),
        json!([{"request_id": "request-1", "value": {"movement_id": "movement-1", "revision": "2"}}]),
    ] {
        let mut state = ResponseState::new(Some(&contract), true);
        state
            .observe("move", &NodeOutcome::ok(payload.clone()), None)
            .unwrap();
        state.observe("store", &failed(), None).unwrap();
        assert!(
            state
                .evidence(
                    WalkStatus::Failed,
                    Some(&failure("store", FailureKind::Terminal)),
                    None
                )
                .is_none(),
            "{payload}"
        );
        assert!(state.effect_evidence().is_none(), "{payload}");
    }
    for outcome in [failed(), NodeOutcome::Cancelled] {
        let mut state = ResponseState::new(Some(&contract), true);
        state.observe("move", &outcome, None).unwrap();
        assert!(
            state
                .evidence(
                    WalkStatus::Failed,
                    Some(&failure("move", FailureKind::Terminal)),
                    None
                )
                .is_none()
        );
    }
}

#[test]
fn a_second_successful_selected_visit_makes_the_evidence_ambiguous() {
    let contract = contract();
    for repeated in [committed(), json!([{"error": {"code": "rejected"}}])] {
        let mut state = ResponseState::new(Some(&contract), true);
        state
            .observe("move", &NodeOutcome::ok(committed()), None)
            .unwrap();
        assert!(
            state
                .evidence(
                    WalkStatus::Failed,
                    Some(&failure("store", FailureKind::Terminal)),
                    None
                )
                .is_some()
        );
        state
            .observe("move", &NodeOutcome::ok(repeated), None)
            .unwrap();
        state.observe("store", &failed(), None).unwrap();
        assert!(
            state
                .evidence(
                    WalkStatus::Failed,
                    Some(&failure("store", FailureKind::Terminal)),
                    None
                )
                .is_none()
        );
        assert!(state.effect_evidence().is_none());
        state
            .observe("move", &NodeOutcome::ok(committed()), None)
            .unwrap();
        assert!(
            state
                .evidence(
                    WalkStatus::Failed,
                    Some(&failure("store", FailureKind::Terminal)),
                    None
                )
                .is_none()
        );
    }
}

#[test]
fn terminal_schema_refusal_preserves_only_the_original_committed_result() {
    let contract = contract();
    let mut state = ResponseState::new(Some(&contract), true);
    state
        .observe("move", &NodeOutcome::ok(committed()), None)
        .unwrap();
    let error = state
        .observe("store", &NodeOutcome::ok(committed()), None)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "terminal response violates its declared schema"
    );
    let error = state.interrupted(error, None);
    let interrupted = error.downcast_ref::<InterruptedResponse>().unwrap();
    assert_eq!(interrupted.evidence.committed_result, committed());
    assert_eq!(
        interrupted.source.to_string(),
        "terminal response violates its declared schema"
    );

    let mut valid = ResponseState::new(Some(&contract), true);
    valid
        .observe("move", &NodeOutcome::ok(committed()), None)
        .unwrap();
    valid
        .observe("store", &NodeOutcome::ok(stored()), None)
        .unwrap();
}

#[test]
fn undeclared_and_queued_deliveries_collect_no_response_evidence() {
    let contract = contract();
    for (contract, caller_attached) in [(None, true), (Some(&contract), false)] {
        let mut state = ResponseState::new(contract, caller_attached);
        state
            .observe("move", &NodeOutcome::ok(committed()), None)
            .unwrap();
        state
            .observe("store", &NodeOutcome::ok(Value::Null), None)
            .unwrap();
        assert!(state.effect_evidence().is_none());
        assert!(
            state
                .evidence(
                    WalkStatus::Failed,
                    Some(&failure("store", FailureKind::Terminal)),
                    None
                )
                .is_none()
        );
        let error = state.interrupted(
            io::Error::new(io::ErrorKind::BrokenPipe, "transport closed").into(),
            None,
        );
        assert!(error.downcast_ref::<InterruptedResponse>().is_none());
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(error.to_string(), "transport closed");
    }
}

#[test]
fn an_existing_verdict_suppresses_partial_evidence() {
    let contract = contract();
    let mut state = ResponseState::new(Some(&contract), true);
    state
        .observe("move", &NodeOutcome::ok(committed()), None)
        .unwrap();
    state.observe("store", &failed(), None).unwrap();
    for verdict in [
        Verdict::Respond {
            payload: stored(),
            node_id: "store".to_owned(),
        },
        Verdict::Emit {
            event: json!({"movement_id": "movement-1"}),
            dedup_id: "event-1".to_owned(),
            entity: "movement".to_owned(),
            operation: wamn_event_wire::Op::Insert,
        },
        Verdict::Discard,
    ] {
        assert!(
            state
                .evidence(
                    WalkStatus::Failed,
                    Some(&failure("store", FailureKind::Terminal)),
                    Some(&verdict)
                )
                .is_none()
        );
    }
    assert_eq!(
        state
            .evidence(
                WalkStatus::Failed,
                Some(&failure("store", FailureKind::Terminal)),
                None
            )
            .unwrap()
            .committed_result,
        committed()
    );
}

#[test]
fn interruption_retains_the_original_error_and_the_selected_result() {
    let contract = contract();
    let mut state = ResponseState::new(Some(&contract), true);
    state
        .observe("move", &NodeOutcome::ok(committed()), None)
        .unwrap();
    let source = anyhow::Error::new(io::Error::new(
        io::ErrorKind::ConnectionReset,
        "connection reset",
    ))
    .context("invoke store");
    let error = state.interrupted(source, state.effect_evidence().as_ref());
    let interrupted = error.downcast_ref::<InterruptedResponse>().unwrap();
    assert_eq!(interrupted.evidence.committed_result, committed());
    assert_eq!(interrupted.evidence.effect_outcome, None);
    assert_eq!(interrupted.source.to_string(), "invoke store");
    assert_eq!(
        interrupted
            .source
            .downcast_ref::<io::Error>()
            .unwrap()
            .kind(),
        io::ErrorKind::ConnectionReset
    );
    assert_eq!(
        interrupted.source.root_cause().to_string(),
        "connection reset"
    );
}

#[test]
fn deliveries_reuse_compiled_schemas_without_sharing_result_state() {
    let mut contract = contract();
    // Changing source metadata cannot change a schema already compiled for this cache entry.
    contract.committed.as_mut().unwrap().source = json!({"type": "null"});
    contract.normal.source = json!({"type": "null"});
    for _ in 0..2 {
        let mut state = ResponseState::new(Some(&contract), true);
        assert!(
            state
                .evidence(
                    WalkStatus::Failed,
                    Some(&failure("store", FailureKind::Terminal)),
                    None
                )
                .is_none()
        );
        state
            .observe("move", &NodeOutcome::ok(committed()), None)
            .unwrap();
        state
            .observe("store", &NodeOutcome::ok(stored()), None)
            .unwrap();
        assert_eq!(
            state
                .evidence(
                    WalkStatus::Failed,
                    Some(&failure("store", FailureKind::Terminal)),
                    None
                )
                .unwrap()
                .committed_result,
            committed()
        );
        assert!(
            state
                .observe("store", &NodeOutcome::ok(Value::Null), None)
                .is_err()
        );
    }
}

#[test]
fn cancellation_effect_evidence_belongs_only_to_the_cancelled_node_status() {
    let contract = contract();
    let mut state = ResponseState::new(Some(&contract), true);
    state
        .observe("move", &NodeOutcome::ok(committed()), None)
        .unwrap();
    state
        .observe("store", &NodeOutcome::Cancelled, None)
        .unwrap();
    let absent = state.evidence(WalkStatus::Cancelled, None, None).unwrap();
    assert_eq!(absent.committed_result, committed());
    assert_eq!(
        absent.effect_outcome, None,
        "cancellation does not infer an effect outcome"
    );

    // Supply the observed slot at this boundary without exposing the capability recorder.
    state.failed_effect = Some(("store".to_owned(), EffectOutcome::ResponseLost));
    let observed = state.evidence(WalkStatus::Cancelled, None, None).unwrap();
    assert_eq!(observed.committed_result, committed());
    assert_eq!(observed.effect_outcome, Some(EffectOutcome::ResponseLost));
    assert_eq!(
        state
            .evidence(
                WalkStatus::Failed,
                Some(&failure("store", FailureKind::Terminal)),
                None
            )
            .unwrap()
            .effect_outcome,
        None
    );
    assert!(state.evidence(WalkStatus::Completed, None, None).is_none());
    assert!(
        state
            .evidence(WalkStatus::Cancelled, None, Some(&Verdict::Discard))
            .is_none()
    );

    state.observe("store", &failed(), None).unwrap();
    state.failed_effect = Some(("store".to_owned(), EffectOutcome::Timeout));
    assert_eq!(
        state
            .evidence(WalkStatus::Cancelled, None, None)
            .unwrap()
            .effect_outcome,
        None
    );
    for kind in [
        FailureKind::Terminal,
        FailureKind::RetryExhausted,
        FailureKind::InvalidInput,
    ] {
        assert_eq!(
            state
                .evidence(WalkStatus::Failed, Some(&failure("store", kind)), None)
                .unwrap()
                .effect_outcome,
            Some(EffectOutcome::Timeout)
        );
    }
    for kind in [
        FailureKind::HopLimit,
        FailureKind::UnreleasedCaller,
        FailureKind::MissingDedupId,
        FailureKind::RespondWithoutCaller,
        FailureKind::SecondVerdict,
    ] {
        assert_eq!(
            state
                .evidence(WalkStatus::Failed, Some(&failure("store", kind)), None)
                .unwrap()
                .effect_outcome,
            None
        );
    }
    assert_eq!(
        state
            .evidence(
                WalkStatus::Failed,
                Some(&failure("later", FailureKind::Terminal)),
                None
            )
            .unwrap()
            .effect_outcome,
        None
    );
    state
        .observe("later", &NodeOutcome::Cancelled, None)
        .unwrap();
    assert_eq!(
        state
            .evidence(WalkStatus::Cancelled, None, None)
            .unwrap()
            .effect_outcome,
        None
    );
}
