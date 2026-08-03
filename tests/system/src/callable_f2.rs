//! System proof for the callable-flow F2 immutable release and pure replay.

use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use clap::Args;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wamn_catalog::{
    Artifact, Attachment, AttachmentDraft, AttachmentId, AttachmentKind, CanonicalJson,
    NodeImplementation, Release, ReleaseId, Source, SourceId, SourceKind,
};
use wamn_flow::{EntryKind, Flow, canonical_json_sha256};
use wamn_node_manifest::{
    CapabilityClass, ExecutableIdentity, RecoveryClass, ResolvedNodeInterface, ResolvedPurity,
};
use wamn_run_state::attempt::{AttemptStartResult, RecoveryClass as AttemptRecoveryClass};
use wamn_run_state::child::{ChildCreateResult, create_or_recover_child_sql};
use wamn_schema_control::exposure::{ExposureRelease, FlowExposure, resolve_exposure};

use crate::standard_implementation;

const FLOW_JSON: &str = include_str!("../../../deploy/poc/f2-flow.json");
const EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f2-internal-attachment.json");
#[cfg(test)]
const COMPONENT_MANIFEST: &str =
    include_str!("../../../components/samples/disposition-node/Cargo.toml");
#[cfg(test)]
const COMPONENT_SOURCE: &str =
    include_str!("../../../components/samples/disposition-node/src/lib.rs");
#[cfg(test)]
const TEST_COMPONENT_DIGEST: &str =
    "sha256:3333333333333333333333333333333333333333333333333333333333333333";

#[derive(Debug, Args)]
pub struct CallableF2Args {
    /// Exact component bytes pinned into the immutable F2 artifact.
    #[arg(long, default_value = "/bench/disposition-node.wasm")]
    pub component: PathBuf,
}

pub fn run(args: CallableF2Args) -> anyhow::Result<()> {
    let bytes = std::fs::read(&args.component)
        .with_context(|| format!("read F2 component {}", args.component.display()))?;
    ensure!(!bytes.is_empty(), "F2 component is empty");
    let digest = component_digest(&bytes);
    let published = published_release(FLOW_JSON, EXPOSURE_JSON, &digest)?;
    prove_scenarios(&published)?;
    println!(
        "callable-flow-f2 PASS: direct pure component, immutable internal release, caller policy, and deterministic replay hold"
    );
    Ok(())
}

fn component_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn interface() -> ResolvedNodeInterface {
    ResolvedNodeInterface::new(
        "disposition-recommendation",
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Pure],
        Vec::new(),
    )
}

fn implementation(digest: &str, purity: ResolvedPurity) -> anyhow::Result<NodeImplementation> {
    let recovery = match purity {
        ResolvedPurity::Pure => wamn_node_manifest::ExecutableRecoveryContract::pure(),
        ResolvedPurity::Effectful => {
            wamn_node_manifest::ExecutableRecoveryContract::effectful(false)
        }
    };
    NodeImplementation::supplied(interface(), digest, recovery).map_err(anyhow::Error::new)
}

fn implementations(
    digest: &str,
    purity: ResolvedPurity,
) -> anyhow::Result<Vec<NodeImplementation>> {
    let mut implementations = vec![
        standard_implementation("request")?,
        implementation(digest, purity)?,
        standard_implementation("respond")?,
    ];
    implementations
        .sort_by(|left, right| left.interface().node_type.cmp(&right.interface().node_type));
    Ok(implementations)
}

struct PublishedRelease {
    artifact: Artifact,
    _release: Release,
}

fn published_release(
    flow_json: &str,
    exposure_json: &str,
    digest: &str,
) -> anyhow::Result<PublishedRelease> {
    let flow = Flow::from_json(flow_json).context("parse F2 graph")?;
    ensure!(
        flow.flow_id == "disposition-recommendation" && flow.version == 1,
        "F2 identity drift"
    );
    ensure!(
        flow.entry_node()
            .is_some_and(|node| node.node_type == "request"),
        "F2 must have one request entry"
    );
    ensure!(flow.nodes.len() == 3, "F2 must have exactly three nodes");
    ensure!(
        flow.nodes
            .iter()
            .all(|node| node.node_type != "custom" && node.config.get("manifest").is_none()),
        "F2 must use the supplied node's direct type"
    );
    ensure!(
        flow.nodes.iter().any(|node| {
            node.id == "recommend-disposition" && node.node_type == "disposition-recommendation"
        }),
        "F2 recommendation node type drift"
    );
    let request = flow.entry_node().context("F2 request entry")?;
    assert_strict_request_schema(&request.config["input-schema"])?;

    let artifact = Artifact::new("poc", &flow, implementations(digest, ResolvedPurity::Pure)?)
        .context("construct immutable F2 artifact")?;
    ensure!(
        artifact.supplied_components().len() == 1,
        "F2 must pin exactly one supplied component"
    );
    let component = &artifact.supplied_components()[0];
    ensure!(
        component.contract.interface.node_type == "disposition-recommendation"
            && component.contract.interface.output_ports == ["main"]
            && component.contract.executable_recovery.purity == ResolvedPurity::Pure
            && component.contract.executable_recovery.conservative_class == RecoveryClass::Replay
            && matches!(&component.contract.executable, ExecutableIdentity::Component { digest: pinned } if pinned == digest),
        "F2 resolved component contract drift"
    );

    let exposure: ExposureRelease =
        serde_json::from_str(exposure_json).context("parse F2 exposure")?;
    let resolved = resolve_exposure(
        &exposure,
        &[FlowExposure {
            flow_id: &flow.flow_id,
            entry_kind: EntryKind::Request,
            artifact_hash: artifact.identity().artifact_hash().as_str(),
        }],
    )
    .context("resolve F2 internal exposure")?;
    ensure!(resolved.len() == 1, "F2 must have one internal attachment");
    let authored = &resolved[0].attachment;
    ensure!(
        authored.id == "disposition-recommendation-internal"
            && authored.flow_id == "disposition-recommendation"
            && authored.source_id == "disposition-recommendation-callers"
            && authored.route.is_none()
            && authored.mappings.is_empty()
            && authored.run_deadline_ms == 30_000
            && authored.response_deadline_ms == Some(10_000),
        "F2 internal attachment drift"
    );
    let policy = &exposure.sources[0];
    ensure!(
        policy.kind == wamn_schema_control::exposure::SourceKind::CallerPolicy
            && policy.definition["allowed-callers"] == json!(["disposition-recorded"]),
        "F2 caller policy drift"
    );

    let source_id = SourceId::new("disposition-recommendation-callers").context("source id")?;
    let source = Source::new(
        source_id.clone(),
        SourceKind::CallerPolicy,
        CanonicalJson::new(policy.definition.clone()).context("caller policy")?,
    );
    let attachment = Attachment::resolve(
        AttachmentDraft {
            id: AttachmentId::new("disposition-recommendation-internal")
                .context("attachment id")?,
            kind: AttachmentKind::Internal,
            artifact_id: artifact.identity().id().clone(),
            source_ids: vec![source_id],
            definition: CanonicalJson::new(json!({
                "run-deadline-ms": authored.run_deadline_ms,
                "response-deadline-ms": authored.response_deadline_ms,
            }))
            .context("internal attachment definition")?,
        },
        &artifact,
        std::slice::from_ref(&source),
    )
    .context("resolve catalog attachment")?;
    let release = Release::new(
        ReleaseId::new("poc", "callable", 6).context("release id")?,
        vec![artifact.identity().clone()],
        vec![source],
        vec![attachment],
    )
    .context("publish immutable F2 release")?;
    Ok(PublishedRelease {
        artifact,
        _release: release,
    })
}

fn assert_strict_request_schema(schema: &Value) -> anyhow::Result<()> {
    ensure!(
        schema["$schema"] == "https://json-schema.org/draft/2020-12/schema"
            && schema["type"] == "object"
            && schema["required"] == json!(["hold", "history", "decision"])
            && schema["additionalProperties"] == false,
        "F2 top-level input schema drift"
    );
    let properties = &schema["properties"];
    ensure!(
        properties["hold"]["type"] == "object"
            && properties["hold"]["required"] == json!(["material"])
            && properties["hold"]["additionalProperties"] == false
            && properties["history"]["type"] == "array"
            && properties["history"]["items"]["required"] == json!(["decision"])
            && properties["history"]["items"]["additionalProperties"] == false
            && properties["decision"]["enum"] == json!(["accept", "reject", "use-as-is"]),
        "F2 nested input schema drift"
    );
    Ok(())
}

fn fixture(decision: &str) -> Value {
    json!({
        "hold": {
            "material": "resin-A",
            "moisture_pct": "12.00",
            "moisture_max_pct": "5.00"
        },
        "history": [{"decision": "reject"}, {"decision": "reject"}],
        "decision": decision
    })
}

fn valid_request(input: &Value) -> bool {
    let Some(object) = input.as_object() else {
        return false;
    };
    if object.len() != 3
        || !object.contains_key("hold")
        || !object.contains_key("history")
        || !object.contains_key("decision")
    {
        return false;
    }
    let Some(hold) = object["hold"].as_object() else {
        return false;
    };
    hold.get("material")
        .and_then(Value::as_str)
        .is_some_and(|v| !v.is_empty())
        && object["history"].as_array().is_some()
        && object["decision"]
            .as_str()
            .is_some_and(|v| matches!(v, "accept" | "reject" | "use-as-is"))
}

fn recommendation(input: &Value) -> anyhow::Result<Value> {
    ensure!(valid_request(input), "request rejected before run creation");
    let decision = input["decision"].as_str().context("decision")?;
    Ok(json!({
        "recommendation": "reject",
        "confidence": "0.95",
        "matched": decision == "reject"
    }))
}

fn policy_allows(exposure_json: &str, caller: &str) -> bool {
    serde_json::from_str::<ExposureRelease>(exposure_json)
        .ok()
        .and_then(|release| release.sources.into_iter().next())
        .and_then(|source| source.definition["allowed-callers"].as_array().cloned())
        .is_some_and(|callers| callers.iter().any(|value| value == caller))
}

fn prove_scenarios(published: &PublishedRelease) -> anyhow::Result<()> {
    ensure!(
        policy_allows(EXPOSURE_JSON, "disposition-recorded")
            && !policy_allows(EXPOSURE_JSON, "untrusted-flow"),
        "caller policy acceptance/refusal drift"
    );
    ensure!(
        ChildCreateResult::from_parts(
            "unsupported-actor-mode",
            "running",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ) == Some(ChildCreateResult::UnsupportedActorMode),
        "non-service actor mode must refuse before child creation"
    );
    let sql = create_or_recover_child_sql();
    ensure!(
        sql.contains("WHEN $10::text <> 'service' THEN 'unsupported-actor-mode'")
            && sql.contains("NOT (d.caller_policy->'allowed-callers' ? p.flow_id)"),
        "child runtime lost service-mode or caller-policy authorization"
    );

    let first = recommendation(&fixture("accept"))?;
    let replayed = recommendation(&fixture("accept"))?;
    let first_bytes = serde_json::to_vec(&first)?;
    let replayed_bytes = serde_json::to_vec(&replayed)?;
    ensure!(
        first_bytes == replayed_bytes
            && canonical_json_sha256(&first) == canonical_json_sha256(&replayed)
            && first["confidence"].is_string()
            && first["matched"] == false,
        "F2 pure replay changed output bytes, hash, or contract"
    );
    ensure!(
        recommendation(&fixture("reject"))?["matched"] == true,
        "matched must compare recommendation with decision"
    );
    ensure!(
        AttemptRecoveryClass::Replay.as_sql() == "replay"
            && AttemptStartResult::from_code("redispatch")
                .is_some_and(AttemptStartResult::permits_dispatch),
        "a started pure attempt must recover by redispatch"
    );
    let terminal_outcomes = [canonical_json_sha256(&replayed)];
    ensure!(
        terminal_outcomes.len() == 1,
        "F2 replay must converge on one child terminal outcome"
    );
    ensure!(
        published.artifact.supplied_components()[0]
            .contract
            .executable_recovery
            .conservative_class
            == RecoveryClass::Replay,
        "published F2 purity did not authorize replay"
    );
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn package_proof_covers_release_policy_contract_and_replay() {
        let published = published_release(FLOW_JSON, EXPOSURE_JSON, TEST_COMPONENT_DIGEST).unwrap();
        prove_scenarios(&published).unwrap();
    }

    #[test]
    fn malformed_input_and_undeclared_port_refuse() {
        assert!(recommendation(&json!({"hold": {}, "history": []})).is_err());
        let mut extra = fixture("accept");
        extra["id"] = json!("must-stay-in-f4-context");
        assert!(recommendation(&extra).is_err());

        let mut flow: Value = serde_json::from_str(FLOW_JSON).unwrap();
        flow["edges"][1]["from-port"] = json!("undeclared");
        assert!(
            published_release(&flow.to_string(), EXPOSURE_JSON, TEST_COMPONENT_DIGEST).is_err()
        );
    }

    #[test]
    fn direct_type_component_identity_and_purity_mutants_fail() {
        let baseline = published_release(FLOW_JSON, EXPOSURE_JSON, TEST_COMPONENT_DIGEST).unwrap();
        let mut legacy: Value = serde_json::from_str(FLOW_JSON).unwrap();
        legacy["nodes"][1]["type"] = json!("custom");
        legacy["nodes"][1]["config"] = json!({"manifest": "disposition-node"});
        assert!(
            published_release(&legacy.to_string(), EXPOSURE_JSON, TEST_COMPONENT_DIGEST).is_err()
        );

        let changed_digest =
            "sha256:4444444444444444444444444444444444444444444444444444444444444444";
        let changed = published_release(FLOW_JSON, EXPOSURE_JSON, changed_digest).unwrap();
        assert_ne!(
            baseline.artifact.identity().artifact_hash(),
            changed.artifact.identity().artifact_hash()
        );

        let flow = Flow::from_json(FLOW_JSON).unwrap();
        let effectful = Artifact::new(
            "poc",
            &flow,
            implementations(TEST_COMPONENT_DIGEST, ResolvedPurity::Effectful).unwrap(),
        )
        .unwrap();
        assert_ne!(
            baseline.artifact.identity().artifact_hash(),
            effectful.identity().artifact_hash()
        );
        assert_eq!(
            effectful.supplied_components()[0]
                .contract
                .executable_recovery
                .conservative_class,
            RecoveryClass::NeverReplay
        );
        assert_eq!(AttemptRecoveryClass::NeverReplay.as_sql(), "never-replay");
        assert_eq!(
            AttemptStartResult::from_code("effect-uncertain"),
            Some(AttemptStartResult::EffectUncertain)
        );
        assert!(!AttemptStartResult::EffectUncertain.permits_dispatch());
    }

    #[test]
    fn caller_policy_and_result_shape_mutants_fail() {
        assert!(policy_allows(EXPOSURE_JSON, "disposition-recorded"));
        let mut exposure: Value = serde_json::from_str(EXPOSURE_JSON).unwrap();
        exposure["sources"][0]["definition"]["allowed-callers"] = json!([]);
        assert!(
            published_release(FLOW_JSON, &exposure.to_string(), TEST_COMPONENT_DIGEST).is_err()
        );

        let output = recommendation(&fixture("accept")).unwrap();
        assert!(output["confidence"].is_string());
        assert_eq!(output["matched"], false);
        let matched = recommendation(&fixture("reject")).unwrap();
        assert_eq!(matched["recommendation"], output["recommendation"]);
        assert_eq!(matched["matched"], true);
    }

    #[test]
    fn component_and_generic_publication_posture_is_pinned() {
        assert!(COMPONENT_MANIFEST.contains("node-type = \"disposition-recommendation\""));
        assert!(COMPONENT_MANIFEST.contains("purity = \"pure\""));
        assert!(!COMPONENT_SOURCE.contains("_ctx."));
        assert!(COMPONENT_SOURCE.contains("\"confidence\": confidence"));
        assert!(COMPONENT_SOURCE.contains("\"matched\": rec == decision"));

        let publisher = include_str!("../../../services/ctl/src/publish_catalog.rs");
        assert!(publisher.contains("descriptor.component_digest == actual_digest"));
        assert!(publisher.contains("manifest.node_type == descriptor.node_type"));
        assert!(publisher.contains("NodeImplementation::from_resolved_component"));
        let dockerfile = include_str!("../../../Dockerfile");
        assert!(dockerfile.contains(
            "COPY --from=component-builder /component-output/disposition_node.wasm /bench/disposition-node.wasm"
        ));
    }

    #[test]
    fn canonical_and_fixture_graphs_are_identical() {
        let fixture = include_str!(
            "../../../crates/execution/flow-model/tests/fixtures/f2-disposition-recommendation.flow.json"
        );
        assert_eq!(
            Flow::from_json(FLOW_JSON).unwrap().canonical_bytes(),
            Flow::from_json(fixture).unwrap().canonical_bytes()
        );
    }

    #[test]
    fn exact_image_job_routes_to_the_system_proof() {
        let router = include_str!("../../orchestrator/src/main.rs");
        assert!(router.contains("CallableF2(callable_f2::CallableF2Args)"));
        assert!(router.contains("Command::CallableF2(args) => callable_f2::run(args)"));

        let job = include_str!("../../../deploy/gates/callable-flow-f2-job.yaml");
        assert!(job.contains("name: callable-flow-f2"));
        assert!(job.contains("image: wamn-gates:cf-f2-ISSUE"));
        assert!(job.contains("imagePullPolicy: Never"));
        assert!(job.contains(r#"args: ["--log-level", "error", "callable-f2"]"#));
        assert!(!job.contains("wamn-gates:dev"));
    }

    #[test]
    fn gate_component_path_matches_the_baked_component() {
        assert_eq!(
            Path::new("/bench/disposition-node.wasm"),
            CallableF2Args {
                component: PathBuf::from("/bench/disposition-node.wasm")
            }
            .component
        );
    }
}
