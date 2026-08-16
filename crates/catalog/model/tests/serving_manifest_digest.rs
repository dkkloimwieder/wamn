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
    CallableReturnContract, ServingAttachment, ServingFlow, ServingManifest, ServingRelease,
};

const PLAN: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ART: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const DEF: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const GUARD: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

fn release() -> ServingRelease {
    ServingRelease {
        tenant_id: "t1".to_string(),
        catalog_id: "cat".to_string(),
        catalog_version: 7,
        environment: "prod".to_string(),
    }
}

fn flow(calls: BTreeSet<String>) -> ServingFlow {
    ServingFlow {
        flow_version: 3,
        plan_hash: PLAN.to_string(),
        source_artifact: ART.to_string(),
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
    callee.callable_contract = Some(CallableContract {
        version: CALLABLE_CONTRACT_VERSION.to_string(),
        input_schema_hash: GUARD.to_string(),
        return_contract: CallableReturnContract::UntypedJsonBody,
        effect_ceiling: CallableEffectCeiling::Effectful,
    });
    ServingManifest::new(
        release(),
        BTreeMap::from([
            ("root".to_string(), flow(BTreeSet::from(["callee".to_string()]))),
            ("callee".to_string(), callee),
        ]),
        BTreeMap::from([(
            "orders".to_string(),
            ServingAttachment {
                kind: AttachmentKind::Http,
                flow_id: "root".to_string(),
                definition_hash: DEF.to_string(),
                enabled: true,
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
            json!({
                "schema-version": "0.1",
                "registration-id": "orders-changed",
                "catalog-id": "cat",
                "flow-id": "callee",
                "entity": "orders",
                "ops": ["insert"]
            }),
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

/// Drift guard: the exact RFC 8785 preimage bytes of the minimal manifest. Any
/// silent change to the projection — a field entering or leaving it, a key
/// spelling moving — fails here with a readable diff. Every already-published
/// manifest digest moves with these bytes; a pod carrying a pre-change identity
/// then fails closed in [`ServingManifest::read`], and republishing the release
/// is the migration story.
#[test]
fn manifest_preimage_bytes_are_pinned() {
    assert_eq!(
        String::from_utf8(minimal().canonical_bytes()).expect("the preimage is UTF-8"),
        concat!(
            r#"{"attachments":{},"#,
            r#""flows":{"root":{"callable-contract":null,"calls":[],"flow-version":3,"#,
            r#""plan-hash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","#,
            r#""source-artifact":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}},"#,
            r#""format-version":"0.1","#,
            r#""registrations":{},"#,
            r#""release":{"catalog-id":"cat","catalog-version":7,"environment":"prod","tenant-id":"t1"}}"#,
        ),
        "manifest keys are UTF-16 ordered and no field is omitted"
    );
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
    assert!(baseline.digest().starts_with("sha256:"));
}

/// The classification guard behind the projection. A field added to any manifest
/// type still compiles while nobody notices it reached the digest — these pinned
/// key sets are what make the frozen shape a decision rather than an accident.
#[test]
fn every_manifest_field_is_pinned_in_the_projection() {
    let document = serde_json::to_value(maximal()).expect("the manifest serializes");

    assert_eq!(
        sorted_keys(&document),
        ["attachments", "flows", "format-version", "registrations", "release"],
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
            "callable-contract",
            "calls",
            "flow-version",
            "plan-hash",
            "source-artifact"
        ],
        "the per-flow projection is plan-hash / source-artifact / callable-contract, plus \
         the call-edge adjacency and the flow version the run row needs"
    );
    assert_eq!(
        sorted_keys(&document["attachments"]["orders"]),
        [
            "auth-policy",
            "definition",
            "definition-hash",
            "enabled",
            "flow-id",
            "kind"
        ],
        "the attachment projection carries exactly what `InvocationTarget` reads"
    );
    assert_eq!(document["attachments"]["orders"]["kind"], "http");
    assert!(
        document["registrations"]["orders-changed"].is_object(),
        "a registration travels as the stored document, decoded by wamn-event-reg"
    );
}
