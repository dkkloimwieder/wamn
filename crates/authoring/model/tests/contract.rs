use std::path::Path;

use serde_json::{Value, json};
use wamn_authoring_model::{
    AuthoringCommand, AuthoringCommandKind, AuthoringDocument, AuthoringOutcome, AuthoringRefusal,
    AuthoringReportQuery, AuthoringRequest, AuthoringResponse, AuthoringScope, AuthoringSuccess,
    BranchCoverageProjection, BranchIdentity, CaseResultProjection, CatalogIdentity,
    CommandRefusal, ContractDecodeError, CoverageState, DraftIdentity, DraftRun, DraftRunReceipt,
    DraftSuiteProjection, EdgeCoverageProjection, EdgeIdentity, EdgeInputPort, NodeOutcome,
    NodeResultProjection, PassFail, PublishValidatedDraft, PublishedFlowIdentity, ResourceKind,
    SCHEMA_VERSION, SaveFlowDraft, SuiteOutcome, SuiteProjectionState, SuiteRef, SuiteRun,
    SuiteRunReceipt, ValidateDraft, ValidatedDraftIdentity, ValidatedDraftRef, ValidationIssue,
    ValidationSeverity, decode_document,
};

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
