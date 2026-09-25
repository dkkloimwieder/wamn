//! Digest and closed-shape tests for serving-manifest format 3.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use wamn_catalog::{
    ArtifactHash, AttachmentAuthPolicy, AttachmentKind, AttachmentTarget, CatalogIdentityError,
    DefinitionHash, EffectiveReleaseId, OperationKind, PackageCoordinate, ServingAttachment,
    ServingComponent, ServingComponentOperation, ServingManifest, ServingRegistration,
    ServingRegistrationInput, ServingRelease, ServingRoute, ServingWiring,
    parse_attachment_auth_policy,
};

mod mint_vector {
    include!("fixtures/release_manifest_mint_vector.rs");
}

const COMPONENT_A: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const COMPONENT_B: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const GRAPH_A: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
const GRAPH_B: &str = "sha256:4444444444444444444444444444444444444444444444444444444444444444";
const DEFINITION: &str = "sha256:5555555555555555555555555555555555555555555555555555555555555555";

fn artifact_hash(value: &str) -> ArtifactHash {
    ArtifactHash::parse(value).expect("fixture artifact hash is canonical")
}

fn definition_hash(value: &str) -> DefinitionHash {
    DefinitionHash::parse(value).expect("fixture definition hash is canonical")
}

fn release() -> ServingRelease {
    ServingRelease {
        tenant_id: "manifest-mint-tenant".into(),
        effective_release_id: EffectiveReleaseId::new(3).expect("non-zero release"),
        environment: "prod".into(),
        packages: BTreeSet::from([
            PackageCoordinate::new("platform_fixture", "1.0.0").unwrap(),
            PackageCoordinate::new("platform_fixture_overlay", "3.0.0").unwrap(),
        ]),
    }
}

fn components() -> BTreeSet<ServingComponent> {
    BTreeSet::from([
        ServingComponent {
            package_id: "platform_fixture".into(),
            component: "transform".into(),
            interface_version: "0.1".into(),
            digest: artifact_hash(COMPONENT_B),
            operations: BTreeMap::from([(
                "map".into(),
                ServingComponentOperation {
                    pre_commit: None,
                    committed_result_schema: None,
                    fresh_only: false,
                    registered_operation: None,
                    permissions: BTreeSet::new(),
                    participant: None,
                    statements: BTreeMap::new(),
                },
            )]),
        },
        ServingComponent {
            package_id: "platform_fixture_overlay".into(),
            component: "http-request".into(),
            interface_version: "0.1".into(),
            digest: artifact_hash(COMPONENT_A),
            operations: BTreeMap::from([(
                "platform-fixture-overlay:widget/get@3.0.0".into(),
                ServingComponentOperation {
                    pre_commit: None,
                    committed_result_schema: None,
                    fresh_only: false,
                    registered_operation: Some("platform-fixture-overlay:widget/get@3.0.0".into()),
                    permissions: BTreeSet::from([
                        "platform-fixture-overlay:widget/get@3.0.0".into()
                    ]),
                    participant: None,
                    statements: BTreeMap::new(),
                },
            )]),
        },
    ])
}

fn wirings() -> BTreeSet<ServingWiring> {
    BTreeSet::from([
        ServingWiring {
            package_id: "platform_fixture".into(),
            wiring_id: "shipping".into(),
            wiring_version: 2,
            graph_hash: definition_hash(GRAPH_B),
        },
        ServingWiring {
            package_id: "platform_fixture_overlay".into(),
            wiring_id: "orders".into(),
            wiring_version: 1,
            graph_hash: definition_hash(GRAPH_A),
        },
    ])
}

fn routes() -> BTreeSet<ServingRoute> {
    BTreeSet::from([ServingRoute {
        package_id: "platform_fixture_overlay".into(),
        component: "http-request".into(),
        operation: "platform-fixture-overlay:widget/get@3.0.0".into(),
        kind: OperationKind::Get,
        reads: BTreeSet::new(),
        revision: None,
    }])
}

fn manifest() -> ServingManifest {
    ServingManifest::new(
        release(),
        components(),
        routes(),
        wirings(),
        BTreeMap::from([
            (
                "widget-get-http".into(),
                ServingAttachment {
                    kind: AttachmentKind::Http,
                    package_id: "platform_fixture_overlay".into(),
                    target: AttachmentTarget::Route {
                        component: "http-request".into(),
                        operation: "platform-fixture-overlay:widget/get@3.0.0".into(),
                    },
                    definition_hash: definition_hash(DEFINITION),
                    definition: json!({
                        "id": "widget-get-http",
                        "kind": "http",
                        "run-deadline-ms": 30000
                    }),
                    auth_policy: json!({"modes": ["pat"]}),
                    registered_operation: Some("platform-fixture-overlay:widget/get@3.0.0".into()),
                },
            ),
            (
                "orders-http".into(),
                ServingAttachment {
                    kind: AttachmentKind::Http,
                    package_id: "platform_fixture_overlay".into(),
                    target: AttachmentTarget::Wiring {
                        wiring_id: "orders".into(),
                        wiring_version: 1,
                    },
                    definition_hash: definition_hash(DEFINITION),
                    definition: json!({
                        "id": "orders-http",
                        "kind": "http",
                        "run-deadline-ms": 30000
                    }),
                    auth_policy: json!({"modes": ["pat"]}),
                    registered_operation: Some("platform-fixture-overlay:widget/get@3.0.0".into()),
                },
            ),
        ]),
        BTreeMap::from([(
            "platform_fixture::orders-changed".into(),
            ServingRegistration {
                package_id: "platform_fixture".into(),
                source_package_id: "platform_fixture".into(),
                wiring_id: "shipping".into(),
                wiring_version: 2,
                entity: "orders".into(),
                ops: BTreeSet::from(["insert".into(), "update".into()]),
                input: ServingRegistrationInput::Batch,
            },
        )]),
    )
    .expect("the format-three fixture is valid")
}

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("a JSON object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

#[test]
fn the_format_three_preimage_and_digest_are_pinned() {
    let expected = manifest();
    assert_eq!(expected.canonical_bytes(), mint_vector::CANONICAL_BYTES);
    assert_eq!(expected.digest().as_str(), mint_vector::DIGEST);

    let (read, digest) = ServingManifest::from_canonical_bytes(mint_vector::CANONICAL_BYTES)
        .expect("the format-three vector is admitted by the reader");
    assert_eq!(read, expected);
    assert_eq!(digest.as_str(), mint_vector::DIGEST);

    let raw = <sha2::Sha256 as sha2::Digest>::digest(mint_vector::CANONICAL_BYTES);
    let hex: String = raw.iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write as _;
        write!(out, "{byte:02x}").expect("writing to a string is infallible");
        out
    });
    assert_eq!(digest.as_str(), format!("sha256:{hex}"));
}

#[test]
fn fresh_only_is_digest_bound_without_changing_format_three_default_bytes() {
    let baseline = manifest();
    let mut fresh = baseline.clone();
    fresh.components = fresh
        .components
        .into_iter()
        .map(|mut component| {
            for operation in component.operations.values_mut() {
                if operation.registered_operation.is_some() {
                    operation.fresh_only = true;
                }
            }
            component
        })
        .collect();
    let (admitted, digest) = ServingManifest::from_canonical_bytes(&fresh.canonical_bytes())
        .expect("format-three reader admits registered fresh-only operations");
    assert_eq!(admitted, fresh);
    assert_eq!(admitted.format_version, 3);
    assert_ne!(digest, baseline.digest());
    assert_eq!(baseline.canonical_bytes(), mint_vector::CANONICAL_BYTES);
}

#[test]
fn fresh_only_release_reader_refuses_unregistered_exports_and_non_boolean_flags() {
    let mut unregistered = manifest();
    unregistered.components = unregistered
        .components
        .into_iter()
        .map(|mut component| {
            for operation in component.operations.values_mut() {
                if operation.registered_operation.is_none() {
                    operation.fresh_only = true;
                }
            }
            component
        })
        .collect();
    let error = ServingManifest::from_canonical_bytes(&unregistered.canonical_bytes())
        .expect_err("unregistered palette export cannot require fresh authentication");
    assert!(error.to_string().contains("requires no permission"));

    for value in [json!(null), json!("true"), json!(1)] {
        let mut document = serde_json::to_value(manifest()).unwrap();
        let operation = document["components"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .flat_map(|component| {
                component["operations"]
                    .as_object_mut()
                    .unwrap()
                    .values_mut()
            })
            .find(|operation| operation.get("registered-operation").is_some())
            .unwrap();
        operation["fresh-only"] = value;
        let bytes = wamn_execution_contract::canonical_json_bytes(&document);
        assert!(ServingManifest::from_canonical_bytes(&bytes).is_err());
    }
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
    ] {
        let policy = json!({"modes": modes});
        let parsed = parse_attachment_auth_policy(&policy).expect("supported modes");
        assert_eq!(parsed, expected);
        assert_eq!(parsed.allows_pat(), allows_pat);
        assert_eq!(parsed.allows_session(), allows_session);

        let mut candidate = manifest();
        let attachment = candidate
            .workflow
            .attachments
            .get_mut("orders-http")
            .unwrap();
        attachment.auth_policy = policy.clone();
        if parsed == AttachmentAuthPolicy::None {
            attachment.registered_operation = None;
        }
        let bytes = candidate.canonical_bytes();
        let (admitted, digest) = ServingManifest::from_canonical_bytes(&bytes)
            .expect("canonical policy survives the release reader");
        assert_eq!(
            admitted.workflow.attachments["orders-http"].auth_policy,
            policy
        );
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
            .workflow
            .attachments
            .get_mut("orders-http")
            .unwrap()
            .auth_policy = policy;
        assert_eq!(
            ServingManifest::from_canonical_bytes(&candidate.canonical_bytes()),
            Err(CatalogIdentityError::InvalidAttachmentAuthPolicy {
                attachment_id: "orders-http".into(),
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
/// package id, as `crates/control/lib/src/push_release_manifest.rs:529` shows
/// with `http-request` under `"package-id":"orders"`. Deriving the id from the
/// namespace produced the reserved id `wamn`, which is not a manifest any
/// publisher can mint.
fn operation_provider_manifest(package: &str, export: &str, version: &str) -> ServingManifest {
    let operation = ServingComponentOperation {
        pre_commit: None,
        registered_operation: None,
        fresh_only: false,
        committed_result_schema: None,
        permissions: BTreeSet::new(),
        participant: None,
        statements: BTreeMap::new(),
    };
    ServingManifest::new(
        ServingRelease {
            tenant_id: "provider-tenant".into(),
            effective_release_id: EffectiveReleaseId::new(1).expect("nonzero release"),
            environment: "test".into(),
            packages: BTreeSet::from([
                PackageCoordinate::new(package, version).unwrap(),
                PackageCoordinate::new("consumer", "1.0.0").unwrap(),
            ]),
        },
        BTreeSet::from([
            ServingComponent {
                package_id: package.into(),
                component: "provider".into(),
                interface_version: "0.1.0".into(),
                digest: artifact_hash(COMPONENT_A),
                operations: BTreeMap::from([(
                    export.into(),
                    ServingComponentOperation {
                        // Registered only when `package` owns the export's
                        // namespace, which is the rule
                        // `validate_canonical_operation_for_package` enforces. A
                        // platform interface is exported as a palette operation,
                        // and a palette operation carries no registered
                        // operation.
                        registered_operation: export
                            .starts_with(&format!("{package}:"))
                            .then(|| export.to_owned()),
                        permissions: export
                            .starts_with(&format!("{package}:"))
                            .then(|| export.to_owned())
                            .into_iter()
                            .collect(),
                        ..operation.clone()
                    },
                )]),
            },
            ServingComponent {
                package_id: "consumer".into(),
                component: "consumer".into(),
                interface_version: "0.1.0".into(),
                digest: artifact_hash(COMPONENT_B),
                operations: BTreeMap::from([("consumer:entry/run@1.0.0".into(), operation)]),
            },
        ]),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeMap::new(),
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
            .find(|component| component.component == "provider")
            .expect("fixture provider")
            .clone();
        other.component = "another-provider".into();
        other.digest = artifact_hash(COMPONENT_B);
        let operation = other.operations.get_mut(export).unwrap();
        operation.registered_operation = None;
        operation.permissions.clear();
        assert!(candidate.components.insert(other));
        let (admitted, _) = ServingManifest::from_canonical_bytes(&candidate.canonical_bytes())
            .expect("the host can address duplicate export-only interfaces directly");
        assert_eq!(admitted, candidate);
    }
}

#[test]
fn every_manifest_hash_validates_during_deserialization() {
    let invalid = "sha256:not-a-canonical-digest";

    let mut component_document = serde_json::to_value(manifest()).expect("manifest serializes");
    component_document["components"][0]["digest"] = json!(invalid);
    let component_error = serde_json::from_value::<ServingManifest>(component_document)
        .expect_err("an invalid artifact hash must be refused while decoding");
    assert!(component_error.to_string().contains("artifact-hash"));

    let mut wiring_document = serde_json::to_value(manifest()).expect("manifest serializes");
    wiring_document["workflow"]["wirings"][0]["graph-hash"] = json!(invalid);
    let wiring_error = serde_json::from_value::<ServingManifest>(wiring_document)
        .expect_err("an invalid wiring definition hash must be refused while decoding");
    assert!(wiring_error.to_string().contains("definition-hash"));

    let mut attachment_document = serde_json::to_value(manifest()).expect("manifest serializes");
    attachment_document["workflow"]["attachments"]["orders-http"]["definition-hash"] =
        json!(invalid);
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
        wirings().into_iter().rev().collect(),
        baseline
            .every_attachment()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|(id, item)| (id.to_owned(), ServingAttachment::from(item)))
            .collect(),
        baseline
            .workflow
            .registrations
            .iter()
            .rev()
            .map(|(id, item)| (id.clone(), item.clone()))
            .collect(),
    )
    .expect("permuted facts remain valid");

    assert_eq!(baseline.canonical_bytes(), permuted.canonical_bytes());
    assert_eq!(baseline.digest(), permuted.digest());
}

#[test]
fn every_manifest_field_is_pinned() {
    let document = serde_json::to_value(manifest()).expect("manifest serializes");
    assert_eq!(
        sorted_keys(&document),
        [
            "attachments",
            "components",
            "format-version",
            "release",
            "routes",
            "workflow"
        ]
    );
    assert_eq!(
        sorted_keys(&document["workflow"]),
        ["attachments", "registrations", "wirings"]
    );
    assert_eq!(
        sorted_keys(&document["routes"][0]),
        ["component", "kind", "operation", "package-id"]
    );
    assert_eq!(
        sorted_keys(&document["attachments"]["widget-get-http"]),
        [
            "auth-policy",
            "component",
            "definition",
            "definition-hash",
            "kind",
            "operation",
            "package-id",
            "registered-operation"
        ]
    );
    assert_eq!(
        sorted_keys(&document["components"][0]),
        [
            "component",
            "digest",
            "interface-version",
            "operations",
            "package-id",
        ]
    );
    assert_eq!(
        sorted_keys(&document["workflow"]["wirings"][0]),
        ["graph-hash", "package-id", "wiring-id", "wiring-version"]
    );
    assert_eq!(
        sorted_keys(&document["workflow"]["attachments"]["orders-http"]),
        [
            "auth-policy",
            "definition",
            "definition-hash",
            "kind",
            "package-id",
            "registered-operation",
            "wiring-id",
            "wiring-version"
        ]
    );
    assert_eq!(
        sorted_keys(&document["workflow"]["registrations"]["platform_fixture::orders-changed"]),
        [
            "entity",
            "input",
            "ops",
            "package-id",
            "source-package-id",
            "wiring-id",
            "wiring-version"
        ]
    );

    let text = String::from_utf8(manifest().canonical_bytes()).expect("manifest is UTF-8");
    for retired in [
        "flow-id",
        "flows",
        "plan-hash",
        "calls",
        "callable-contract",
        "source-artifact",
        "binding-base-artifact",
    ] {
        assert!(
            !text.contains(retired),
            "retired key {retired} re-entered format 3"
        );
    }
}

#[test]
fn each_exact_target_reaches_the_digest() {
    let baseline = manifest();
    let mut retargeted = manifest();
    let registration = retargeted
        .workflow
        .registrations
        .get_mut("platform_fixture::orders-changed")
        .expect("fixture registration");
    registration.wiring_id = "orders".into();
    registration.wiring_version = 1;

    assert_ne!(baseline.canonical_bytes(), retargeted.canonical_bytes());
    assert_ne!(baseline.digest(), retargeted.digest());

    let mut regrained = manifest();
    regrained
        .workflow
        .registrations
        .get_mut("platform_fixture::orders-changed")
        .expect("fixture registration")
        .input = ServingRegistrationInput::Event;
    assert_ne!(baseline.digest(), regrained.digest());
}
