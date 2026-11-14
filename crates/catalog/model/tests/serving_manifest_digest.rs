//! Digest and closed-shape tests for serving-manifest format 11.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use wamn_catalog::{
    AdmittedComponentOperation, AttachmentAuthPolicy, AttachmentTarget, AttachmentType,
    CatalogIdentityError, ComponentDescriptor, DefinitionHash, OperationType, PackageCoordinate,
    ServingAttachment, ServingComponent, ServingManifest, ServingRelease, ServingRoute,
    parse_attachment_auth_policy,
};

mod format_three_vector {
    include!("fixtures/release_manifest_format_three_vector.rs");
}

const COMPONENT_A: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const COMPONENT_B: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const DEFINITION: &str = "sha256:5555555555555555555555555555555555555555555555555555555555555555";

fn definition_hash(value: &str) -> DefinitionHash {
    DefinitionHash::parse(value).expect("fixture definition hash is canonical")
}

fn release() -> ServingRelease {
    ServingRelease {
        packages: BTreeSet::from([
            PackageCoordinate::new("platform_fixture", "1.0.0").unwrap(),
            PackageCoordinate::new("platform_fixture_overlay", "3.0.0").unwrap(),
        ]),
    }
}

fn components() -> BTreeSet<ServingComponent> {
    BTreeSet::from([
        ServingComponent {
            runtime_schema: None,
            statement_instances: std::collections::BTreeMap::new(),
            package_id: "platform_fixture".into(),
            descriptor: {
                let mut descriptor = ComponentDescriptor::named("transform", "0.1", COMPONENT_B);
                descriptor.operations = BTreeMap::from([(
                    "map".into(),
                    AdmittedComponentOperation {
                        connections: Vec::new(),
                        host_transaction: false,
                        input_ports: Vec::new(),
                        output_ports: Vec::new(),
                        parameters: Vec::new(),
                        committed_result_schema: None,
                        registered_operation: None,
                        statements: BTreeMap::new(),
                    },
                )]);
                descriptor
            },
        },
        ServingComponent {
            runtime_schema: None,
            statement_instances: std::collections::BTreeMap::new(),
            package_id: "platform_fixture_overlay".into(),
            descriptor: {
                let mut descriptor = ComponentDescriptor::named("http-request", "0.1", COMPONENT_A);
                descriptor.operations = BTreeMap::from([(
                    "platform-fixture-overlay:widget/get@3.0.0".into(),
                    AdmittedComponentOperation {
                        connections: Vec::new(),
                        host_transaction: false,
                        input_ports: Vec::new(),
                        output_ports: Vec::new(),
                        parameters: Vec::new(),
                        committed_result_schema: None,
                        registered_operation: Some(
                            "platform-fixture-overlay:widget/get@3.0.0".into(),
                        ),
                        statements: BTreeMap::new(),
                    },
                )]);
                descriptor
            },
        },
    ])
}

fn routes() -> BTreeSet<ServingRoute> {
    BTreeSet::from([ServingRoute {
        package_id: "platform_fixture_overlay".into(),
        component: "http-request".into(),
        operation: "platform-fixture-overlay:widget/get@3.0.0".into(),
        type_: OperationType::Get,
        reads: BTreeSet::new(),
        revision: None,
        idempotency: None,
    }])
}

fn manifest() -> ServingManifest {
    ServingManifest::new(
        release(),
        components(),
        routes(),
        BTreeMap::from([(
            "widget-get-http".into(),
            ServingAttachment {
                type_: AttachmentType::Http,
                package_id: "platform_fixture_overlay".into(),
                target: AttachmentTarget::Route {
                    component: "http-request".into(),
                    operation: "platform-fixture-overlay:widget/get@3.0.0".into(),
                },
                definition_hash: definition_hash(DEFINITION),
                definition: json!({
                    "id": "widget-get-http",
                    "type": "http",
                    "run-deadline-ms": 30000
                }),
                auth_policy: json!({"modes": ["pat"]}),
                registered_operation: Some("platform-fixture-overlay:widget/get@3.0.0".into()),
            },
        )]),
    )
    .expect("the format-eleven fixture is valid")
}

#[test]
fn the_frozen_format_three_vector_refuses_with_its_version() {
    let error = ServingManifest::from_canonical_bytes(format_three_vector::CANONICAL_BYTES)
        .expect_err("format 9 refuses the frozen format-3 bytes");
    assert!(
        matches!(
            &error,
            CatalogIdentityError::UnsupportedServingManifestVersion { requested }
                if requested == "3"
        ),
        "the format-3 bytes refuse with their version, not an unknown field: {error:?}"
    );
}

#[test]
fn authentication_modes_are_shared_and_bound_into_canonical_bytes() {
    let mut authenticated_digests = BTreeSet::new();
    for (modes, expected, allows_pat, allows_session) in [
        (json!(["none"]), AttachmentAuthPolicy::None, false, false),
        (json!(["pat"]), AttachmentAuthPolicy::Pat, true, false),
        (
            json!(["session"]),
            AttachmentAuthPolicy::Session,
            false,
            true,
        ),
        (
            json!(["pat", "session"]),
            AttachmentAuthPolicy::PatAndSession,
            true,
            true,
        ),
        (
            json!(["signature"]),
            AttachmentAuthPolicy::Signature {
                header: "X-Signature",
                credential: "webhook",
            },
            false,
            false,
        ),
    ] {
        let mut policy = json!({"modes": modes});
        if let AttachmentAuthPolicy::Signature { header, credential } = expected {
            policy["header"] = json!(header);
            policy["credential"] = json!(credential);
        }
        let parsed = parse_attachment_auth_policy(&policy).expect("supported modes");
        assert_eq!(parsed, expected);
        assert_eq!(parsed.allows_pat(), allows_pat);
        assert_eq!(parsed.allows_session(), allows_session);

        let mut candidate = manifest();
        let attachment = candidate.attachments.get_mut("widget-get-http").unwrap();
        attachment.auth_policy = policy.clone();
        if parsed.has_no_caller() {
            attachment.registered_operation = None;
        }
        let bytes = candidate.canonical_bytes();
        let (admitted, digest) = ServingManifest::from_canonical_bytes(&bytes)
            .expect("canonical policy survives the release reader");
        assert_eq!(admitted.attachments["widget-get-http"].auth_policy, policy);
        assert_eq!(admitted.canonical_bytes(), bytes);
        if parsed != AttachmentAuthPolicy::None {
            assert!(authenticated_digests.insert(digest.as_str().to_owned()));
        }
    }
}

#[test]
fn malformed_authentication_lists_are_refused_by_parser_and_release_reader() {
    for policy in [
        json!(null),
        json!([]),
        json!({}),
        json!({"mode": "pat"}),
        json!({"modes": null}),
        json!({"modes": "pat"}),
        json!({"modes": []}),
        json!({"modes": [null]}),
        json!({"modes": [1]}),
        json!({"modes": [""]}),
        json!({"modes": ["unknown"]}),
        json!({"modes": ["PAT"]}),
        json!({"modes": ["pat", "pat"]}),
        json!({"modes": ["session", "session"]}),
        json!({"modes": ["none", "none"]}),
        json!({"modes": ["none", "pat"]}),
        json!({"modes": ["session", "none"]}),
        json!({"modes": ["session", "pat"]}),
        json!({"modes": ["pat", "unknown"]}),
        json!({"modes": ["pat", "session", "session"]}),
        json!({"modes": ["pat"], "mode": "pat"}),
        json!({"modes": ["pat"], "unknown": true}),
    ] {
        assert_eq!(parse_attachment_auth_policy(&policy), None);
        let mut candidate = manifest();
        candidate
            .attachments
            .get_mut("widget-get-http")
            .unwrap()
            .auth_policy = policy;
        assert_eq!(
            ServingManifest::from_canonical_bytes(&candidate.canonical_bytes()),
            Err(CatalogIdentityError::InvalidAttachmentAuthPolicy {
                attachment_id: "widget-get-http".into(),
            })
        );
    }
}

/// A provider and a consumer, where `package` owns the component that exports
/// `export`.
///
/// The owning package id is a parameter rather than the export's namespace,
/// because production does not tie the two together: a component that exports
/// the platform interface `wamn:node/handler@0.1.0` carries the application's own
/// package id, as the fixture of `crates/control/release/src/publish_release.rs` shows
/// with `http-request` under `"package-id":"orders"`. Deriving the id from the
/// namespace produced the reserved id `wamn`, which is not a manifest any
/// publisher can publish.
fn operation_provider_manifest(package: &str, export: &str, version: &str) -> ServingManifest {
    let operation = AdmittedComponentOperation {
        connections: Vec::new(),
        host_transaction: false,
        input_ports: Vec::new(),
        output_ports: Vec::new(),
        parameters: Vec::new(),
        registered_operation: None,
        committed_result_schema: None,
        statements: BTreeMap::new(),
    };
    ServingManifest::new(
        ServingRelease {
            packages: BTreeSet::from([
                PackageCoordinate::new(package, version).unwrap(),
                PackageCoordinate::new("consumer", "1.0.0").unwrap(),
            ]),
        },
        BTreeSet::from([
            ServingComponent {
                runtime_schema: None,
                statement_instances: std::collections::BTreeMap::new(),
                package_id: package.into(),
                descriptor: {
                    let mut descriptor =
                        ComponentDescriptor::named("provider", "0.1.0", COMPONENT_A);
                    descriptor.operations = BTreeMap::from([(
                        export.into(),
                        AdmittedComponentOperation {
                            connections: Vec::new(),
                            host_transaction: false,
                            input_ports: Vec::new(),
                            output_ports: Vec::new(),
                            parameters: Vec::new(),
                            // Registered only when `package` owns the export's
                            // namespace, which is the rule
                            // `validate_canonical_operation_for_package` enforces. A
                            // platform interface is exported as a palette operation,
                            // and a palette operation carries no registered
                            // operation.
                            registered_operation: export
                                .starts_with(&format!("{package}:"))
                                .then(|| export.to_owned()),

                            ..operation.clone()
                        },
                    )]);
                    descriptor
                },
            },
            ServingComponent {
                runtime_schema: None,
                statement_instances: std::collections::BTreeMap::new(),
                package_id: "consumer".into(),
                descriptor: {
                    let mut descriptor =
                        ComponentDescriptor::named("consumer", "0.1.0", COMPONENT_B);
                    descriptor.operations =
                        BTreeMap::from([("consumer:entry/run@1.0.0".into(), operation)]);
                    descriptor
                },
            },
        ]),
        BTreeSet::new(),
        BTreeMap::new(),
    )
    .expect("one provider and one consumer form a valid manifest")
}

#[test]
fn duplicate_export_only_interfaces_are_admitted() {
    // The platform interface is exported by an application component under the
    // application's own package id, the shape a published manifest carries.
    for (package, export, version) in [
        ("orders", "wamn:node/handler@0.1.0", "0.1.0"),
        ("provider", "provider:entry/run@1.0.0", "1.0.0"),
    ] {
        let mut candidate = operation_provider_manifest(package, export, version);
        let mut other = candidate
            .components
            .iter()
            .find(|component| component.descriptor.component == "provider")
            .expect("fixture provider")
            .clone();
        other.descriptor.component = "another-provider".into();
        other.descriptor.component_digest = COMPONENT_B.into();
        let operation = other.descriptor.operations.get_mut(export).unwrap();
        operation.registered_operation = None;
        assert!(candidate.components.insert(other));
        let (admitted, _) = ServingManifest::from_canonical_bytes(&candidate.canonical_bytes())
            .expect("the host can address duplicate export-only interfaces directly");
        assert_eq!(admitted, candidate);
    }
}

#[test]
fn every_manifest_hash_validates_during_deserialization() {
    let invalid = "sha256:not-a-canonical-digest";

    let mut component_manifest = manifest();
    let mut component = component_manifest.components.pop_first().unwrap();
    invalid.clone_into(&mut component.descriptor.component_digest);
    component_manifest.components.insert(component);
    let component_error =
        ServingManifest::from_canonical_bytes(&component_manifest.canonical_bytes())
            .expect_err("an invalid component digest must be refused while decoding");
    assert!(component_error.to_string().contains("artifact-hash"));

    let mut attachment_document = serde_json::to_value(manifest()).expect("manifest serializes");
    attachment_document["attachments"]["widget-get-http"]["definition-hash"] = json!(invalid);
    let attachment_error = serde_json::from_value::<ServingManifest>(attachment_document)
        .expect_err("an invalid attachment definition hash must be refused while decoding");
    assert!(attachment_error.to_string().contains("definition-hash"));
}

#[test]
fn all_six_collections_have_canonical_order() {
    let baseline = manifest();
    let permuted = ServingManifest::new(
        release(),
        components().into_iter().rev().collect(),
        routes().into_iter().rev().collect(),
        baseline
            .every_attachment()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|(id, item)| (id.to_owned(), ServingAttachment::from(item)))
            .collect(),
    )
    .expect("permuted facts remain valid");

    assert_eq!(baseline.canonical_bytes(), permuted.canonical_bytes());
    assert_eq!(baseline.digest(), permuted.digest());
}
