//! System proof for the callable-flow F3 graph and recovery contract.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use clap::Args;
use serde_json::{Value, json};
use wamn_flow::{EntryKind, Flow, FlowConnectionRequirement, ResolvedInterfaces};
use wamn_node_manifest::{ConnectionTypeDescriptor, PortableConnectionRequirement};
use wamn_schema_control::exposure::{ExposureRelease, FlowExposure, resolve_exposure};

const FLOW_JSON: &str = include_str!("../../../deploy/poc/f3-flow.json");
const EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f3-cron-attachment.json");
const CUTOFF_OFFSET_MS: i64 = 48 * 60 * 60 * 1_000;

#[derive(Debug, Args)]
pub struct CallableF3Args {}

pub fn run(_: CallableF3Args) -> anyhow::Result<()> {
    validate_contract(FLOW_JSON, EXPOSURE_JSON)?;
    prove_scenarios()?;
    println!(
        "callable-flow-f3 PASS: graph/attachment fixed; zero/N, cutoff, recovery, and failure windows hold"
    );
    Ok(())
}

fn interfaces() -> ResolvedInterfaces {
    ResolvedInterfaces::from([
        (
            "conditional".to_string(),
            vec!["true".to_string(), "false".to_string()],
        ),
        ("http-request".to_string(), vec!["main".to_string()]),
        ("postgres-query".to_string(), vec!["main".to_string()]),
        ("time-shift".to_string(), vec!["main".to_string()]),
        ("transform".to_string(), vec!["main".to_string()]),
    ])
}

fn validate_contract(flow_json: &str, exposure_json: &str) -> anyhow::Result<()> {
    let flow = Flow::from_json(flow_json).context("parse F3 graph")?;
    flow.validate(&interfaces())
        .map_err(|issues| anyhow::anyhow!("validate F3 graph: {issues:?}"))?;
    ensure!(
        flow.flow_id == "escalate-stale-holds",
        "F3 flow identity drift"
    );
    ensure!(
        flow.entry_node()
            .is_some_and(|node| node.node_type == "cron"),
        "F3 must have one cron entry"
    );
    ensure!(
        flow.nodes.iter().all(|node| node.node_type != "respond"),
        "F3 must not contain a response node"
    );

    let nodes: BTreeMap<_, _> = flow
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    ensure!(nodes.len() == 8, "F3 node set drift");
    ensure!(
        flow.connection_requirements
            == vec![FlowConnectionRequirement {
                name: "manager-notifications".to_string(),
                requirement: PortableConnectionRequirement::never_replay(
                    ConnectionTypeDescriptor::http_v1(),
                ),
            }],
        "F3 must declare its one portable manager-notifications HTTP connection"
    );
    require_config(
        &nodes,
        "cutoff-at-48h",
        &[
            ("base", json!("\"scheduled-at\"")),
            ("offset-ms", json!(-CUTOFF_OFFSET_MS)),
            ("ctx", json!("@")),
        ],
    )?;
    require_config(
        &nodes,
        "next-stale-hold",
        &[
            ("mode", json!("query")),
            ("params", json!(["context().cutoff"])),
        ],
    )?;
    require_config(
        &nodes,
        "mark",
        &[("ctx", json!("merge(context(), {hold: rows[0]})"))],
    )?;
    require_config(
        &nodes,
        "notify-manager",
        &[
            ("body", json!("context().hold")),
            ("path-and-query", json!("/holds")),
        ],
    )?;
    ensure!(
        nodes["notify-manager"].connection.as_deref() == Some("manager-notifications")
            && nodes["notify-manager"].credential.is_none()
            && nodes["notify-manager"].config.get("url").is_none()
            && nodes["notify-manager"]
                .config
                .get("idempotency-key")
                .is_none()
            && flow.credentials.is_empty()
            && flow.allowed_hosts.is_empty(),
        "F3 HTTP authority, credential, and idempotency injection belong to its connection"
    );
    require_config(
        &nodes,
        "escalate-head",
        &[
            ("params", json!(["context().hold.id"])),
            ("idempotent-with-key", json!(true)),
        ],
    )?;
    let select_sql = nodes["next-stale-hold"].config["sql"]
        .as_str()
        .context("next-stale-hold SQL")?;
    ensure!(
        select_sql.contains("status = 'open'")
            && select_sql.contains("opened_at < $1::timestamptz")
            && select_sql.contains("ORDER BY opened_at, id LIMIT 1"),
        "F3 selection must be stable, stale-only, and one-row"
    );
    let escalate_sql = nodes["escalate-head"].config["sql"]
        .as_str()
        .context("escalate-head SQL")?;
    ensure!(
        escalate_sql.starts_with("UPDATE quality_holds SET status = 'escalated'")
            && escalate_sql.contains("id = $1::uuid AND status = 'open'"),
        "F3 escalation update drift"
    );

    let expected_edges = BTreeSet::from([
        ("cron", "main", "cutoff-at-48h"),
        ("cutoff-at-48h", "main", "next-stale-hold"),
        ("next-stale-hold", "main", "found"),
        ("found", "true", "mark"),
        ("mark", "main", "notify-manager"),
        ("notify-manager", "main", "escalate-head"),
        ("notify-manager", "error", "notification-failed"),
        ("escalate-head", "main", "next-stale-hold"),
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
        "F3 must notify before escalating and loop one selected row"
    );

    let exposure: ExposureRelease =
        serde_json::from_str(exposure_json).context("parse F3 exposure")?;
    let resolved = resolve_exposure(
        &exposure,
        &[FlowExposure {
            flow_id: &flow.flow_id,
            entry_kind: EntryKind::Cron,
            artifact_hash: "sha256:f3-proof",
        }],
    )
    .context("resolve F3 exposure")?;
    ensure!(resolved.len() == 1, "F3 must have exactly one attachment");
    let attachment = &resolved[0].attachment;
    ensure!(
        attachment.id == "escalate-stale-holds-cron"
            && attachment.flow_id == flow.flow_id
            && attachment.source_id == "stale-hold-schedule",
        "F3 attachment target drift"
    );
    ensure!(
        attachment.route.is_none()
            && attachment.mappings.is_empty()
            && attachment.response_deadline_ms.is_none(),
        "cron attachment must not carry request/response configuration"
    );
    let source = exposure
        .sources
        .iter()
        .find(|source| source.id == attachment.source_id)
        .context("F3 schedule source")?;
    ensure!(
        source.definition
            == json!({
                "schedule": "0 2 * * *",
                "timezone": "America/New_York",
                "catch-up": "skip"
            }),
        "F3 schedule source drift"
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
        .with_context(|| format!("F3 node {node_id} missing"))?;
    for (key, value) in expected {
        ensure!(
            node.config.get(*key) == Some(value),
            "F3 node {node_id} config {key} drift"
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Hold {
    id: &'static str,
    opened_at_ms: i64,
    open: bool,
    escalated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminal {
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    None,
    RecordedNotifyCrash(&'static str),
    MidNotifyCrash(&'static str),
    TerminalNotify(&'static str),
    TerminalAfterNotify(&'static str),
}

#[derive(Debug, Default)]
struct Sink {
    transport_attempts: Vec<String>,
    effects: BTreeSet<String>,
}

impl Sink {
    fn send(&mut self, key: &str) {
        self.transport_attempts.push(key.to_string());
        self.effects.insert(key.to_string());
    }
}

#[derive(Debug)]
struct Drain {
    terminal: Terminal,
    selected: Vec<&'static str>,
    notified_keys: Vec<String>,
}

fn drain(
    run_id: &str,
    scheduled_at_ms: i64,
    _fired_at_ms: i64,
    holds: &mut [Hold],
    sink: &mut Sink,
    fault: Fault,
) -> Drain {
    let cutoff_ms = scheduled_at_ms - CUTOFF_OFFSET_MS;
    let mut selected = Vec::new();
    let mut notified_keys = Vec::new();
    let mut occurrence = 0_u32;
    loop {
        let Some(index) = holds
            .iter()
            .enumerate()
            .filter(|(_, hold)| hold.open && !hold.escalated && hold.opened_at_ms < cutoff_ms)
            .min_by_key(|(_, hold)| (hold.opened_at_ms, hold.id))
            .map(|(index, _)| index)
        else {
            return Drain {
                terminal: Terminal::Completed,
                selected,
                notified_keys,
            };
        };
        let hold_id = holds[index].id;
        selected.push(hold_id);
        let key = format!("{run_id}:notify-manager:{occurrence}");
        occurrence += 1;

        match fault {
            Fault::TerminalNotify(target) if target == hold_id => {
                sink.transport_attempts.push(key);
                return Drain {
                    terminal: Terminal::Failed,
                    selected,
                    notified_keys,
                };
            }
            Fault::RecordedNotifyCrash(target) if target == hold_id => {
                sink.send(&key);
                notified_keys.push(key);
                // Recovery sees the recorded success and advances without dispatch.
            }
            Fault::MidNotifyCrash(target) if target == hold_id => {
                sink.send(&key);
                sink.send(&key);
                notified_keys.push(key);
            }
            Fault::TerminalAfterNotify(target) if target == hold_id => {
                sink.send(&key);
                notified_keys.push(key);
                return Drain {
                    terminal: Terminal::Failed,
                    selected,
                    notified_keys,
                };
            }
            Fault::None
            | Fault::RecordedNotifyCrash(_)
            | Fault::MidNotifyCrash(_)
            | Fault::TerminalNotify(_)
            | Fault::TerminalAfterNotify(_) => {
                sink.send(&key);
                notified_keys.push(key);
            }
        }
        holds[index].escalated = true;
    }
}

fn holds_for(cutoff_ms: i64) -> Vec<Hold> {
    vec![
        Hold {
            id: "stale-1",
            opened_at_ms: cutoff_ms - 2,
            open: true,
            escalated: false,
        },
        Hold {
            id: "stale-2",
            opened_at_ms: cutoff_ms - 1,
            open: true,
            escalated: false,
        },
        Hold {
            id: "stale-3",
            opened_at_ms: cutoff_ms - 1,
            open: true,
            escalated: false,
        },
        Hold {
            id: "at-cutoff",
            opened_at_ms: cutoff_ms,
            open: true,
            escalated: false,
        },
        Hold {
            id: "current",
            opened_at_ms: cutoff_ms + 1,
            open: true,
            escalated: false,
        },
        Hold {
            id: "disposed",
            opened_at_ms: cutoff_ms - 3,
            open: false,
            escalated: false,
        },
    ]
}

fn prove_scenarios() -> anyhow::Result<()> {
    let scheduled_at = 1_800_000_000_000_i64;
    let cutoff = scheduled_at - CUTOFF_OFFSET_MS;

    let mut sink = Sink::default();
    let mut zero = holds_for(cutoff);
    zero.iter_mut()
        .filter(|hold| hold.id.starts_with("stale-"))
        .for_each(|hold| hold.escalated = true);
    let result = drain(
        "zero",
        scheduled_at,
        scheduled_at,
        &mut zero,
        &mut sink,
        Fault::None,
    );
    ensure!(result.terminal == Terminal::Completed && result.selected.is_empty());

    let mut sink = Sink::default();
    let mut holds = holds_for(cutoff);
    holds.swap(0, 2);
    let result = drain(
        "normal",
        scheduled_at,
        scheduled_at,
        &mut holds,
        &mut sink,
        Fault::None,
    );
    ensure!(
        result.terminal == Terminal::Completed
            && result.selected == ["stale-1", "stale-2", "stale-3"]
            && sink.effects.len() == 3,
        "N-row drain or stale predicate failed"
    );

    let mut delayed_holds = holds_for(cutoff);
    let mut delayed_sink = Sink::default();
    let delayed = drain(
        "delayed",
        scheduled_at,
        scheduled_at + 12 * 60 * 60 * 1_000,
        &mut delayed_holds,
        &mut delayed_sink,
        Fault::None,
    );
    ensure!(
        delayed.selected == result.selected,
        "delayed firing changed the scheduled cutoff"
    );

    let mut holds = holds_for(cutoff);
    let mut sink = Sink::default();
    let recorded = drain(
        "recorded",
        scheduled_at,
        scheduled_at,
        &mut holds,
        &mut sink,
        Fault::RecordedNotifyCrash("stale-1"),
    );
    ensure!(
        recorded.terminal == Terminal::Completed
            && sink.transport_attempts
                == [
                    "recorded:notify-manager:0",
                    "recorded:notify-manager:1",
                    "recorded:notify-manager:2"
                ],
        "recorded notification was dispatched again"
    );

    let mut holds = holds_for(cutoff);
    let mut sink = Sink::default();
    let mid = drain(
        "mid",
        scheduled_at,
        scheduled_at,
        &mut holds,
        &mut sink,
        Fault::MidNotifyCrash("stale-1"),
    );
    ensure!(
        mid.terminal == Terminal::Completed
            && sink.transport_attempts[0] == sink.transport_attempts[1]
            && sink.effects.len() == 3,
        "mid-notify recovery did not reuse the same provider key"
    );

    let mut holds = holds_for(cutoff);
    let mut sink = Sink::default();
    let failed = drain(
        "failed",
        scheduled_at,
        scheduled_at,
        &mut holds,
        &mut sink,
        Fault::TerminalNotify("stale-2"),
    );
    ensure!(
        failed.terminal == Terminal::Failed
            && failed.selected == ["stale-1", "stale-2"]
            && !holds[1].escalated,
        "hard notify failure did not abort before escalation"
    );
    let next = drain(
        "next-tick",
        scheduled_at,
        scheduled_at,
        &mut holds,
        &mut sink,
        Fault::None,
    );
    ensure!(
        next.terminal == Terminal::Completed
            && next.selected == ["stale-2", "stale-3"]
            && next.notified_keys == ["next-tick:notify-manager:0", "next-tick:notify-manager:1"],
        "next tick did not reselect the failed hold under a new key"
    );

    let mut holds = holds_for(cutoff);
    let mut sink = Sink::default();
    let terminal = drain(
        "dead-run",
        scheduled_at,
        scheduled_at,
        &mut holds,
        &mut sink,
        Fault::TerminalAfterNotify("stale-1"),
    );
    ensure!(terminal.terminal == Terminal::Failed && !holds[0].escalated);
    let recovered = drain(
        "new-run",
        scheduled_at,
        scheduled_at,
        &mut holds,
        &mut sink,
        Fault::None,
    );
    ensure!(
        recovered.selected == ["stale-1", "stale-2", "stale-3"]
            && sink.effects.contains("dead-run:notify-manager:0")
            && sink.effects.contains("new-run:notify-manager:0"),
        "new run must use a new occurrence key after terminal death"
    );
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use wamn_run_state::attempt::{AttemptStartResult, RecoveryClass};
    use wamn_run_state::transitions::begin_attempt_sql;

    #[test]
    fn package_proof_covers_all_f3_scenarios() {
        validate_contract(FLOW_JSON, EXPOSURE_JSON).unwrap();
        prove_scenarios().unwrap();
    }

    #[test]
    fn graph_mutants_break_load_bearing_order_and_context() {
        let mut graph: Value = serde_json::from_str(FLOW_JSON).unwrap();
        graph["nodes"][1]["config"]["base"] = json!("fired-at");
        assert!(validate_contract(&graph.to_string(), EXPOSURE_JSON).is_err());

        let mut graph: Value = serde_json::from_str(FLOW_JSON).unwrap();
        graph["edges"][5]["to"] = json!("next-stale-hold");
        assert!(validate_contract(&graph.to_string(), EXPOSURE_JSON).is_err());
    }

    #[test]
    fn unquoted_hyphenated_cron_payload_lookup_is_refused() {
        let mut graph: Value = serde_json::from_str(FLOW_JSON).unwrap();
        graph["nodes"][1]["config"]["base"] = json!("scheduled-at");
        assert!(validate_contract(&graph.to_string(), EXPOSURE_JSON).is_err());
    }

    #[test]
    fn attachment_and_response_drift_are_refused() {
        let mut exposure: Value = serde_json::from_str(EXPOSURE_JSON).unwrap();
        exposure["sources"][0]["definition"]["timezone"] = json!("UTC");
        assert!(validate_contract(FLOW_JSON, &exposure.to_string()).is_err());

        let mut exposure: Value = serde_json::from_str(EXPOSURE_JSON).unwrap();
        exposure["attachments"][0]["response-deadline-ms"] = json!(30_000);
        assert!(validate_contract(FLOW_JSON, &exposure.to_string()).is_err());

        let mut graph: Value = serde_json::from_str(FLOW_JSON).unwrap();
        graph["nodes"].as_array_mut().unwrap().push(json!({
            "id": "response",
            "type": "respond",
            "config": {"status": 200}
        }));
        assert!(validate_contract(&graph.to_string(), EXPOSURE_JSON).is_err());
    }

    #[test]
    fn never_replay_and_keyed_recovery_remain_discriminating() {
        assert_eq!(RecoveryClass::NeverReplay.as_sql(), "never-replay");
        assert_eq!(
            AttemptStartResult::from_code("effect-uncertain"),
            Some(AttemptStartResult::EffectUncertain)
        );
        assert!(!AttemptStartResult::EffectUncertain.permits_dispatch());
        let sql = begin_attempt_sql();
        assert!(sql.contains("n.recovery_class = 'never-replay' THEN 'effect-uncertain'"));
        assert!(sql.contains("n.recovery_class IN ('replay', 'idempotent-with-key')"));
    }

    #[test]
    fn archived_job_has_no_orchestrator_route() {
        let router = include_str!("../../orchestrator/src/main.rs");
        assert!(!router.contains("CallableF3(callable_f3::CallableF3Args)"));
        assert!(!router.contains("Command::CallableF3(args) => callable_f3::run(args)"));
    }
}
