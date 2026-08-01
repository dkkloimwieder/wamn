//! PLAN-2A exact-node specialization proof over the frozen fleet fixture.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use wamn_catalog::{
    ExecutionBundleIdentity, ExecutionBundleInput, ExecutionBundlePackaging, ExecutionPlugManifest,
    NodeImplementation,
};
use wamn_node_manifest::{CapabilityClass, RecoveryClass, ResolvedNodeInterface, ResolvedPurity};

const FLEET_JSON: &str = include_str!("../../fixtures/exact-node-fleet.json");
const TOOL_IDENTITY: &str = "wac-cli@0.10.1";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Fleet {
    schema_version: u32,
    nodes: Vec<NodeFixture>,
    flows: Vec<FlowFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct NodeFixture {
    node_type: String,
    component: String,
    world: String,
    capability_worlds: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct FlowFixture {
    flow: String,
    driver: String,
    selected_nodes: Vec<String>,
    invocation_weight: u32,
}

fn fleet() -> Fleet {
    serde_json::from_str(FLEET_JSON).expect("frozen exact-node fleet manifest parses")
}

fn digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    format!("sha256:{}", hex::encode(hash))
}

fn input(identity: &str, digest: String) -> ExecutionBundleInput {
    ExecutionBundleInput::new(identity, digest).expect("fixture content identity is valid")
}

fn build_identity(
    fleet: &Fleet,
    flow: &FlowFixture,
    component_digests: &BTreeMap<String, String>,
    driver_digest: String,
    tool_digest: String,
) -> ExecutionBundleIdentity {
    let mut implementations = Vec::with_capacity(flow.selected_nodes.len());
    let mut plugs = Vec::with_capacity(flow.selected_nodes.len());
    for node_type in &flow.selected_nodes {
        let node = fleet
            .nodes
            .iter()
            .find(|candidate| candidate.node_type == *node_type)
            .expect("selected node exists in frozen manifest");
        let component_digest = component_digests
            .get(node_type)
            .expect("selected node component digest exists")
            .clone();
        let interface = ResolvedNodeInterface::new(
            node_type,
            &node.world,
            vec!["main".to_string()],
            vec![CapabilityClass::Pure],
            Vec::new(),
            ResolvedPurity::Pure,
            RecoveryClass::Replay,
        );
        implementations.push(
            NodeImplementation::supplied(interface, component_digest.clone())
                .expect("fixture implementation resolves"),
        );
        plugs.push(
            ExecutionPlugManifest::new(node_type, vec![node_type.clone()], component_digest)
                .expect("one selected node forms one exact plug"),
        );
    }

    ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::ExactNode,
        input("exact-node-runner@fixture-1", driver_digest),
        input(TOOL_IDENTITY, tool_digest),
    )
    .implementations(implementations)
    .plugs(plugs)
    .build()
    .expect("frozen flow produces a canonical exact-node bundle identity")
}

fn validate_fleet(fleet: &Fleet) {
    assert_eq!(fleet.schema_version, 1);
    assert!(!fleet.nodes.is_empty());
    assert!(!fleet.flows.is_empty());
    assert_eq!(
        fleet
            .flows
            .iter()
            .map(|flow| flow.invocation_weight)
            .sum::<u32>(),
        100,
        "frozen invocation weights must describe the complete observed fleet"
    );

    let mut previous_node = None;
    for node in &fleet.nodes {
        assert!(
            previous_node.is_none_or(|previous: &str| previous < node.node_type.as_str()),
            "node fixtures must be sorted and unique"
        );
        previous_node = Some(node.node_type.as_str());
    }
    let known = fleet
        .nodes
        .iter()
        .map(|node| node.node_type.as_str())
        .collect::<BTreeSet<_>>();
    let mut flow_names = BTreeSet::new();
    for flow in &fleet.flows {
        assert!(
            flow_names.insert(flow.flow.as_str()),
            "flow names must be unique"
        );
        assert!(!flow.selected_nodes.is_empty());
        assert!(
            flow.selected_nodes.windows(2).all(|pair| pair[0] < pair[1]),
            "selected node sets must be sorted and unique"
        );
        assert!(
            flow.selected_nodes
                .iter()
                .all(|node_type| known.contains(node_type.as_str())),
            "every selected node must resolve through the frozen manifest"
        );
    }
}

#[test]
fn exact_node_layout_is_frozen_and_digest_changes_are_local() {
    let fleet = fleet();
    validate_fleet(&fleet);

    let baseline_digests = fleet
        .nodes
        .iter()
        .map(|node| (node.node_type.clone(), digest(node.component.as_bytes())))
        .collect::<BTreeMap<_, _>>();
    let driver_digest = digest(b"fixture-runner");
    let tool_digest = digest(b"wac-cli-0.10.1");
    let baseline = fleet
        .flows
        .iter()
        .map(|flow| {
            (
                flow.flow.as_str(),
                build_identity(
                    &fleet,
                    flow,
                    &baseline_digests,
                    driver_digest.clone(),
                    tool_digest.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        baseline["org-a:project-a/alpha-only"], baseline["org-b:project-b/alpha-reuse"],
        "flow and tenant identity must not defeat observed node-set reuse"
    );

    let mut beta_mutant = baseline_digests.clone();
    beta_mutant.insert("beta".to_string(), digest(b"mutated-beta-component"));
    for flow in &fleet.flows {
        let mutant = build_identity(
            &fleet,
            flow,
            &beta_mutant,
            driver_digest.clone(),
            tool_digest.clone(),
        );
        assert_eq!(
            baseline[flow.flow.as_str()] != mutant,
            flow.selected_nodes
                .iter()
                .any(|node_type| node_type == "beta"),
            "beta digest mutation must invalidate exactly the selecting bundles"
        );
    }
}

fn required_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} must be set")))
}

fn run(command: &mut Command, description: &str) -> std::process::Output {
    let output = command.output().unwrap_or_else(|error| {
        panic!("failed to launch {description}: {error}");
    });
    assert!(
        output.status.success(),
        "{description} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn contains(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

struct CompositionGate<'a> {
    wac: &'a Path,
    component_dir: &'a Path,
    output_dir: &'a Path,
    fleet: &'a Fleet,
}

impl CompositionGate<'_> {
    fn compose(
        &self,
        flow: &FlowFixture,
        driver: &Path,
        output: &Path,
        stage_prefix: &str,
    ) -> Vec<u8> {
        let mut input = driver.to_path_buf();
        let mut log = Vec::new();
        for (stage, node_type) in flow.selected_nodes.iter().enumerate() {
            let node = self
                .fleet
                .nodes
                .iter()
                .find(|candidate| candidate.node_type == *node_type)
                .expect("selected node exists");
            let stage_output = if stage + 1 == flow.selected_nodes.len() {
                output.to_path_buf()
            } else {
                self.output_dir
                    .join(format!("{stage_prefix}-stage-{stage}.wasm"))
            };
            let stage_log = run(
                Command::new(self.wac)
                    .arg("plug")
                    .arg(&input)
                    .arg("--plug")
                    .arg(self.component_dir.join(&node.component))
                    .arg("-o")
                    .arg(&stage_output),
                "compose one deterministic exact-node plug",
            );
            log.extend_from_slice(&stage_log.stdout);
            log.extend_from_slice(&stage_log.stderr);
            input = stage_output;
        }
        log
    }
}

/// Focused artifact gate. The fixture components are built before this ignored test is run.
#[test]
#[ignore = "run explicitly with WAMN_EXACT_* paths after building wasm32-wasip2 fixtures"]
fn exact_node_artifacts_compose_deterministically_and_exclude_unused_worlds() {
    let fleet = fleet();
    validate_fleet(&fleet);
    let component_dir = required_path("WAMN_EXACT_COMPONENT_DIR");
    let output_dir = required_path("WAMN_EXACT_OUTPUT_DIR");
    let wac = required_path("WAMN_WAC_PATH");
    let wasm_tools = required_path("WAMN_WASM_TOOLS_PATH");
    std::fs::create_dir_all(&output_dir).expect("create exact-node gate output directory");

    let wac_version = run(
        Command::new(&wac).arg("--version"),
        "read pinned wac version",
    );
    assert_eq!(
        String::from_utf8_lossy(&wac_version.stdout).trim(),
        "wac-cli 0.10.1"
    );
    let tool_digest = digest(&std::fs::read(&wac).expect("read pinned wac executable"));
    let component_digests = fleet
        .nodes
        .iter()
        .map(|node| {
            let bytes = std::fs::read(component_dir.join(&node.component))
                .expect("read exact-node component fixture");
            (node.node_type.clone(), digest(&bytes))
        })
        .collect::<BTreeMap<_, _>>();
    let composer = CompositionGate {
        wac: &wac,
        component_dir: &component_dir,
        output_dir: &output_dir,
        fleet: &fleet,
    };

    let mut observed_by_identity = BTreeMap::<String, Vec<u8>>::new();
    for (index, flow) in fleet.flows.iter().enumerate() {
        let driver = component_dir.join(&flow.driver);
        let driver_bytes = std::fs::read(&driver).expect("read exact-node driver fixture");
        let identity = build_identity(
            &fleet,
            flow,
            &component_digests,
            digest(&driver_bytes),
            tool_digest.clone(),
        );
        let first = output_dir.join(format!("bundle-{index}-a.wasm"));
        let second = output_dir.join(format!("bundle-{index}-b.wasm"));

        let first_log = composer.compose(flow, &driver, &first, &format!("bundle-{index}-a"));
        composer.compose(flow, &driver, &second, &format!("bundle-{index}-b"));

        let first_bytes = std::fs::read(&first).expect("read first composed bundle");
        let second_bytes = std::fs::read(&second).expect("read rebuilt composed bundle");
        assert_eq!(
            digest(&first_bytes),
            digest(&second_bytes),
            "same inputs must reproduce exact bytes"
        );
        assert_eq!(first_bytes, second_bytes);
        let provenance = identity.provenance(&first_bytes, &first_log);
        assert_eq!(provenance.verify_rebuild(&identity, &second_bytes), Ok(()));

        let outer_world = run(
            Command::new(&wasm_tools)
                .arg("component")
                .arg("wit")
                .arg(&first),
            "inspect composed component world",
        );
        let outer_world = String::from_utf8(outer_world.stdout).expect("WIT output is UTF-8");
        assert!(outer_world.contains("export run-flow"));
        assert!(
            !outer_world.contains("import "),
            "pure bundle retains an outer import"
        );

        for node in &fleet.nodes {
            let selected = flow.selected_nodes.contains(&node.node_type);
            assert_eq!(
                contains(&first_bytes, &node.world),
                selected,
                "artifact implementation set differs for {}",
                node.node_type
            );
            for capability_world in &node.capability_worlds {
                assert!(
                    !contains(&first_bytes, capability_world),
                    "unselected capability world {capability_world} entered the bundle"
                );
            }
        }

        if let Some(previous) =
            observed_by_identity.insert(identity.hash().to_string(), first_bytes)
        {
            assert_eq!(
                previous, second_bytes,
                "shared identity produced different bytes"
            );
        }
    }
}
