//! Digest and closed-shape tests for serving-manifest format 1.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use wamn_catalog::{
    ArtifactHash, AttachmentAuthPolicy, AttachmentKind, CatalogIdentityError,
    ComponentOperationDependency, DefinitionHash, EffectiveReleaseId, PackageCoordinate,
    ServingAttachment, ServingComponent, ServingComponentOperation, ServingManifest,
    ServingRegistration, ServingRegistrationInput, ServingRelease, ServingWiring,
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
            PackageCoordinate::new("wamn_receiving", "1.0.0").unwrap(),
            PackageCoordinate::new("client_acme_receiving", "3.0.0").unwrap(),
        ]),
    }
}

fn components() -> BTreeSet<ServingComponent> {
    BTreeSet::from([
        ServingComponent {
            package_id: "wamn_receiving".into(),
            component: "transform".into(),
            interface_version: "0.1".into(),
            digest: artifact_hash(COMPONENT_B),
            operations: BTreeMap::from([(
                "map".into(),
                ServingComponentOperation {
                    committed_result_schema: None,
                    fresh_only: false,
                    registered_operation: None,
                    dependencies: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
        },
        ServingComponent {
            package_id: "client_acme_receiving".into(),
            component: "http-request".into(),
            interface_version: "0.1".into(),
            digest: artifact_hash(COMPONENT_A),
            operations: BTreeMap::from([(
                "client-acme-receiving:purchase-order/get@3.0.0".into(),
                ServingComponentOperation {
                    committed_result_schema: None,
                    fresh_only: false,
                    registered_operation: Some(
                        "client-acme-receiving:purchase-order/get@3.0.0".into(),
                    ),
                    dependencies: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
        },
    ])
}

fn wirings() -> BTreeSet<ServingWiring> {
    BTreeSet::from([
        ServingWiring {
            package_id: "wamn_receiving".into(),
            wiring_id: "shipping".into(),
            wiring_version: 2,
            graph_hash: definition_hash(GRAPH_B),
        },
        ServingWiring {
            package_id: "client_acme_receiving".into(),
            wiring_id: "orders".into(),
            wiring_version: 1,
            graph_hash: definition_hash(GRAPH_A),
        },
    ])
}

fn manifest() -> ServingManifest {
    ServingManifest::new(
        release(),
        components(),
        wirings(),
        BTreeMap::from([(
            "orders-http".into(),
            ServingAttachment {
                kind: AttachmentKind::Http,
                package_id: "client_acme_receiving".into(),
                wiring_id: "orders".into(),
                wiring_version: 1,
                definition_hash: definition_hash(DEFINITION),
                definition: json!({
                    "id": "orders-http",
                    "kind": "http",
                    "run-deadline-ms": 30000
                }),
                auth_policy: json!({"modes": ["pat"]}),
                registered_operation: Some("client-acme-receiving:purchase-order/get@3.0.0".into()),
            },
        )]),
        BTreeMap::from([(
            "wamn_receiving::orders-changed".into(),
            ServingRegistration {
                package_id: "wamn_receiving".into(),
                source_package_id: "wamn_receiving".into(),
                wiring_id: "shipping".into(),
                wiring_version: 2,
                entity: "orders".into(),
                ops: BTreeSet::from(["insert".into(), "update".into()]),
                input: ServingRegistrationInput::Batch,
            },
        )]),
    )
    .expect("the format-one fixture is valid")
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
fn the_format_one_preimage_and_digest_are_pinned() {
    let expected = manifest();
    assert_eq!(expected.canonical_bytes(), mint_vector::CANONICAL_BYTES);
    assert_eq!(expected.digest().as_str(), mint_vector::DIGEST);

    let (read, digest) = ServingManifest::from_canonical_bytes(mint_vector::CANONICAL_BYTES)
        .expect("the v1 vector is admitted by the reader");
    assert_eq!(read, expected);
    assert_eq!(digest.as_str(), mint_vector::DIGEST);

    let raw = <sha2::Sha256 as sha2::Digest>::digest(mint_vector::CANONICAL_BYTES);
    let hex: String = raw.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(digest.as_str(), format!("sha256:{hex}"));
}

#[test]
fn fresh_only_is_digest_bound_without_changing_format_one_default_bytes() {
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
        .expect("format-one reader admits registered fresh-only operations");
    assert_eq!(admitted, fresh);
    assert_eq!(admitted.format_version, 1);
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
    assert!(error.to_string().contains("unregistered export"));

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
        let attachment = candidate.attachments.get_mut("orders-http").unwrap();
        attachment.auth_policy = policy.clone();
        if parsed == AttachmentAuthPolicy::None {
            attachment.registered_operation = None;
        }
        let bytes = candidate.canonical_bytes();
        let (admitted, digest) = ServingManifest::from_canonical_bytes(&bytes)
            .expect("canonical policy survives the release reader");
        assert_eq!(admitted.attachments["orders-http"].auth_policy, policy);
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

fn operation_provider_manifest(export: &str, version: &str) -> ServingManifest {
    let package = export.split_once(':').expect("full interface identity").0;
    let operation = ServingComponentOperation {
        registered_operation: None,
        fresh_only: false,
        committed_result_schema: None,
        dependencies: Vec::new(),
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
                        registered_operation: Some(export.into()),
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
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .expect("one provider and one consumer form a valid manifest")
}

fn add_operation_import(manifest: &mut ServingManifest, export: &str, version: &str) {
    let mut consumer = manifest
        .components
        .iter()
        .find(|component| component.package_id == "consumer")
        .expect("fixture consumer")
        .clone();
    assert!(manifest.components.remove(&consumer));
    consumer
        .operations
        .get_mut("consumer:entry/run@1.0.0")
        .expect("consumer operation")
        .dependencies = vec![ComponentOperationDependency {
        package: export
            .split_once(':')
            .expect("full interface identity")
            .0
            .into(),
        version: version.into(),
        digest: COMPONENT_A.into(),
        operation: export.into(),
    }];
    assert!(manifest.components.insert(consumer));
}

#[test]
fn duplicate_export_only_interfaces_refuse_only_after_the_closure_imports_them() {
    for (export, version) in [
        ("wamn:node/handler@0.1.0", "0.1.0"),
        ("provider:entry/run@1.0.0", "1.0.0"),
    ] {
        let mut candidate = operation_provider_manifest(export, version);
        let mut other = candidate
            .components
            .iter()
            .find(|component| component.component == "provider")
            .expect("fixture provider")
            .clone();
        other.component = "another-provider".into();
        other.digest = artifact_hash(COMPONENT_B);
        other
            .operations
            .get_mut(export)
            .unwrap()
            .registered_operation = None;
        assert!(candidate.components.insert(other));
        let (admitted, _) = ServingManifest::from_canonical_bytes(&candidate.canonical_bytes())
            .expect("the host can address duplicate export-only interfaces directly");
        assert_eq!(admitted, candidate);

        add_operation_import(&mut candidate, export, version);
        let error = ServingManifest::from_canonical_bytes(&candidate.canonical_bytes())
            .expect_err("one selected digest cannot disambiguate an imported interface");
        let detail = error.to_string();
        for expected in [
            "ambiguous component providers",
            export,
            COMPONENT_A,
            COMPONENT_B,
        ] {
            assert!(detail.contains(expected), "{expected}: {detail}");
        }
    }
}

#[test]
fn imported_provider_ambiguity_does_not_deduplicate_coordinates_or_digests() {
    let export = "provider:entry/run@1.0.0";
    for same_digest in [false, true] {
        let mut candidate = operation_provider_manifest(export, "1.0.0");
        add_operation_import(&mut candidate, export, "1.0.0");
        ServingManifest::from_canonical_bytes(&candidate.canonical_bytes())
            .expect("one exact imported provider is admitted");
        let mut other = candidate
            .components
            .iter()
            .find(|component| component.component == "provider")
            .expect("fixture provider")
            .clone();
        if same_digest {
            other.component = "same-bytes-another-provider".into();
        } else {
            other.digest = artifact_hash(COMPONENT_B);
        }
        assert!(candidate.components.insert(other));
        let error = ServingManifest::from_canonical_bytes(&candidate.canonical_bytes())
            .expect_err("each provider remains distinct at native resolution");
        assert!(error.to_string().contains("ambiguous component providers"));
    }
}

#[test]
fn imported_interface_identity_includes_the_complete_version() {
    let export = "provider:entry/run@1.0.0";
    let mut candidate = operation_provider_manifest(export, "1.0.0");
    add_operation_import(&mut candidate, export, "1.0.0");
    let mut other = candidate
        .components
        .iter()
        .find(|component| component.component == "provider")
        .expect("fixture provider")
        .clone();
    other.component = "another-version".into();
    other.digest = artifact_hash(COMPONENT_B);
    let mut operation = other.operations.remove(export).unwrap();
    operation.registered_operation = None;
    other
        .operations
        .insert("provider:entry/run@2.0.0".into(), operation);
    assert!(candidate.components.insert(other));
    let (admitted, _) = ServingManifest::from_canonical_bytes(&candidate.canonical_bytes())
        .expect("a different full interface version is not a competing provider");
    assert_eq!(admitted, candidate);
}

#[test]
fn a_unique_imported_provider_must_still_match_the_exact_dependency_digest() {
    let export = "provider:entry/run@1.0.0";
    let mut candidate = operation_provider_manifest(export, "1.0.0");
    add_operation_import(&mut candidate, export, "1.0.0");
    let mut provider = candidate
        .components
        .iter()
        .find(|component| component.component == "provider")
        .expect("fixture provider")
        .clone();
    assert!(candidate.components.remove(&provider));
    provider.digest = artifact_hash(COMPONENT_B);
    assert!(candidate.components.insert(provider));
    let error = ServingManifest::from_canonical_bytes(&candidate.canonical_bytes())
        .expect_err("one provider cannot replace the pinned artifact provenance");
    let detail = error.to_string();
    assert!(detail.contains("resolves to 0 exact component facts"));
    assert!(detail.contains(COMPONENT_A));
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
    wiring_document["wirings"][0]["graph-hash"] = json!(invalid);
    let wiring_error = serde_json::from_value::<ServingManifest>(wiring_document)
        .expect_err("an invalid wiring definition hash must be refused while decoding");
    assert!(wiring_error.to_string().contains("definition-hash"));

    let mut attachment_document = serde_json::to_value(manifest()).expect("manifest serializes");
    attachment_document["attachments"]["orders-http"]["definition-hash"] = json!(invalid);
    let attachment_error = serde_json::from_value::<ServingManifest>(attachment_document)
        .expect_err("an invalid attachment definition hash must be refused while decoding");
    assert!(attachment_error.to_string().contains("definition-hash"));
}

#[test]
fn all_four_collections_have_canonical_order() {
    let baseline = manifest();
    let permuted = ServingManifest::new(
        release(),
        components().into_iter().rev().collect(),
        wirings().into_iter().rev().collect(),
        baseline
            .attachments
            .iter()
            .rev()
            .map(|(id, item)| (id.clone(), item.clone()))
            .collect(),
        baseline
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
            "registrations",
            "release",
            "wirings"
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
        sorted_keys(&document["wirings"][0]),
        ["graph-hash", "package-id", "wiring-id", "wiring-version"]
    );
    assert_eq!(
        sorted_keys(&document["attachments"]["orders-http"]),
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
        sorted_keys(&document["registrations"]["wamn_receiving::orders-changed"]),
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
            "retired key {retired} re-entered v1"
        );
    }
}

#[test]
fn each_exact_target_reaches_the_digest() {
    let baseline = manifest();
    let mut retargeted = manifest();
    let registration = retargeted
        .registrations
        .get_mut("wamn_receiving::orders-changed")
        .expect("fixture registration");
    registration.wiring_id = "orders".into();
    registration.wiring_version = 1;

    assert_ne!(baseline.canonical_bytes(), retargeted.canonical_bytes());
    assert_ne!(baseline.digest(), retargeted.digest());

    let mut regrained = manifest();
    regrained
        .registrations
        .get_mut("wamn_receiving::orders-changed")
        .expect("fixture registration")
        .input = ServingRegistrationInput::Event;
    assert_ne!(baseline.digest(), regrained.digest());
}
