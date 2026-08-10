//! Integration tests over the canonical example flows (S3 + POC F1/F3/F4):
//! import round-trips, structural validation passes, each flow conforms to the
//! published JSON Schema, the committed schema matches the types, and the diff
//! detects real changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use boon::{Compiler, Schemas};
use serde_json::json;
use wamn_flow::{CronInput, EventInput, Flow, ResolvedInterfaces, RowEvent};

const FIXTURES: &[&str] = &[
    "f0-echo.flow.json",
    "s3-demo.flow.json",
    "f1-receipt-received.flow.json",
    "f2-disposition-recommendation.flow.json",
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

fn interfaces() -> ResolvedInterfaces {
    BTreeMap::from([
        ("conditional".into(), vec!["true".into(), "false".into()]),
        ("evaluate-specs".into(), vec!["main".into()]),
        ("normalize-receipt".into(), vec!["main".into()]),
        ("disposition-recommendation".into(), vec!["main".into()]),
        ("custom".into(), vec!["main".into()]),
        ("http-request".into(), vec!["main".into()]),
        ("invoke-flow".into(), vec!["main".into()]),
        ("pg-write".into(), vec!["main".into()]),
        ("postgres".into(), vec!["main".into()]),
        ("postgres-query".into(), vec!["main".into()]),
        ("time-shift".into(), vec!["main".into()]),
        ("transform".into(), vec!["main".into()]),
    ])
}

#[test]
fn t0_fixtures_parse_and_pass_structural_validation() {
    for name in FIXTURES {
        let (_, flow) = load(name);
        assert!(
            flow.is_valid(&interfaces()),
            "{name} should validate; issues: {:?}",
            flow.issues(&interfaces())
        );
        assert!(
            flow.issues(&interfaces()).is_empty(),
            "{name} has unexpected issues: {:?}",
            flow.issues(&interfaces())
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
    // docs/archive/contracts/flow-schema.schema.json to the real flows the editor/SDK will send.
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
    //   cargo run -p wamn-flow --example print-flow-schema > docs/archive/contracts/flow-schema.schema.json
    let committed = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs/archive/contracts/flow-schema.schema.json"),
    )
    .expect("read committed schema");
    assert_eq!(
        committed,
        wamn_flow::json_schema_string(),
        "docs/archive/contracts/flow-schema.schema.json is stale — regenerate it (see print-flow-schema example)"
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
        .find(|n| n.id == "evaluate-specs")
        .unwrap()
        .config = serde_json::json!({ "compare": "exact-decimal", "tolerance": true });
    // 2) add a node + edge
    v2.nodes.push(wamn_flow::Node {
        id: "audit".into(),
        node_type: "custom".into(),
        label: None,
        config: serde_json::json!({}),
        connection: None,
        credential: None,
    });
    v2.edges.push(wamn_flow::Edge {
        from: "create-holds".into(),
        from_port: "main".into(),
        to: "audit".into(),
        to_port: None,
        ordinal: None,
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
            .any(|c| c.id == "evaluate-specs" && c.config_changed)
    );
    assert!(d.edges_added.iter().any(|e| e.to == "audit"));
    assert!(d.credentials_added.contains(&"audit-sink".to_string()));

    // A flow diffed against itself is empty.
    assert!(wamn_flow::diff(&v1, &v1).is_empty());
}

#[test]
fn diff_detects_connection_reference_and_requirement_changes() {
    let (_, old) = load("f3-escalate-stale-holds.flow.json");
    let mut changed_reference = old.clone();
    changed_reference
        .nodes
        .iter_mut()
        .find(|node| node.node_type == "http-request")
        .unwrap()
        .connection = Some("replacement".into());
    let reference_diff = wamn_flow::diff(&old, &changed_reference);
    assert!(
        reference_diff.nodes_changed.iter().any(|change| {
            change.connection_changed
                == Some((
                    Some("manager-notifications".into()),
                    Some("replacement".into()),
                ))
        }),
        "a logical connection reference is a visible node change"
    );

    let mut changed_requirement = old.clone();
    changed_requirement.connection_requirements[0]
        .requirement
        .requirement_version = "mutant".into();
    let requirement_diff = wamn_flow::diff(&old, &changed_requirement);
    assert_eq!(requirement_diff.connection_requirements_added.len(), 1);
    assert_eq!(requirement_diff.connection_requirements_removed.len(), 1);
}

/// F3 keeps the scheduled anchor and selected hold in durable context while it
/// drains one row at a time. The false port is deliberately unwired: frontier
/// exhaustion is the callerless flow's successful completion.
#[test]
fn f3_escalate_stale_holds_shape() {
    let (_, f) = load("f3-escalate-stale-holds.flow.json");

    assert!(
        f.nodes
            .iter()
            .any(|node| node.id == "cron" && node.node_type == "cron"),
        "F3 has a cron entry"
    );
    assert!(
        f.edges
            .iter()
            .any(|edge| edge.from == "cron" && edge.to == "cutoff-at-48h"),
        "the cron payload enters the cutoff computation first"
    );
    assert!(
        !f.nodes.iter().any(|node| node.node_type == "respond"),
        "callerless F3 has no response node"
    );

    // The artifact declares a portable logical requirement, never environment
    // authority or credential selection.
    assert!(f.allowed_hosts.is_empty());
    assert!(f.credentials.is_empty());
    assert_eq!(
        f.nodes
            .iter()
            .find(|n| n.id == "notify-manager")
            .and_then(|n| n.connection.as_deref()),
        Some("manager-notifications"),
        "notify references the portable connection requirement"
    );
    assert_eq!(
        f.nodes
            .iter()
            .find(|n| n.id == "notify-manager")
            .and_then(|n| n.config.get("path-and-query")),
        Some(&serde_json::json!("/holds")),
        "notify carries only a connection-relative target"
    );

    assert!(
        f.edges
            .iter()
            .any(|e| e.from == "escalate-head" && e.to == "next-stale-hold"),
        "escalation loops to the next one-row selection"
    );
    assert!(
        !f.edges
            .iter()
            .any(|edge| edge.from == "found" && edge.from_port == "false"),
        "found.false completes naturally"
    );
    let cutoff = f
        .nodes
        .iter()
        .find(|node| node.id == "cutoff-at-48h")
        .expect("cutoff node");
    let base = cutoff.config["base"].as_str().expect("base expression");
    assert_eq!(base, "\"scheduled-at\"");
    let selected = jmespath::compile(base)
        .expect("quoted identifier compiles")
        .search(json!({"scheduled-at": 42}))
        .expect("quoted identifier evaluates");
    assert_eq!(serde_json::to_value(selected).unwrap(), json!(42));
    assert_eq!(cutoff.config["ctx"], "@");
    let mark = f
        .nodes
        .iter()
        .find(|node| node.id == "mark")
        .expect("mark node");
    assert_eq!(
        mark.config["ctx"], "merge(context(), {hold: rows[0]})",
        "mark explicitly preserves the cutoff while storing the selected hold"
    );
}

#[test]
fn f2_disposition_recommendation_shape() {
    let (raw, flow) = load("f2-disposition-recommendation.flow.json");
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(flow.nodes.len(), 3);
    assert_eq!(flow.edges.len(), 2);
    assert_eq!(flow.nodes[0].node_type, "request");
    assert_eq!(flow.nodes[1].node_type, "disposition-recommendation");
    assert_eq!(flow.nodes[2].node_type, "respond");
    assert!(!raw.contains(r#""type": "custom""#));
    assert!(!raw.contains(r#""manifest""#));
    assert_eq!(
        parsed["nodes"][0]["config"]["input-schema"]["required"],
        json!(["hold", "history", "decision"])
    );
    assert_eq!(
        parsed["nodes"][0]["config"]["input-schema"]["additionalProperties"],
        false
    );
}

#[test]
fn t0_cron_and_event_inputs_round_trip_and_omit_absent_images() {
    let cron = CronInput {
        scheduled_at: "2026-07-27T02:00:00Z".into(),
        fired_at: "2026-07-27T02:00:03Z".into(),
    };
    let cron_json = serde_json::to_string(&cron).unwrap();
    assert_eq!(serde_json::from_str::<CronInput>(&cron_json).unwrap(), cron);

    let event = EventInput {
        event: RowEvent::Insert,
        new: Some(
            json!({"id": "d-1", "decision": "accept"})
                .as_object()
                .unwrap()
                .clone(),
        ),
        old: None,
    };
    let event_json = serde_json::to_string(&event).unwrap();
    assert!(!event_json.contains("\"old\""), "{event_json}");
    assert_eq!(
        serde_json::from_str::<EventInput>(&event_json).unwrap(),
        event
    );
    assert!(
        serde_json::from_value::<EventInput>(
            json!({"event": "insert", "new": {"id": "d-1"}, "old": null})
        )
        .is_err(),
        "absent event images are omitted, never null"
    );
}

#[test]
fn t0_old_trigger_and_scalar_entry_have_no_reader() {
    let legacy = r#"{
      "schema-version": "0.1",
      "flow-id": "legacy",
      "version": 1,
      "trigger": {"type": "manual"},
      "entry": "out",
      "nodes": [{"id": "out", "type": "respond", "config": {"status": 200}}]
    }"#;
    assert!(Flow::from_json(legacy).is_err());
}

#[test]
fn t0_f0_through_f4_use_typed_entries_and_no_legacy_definition_fields() {
    let expected = [
        ("f0-echo.flow.json", "request"),
        ("f1-receipt-received.flow.json", "request"),
        ("f2-disposition-recommendation.flow.json", "request"),
        ("f3-escalate-stale-holds.flow.json", "cron"),
        ("f4-disposition-recorded.flow.json", "event"),
    ];
    for (name, entry_type) in expected {
        let (raw, flow) = load(name);
        let object = serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
        assert!(!object.contains_key("trigger"), "{name} has legacy Trigger");
        assert!(
            !object.contains_key("entry"),
            "{name} has legacy Flow::entry"
        );
        assert_eq!(
            flow.entry_node().map(|node| node.node_type.as_str()),
            Some(entry_type),
            "{name} has the wrong typed entry"
        );
    }
}

#[test]
fn t0_event_entry_has_no_attachment_lookup_and_callerless_flows_have_no_response() {
    let (_, event) = load("f4-disposition-recorded.flow.json");
    let entry = event.entry_node().expect("F4 event entry");
    assert!(
        entry.config.is_null()
            || entry
                .config
                .as_object()
                .is_some_and(serde_json::Map::is_empty),
        "event registration resolution stays outside the graph"
    );
    for name in [
        "f3-escalate-stale-holds.flow.json",
        "f4-disposition-recorded.flow.json",
    ] {
        let (_, flow) = load(name);
        assert!(
            flow.nodes.iter().all(|node| node.node_type != "respond"),
            "{name} must complete naturally or fail"
        );
    }
}

#[test]
fn mutant_event_attachment_lookup_is_rejected_at_the_entry_field_home() {
    let (_, mut flow) = load("f4-disposition-recorded.flow.json");
    flow.nodes
        .iter_mut()
        .find(|node| node.node_type == "event")
        .unwrap()
        .config = json!({"attachment-id": "legacy-event-source"});
    assert!(
        flow.issues(&interfaces())
            .iter()
            .any(|issue| issue.code == "entry-has-source-config"),
        "an event entry must not absorb attachment/registration lookup"
    );
}

#[test]
fn mutant_f3_or_f4_terminal_response_is_rejected() {
    for name in [
        "f3-escalate-stale-holds.flow.json",
        "f4-disposition-recorded.flow.json",
    ] {
        let (_, mut flow) = load(name);
        flow.nodes.push(wamn_flow::Node {
            id: "legacy-response".into(),
            node_type: "respond".into(),
            label: None,
            config: json!({"status": 200}),
            connection: None,
            credential: None,
        });
        let from = if name.starts_with("f3") {
            "found"
        } else {
            "notify-erp"
        };
        flow.edges.push(wamn_flow::Edge {
            from: from.into(),
            from_port: if name.starts_with("f3") {
                "false".into()
            } else {
                "main".into()
            },
            to: "legacy-response".into(),
            to_port: None,
            ordinal: None,
        });
        assert!(
            flow.issues(&interfaces())
                .iter()
                .any(|issue| issue.code == "respond-without-request-entry"),
            "{name} accepted a response node"
        );
    }
}

#[test]
fn t0_canonical_graph_bytes_and_hash_ignore_json_key_order_and_whitespace() {
    let raw_a = r#"{
      "schema-version": "0.1",
      "flow-id": "canonical",
      "version": 1,
      "nodes": [
        {"id": "tick", "type": "cron"},
        {"id": "work", "type": "custom", "config": {"z": 2, "a": 1}}
      ],
      "edges": [{"from": "tick", "to": "work"}]
    }"#;
    let raw_b = r#"{"edges":[{"to":"work","from":"tick"}],"nodes":[
      {"type":"cron","id":"tick"},
      {"config":{"a":1,"z":2},"type":"custom","id":"work"}
    ],"version":1,"flow-id":"canonical","schema-version":"0.1"}"#;
    let a = Flow::from_json(raw_a).unwrap();
    let b = Flow::from_json(raw_b).unwrap();
    assert_eq!(a.canonical_bytes(), b.canonical_bytes());
    assert_eq!(a.graph_hash(), b.graph_hash());

    let mut unequal_hashes = std::collections::BTreeSet::new();
    for version in 1..=64 {
        let mut variant = a.clone();
        variant.version = version;
        assert!(
            unequal_hashes.insert(variant.graph_hash()),
            "unequal generated fixture {version} reused a digest"
        );
    }
}
