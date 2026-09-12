use serde_json::{Value, json};
use wamn_client::descriptor::FieldSchema;
use wamn_client::request::build_request;
use wamn_client::{ClientError, FieldDescriptor, HttpResponse};
use wamn_client_tui::submission::{
    ErrorCase, Evidence, Replay, ResponseContract, SessionBinding, State, Submission, classify,
};
use wamn_schema_generator::client_ir::{ClientContractIr, FieldIr};

// Runtime projection in this test supplies the same static descriptors that
// generated bindings contain; production screens never allocate schemas.
fn runtime_fields(fields: &[FieldIr]) -> &'static [FieldSchema] {
    Box::leak(
        fields
            .iter()
            .map(|field| {
                let values: Vec<&'static str> = field
                    .values
                    .iter()
                    .map(|value| &*Box::leak(value.clone().into_boxed_str()))
                    .collect();
                FieldSchema {
                    field: FieldDescriptor {
                        path: Box::leak(field.path.clone().into_boxed_str()),
                        type_name: Box::leak(field.type_name.clone().into_boxed_str()),
                        nullable: field.nullable,
                        values: Box::leak(values.into_boxed_slice()),
                    },
                    required: field.required,
                    children: runtime_fields(&field.children),
                    minimum: field.minimum,
                    maximum: field.maximum,
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    )
}

const INPUT: &[FieldSchema] = &[FieldSchema {
    field: FieldDescriptor {
        path: "request_id",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    required: true,
    children: &[],
    minimum: None,
    maximum: None,
}];
const OUTPUT: &[FieldSchema] = &[FieldSchema {
    field: FieldDescriptor {
        path: "receipt_id",
        type_name: "uuid",
        nullable: false,
        values: &[],
    },
    required: true,
    children: &[],
    minimum: None,
    maximum: None,
}];
const ERRORS: &[ErrorCase] = &[
    ErrorCase {
        literal: "quantity_exceeds_remaining",
        required: &["field", "id"],
        sources: &["transaction_invariant"],
    },
    ErrorCase {
        literal: "retry",
        required: &[],
        sources: &["serialization_failure", "connection_unavailable"],
    },
];
fn contract(replay: Replay) -> ResponseContract {
    ResponseContract {
        schema: Some(r#"{"type":"array"}"#),
        partial_schema: None,
        fields: OUTPUT,
        result_class: Some("one"),
        errors: ERRORS,
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay,
    }
}
fn binding(instance: &str) -> SessionBinding {
    SessionBinding {
        url: "http://example.test".into(),
        host: Some("receiving.test".into()),
        target_instance: instance.into(),
    }
}
fn request() -> wamn_client::request::BuiltRequest {
    build_request(INPUT, &json!({"request_id":"intent-1"}), None).unwrap()
}
#[expect(
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    reason = "fixtures accept owned JSON and mirror the transport result consumed by the reducer"
)]
fn response(status: u16, body: Value) -> Result<HttpResponse, ClientError> {
    Ok(HttpResponse {
        status,
        body: body.to_string(),
    })
}
fn denied() -> Result<HttpResponse, ClientError> {
    response(
        403,
        json!({"error":{"code":"permission-denied","operation":"receiving.record_receipt"}}),
    )
}
fn lost() -> Result<HttpResponse, ClientError> {
    Err(ClientError::Transport {
        detail: "response lost".into(),
    })
}

#[test]
fn a_first_authorization_refusal_keeps_the_draft_editable() {
    let mut submission = Submission::new(binding("one"));
    let contract = contract(Replay::Claim);
    let first = submission.begin(request(), &contract).unwrap();
    assert!(submission.resolve(first, &contract, denied()));
    assert!(matches!(submission.state(), State::Refused(_)));
    assert!(submission.begin(request(), &contract).is_ok());
}

#[test]
fn a_retry_refusal_does_not_clear_an_earlier_uncertain_intent() {
    let mut submission = Submission::new(binding("one"));
    let contract = contract(Replay::Claim);
    let first = submission.begin(request(), &contract).unwrap();
    let body = submission.captured().unwrap().body().to_vec();
    submission.resolve(first, &contract, lost());
    let retry = submission.retry().unwrap();
    assert_eq!(submission.captured().unwrap().body(), body);
    submission.resolve(retry, &contract, denied());
    assert!(matches!(
        submission.state(),
        State::Uncertain {
            retry_refusal: Some(_),
            ..
        }
    ));
    assert_eq!(submission.captured().unwrap().body(), body);
    assert!(submission.begin(request(), &contract).is_err());
}

#[test]
fn pending_blocks_double_submit_new_intent_and_retry() {
    let mut submission = Submission::new(binding("one"));
    submission
        .begin(request(), &contract(Replay::Claim))
        .unwrap();
    assert!(
        submission
            .begin(request(), &contract(Replay::Claim))
            .is_err()
    );
    assert!(submission.new_command().is_err());
    assert!(submission.retry().is_err());
}

#[test]
fn only_the_captured_route_replay_contract_grants_retry() {
    for replay in [Replay::Claim, Replay::State, Replay::Unknown] {
        let mut submission = Submission::new(binding("one"));
        let mut contract = contract(replay);
        contract.direct = replay != Replay::Unknown;
        let attempt = submission.begin(request(), &contract).unwrap();
        submission.resolve(attempt, &contract, lost());
        assert_eq!(submission.retry().is_ok(), replay == Replay::Claim);
    }
}

#[test]
fn correlated_success_spends_the_submission_until_an_explicit_new_intent() {
    let mut submission = Submission::new(binding("one"));
    let contract = contract(Replay::Claim);
    let attempt = submission.begin(request(), &contract).unwrap();
    submission.resolve(attempt, &contract, response(200, json!([{
        "request_id":"intent-1", "value":{"receipt_id":"33333333-0000-0000-0000-000000000009"}
    }])));
    assert!(matches!(
        submission.state(),
        State::Succeeded { opaque: false, .. }
    ));
    assert!(submission.retry().is_err());
    assert!(submission.begin(request(), &contract).is_err());
    submission.new_command().unwrap();
    assert!(submission.captured().is_none());
    assert!(submission.begin(request(), &contract).is_ok());
}

#[test]
fn confirmed_partial_completion_spends_the_submission_and_keeps_both_results() {
    let mut submission = Submission::new(binding("one"));
    let contract = contract(Replay::Unknown);
    let attempt = submission.begin(request(), &contract).unwrap();
    let committed_result = json!({"movement_id":"movement-1"});
    let failed_outcome = json!({"operation":"label", "outcome":"refused"});
    submission.resolve_evidence(
        attempt,
        Evidence::PartiallyCompleted {
            committed_result: committed_result.clone(),
            failed_outcome: failed_outcome.clone(),
        },
    );
    assert_eq!(
        submission.state(),
        &State::PartiallyCompleted {
            committed_result,
            failed_outcome
        }
    );
    assert!(submission.begin(request(), &contract).is_err());
    assert!(submission.retry().is_err());
}

#[test]
fn malformed_unknown_and_ambiguous_outcomes_never_become_editable_refusals() {
    let contract = contract(Replay::Claim);
    for body in [
        json!([]),
        json!([{"request_id":"another", "value":{"receipt_id":"33333333-0000-0000-0000-000000000009"}}]),
        json!([{"request_id":"intent-1", "value":{"receipt_id":42}}]),
        json!([{"request_id":"intent-1", "error":{"code":"new_literal","detail":{}}}]),
        json!([{"request_id":"intent-1", "error":{"code":"retry","detail":{}}}]),
        json!([{"request_id":"intent-1", "error":{"code":"quantity_exceeds_remaining","detail":{"field":"quantity"}}}]),
        json!([{"request_id":"intent-1", "value":{"receipt_id":"33333333-0000-0000-0000-000000000009"}, "error":{"code":"quantity_exceeds_remaining","detail":{"field":"quantity","id":"line-1"}}}]),
        json!([{"request_id":"intent-1"}]),
    ] {
        assert!(
            matches!(
                classify(&contract, "intent-1", response(200, body.clone())),
                Evidence::Uncertain(_)
            ),
            "{body}"
        );
    }
    assert!(matches!(
        classify(
            &contract,
            "intent-1",
            Ok(HttpResponse {
                status: 200,
                body: "not JSON".into()
            })
        ),
        Evidence::Uncertain(_)
    ));
}

#[test]
fn a_declared_transaction_refusal_requires_its_details_and_the_whole_route() {
    let body = json!([{"request_id":"intent-1", "error":{"code":"quantity_exceeds_remaining","detail":{"field":"quantity","id":"line-1"}}}]);
    let mut contract = contract(Replay::Claim);
    assert!(matches!(
        classify(&contract, "intent-1", response(200, body.clone())),
        Evidence::Refused(_)
    ));
    contract.direct = false;
    contract.replay = Replay::Unknown;
    assert!(matches!(
        classify(&contract, "intent-1", response(200, body)),
        Evidence::Uncertain(_)
    ));
    assert!(matches!(
        classify(&contract, "intent-1", denied()),
        Evidence::Uncertain(_)
    ));
}

#[test]
fn replacement_blocks_sends_clears_capture_and_rejects_late_old_responses() {
    let mut submission = Submission::new(binding("one"));
    let contract = contract(Replay::Claim);
    let old = submission.begin(request(), &contract).unwrap();
    assert!(
        submission.invalidate(),
        "pending work can still complete on the old target"
    );
    assert!(!submission.available());
    assert!(submission.captured().is_none());
    assert!(submission.begin(request(), &contract).is_err());
    assert!(submission.activate(binding("two")));
    let current = submission.begin(request(), &contract).unwrap();
    assert!(
        !submission.resolve_evidence(old, Evidence::Refused(json!({"code":"permission_denied"})))
    );
    assert_eq!(submission.state(), &State::Pending);
    assert!(submission.resolve(current, &contract, lost()));
}

#[test]
fn a_failed_rebuild_that_keeps_its_activation_keeps_the_session_usable() {
    let mut submission = Submission::new(binding("one"));
    assert!(!submission.activate(binding("one")));
    assert!(submission.available());
    assert!(
        submission
            .begin(request(), &contract(Replay::Claim))
            .is_ok()
    );
}

#[test]
fn composed_and_state_uncertainty_offer_refresh_with_their_exact_limit() {
    use wamn_client_tui::submission::recovery_message;
    let mut composed = contract(Replay::Unknown);
    composed.direct = false;
    assert!(recovery_message(&composed).contains("repeat downstream effects"));
    assert!(recovery_message(&contract(Replay::State)).starts_with("Refresh the record"));
    assert!(recovery_message(&contract(Replay::Claim)).starts_with("Retry the captured"));
}

#[test]
fn exact_ingress_refusals_report_no_dispatch_even_for_composed_routes() {
    let mut contract = contract(Replay::Unknown);
    contract.direct = false;
    for (status, code) in [
        (400, "schema-invalid"),
        (400, "malformed-json"),
        (413, "mapped-payload-too-large"),
        (429, "route-capacity-exhausted"),
    ] {
        assert!(matches!(
            classify(
                &contract,
                "intent-1",
                response(status, json!({"error":{"code":code}}))
            ),
            Evidence::Refused(_)
        ));
        assert!(matches!(
            classify(
                &contract,
                "intent-1",
                response(
                    status,
                    json!({"error":{"code":code,"message":"downstream failure"}})
                )
            ),
            Evidence::Uncertain(_)
        ));
    }
    assert!(matches!(
        classify(
            &contract,
            "intent-1",
            Ok(HttpResponse {
                status: 413,
                body: "request body exceeds 1048576-byte limit\n".into()
            })
        ),
        Evidence::Refused(_)
    ));
}

#[test]
fn an_unknown_error_literal_remains_visible_without_becoming_a_refusal() {
    let evidence = classify(
        &contract(Replay::Claim),
        "intent-1",
        response(
            200,
            json!([{"request_id":"intent-1","error":{"code":"new_literal","detail":{}}}]),
        ),
    );
    assert!(matches!(evidence, Evidence::Uncertain(reason) if reason.contains("new_literal")));
}
#[test]
fn unknown_http_error_literals_remain_diagnostic_and_keep_the_intent_uncertain() {
    for status in [400, 403, 500, 503] {
        for body in [
            json!({"error":{"code":"new_http_literal"}}),
            json!([{"request_id":"intent-1","error":{"code":"new_http_literal"}}]),
        ] {
            let contract = contract(Replay::Claim);
            let mut submission = Submission::new(binding("one"));
            let attempt = submission.begin(request(), &contract).unwrap();
            assert!(submission.resolve(attempt, &contract, response(status, body)));
            assert!(
                matches!(
                    submission.state(),
                    State::Uncertain { reason, retry_refusal: None }
                        if reason.contains("new_http_literal")
                ),
                "HTTP {status}: {:?}",
                submission.state()
            );
            assert_eq!(submission.captured(), Some(&request()));
            assert!(submission.begin(request(), &contract).is_err());
        }
    }
}

#[test]
fn schema_and_correlation_failures_preserve_literals_without_proving_refusal() {
    const STRICT_SCHEMA: &str =
        r#"{"type":"array","items":{"type":"object","required":["request_id","value"]}}"#;
    for (schema, request_id, reason) in [
        (
            Some(STRICT_SCHEMA),
            "intent-1",
            "violates the served contract",
        ),
        (Some("{"), "intent-1", "served response contract is invalid"),
        (
            None,
            "another-intent",
            "does not match the submitted request",
        ),
    ] {
        let mut contract = contract(Replay::Claim);
        contract.schema = schema;
        let mut submission = Submission::new(binding("one"));
        let attempt = submission.begin(request(), &contract).unwrap();
        assert!(submission.resolve(
            attempt,
            &contract,
            response(
                200,
                json!([{
                    "request_id":request_id,
                    "error":{"code":"new_contract_literal"}
                }])
            )
        ));
        assert!(
            matches!(
                submission.state(),
                State::Uncertain { reason: actual, retry_refusal: None }
                    if actual.contains(reason) && actual.contains("new_contract_literal")
            ),
            "{:?}",
            submission.state()
        );
        assert_eq!(submission.captured(), Some(&request()));
        assert!(submission.begin(request(), &contract).is_err());
    }
}

#[test]
fn receiving_update_validates_the_public_success_row_without_sql_bookkeeping_columns() {
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let ir = ClientContractIr::from_release(
        "receiving",
        &root.join("apps/wamn_receiving/generated/contracts"),
        &root.join("apps/wamn_receiving/publication/attachments.json"),
    )
    .expect("project regenerated Receiving release");
    let operation = ir
        .models
        .iter()
        .flat_map(|model| &model.operations)
        .find(|operation| operation.operation == "wamn-receiving:purchase-order/update@1.0.0")
        .expect("Receiving update operation");
    let route = operation.route.as_ref().expect("served update route");
    let contract = ResponseContract {
        partial_schema: None,
        schema: route
            .response
            .schema
            .as_ref()
            .map(|schema| &*Box::leak(schema.to_string().into_boxed_str())),
        fields: runtime_fields(&route.response.fields),
        result_class: route
            .response
            .result_class
            .as_ref()
            .map(|class| &*Box::leak(class.clone().into_boxed_str())),
        errors: &[],
        kind: Box::leak(operation.kind.clone().into_boxed_str()),
        transaction: operation
            .transaction
            .as_ref()
            .map(|transaction| &*Box::leak(transaction.clone().into_boxed_str())),
        direct: route.direct,
        replay: Replay::Unknown,
    };
    let public_row = json!({
        "id":"00000000-0000-0000-0000-000000000001",
        "purchase_order_number":"PO-100",
        "supplier_id":"00000000-0000-0000-0000-000000000002",
        "status":"open",
        "row_version":"8",
        "created_at":"2026-09-08T16:00:00.000000Z",
        "updated_at":"2026-09-08T17:00:00.000000Z"
    });
    assert!(matches!(
        classify(
            &contract,
            "update-1",
            response(
                200,
                json!([
                    {"request_id":"update-1","value":public_row}
                ])
            )
        ),
        Evidence::Succeeded { opaque: false, .. }
    ));
    let mut malformed = public_row;
    malformed
        .as_object_mut()
        .expect("public row")
        .remove("row_version");
    assert!(matches!(
        classify(
            &contract,
            "update-1",
            response(
                200,
                json!([
                    {"request_id":"update-1","value":malformed}
                ])
            )
        ),
        Evidence::Uncertain(_)
    ));
}
#[test]
fn opaque_fields_preserve_rows_but_do_not_erase_known_result_cardinality() {
    let row = json!({"untyped":[true,7,{"detail":null}]});
    for (class, valid, invalid) in [
        ("one", row.clone(), vec![json!(7), Value::Null, json!([])]),
        (
            "bounded_list",
            json!({"rows":[row]}),
            vec![
                json!(7),
                json!({}),
                json!({"rows":{}}),
                json!({"rows":[7]}),
                json!({"rows":[null]}),
                json!({"rows":[[]]}),
            ],
        ),
        (
            "page",
            json!({"item":[row],"next_cursor":null}),
            vec![
                json!(7),
                json!({"next_cursor":null}),
                json!({"item":{},"next_cursor":null}),
                json!({"item":[7],"next_cursor":null}),
                json!({"item":[null],"next_cursor":null}),
                json!({"item":[[]],"next_cursor":null}),
            ],
        ),
    ] {
        let mut contract = contract(Replay::Unknown);
        contract.fields = &[];
        contract.result_class = Some(class);
        assert_eq!(
            classify(
                &contract,
                "intent-1",
                response(
                    200,
                    json!([
                        {"request_id":"intent-1","value":valid}
                    ])
                )
            ),
            Evidence::Succeeded {
                value: valid,
                opaque: true
            },
            "{class} preserves unknown field data"
        );
        for value in invalid {
            assert!(
                matches!(
                    classify(
                        &contract,
                        "intent-1",
                        response(
                            200,
                            json!([
                                {"request_id":"intent-1","value":value}
                            ])
                        )
                    ),
                    Evidence::Uncertain(_)
                ),
                "{class} has a malformed row or collection: {value}"
            );
        }
        let empty = match class {
            "page" => json!({"item":[],"next_cursor":null}),
            "bounded_list" => json!({"rows":[]}),
            _ => json!({}),
        };
        assert!(matches!(
            classify(
                &contract,
                "intent-1",
                response(
                    200,
                    json!([
                        {"request_id":"intent-1","value":empty}
                    ])
                )
            ),
            Evidence::Succeeded { opaque: true, .. }
        ));
    }
}

#[test]
fn page_cursor_requires_its_declared_carrier_with_typed_or_opaque_rows() {
    for fields in [&[][..], OUTPUT] {
        let mut contract = contract(Replay::Unknown);
        contract.fields = fields;
        contract.result_class = Some("page");
        for cursor in [Value::Null, json!("opaque +/%=\n")] {
            let value = json!({"item":[],"next_cursor":cursor});
            assert_eq!(
                classify(
                    &contract,
                    "intent-1",
                    response(
                        200,
                        json!([
                            {"request_id":"intent-1","value":value}
                        ])
                    )
                ),
                Evidence::Succeeded {
                    value,
                    opaque: fields.is_empty()
                }
            );
        }
        for value in [
            json!({"item":[]}),
            json!({"item":[],"next_cursor":7}),
            json!({"item":[],"next_cursor":false}),
            json!({"item":[],"next_cursor":{}}),
            json!({"item":[],"next_cursor":[]}),
        ] {
            assert!(matches!(
                classify(
                    &contract,
                    "intent-1",
                    response(
                        200,
                        json!([
                            {"request_id":"intent-1","value":value}
                        ])
                    )
                ),
                Evidence::Uncertain(_)
            ));
        }
    }
}

#[test]
fn unknown_cardinality_does_not_invent_object_or_collection_requirements() {
    let mut contract = contract(Replay::Unknown);
    contract.fields = &[];
    contract.result_class = None;
    for value in [json!(7), Value::Null, json!([false]), json!({"item":7})] {
        assert_eq!(
            classify(
                &contract,
                "intent-1",
                response(
                    200,
                    json!([
                        {"request_id":"intent-1","value":value}
                    ])
                )
            ),
            Evidence::Succeeded {
                value,
                opaque: true
            }
        );
    }
}

fn wms_move_contract() -> ResponseContract {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let ir = ClientContractIr::from_release(
        "wms",
        &root.join("apps/wamn_wms/generated/contracts"),
        &root.join("apps/wamn_wms/publication/attachments.json"),
    )
    .expect("project WMS served response declarations");
    let operation = ir
        .models
        .iter()
        .flat_map(|model| &model.operations)
        .find(|operation| operation.operation == "wamn-wms:inventory/move@1.0.0")
        .expect("WMS move operation");
    let route = operation.route.as_ref().expect("composed move route");
    assert!(!route.direct);
    assert!(route.replay.is_none());
    ResponseContract {
        schema: route
            .response
            .schema
            .as_ref()
            .map(|schema| &*Box::leak(schema.to_string().into_boxed_str())),
        partial_schema: route
            .response
            .partial_schema
            .as_ref()
            .map(|schema| &*Box::leak(schema.to_string().into_boxed_str())),
        fields: runtime_fields(&route.response.fields),
        result_class: route
            .response
            .result_class
            .as_ref()
            .map(|class| &*Box::leak(class.clone().into_boxed_str())),
        errors: &[],
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: false,
        replay: Replay::Unknown,
    }
}

fn movement_result() -> Value {
    json!({
        "movement_id":"33333333-0000-0000-0000-000000000009",
        "pallet_id":"33333333-0000-0000-0000-000000000002",
        "location_id":"33333333-0000-0000-0000-000000000003",
        "pallet_status":"available",
        "row_version":8
    })
}

fn partial_body() -> Value {
    json!({
        "committed_result":[{"request_id":"intent-1","value":movement_result()}],
        "failed_outcome":{"code":"write_failed","message":"storage request failed","operation":"wamn:node/async-handler@0.1.0"}
    })
}

#[test]
fn declared_partial_http_bytes_preserve_the_commit_and_disable_composed_replay() {
    let contract = wms_move_contract();
    for effect in [
        None,
        Some("refused-before-dispatch"),
        Some("responded"),
        Some("timeout"),
        Some("cancelled"),
        Some("effect-uncertain"),
        Some("response-lost"),
    ] {
        let mut body = partial_body();
        if let Some(effect) = effect {
            body["failed_outcome"]["effect_outcome"] = json!(effect);
        }
        let mut submission = Submission::new(binding("one"));
        let attempt = submission.begin(request(), &contract).unwrap();
        assert!(submission.resolve(attempt, &contract, response(500, body.clone())));
        assert_eq!(
            submission.state(),
            &State::PartiallyCompleted {
                committed_result: movement_result(),
                failed_outcome: body["failed_outcome"].clone()
            }
        );
        assert!(submission.retry().is_err());
        assert!(submission.begin(request(), &contract).is_err());
    }
    let mut denied_after_commit = partial_body();
    denied_after_commit["failed_outcome"] =
        json!({"code":"permission-denied","operation":"downstream"});
    assert!(matches!(
        classify(&contract, "intent-1", response(403, denied_after_commit)),
        Evidence::PartiallyCompleted { .. }
    ));
}

#[test]
fn partial_bytes_require_the_declared_shape_and_one_matching_successful_commit() {
    let contract = wms_move_contract();
    let good = partial_body();
    let mut malformed = Vec::new();
    for key in ["committed_result", "failed_outcome"] {
        let mut body = good.clone();
        body.as_object_mut().unwrap().remove(key);
        malformed.push(body);
    }
    for (pointer, value) in [
        ("/committed_result", json!([])),
        (
            "/committed_result",
            json!([good["committed_result"][0], good["committed_result"][0]]),
        ),
        ("/committed_result/0/request_id", json!("another-intent")),
        ("/committed_result/0/value/movement_id", json!(7)),
        ("/committed_result/0/value/row_version", json!("8")),
        ("/failed_outcome/code", json!("")),
    ] {
        let mut body = good.clone();
        *body.pointer_mut(pointer).unwrap() = value;
        malformed.push(body);
    }
    let mut mixed = good.clone();
    mixed["committed_result"][0]["error"] = json!({"code":"timeout"});
    malformed.push(mixed);
    let mut unobserved = good.clone();
    unobserved["failed_outcome"]["effect_outcome"] = json!("rolled-back");
    malformed.push(unobserved);
    let mut all_results = good.clone();
    all_results["node_results"] = json!({"label":{"zpl":"private"}});
    malformed.push(all_results);
    let mut extra_failure = good.clone();
    extra_failure["failed_outcome"]["trace"] = json!({"private":true});
    malformed.push(extra_failure);
    for body in malformed {
        let mut submission = Submission::new(binding("one"));
        let attempt = submission.begin(request(), &contract).unwrap();
        assert!(submission.resolve(attempt, &contract, response(500, body.clone())));
        assert!(
            matches!(submission.state(), State::Uncertain { .. }),
            "{body}"
        );
        assert!(submission.retry().is_err());
    }
    for schema in [None, Some("{")] {
        let mut undeclared = contract;
        undeclared.partial_schema = schema;
        assert!(matches!(
            classify(&undeclared, "intent-1", response(500, good.clone())),
            Evidence::Uncertain(_)
        ));
    }
    for status in [200, 302] {
        assert!(matches!(
            classify(&contract, "intent-1", response(status, good.clone())),
            Evidence::Uncertain(_)
        ));
    }
    assert!(matches!(
        classify(&contract, "", response(500, good)),
        Evidence::Uncertain(_)
    ));
    assert!(matches!(
        classify(&contract, "intent-1", lost()),
        Evidence::Uncertain(_)
    ));
    assert!(matches!(
        classify(
            &contract,
            "intent-1",
            response(500, json!({"error":{"code":"timeout"}}))
        ),
        Evidence::Uncertain(_)
    ));
}

#[test]
fn wms_normal_bytes_use_the_terminal_label_result_and_keep_passed_errors_uncertain() {
    let contract = wms_move_contract();
    let mut value = movement_result();
    value["zpl"] = json!("^XA^XZ");
    value["stored"] = json!({"container":"labels","key":"movement-label"});
    let body = json!([{"request_id":"intent-1","value":value}]);
    assert_eq!(
        classify(&contract, "intent-1", response(200, body.clone())),
        Evidence::Succeeded {
            value: value.clone(),
            opaque: false
        }
    );
    for path in [
        "/0/value/stored/key",
        "/0/value/stored/container",
        "/0/value/zpl",
    ] {
        let mut malformed = body.clone();
        *malformed.pointer_mut(path).unwrap() = json!(7);
        assert!(
            matches!(
                classify(&contract, "intent-1", response(200, malformed)),
                Evidence::Uncertain(_)
            ),
            "{path}"
        );
    }
    for revision in [json!("8"), json!(8.5), json!(u64::MAX)] {
        let mut malformed = body.clone();
        malformed[0]["value"]["row_version"] = revision;
        assert!(matches!(
            classify(&contract, "intent-1", response(200, malformed)),
            Evidence::Uncertain(_)
        ));
    }
    assert!(matches!(
        classify(
            &contract,
            "intent-1",
            response(
                200,
                json!([{
                    "request_id":"intent-1","value":movement_result()
                }])
            )
        ),
        Evidence::Uncertain(_)
    ));
    let refusal =
        json!([{"request_id":"intent-1","error":{"code":"concurrency_conflict","detail":{}}}]);
    let schema: Value = serde_json::from_str(contract.schema.unwrap()).unwrap();
    wamn_client::request::validate_schema(&schema, &refusal)
        .expect("palette error items pass through the declared normal envelope");
    assert!(matches!(
        classify(&contract, "intent-1", response(200, refusal)),
        Evidence::Uncertain(_)
    ));
}

#[test]
fn undeclared_concurrency_conflict_remains_uncertain_with_both_revisions_visible() {
    let contract = contract(Replay::Unknown);
    for (expected, observed) in [(json!(4), json!(7)), (json!("4"), json!("7"))] {
        let mut submission = Submission::new(binding("one"));
        let attempt = submission.begin(request(), &contract).unwrap();
        assert!(submission.resolve(
            attempt,
            &contract,
            response(
                200,
                json!([{
                    "request_id":"intent-1",
                    "error":{
                        "code":"concurrency_conflict",
                        "detail":{"expected_row_version":expected,"observed_row_version":observed}
                    }
                }])
            )
        ));
        let State::Uncertain { reason, .. } = submission.state() else {
            panic!("an undeclared error cannot establish refusal");
        };
        assert!(reason.contains("concurrency_conflict"));
        assert!(reason.contains("expected_row_version=4, observed_row_version=7"));
        assert!(submission.retry().is_err());
        assert!(submission.begin(request(), &contract).is_err());
    }
}

#[test]
fn malformed_conflict_detail_does_not_invent_revisions_or_render_arbitrary_fields() {
    let contract = contract(Replay::Unknown);
    for detail in [
        json!({"expected_row_version":4,"observed_row_version":7.5}),
        json!({"expected_row_version":null,"observed_row_version":"7"}),
        json!({"expected_row_version":"not-a-revision","observed_row_version":7}),
        json!({"expected_row_version":"9223372036854775808","observed_row_version":7}),
        json!({"expected_row_version":4}),
        json!({"private":"private-detail-must-not-render"}),
    ] {
        let evidence = classify(
            &contract,
            "intent-1",
            response(
                200,
                json!([{
                    "request_id":"intent-1",
                    "error":{"code":"concurrency_conflict","detail":detail}
                }]),
            ),
        );
        let Evidence::Uncertain(reason) = evidence else {
            panic!("malformed details cannot establish an outcome");
        };
        assert!(reason.ends_with("the server reported concurrency_conflict"));
        assert!(!reason.contains("expected_row_version="));
        assert!(!reason.contains("observed_row_version="));
        assert!(!reason.contains("private-detail"));
    }
    let Evidence::Uncertain(reason) = classify(
        &contract,
        "intent-1",
        response(
            200,
            json!([{
                "request_id":"intent-1",
                "error":{
                    "code":"unknown_conflict",
                    "detail":{"expected_row_version":4,"observed_row_version":7}
                }
            }]),
        ),
    ) else {
        panic!("an unknown code cannot establish an outcome");
    };
    assert!(reason.ends_with("the server reported unknown_conflict"));
}
