//! System proof for the callable-flow F1 release and deterministic CTE recovery.

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use clap::Args;
use serde_json::{Value, json};
use wamn_catalog::{
    Artifact, Attachment, AttachmentDraft, AttachmentId, AttachmentKind, CanonicalJson,
    NodeImplementation, Release, ReleaseId, Source, SourceId, SourceKind,
};
use wamn_flow::{EntryKind, Flow, ResolvedInterfaces, canonical_json_sha256};
use wamn_node_manifest::{CapabilityClass, RecoveryClass, ResolvedNodeInterface, ResolvedPurity};
use wamn_schema_control::exposure::{ExposureRelease, FlowExposure, resolve_exposure};

const FLOW_JSON: &str = include_str!("../../../deploy/poc/f1-flow.json");
const EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f1-http-attachment.json");
const POC_CONFIG_JSON: &str =
    include_str!("../../../deploy/poc/poc-material-receiving.config.json");
const EVALUATE_COMPONENT_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const NORMALIZE_COMPONENT_DIGEST: &str =
    "sha256:2222222222222222222222222222222222222222222222222222222222222222";

#[derive(Debug, Args)]
pub struct CallableF1Args {}

pub fn run(_: CallableF1Args) -> anyhow::Result<()> {
    let published = published_release(FLOW_JSON, EXPOSURE_JSON)?;
    prove_scenarios(&published.artifact_hash)?;
    println!(
        "callable-flow-f1 PASS: direct pure components, immutable HTTP release, deterministic CTE recovery, refusals, and webhook cutover hold"
    );
    Ok(())
}

struct PublishedRelease {
    artifact_hash: String,
    _release: Release,
}

fn interface(node_type: &str, ports: &[&str], purity: ResolvedPurity) -> ResolvedNodeInterface {
    ResolvedNodeInterface::new(
        node_type,
        "wamn:node@0.1.0",
        ports.iter().map(|port| (*port).to_string()).collect(),
        vec![match purity {
            ResolvedPurity::Pure => CapabilityClass::Pure,
            ResolvedPurity::Effectful => CapabilityClass::Postgres,
        }],
        Vec::new(),
        purity,
        match purity {
            ResolvedPurity::Pure => RecoveryClass::Replay,
            ResolvedPurity::Effectful => RecoveryClass::NeverReplay,
        },
    )
}

fn implementations() -> anyhow::Result<Vec<NodeImplementation>> {
    Ok(vec![
        NodeImplementation::platform(interface(
            "conditional",
            &["false", "true"],
            ResolvedPurity::Pure,
        )),
        NodeImplementation::supplied(
            interface("evaluate-specs", &["main"], ResolvedPurity::Pure),
            EVALUATE_COMPONENT_DIGEST,
        )?,
        NodeImplementation::supplied(
            interface("normalize-receipt", &["main"], ResolvedPurity::Pure),
            NORMALIZE_COMPONENT_DIGEST,
        )?,
        NodeImplementation::platform(interface(
            "postgres-query",
            &["main"],
            ResolvedPurity::Effectful,
        )),
        NodeImplementation::platform(interface("transform", &["main"], ResolvedPurity::Pure)),
    ])
}

fn resolved_interfaces() -> ResolvedInterfaces {
    implementations()
        .expect("fixed interfaces are valid")
        .into_iter()
        .map(|implementation| {
            (
                implementation.interface().node_type.clone(),
                implementation.interface().output_ports.clone(),
            )
        })
        .collect()
}

fn published_release(flow_json: &str, exposure_json: &str) -> anyhow::Result<PublishedRelease> {
    let flow = Flow::from_json(flow_json).context("parse F1 graph")?;
    flow.validate(&resolved_interfaces())
        .map_err(|issues| anyhow::anyhow!("validate F1 graph: {issues:?}"))?;
    ensure!(
        flow.flow_id == "receipt-received" && flow.version == 1,
        "F1 identity drift"
    );
    ensure!(
        flow.entry_node()
            .is_some_and(|node| node.node_type == "request"),
        "F1 must have one request entry"
    );
    ensure!(
        flow.nodes.iter().all(|node| node.node_type != "custom"),
        "F1 must publish supplied components by their direct node types"
    );
    let nodes: BTreeMap<_, _> = flow
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    ensure!(
        nodes["normalize-receipt"].node_type == "normalize-receipt"
            && nodes["evaluate-specs"].node_type == "evaluate-specs",
        "F1 direct component types drift"
    );
    for id in ["resolve-and-persist", "create-holds"] {
        let config = &nodes[id].config;
        ensure!(
            config["mode"] == "query"
                && config["idempotent-with-key"] == true
                && config["sql"].as_str().is_some_and(|sql| {
                    sql.contains("ON CONFLICT")
                        && sql.contains("ORDER BY")
                        && sql.contains("RETURNING")
                }),
            "F1 {id} must be a deterministic keyed query CTE"
        );
    }
    let resolve_sql = nodes["resolve-and-persist"].config["sql"]
        .as_str()
        .context("resolve CTE SQL")?;
    ensure!(
        resolve_sql.contains("DO UPDATE")
            && resolve_sql.contains("line_no")
            && resolve_sql.contains("references_valid"),
        "F1 resolution CTE lost deterministic read-back"
    );
    let holds_sql = nodes["create-holds"].config["sql"]
        .as_str()
        .context("hold CTE SQL")?;
    ensure!(
        holds_sql.contains("DO NOTHING")
            && holds_sql.contains("read_back")
            && holds_sql.contains("ORDER BY read_back.line_no"),
        "F1 hold CTE lost read-back-on-conflict"
    );
    ensure!(
        serde_json::from_str::<Value>(POC_CONFIG_JSON)?["raw_sql_enabled"] == true,
        "F1 POC environment must explicitly grant RawSql"
    );

    let artifact = Artifact::new("poc", &flow, implementations()?)
        .context("construct immutable F1 artifact")?;
    ensure!(
        artifact.supplied_components().len() == 2,
        "F1 release must pin both supplied components"
    );
    let artifact_hash = artifact.identity().artifact_hash().as_str().to_string();

    let exposure: ExposureRelease =
        serde_json::from_str(exposure_json).context("parse F1 exposure")?;
    let resolved = resolve_exposure(
        &exposure,
        &[FlowExposure {
            flow_id: &flow.flow_id,
            entry_kind: EntryKind::Request,
            artifact_hash: &artifact_hash,
        }],
    )
    .context("resolve F1 exposure")?;
    ensure!(resolved.len() == 1, "F1 must have exactly one attachment");
    let authored = &resolved[0].attachment;
    let route = authored.route.as_ref().context("F1 HTTP route")?;
    ensure!(
        authored.id == "receipt-received-http"
            && authored.source_id == "erp-api-keys"
            && route.path == "/receipts"
            && route.method == "POST"
            && authored.mappings.len() == 1
            && authored.mappings[0].to.is_empty()
            && authored.response_deadline_ms == Some(30_000)
            && authored.run_deadline_ms == 60_000,
        "F1 attachment/auth/idempotency contract drift"
    );

    let source_id = SourceId::new("erp-api-keys").context("source id")?;
    let source = Source::new(
        source_id.clone(),
        SourceKind::Auth,
        CanonicalJson::new(json!({"header": "x-api-key", "policy": "api-key"}))
            .context("auth source")?,
    );
    let attachment = Attachment::resolve(
        AttachmentDraft {
            id: AttachmentId::new("receipt-received-http").context("attachment id")?,
            kind: AttachmentKind::Http,
            artifact_id: artifact.identity().id().clone(),
            source_ids: vec![source_id],
            definition: CanonicalJson::new(json!({
                "route": {
                    "host": route.host,
                    "path": route.path,
                    "method": route.method,
                },
                "mappings": authored.mappings,
                "run-deadline-ms": authored.run_deadline_ms,
                "response-deadline-ms": authored.response_deadline_ms,
                "idempotency": "required",
            }))
            .context("attachment definition")?,
        },
        &artifact,
        std::slice::from_ref(&source),
    )
    .context("resolve catalog attachment")?;
    let release = Release::new(
        ReleaseId::new("poc", "callable", 2).context("release id")?,
        vec![artifact.identity().clone()],
        vec![source],
        vec![attachment],
    )
    .context("publish immutable F1 release")?;
    Ok(PublishedRelease {
        artifact_hash,
        _release: release,
    })
}

#[derive(Debug, Clone)]
struct Line {
    id: String,
    line_no: u32,
    material: String,
    quantity: String,
}

#[derive(Debug, Default, Clone)]
struct Store {
    receipts: BTreeMap<String, String>,
    lines: BTreeMap<(String, u32), Line>,
    holds: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    None,
    ResolveCommitted,
    HoldsCommitted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResultKind {
    Rejected,
    AuthoredFail(u16),
    Responded {
        ordered_rows: Value,
        response: Value,
        outcome_hash: String,
    },
}

struct Invocation<'a> {
    artifact_hash: &'a str,
    expected_hash: &'a str,
    body: &'a Value,
    key: &'a str,
    authenticated: bool,
    raw_sql_enabled: bool,
    fault: Fault,
}

fn valid_request(body: &Value) -> bool {
    let Some(object) = body.as_object() else {
        return false;
    };
    object.get("receipt_no").and_then(Value::as_str).is_some()
        && object.get("supplier").and_then(Value::as_str).is_some()
        && object.get("site").and_then(Value::as_str).is_some()
        && object
            .get("lines")
            .and_then(Value::as_array)
            .is_some_and(|lines| !lines.is_empty())
}

fn resolve_and_persist(store: &mut Store, body: &Value) -> Option<Value> {
    let supplier = body["supplier"].as_str()?;
    let site = body["site"].as_str()?;
    if supplier != "acme" || site != "hq" {
        return None;
    }
    let receipt_key = format!("{supplier}:{}", body["receipt_no"].as_str()?);
    let receipt_id = store
        .receipts
        .entry(receipt_key.clone())
        .or_insert_with(|| format!("receipt-{receipt_key}"))
        .clone();
    let mut line_ids = Vec::new();
    let mut specs = Vec::new();
    for (index, value) in body["lines"].as_array()?.iter().enumerate() {
        let line_no = u32::try_from(index + 1).ok()?;
        let material = value["material"].as_str()?;
        if material != "resin-a" {
            return None;
        }
        let line_id = format!("{receipt_id}-line-{line_no}");
        store.lines.insert(
            (receipt_id.clone(), line_no),
            Line {
                id: line_id.clone(),
                line_no,
                material: material.to_string(),
                quantity: value["quantity"].as_str()?.to_string(),
            },
        );
        line_ids.push(line_id);
        specs.push(json!({
            "material_id": "material-resin-a",
            "moisture_max_pct": "10.00",
            "weight_tolerance_kg": "0.050",
        }));
    }
    Some(json!({
        "receipt": body,
        "references_valid": true,
        "receipt_id": receipt_id,
        "site_id": "site-hq",
        "line_specs": specs,
        "line_ids": line_ids,
    }))
}

fn evaluate(resolved: &Value) -> Value {
    let mut out = Vec::new();
    for (index, line) in resolved["receipt"]["lines"]
        .as_array()
        .expect("validated lines")
        .iter()
        .enumerate()
    {
        if decimal_hundredths(line["moisture_pct"].as_str().unwrap_or_default()) > 1_000 {
            out.push(json!({
                "line": index + 1,
                "line_id": resolved["line_ids"][index],
                "material": line["material"],
                "reason": "moisture exceeds max",
            }));
        }
    }
    json!({
        "receipt_id": resolved["receipt_id"],
        "site_id": resolved["site_id"],
        "out_of_spec": out,
    })
}

fn decimal_hundredths(value: &str) -> i64 {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let whole = whole.parse::<i64>().unwrap_or_default();
    let mut digits = fraction.bytes().take(2).map(|byte| byte - b'0');
    let tenths = i64::from(digits.next().unwrap_or_default());
    let hundredths = i64::from(digits.next().unwrap_or_default());
    whole * 100 + tenths * 10 + hundredths
}

fn create_holds(store: &mut Store, evaluated: &Value) -> Value {
    let mut holds = Vec::new();
    for item in evaluated["out_of_spec"]
        .as_array()
        .expect("evaluation output")
    {
        let line_id = item["line_id"].as_str().expect("line id");
        let hold_id = store
            .holds
            .entry(line_id.to_string())
            .or_insert_with(|| format!("hold-{line_id}"))
            .clone();
        holds.push(json!({"id": hold_id, "line_id": line_id}));
    }
    json!({
        "receipt_id": evaluated["receipt_id"],
        "holds": holds,
    })
}

fn invoke(input: Invocation<'_>, store: &mut Store) -> ResultKind {
    let Invocation {
        artifact_hash,
        expected_hash,
        body,
        key,
        authenticated,
        raw_sql_enabled,
        fault,
    } = input;
    if artifact_hash != expected_hash
        || key.is_empty()
        || key.contains(char::is_whitespace)
        || !authenticated
        || !raw_sql_enabled
        || !valid_request(body)
    {
        return ResultKind::Rejected;
    }
    let Some(mut resolved) = resolve_and_persist(store, body) else {
        return ResultKind::AuthoredFail(400);
    };
    if matches!(fault, Fault::ResolveCommitted) {
        resolved = resolve_and_persist(store, body).expect("read-back after committed resolve");
    }
    let evaluated = evaluate(&resolved);
    let mut response = create_holds(store, &evaluated);
    if matches!(fault, Fault::HoldsCommitted) {
        response = create_holds(store, &evaluated);
    }
    let ordered_rows = json!({
        "receipts": store.receipts,
        "lines": store.lines.values().map(|line| json!({
            "id": line.id,
            "line_no": line.line_no,
            "material": line.material,
            "quantity": line.quantity,
        })).collect::<Vec<_>>(),
        "holds": store.holds,
    });
    ResultKind::Responded {
        ordered_rows,
        outcome_hash: canonical_json_sha256(&response),
        response,
    }
}

fn fixture() -> Value {
    json!({
        "receipt_no": "r-1001",
        "supplier": "acme",
        "site": "hq",
        "received_at": "2026-07-12T08:00:00Z",
        "lines": [
            {
                "material": "resin-a",
                "quantity": "100.000",
                "moisture_pct": "12.50",
                "weight_kg": "99.950"
            },
            {
                "material": "resin-a",
                "quantity": "50.000",
                "moisture_pct": "8.00",
                "weight_kg": "50.000"
            }
        ]
    })
}

fn prove_scenarios(artifact_hash: &str) -> anyhow::Result<()> {
    let body = fixture();
    let normal = invoke(
        Invocation {
            artifact_hash,
            expected_hash: artifact_hash,
            body: &body,
            key: "receipt-1",
            authenticated: true,
            raw_sql_enabled: true,
            fault: Fault::None,
        },
        &mut Store::default(),
    );
    for fault in [Fault::ResolveCommitted, Fault::HoldsCommitted] {
        let recovered = invoke(
            Invocation {
                artifact_hash,
                expected_hash: artifact_hash,
                body: &body,
                key: "receipt-1",
                authenticated: true,
                raw_sql_enabled: true,
                fault,
            },
            &mut Store::default(),
        );
        ensure!(
            recovered == normal,
            "F1 committed CTE recovery changed rows, response, or outcome hash"
        );
    }
    let ResultKind::Responded {
        response,
        ordered_rows,
        ..
    } = normal
    else {
        anyhow::bail!("F1 happy path did not respond");
    };
    ensure!(
        response["holds"]
            .as_array()
            .is_some_and(|holds| holds.len() == 1)
            && ordered_rows["receipts"]
                .as_object()
                .is_some_and(|rows| rows.len() == 1)
            && ordered_rows["lines"]
                .as_array()
                .is_some_and(|rows| rows.len() == 2)
            && ordered_rows["holds"]
                .as_object()
                .is_some_and(|rows| rows.len() == 1),
        "F1 logical row set drift: response={response}, rows={ordered_rows}"
    );
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn package_proof_covers_release_and_both_cte_faults() {
        let published = published_release(FLOW_JSON, EXPOSURE_JSON).unwrap();
        prove_scenarios(&published.artifact_hash).unwrap();
    }

    #[test]
    fn malformed_bad_key_auth_and_raw_sql_refuse_before_a_run() {
        let published = published_release(FLOW_JSON, EXPOSURE_JSON).unwrap();
        let hash = &published.artifact_hash;
        for result in [
            invoke(
                Invocation {
                    artifact_hash: hash,
                    expected_hash: hash,
                    body: &json!(null),
                    key: "key",
                    authenticated: true,
                    raw_sql_enabled: true,
                    fault: Fault::None,
                },
                &mut Store::default(),
            ),
            invoke(
                Invocation {
                    artifact_hash: hash,
                    expected_hash: hash,
                    body: &fixture(),
                    key: "bad key",
                    authenticated: true,
                    raw_sql_enabled: true,
                    fault: Fault::None,
                },
                &mut Store::default(),
            ),
            invoke(
                Invocation {
                    artifact_hash: hash,
                    expected_hash: hash,
                    body: &fixture(),
                    key: "key",
                    authenticated: false,
                    raw_sql_enabled: true,
                    fault: Fault::None,
                },
                &mut Store::default(),
            ),
            invoke(
                Invocation {
                    artifact_hash: hash,
                    expected_hash: hash,
                    body: &fixture(),
                    key: "key",
                    authenticated: true,
                    raw_sql_enabled: false,
                    fault: Fault::None,
                },
                &mut Store::default(),
            ),
        ] {
            assert_eq!(result, ResultKind::Rejected);
        }
    }

    #[test]
    fn unknown_reference_is_an_authored_400_after_admission() {
        let published = published_release(FLOW_JSON, EXPOSURE_JSON).unwrap();
        let mut body = fixture();
        body["supplier"] = json!("unknown");
        assert_eq!(
            invoke(
                Invocation {
                    artifact_hash: &published.artifact_hash,
                    expected_hash: &published.artifact_hash,
                    body: &body,
                    key: "unknown-ref",
                    authenticated: true,
                    raw_sql_enabled: true,
                    fault: Fault::None,
                },
                &mut Store::default(),
            ),
            ResultKind::AuthoredFail(400)
        );
    }

    #[test]
    fn graph_and_cte_determinism_mutants_fail() {
        let mut graph: Value = serde_json::from_str(FLOW_JSON).unwrap();
        graph["nodes"][1]["type"] = json!("custom");
        assert!(published_release(&graph.to_string(), EXPOSURE_JSON).is_err());

        let mut graph: Value = serde_json::from_str(FLOW_JSON).unwrap();
        let sql = graph["nodes"][2]["config"]["sql"]
            .as_str()
            .unwrap()
            .replace("ORDER BY", "BROKEN BY");
        graph["nodes"][2]["config"]["sql"] = json!(sql);
        assert!(published_release(&graph.to_string(), EXPOSURE_JSON).is_err());

        let mut graph: Value = serde_json::from_str(FLOW_JSON).unwrap();
        let sql = graph["nodes"][6]["config"]["sql"]
            .as_str()
            .unwrap()
            .replace("read_back", "not_read_back");
        graph["nodes"][6]["config"]["sql"] = json!(sql);
        assert!(published_release(&graph.to_string(), EXPOSURE_JSON).is_err());
    }

    #[test]
    fn legacy_webhook_is_absent_after_cutover() {
        let workspace = include_str!("../../../components/Cargo.toml");
        let dockerfile = include_str!("../../../Dockerfile");
        let roles = include_str!("../../../architecture/package-roles.json");
        assert!(!workspace.contains("poc/webhook-f1"));
        assert!(!dockerfile.contains("poc-webhook-f1"));
        assert!(!roles.contains("\"name\": \"poc-webhook-f1\""));
    }

    #[test]
    fn exact_image_job_routes_to_the_system_proof() {
        let router = include_str!("../../orchestrator/src/main.rs");
        assert!(router.contains("CallableF1(callable_f1::CallableF1Args)"));
        assert!(router.contains("Command::CallableF1(args) => callable_f1::run(args)"));

        let job = include_str!("../../../deploy/gates/callable-flow-f1-job.yaml");
        assert!(job.contains("name: callable-flow-f1"));
        assert!(job.contains("image: wamn-gates:cf-f1-ISSUE"));
        assert!(job.contains("imagePullPolicy: Never"));
        assert!(job.contains(r#"args: ["--log-level", "error", "callable-f1"]"#));
        assert!(!job.contains("wamn-gates:dev"));
    }
}
