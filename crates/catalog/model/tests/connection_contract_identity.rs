use sha2::{Digest as _, Sha256};
use wamn_catalog::{
    Artifact, CatalogIdentityError, ExecutionBundleIdentity, ExecutionBundleInput,
    ExecutionBundlePackaging, ExecutionPlugManifest, NodeImplementation,
};
use wamn_flow::Flow;
use wamn_node_manifest::{
    CapabilityClass, ConnectionRequirement, ExecutableRecoveryContract, ResolvedNodeInterface,
};

const HTTP_WIT: &[u8] = include_bytes!("../../../../docs/contracts/wamn-connection.wit");

fn digest(bytes: &[u8]) -> String {
    let value = Sha256::digest(bytes);
    let encoded = value
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{encoded}")
}

fn fixed_digest(byte: u8) -> String {
    digest(&[byte])
}

fn artifact_new(
    tenant: &str,
    flow: &Flow,
    mut implementations: Vec<NodeImplementation>,
) -> Result<Artifact, CatalogIdentityError> {
    let request = NodeImplementation::platform(
        ResolvedNodeInterface::new(
            "request",
            "wamn:node/node@0.1.0",
            vec!["main".to_string()],
            vec![CapabilityClass::Pure],
            Vec::new(),
        ),
        ExecutableRecoveryContract::pure(),
    );
    let respond = NodeImplementation::platform(
        ResolvedNodeInterface::new(
            "respond",
            "wamn:node/node@0.1.0",
            vec!["main".to_string()],
            vec![CapabilityClass::Pure],
            Vec::new(),
        ),
        ExecutableRecoveryContract::pure(),
    );
    for implementation in [request, respond] {
        let index = implementations
            .iter()
            .position(|candidate| {
                candidate.interface().node_type > implementation.interface().node_type
            })
            .unwrap_or(implementations.len());
        implementations.insert(index, implementation);
    }
    Artifact::new(tenant, flow, implementations)
}

fn implementation(contract: &str, executable_digest: String) -> NodeImplementation {
    let interface = ResolvedNodeInterface::new(
        "http-node",
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Http],
        vec![ConnectionRequirement {
            requirement_type: "http".to_string(),
            contract: contract.to_string(),
        }],
    );
    NodeImplementation::supplied(
        interface,
        executable_digest,
        ExecutableRecoveryContract::effectful(false),
    )
    .expect("typed HTTP fixture resolves")
}

fn flow() -> Flow {
    Flow::from_json(
        r#"{
          "schema-version":"0.1",
          "flow-id":"connection-contract-identity",
          "version":1,
          "nodes":[
            {"id":"request","type":"request","config":{"input-schema":true}},
            {"id":"call","type":"http-node"},
            {"id":"response","type":"respond","config":{"status":200}}
          ],
          "edges":[
            {"from":"request","to":"call"},
            {"from":"call","to":"response"}
          ]
        }"#,
    )
    .expect("identity fixture flow parses")
}

fn bundle(implementation: NodeImplementation) -> ExecutionBundleIdentity {
    ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::ExactNode,
        ExecutionBundleInput::new("runner@fixture", fixed_digest(2)).unwrap(),
        ExecutionBundleInput::new("wac@fixture", fixed_digest(3)).unwrap(),
    )
    .implementations(vec![implementation])
    .plugs(vec![
        ExecutionPlugManifest::new("http-node", vec!["http-node".to_string()], fixed_digest(4))
            .unwrap(),
    ])
    .build()
    .expect("identity fixture bundle builds")
}

#[test]
fn material_http_wit_change_is_rejected_or_versioned_through_every_identity() {
    let baseline_digest = digest(HTTP_WIT);
    let material_mutant = String::from_utf8(HTTP_WIT.to_vec())
        .expect("authoritative WIT is UTF-8")
        .replace("timeout,", "timeout(string),");
    assert_ne!(baseline_digest, digest(material_mutant.as_bytes()));

    let baseline = implementation("wamn:connection/http@0.1.0", baseline_digest);
    let versioned_mutant = implementation(
        "wamn:connection/http@0.2.0",
        digest(material_mutant.as_bytes()),
    );

    assert_ne!(
        baseline.contract().identity_hash(),
        versioned_mutant.contract().identity_hash(),
        "material connection ABI changes must invalidate resolved-node identity"
    );
    let baseline_artifact = artifact_new("tenant-a", &flow(), vec![baseline.clone()]).unwrap();
    let mutant_artifact =
        artifact_new("tenant-a", &flow(), vec![versioned_mutant.clone()]).unwrap();
    assert_ne!(
        baseline_artifact.identity().artifact_hash(),
        mutant_artifact.identity().artifact_hash(),
        "material connection ABI changes must invalidate artifact identity"
    );
    assert_ne!(
        bundle(baseline).hash(),
        bundle(versioned_mutant).hash(),
        "material connection ABI changes must invalidate execution-bundle identity"
    );
}
