//! PLAN-2A capability-class specialization proof over the frozen exact-node fleet inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use wamn_catalog::{
    ExecutionBundleIdentity, ExecutionBundleInput, ExecutionBundlePackaging, ExecutionPlugManifest,
    NodeImplementation,
};
use wamn_node_manifest::{CapabilityClass, RecoveryClass, ResolvedNodeInterface, ResolvedPurity};

const FLEET_JSON: &str = include_str!("../../fixtures/capability-class-fleet.json");
const EXACT_FLEET_JSON: &str = include_str!("../../fixtures/exact-node-fleet.json");
const TOOL_IDENTITY: &str = "wac-cli@0.10.1";
const COMPOSITION_INVARIANT: &str = "sorted-single-plug-v1";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Fleet {
    schema_version: u32,
    classes: Vec<ClassFixture>,
    nodes: Vec<NodeFixture>,
    flows: Vec<FlowFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ClassFixture {
    class: String,
    component: String,
    members: Vec<String>,
    member_worlds: Vec<String>,
    capability_worlds: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct NodeFixture {
    node_type: String,
    class: String,
    world: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct FlowFixture {
    flow: String,
    driver: String,
    selected_nodes: Vec<String>,
    selected_classes: Vec<String>,
    invocation_weight: u32,
}

#[derive(Debug, Deserialize)]
struct ExactFleet {
    flows: Vec<ExactFlowFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ExactFlowFixture {
    flow: String,
    driver: String,
    selected_nodes: Vec<String>,
    invocation_weight: u32,
}

fn fleet() -> Fleet {
    serde_json::from_str(FLEET_JSON).expect("frozen capability-class fleet manifest parses")
}

fn digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    format!("sha256:{}", hex::encode(hash))
}

fn input(identity: &str, digest: String) -> ExecutionBundleInput {
    ExecutionBundleInput::new(identity, digest).expect("fixture content identity is valid")
}

fn capability(class: &str) -> CapabilityClass {
    match class {
        "http" => CapabilityClass::Http,
        "postgres" => CapabilityClass::Postgres,
        "pure" => CapabilityClass::Pure,
        other => panic!("unknown capability class {other:?}"),
    }
}

fn build_identity(
    fleet: &Fleet,
    flow: &FlowFixture,
    class_digests: &BTreeMap<String, String>,
    driver_digest: String,
    tool_digest: String,
) -> ExecutionBundleIdentity {
    let implementations = flow
        .selected_nodes
        .iter()
        .map(|node_type| {
            let node = fleet
                .nodes
                .iter()
                .find(|candidate| candidate.node_type == *node_type)
                .expect("selected node exists");
            let class_digest = class_digests
                .get(&node.class)
                .expect("selected class digest exists")
                .clone();
            let class = capability(&node.class);
            let (purity, recovery) = if class == CapabilityClass::Pure {
                (ResolvedPurity::Pure, RecoveryClass::Replay)
            } else {
                (ResolvedPurity::Effectful, RecoveryClass::NeverReplay)
            };
            let interface = ResolvedNodeInterface::new(
                node_type,
                &node.world,
                vec!["main".to_string()],
                vec![class],
                Vec::new(),
                purity,
                recovery,
            );
            NodeImplementation::supplied(interface, class_digest)
                .expect("fixture class implementation resolves")
        })
        .collect::<Vec<_>>();
    let plugs = flow
        .selected_classes
        .iter()
        .map(|class_name| {
            let class = fleet
                .classes
                .iter()
                .find(|candidate| candidate.class == *class_name)
                .expect("selected class exists");
            ExecutionPlugManifest::new(
                class_name,
                class.members.clone(),
                class_digests
                    .get(class_name)
                    .expect("selected class digest exists")
                    .clone(),
            )
            .expect("full first-party class forms one executable plug")
        })
        .collect::<Vec<_>>();

    ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::CapabilityClass,
        input("exact-node-runner@fixture-1", driver_digest),
        input(TOOL_IDENTITY, tool_digest),
    )
    .implementations(implementations)
    .plugs(plugs)
    .build()
    .expect("frozen flow produces a canonical capability-class bundle identity")
}

fn validate_fleet(fleet: &Fleet) {
    let exact_fleet: ExactFleet =
        serde_json::from_str(EXACT_FLEET_JSON).expect("frozen exact-node fleet manifest parses");
    assert_eq!(fleet.schema_version, 1);
    assert_eq!(
        fleet
            .classes
            .iter()
            .map(|class| class.class.as_str())
            .collect::<Vec<_>>(),
        ["http", "postgres", "pure"]
    );
    assert_eq!(
        fleet
            .flows
            .iter()
            .map(|flow| flow.invocation_weight)
            .sum::<u32>(),
        100
    );
    assert_eq!(fleet.flows.len(), exact_fleet.flows.len());
    for (class_flow, exact_flow) in fleet.flows.iter().zip(&exact_fleet.flows) {
        assert_eq!(class_flow.flow, exact_flow.flow);
        assert_eq!(class_flow.driver, exact_flow.driver);
        assert_eq!(class_flow.selected_nodes, exact_flow.selected_nodes);
        assert_eq!(class_flow.invocation_weight, exact_flow.invocation_weight);
    }

    let mut known_members = BTreeMap::new();
    for class in &fleet.classes {
        assert!(class.members.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(!class.members.is_empty());
        assert_eq!(class.members.len(), class.member_worlds.len());
        for member in &class.members {
            assert!(
                known_members
                    .insert(member.as_str(), class.class.as_str())
                    .is_none(),
                "class members must be unique"
            );
        }
    }
    for node in &fleet.nodes {
        assert_eq!(
            known_members.get(node.node_type.as_str()),
            Some(&node.class.as_str())
        );
    }

    let known_classes = fleet
        .classes
        .iter()
        .map(|class| class.class.as_str())
        .collect::<BTreeSet<_>>();
    for flow in &fleet.flows {
        assert!(flow.selected_nodes.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            flow.selected_classes
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        );
        assert!(
            flow.selected_classes
                .iter()
                .all(|class| known_classes.contains(class.as_str()))
        );
        let derived = flow
            .selected_nodes
            .iter()
            .map(|node_type| {
                fleet
                    .nodes
                    .iter()
                    .find(|node| node.node_type == *node_type)
                    .expect("selected node exists")
                    .class
                    .as_str()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            derived,
            flow.selected_classes.iter().map(String::as_str).collect()
        );
        for class_name in &flow.selected_classes {
            let class = fleet
                .classes
                .iter()
                .find(|candidate| candidate.class == *class_name)
                .expect("selected class exists");
            assert!(
                class
                    .members
                    .iter()
                    .any(|member| !flow.selected_nodes.contains(member)),
                "selected classes must demonstrate an unused carried member"
            );
        }
    }
}

#[test]
fn capability_class_layout_has_class_wide_member_blast_radius() {
    let fleet = fleet();
    validate_fleet(&fleet);
    let baseline_digests = fleet
        .classes
        .iter()
        .map(|class| (class.class.clone(), digest(class.component.as_bytes())))
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
        baseline["org-a:project-a/alpha-only"],
        baseline["org-b:project-b/alpha-reuse"]
    );
    for class in &fleet.classes {
        let mut mutant_digests = baseline_digests.clone();
        mutant_digests.insert(
            class.class.clone(),
            digest(format!("mutated-{}-member", class.class).as_bytes()),
        );
        for flow in &fleet.flows {
            let mutant = build_identity(
                &fleet,
                flow,
                &mutant_digests,
                driver_digest.clone(),
                tool_digest.clone(),
            );
            assert_eq!(
                baseline[flow.flow.as_str()] != mutant,
                flow.selected_classes.contains(&class.class),
                "a member rebuild must invalidate every and only bundle carrying {}",
                class.class
            );
        }
    }
    assert!(
        fleet
            .flows
            .iter()
            .all(|flow| !flow.selected_classes.contains(&"postgres".to_string()))
    );
}

#[derive(Debug)]
struct CompositionPlan<'a> {
    stages: Vec<Vec<&'a str>>,
}

impl CompositionPlan<'_> {
    fn provenance_log(&self) -> Vec<u8> {
        let mut log =
            format!("tool={TOOL_IDENTITY}\ncomposition-invariant={COMPOSITION_INVARIANT}\n");
        for (index, classes) in self.stages.iter().enumerate() {
            writeln!(log, "stage={index};plugs={}", classes.join(","))
                .expect("writing composition provenance to a string cannot fail");
        }
        log.into_bytes()
    }
}

fn composition_plan(flow: &FlowFixture) -> CompositionPlan<'_> {
    CompositionPlan {
        stages: flow
            .selected_classes
            .iter()
            .map(|class| vec![class.as_str()])
            .collect(),
    }
}

#[test]
fn capability_class_composition_plan_pins_reproducible_single_plug_stages() {
    let fleet = fleet();
    validate_fleet(&fleet);
    for flow in &fleet.flows {
        let plan = composition_plan(flow);
        assert_eq!(plan.stages.len(), flow.selected_classes.len());
        assert!(plan.stages.iter().all(|stage| stage.len() == 1));
        assert_eq!(
            plan.stages.iter().flatten().copied().collect::<Vec<_>>(),
            flow.selected_classes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        let provenance = String::from_utf8(plan.provenance_log()).expect("provenance is UTF-8");
        assert!(provenance.contains("tool=wac-cli@0.10.1\n"));
        assert!(provenance.contains("composition-invariant=sorted-single-plug-v1\n"));
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
        let plan = composition_plan(flow);
        let mut input = driver.to_path_buf();
        for (stage, class_names) in plan.stages.iter().enumerate() {
            let stage_output = if stage + 1 == plan.stages.len() {
                output.to_path_buf()
            } else {
                self.output_dir
                    .join(format!("{stage_prefix}-stage-{stage}.wasm"))
            };
            let mut command = Command::new(self.wac);
            command.arg("plug").arg(&input);
            for class_name in class_names {
                let class = self
                    .fleet
                    .classes
                    .iter()
                    .find(|candidate| candidate.class == *class_name)
                    .expect("selected class exists");
                command
                    .arg("--plug")
                    .arg(self.component_dir.join(&class.component));
            }
            command.arg("-o").arg(&stage_output);
            run(&mut command, "compose capability-class plug stage");
            input = stage_output;
        }
        plan.provenance_log()
    }
}

/// Focused artifact gate. Build the class fixtures and shared exact drivers before running.
#[test]
#[ignore = "run explicitly with WAMN_CAPABILITY_CLASS_* paths after building wasm32-wasip2 fixtures"]
fn capability_class_artifacts_are_deterministic_and_match_selected_class_worlds() {
    let fleet = fleet();
    validate_fleet(&fleet);
    let component_dir = required_path("WAMN_CAPABILITY_CLASS_COMPONENT_DIR");
    let output_dir = required_path("WAMN_CAPABILITY_CLASS_OUTPUT_DIR");
    let wac = required_path("WAMN_WAC_PATH");
    let wasm_tools = required_path("WAMN_WASM_TOOLS_PATH");
    std::fs::create_dir_all(&output_dir).expect("create capability-class gate output directory");

    let wac_version = run(
        Command::new(&wac).arg("--version"),
        "read pinned wac version",
    );
    assert_eq!(
        String::from_utf8_lossy(&wac_version.stdout).trim(),
        "wac-cli 0.10.1"
    );
    let tool_digest = digest(&std::fs::read(&wac).expect("read pinned wac executable"));
    let class_digests = fleet
        .classes
        .iter()
        .map(|class| {
            let path = component_dir.join(&class.component);
            let bytes = std::fs::read(&path).expect("read capability-class component fixture");
            let world = run(
                Command::new(&wasm_tools)
                    .arg("component")
                    .arg("wit")
                    .arg(&path),
                "inspect full capability-class component world",
            );
            let world = String::from_utf8(world.stdout).expect("WIT output is UTF-8");
            for member_world in &class.member_worlds {
                assert!(
                    world.contains(member_world),
                    "class omits member world {member_world}"
                );
            }
            for capability_world in &class.capability_worlds {
                assert!(
                    world.contains(capability_world),
                    "class omits capability world {capability_world}"
                );
            }
            (class.class.clone(), digest(&bytes))
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
        let driver_bytes = std::fs::read(&driver).expect("read shared exact-node driver fixture");
        let identity = build_identity(
            &fleet,
            flow,
            &class_digests,
            digest(&driver_bytes),
            tool_digest.clone(),
        );
        let first = output_dir.join(format!("bundle-{index}-a.wasm"));
        let second = output_dir.join(format!("bundle-{index}-b.wasm"));
        let first_log = composer.compose(flow, &driver, &first, &format!("bundle-{index}-a"));
        composer.compose(flow, &driver, &second, &format!("bundle-{index}-b"));

        let first_bytes = std::fs::read(&first).expect("read first class-composed bundle");
        let second_bytes = std::fs::read(&second).expect("read rebuilt class-composed bundle");
        assert_eq!(digest(&first_bytes), digest(&second_bytes));
        assert_eq!(
            first_bytes, second_bytes,
            "same inputs must reproduce exact bytes"
        );
        let provenance = identity.provenance(&first_bytes, &first_log);
        assert_eq!(provenance.verify_rebuild(&identity, &second_bytes), Ok(()));

        let outer_world = run(
            Command::new(&wasm_tools)
                .arg("component")
                .arg("wit")
                .arg(&first),
            "inspect class-composed component world",
        );
        let outer_world = String::from_utf8(outer_world.stdout).expect("WIT output is UTF-8");
        assert!(outer_world.contains("export run-flow"));
        for class in &fleet.classes {
            let selected = flow.selected_classes.contains(&class.class);
            for member_world in &class.member_worlds {
                assert_eq!(
                    contains(&first_bytes, member_world),
                    selected,
                    "class member world {member_world} disagrees with class selection"
                );
            }
            for capability_world in &class.capability_worlds {
                assert_eq!(
                    outer_world.contains(capability_world),
                    selected,
                    "outer capability world {capability_world} disagrees with class selection"
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
