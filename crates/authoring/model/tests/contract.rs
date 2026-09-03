use serde_json::{Value, json};
use wamn_authoring_model::{
    AuthoringCommandKind, AuthoringDocument, AuthoringQueryKind, AuthoringQueryOutcome,
    AuthoringQueryResponse, AuthoringResponseEnvelope, ContractDecodeErrorKind, GetReportRefusal,
    MAX_QUERY_ID_BYTES, MAX_TEST_SET_CASES, QueryId, ReportProjection, SCHEMA_VERSION,
    ValidatedDraftRef, decode_document,
};

fn scope() -> Value {
    json!({"project-id": "receiving", "environment": "dev"})
}

/// One wiring document, exactly as `catalog.wirings.graph_json` stores it.
///
/// `publish` carries the document itself (wamn-0h0g.7.10), so the frozen
/// literals below carry a real one. The contract does not parse it — that is
/// `wamn_catalog::WiringDocument::parse`'s job on the server — so what this
/// pins is that the field round-trips byte-identical.
fn wiring_document() -> Value {
    json!({
        "format-version": "0.1",
        "wiring-id": "orders-create",
        "version": 1,
        "entry": "node",
        "nodes": {
            "node": {
                "component": "entity",
                "interface-version": "0.1",
                "operation": "create"
            }
        }
    })
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

/// The WHOLE surviving inventory, frozen as literals.
///
/// wamn-0h0g.8.5.5 collapsed five commands and two queries to two commands and
/// one query. This crate is a registered drift gate, so the move is deliberate
/// and the survivors are pinned as complete documents: an added, removed or
/// renamed field on any of them fails here.
#[test]
fn exact_two_commands_and_one_query_round_trip() {
    // Both commands carry the DOCUMENT and its package placement: `publish` since
    // wamn-0h0g.7.10, `gate` since wamn-0h0g.8.28 re-pointed the gate off the
    // stored row it could not have read on a first transition.
    let input = json!({
        "scope": scope(), "package-id": "orders",
        "package-version": "1.0.0", "document": wiring_document()
    });
    let commands = [
        // wamn-0h0g.7.11 moved this pin deliberately: the wire literal is now
        // `gate`, the same name the Rust variant carries.
        command("gate", input.clone()),
        command("publish", input),
    ];
    let queries = [query(
        "get-report",
        json!({"scope": scope(), "report-id": "report-1"}),
        "report-1",
    )];

    for document in commands.into_iter().chain(queries) {
        let decoded = decode(&document);
        assert_eq!(serde_json::to_value(decoded).expect("round trip"), document);
    }
}

/// The four collapsed operations are GONE from the contract, not merely
/// unmounted: a well-formed document naming one no longer decodes at all.
#[test]
fn the_collapsed_draft_operations_no_longer_decode() {
    let validated = json!({"validated-draft-id": "validated-1"});
    let refused_commands = [
        command(
            "save-draft",
            json!({
                "scope": scope(), "draft-id": "draft-1", "wiring-id": "wiring-1",
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
    ];
    let refused_queries = [query(
        "read-draft",
        json!({"scope": scope(), "draft": {"draft-id": "draft-1", "revision": 1}}),
        "read-1",
    )];
    for document in refused_commands.into_iter().chain(refused_queries) {
        let encoded = serde_json::to_string(&document).expect("document serializes");
        assert_eq!(
            decode_document(&encoded)
                .expect_err("a collapsed operation must not decode")
                .kind(),
            ContractDecodeErrorKind::Json,
            "{document}"
        );
    }
}

/// The command inventory is exactly two, in both the tagged enum and the
/// standalone kind vocabulary the ledger shares.
#[test]
fn command_inventory_and_operation_pairing_are_exact() {
    let schema = wamn_authoring_model::json_schema();
    for (definition, field) in [
        ("AuthoringCommand", "kind"),
        ("AuthoringSuccess", "command"),
        ("CommandRefusal", "command"),
    ] {
        assert_eq!(
            schema_discriminators(&schema, definition, field),
            ["gate", "publish"],
            "{definition} inventory drifted"
        );
    }
    let kind_schema = serde_json::to_value(schemars::schema_for!(AuthoringCommandKind))
        .expect("command-kind schema serializes");
    assert_eq!(kind_schema["enum"], json!(["gate", "publish"]));
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
    assert_eq!(MAX_TEST_SET_CASES, 256);
}

#[test]
fn query_refusal_preserves_version_and_query_id() {
    let response = AuthoringDocument::Response(Box::new(AuthoringResponseEnvelope::Query(
        AuthoringQueryResponse {
            schema_version: SCHEMA_VERSION.to_owned(),
            query_id: QueryId::try_from("report-1".to_owned()).expect("valid query id"),
            outcome: AuthoringQueryOutcome::Refused(wamn_authoring_model::QueryRefusal::GetReport(
                GetReportRefusal::ReportNotFound {
                    report_id: "report-9".to_owned(),
                },
            )),
        },
    )));
    let value = serde_json::to_value(response).expect("response serializes");
    assert_eq!(value["body"]["schema-version"], SCHEMA_VERSION);
    assert_eq!(value["body"]["query-id"], "report-1");
    assert_eq!(value["body"]["outcome"]["value"]["query"], "get-report");
}

#[test]
fn finalized_report_projects_only_current_control_store_facts() {
    let report = ReportProjection::Finalized {
        report_id: "report-1".to_owned(),
        validated_draft: ValidatedDraftRef {
            validated_draft_id: "validated-1".to_owned(),
        },
        passed: true,
        summary: json!({"cases": []}),
    };
    let value = serde_json::to_value(report).expect("report serializes");
    assert_eq!(
        value,
        json!({
            "state": "finalized",
            "report-id": "report-1",
            "validated-draft": {"validated-draft-id": "validated-1"},
            "passed": true,
            "summary": {"cases": []},
        })
    );
    assert!(
        !wamn_authoring_model::json_schema_string().contains("resolution-map"),
        "the public schema retained a fact the control report no longer records"
    );

    // wamn-0h0g.8.5.5: `finalized` is the WHOLE projection now. `Pending` was
    // reachable only while the reservation protocol stood, and the owner ruling
    // of 2026-08-25 struck that lineage entire -- so a report either exists for
    // its key or `report-not-found` answers. Pinned as an absence on the public
    // schema as well as on the type, because re-adding the variant is a wire
    // change and must fail here rather than ship.
    // Quoted, because the type's own doc comment reaches the schema as a
    // `description` and says the word; what must be absent is the STATE TAG.
    assert!(
        !wamn_authoring_model::json_schema_string().contains("\"pending\""),
        "the public schema retained the deleted pending report state"
    );
    let refused: Result<ReportProjection, _> = serde_json::from_value(json!({
        "state": "pending",
        "report-id": "report-1",
        "validated-draft": {"validated-draft-id": "validated-1"},
    }));
    assert!(refused.is_err(), "a pending projection still decodes");
}

/// The constitutional clause's refusal, frozen as a whole wire document.
///
/// A gate is a JUDGMENT ABOUT A DOCUMENT, not an execution of it, so a candidate
/// reaching a component with a non-empty effects projection is refused TYPED
/// rather than executed. The refusal names the exact components, so a client can
/// act on it without parsing prose.
#[test]
fn the_effect_free_clause_has_a_typed_refusal_on_the_wire() {
    let document = json!({
        "document": "response",
        "body": {
            "schema-version": SCHEMA_VERSION,
            "command-id": "gate-1",
            "outcome": {
                "status": "refused",
                "value": {
                    "command": "gate",
                    "reason": {
                        "kind": "effectful-component-reached",
                        "components": ["acme:ledger", "acme:mailer"]
                    }
                }
            }
        }
    });
    let decoded = decode(&document);
    assert_eq!(
        serde_json::to_value(decoded).expect("round trip"),
        document,
        "the effect-posture refusal is not wire-stable"
    );

    // It is a gate refusal and nothing else: the components list is required,
    // so a refusal that names no component cannot be composed.
    let mut incomplete = document;
    incomplete["body"]["outcome"]["value"]["reason"]
        .as_object_mut()
        .expect("reason is an object")
        .remove("components");
    assert_eq!(
        decode_document(&serde_json::to_string(&incomplete).expect("serializes"))
            .expect_err("the refusal must name the components it refused on")
            .kind(),
        ContractDecodeErrorKind::Json
    );
}

#[test]
fn operation_specific_refusal_pairing_rejects_cross_operation_reason() {
    // `report-not-successful` is publish's alone; the gate cannot answer with it.
    let invalid = json!({
        "document": "response",
        "body": {
            "schema-version": SCHEMA_VERSION,
            "command-id": "gate-1",
            "outcome": {
                "status": "refused",
                "value": {
                    "command": "gate",
                    "reason": {"kind": "report-not-successful"}
                }
            }
        }
    });
    assert_eq!(
        decode_document(&serde_json::to_string(&invalid).expect("serializes"))
            .expect_err("a publish-only refusal cannot answer the gate")
            .kind(),
        ContractDecodeErrorKind::Json
    );
    // And the converse: the gate's effect-posture refusal is not publish's.
    let inverted = json!({
        "document": "response",
        "body": {
            "schema-version": SCHEMA_VERSION,
            "command-id": "publish-1",
            "outcome": {
                "status": "refused",
                "value": {
                    "command": "publish",
                    "reason": {
                        "kind": "effectful-component-reached",
                        "components": ["acme:ledger"]
                    }
                }
            }
        }
    });
    assert_eq!(
        decode_document(&serde_json::to_string(&inverted).expect("serializes"))
            .expect_err("a gate-only refusal cannot answer publish")
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
        // wamn-0h0g.8.5.5: the draft concept is a client-side file, so every
        // operation, payload and refusal that named server-side draft state is
        // gone from the wire rather than merely unmounted.
        "save-draft",
        "read-draft",
        "draft-run",
        "SaveDraft",
        "ReadDraft",
        "DraftRun",
        "DraftRunCapture",
        "DraftIdentity",
        "DraftDocument",
        "DraftRevisionRef",
        "ValidatedDraftIdentity",
        "expected-revision",
        "revision-conflict",
        "draft-revision-not-found",
        "unresolvable-callee-name",
        "missing-recorded-callability",
        "contract-incompatibility",
        "ValidationIssue",
        "SafeUint64",
        // wamn-0h0g.8.28: both commands carry the document, so neither can look
        // one up and neither can find it missing or drifted from a stored row.
        // `ValidatedDraftRef` itself SURVIVES — `gate` answers with one, carrying
        // the identity the server derived — so only the refusals are retired.
        "validated-draft-not-found",
        "validated-draft-drift",
        // wamn-0h0g.7.11 renamed the gate's wire literal. The superseded
        // spelling is retired vocabulary, not merely unused.
        "test-set-run",
    ] {
        assert!(
            !schema.contains(retired),
            "retired literal survived: {retired}"
        );
    }
    for required in [
        // Quoted: bare `gate` is a substring of every `Gate*` definition name,
        // so an unquoted probe would pass on a schema that had lost the literal.
        "\"gate\"",
        "get-report",
        "publish",
        // The constitutional clause's refusal is part of the public contract.
        "effectful-component-reached",
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
        ["get-report"]
    );
    let kind_schema = serde_json::to_value(schemars::schema_for!(AuthoringQueryKind))
        .expect("query-kind schema serializes");
    assert_eq!(kind_schema["enum"], json!(["get-report"]));
    assert_eq!(
        schema_discriminators(&schema, "AuthoringQuerySuccess", "query"),
        ["get-report"]
    );
    assert_eq!(
        schema_discriminators(&schema, "QueryRefusal", "query"),
        ["get-report"]
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

/// Publish carries the document and its package placement, and NOTHING that
/// asserts an identity the server must derive (wamn-0h0g.7.10).
///
/// The removed halves are pinned as removed, not merely absent from the happy
/// path: `successful-report-id` died with wamn-0h0g.8.5.6's collapse of report
/// id into `wiring_hash`, `validated-draft` named the document this command now
/// carries whole, and a literal `wiring-hash` would reopen the wamn-0h0g.7.8
/// close ruling by handing the server a forgeable, replayable proof value.
/// `deny_unknown_fields` is what refuses all three, so each is exercised.
#[test]
fn publish_carries_the_document_and_derives_no_identity_from_the_wire() {
    let complete = json!({
        "scope": scope(),
        "package-id": "orders",
        "package-version": "1.0.0",
        "document": wiring_document()
    });
    decode(&command("publish", complete.clone()));
    let mut attributed = complete.clone();
    attributed
        .as_object_mut()
        .expect("publish input is an object")
        .insert(
            "provenance".to_owned(),
            json!({"commit": "0123456789abcdef", "ref": null, "dirty": false}),
        );
    decode(&command("publish", attributed));

    // Every field is load-bearing: dropping any one leaves `catalog.wirings`
    // unwritable, so none may carry a serde default.
    for field in ["scope", "package-id", "package-version", "document"] {
        let mut omitted = complete.clone();
        assert!(
            omitted
                .as_object_mut()
                .expect("publish input is an object")
                .remove(field)
                .is_some(),
            "{field} was not in the complete publish input"
        );
        let encoded =
            serde_json::to_string(&command("publish", omitted)).expect("command serializes");
        assert_eq!(
            decode_document(&encoded).unwrap_err().kind(),
            ContractDecodeErrorKind::Json,
            "publish without {field} decoded"
        );
    }

    // The three retired carriers are REFUSED, not ignored.
    for (field, value) in [
        ("successful-report-id", json!("report-1")),
        (
            "validated-draft",
            json!({"validated-draft-id": "validated-1"}),
        ),
        ("wiring-hash", json!("sha256:".to_owned() + &"0".repeat(64))),
    ] {
        let mut extra = complete.clone();
        extra
            .as_object_mut()
            .expect("publish input is an object")
            .insert(field.to_owned(), value);
        let encoded =
            serde_json::to_string(&command("publish", extra)).expect("command serializes");
        assert_eq!(
            decode_document(&encoded).unwrap_err().kind(),
            ContractDecodeErrorKind::Json,
            "publish admitted the retired {field} field"
        );
    }

    // A serde default on any field would also drop it from the published
    // required set, so pin the exact required set (wamn-0h0g.15.121). This is
    // frozen as a WHOLE VALUE: an added, removed or renamed field fails here.
    let schema = wamn_authoring_model::json_schema();
    assert_eq!(
        schema["definitions"]["PublishValidatedDraft"]["required"],
        json!(["document", "package-id", "package-version", "scope"])
    );
    assert_eq!(
        schema["definitions"]["PublishValidatedDraft"]["properties"]["provenance"]["anyOf"][0]["$ref"],
        "#/definitions/CommitProvenance"
    );
}
