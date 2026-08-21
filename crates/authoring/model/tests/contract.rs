use serde_json::{Value, json};
use wamn_authoring_model::{
    AuthoringDocument, AuthoringQueryKind, AuthoringQueryOutcome, AuthoringQueryResponse,
    AuthoringQuerySuccess, AuthoringResponseEnvelope, ContractDecodeErrorKind, DraftDocument,
    DraftIdentity, DraftRevisionRef, DraftRun, DraftRunCapture, MAX_QUERY_ID_BYTES,
    MAX_TEST_SET_CASES, QueryId, ReadDraftRefusal, SAFE_INTEGER_MAX, SCHEMA_VERSION, SafeUint64,
    decode_document,
};

fn scope() -> Value {
    json!({"project-id": "receiving", "environment": "dev"})
}

fn command(kind: &str, input: Value) -> Value {
    json!({
        "document": "request",
        "body": {
            "schema-version": SCHEMA_VERSION,
            "command-id": format!("{kind}-1"),
            "command": {"kind": kind, "input": input}
        }
    })
}

fn query(kind: &str, input: Value, query_id: &str) -> Value {
    json!({
        "document": "request",
        "body": {
            "schema-version": SCHEMA_VERSION,
            "query-id": query_id,
            "query": {"kind": kind, "input": input}
        }
    })
}

fn decode(value: &Value) -> AuthoringDocument {
    decode_document(&serde_json::to_string(value).expect("document serializes"))
        .expect("document decodes")
}

fn schema_discriminators<'a>(schema: &'a Value, definition: &str, field: &str) -> Vec<&'a str> {
    schema["definitions"][definition]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("{definition} has no oneOf"))
        .iter()
        .map(|variant| {
            variant["properties"][field]["enum"][0]
                .as_str()
                .unwrap_or_else(|| panic!("{definition} variant has no {field}"))
        })
        .collect()
}

#[test]
fn exact_five_commands_and_two_queries_round_trip() {
    let validated = json!({"validated-draft-id": "validated-1"});
    let commands = [
        command(
            "save-flow-draft",
            json!({
                "scope": scope(), "draft-id": "draft-1", "flow-id": "flow-1",
                "expected-revision": 0, "definition": "{draft"
            }),
        ),
        command(
            "validate",
            json!({"scope": scope(), "draft": {"draft-id": "draft-1", "revision": 1}}),
        ),
        command(
            "draft-run",
            json!({"scope": scope(), "validated-draft": validated, "input": {"value": 1}}),
        ),
        command(
            "test-set-run",
            json!({"scope": scope(), "validated-draft": validated}),
        ),
        command(
            "publish",
            json!({
                "scope": scope(), "validated-draft": validated,
                "successful-report-id": "report-1"
            }),
        ),
    ];
    let queries = [
        query(
            "read-draft",
            json!({"scope": scope(), "draft": {"draft-id": "draft-1", "revision": 1}}),
            "read-1",
        ),
        query(
            "get-report",
            json!({"scope": scope(), "report-id": "report-1"}),
            "report-1",
        ),
    ];

    for document in commands.into_iter().chain(queries) {
        let decoded = decode(&document);
        assert_eq!(serde_json::to_value(decoded).expect("round trip"), document);
    }
}

#[test]
fn query_id_enforces_exact_utf8_byte_boundary() {
    let at_limit = "q".repeat(MAX_QUERY_ID_BYTES);
    decode(&query(
        "get-report",
        json!({"scope": scope(), "report-id": "report-1"}),
        &at_limit,
    ));

    for refused in [
        String::new(),
        "q".repeat(MAX_QUERY_ID_BYTES + 1),
        "🙂".repeat(MAX_QUERY_ID_BYTES / 4 + 1),
    ] {
        let encoded = serde_json::to_string(&query(
            "get-report",
            json!({"scope": scope(), "report-id": "report-1"}),
            &refused,
        ))
        .expect("query serializes");
        assert_eq!(
            decode_document(&encoded)
                .expect_err("query-id must be refused")
                .kind(),
            ContractDecodeErrorKind::Json
        );
    }

    QueryId::try_from("🙂".repeat(MAX_QUERY_ID_BYTES / 4))
        .expect("exact multibyte boundary is accepted");
}

#[test]
fn public_numeric_and_test_set_bounds_match_their_owners() {
    assert_eq!(MAX_QUERY_ID_BYTES, 64);
    assert_eq!(SAFE_INTEGER_MAX, 9_007_199_254_740_991);
    assert_eq!(MAX_TEST_SET_CASES, 256);
}

#[test]
fn query_refusal_preserves_version_and_query_id() {
    let response = AuthoringDocument::Response(Box::new(AuthoringResponseEnvelope::Query(
        AuthoringQueryResponse {
            schema_version: SCHEMA_VERSION.to_owned(),
            query_id: QueryId::try_from("read-1".to_owned()).expect("valid query id"),
            outcome: AuthoringQueryOutcome::Refused(wamn_authoring_model::QueryRefusal::ReadDraft(
                ReadDraftRefusal::DraftRevisionNotFound {
                    draft_id: "draft-1".to_owned(),
                    revision: SafeUint64::try_from(7_u64).expect("safe revision"),
                },
            )),
        },
    )));
    let value = serde_json::to_value(response).expect("response serializes");
    assert_eq!(value["body"]["schema-version"], SCHEMA_VERSION);
    assert_eq!(value["body"]["query-id"], "read-1");
    assert_eq!(value["body"]["outcome"]["value"]["query"], "read-draft");
}

#[test]
fn completed_query_is_operation_specific() {
    let revision = SafeUint64::try_from(2_u64).expect("safe revision");
    let response = AuthoringDocument::Response(Box::new(AuthoringResponseEnvelope::Query(
        AuthoringQueryResponse {
            schema_version: SCHEMA_VERSION.to_owned(),
            query_id: QueryId::try_from("read-2".to_owned()).expect("valid query id"),
            outcome: AuthoringQueryOutcome::Completed(Box::new(AuthoringQuerySuccess::ReadDraft(
                DraftDocument {
                    draft: DraftIdentity {
                        draft_id: "draft-1".to_owned(),
                        flow_id: "flow-1".to_owned(),
                        revision,
                    },
                    definition: "{draft".to_owned(),
                },
            ))),
        },
    )));
    let value = serde_json::to_value(response).expect("response serializes");
    assert_eq!(value["body"]["outcome"]["value"]["query"], "read-draft");
    assert_eq!(
        value["body"]["outcome"]["value"]["result"]["definition"],
        "{draft"
    );
}

#[test]
fn operation_specific_refusal_pairing_rejects_cross_operation_reason() {
    let invalid = json!({
        "document": "response",
        "body": {
            "schema-version": SCHEMA_VERSION,
            "command-id": "save-1",
            "outcome": {
                "status": "refused",
                "value": {
                    "command": "save-flow-draft",
                    "reason": {"kind": "contract-incompatibility", "site": "call", "flow-id": "f"}
                }
            }
        }
    });
    assert_eq!(
        decode_document(&serde_json::to_string(&invalid).expect("serializes"))
            .expect_err("validate-only refusal cannot answer save")
            .kind(),
        ContractDecodeErrorKind::Json
    );
}

#[test]
fn retired_and_forbidden_vocabulary_is_absent() {
    let schema = wamn_authoring_model::json_schema_string();
    for retired in [
        "get-run",
        "GetRun",
        "RunProjection",
        "RunStatus",
        "RunFailure",
        "RunNodeProjection",
        "NodeOutputProjection",
        "OutputTooLarge",
        "GetRunRefusal",
        "suite-run",
        "suite-projection",
        "grant-draft-safe-generation",
        "revoke-draft-safe-generation",
        "cycle-detected",
        "depth-exceeded",
        "expanded-node-limit",
        "plan-expansion",
        "TestSetInput",
        "TestSetIdentity",
    ] {
        assert!(
            !schema.contains(retired),
            "retired literal survived: {retired}"
        );
    }
    for required in [
        "test-set-run",
        "read-draft",
        "get-report",
        "unresolvable-callee-name",
        "missing-recorded-callability",
        "contract-incompatibility",
        "x-max-utf8-bytes",
    ] {
        assert!(
            schema.contains(required),
            "required literal is absent: {required}"
        );
    }
}

#[test]
fn query_inventory_and_operation_pairing_are_exact() {
    let schema = wamn_authoring_model::json_schema();
    assert_eq!(
        schema_discriminators(&schema, "AuthoringQuery", "kind"),
        ["read-draft", "get-report"]
    );
    let kind_schema = serde_json::to_value(schemars::schema_for!(AuthoringQueryKind))
        .expect("query-kind schema serializes");
    assert_eq!(kind_schema["enum"], json!(["read-draft", "get-report"]));
    assert_eq!(
        schema_discriminators(&schema, "AuthoringQuerySuccess", "query"),
        ["read-draft", "get-report"]
    );
    assert_eq!(
        schema_discriminators(&schema, "QueryRefusal", "query"),
        ["read-draft", "get-report"]
    );
}

#[test]
fn unsupported_version_is_classified_without_dispatch() {
    let mut request = query(
        "get-report",
        json!({"scope": scope(), "report-id": "report-1"}),
        "query-1",
    );
    request["body"]["schema-version"] = json!("0.2");
    let error = decode_document(&serde_json::to_string(&request).expect("serializes"))
        .expect_err("unsupported version must fail");
    assert_eq!(
        error.kind(),
        ContractDecodeErrorKind::UnsupportedContractVersion
    );
    assert_eq!(error.requested(), Some("0.2"));
}

#[test]
fn safe_integer_still_refuses_first_unrepresentable_value() {
    assert!(SafeUint64::try_from(SAFE_INTEGER_MAX).is_ok());
    assert!(SafeUint64::try_from(SAFE_INTEGER_MAX + 1).is_err());
}

#[test]
fn query_request_is_exactly_the_three_ratified_fields() {
    let mut request = query(
        "get-report",
        json!({"scope": scope(), "report-id": "report-1"}),
        "query-1",
    );
    request["body"]["command-id"] = json!("forged-ledger-identity");
    assert!(
        decode_document(&serde_json::to_string(&request).expect("serializes")).is_err(),
        "query envelope admitted a command-ledger field"
    );
}

#[test]
fn query_projection_reference_types_are_stable() {
    let reference = DraftRevisionRef {
        draft_id: "draft-1".to_owned(),
        revision: SafeUint64::try_from(1_u64).expect("safe revision"),
    };
    assert_eq!(
        serde_json::to_value(reference).expect("serializes"),
        json!({"draft-id": "draft-1", "revision": 1})
    );
}

#[test]
fn draft_run_capture_defaults_to_full_and_accepts_only_full_or_off() {
    let omitted = json!({
        "scope": scope(),
        "validated-draft": {"validated-draft-id": "validated-1"},
        "input": {"value": 1}
    });
    let defaulted: DraftRun =
        serde_json::from_value(omitted.clone()).expect("omitted capture is valid");
    assert_eq!(DraftRunCapture::default(), DraftRunCapture::Full);
    assert_eq!(defaulted.capture, DraftRunCapture::Full);
    assert_eq!(
        serde_json::to_value(defaulted).expect("draft run serializes"),
        omitted,
        "full capture is the omitted wire-canonical form"
    );

    for (literal, expected) in [
        ("full", DraftRunCapture::Full),
        ("off", DraftRunCapture::Off),
    ] {
        let mut document = omitted.clone();
        document["capture"] = json!(literal);
        let explicit: DraftRun =
            serde_json::from_value(document).expect("ratified capture literal is valid");
        assert_eq!(explicit.capture, expected);
    }

    for refused in ["Full", "OFF", "scrubbed", "preview", ""] {
        let mut document = omitted.clone();
        document["capture"] = json!(refused);
        assert!(
            serde_json::from_value::<DraftRun>(document).is_err(),
            "capture literal {refused:?} must be refused"
        );
    }

    // The published schema states no `default` for `capture` precisely because
    // full is the default: `skip_serializing_if` suppresses the schemars default
    // whenever that default never reaches the wire. A default of off would
    // publish `allOf` plus `"default": "off"` instead (wamn-0h0g.15.121).
    let schema = wamn_authoring_model::json_schema();
    assert_eq!(
        schema["definitions"]["DraftRun"]["properties"]["capture"],
        json!({"$ref": "#/definitions/DraftRunCapture"})
    );
    assert_eq!(
        schema["definitions"]["DraftRunCapture"]["enum"],
        json!(["full", "off"])
    );
}

#[test]
fn publish_requires_a_successful_report_unconditionally() {
    let complete = json!({
        "scope": scope(),
        "validated-draft": {"validated-draft-id": "validated-1"},
        "successful-report-id": "report-1"
    });
    decode(&command("publish", complete.clone()));

    let mut omitted = complete.clone();
    let removed = omitted
        .as_object_mut()
        .expect("publish input is an object")
        .remove("successful-report-id");
    assert_eq!(removed, Some(json!("report-1")));
    let mut nulled = complete;
    nulled["successful-report-id"] = Value::Null;

    for refused in [omitted, nulled] {
        let encoded =
            serde_json::to_string(&command("publish", refused)).expect("command serializes");
        assert_eq!(
            decode_document(&encoded)
                .expect_err("publish without a stated successful report must be refused")
                .kind(),
            ContractDecodeErrorKind::Json
        );
    }

    // A serde default on the report identity would also drop it from the
    // published required set, so pin the exact required triple (wamn-0h0g.15.121).
    let schema = wamn_authoring_model::json_schema();
    assert_eq!(
        schema["definitions"]["PublishValidatedDraft"]["required"],
        json!(["scope", "successful-report-id", "validated-draft"])
    );
}
