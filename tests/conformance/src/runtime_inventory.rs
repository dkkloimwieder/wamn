//! Recorded wash-runtime feature and generated-workload inventory.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const INVENTORY: &str = include_str!("../runtime-inventory.json");
const ALLOWED_WASH_RUNTIME_FEATURES: [&str; 4] = ["oci", "wasi-config", "wasi-otel", "washlet"];
const CFG_TEST_MODULE: &str = "#[cfg(test)]\nmod tests {";
const EXECUTION_HOST_STORE_CONSTRUCTOR: &str =
    "let mut store = Store::new(raw, SharedCtx::new(ctx));";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum WorkloadAbi {
    P2Components,
    P2CliService,
    P3Service,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimePin {
    cargo_tree_root: String,
    default_features: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Consumer {
    package: String,
    manifest: String,
    features: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbiEvidence {
    path: String,
    required_marker: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkloadManifest {
    path: String,
    deployment_state: String,
    abi: WorkloadAbi,
    abi_evidence: Option<AbiEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    schema_version: u32,
    wash_runtime: RuntimePin,
    live_store_paths: BTreeSet<String>,
    consumers: Vec<Consumer>,
    workload_manifests: Vec<WorkloadManifest>,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    manifest_path: String,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn inventory() -> Inventory {
    serde_json::from_str(INVENTORY).expect("runtime-inventory.json must be valid")
}

fn cargo_tree(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["tree", "--locked", "--offline"])
        .args(arguments)
        .output()
        .expect("run cargo tree for runtime inventory");
    assert!(
        output.status.success(),
        "cargo tree {} failed:\n{}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("cargo tree output must be UTF-8")
}

fn wash_runtime_source(root: &Path) -> PathBuf {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .output()
        .expect("run cargo metadata for wash-runtime source");
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: CargoMetadata =
        serde_json::from_slice(&output.stdout).expect("cargo metadata must be valid JSON");
    let manifest = metadata
        .packages
        .iter()
        .find(|package| package.name == "wash-runtime")
        .expect("resolved graph must contain wash-runtime");
    Path::new(&manifest.manifest_path)
        .parent()
        .expect("wash-runtime manifest must have a parent")
        .to_path_buf()
}

fn workspace_wash_runtime_declaration(root: &Path) -> String {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml");
    let mut workspace_dependencies = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            workspace_dependencies = trimmed == "[workspace.dependencies]";
            continue;
        }
        if workspace_dependencies && let Some(declaration) = trimmed.strip_prefix("wash-runtime =")
        {
            return declaration.split_whitespace().collect();
        }
    }
    panic!("root [workspace.dependencies] must declare wash-runtime");
}

fn validate_one(source: &str, marker: &str, seam: &str) -> Result<(), String> {
    let observed = source.matches(marker).count();
    if observed == 1 {
        Ok(())
    } else {
        Err(format!(
            "{seam} must retain exactly one `{marker}` marker; found {observed}"
        ))
    }
}

fn assert_one(source: &str, marker: &str, seam: &str) {
    validate_one(source, marker, seam).unwrap_or_else(|error| panic!("{error}"));
}

fn production_execution_host_source(source: &str) -> Result<&str, String> {
    let test_modules = source.matches(CFG_TEST_MODULE).count();
    if test_modules != 1 {
        return Err(format!(
            "ExecutionHost source must retain exactly one terminal `{CFG_TEST_MODULE}` module; \
             found {test_modules}"
        ));
    }
    let (production, _) = source
        .split_once(CFG_TEST_MODULE)
        .expect("the counted cfg(test) module must split");
    Ok(production)
}

fn validate_execution_host_store_constructor(source: &str) -> Result<(), String> {
    let production = production_execution_host_source(source)?;
    validate_one(
        production,
        EXECUTION_HOST_STORE_CONSTRUCTOR,
        "production ExecutionHost store constructor",
    )
}

fn observed_store_paths(root: &Path, wash_runtime: &Path) -> BTreeSet<String> {
    let wash_manifest_path = wash_runtime.join("Cargo.toml");
    let wash_manifest = fs::read_to_string(&wash_manifest_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", wash_manifest_path.display()));
    assert_one(
        &wash_manifest,
        "host-component-plugins = []",
        "host-component plugin feature",
    );
    let plugin_module_path = wash_runtime.join("src/plugin/mod.rs");
    let plugin_module = fs::read_to_string(&plugin_module_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", plugin_module_path.display()));
    assert_one(
        &plugin_module,
        "#[cfg(all(feature = \"host-component-plugins\", feature = \"oci\"))]",
        "host-component plugin module gate",
    );

    let linked_call_path = wash_runtime.join("src/engine/linked_call.rs");
    let linked_call = fs::read_to_string(&linked_call_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", linked_call_path.display()));
    assert_one(
        &linked_call,
        "pub(crate) async fn new_store_from_templates(",
        "fork store constructor",
    );
    assert_one(
        &linked_call,
        "let mut store = wasmtime::Store::new(engine, shared_ctx);",
        "fork store constructor",
    );

    let component_plugin_path = wash_runtime.join("src/plugin/component_host/mod.rs");
    let component_plugin = fs::read_to_string(&component_plugin_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", component_plugin_path.display()));
    assert_one(
        &component_plugin,
        "Store::new(engine.inner(), SharedCtx::new(ctx).with_resource_registry())",
        "feature-gated host-component plugin store",
    );

    let pool_path = wash_runtime.join("src/engine/instance_pool.rs");
    let pool = fs::read_to_string(&pool_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", pool_path.display()));
    assert!(
        pool.contains("Some(pool_size) => Self::Warm"),
        "nonzero pool_size must remain the warm-store activation seam"
    );

    let execution_path = root.join("crates/execution/host/src/lib.rs");
    let execution = fs::read_to_string(&execution_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", execution_path.display()));
    validate_execution_host_store_constructor(&execution).unwrap_or_else(|error| panic!("{error}"));

    let node_path = root.join("crates/platform/node-runtime/src/lib.rs");
    let node = fs::read_to_string(&node_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", node_path.display()));
    assert_one(
        &node,
        "let mut store = Store::new(raw, SharedCtx::new(context));",
        "NodeRuntime store constructor",
    );

    BTreeSet::from([
        "fork: new_store_from_templates (single production site)".to_string(),
        "wamn: ExecutionHost store (crates/execution/host)".to_string(),
        "wamn: NodeRuntime store (crates/platform/node-runtime)".to_string(),
    ])
}

fn resolved_features(tree: &str) -> BTreeSet<String> {
    tree.lines()
        .filter_map(|line| {
            line.split_once("wash-runtime feature \"")
                .and_then(|(_, suffix)| suffix.split_once('"'))
                .map(|(feature, _)| feature.to_string())
        })
        .collect()
}

fn service_consumers(root: &Path) -> BTreeSet<String> {
    let tree = cargo_tree(
        root,
        &["--workspace", "-e", "features", "-i", "wash-runtime"],
    );
    let service_prefix = format!("{}/services/", root.display());

    tree.lines()
        .filter_map(|line| {
            let (_, path) = line.split_once(&service_prefix)?;
            let service = path.split(')').next()?.split('/').next()?;
            Some(format!("services/{service}/Cargo.toml"))
        })
        .collect()
}

fn workload_manifests(root: &Path) -> BTreeSet<String> {
    let platform = root.join("deploy/platform");
    fs::read_dir(&platform)
        .unwrap_or_else(|error| panic!("read {}: {error}", platform.display()))
        .filter_map(|entry| {
            let path = entry.expect("read deploy/platform entry").path();
            let extension = path.extension()?.to_str()?;
            if !matches!(extension, "yaml" | "yml") {
                return None;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            if !source
                .lines()
                .any(|line| line.trim() == "kind: WorkloadDeployment")
            {
                return None;
            }
            Some(
                path.strip_prefix(root)
                    .expect("platform manifest must be repository-relative")
                    .to_string_lossy()
                    .to_string(),
            )
        })
        .collect()
}

fn yaml_i32(source: &str, key: &str) -> Vec<i32> {
    source
        .lines()
        .filter_map(|line| {
            let value = line.trim().strip_prefix(key)?.trim();
            Some(
                value
                    .parse()
                    .unwrap_or_else(|_| panic!("{key} must be an integer, got `{value}`")),
            )
        })
        .collect()
}

fn validate_feature_policy(features: &BTreeSet<String>) -> Result<(), String> {
    if features.contains("host-component-plugins") {
        return Err(
            "host-component-plugins enables plugin-owned stores beyond the three recorded paths"
                .to_string(),
        );
    }

    let allowed = ALLOWED_WASH_RUNTIME_FEATURES
        .into_iter()
        .map(str::to_string)
        .collect();
    let unknown: Vec<_> = features.difference(&allowed).cloned().collect();
    if !unknown.is_empty() {
        return Err(format!(
            "unreviewed wash-runtime features may widen live store paths: {}",
            unknown.join(", ")
        ));
    }
    Ok(())
}

fn validate_ip_name_lookup_defaults(path: &str, source: &str) -> Result<(), String> {
    let lines: Vec<_> = source.lines().collect();
    let mut local_resources_blocks = 0;

    for (index, line) in lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim() == "localResources:")
    {
        local_resources_blocks += 1;
        let block_indent = line.len() - line.trim_start().len();
        let block_end = lines[index + 1..]
            .iter()
            .position(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty()
                    && !trimmed.starts_with('#')
                    && line.len() - line.trim_start().len() <= block_indent
            })
            .map_or(lines.len(), |offset| index + 1 + offset);
        let block = &lines[index + 1..block_end];
        let child_indent = block
            .iter()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
            .map(|line| line.len() - line.trim_start().len())
            .min();
        let values: Vec<_> = block
            .iter()
            .filter(|line| {
                child_indent.is_some_and(|indent| line.len() - line.trim_start().len() == indent)
            })
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("allowedIpNameLookups:")
                    .map(str::trim)
            })
            .collect();

        if values.len() != 1 {
            return Err(format!(
                "{path}: each localResources block must contain exactly one allowedIpNameLookups field; found {}",
                values.len()
            ));
        }
        if values[0] != "[]" {
            return Err(format!(
                "{path}: allowedIpNameLookups must default to [], got `{}`",
                values[0]
            ));
        }
    }

    if local_resources_blocks == 0 {
        return Err(format!(
            "{path}: workload must expose localResources.allowedIpNameLookups with default []"
        ));
    }
    Ok(())
}

fn validate_workload_policy(path: &str, source: &str, abi: &WorkloadAbi) -> Result<(), String> {
    let pool_sizes = yaml_i32(source, "poolSize:");
    if let Some(pool_size) = pool_sizes.into_iter().find(|pool_size| *pool_size != 0) {
        return Err(format!(
            "{path}: poolSize {pool_size} enables reusable component stores"
        ));
    }

    validate_ip_name_lookup_defaults(path, source)?;

    let has_components = source.lines().any(|line| line == "      components:");
    // maxInvocations has no effect when poolSize is absent or zero.
    let _max_invocations = yaml_i32(source, "maxInvocations:");

    let has_workload_service = source.lines().any(|line| line == "      service:");
    match abi {
        WorkloadAbi::P2Components if has_workload_service || !has_components => {
            return Err(format!(
                "{path}: recorded as P2 components but its components/service shape disagrees"
            ));
        }
        WorkloadAbi::P2CliService if !has_workload_service => {
            return Err(format!(
                "{path}: recorded as a P2 CLI service but has no workload service"
            ));
        }
        WorkloadAbi::P3Service => {
            return Err(format!(
                "{path}: P3 service workload deployment is excluded"
            ));
        }
        WorkloadAbi::P2Components | WorkloadAbi::P2CliService => {}
    }
    Ok(())
}

#[test]
fn resolved_feature_and_deployed_workload_inventory_is_current() {
    let root = repository_root();
    let inventory = inventory();
    assert_eq!(inventory.schema_version, 1);
    let declaration = workspace_wash_runtime_declaration(&root);
    assert!(
        declaration.contains("default-features=false"),
        "root wash-runtime dependency must explicitly set default-features = false"
    );
    assert!(
        !inventory.wash_runtime.default_features,
        "recorded wash-runtime default-feature policy drifted"
    );
    assert_eq!(
        inventory.live_store_paths,
        observed_store_paths(&root, &wash_runtime_source(&root)),
        "the base upgrade has exactly three live store paths"
    );
    assert_eq!(
        inventory.consumers.len(),
        5,
        "the inventory must retain all five production consumers"
    );

    let recorded_manifests = inventory
        .consumers
        .iter()
        .map(|consumer| consumer.manifest.clone())
        .collect();
    assert_eq!(
        service_consumers(&root),
        recorded_manifests,
        "production services consuming wash-runtime drifted"
    );

    let mut all_features = BTreeSet::new();
    for consumer in &inventory.consumers {
        let tree = cargo_tree(
            &root,
            &[
                "-p",
                &consumer.package,
                "-e",
                "features",
                "-i",
                "wash-runtime",
            ],
        );
        assert_eq!(
            tree.lines().next(),
            Some(inventory.wash_runtime.cargo_tree_root.as_str()),
            "{} resolved a different wash-runtime identity",
            consumer.package
        );
        let actual = resolved_features(&tree);
        assert_eq!(
            actual, consumer.features,
            "{} wash-runtime feature inventory drifted",
            consumer.package
        );
        all_features.extend(actual);
    }
    validate_feature_policy(&all_features).unwrap_or_else(|error| panic!("{error}"));

    let recorded_workloads = inventory
        .workload_manifests
        .iter()
        .map(|workload| workload.path.clone())
        .collect();
    assert_eq!(
        workload_manifests(&root),
        recorded_workloads,
        "generated WorkloadDeployment manifest inventory drifted"
    );
    assert_eq!(
        inventory.workload_manifests.len(),
        5,
        "the inventory must retain all five generated workload manifests"
    );
    for workload in &inventory.workload_manifests {
        let expected_state = if workload.path.contains(".example.") {
            "template"
        } else {
            "active"
        };
        assert_eq!(
            workload.deployment_state, expected_state,
            "{} deployment-state classification drifted",
            workload.path
        );
        let path = root.join(&workload.path);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        validate_workload_policy(&workload.path, &source, &workload.abi)
            .unwrap_or_else(|error| panic!("{error}"));
        match (&workload.abi, &workload.abi_evidence) {
            (WorkloadAbi::P2CliService, Some(evidence)) => {
                let evidence_path = root.join(&evidence.path);
                let evidence_source = fs::read_to_string(&evidence_path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", evidence_path.display()));
                assert!(
                    evidence_source.contains(&evidence.required_marker),
                    "{} no longer proves the P2 CLI service classification for {}",
                    evidence.path,
                    workload.path
                );
                let component = evidence_path
                    .parent()
                    .expect("component Cargo.toml must have a parent");
                assert!(
                    component.join("src/main.rs").is_file(),
                    "{} must remain a command component",
                    evidence.path
                );
                let world_path = component.join("wit/world.wit");
                let world = fs::read_to_string(&world_path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", world_path.display()));
                assert!(
                    !world.contains("wasi:http/handler") && !world.contains("wasmcloud:messaging"),
                    "{} adopted a P3 service export",
                    world_path.display()
                );
            }
            (WorkloadAbi::P2Components, None) => {}
            _ => panic!(
                "{} ABI classification has missing or unexpected evidence",
                workload.path
            ),
        }
    }
}

#[test]
fn host_component_plugins_mutation_is_rejected() {
    let mut features = BTreeSet::from(["oci".to_string(), "washlet".to_string()]);
    features.insert("host-component-plugins".to_string());
    let error = validate_feature_policy(&features).expect_err("mutation must fail closed");
    assert!(error.contains("plugin-owned stores beyond the three recorded paths"));
}

#[test]
fn nonzero_pool_size_mutation_is_rejected() {
    let mutant = "components:\n  - name: mutant\n    poolSize: 1\n    maxInvocations: 10\n";
    let error =
        validate_workload_policy("pool-size-mutant.yaml", mutant, &WorkloadAbi::P2Components)
            .expect_err("mutation must fail closed");
    assert!(error.contains("poolSize 1 enables reusable component stores"));
}

#[test]
fn nonempty_ip_name_lookup_default_mutation_is_rejected() {
    let mutant = "      components:\n        - name: mutant\n          localResources:\n            allowedIpNameLookups: [\"example.com\"]\n";
    let error = validate_workload_policy("lookup-mutant.yaml", mutant, &WorkloadAbi::P2Components)
        .expect_err("nonempty allowedIpNameLookups default must fail closed");
    assert!(error.contains("allowedIpNameLookups must default to []"));
}

#[test]
fn missing_misspelled_or_duplicate_ip_name_lookup_defaults_are_rejected() {
    let mutants = [
        (
            "missing",
            "      components:\n        - name: mutant\n          localResources:\n            config: {}\n",
        ),
        (
            "wrong-allow-prefix",
            "      components:\n        - name: mutant\n          localResources:\n            allowIpNameLookups: []\n",
        ),
        (
            "legacy-singular",
            "      components:\n        - name: mutant\n          localResources:\n            allowIpNameLookup: []\n",
        ),
        (
            "duplicate",
            "      components:\n        - name: mutant\n          localResources:\n            allowedIpNameLookups: []\n            allowedIpNameLookups: []\n",
        ),
    ];

    for (name, mutant) in mutants {
        let error =
            validate_workload_policy("lookup-mutant.yaml", mutant, &WorkloadAbi::P2Components)
                .expect_err("invalid allowedIpNameLookups structure must fail closed");
        assert!(
            error.contains("must contain exactly one allowedIpNameLookups field"),
            "{name} mutation failed for an unexpected reason: {error}"
        );
    }
}

#[test]
fn execution_host_inventory_ignores_cfg_test_store_constructor() {
    let source = format!(
        "{EXECUTION_HOST_STORE_CONSTRUCTOR}\n\
         {CFG_TEST_MODULE}\n\
             {EXECUTION_HOST_STORE_CONSTRUCTOR}\n\
         }}\n"
    );
    assert_eq!(
        source.matches(EXECUTION_HOST_STORE_CONSTRUCTOR).count(),
        2,
        "fixture must contain identical production and test-only constructors"
    );
    validate_execution_host_store_constructor(&source)
        .expect("the cfg(test) constructor must not widen the production inventory");
}

#[test]
fn execution_host_inventory_rejects_removed_or_duplicated_production_constructor() {
    let test_module = format!(
        "{CFG_TEST_MODULE}\n\
             {EXECUTION_HOST_STORE_CONSTRUCTOR}\n\
         }}\n"
    );
    let removed = validate_execution_host_store_constructor(&test_module)
        .expect_err("removing the production ExecutionHost constructor must fail");
    assert!(
        removed.ends_with("found 0"),
        "removed-constructor failure must report the production count: {removed}"
    );

    let duplicated = format!(
        "{EXECUTION_HOST_STORE_CONSTRUCTOR}\n\
         {EXECUTION_HOST_STORE_CONSTRUCTOR}\n\
         {test_module}"
    );
    let duplicate = validate_execution_host_store_constructor(&duplicated)
        .expect_err("duplicating the production ExecutionHost constructor must fail");
    assert!(
        duplicate.ends_with("found 2"),
        "duplicate-constructor failure must report the production count: {duplicate}"
    );
}
