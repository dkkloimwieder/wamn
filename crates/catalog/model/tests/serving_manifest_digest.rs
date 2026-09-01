//! Digest and closed-shape proofs for serving-manifest format 3.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use wamn_catalog::{
    ArtifactHash, AttachmentKind, DefinitionHash, EffectiveReleaseId, PackageCoordinate,
    ServingAttachment, ServingComponent, ServingManifest, ServingRegistration,
    ServingRegistrationInput, ServingRelease, ServingWiring,
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
            registered_operation: None,
        },
        ServingComponent {
            package_id: "client_acme_receiving".into(),
            component: "http-request".into(),
            interface_version: "0.1".into(),
            digest: artifact_hash(COMPONENT_A),
            registered_operation: Some("client_acme_receiving@3.0.0::purchase_order.get".into()),
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
                auth_policy: json!({"mode": "pat"}),
                registered_operation: Some(
                    "client_acme_receiving@3.0.0::purchase_order.get".into(),
                ),
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
        .expect("the v3 vector is admitted by the reader");
    assert_eq!(read, expected);
    assert_eq!(digest.as_str(), mint_vector::DIGEST);

    let raw = <sha2::Sha256 as sha2::Digest>::digest(mint_vector::CANONICAL_BYTES);
    let hex: String = raw.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(digest.as_str(), format!("sha256:{hex}"));
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
            "package-id",
            "registered-operation"
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
            "retired key {retired} re-entered v3"
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
