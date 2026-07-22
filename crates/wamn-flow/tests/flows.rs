//! Integration tests over the canonical example flows (S3 + POC F1/F3/F4):
//! import round-trips, structural validation passes, each flow conforms to the
//! published JSON Schema, the committed schema matches the types, and the diff
//! detects real changes.

use std::path::{Path, PathBuf};

use boon::{Compiler, Schemas};
use wamn_flow::Flow;

const FIXTURES: &[&str] = &[
    "s3-demo.flow.json",
    "f1-receipt-received.flow.json",
    "f3-escalate-stale-holds.flow.json",
    "f4-disposition-recorded.flow.json",
];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load(name: &str) -> (String, Flow) {
    let raw = std::fs::read_to_string(fixture_dir().join(name)).expect("read fixture");
    let flow = Flow::from_json(&raw).unwrap_or_else(|e| panic!("{name} parses: {e}"));
    (raw, flow)
}

#[test]
fn fixtures_parse_and_validate() {
    for name in FIXTURES {
        let (_, flow) = load(name);
        assert!(
            flow.is_valid(),
            "{name} should validate; issues: {:?}",
            flow.issues()
        );
        // No warnings either — the example flows are clean (no dead nodes).
        assert!(
            flow.issues().is_empty(),
            "{name} has unexpected issues: {:?}",
            flow.issues()
        );
    }
}

#[test]
fn fixtures_round_trip() {
    for name in FIXTURES {
        let (_, flow) = load(name);
        let reparsed = Flow::from_json(&flow.to_json()).expect("re-parse export");
        assert_eq!(flow, reparsed, "{name} does not round-trip");
    }
}

#[test]
fn fixtures_conform_to_published_schema() {
    // The language-neutral contract must accept every example flow — this ties
    // docs/flow-schema.schema.json to the real flows the editor/SDK will send.
    let schema = wamn_flow::json_schema();
    let mut compiler = Compiler::new();
    compiler
        .add_resource("mem://flow-schema.json", schema)
        .expect("add schema resource");
    let mut schemas = Schemas::new();
    let sch = compiler
        .compile("mem://flow-schema.json", &mut schemas)
        .expect("compile schema");

    for name in FIXTURES {
        let raw = std::fs::read_to_string(fixture_dir().join(name)).expect("read fixture");
        let instance: serde_json::Value = serde_json::from_str(&raw).expect("fixture is json");
        if let Err(e) = schemas.validate(&instance, sch) {
            panic!("{name} does not conform to the published schema:\n{e}");
        }
    }
}

#[test]
fn committed_schema_matches_types() {
    // Drift guard: regenerate with
    //   cargo run -p wamn-flow --example print-schema > docs/flow-schema.schema.json
    let committed = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/flow-schema.schema.json"),
    )
    .expect("read committed schema");
    assert_eq!(
        committed,
        wamn_flow::json_schema_string(),
        "docs/flow-schema.schema.json is stale — regenerate it (see print-schema example)"
    );
}

#[test]
fn diff_detects_changes() {
    let (_, v1) = load("f1-receipt-received.flow.json");

    let mut v2 = v1.clone();
    v2.version = 2;
    // 1) change a node's config
    v2.nodes
        .iter_mut()
        .find(|n| n.id == "evaluate")
        .unwrap()
        .config = serde_json::json!({ "compare": "exact-decimal", "tolerance": true });
    // 2) add a node + edge
    v2.nodes.push(wamn_flow::Node {
        id: "audit".into(),
        node_type: "custom".into(),
        label: None,
        config: serde_json::json!({}),
        credential: None,
    });
    v2.edges.push(wamn_flow::Edge {
        from: "holds".into(),
        from_port: "main".into(),
        to: "audit".into(),
        to_port: None,
    });
    // 3) declare a credential
    v2.credentials.push(wamn_flow::CredentialRef {
        name: "audit-sink".into(),
        kind: None,
        description: None,
    });

    let d = wamn_flow::diff(&v1, &v2);
    assert!(!d.is_empty());
    assert!(d.nodes_added.contains(&"audit".to_string()));
    assert!(d.nodes_removed.is_empty());
    assert!(
        d.nodes_changed
            .iter()
            .any(|c| c.id == "evaluate" && c.config_changed)
    );
    assert!(d.edges_added.iter().any(|e| e.to == "audit"));
    assert!(d.credentials_added.contains(&"audit-sink".to_string()));

    // A flow diffed against itself is empty.
    assert!(wamn_flow::diff(&v1, &v1).is_empty());
}

/// F3 `escalate-stale-holds` drains ALL stale holds through a STRUCTURAL cycle,
/// not a single hold. The design (POC F3, wamn-24i):
///
/// - `shift` (time-shift, entry) computes `cutoff = fire-at-ms − 48h` ONCE — the
///   arithmetic JMESPath lacks. The cutoff is consumed by the `list` filter and
///   never needs to survive the cycle.
/// - `list-stale` (postgres list) captures the stale open holds (opened_at <
///   cutoff) as an in-memory array, oldest first, capped at `limit`.
/// - the loop `gate → advance → gate` walks that array: `gate` (conditional,
///   pass-through) tests `length(@) > 0`; on the TRUE port it fans out to
///   `escalate` (mark head hold escalated) and to `advance` (transform `[1:]`,
///   the tail) which loops back to `gate`. `escalate → notify` is a DEAD-END
///   branch — the loop state (the array) lives only on the pass-through /
///   transform edges, so the destructive `escalate`/`notify`/`list` node
///   outputs never clobber it. FALSE port ends at `done`.
///
/// Practical bound: ~4 dispatches/hold (gate, escalate, advance, notify) under
/// the engine's 10k per-run dispatch budget, and `list.limit = 500` — so one
/// nightly run drains up to 500 stale holds; a larger backlog drains across
/// successive nights (each run escalates the oldest 500). The cycle proves the
/// engine's per-visit occurrence tracking (R24) on the deployed runner.
#[test]
fn f3_escalate_stale_holds_shape() {
    use wamn_flow::Trigger;
    let (_, f) = load("f3-escalate-stale-holds.flow.json");

    assert!(
        matches!(f.trigger, Trigger::Cron { .. }),
        "F3 is cron-triggered"
    );
    assert_eq!(f.entry, "shift", "entry computes the cutoff first");

    // Egress + credential are DECLARED (fail-closed capability-by-declaration).
    assert_eq!(f.allowed_hosts, vec!["notify.example".to_string()]);
    assert!(
        f.credentials.iter().any(|c| c.name == "notify-webhook"),
        "the webhook credential is declared"
    );
    assert_eq!(
        f.nodes
            .iter()
            .find(|n| n.id == "notify")
            .and_then(|n| n.credential.clone()),
        Some("notify-webhook".to_string()),
        "notify references the declared credential"
    );

    // The STRUCTURAL cycle: advance loops back to the gate (a real cycle, not a
    // rejected self-loop), and the gate fans out to both escalate and advance
    // on its TRUE port.
    assert!(
        f.edges
            .iter()
            .any(|e| e.from == "advance" && e.to == "gate"),
        "advance closes the cycle back to the gate"
    );
    let true_targets: Vec<&str> = f
        .edges
        .iter()
        .filter(|e| e.from == "gate" && e.from_port == "true")
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        true_targets.contains(&"escalate") && true_targets.contains(&"advance"),
        "gate.true fans out to escalate AND the loop advance; got {true_targets:?}"
    );

    // notify is the dead-end of the escalate branch — it carries no loop state.
    assert!(
        !f.edges.iter().any(|e| e.from == "notify"),
        "notify is a dead-end (its output never re-enters the loop)"
    );

    // The list filter is BOTH predicates: status = open (so disposed/escalated
    // holds are never re-escalated — mutant ii) AND opened_at < the cutoff (so
    // fresh holds are left alone). Dropping either is a real escalation bug.
    let list = f
        .nodes
        .iter()
        .find(|n| n.id == "list-stale")
        .expect("list-stale node");
    let filters = &list.config["filters"];
    assert_eq!(
        filters["status"], "eq.open",
        "list must filter status = open"
    );
    assert_eq!(
        filters["opened_at"], "lt.{{cutoff}}",
        "list must filter opened_at < the computed cutoff"
    );
}
