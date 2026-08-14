use wamn_catalog::{
    Artifact, DraftArtifact, ExecutionEffectPolicy, ExecutionPlanBody, ExecutionPlanEdge,
    ExecutionPlanNode, ExecutionPlanV2, ExecutionRuntimeRevision, PinnedDraftArtifact,
    RootTerminalBehavior, StoredValidatedDraftContext, ValidatedDraftIdentity,
    ValidatedDraftIdentityInput, execution_bundle_hash,
};
use wamn_flow::Flow;
use wamn_flow::node_contract::{EffectPolicy, NodeInterface};

const RUNNER: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PROVIDER: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BINDING_BASE: &str =
    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn flow(version: u32) -> Flow {
    Flow::from_json(&format!(
        r#"{{"schema-version":"0.1","flow-id":"draft-flow","version":{version},"nodes":[{{"id":"entry","type":"event"}},{{"id":"step","type":"custom"}}],"edges":[{{"from":"entry","to":"step"}}]}}"#
    ))
    .unwrap()
}

fn implementations() -> Vec<NodeInterface> {
    ["custom", "event"]
        .into_iter()
        .map(|node_type| NodeInterface {
            node_type: node_type.to_string(),
            output_ports: vec!["main".into()],
            capabilities: Vec::new(),
            connection_requirements: Vec::new(),
            effect_policy: EffectPolicy::Pure,
        })
        .collect()
}

fn plan(root_artifact_hash: &str) -> ExecutionPlanV2 {
    ExecutionPlanV2::new(
        ExecutionRuntimeRevision {
            flowrunner_component_digest: RUNNER.into(),
            effect_provider_revision: PROVIDER.into(),
            host_effect_contract_version: "0.1".into(),
        },
        root_artifact_hash,
        ExecutionPlanBody {
            entry_instruction: "entry".parse().unwrap(),
            nodes: [("entry", "event"), ("step", "custom")]
                .into_iter()
                .map(|(node_id, node_type)| ExecutionPlanNode {
                    local_node_id: node_id.parse().unwrap(),
                    source_node_id: node_id.into(),
                    node_type: node_type.into(),
                    config: serde_json::json!({}),
                    effect_policy: ExecutionEffectPolicy::Pure,
                    source_connection_requirement: None,
                })
                .collect(),
            edges: vec![ExecutionPlanEdge {
                source: "entry".parse().unwrap(),
                source_port: "main".into(),
                destination: "step".parse().unwrap(),
                destination_port: None,
                fan_out_ordinal: 0,
            }],
            root_terminal_behavior: RootTerminalBehavior::FrontierExhaustion,
            entry_input_schema_guard: serde_json::Value::Bool(true),
            callable_contract: None,
            source_map: ["entry", "step"]
                .into_iter()
                .map(|node_id| wamn_catalog::ExecutionSourceMapEntry {
                    local_node_id: node_id.parse().unwrap(),
                    source_node_id: node_id.into(),
                })
                .collect(),
        },
    )
    .unwrap()
}

fn draft() -> DraftArtifact {
    let flow = flow(7);
    let artifact = Artifact::new("tenant-a", &flow, implementations()).unwrap();
    DraftArtifact::new(
        "tenant-a",
        &flow,
        implementations(),
        plan(artifact.identity().artifact_hash().as_str()),
    )
    .unwrap()
}

#[test]
fn draft_binds_the_slim_artifact_to_the_only_execution_plan() {
    let draft = draft();
    assert_eq!(
        draft.execution_plan().header.root_artifact_hash,
        draft.artifact().identity().artifact_hash().as_str()
    );
    let mut wrong = draft.execution_plan().clone();
    wrong.header.root_artifact_hash = BINDING_BASE.into();
    assert!(DraftArtifact::new("tenant-a", &flow(7), implementations(), wrong).is_err());
}

#[test]
fn persisted_draft_reverifies_exact_plan_bytes_hash_and_composite_identity() {
    let draft = draft();
    let graph = flow(7);
    let bundle_bytes = serde_json::to_vec(draft.execution_plan()).unwrap();
    let bundle_hash = execution_bundle_hash(&bundle_bytes);
    let identity_input = ValidatedDraftIdentityInput {
        tenant_id: "tenant-a",
        draft_id: "draft-a",
        draft_revision: 3,
        flow_id: "draft-flow",
        runtime_flow_version: 7,
        draft_content_hash: draft.content_hash().as_str(),
        draft_artifact_hash: draft.artifact().identity().artifact_hash().as_str(),
        execution_bundle_hash: &bundle_hash,
        catalog_id: "catalog-a",
        catalog_version: 4,
        environment: "dev",
        binding_base_artifact_hash: BINDING_BASE,
    };
    let identity = ValidatedDraftIdentity::new(identity_input).unwrap();
    let context = StoredValidatedDraftContext {
        expected_identity_hash: identity.as_str(),
        draft_id: "draft-a",
        draft_revision: 3,
        catalog_id: "catalog-a",
        catalog_version: 4,
        environment: "dev",
        binding_base_artifact_hash: BINDING_BASE,
    };
    let pinned = PinnedDraftArtifact::from_storage(
        "tenant-a",
        "draft-flow",
        7,
        draft.content_hash().as_str(),
        &graph.to_json(),
        draft.artifact().graph_hash(),
        draft.artifact().identity().artifact_hash().as_str(),
        &bundle_bytes,
        &bundle_hash,
        context,
    )
    .unwrap();
    assert_eq!(pinned.execution_plan(), draft.execution_plan());

    let mut bytes = bundle_bytes;
    bytes.push(b' ');
    assert!(
        PinnedDraftArtifact::from_storage(
            "tenant-a",
            "draft-flow",
            7,
            draft.content_hash().as_str(),
            &graph.to_json(),
            draft.artifact().graph_hash(),
            draft.artifact().identity().artifact_hash().as_str(),
            &bytes,
            &bundle_hash,
            context,
        )
        .is_err()
    );
}
