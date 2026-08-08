use std::path::Path;

use serde_json::{Value, json};
use wamn_authoring_model::{
    AuthoringCommand, AuthoringCommandKind, AuthoringDocument, AuthoringOutcome, AuthoringRefusal,
    AuthoringReportQuery, AuthoringRequest, AuthoringResponse, AuthoringScope, AuthoringSuccess,
    BranchCoverageProjection, BranchIdentity, CaseResultProjection, CatalogIdentity,
    CommandRefusal, CommitProvenance, ContractDecodeError, CoverageState, DraftIdentity, DraftRun,
    DraftRunReceipt, DraftSuiteProjection, EdgeCoverageProjection, EdgeIdentity, EdgeInputPort,
    NodeOutcome, NodeResultProjection, PassFail, PendingReportReason, PendingSuiteProjection,
    PublishValidatedDraft, PublishedFlowIdentity, ResourceKind, SAFE_INTEGER_MAX, SCHEMA_VERSION,
    SafeUint64, SaveFlowDraft, SuiteExecutionRefusal, SuiteOutcome, SuiteProjectionState, SuiteRef,
    SuiteRun, SuiteRunReceipt, ValidateDraft, ValidatedDraftIdentity, ValidatedDraftRef,
    ValidationIssue, ValidationSeverity, decode_document,
};

/// Definitions whose variants carry structured refusal fields.
const REFUSAL_DEFINITIONS: [&str; 3] = [
    "AuthoringRefusal",
    "PendingReportReason",
    "SuiteExecutionRefusal",
];

/// A literal known to sit inside the exactly representable wire domain.
fn exact(value: u64) -> SafeUint64 {
    SafeUint64::try_from(value).expect("test literal is inside the wire domain")
}

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
        revision: exact(3),
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
    AuthoringDocument::Request(Box::new(AuthoringRequest {
        schema_version: SCHEMA_VERSION.into(),
        command_id: command_id.into(),
        command,
    }))
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
        edit_to_run_ms: Some(exact(41)),
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
            expected_revision: exact(2),
            actual_revision: Some(exact(3)),
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
                expected_revision: exact(2),
                definition: "{invalid intermediate flow text".into(),
                provenance: None,
            }),
        ),
        (
            "validate",
            AuthoringCommand::Validate(ValidateDraft {
                scope: scope(),
                draft: wamn_authoring_model::DraftRevisionRef {
                    draft_id: "draft-7".into(),
                    revision: exact(3),
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
            expected_revision: exact(2),
            actual_revision: Some(exact(3)),
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

/// Provenance is optional, inert, and structurally incapable of naming anyone.
#[test]
fn commit_provenance_is_optional_attribution_and_never_an_identity() {
    let with_provenance = |provenance: Option<CommitProvenance>| {
        request(
            "save-receiving-draft-3",
            AuthoringCommand::SaveFlowDraft(SaveFlowDraft {
                scope: scope(),
                draft_id: "draft-7".into(),
                flow_id: "receive-material".into(),
                expected_revision: exact(2),
                definition: "{invalid intermediate flow text".into(),
                provenance,
            }),
        )
    };
    let attributed = CommitProvenance {
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        r#ref: Some("refs/heads/main".into()),
        dirty: false,
    };

    // Omitting the field is legal for every existing 0.1 producer: a document
    // written before this field existed still decodes, and decodes to `None`.
    let mut without = serde_json::to_value(with_provenance(None)).unwrap();
    let input = without["body"]["command"]["input"]
        .as_object_mut()
        .expect("a command input");
    assert_eq!(input.remove("provenance"), Some(json!(null)));
    let AuthoringDocument::Request(decoded) =
        decode_document(&without.to_string()).expect("a document without provenance decodes")
    else {
        panic!("a request decodes as a request")
    };
    let AuthoringCommand::SaveFlowDraft(saved) = decoded.command else {
        panic!("save-flow-draft decodes as itself")
    };
    assert_eq!(saved.provenance, None);

    // Present, it round-trips exactly, including an explicitly null ref.
    for provenance in [
        attributed.clone(),
        CommitProvenance {
            r#ref: None,
            dirty: true,
            ..attributed.clone()
        },
    ] {
        let document = with_provenance(Some(provenance.clone()));
        let text = serde_json::to_string(&document).expect("emit provenance");
        assert_eq!(
            decode_document(&text).expect("provenance decodes"),
            document,
            "provenance did not round-trip"
        );
        let wire = serde_json::to_value(&document).unwrap();
        let emitted = &wire["body"]["command"]["input"]["provenance"];
        assert_eq!(emitted["commit"], json!(provenance.commit));
        assert_eq!(emitted["dirty"], json!(provenance.dirty));
        // `ref` is required-and-nullable, never omitted, so a missing key can
        // never be confused with a client that knew no ref.
        assert!(emitted.get("ref").is_some());
    }

    // Provenance is not an identity channel: it carries no field a reader could
    // mistake for a principal, a role, or a credential.
    let fields = serde_json::to_value(&attributed).unwrap();
    let fields = fields.as_object().expect("provenance is an object");
    assert_eq!(
        fields.keys().map(String::as_str).collect::<Vec<_>>(),
        // `serde_json` maps are ordered, so this is the whole field set.
        ["commit", "dirty", "ref"]
    );
    for identity in [
        "author",
        "committer",
        "email",
        "principal",
        "subject",
        "role",
        "user",
        "signer",
    ] {
        assert!(
            !fields.contains_key(identity),
            "provenance published an identity-shaped field {identity}"
        );
    }

    // And it stays closed to smuggled fields like every other contract struct.
    for smuggled in ["principal", "role", "token", "author"] {
        let mut wire = serde_json::to_value(&attributed).unwrap();
        wire.as_object_mut()
            .expect("provenance is an object")
            .insert(smuggled.into(), json!("bob@example.com"));
        assert!(
            serde_json::from_value::<CommitProvenance>(wire).is_err(),
            "provenance accepted smuggled {smuggled}"
        );
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
                revision: exact(3),
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

/// Collect every `format: uint64` site with the `maximum` it publishes, keyed
/// by a readable path.
fn uint64_sites(schema: &Value, path: String, found: &mut Vec<(String, Option<Value>)>) {
    match schema {
        Value::Object(members) => {
            if members.get("format") == Some(&json!("uint64")) {
                found.push((path.clone(), members.get("maximum").cloned()));
            }
            for (name, member) in members {
                uint64_sites(member, format!("{path}/{name}"), found);
            }
        }
        Value::Array(members) => {
            for (index, member) in members.iter().enumerate() {
                uint64_sites(member, format!("{path}/{index}"), found);
            }
        }
        _ => {}
    }
}

/// The published schema is the contract a non-Rust client compiles against, so
/// every uint64 site must carry the bound the Rust boundary enforces — read
/// back from the committed bytes, as an exact integer.
///
/// `as_u64` is the assertion that matters: a float bound would be written
/// `9007199254740991.0`, which `serde_json` reads back as `9007199254740990`,
/// silently moving the contract one below the value it is meant to admit.
#[test]
fn every_uint64_schema_site_publishes_the_safe_integer_maximum() {
    let committed: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../docs/contracts/authoring-surface.schema.json"),
        )
        .expect("read committed authoring schema"),
    )
    .expect("parse committed authoring schema");

    let mut found = Vec::new();
    uint64_sites(&committed, "#".to_owned(), &mut found);

    assert_eq!(
        found.len(),
        6,
        "expected the six known uint64 sites, found {found:?}"
    );
    for (path, maximum) in &found {
        assert_eq!(
            maximum.as_ref().and_then(Value::as_u64),
            Some(SAFE_INTEGER_MAX),
            "{path} must publish an exact integer maximum {SAFE_INTEGER_MAX} so every client refuses what Rust refuses"
        );
    }
}

/// `2^53-1` is inside the domain: it survives both directions unchanged.
#[test]
fn safe_integer_maximum_round_trips_exactly_in_both_directions() {
    let document = request(
        "save-boundary",
        AuthoringCommand::SaveFlowDraft(SaveFlowDraft {
            scope: scope(),
            draft_id: "draft-7".into(),
            flow_id: "receive-material".into(),
            expected_revision: exact(SAFE_INTEGER_MAX),
            definition: "{}".into(),
            provenance: None,
        }),
    );
    let text = serde_json::to_string(&document).expect("emit the boundary revision");
    assert!(
        text.contains("9007199254740991"),
        "the emitted document must carry the exact integer, not a rounded one"
    );
    assert_eq!(
        decode_document(&text).expect("accept the boundary"),
        document
    );

    let projection = DraftSuiteProjection {
        edit_to_run_ms: Some(exact(SAFE_INTEGER_MAX)),
        ..projection()
    };
    let text = serde_json::to_string(&projection).expect("emit the boundary latency");
    let decoded: DraftSuiteProjection = serde_json::from_str(&text).expect("accept the boundary");
    assert_eq!(decoded.edit_to_run_ms, Some(exact(SAFE_INTEGER_MAX)));
    assert_eq!(u64::from(decoded.edit_to_run_ms.unwrap()), SAFE_INTEGER_MAX);
}

/// `2^53` is the first value JavaScript cannot hold exactly, so the contract
/// refuses it on decode and cannot construct it for encode.
#[test]
fn two_to_the_fifty_third_is_refused_in_both_directions() {
    assert_out_of_domain_refuses(9_007_199_254_740_992);
}

/// `u64::MAX` is refused by the same boundary; nothing rounds it into an
/// identity a client would then echo back.
#[test]
fn u64_max_is_refused_in_both_directions() {
    assert_out_of_domain_refuses(u64::MAX);
}

/// Both wire directions refuse `value`: decode rejects the document, and the
/// bounded type cannot be built, so no serializer can ever emit it.
fn assert_out_of_domain_refuses(value: u64) {
    assert!(
        value > SAFE_INTEGER_MAX,
        "the fixture must sit outside the wire domain"
    );

    // Emit: the type is the guard, from a `u64` and from a `bigint` column.
    assert!(SafeUint64::try_from(value).is_err());
    if let Ok(stored) = i64::try_from(value) {
        assert!(SafeUint64::try_from(stored).is_err());
    }

    // Accept: every uint64 site refuses through the existing decode rejection.
    let document = request(
        "save-out-of-domain",
        AuthoringCommand::SaveFlowDraft(SaveFlowDraft {
            scope: scope(),
            draft_id: "draft-7".into(),
            flow_id: "receive-material".into(),
            expected_revision: exact(0),
            definition: "{}".into(),
            provenance: None,
        }),
    );
    let mut wire = serde_json::to_value(&document).unwrap();
    wire["body"]["command"]["input"]["expected-revision"] = json!(value);
    assert!(
        matches!(
            decode_document(&wire.to_string()),
            Err(ContractDecodeError::Json(_))
        ),
        "expected-revision {value} must be refused, never rounded"
    );

    let mut wire = serde_json::to_value(projection()).unwrap();
    wire["edit-to-run-ms"] = json!(value);
    assert!(
        serde_json::from_value::<DraftSuiteProjection>(wire).is_err(),
        "edit-to-run-ms {value} must be refused, never rounded"
    );

    let mut wire = serde_json::to_value(refusal_response(AuthoringRefusal::RevisionConflict {
        expected_revision: exact(2),
        actual_revision: Some(exact(3)),
    }))
    .unwrap();
    wire["body"]["outcome"]["value"]["reason"]["actual-revision"] = json!(value);
    assert!(
        matches!(
            decode_document(&wire.to_string()),
            Err(ContractDecodeError::Json(_))
        ),
        "actual-revision {value} must be refused, never rounded"
    );
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
