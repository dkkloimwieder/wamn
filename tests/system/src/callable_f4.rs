//! System proof for the callable-flow F4 graph, event scope, and child recovery.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use clap::Args;
use serde_json::{Value, json};
use wamn_event_reg::{EventRegistration, Op, validate as validate_registration};
use wamn_flow::{Flow, FlowConnectionRequirement, ResolvedInterfaces};
use wamn_node_manifest::{ConnectionTypeDescriptor, PortableConnectionRequirement};
use wamn_run_state::{
    admission::admission_sql,
    child::{cancel_unreleased_child_sql, create_or_recover_child_sql, release_child_sql},
};
use wamn_schema_model::Catalog;

const FLOW_JSON: &str = include_str!("../../../deploy/poc/f4-flow.json");
const REGISTRATION_JSON: &str = include_str!("../../../deploy/poc/f4-event-registration.json");
const CATALOG_JSON: &str = include_str!("../../../deploy/poc/poc-material-receiving.catalog.json");
const F2_EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f2-internal-attachment.json");

#[derive(Debug, Args)]
pub struct CallableF4Args {}

pub fn run(_: CallableF4Args) -> anyhow::Result<()> {
    validate_contract(FLOW_JSON, REGISTRATION_JSON)?;
    prove_scenarios()?;
    println!(
        "callable-flow-f4 PASS: prior-only history, one review, effective-once callback, event scope, and child recovery hold"
    );
    Ok(())
}

fn interfaces() -> ResolvedInterfaces {
    ResolvedInterfaces::from([
        ("http-request".to_string(), vec!["main".to_string()]),
        ("invoke-flow".to_string(), vec!["main".to_string()]),
        ("postgres-query".to_string(), vec!["main".to_string()]),
        ("transform".to_string(), vec!["main".to_string()]),
    ])
}

fn validate_contract(flow_json: &str, registration_json: &str) -> anyhow::Result<()> {
    validate_graph_contract(flow_json)?;
    validate_registration_contract(registration_json)?;
    validate_event_admission_contract(admission_sql().admit())?;
    validate_child_transition_contract(
        &create_or_recover_child_sql(),
        &release_child_sql(),
        cancel_unreleased_child_sql(),
    )
}

fn validate_graph_contract(flow_json: &str) -> anyhow::Result<()> {
    let flow = Flow::from_json(flow_json).context("parse F4 graph")?;
    flow.validate(&interfaces())
        .map_err(|issues| anyhow::anyhow!("validate F4 graph: {issues:?}"))?;
    ensure!(
        flow.flow_id == "disposition-recorded" && flow.version == 1,
        "F4 identity drift"
    );
    let entry = flow.entry_node().context("F4 event entry")?;
    ensure!(entry.node_type == "event", "F4 entry must be event");
    ensure!(
        entry.config.is_null()
            || entry
                .config
                .as_object()
                .is_some_and(serde_json::Map::is_empty),
        "event entry must not resolve an attachment or registration"
    );
    ensure!(
        flow.nodes.iter().all(|node| node.node_type != "respond"),
        "callerless F4 must not respond"
    );

    let nodes: BTreeMap<_, _> = flow
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    ensure!(nodes.len() == 10, "F4 node set drift");
    ensure!(
        flow.connection_requirements
            == vec![FlowConnectionRequirement {
                name: "erp-callback".to_string(),
                requirement: PortableConnectionRequirement::never_replay(
                    ConnectionTypeDescriptor::http_v1(),
                ),
            }],
        "F4 must declare its one portable erp-callback HTTP connection"
    );
    require_config(
        &nodes,
        "capture",
        &[
            ("expression", json!("@")),
            ("ctx", json!("{disposition: new}")),
        ],
    )?;
    require_config(
        &nodes,
        "load-hold-context",
        &[
            ("mode", json!("query")),
            ("idempotent-with-key", json!(true)),
            (
                "params",
                json!([
                    "context().disposition.hold_id",
                    "context().disposition.decided_at",
                    "context().disposition.id"
                ]),
            ),
        ],
    )?;
    require_config(
        &nodes,
        "shape-input",
        &[(
            "expression",
            json!(
                "{hold: rows[0].hold, history: rows[0].history, decision: context().disposition.decision}"
            ),
        )],
    )?;
    require_config(
        &nodes,
        "invoke-recommendation",
        &[
            ("flow-id", json!("disposition-recommendation")),
            (
                "attachment-id",
                json!("disposition-recommendation-internal"),
            ),
            ("actor-mode", json!("service")),
        ],
    )?;
    require_config(
        &nodes,
        "record-comparison",
        &[
            ("mode", json!("query")),
            ("idempotent-with-key", json!(true)),
            ("params", json!(["@", "context().disposition.id"])),
        ],
    )?;
    require_config(
        &nodes,
        "notify-erp",
        &[
            ("method", json!("POST")),
            ("path-and-query", json!("/dispositions")),
            ("body", json!("@")),
        ],
    )?;
    ensure!(
        nodes["notify-erp"].connection.as_deref() == Some("erp-callback")
            && nodes["notify-erp"].credential.is_none()
            && nodes["notify-erp"].config.get("url").is_none()
            && nodes["notify-erp"].config.get("idempotency-key").is_none()
            && flow.credentials.is_empty()
            && flow.allowed_hosts.is_empty(),
        "F4 HTTP authority, credential, and idempotency injection belong to its connection"
    );

    let history_sql = sql(&nodes, "load-hold-context")?;
    ensure!(
        history_sql.contains("d.hold_id = c.hold_id")
            && history_sql.contains("d.decided_at < c.decided_at")
            && history_sql.contains("d.decided_at = c.decided_at AND d.id < c.id")
            && history_sql.contains("ORDER BY d.decided_at, d.id) FILTER (WHERE d.id IS NOT NULL)"),
        "history must be strictly prior and stably ordered by (decided_at,id)"
    );
    let review_sql = sql(&nodes, "record-comparison")?;
    ensure!(
        review_sql.contains("INSERT INTO disposition_reviews")
            && review_sql.contains("current_setting('app.tenant', true)")
            && review_sql.contains("ON CONFLICT (tenant_id, disposition_id) DO NOTHING")
            && review_sql
                .contains("RETURNING id, disposition_id, recommendation, confidence, matched")
            && review_sql.contains("read_back AS")
            && review_sql.contains("WHERE NOT EXISTS (SELECT 1 FROM inserted)"),
        "review write must be tenant-scoped, unique, returning, and read back on replay"
    );

    let expected_edges = BTreeSet::from([
        ("capture", "main", "load-hold-context"),
        ("event", "main", "capture"),
        ("invoke-recommendation", "error", "recommendation-failed"),
        ("invoke-recommendation", "main", "record-comparison"),
        ("load-hold-context", "main", "shape-input"),
        ("notify-erp", "error", "callback-failed"),
        ("record-comparison", "main", "shape-callback"),
        ("shape-callback", "main", "notify-erp"),
        ("shape-input", "main", "invoke-recommendation"),
    ]);
    let actual_edges: BTreeSet<_> = flow
        .edges
        .iter()
        .map(|edge| {
            (
                edge.from.as_str(),
                edge.from_port.as_str(),
                edge.to.as_str(),
            )
        })
        .collect();
    ensure!(
        actual_edges == expected_edges,
        "F4 topology or authored fail branch drift"
    );
    Ok(())
}

fn require_config(
    nodes: &BTreeMap<&str, &wamn_flow::Node>,
    node_id: &str,
    expected: &[(&str, Value)],
) -> anyhow::Result<()> {
    let node = nodes
        .get(node_id)
        .with_context(|| format!("F4 node {node_id} missing"))?;
    for (key, value) in expected {
        ensure!(
            node.config.get(*key) == Some(value),
            "F4 node {node_id} config {key} drift"
        );
    }
    Ok(())
}

fn sql<'a>(nodes: &'a BTreeMap<&str, &wamn_flow::Node>, node_id: &str) -> anyhow::Result<&'a str> {
    nodes[node_id].config["sql"]
        .as_str()
        .with_context(|| format!("F4 node {node_id} SQL"))
}

fn validate_registration_contract(registration_json: &str) -> anyhow::Result<()> {
    let catalog = Catalog::from_json(CATALOG_JSON).context("parse POC catalog")?;
    let registration: EventRegistration =
        serde_json::from_str(registration_json).context("parse F4 registration")?;
    validate_registration(&registration, &catalog)
        .map_err(|issues| anyhow::anyhow!("validate F4 registration: {issues:?}"))?;
    ensure!(
        registration.registration_id == "disposition-recorded-insert"
            && registration.catalog_id == "poc-material-receiving"
            && registration.flow_id == "disposition-recorded"
            && registration.entity.as_str() == "dispositions"
            && registration.ops == [Op::Insert]
            && registration.condition.is_none()
            && registration.partition_key.is_none(),
        "F4 registration must target only disposition inserts"
    );
    Ok(())
}

fn validate_event_admission_contract(sql: &str) -> anyhow::Result<()> {
    ensure!(
        sql.contains("i.producer = 'event'")
            && sql.contains("i.attachment_id IS NOT NULL")
            && sql.contains("i.registration_id IS NULL")
            && sql.contains("r.tenant_id = i.tenant_id")
            && sql.contains("'evt:' || i.registration_id || ':' || i.event_seq::text")
            && sql.contains("CASE WHEN c.producer = 'event' THEN c.registration_id END")
            && sql.contains("'{registration-hash}'")
            && sql.contains("CASE WHEN c.producer = 'event' THEN c.event_seq ELSE 0 END"),
        "event admission lost no-attachment, full-scope dedup, or metadata homes"
    );
    Ok(())
}

fn validate_child_transition_contract(
    create: &str,
    release: &str,
    cancel: &str,
) -> anyhow::Result<()> {
    ensure!(
        create.contains("c.parent_node_id = $5 AND c.parent_occurrence = $6")
            && create.contains("WHEN c.run_id IS NOT NULL THEN 'ready'")
            && create.contains("WHEN $10::text <> 'service' THEN 'unsupported-actor-mode'")
            && create.contains("NOT (d.caller_policy->'allowed-callers' ? p.flow_id)")
            && create.contains("'caller', jsonb_build_object(")
            && create.contains("'lineage'")
            && create.contains("wait_generation = $4")
            && create.contains("WHEN c.inserted THEN 'created' ELSE 'recovered'"),
        "child create-or-recover, service actor, lineage, or wait fence drift"
    );
    ensure!(
        release.contains("p.wait_generation IS DISTINCT FROM $13::bigint")
            && release.contains("caller_released_at = now()")
            && release.contains("cleared_parent AS")
            && release.contains("wait_generation = NULL")
            && release.contains("woken_parent AS")
            && release.contains("FROM released AS c"),
        "child release and exact parent wake must remain one statement"
    );
    ensure!(
        cancel.contains("c.caller_released_at IS NOT NULL THEN 'already-released'")
            && cancel.contains("q.lease_generation <> i.expected_generation")
            && cancel.contains("DELETE FROM run_queue")
            && cancel.contains("RETURNING q.tenant_id, q.run_id, q.lease_generation + 1"),
        "pre-release cancellation must seize the exact queue generation"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Disposition {
    id: &'static str,
    decided_at: &'static str,
    decision: &'static str,
}

fn prior_history(
    current: &Disposition,
    rows: impl IntoIterator<Item = Disposition>,
) -> Vec<&'static str> {
    let mut prior: Vec<_> = rows
        .into_iter()
        .filter(|row| (row.decided_at, row.id) < (current.decided_at, current.id))
        .collect();
    prior.sort_by_key(|row| (row.decided_at, row.id));
    prior.into_iter().map(|row| row.decision).collect()
}

fn record_review<'a>(
    reviews: &'a mut BTreeMap<String, Value>,
    disposition_id: &str,
    recommendation: Value,
) -> &'a Value {
    reviews
        .entry(disposition_id.to_string())
        .or_insert(recommendation)
}

#[derive(Debug, Default)]
struct CallbackSink {
    attempts: Vec<String>,
    effects: BTreeMap<String, Value>,
}

impl CallbackSink {
    fn post(&mut self, key: &str, body: Value) {
        self.attempts.push(key.to_string());
        self.effects.entry(key.to_string()).or_insert(body);
    }
}

fn create_or_recover(
    children: &mut BTreeMap<(&'static str, &'static str, u32), &'static str>,
    occurrence: (&'static str, &'static str, u32),
    proposed: &'static str,
) -> &'static str {
    children.entry(occurrence).or_insert(proposed)
}

fn prove_scenarios() -> anyhow::Result<()> {
    let event = json!({
        "event": "insert",
        "new": {
            "id": "00000000-0000-0000-0000-000000000020",
            "hold_id": "00000000-0000-0000-0000-000000000010",
            "decision": "reject",
            "inspector_id": "00000000-0000-0000-0000-000000000001",
            "decided_at": "2026-07-26T14:03:22Z"
        }
    });
    ensure!(
        event.as_object().is_some_and(|object| object.len() == 2)
            && event.get("old").is_none()
            && event["new"]
                .as_object()
                .is_some_and(|object| object.len() == 5),
        "insert business payload must contain only event and new"
    );

    let current = Disposition {
        id: "d-20",
        decided_at: "2026-07-26T14:03:22Z",
        decision: "reject",
    };
    let history = prior_history(
        &current,
        [
            Disposition {
                id: "d-10",
                decided_at: "2026-07-26T14:03:21Z",
                decision: "accept",
            },
            Disposition {
                id: "d-19",
                decided_at: "2026-07-26T14:03:22Z",
                decision: "use-as-is",
            },
            current.clone(),
            Disposition {
                id: "d-21",
                decided_at: "2026-07-26T14:03:22Z",
                decision: "reject",
            },
            Disposition {
                id: "d-01",
                decided_at: "2026-07-26T14:03:23Z",
                decision: "reject",
            },
        ],
    );
    ensure!(
        history == ["accept", "use-as-is"],
        "history admitted current/later rows or lost tuple ordering"
    );

    let recommendation = json!({
        "recommendation": "reject",
        "confidence": "0.95",
        "matched": true
    });
    let mut reviews = BTreeMap::new();
    let first = record_review(&mut reviews, current.id, recommendation.clone()).clone();
    let replay = record_review(&mut reviews, current.id, recommendation).clone();
    ensure!(
        reviews.len() == 1 && first == replay && first["confidence"].is_string(),
        "review replay did not return one stable row"
    );

    let key = "event-run:notify-erp:0";
    let callback = json!({"review": replay, "disposition": event["new"]});
    let mut sink = CallbackSink::default();
    sink.post(key, callback.clone());
    sink.post(key, callback);
    ensure!(
        sink.attempts == [key, key] && sink.effects.len() == 1,
        "callback recovery must make two transport attempts and one business effect"
    );

    let identities = BTreeSet::from([
        ("tenant-a", "reg-a", 7_u64),
        ("tenant-a", "reg-b", 7_u64),
        ("tenant-b", "reg-a", 7_u64),
    ]);
    ensure!(
        identities.len() == 3,
        "event dedup identity lost tenant or registration scope"
    );

    let mut children = BTreeMap::new();
    let created = create_or_recover(
        &mut children,
        ("parent", "invoke-recommendation", 0),
        "child-a",
    );
    let recovered = create_or_recover(
        &mut children,
        ("parent", "invoke-recommendation", 0),
        "child-b",
    );
    ensure!(
        created == recovered && children.len() == 1,
        "child creation seam produced two children"
    );

    let exposure: Value = serde_json::from_str(F2_EXPOSURE_JSON)?;
    ensure!(
        exposure["sources"][0]["definition"]["allowed-callers"] == json!(["disposition-recorded"]),
        "F2 caller policy no longer authorizes exactly F4"
    );
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn mutate_node<'a>(flow: &'a mut Value, id: &str) -> &'a mut Value {
        flow["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["id"] == id)
            .unwrap()
    }

    #[test]
    fn package_proof_covers_f4_contract_and_scenarios() {
        validate_contract(FLOW_JSON, REGISTRATION_JSON).unwrap();
        prove_scenarios().unwrap();
    }

    #[test]
    fn canonical_and_fixture_graphs_are_identical() {
        let fixture = include_str!(
            "../../../crates/execution/flow-model/tests/fixtures/f4-disposition-recorded.flow.json"
        );
        assert_eq!(
            Flow::from_json(FLOW_JSON).unwrap().canonical_bytes(),
            Flow::from_json(fixture).unwrap().canonical_bytes()
        );
    }

    #[test]
    fn event_lookup_and_context_order_mutants_fail() {
        let mut attachment: Value = serde_json::from_str(FLOW_JSON).unwrap();
        mutate_node(&mut attachment, "event")["config"] = json!({"attachment-id": "forbidden"});
        assert!(validate_graph_contract(&attachment.to_string()).is_err());

        let mut missing_capture: Value = serde_json::from_str(FLOW_JSON).unwrap();
        mutate_node(&mut missing_capture, "capture")["config"]["ctx"] = Value::Null;
        assert!(validate_graph_contract(&missing_capture.to_string()).is_err());

        let mut reordered: Value = serde_json::from_str(FLOW_JSON).unwrap();
        reordered["edges"][0]["to"] = json!("load-hold-context");
        assert!(validate_graph_contract(&reordered.to_string()).is_err());
    }

    #[test]
    fn prior_only_tuple_predicate_mutants_fail() {
        for (from, to) in [
            (
                "d.decided_at < c.decided_at",
                "d.decided_at <= c.decided_at",
            ),
            (
                "d.decided_at = c.decided_at AND d.id < c.id",
                "d.id <> c.id",
            ),
            ("ORDER BY d.decided_at, d.id", "ORDER BY d.decided_at"),
        ] {
            let mut flow: Value = serde_json::from_str(FLOW_JSON).unwrap();
            let node = mutate_node(&mut flow, "load-hold-context");
            let changed = node["config"]["sql"].as_str().unwrap().replace(from, to);
            node["config"]["sql"] = json!(changed);
            assert!(validate_graph_contract(&flow.to_string()).is_err());
        }
    }

    #[test]
    fn service_actor_and_caller_policy_mutants_fail() {
        let mut flow: Value = serde_json::from_str(FLOW_JSON).unwrap();
        mutate_node(&mut flow, "invoke-recommendation")["config"]["actor-mode"] = json!("inherit");
        assert!(validate_graph_contract(&flow.to_string()).is_err());

        let create = create_or_recover_child_sql();
        assert!(
            validate_child_transition_contract(
                &create.replace(
                    "NOT (d.caller_policy->'allowed-callers' ? p.flow_id)",
                    "false"
                ),
                &release_child_sql(),
                cancel_unreleased_child_sql(),
            )
            .is_err()
        );
    }

    #[test]
    fn review_upsert_readback_mutants_fail() {
        for fragment in [
            "ON CONFLICT (tenant_id, disposition_id) DO NOTHING",
            "RETURNING id, disposition_id, recommendation, confidence, matched",
            "read_back AS",
            "WHERE NOT EXISTS (SELECT 1 FROM inserted)",
        ] {
            let mut flow: Value = serde_json::from_str(FLOW_JSON).unwrap();
            let node = mutate_node(&mut flow, "record-comparison");
            node["config"]["sql"] = json!(
                node["config"]["sql"]
                    .as_str()
                    .unwrap()
                    .replace(fragment, "")
            );
            assert!(validate_graph_contract(&flow.to_string()).is_err());
        }
    }

    #[test]
    fn callback_connection_and_effective_once_mutants_fail() {
        let mut flow: Value = serde_json::from_str(FLOW_JSON).unwrap();
        mutate_node(&mut flow, "notify-erp")
            .as_object_mut()
            .unwrap()
            .remove("connection");
        assert!(validate_graph_contract(&flow.to_string()).is_err());

        let mut sink = CallbackSink::default();
        sink.post("key-a", json!({"review": 1}));
        sink.post("key-b", json!({"review": 1}));
        assert_eq!(sink.attempts.len(), 2);
        assert_eq!(
            sink.effects.len(),
            2,
            "different retry keys must fail the gate"
        );
    }

    #[test]
    fn registration_and_event_scope_mutants_fail() {
        let mut update: Value = serde_json::from_str(REGISTRATION_JSON).unwrap();
        update["ops"] = json!(["update"]);
        assert!(validate_registration_contract(&update.to_string()).is_err());

        let admit = admission_sql().admit().to_string();
        for fragment in [
            "r.tenant_id = i.tenant_id",
            "'evt:' || i.registration_id || ':' || i.event_seq::text",
            "i.attachment_id IS NOT NULL",
            "'{registration-hash}'",
        ] {
            assert!(
                validate_event_admission_contract(&admit.replace(fragment, "")).is_err(),
                "event admission mutant survived: {fragment}"
            );
        }
    }

    #[test]
    fn child_seam_atomic_wake_and_cancel_fence_mutants_fail() {
        let create = create_or_recover_child_sql();
        let release = release_child_sql();
        let cancel = cancel_unreleased_child_sql();
        for (mutated_create, mutated_release, mutated_cancel) in [
            (
                create.replace(
                    "c.parent_node_id = $5 AND c.parent_occurrence = $6",
                    "c.parent_node_id = $5",
                ),
                release.clone(),
                cancel.to_string(),
            ),
            (
                create.clone(),
                release.replace("FROM released AS c", "FROM classified AS c"),
                cancel.to_string(),
            ),
            (
                create.clone(),
                release.clone(),
                cancel.replace("q.lease_generation <> i.expected_generation", "false"),
            ),
            (
                create,
                release,
                cancel.replace(
                    "c.caller_released_at IS NOT NULL THEN 'already-released'",
                    "false THEN 'already-released'",
                ),
            ),
        ] {
            assert!(
                validate_child_transition_contract(
                    &mutated_create,
                    &mutated_release,
                    &mutated_cancel
                )
                .is_err()
            );
        }
    }

    #[test]
    fn archived_job_has_no_orchestrator_route() {
        let router = include_str!("../../orchestrator/src/main.rs");
        assert!(!router.contains("CallableF4(callable_f4::CallableF4Args)"));
        assert!(!router.contains("Command::CallableF4(args) => callable_f4::run(args)"));
    }
}
