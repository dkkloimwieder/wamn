//! Digest-ordering proofs for the mounted serving manifest.
//!
//! The manifest preimage is [`ServingManifest::canonical_bytes`] (RFC 8785) and
//! its identity is [`ServingManifest::digest`]. This file covers the clauses the
//! six consumer beads depend on: identical content mints identical bytes and
//! digest irrespective of map or document key order, and the frozen field set is
//! stated explicitly so a field cannot enter or leave the digest silently.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use wamn_catalog::{
    AttachmentKind, CALLABLE_CONTRACT_VERSION, CallableContract, CallableEffectCeiling,
    CallableReturnContract, ServingAttachment, ServingFlow, ServingManifest, ServingRegistration,
    ServingRelease,
};

mod mint_vector {
    include!("fixtures/release_manifest_mint_vector.rs");
}

const PLAN: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ART: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const DEF: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const GUARD: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
/// A released base that is *not* the flow's own artifact — the candidate
/// overlay, the only case in which the two artifact hashes diverge.
const BASE: &str = "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn release() -> ServingRelease {
    ServingRelease {
        tenant_id: "t1".to_string(),
        catalog_id: "cat".to_string(),
        catalog_version: 7,
        environment: "prod".to_string(),
    }
}

/// A released member — the ordinary case, in which the binding base is the
/// flow's own artifact.
fn flow(calls: BTreeSet<String>) -> ServingFlow {
    ServingFlow {
        flow_version: 3,
        plan_hash: PLAN.to_string(),
        source_artifact: ART.to_string(),
        binding_base_artifact: ART.to_string(),
        callable_contract: None,
        calls,
    }
}

/// The smallest valid manifest: one member flow, no exposure, no registrations.
fn minimal() -> ServingManifest {
    ServingManifest::new(
        release(),
        BTreeMap::from([("root".to_string(), flow(BTreeSet::new()))]),
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .expect("the minimal manifest is valid")
}

/// Every projection populated, so serializing skips nothing.
fn maximal() -> ServingManifest {
    let mut callee = flow(BTreeSet::from(["root".to_string()]));
    callee.flow_version = 1;
    // A candidate overlay, so the maximal fixture exercises the diverging case.
    callee.binding_base_artifact = BASE.to_string();
    callee.callable_contract = Some(CallableContract {
        version: CALLABLE_CONTRACT_VERSION.to_string(),
        input_schema_hash: GUARD.to_string(),
        return_contract: CallableReturnContract::UntypedJsonBody,
        effect_ceiling: CallableEffectCeiling::Effectful,
    });
    ServingManifest::new(
        release(),
        BTreeMap::from([
            (
                "root".to_string(),
                flow(BTreeSet::from(["callee".to_string()])),
            ),
            ("callee".to_string(), callee),
        ]),
        BTreeMap::from([(
            "orders".to_string(),
            ServingAttachment {
                kind: AttachmentKind::Http,
                flow_id: "root".to_string(),
                definition_hash: DEF.to_string(),
                definition: json!({
                    "id": "orders",
                    "kind": "http",
                    "flow-id": "root",
                    "source-id": "public",
                    "route": {"host": "*", "path": "/orders", "method": "POST"},
                    "run-deadline-ms": 30000
                }),
                auth_policy: json!({"mode": "none"}),
            },
        )]),
        BTreeMap::from([(
            "orders-changed".to_string(),
            ServingRegistration {
                flow_id: "callee".to_string(),
                entity: "orders".to_string(),
                ops: BTreeSet::from(["insert".to_string()]),
            },
        )]),
    )
    .expect("the maximal manifest is valid")
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

/// Drift guard for the synthetic minimal serializer-shape fixture.
///
/// Any silent projection change — a field entering or leaving it, or a key
/// spelling moving — fails with a readable diff. This hand-built fixture is not
/// a mintable producer vector; the shared live-mint vector below exclusively
/// owns producer-reader identity coupling.
#[test]
fn manifest_preimage_bytes_are_pinned() {
    assert_eq!(
        String::from_utf8(minimal().canonical_bytes()).expect("the preimage is UTF-8"),
        concat!(
            r#"{"attachments":{},"#,
            r#""flows":{"root":{"#,
            r#""binding-base-artifact":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","#,
            r#""callable-contract":null,"calls":[],"flow-version":3,"#,
            r#""plan-hash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","#,
            r#""source-artifact":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}},"#,
            r#""format-version":"0.1","#,
            r#""registrations":{},"#,
            r#""release":{"catalog-id":"cat","catalog-version":7,"environment":"prod","tenant-id":"t1"}}"#,
        ),
        "manifest keys are UTF-16 ordered and no field is omitted"
    );
}

/// The golden vector, reader side: the real mint's canonical bytes and digest.
///
/// The mint-side live test consumes the same fixture and proves that the real
/// producer emits it. This side admits those bytes through the only reader entry
/// point and derives their name, preserving producer-coupled identity rather
/// than pinning a hand-built manifest the producer cannot mint.
#[test]
fn the_golden_vector_digest_is_pinned() {
    let (manifest, digest) = ServingManifest::from_canonical_bytes(mint_vector::CANONICAL_BYTES)
        .expect("the real mint's vector is admitted by the reader");
    assert_eq!(
        digest.as_str(),
        mint_vector::DIGEST,
        "the reader no longer names the real mint's canonical vector identically"
    );
    assert_eq!(manifest.digest(), digest);
    // And the identity is a plain SHA-256 over those bytes with NO framing. This
    // crate's other hashing path deliberately frames its input with a domain
    // tag (`wamn.catalog.identity.v0.1`), so routing the manifest through it —
    // the exact silent fork the producer-coupling rider forbids — would break
    // here rather than quietly renaming every release.
    let raw = <sha2::Sha256 as sha2::Digest>::digest(mint_vector::CANONICAL_BYTES);
    let hex: String = raw.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(digest.as_str(), format!("sha256:{hex}"));
}

/// The digest is taken over the projection, not over the bytes it arrived in:
/// two documents whose keys are written in opposite orders are the same
/// manifest. `read` is what refuses the non-canonical encoding at the mount;
/// `digest` must agree with itself before that refusal can mean anything.
#[test]
fn document_key_order_cannot_change_the_digest() {
    const FORWARD: &str = r#"{
      "format-version": "0.1",
      "release": {
        "tenant-id": "t1", "catalog-id": "cat",
        "catalog-version": 7, "environment": "prod"
      },
      "flows": {
        "root": {
          "flow-version": 3,
          "plan-hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          "source-artifact": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "binding-base-artifact": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "callable-contract": null,
          "calls": []
        }
      },
      "attachments": {},
      "registrations": {}
    }"#;
    const REVERSED: &str = r#"{
      "registrations": {},
      "attachments": {},
      "flows": {
        "root": {
          "calls": [],
          "callable-contract": null,
          "binding-base-artifact": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "source-artifact": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          "plan-hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          "flow-version": 3
        }
      },
      "release": {
        "environment": "prod", "catalog-version": 7,
        "catalog-id": "cat", "tenant-id": "t1"
      },
      "format-version": "0.1"
    }"#;

    let forward: ServingManifest = serde_json::from_str(FORWARD).expect("the document parses");
    let reversed: ServingManifest = serde_json::from_str(REVERSED).expect("the document parses");

    assert_eq!(forward, minimal());
    assert_eq!(forward.canonical_bytes(), reversed.canonical_bytes());
    assert_eq!(forward.digest(), reversed.digest());
    assert_eq!(forward.digest(), minimal().digest());
}

/// Map insertion order is not identity either, at every projection.
#[test]
fn map_insertion_order_cannot_change_the_digest() {
    let baseline = maximal();
    let mut permuted = maximal();
    let reversed: BTreeMap<String, ServingFlow> = permuted
        .flows
        .iter()
        .rev()
        .map(|(flow_id, flow)| (flow_id.clone(), flow.clone()))
        .collect();
    permuted.flows = reversed;

    assert_eq!(baseline.canonical_bytes(), permuted.canonical_bytes());
    assert_eq!(baseline.digest(), permuted.digest());
    assert!(baseline.digest().as_str().starts_with("sha256:"));
}

/// The classification guard behind the projection. A field added to any manifest
/// type still compiles while nobody notices it reached the digest — these pinned
/// key sets are what make the frozen shape a decision rather than an accident.
#[test]
fn every_manifest_field_is_pinned_in_the_projection() {
    let document = serde_json::to_value(maximal()).expect("the manifest serializes");

    assert_eq!(
        sorted_keys(&document),
        [
            "attachments",
            "flows",
            "format-version",
            "registrations",
            "release"
        ],
        "a new `ServingManifest` field must be classified here"
    );
    assert_eq!(
        sorted_keys(&document["release"]),
        ["catalog-id", "catalog-version", "environment", "tenant-id"],
        "release identity is the (tenant, catalog, version) triple plus the environment"
    );
    assert_eq!(
        sorted_keys(&document["flows"]["callee"]),
        [
            "binding-base-artifact",
            "callable-contract",
            "calls",
            "flow-version",
            "plan-hash",
            "source-artifact"
        ],
        "the per-flow projection is plan-hash / source-artifact / binding-base-artifact / \
         callable-contract, plus the call-edge adjacency and the flow version the run row needs"
    );
    assert_eq!(
        document["flows"]["callee"]["binding-base-artifact"], BASE,
        "the binding base carries its own value: the plan verifiers read source-artifact, \
         binding resolution reads this, and a candidate's two hashes differ"
    );
    assert_eq!(
        document["flows"]["root"]["binding-base-artifact"],
        document["flows"]["root"]["source-artifact"],
        "a released member's binding base is its own artifact — the mint-side default"
    );
    assert_eq!(
        sorted_keys(&document["attachments"]["orders"]),
        [
            "auth-policy",
            "definition",
            "definition-hash",
            "flow-id",
            "kind"
        ],
        "the attachment projection is release shape only — activation is environment \
         state, checked at admission, and never in these bytes"
    );
    assert_eq!(document["attachments"]["orders"]["kind"], "http");
    assert_eq!(
        sorted_keys(&document["registrations"]["orders-changed"]),
        ["entity", "flow-id", "ops"],
        "a registration travels as identity only — the condition stays a hot column, \
         read by the materializer at evaluation"
    );
}

/// The binding base is manifest identity in its own right. Two releases that
/// agree on every plan and every source artifact but resolve their bindings
/// under different bases are different documents, so a pod cannot serve one
/// under the other's digest. This is the clause a `#[serde(skip)]`,
/// a `skip_serializing_if`, or a projection that echoed `source-artifact` into
/// both keys would break: each of those collapses the two digests below.
#[test]
fn the_binding_base_reaches_the_digest() {
    let released = minimal();
    let mut candidate = minimal();
    candidate
        .flows
        .get_mut("root")
        .expect("the fixture flow")
        .binding_base_artifact = BASE.to_string();

    assert_ne!(released.canonical_bytes(), candidate.canonical_bytes());
    assert_ne!(released.digest(), candidate.digest());

    // The republish-safety property, stated against the derived identity rather
    // than against a comparison parameter: a rebased manifest is admitted — it is
    // valid content — but it derives its OWN name, so it can never be served
    // under the released one. A shifted digest is not a masquerade; it is a
    // correct name for different content.
    let (_, derived) = ServingManifest::from_canonical_bytes(&candidate.canonical_bytes())
        .expect("a rebased manifest is valid content in its own right");
    assert_eq!(derived, candidate.digest());
    assert_ne!(
        derived,
        released.digest(),
        "a rebased manifest may not mount under the identity it was not minted with"
    );
}

/// The key is required, exactly like `callable-contract`: absence is a refusal,
/// never a silent fall back to `source-artifact`. A reader-side default would
/// recreate the double-duty the field exists to end — the mint decides which
/// case a flow is in, and the wire records that decision.
#[test]
fn a_flow_without_a_binding_base_is_refused_at_the_mount() {
    let mut document = serde_json::to_value(minimal()).expect("the manifest serializes");
    document["flows"]["root"]
        .as_object_mut()
        .expect("a flow is an object")
        .remove("binding-base-artifact");
    assert!(serde_json::from_value::<ServingManifest>(document).is_err());

    let mut null_base = serde_json::to_value(minimal()).expect("the manifest serializes");
    null_base["flows"]["root"]["binding-base-artifact"] = Value::Null;
    assert!(serde_json::from_value::<ServingManifest>(null_base).is_err());
}

/// The two fields the release-shape ruling removed. `deny_unknown_fields` is what
/// keeps them out for good: a mount carrying either is refused rather than
/// silently ignored, so a manifest minted by a pre-ruling writer cannot serve.
#[test]
fn the_removed_environment_state_is_refused_at_the_mount() {
    let mut with_enabled = serde_json::to_value(maximal()).expect("the manifest serializes");
    with_enabled["attachments"]["orders"]["enabled"] = json!(true);
    assert!(serde_json::from_value::<ServingManifest>(with_enabled).is_err());

    let mut with_condition = serde_json::to_value(maximal()).expect("the manifest serializes");
    with_condition["registrations"]["orders-changed"]["condition"] = json!("new.total > `10`");
    assert!(serde_json::from_value::<ServingManifest>(with_condition).is_err());
}
