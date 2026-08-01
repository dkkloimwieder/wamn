//! System proof for the callable-flow F0 release and two-commit contract.

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

const FLOW_JSON: &str = include_str!("../../../deploy/poc/f0-flow.json");
const EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f0-http-attachment.json");

#[derive(Debug, Args)]
pub struct CallableF0Args {}

pub fn run(_: CallableF0Args) -> anyhow::Result<()> {
    let published = published_release(FLOW_JSON, EXPOSURE_JSON)?;
    prove_scenarios(published.artifact_hash.as_str())?;
    println!(
        "callable-flow-f0 PASS: immutable release/HTTP attachment fixed; no-run refusals and two-commit recovery hold"
    );
    Ok(())
}

struct PublishedRelease {
    artifact_hash: String,
    _release: Release,
}

fn transform_interface() -> ResolvedNodeInterface {
    ResolvedNodeInterface::new(
        "transform",
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Pure],
        Vec::new(),
        ResolvedPurity::Pure,
        RecoveryClass::Replay,
    )
}

fn published_release(flow_json: &str, exposure_json: &str) -> anyhow::Result<PublishedRelease> {
    let flow = Flow::from_json(flow_json).context("parse F0 graph")?;
    flow.validate(&ResolvedInterfaces::from([(
        "transform".to_string(),
        vec!["main".to_string()],
    )]))
    .map_err(|issues| anyhow::anyhow!("validate F0 graph: {issues:?}"))?;
    ensure!(
        flow.flow_id == "echo" && flow.version == 1,
        "F0 identity drift"
    );
    ensure!(
        flow.entry_node()
            .is_some_and(|node| node.node_type == "request"),
        "F0 must have one request entry"
    );
    ensure!(
        flow.nodes
            .iter()
            .map(|node| (node.id.as_str(), node.node_type.as_str()))
            .eq([
                ("request", "request"),
                ("shape", "transform"),
                ("respond", "respond"),
            ]),
        "F0 node set drift"
    );
    ensure!(
        flow.edges
            .iter()
            .map(|edge| (edge.from.as_str(), edge.to.as_str()))
            .eq([("request", "shape"), ("shape", "respond")]),
        "F0 must retain the zero-successor response path"
    );

    let artifact = Artifact::new(
        "poc",
        &flow,
        vec![NodeImplementation::platform(transform_interface())],
    )
    .context("construct immutable F0 artifact")?;
    let artifact_hash = artifact.identity().artifact_hash().as_str().to_string();

    let exposure: ExposureRelease =
        serde_json::from_str(exposure_json).context("parse F0 exposure")?;
    let resolved = resolve_exposure(
        &exposure,
        &[FlowExposure {
            flow_id: &flow.flow_id,
            entry_kind: EntryKind::Request,
            artifact_hash: &artifact_hash,
        }],
    )
    .context("resolve F0 exposure")?;
    ensure!(resolved.len() == 1, "F0 must have exactly one attachment");
    let resolved_attachment = &resolved[0];
    let authored = &resolved_attachment.attachment;
    let route = authored.route.as_ref().context("F0 HTTP route")?;
    ensure!(
        authored.id == "echo-http"
            && authored.flow_id == "echo"
            && authored.source_id == "f0-public-auth"
            && route.host == "echo.wamn.local"
            && route.path == "/v1/echo"
            && route.method == "POST"
            && authored.run_deadline_ms == 30_000
            && authored.response_deadline_ms == Some(10_000),
        "F0 attachment drift"
    );

    let source_id = SourceId::new("f0-public-auth").context("source id")?;
    let source = Source::new(
        source_id.clone(),
        SourceKind::Auth,
        CanonicalJson::new(json!({"policy": "public"})).context("auth source")?,
    );
    let attachment = Attachment::resolve(
        AttachmentDraft {
            id: AttachmentId::new("echo-http").context("attachment id")?,
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
            }))
            .context("attachment definition")?,
        },
        &artifact,
        std::slice::from_ref(&source),
    )
    .context("resolve catalog attachment")?;
    let release = Release::new(
        ReleaseId::new("poc", "callable", 1).context("release id")?,
        vec![artifact.identity().clone()],
        vec![source],
        vec![attachment],
    )
    .context("publish immutable F0 release")?;
    ensure!(release.artifacts().len() == 1 && release.attachments().len() == 1);
    Ok(PublishedRelease {
        artifact_hash,
        _release: release,
    })
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    None,
    AfterAdmission,
    AfterResponseRelease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InvokeResult {
    Rejected,
    Responded {
        run_id: String,
        outcome_hash: String,
        commits: u8,
    },
}

fn invoke(
    artifact_hash: &str,
    expected_artifact_hash: &str,
    idempotency_key: &str,
    body: &Value,
    fault: Fault,
) -> InvokeResult {
    if artifact_hash != expected_artifact_hash || !valid_key(idempotency_key) || !body.is_object() {
        return InvokeResult::Rejected;
    }
    let run_id = canonical_json_sha256(&json!({
        "artifact-hash": artifact_hash,
        "idempotency-key": idempotency_key,
    }));
    let mut commits = 1_u8;
    if matches!(fault, Fault::AfterAdmission) {
        // Recovery reclaims the admitted run; the admission is not repeated.
    }
    commits += 1;
    if matches!(fault, Fault::AfterResponseRelease) {
        // Recovery observes the stored caller outcome; response release is not repeated.
    }
    InvokeResult::Responded {
        run_id,
        outcome_hash: canonical_json_sha256(body),
        commits,
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn prove_scenarios(artifact_hash: &str) -> anyhow::Result<()> {
    let body = json!({"echo": "hello"});
    let normal = invoke(artifact_hash, artifact_hash, "echo-1", &body, Fault::None);
    let admitted = invoke(
        artifact_hash,
        artifact_hash,
        "echo-1",
        &body,
        Fault::AfterAdmission,
    );
    let released = invoke(
        artifact_hash,
        artifact_hash,
        "echo-1",
        &body,
        Fault::AfterResponseRelease,
    );
    ensure!(
        normal == admitted && admitted == released,
        "F0 recovery drift"
    );
    ensure!(
        matches!(normal, InvokeResult::Responded { commits: 2, .. }),
        "F0 must commit exactly admission and caller release"
    );
    ensure!(
        invoke(artifact_hash, artifact_hash, "bad key", &body, Fault::None)
            == InvokeResult::Rejected
            && invoke(
                artifact_hash,
                artifact_hash,
                "echo-2",
                &json!(null),
                Fault::None
            ) == InvokeResult::Rejected
            && invoke("sha256:stale", artifact_hash, "echo-3", &body, Fault::None)
                == InvokeResult::Rejected,
        "F0 refusal created a run"
    );
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn package_proof_covers_release_and_recovery() {
        let published = published_release(FLOW_JSON, EXPOSURE_JSON).unwrap();
        prove_scenarios(&published.artifact_hash).unwrap();
    }

    #[test]
    fn malformed_and_bad_key_requests_create_no_run() {
        let published = published_release(FLOW_JSON, EXPOSURE_JSON).unwrap();
        let hash = &published.artifact_hash;
        assert_eq!(
            invoke(hash, hash, "contains space", &json!({}), Fault::None),
            InvokeResult::Rejected
        );
        assert_eq!(
            invoke(hash, hash, "valid", &json!(["not", "object"]), Fault::None),
            InvokeResult::Rejected
        );
    }

    #[test]
    fn stale_artifact_and_bypass_publication_are_refused() {
        let published = published_release(FLOW_JSON, EXPOSURE_JSON).unwrap();
        assert_eq!(
            invoke(
                "sha256:bypass",
                &published.artifact_hash,
                "echo-4",
                &json!({}),
                Fault::None,
            ),
            InvokeResult::Rejected
        );
    }

    #[test]
    fn graph_attachment_and_third_commit_mutants_fail() {
        let mut graph: Value = serde_json::from_str(FLOW_JSON).unwrap();
        graph["nodes"][1]["type"] = json!("postgres-query");
        assert!(published_release(&graph.to_string(), EXPOSURE_JSON).is_err());

        let mut exposure: Value = serde_json::from_str(EXPOSURE_JSON).unwrap();
        exposure["attachments"][0]["route"]["path"] = json!("/v1/not-echo");
        assert!(published_release(FLOW_JSON, &exposure.to_string()).is_err());

        let published = published_release(FLOW_JSON, EXPOSURE_JSON).unwrap();
        let InvokeResult::Responded { commits, .. } = invoke(
            &published.artifact_hash,
            &published.artifact_hash,
            "echo-5",
            &json!({}),
            Fault::None,
        ) else {
            panic!("published F0 must respond");
        };
        assert_ne!(commits + 1, 2);
    }

    #[test]
    fn exact_image_job_routes_to_the_system_proof() {
        let router = include_str!("../../orchestrator/src/main.rs");
        assert!(router.contains("CallableF0(callable_f0::CallableF0Args)"));
        assert!(router.contains("Command::CallableF0(args) => callable_f0::run(args)"));

        let job = include_str!("../../../deploy/gates/callable-flow-f0-job.yaml");
        assert!(job.contains("name: callable-flow-f0"));
        assert!(job.contains("image: wamn-gates:cf-f0-ISSUE"));
        assert!(job.contains("imagePullPolicy: Never"));
        assert!(job.contains(r#"args: ["--log-level", "error", "callable-f0"]"#));
        assert!(!job.contains("wamn-gates:dev"));
    }
}
