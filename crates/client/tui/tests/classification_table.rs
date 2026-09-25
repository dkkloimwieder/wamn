//! The shared classification cases, read from the same file the browser reads.
//!
//! `classify()` owns the rule. `web/runtime/src/transport.ts` implements the
//! same rule for the browser, and `web/runtime/test/classification.test.ts`
//! reads this file too. A case that one client reads differently is a defect
//! in one of them.

use std::collections::BTreeMap;

use serde_json::Value;
use wamn_client::HttpResponse;
use wamn_client::descriptor::FieldSchema;
use wamn_client_tui::submission::{ErrorCase, Evidence, Replay, ResponseContract, classify};

/// The contract holds `'static` references, so every case leaks its own.
fn response_contract(contract: &Value) -> ResponseContract {
    let errors: Vec<ErrorCase> = contract["errors"]
        .as_array()
        .expect("the contract lists its refusals")
        .iter()
        .map(|refusal| ErrorCase {
            literal: leak(refusal["literal"].as_str().expect("a refusal literal")),
            required: leak_list(&refusal["required"]),
            sources: leak_list(&refusal["sources"]),
        })
        .collect();
    ResponseContract {
        schema: None,
        partial_schema: contract["partial_schema"].as_str().map(leak),
        fields: &[] as &[FieldSchema],
        result_class: contract["result_class"].as_str().map(leak),
        errors: Box::leak(errors.into_boxed_slice()),
        kind: leak(contract["kind"].as_str().expect("an operation kind")),
        transaction: contract["transaction"].as_str().map(leak),
        direct: contract["direct"].as_bool().expect("a direct flag"),
        replay: Replay::Unknown,
    }
}

fn leak(value: &str) -> &'static str {
    Box::leak(value.to_owned().into_boxed_str())
}

fn leak_list(values: &Value) -> &'static [&'static str] {
    let leaked: Vec<&'static str> = values
        .as_array()
        .expect("a list of literals")
        .iter()
        .map(|value| leak(value.as_str().expect("a literal")))
        .collect();
    Box::leak(leaked.into_boxed_slice())
}

#[test]
fn every_shared_case_classifies_as_the_table_states() {
    let table: Value = serde_json::from_str(include_str!("data/classification-cases.json"))
        .expect("the shared case table parses");
    let cases = table["cases"].as_array().expect("the table lists cases");
    assert!(cases.len() >= 20, "the table covers every branch");

    for case in cases {
        let name = case["name"].as_str().expect("a case name");
        let contract = response_contract(&table["contracts"][case["contract"].as_str().unwrap()]);
        let response = HttpResponse {
            actor_labels: BTreeMap::new(),
            status: u16::try_from(case["status"].as_u64().expect("a status")).expect("a status"),
            body: case["body"].as_str().expect("a body").to_owned(),
        };
        // A read case states a null identity: its outcome matches by position.
        let request_id = case["request_id"].as_str();
        let evidence = classify(&contract, request_id, Ok(response));
        let expect = &case["expect"];
        let outcome = expect["outcome"].as_str().expect("an expected outcome");
        match (&evidence, outcome) {
            (Evidence::Succeeded { value, .. }, "completed") => {
                assert_eq!(value, &expect["value"], "{name}");
            }
            (Evidence::Refused(refusal), "refused") => match expect["code"].as_str() {
                Some(code) => {
                    assert_eq!(refusal["code"], code, "{name}");
                    // The detail is everything the refusal states besides
                    // its code, which is how the browser carries it too.
                    let mut detail = refusal.clone();
                    detail
                        .as_object_mut()
                        .expect("a refusal object")
                        .remove("code");
                    if expect["detail"].is_null() {
                        assert_eq!(
                            detail,
                            serde_json::json!({}),
                            "{name}: the refusal states nothing besides its code"
                        );
                    } else {
                        assert_eq!(detail, expect["detail"], "{name}");
                    }
                }
                // A refusal that states no code carries its text instead.
                None => assert_eq!(refusal, &expect["detail"], "{name}"),
            },
            (
                Evidence::PartiallyCompleted {
                    committed_result,
                    failed_outcome,
                },
                "partially_completed",
            ) => {
                assert_eq!(committed_result, &expect["committed_result"], "{name}");
                assert_eq!(failed_outcome, &expect["failed_outcome"], "{name}");
            }
            (Evidence::Uncertain(reason), "uncertain") => {
                assert_eq!(
                    reason,
                    expect["reason"].as_str().expect("an expected reason"),
                    "{name}"
                );
            }
            (evidence, outcome) => panic!("{name}: expected {outcome}, read {evidence:?}"),
        }
    }
}
