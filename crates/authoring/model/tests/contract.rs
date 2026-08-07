use std::path::Path;

use serde_json::{Value, json};
use wamn_authoring_model::{
    AuthoringCommand, AuthoringCommandKind, AuthoringDocument, AuthoringOutcome, AuthoringRefusal,
    AuthoringReportQuery, AuthoringRequest, AuthoringResponse, AuthoringScope, AuthoringSuccess,
    BranchCoverageProjection, BranchIdentity, CaseResultProjection, CatalogIdentity,
    CommandRefusal, ContractDecodeError, CoverageState, DraftIdentity, DraftRun, DraftRunReceipt,
    DraftSuiteProjection, EdgeCoverageProjection, EdgeIdentity, EdgeInputPort, NodeOutcome,
    NodeResultProjection, PassFail, PendingReportReason, PendingSuiteProjection,
    PublishValidatedDraft, PublishedFlowIdentity, ResourceKind, SCHEMA_VERSION, SaveFlowDraft,
    SuiteExecutionRefusal, SuiteOutcome, SuiteProjectionState, SuiteRef, SuiteRun, SuiteRunReceipt,
    ValidateDraft, ValidatedDraftIdentity, ValidatedDraftRef, ValidationIssue, ValidationSeverity,
    decode_document,
};

/// Definitions whose variants carry structured refusal fields.
const REFUSAL_DEFINITIONS: [&str; 3] = [
    "AuthoringRefusal",
    "PendingReportReason",
    "SuiteExecutionRefusal",
];

fn scope() -> AuthoringScope {
    AuthoringScope {
        project_id: "receiving".into(),
        environment: "dev".into(),
    }
}

fn draft() -> DraftIdentity {
    DraftIdentity {
        draft_id: "draft-7".into(),
        flow_id: "receive-material".into(),
        revision: 3,
    }
}

fn validated() -> ValidatedDraftIdentity {
    ValidatedDraftIdentity {
        validated_draft_id: "sha256:validated".into(),
        draft: draft(),
        runtime_flow_version: 4,
        artifact_hash: "sha256:artifact".into(),
        execution_bundle_hash: "sha256:bundle".into(),
        catalog: CatalogIdentity {
            catalog_id: "receiving-catalog".into(),
            version: 9,
        },
        environment: "dev".into(),
    }
}

fn validated_ref() -> ValidatedDraftRef {
    ValidatedDraftRef {
        validated_draft_id: "sha256:validated".into(),
    }
}

fn request(command_id: &str, command: AuthoringCommand) -> AuthoringDocument {
    AuthoringDocument::Request(AuthoringRequest {
        schema_version: SCHEMA_VERSION.into(),
        command_id: command_id.into(),
        command,
    })
}

fn projection() -> DraftSuiteProjection {
    DraftSuiteProjection {
        projection_version: SCHEMA_VERSION.into(),
        report_id: "report-5".into(),
        execution_id: "execution-5".into(),
        draft: validated(),
        suite: SuiteRef {
            suite_id: "happy-and-hold".into(),
            flow_version: 3,
        },
        outcome: SuiteOutcome::Failed,
        edit_to_run_ms: Some(41),
        cases: vec![CaseResultProjection {
            case_id: "hold".into(),
            run_id: "run-hold".into(),
            outcome: PassFail::Failed,
            failure: None,
        }],
        nodes: vec![NodeResultProjection {
            node_id: "decide".into(),
            outcome: NodeOutcome::Passed,
            observed_case_ids: vec!["hold".into()],
            failed_case_ids: Vec::new(),
        }],
        branches: vec![BranchCoverageProjection {
            branch: BranchIdentity {
                from_node_id: "decide".into(),
                from_port: "hold".into(),
            },
            coverage: CoverageState::Covered,
        }],
        edges: vec![EdgeCoverageProjection {
            edge: EdgeIdentity {
                from_node_id: "decide".into(),
                from_port: "hold".into(),
                to_node_id: "create-hold".into(),
                to_port: EdgeInputPort::Default,
            },
            coverage: CoverageState::NotObserved,
        }],
    }
}

fn refusal_response(reason: AuthoringRefusal) -> AuthoringDocument {
    AuthoringDocument::Response(Box::new(AuthoringResponse {
        schema_version: SCHEMA_VERSION.into(),
        command_id: "refusal-keys".into(),
        outcome: AuthoringOutcome::Refused(CommandRefusal {
            command: AuthoringCommandKind::Validate,
            reason,
        }),
    }))
}

fn projection_response(state: SuiteProjectionState) -> AuthoringDocument {
    AuthoringDocument::Response(Box::new(AuthoringResponse {
        schema_version: SCHEMA_VERSION.into(),
        command_id: "projection-keys".into(),
        outcome: AuthoringOutcome::Completed(Box::new(AuthoringSuccess::SuiteProjection(
            Box::new(state),
        ))),
    }))
}

/// Every field-carrying refusal variant, paired with the schema definition
/// that publishes it and the pointer to it inside its carrier document.
fn structured_refusal_documents() -> Vec<(&'static str, &'static str, AuthoringDocument)> {
    const REASON: &str = "/body/outcome/value/reason";
    const PENDING_REASON: &str = "/body/outcome/value/result/report/reason";
    const SUITE_REFUSAL: &str = "/body/outcome/value/result/report/outcome/refusal";

    let mut cases: Vec<_> = [
        AuthoringRefusal::UnsupportedContractVersion {
            requested: "0.2".into(),
            supported: SCHEMA_VERSION.into(),
        },
        AuthoringRefusal::RevisionConflict {
            expected_revision: 2,
            actual_revision: Some(3),
        },
        AuthoringRefusal::ResourceNotFound {
            resource: ResourceKind::Suite,
            id: "suite-a".into(),
        },
        AuthoringRefusal::InvalidDraft {
            issues: vec![ValidationIssue {
                severity: ValidationSeverity::Error,
                code: "missing-entry".into(),
                path: "/nodes".into(),
                message: "one entry node is required".into(),
            }],
        },
        AuthoringRefusal::UnresolvedNodes {
            node_types: vec!["custom-a".into()],
        },
        AuthoringRefusal::DraftConnectionsDenied {
            connection_names: vec!["erp".into()],
        },
        AuthoringRefusal::PublishBlockedBySuite {
            report_id: "report-5".into(),
        },
        AuthoringRefusal::PublishBlockedByNonterminalRuns {
            run_ids: vec!["run-parked".into()],
        },
    ]
    .into_iter()
    .map(|reason| ("AuthoringRefusal", REASON, refusal_response(reason)))
    .collect();

    cases.push((
        "PendingReportReason",
        PENDING_REASON,
        projection_response(SuiteProjectionState::Pending(PendingSuiteProjection {
            report_id: "report-5".into(),
            execution_id: "execution-5".into(),
            validated_draft: validated_ref(),
            reason: PendingReportReason::CaptureInterrupted {
                run_ids: vec!["run-hold".into()],
            },
            captured_case_ids: vec!["hold".into()],
        })),
    ));

    for refusal in [
        SuiteExecutionRefusal::UndrivableNodes {
            node_types: vec!["custom-a".into()],
        },
        SuiteExecutionRefusal::DraftConnectionsDenied {
            connection_names: vec!["erp".into()],
        },
    ] {
        let mut report = projection();
        report.outcome = SuiteOutcome::Refused(refusal);
        cases.push((
            "SuiteExecutionRefusal",
            SUITE_REFUSAL,
            projection_response(SuiteProjectionState::Finalized(Box::new(report))),
        ));
    }

    cases
}

/// Property names the schema publishes for one tagged variant.
fn published_properties(schema: &Value, definition: &str, kind: &str) -> Vec<String> {
    let variants = schema["definitions"][definition]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("{definition} publishes a tagged variant list"));
    let variant = variants
        .iter()
        .find(|variant| variant["properties"]["kind"]["enum"] == json!([kind]))
        .unwrap_or_else(|| panic!("{definition} publishes no variant for kind `{kind}`"));
    variant["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("{definition} variant `{kind}` publishes properties"))
        .keys()
        .cloned()
        .collect()
}

/// Field-carrying variants the schema publishes for one definition.
fn published_structured_variants(schema: &Value, definition: &str) -> usize {
    schema["definitions"][definition]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("{definition} publishes a tagged variant list"))
        .iter()
        .filter(|variant| {
            variant["properties"]
                .as_object()
                .is_some_and(|properties| properties.len() > 1)
        })
        .count()
}

#[test]
fn command_inventory_is_frontend_neutral_and_round_trips() {
    let suite = SuiteRef {
        suite_id: "suite-a".into(),
        flow_version: 3,
    };
    let commands = [
        (
            "save-flow-draft",
            AuthoringCommand::SaveFlowDraft(SaveFlowDraft {
                scope: scope(),
                draft_id: "draft-7".into(),
                flow_id: "receive-material".into(),
                expected_revision: 2,
                definition: "{invalid intermediate flow text".into(),
            }),
        ),
        (
            "validate",
            AuthoringCommand::Validate(ValidateDraft {
                scope: scope(),
                draft: wamn_authoring_model::DraftRevisionRef {
                    draft_id: "draft-7".into(),
                    revision: 3,
                },
                suite: suite.clone(),
            }),
        ),
        (
            "draft-run",
            AuthoringCommand::DraftRun(DraftRun {
                scope: scope(),
                validated_draft: validated_ref(),
                input: json!({"receipt": "r-1"}),
            }),
        ),
        (
            "suite-run",
            AuthoringCommand::SuiteRun(SuiteRun {
                scope: scope(),
                validated_draft: validated_ref(),
                suite: suite.clone(),
            }),
        ),
        (
            "publish",
            AuthoringCommand::Publish(PublishValidatedDraft {
                scope: scope(),
                validated_draft: validated_ref(),
                successful_report_id: "report-5".into(),
            }),
        ),
        (
            "suite-projection",
            AuthoringCommand::SuiteProjection(AuthoringReportQuery {
                scope: scope(),
                report_id: "report-5".into(),
            }),
        ),
    ];

    for (expected_kind, command) in commands {
        let document = request(expected_kind, command);
        let encoded = serde_json::to_string(&document).unwrap();
        let value: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["body"]["command"]["kind"], expected_kind);
        assert_eq!(decode_document(&encoded).unwrap(), document);
    }

    let command_kind_wires = [
        AuthoringCommandKind::SaveFlowDraft,
        AuthoringCommandKind::Validate,
        AuthoringCommandKind::DraftRun,
        AuthoringCommandKind::SuiteRun,
        AuthoringCommandKind::Publish,
        AuthoringCommandKind::SuiteProjection,
    ]
    .map(|kind| serde_json::to_value(kind).unwrap());
    assert_eq!(
        command_kind_wires,
        [
            json!("save-flow-draft"),
            json!("validate"),
            json!("draft-run"),
            json!("suite-run"),
            json!("publish"),
            json!("suite-projection"),
        ]
    );
}

#[test]
fn every_success_shape_and_typed_refusal_round_trips() {
    let successes = [
        AuthoringSuccess::SaveFlowDraft(draft()),
        AuthoringSuccess::Validate(validated()),
        AuthoringSuccess::DraftRun(DraftRunReceipt {
            run_id: "run-1".into(),
            validated_draft: validated_ref(),
        }),
        AuthoringSuccess::SuiteRun(SuiteRunReceipt {
            report_id: "report-5".into(),
            execution_id: "execution-5".into(),
            validated_draft: validated_ref(),
        }),
        AuthoringSuccess::Publish(PublishedFlowIdentity {
            flow_id: "receive-material".into(),
            version: 4,
            artifact_hash: "sha256:artifact".into(),
        }),
        AuthoringSuccess::SuiteProjection(Box::new(SuiteProjectionState::Finalized(Box::new(
            projection(),
        )))),
    ];

    for (index, success) in successes.into_iter().enumerate() {
        let document = AuthoringDocument::Response(Box::new(AuthoringResponse {
            schema_version: SCHEMA_VERSION.into(),
            command_id: format!("command-{index}"),
            outcome: AuthoringOutcome::Completed(Box::new(success)),
        }));
        let encoded = serde_json::to_string(&document).unwrap();
        assert_eq!(decode_document(&encoded).unwrap(), document);
    }

    let refusals = [
        AuthoringRefusal::AuthorizationDenied,
        AuthoringRefusal::UnsupportedContractVersion {
            requested: "0.2".into(),
            supported: SCHEMA_VERSION.into(),
        },
        AuthoringRefusal::RevisionConflict {
            expected_revision: 2,
            actual_revision: Some(3),
        },
        AuthoringRefusal::ResourceNotFound {
            resource: ResourceKind::Suite,
            id: "suite-a".into(),
        },
        AuthoringRefusal::InvalidDraft {
            issues: vec![ValidationIssue {
                severity: ValidationSeverity::Error,
                code: "missing-entry".into(),
                path: "/nodes".into(),
                message: "one entry node is required".into(),
            }],
        },
        AuthoringRefusal::CatalogDrift,
        AuthoringRefusal::UnresolvedNodes {
            node_types: vec!["custom-a".into()],
        },
        AuthoringRefusal::ValidatedDraftDrift,
        AuthoringRefusal::DraftConnectionsDenied {
            connection_names: vec!["erp".into()],
        },
        AuthoringRefusal::PublishBlockedBySuite {
            report_id: "report-5".into(),
        },
        AuthoringRefusal::PublishExecutableDrift,
        AuthoringRefusal::PublishBlockedByNonterminalRuns {
            run_ids: vec!["run-parked".into()],
        },
    ];

    for (index, reason) in refusals.into_iter().enumerate() {
        let document = AuthoringDocument::Response(Box::new(AuthoringResponse {
            schema_version: SCHEMA_VERSION.into(),
            command_id: format!("refusal-{index}"),
            outcome: AuthoringOutcome::Refused(CommandRefusal {
                command: AuthoringCommandKind::Validate,
                reason,
            }),
        }));
        let encoded = serde_json::to_string(&document).unwrap();
        assert_eq!(decode_document(&encoded).unwrap(), document);
    }
}

#[test]
fn structured_refusals_round_trip_the_property_names_the_schema_publishes() {
    let schema = wamn_authoring_model::json_schema();
    let cases = structured_refusal_documents();
    for definition in REFUSAL_DEFINITIONS {
        assert_eq!(
            cases
                .iter()
                .filter(|(covered, ..)| *covered == definition)
                .count(),
            published_structured_variants(&schema, definition),
            "{definition} publishes a field-carrying variant this table does not cover"
        );
    }

    for (definition, pointer, document) in cases {
        let encoded = serde_json::to_value(&document).expect("document serializes");
        let refusal = encoded
            .pointer(pointer)
            .expect("carrier exposes the refusal");
        let kind = refusal["kind"].as_str().expect("refusal is tagged by kind");

        let mut published = serde_json::Map::new();
        for property in published_properties(&schema, definition, kind) {
            let value = refusal.get(&property).unwrap_or_else(|| {
                panic!(
                    "{definition} variant `{kind}` publishes `{property}`, \
                     which the serde wire form does not emit"
                )
            });
            published.insert(property, value.clone());
        }
        assert_eq!(
            &Value::Object(published.clone()),
            refusal,
            "{definition} variant `{kind}` emits wire fields the schema does not publish"
        );

        let mut candidate = encoded.clone();
        *candidate
            .pointer_mut(pointer)
            .expect("carrier exposes the refusal") = Value::Object(published);
        let decoded = decode_document(&candidate.to_string())
            .unwrap_or_else(|error| panic!("{definition} variant `{kind}` must decode: {error}"));
        assert_eq!(decoded, document);
        assert_eq!(
            serde_json::to_value(&decoded).expect("document serializes"),
            encoded
        );
    }
}

#[test]
fn drifted_refusal_field_spelling_is_rejected() {
    let schema = wamn_authoring_model::json_schema();
    let published = REFUSAL_DEFINITIONS
        .iter()
        .flat_map(|definition| {
            schema["definitions"][*definition]["oneOf"]
                .as_array()
                .unwrap_or_else(|| panic!("{definition} publishes a tagged variant list"))
        })
        .flat_map(|variant| {
            variant["properties"]
                .as_object()
                .expect("variant publishes properties")
                .keys()
        })
        .filter(|property| property.contains('-'))
        .count();
    assert!(
        published > 0,
        "no refusal property carries the canonical multi-word spelling"
    );

    let mut mutants = 0;
    for (definition, pointer, document) in structured_refusal_documents() {
        let encoded = serde_json::to_value(&document).expect("document serializes");
        let refusal = encoded
            .pointer(pointer)
            .and_then(Value::as_object)
            .expect("carrier exposes the refusal")
            .clone();
        for field in refusal.keys().filter(|field| field.contains('-')) {
            let mut drifted = refusal.clone();
            let value = drifted.remove(field).expect("field is present");
            let spelling = field.replace('-', "_");
            drifted.insert(spelling.clone(), value);
            let mut candidate = encoded.clone();
            *candidate
                .pointer_mut(pointer)
                .expect("carrier exposes the refusal") = Value::Object(drifted);
            assert!(
                decode_document(&candidate.to_string()).is_err(),
                "{definition} must reject the drifted field spelling `{spelling}`"
            );
            mutants += 1;
        }
    }
    assert_eq!(
        mutants, published,
        "every published multi-word refusal property must be exercised by a drift mutant"
    );
}

#[test]
fn versions_and_privileged_or_frontend_fields_fail_closed() {
    let schema = wamn_authoring_model::json_schema();
    for (definition, field) in [
        ("AuthoringRequest", "schema-version"),
        ("AuthoringResponse", "schema-version"),
        ("DraftSuiteProjection", "projection-version"),
    ] {
        assert_eq!(
            schema["definitions"][definition]["properties"][field]["enum"],
            json!([SCHEMA_VERSION]),
            "{definition}.{field} must pin the supported version for non-Rust clients"
        );
    }

    let document = request(
        "validate-1",
        AuthoringCommand::Validate(ValidateDraft {
            scope: scope(),
            draft: wamn_authoring_model::DraftRevisionRef {
                draft_id: "draft-7".into(),
                revision: 3,
            },
            suite: SuiteRef {
                suite_id: "suite-a".into(),
                flow_version: 3,
            },
        }),
    );
    let mut value = serde_json::to_value(&document).unwrap();
    value["body"]
        .as_object_mut()
        .unwrap()
        .remove("schema-version");
    assert!(matches!(
        decode_document(&value.to_string()),
        Err(ContractDecodeError::Json(_))
    ));

    let mut value = serde_json::to_value(&document).unwrap();
    value["body"]["schema-version"] = json!("0.2");
    assert!(matches!(
        decode_document(&value.to_string()),
        Err(ContractDecodeError::UnsupportedContractVersion { requested }) if requested == "0.2"
    ));

    for field in [
        "principal",
        "credential",
        "database-url",
        "endpoint",
        "bundle",
        "frontend-state",
        "shell-host",
    ] {
        let mut value = serde_json::to_value(&document).unwrap();
        value["body"]
            .as_object_mut()
            .unwrap()
            .insert(field.into(), json!("https://privileged.invalid/internal"));
        assert!(
            matches!(
                decode_document(&value.to_string()),
                Err(ContractDecodeError::Json(_))
            ),
            "client-controlled {field} must be rejected"
        );
    }

    let response = AuthoringDocument::Response(Box::new(AuthoringResponse {
        schema_version: SCHEMA_VERSION.into(),
        command_id: "projection-1".into(),
        outcome: AuthoringOutcome::Completed(Box::new(AuthoringSuccess::SuiteProjection(
            Box::new(SuiteProjectionState::Finalized(Box::new(
                DraftSuiteProjection {
                    projection_version: "0.2".into(),
                    ..projection()
                },
            ))),
        ))),
    }));
    assert!(matches!(
        decode_document(&serde_json::to_string(&response).unwrap()),
        Err(ContractDecodeError::UnsupportedProjectionVersion { requested }) if requested == "0.2"
    ));
}

#[test]
fn projection_identity_and_observation_states_are_never_implicit() {
    let value = serde_json::to_value(projection()).unwrap();

    assert_eq!(value["nodes"][0]["node-id"], "decide");
    assert_eq!(value["nodes"][0]["outcome"], "passed");
    assert_eq!(value["branches"][0]["branch"]["from-node-id"], "decide");
    assert_eq!(value["branches"][0]["branch"]["from-port"], "hold");
    assert_eq!(value["branches"][0]["coverage"], "covered");
    assert_eq!(value["edges"][0]["edge"]["from-node-id"], "decide");
    assert_eq!(value["edges"][0]["edge"]["from-port"], "hold");
    assert_eq!(value["edges"][0]["edge"]["to-node-id"], "create-hold");
    assert!(value["edges"][0]["edge"].get("to-port").is_some());
    assert!(value["edges"][0]["edge"]["to-port"].is_null());
    assert_eq!(value["edges"][0]["coverage"], "not-observed");

    let mut missing_to_port = value.clone();
    missing_to_port["edges"][0]["edge"]
        .as_object_mut()
        .unwrap()
        .remove("to-port");
    assert!(serde_json::from_value::<DraftSuiteProjection>(missing_to_port).is_err());

    for (state, wire) in [
        (CoverageState::Covered, "covered"),
        (CoverageState::NotCovered, "not-covered"),
        (CoverageState::NotObserved, "not-observed"),
        (CoverageState::Unknown, "unknown"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), wire);
    }
    for (state, wire) in [
        (NodeOutcome::Passed, "passed"),
        (NodeOutcome::Failed, "failed"),
        (NodeOutcome::NotObserved, "not-observed"),
        (NodeOutcome::Unknown, "unknown"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), wire);
    }
}

#[test]
fn committed_schema_matches_public_types() {
    let committed = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs/contracts/authoring-surface.schema.json"),
    )
    .expect("read committed authoring schema");
    assert_eq!(
        committed,
        wamn_authoring_model::json_schema_string(),
        "authoring-surface.schema.json is stale; regenerate it with print-authoring-surface-schema"
    );
}
