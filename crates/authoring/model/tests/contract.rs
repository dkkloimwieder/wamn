use serde_json::{Value, json};
use wamn_authoring_model::{
    AuthoringCommandType, AuthoringDocument, AuthoringQueryOutcome, AuthoringQueryResponse,
    AuthoringQueryType, AuthoringResponseEnvelope, ContractDecodeErrorType, GetReportRefusal,
    MAX_QUERY_ID_BYTES, QueryId, ReportProjection, SCHEMA_VERSION, ValidatedDraftRef,
    decode_document,
};

fn scope() -> Value {
    json!({"project-id": "orders", "environment": "dev"})
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

fn command(kind: &str, input: &Value) -> Value {
    json!({
        "document": "request",
        "body": {
            "schema-version": SCHEMA_VERSION,
            "command-id": format!("{kind}-1"),
            "command": {"type": kind, "input": input}
        }
    })
}

fn query(kind: &str, input: &Value, query_id: &str) -> Value {
    json!({
        "document": "request",
        "body": {
            "schema-version": SCHEMA_VERSION,
            "query-id": query_id,
            "query": {"type": kind, "input": input}
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

/// All surviving command and query kinds, frozen as literals.
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
        command("gate", &input),
        command("publish", &input),
    ];
    let queries = [query(
        "get-report",
        &json!({"scope": scope(), "report-id": "report-1"}),
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
            &json!({
                "scope": scope(), "draft-id": "draft-1", "wiring-id": "wiring-1",
                "expected-revision": 0, "definition": "{draft"
            }),
        ),
        command(
            "validate",
            &json!({"scope": scope(), "draft": {"draft-id": "draft-1", "revision": 1}}),
        ),
        command(
            "draft-run",
            &json!({"scope": scope(), "validated-draft": validated, "input": {"value": 1}}),
        ),
    ];
    let refused_queries = [query(
        "read-draft",
        &json!({"scope": scope(), "draft": {"draft-id": "draft-1", "revision": 1}}),
        "read-1",
    )];
    for document in refused_commands.into_iter().chain(refused_queries) {
        let encoded = serde_json::to_string(&document).expect("document serializes");
        assert_eq!(
            decode_document(&encoded)
                .expect_err("a collapsed operation must not decode")
                .error_type(),
            ContractDecodeErrorType::Json,
            "{document}"
        );
    }
}

/// There are exactly two command kinds, in both the tagged enum and the
/// standalone kind vocabulary the command table shares.
#[test]
fn command_kinds_and_operation_pairing_are_exact() {
    let schema = wamn_authoring_model::json_schema();
    for (definition, field) in [
        ("AuthoringCommand", "type"),
        ("AuthoringSuccess", "command"),
        ("CommandRefusal", "command"),
    ] {
        assert_eq!(
            schema_discriminators(&schema, definition, field),
            ["gate", "publish"],
            "{definition} variant list drifted"
        );
    }
    let kind_schema = serde_json::to_value(schemars::schema_for!(AuthoringCommandType))
        .expect("command-kind schema serializes");
    assert_eq!(kind_schema["enum"], json!(["gate", "publish"]));
}

#[test]
fn query_id_enforces_exact_utf8_byte_boundary() {
    let at_limit = "q".repeat(MAX_QUERY_ID_BYTES);
    decode(&query(
        "get-report",
        &json!({"scope": scope(), "report-id": "report-1"}),
        &at_limit,
    ));

    for refused in [
        String::new(),
        "q".repeat(MAX_QUERY_ID_BYTES + 1),
        "🙂".repeat(MAX_QUERY_ID_BYTES / 4 + 1),
    ] {
        let encoded = serde_json::to_string(&query(
            "get-report",
            &json!({"scope": scope(), "report-id": "report-1"}),
            &refused,
        ))
        .expect("query serializes");
        assert_eq!(
            decode_document(&encoded)
                .expect_err("query-id must be refused")
                .error_type(),
            ContractDecodeErrorType::Json
        );
    }

    QueryId::try_from("🙂".repeat(MAX_QUERY_ID_BYTES / 4))
        .expect("exact multibyte boundary is accepted");
}

#[test]
fn public_numeric_bounds_match_their_owners() {
    assert_eq!(MAX_QUERY_ID_BYTES, 64);
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
                    "reason": {"type": "report-not-successful"}
                }
            }
        }
    });
    assert_eq!(
        decode_document(&serde_json::to_string(&invalid).expect("serializes"))
            .expect_err("a publish-only refusal cannot answer the gate")
            .error_type(),
        ContractDecodeErrorType::Json
    );
}

#[test]
fn query_kinds_and_operation_pairing_are_exact() {
    let schema = wamn_authoring_model::json_schema();
    assert_eq!(
        schema_discriminators(&schema, "AuthoringQuery", "type"),
        ["get-report"]
    );
    let kind_schema = serde_json::to_value(schemars::schema_for!(AuthoringQueryType))
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
        &json!({"scope": scope(), "report-id": "report-1"}),
        "query-1",
    );
    request["body"]["schema-version"] = json!("0.1");
    let error = decode_document(&serde_json::to_string(&request).expect("serializes"))
        .expect_err("unsupported version must fail");
    assert_eq!(
        error.error_type(),
        ContractDecodeErrorType::UnsupportedContractVersion
    );
    assert_eq!(error.requested(), Some("0.1"));
}

#[test]
fn an_old_contract_version_is_refused_before_decode() {
    // A contract 0.1 body tags its command with `kind`, which 0.2 does not
    // know. The version refusal still wins over the decode error.
    let body = json!({
        "document": "request",
        "body": {
            "schema-version": "0.1",
            "command-id": "command-1",
            "command": {"kind": "publish", "input": {}}
        }
    });
    let error = decode_document(&body.to_string()).expect_err("0.1 is refused");
    assert_eq!(
        error.error_type(),
        ContractDecodeErrorType::UnsupportedContractVersion
    );
    assert_eq!(error.requested(), Some("0.1"));
}

#[test]
fn query_request_is_exactly_the_three_ratified_fields() {
    let mut request = query(
        "get-report",
        &json!({"scope": scope(), "report-id": "report-1"}),
        "query-1",
    );
    request["body"]["command-id"] = json!("forged-command-identity");
    assert!(
        decode_document(&serde_json::to_string(&request).expect("serializes")).is_err(),
        "query envelope admitted a command audit field"
    );
}

/// Publish carries the document and its package placement, and NOTHING that
/// asserts an identity the server must derive (wamn-0h0g.7.10).
///
/// The removed halves are pinned as removed, not merely absent from the happy
/// path: `successful-report-id` died with wamn-0h0g.8.5.6's collapse of report
/// id into `wiring_hash`, `validated-draft` named the document this command now
/// carries whole, and a literal `wiring-hash` would reopen the wamn-0h0g.7.8
/// close ruling by handing the server a forgeable, replayable hash value.
/// `deny_unknown_fields` is what refuses all three, so each is exercised.
#[test]
fn publish_carries_the_document_and_derives_no_identity_from_the_wire() {
    let complete = json!({
        "scope": scope(),
        "package-id": "orders",
        "package-version": "1.0.0",
        "document": wiring_document()
    });
    decode(&command("publish", &complete));
    let mut attributed = complete.clone();
    attributed
        .as_object_mut()
        .expect("publish input is an object")
        .insert(
            "provenance".to_owned(),
            json!({"commit": "0123456789abcdef", "ref": null, "dirty": false}),
        );
    decode(&command("publish", &attributed));

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
            serde_json::to_string(&command("publish", &omitted)).expect("command serializes");
        assert_eq!(
            decode_document(&encoded).unwrap_err().error_type(),
            ContractDecodeErrorType::Json,
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
            serde_json::to_string(&command("publish", &extra)).expect("command serializes");
        assert_eq!(
            decode_document(&encoded).unwrap_err().error_type(),
            ContractDecodeErrorType::Json,
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
