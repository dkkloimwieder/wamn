use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const ROOT_WORKSPACE: &str = "root";
const ROOT_MANIFEST: &str = "Cargo.toml";
/// The guests live in more than one Cargo workspace. Feature unification is
/// additive-only inside one invocation, so the `no_std` palette guests are
/// isolated from the members that reach `serde_json/std` (wamn-0h0g.11.56).
const COMPONENT_WORKSPACES: [(&str, &str); 2] = [
    ("components", "components/Cargo.toml"),
    ("components-no-std", "components/no-std/Cargo.toml"),
];
const TIER_MANIFEST: &str = "architecture/workspace-tiers.json";
const PACKAGE_ROLES_MANIFEST: &str = "architecture/package-roles.json";
const WORKSPACE_TIER_HELPER: &str = "tools/workspace-tier";
const ROOT_DEFAULT_MEMBER_PATHS: [&str; 18] = [
    "crates/authoring/model",
    "crates/catalog/model",
    "crates/client/core",
    "crates/client/receiving",
    "crates/client/terminal",
    "crates/client/tui",
    "crates/execution/host",
    "crates/execution/router",
    "crates/execution/run-state",
    "crates/execution/scheduler",
    "crates/identity/platform",
    "crates/platform/component-policy",
    "crates/platform/pg-core",
    "crates/platform/runtime",
    "crates/scenarios/model",
    "services/executor",
    "services/host",
    "services/scenario-worker",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceTierManifest {
    schema_version: String,
    selection: Selection,
    source_inventory: SourceInventory,
    tiers: Tiers,
    #[serde(rename = "profiles")]
    _profiles: serde_json::Value,
    bare_cargo_semantics: Vec<BareCargoSemantics>,
    release_identity: ReleaseIdentity,
    measurement: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    mechanism: String,
    default_members_selected: bool,
    reason: String,
    package_classification_source: String,
    membership_evidence: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceInventory {
    root_workspace: WorkspaceInventory,
    component_workspaces: Vec<ComponentWorkspaceInventory>,
    component_workspace_split_reason: String,
    non_cargo_inputs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceInventory {
    manifest: String,
    package_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentWorkspaceInventory {
    workspace: String,
    manifest: String,
    package_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tiers {
    fast_developer_native: Tier,
    product_components: Tier,
    contract_conformance: Tier,
    full_ci: Tier,
    deployed_system_proof: Tier,
    release: Tier,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tier {
    root_packages: Vec<String>,
    component_packages: Vec<String>,
    non_cargo_inputs: Vec<String>,
    command_semantics: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BareCargoSemantics {
    working_directory: String,
    commands: Vec<String>,
    selected_packages: String,
    qualification: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseIdentity {
    membership_source: String,
    defaults_are_evidence: bool,
    admission_requires_all_fields: bool,
    sr17_owner: String,
    sr26_owner: String,
    required_join_fields: Vec<String>,
    admission_rule: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Role {
    Contract,
    Core,
    Persistence,
    Adapter,
    Component,
    Native,
    Test,
    Poc,
}

#[derive(Debug, Deserialize)]
struct PackageRole {
    workspace: String,
    name: String,
    manifest_path: String,
    role: Role,
    target_class: String,
    bounded_context: String,
    deployable: bool,
}

#[derive(Debug, Deserialize)]
struct NonCargoInput {
    path: String,
}

#[derive(Debug, Deserialize)]
struct PackageRoleManifest {
    packages: Vec<PackageRole>,
    non_cargo_inputs: Vec<NonCargoInput>,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
    workspace_default_members: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<CargoDependency>,
    targets: Vec<CargoTarget>,
}

#[derive(Debug, Deserialize)]
struct CargoDependency {
    name: String,
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    kind: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperTierSummary {
    name: String,
    root_count: usize,
    component_count: usize,
    non_cargo_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperTier {
    name: String,
    root_packages: Vec<String>,
    component_packages: Vec<String>,
    non_cargo_inputs: Vec<String>,
    command_semantics: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperPlan {
    tier: String,
    workspace: String,
    mode: String,
    working_directory: String,
    qualification: String,
    argv: Vec<String>,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn read_json<T: for<'de> Deserialize<'de>>(root: &Path, path: &str) -> T {
    let absolute = root.join(path);
    let source = fs::read_to_string(&absolute)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", absolute.display()));
    serde_json::from_str(&source)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", absolute.display()))
}

fn cargo_metadata(root: &Path, manifest: &str) -> CargoMetadata {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "metadata",
            "--manifest-path",
            manifest,
            "--locked",
            "--offline",
            "--format-version",
            "1",
        ])
        .output()
        .unwrap_or_else(|error| panic!("failed to run cargo metadata for {manifest}: {error}"));
    assert!(
        output.status.success(),
        "cargo metadata failed for {manifest}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid cargo metadata for {manifest}: {error}"))
}

fn helper_output(root: &Path, arguments: &[&str]) -> Output {
    Command::new(root.join(WORKSPACE_TIER_HELPER))
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("failed to run workspace tier helper: {error}"))
}

fn helper_json<T: for<'de> Deserialize<'de>>(output: Output, context: &str) -> T {
    assert!(
        output.status.success(),
        "{context} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{context} returned invalid JSON: {error}"))
}

fn helper_plan(root: &Path, tier: &str, workspace: &str, mode: &str) -> HelperPlan {
    helper_json(
        helper_output(root, &["dry-run", tier, workspace, mode]),
        &format!("workspace tier dry-run for {tier}/{workspace}/{mode}"),
    )
}

fn selected_plan_packages(argv: &[String]) -> Vec<String> {
    let mut selected = Vec::new();
    let mut arguments = argv.iter();
    while let Some(argument) = arguments.next() {
        if argument == "--package" {
            selected.push(
                arguments
                    .next()
                    .expect("--package must have a value")
                    .clone(),
            );
        }
    }
    selected
}

fn workspace_names(metadata: &CargoMetadata) -> BTreeSet<String> {
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    metadata
        .workspace_members
        .iter()
        .map(|id| {
            packages
                .get(id.as_str())
                .unwrap_or_else(|| panic!("workspace member {id} missing from cargo metadata"))
                .to_string()
        })
        .collect()
}

fn default_member_names(metadata: &CargoMetadata) -> BTreeSet<String> {
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    metadata
        .workspace_default_members
        .iter()
        .map(|id| {
            packages
                .get(id.as_str())
                .unwrap_or_else(|| panic!("default member {id} missing from cargo metadata"))
                .to_string()
        })
        .collect()
}

fn default_member_paths(root: &Path, metadata: &CargoMetadata) -> Vec<String> {
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    metadata
        .workspace_default_members
        .iter()
        .map(|id| {
            packages
                .get(id.as_str())
                .unwrap_or_else(|| panic!("default member {id} missing from cargo metadata"))
                .manifest_path
                .parent()
                .expect("workspace member manifest must have a package directory")
                .strip_prefix(root)
                .expect("root workspace member must live below the repository root")
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn names(values: &[String]) -> BTreeSet<String> {
    values.iter().cloned().collect()
}

/// Live metadata for every component workspace, in canonical inventory order.
fn component_metadata(root: &Path) -> Vec<(&'static str, CargoMetadata)> {
    COMPONENT_WORKSPACES
        .iter()
        .map(|(workspace, manifest)| (*workspace, cargo_metadata(root, manifest)))
        .collect()
}

/// Every component package, whichever component workspace owns it.
fn component_member_names(metadata: &[(&str, CargoMetadata)]) -> BTreeSet<String> {
    metadata
        .iter()
        .flat_map(|(_, workspace)| workspace_names(workspace))
        .collect()
}

fn is_component_workspace(workspace: &str) -> bool {
    COMPONENT_WORKSPACES
        .iter()
        .any(|(declared, _)| *declared == workspace)
}

/// The subset of a tier's component packages that one workspace owns, in the
/// tier's own order. A tier names every package it covers; the classification
/// source resolves which workspace builds it.
fn component_packages_owned_by(
    roles: &PackageRoleManifest,
    workspace: &str,
    tier_packages: &[String],
) -> Vec<String> {
    let owned = roles
        .packages
        .iter()
        .filter(|package| package.workspace == workspace)
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();
    tier_packages
        .iter()
        .filter(|package| owned.contains(package.as_str()))
        .cloned()
        .collect()
}

fn assert_exact(label: &str, actual: BTreeSet<String>, expected: BTreeSet<String>) {
    let unexpected = actual.difference(&expected).cloned().collect::<Vec<_>>();
    let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
    assert!(
        unexpected.is_empty() && missing.is_empty(),
        "{label} drifted; unexpected={unexpected:?}; missing={missing:?}"
    );
}

fn assert_sorted_unique(label: &str, values: &[String]) {
    let mut canonical = values.to_vec();
    canonical.sort();
    canonical.dedup();
    assert_eq!(
        values, canonical,
        "{label} must be sorted and contain no duplicates"
    );
}

fn all_tiers(tiers: &Tiers) -> [(&str, &Tier); 6] {
    [
        ("fast_developer_native", &tiers.fast_developer_native),
        ("product_components", &tiers.product_components),
        ("contract_conformance", &tiers.contract_conformance),
        ("full_ci", &tiers.full_ci),
        ("deployed_system_proof", &tiers.deployed_system_proof),
        ("release", &tiers.release),
    ]
}

fn path_dependency_closure(
    metadata: &CargoMetadata,
    selected: &BTreeSet<String>,
) -> BTreeSet<String> {
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    let mut closure = selected.clone();
    loop {
        let before = closure.len();
        let current = closure.iter().cloned().collect::<Vec<_>>();
        for name in current {
            let package = packages
                .get(name.as_str())
                .unwrap_or_else(|| panic!("selected package {name} missing from cargo metadata"));
            closure.extend(
                package
                    .dependencies
                    .iter()
                    .filter(|dependency| dependency.path.is_some())
                    .map(|dependency| dependency.name.clone()),
            );
        }
        if closure.len() == before {
            return closure;
        }
    }
}

fn package_target_kinds(metadata: &CargoMetadata, package_name: &str) -> BTreeSet<String> {
    metadata
        .packages
        .iter()
        .find(|package| package.name == package_name)
        .unwrap_or_else(|| panic!("selected package {package_name} missing from cargo metadata"))
        .targets
        .iter()
        .flat_map(|target| target.kind.iter().cloned())
        .collect()
}

#[test]
fn workspace_tier_helper_list_matches_manifest() {
    let root = repository_root();
    let manifest: WorkspaceTierManifest = read_json(&root, TIER_MANIFEST);
    let roles: PackageRoleManifest = read_json(&root, PACKAGE_ROLES_MANIFEST);
    let summaries: Vec<HelperTierSummary> =
        helper_json(helper_output(&root, &["list"]), "workspace tier list");
    let summary_by_name = summaries
        .iter()
        .map(|summary| (summary.name.as_str(), summary))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(summary_by_name.len(), all_tiers(&manifest.tiers).len());
    for (tier_name, tier) in all_tiers(&manifest.tiers) {
        let summary = summary_by_name
            .get(tier_name)
            .unwrap_or_else(|| panic!("workspace tier list omitted {tier_name}"));
        assert_eq!(summary.root_count, tier.root_packages.len());
        assert_eq!(summary.component_count, tier.component_packages.len());
        assert_eq!(summary.non_cargo_count, tier.non_cargo_inputs.len());

        let listed: HelperTier = helper_json(
            helper_output(&root, &["list", tier_name]),
            &format!("workspace tier list {tier_name}"),
        );
        assert_eq!(listed.name, tier_name);
        assert_eq!(listed.root_packages, tier.root_packages);
        assert_eq!(listed.component_packages, tier.component_packages);
        assert_eq!(listed.non_cargo_inputs, tier.non_cargo_inputs);
        assert_eq!(listed.command_semantics, tier.command_semantics);
    }

    let helper_source = fs::read_to_string(root.join(WORKSPACE_TIER_HELPER))
        .expect("workspace tier helper must be readable");
    // wamn-0h0g.15.137.4: this needle set is DERIVED from the role manifest and
    // so inherits every ordinary-English package name in it -- today `transform`
    // and `http-request`. Prose in the helper containing one false-fires; it was
    // measured doing exactly that. The cost is DELIBERATE: the full ruling, and
    // the measurements that rejected comment-stripping, a needle wrapper, and an
    // exclusion list, are recorded at
    // `selector_tools_do_not_duplicate_canonical_package_inventory` in
    // profile_selectors.rs. Nothing else kills the mutant this exists for -- a
    // helper hardcoding a package list EQUAL to the manifest passes every
    // behavioural tier proof in this file and fails only here.
    for package in &roles.packages {
        assert!(
            !helper_source.contains(&package.name),
            "helper duplicates package name {} instead of reading the manifest",
            package.name
        );
    }
    for input in &roles.non_cargo_inputs {
        assert!(
            !helper_source.contains(&input.path),
            "helper duplicates non-Cargo input {} instead of reading the manifest",
            input.path
        );
    }
}

#[test]
fn workspace_tier_helper_dry_run_matches_manifest() {
    let root = repository_root();
    let manifest: WorkspaceTierManifest = read_json(&root, TIER_MANIFEST);
    let roles: PackageRoleManifest = read_json(&root, PACKAGE_ROLES_MANIFEST);
    let mut cases = vec![
        (
            "fast_developer_native",
            ROOT_WORKSPACE,
            "check",
            manifest.tiers.fast_developer_native.root_packages.clone(),
            vec!["check", "--locked"],
            root.join(ROOT_MANIFEST),
        ),
        (
            "contract_conformance",
            ROOT_WORKSPACE,
            "test",
            manifest.tiers.contract_conformance.root_packages.clone(),
            vec!["test", "--locked", "--no-fail-fast"],
            root.join(ROOT_MANIFEST),
        ),
    ];
    for (workspace, cargo_manifest) in COMPONENT_WORKSPACES {
        cases.push((
            "product_components",
            workspace,
            "build-wasm",
            component_packages_owned_by(
                &roles,
                workspace,
                &manifest.tiers.product_components.component_packages,
            ),
            vec!["build", "--locked", "--target", "wasm32-wasip2"],
            root.join(cargo_manifest),
        ));
    }

    for (tier, workspace, mode, expected_packages, fixed_arguments, cargo_manifest) in cases {
        let plan = helper_plan(&root, tier, workspace, mode);
        let expected_working_directory = cargo_manifest
            .parent()
            .expect("a Cargo manifest always has a workspace directory")
            .to_path_buf();
        assert_eq!(plan.tier, tier);
        assert_eq!(plan.workspace, workspace);
        assert_eq!(plan.mode, mode);
        assert_eq!(
            Path::new(&plan.working_directory),
            expected_working_directory.as_path()
        );
        assert_eq!(
            selected_plan_packages(&plan.argv),
            expected_packages.as_slice()
        );
        assert_eq!(
            plan.qualification.as_str(),
            match tier {
                "fast_developer_native" => {
                    manifest
                        .tiers
                        .fast_developer_native
                        .command_semantics
                        .as_str()
                }
                "product_components" =>
                    manifest.tiers.product_components.command_semantics.as_str(),
                "contract_conformance" => {
                    manifest
                        .tiers
                        .contract_conformance
                        .command_semantics
                        .as_str()
                }
                _ => unreachable!("case table names only the three executable developer tiers"),
            }
        );
        assert!(
            plan.argv
                .windows(2)
                .any(|pair| pair == ["--manifest-path", &cargo_manifest.display().to_string()]),
            "{tier}/{workspace}/{mode} must use the absolute Cargo manifest"
        );
        for argument in fixed_arguments {
            assert!(
                plan.argv.iter().any(|actual| actual == argument),
                "{tier}/{workspace}/{mode} omitted fixed argument {argument}"
            );
        }
        assert!(
            !plan.argv.iter().any(|argument| argument == "-c"),
            "helper must not execute Cargo through a shell"
        );
    }
}

#[test]
fn workspace_tier_helper_runs_safely_outside_repository() {
    let root = repository_root();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow Unix epoch")
        .as_nanos();
    let scratch = std::env::temp_dir().join(format!(
        "wamn workspace tier {} {nonce}",
        std::process::id()
    ));
    fs::create_dir(&scratch).expect("failed to create workspace tier scratch directory");
    assert!(!scratch.starts_with(&root));

    let fake_cargo = scratch.join("fake cargo");
    let capture = scratch.join("captured argv");
    fs::write(
        &fake_cargo,
        r#"#!/usr/bin/env bash
set -euo pipefail
{
  printf '%s\0' "$PWD"
  printf '%s\0' "$@"
} > "$WAMN_FAKE_CARGO_LOG"
exit 23
"#,
    )
    .expect("failed to write fake Cargo executable");
    let mut permissions = fs::metadata(&fake_cargo)
        .expect("failed to read fake Cargo permissions")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_cargo, permissions).expect("failed to make fake Cargo executable");

    let helper = root.join(WORKSPACE_TIER_HELPER);
    let plan: HelperPlan = helper_json(
        Command::new(&helper)
            .current_dir(&scratch)
            .env("CARGO", &fake_cargo)
            .args(["dry-run", "full_ci", "components", "build-wasm"])
            .output()
            .expect("failed to dry-run helper outside the repository"),
        "outside-repository workspace tier dry-run",
    );
    let output = Command::new(&helper)
        .current_dir(&scratch)
        .env("CARGO", &fake_cargo)
        .env("WAMN_FAKE_CARGO_LOG", &capture)
        .args(["run", "full_ci", "components", "build-wasm"])
        .output()
        .expect("failed to run helper outside the repository");
    assert_eq!(output.status.code(), Some(23));

    let captured = fs::read(&capture).expect("fake Cargo did not capture its invocation");
    let captured = captured
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8(field.to_vec()).expect("captured argv must be UTF-8"))
        .collect::<Vec<_>>();
    let mut expected = vec![plan.working_directory.clone()];
    expected.extend(plan.argv.iter().skip(1).cloned());
    assert_eq!(captured, expected);
    assert_eq!(plan.argv.first(), Some(&fake_cargo.display().to_string()));

    fs::remove_dir_all(&scratch).expect("failed to remove workspace tier scratch directory");
}

#[test]
fn workspace_tier_helper_refuses_invalid_and_empty_selections() {
    let root = repository_root();
    let cases = [
        (
            ["dry-run", "unknown_tier", "root", "check"],
            64,
            "unknown tier",
        ),
        (
            ["dry-run", "fast_developer_native", "unknown", "check"],
            64,
            "unknown workspace",
        ),
        (
            ["dry-run", "fast_developer_native", "root", "clean"],
            64,
            "not valid",
        ),
        (
            ["dry-run", "product_components", "root", "check"],
            65,
            "empty root package selection",
        ),
    ];

    for (arguments, expected_code, expected_message) in cases {
        let output = Command::new(root.join(WORKSPACE_TIER_HELPER))
            .current_dir(std::env::temp_dir())
            .env("CARGO", "/definitely/missing/cargo")
            .args(arguments)
            .output()
            .expect("failed to run refusing workspace tier helper case");
        assert_eq!(
            output.status.code(),
            Some(expected_code),
            "unexpected status for {arguments:?}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_message),
            "{arguments:?} did not explain its refusal:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn workspace_tier_helper_full_plans_cover_both_workspaces() {
    let root = repository_root();
    let manifest: WorkspaceTierManifest = read_json(&root, TIER_MANIFEST);
    let root_metadata = cargo_metadata(&root, ROOT_MANIFEST);
    let root_plan = helper_plan(&root, "full_ci", ROOT_WORKSPACE, "test-all");

    assert_exact(
        "full-CI helper root plan",
        names(&selected_plan_packages(&root_plan.argv)),
        workspace_names(&root_metadata),
    );
    for required in ["--all-targets", "--no-fail-fast"] {
        assert!(
            root_plan.argv.iter().any(|argument| argument == required),
            "full-CI root plan omitted {required}"
        );
    }
    for (workspace, cargo_manifest) in COMPONENT_WORKSPACES {
        let plan = helper_plan(&root, "full_ci", workspace, "build-wasm");
        assert_exact(
            &format!("full-CI helper {workspace} plan"),
            names(&selected_plan_packages(&plan.argv)),
            workspace_names(&cargo_metadata(&root, cargo_manifest)),
        );
        assert!(
            plan.argv
                .windows(2)
                .any(|pair| pair == ["--target", "wasm32-wasip2"]),
            "full-CI {workspace} plan must build wasm32-wasip2"
        );
    }

    let listed: HelperTier = helper_json(
        helper_output(&root, &["list", "full_ci"]),
        "workspace tier list full_ci",
    );
    assert_eq!(
        listed.non_cargo_inputs,
        manifest.tiers.full_ci.non_cargo_inputs
    );
    assert!(listed.non_cargo_inputs.is_empty());
}

#[test]
fn workspace_tier_inventory_matches_live_cargo_metadata() {
    let root = repository_root();
    let manifest: WorkspaceTierManifest = read_json(&root, TIER_MANIFEST);
    let roles: PackageRoleManifest = read_json(&root, PACKAGE_ROLES_MANIFEST);
    let root_metadata = cargo_metadata(&root, ROOT_MANIFEST);
    let component_metadata = component_metadata(&root);
    let root_names = workspace_names(&root_metadata);
    let component_names = component_member_names(&component_metadata);

    assert_eq!(manifest.schema_version, "0.1");
    assert_eq!(
        manifest.source_inventory.root_workspace.manifest,
        ROOT_MANIFEST
    );
    assert_eq!(
        root_names.len(),
        manifest.source_inventory.root_workspace.package_count
    );
    assert_eq!(
        manifest.source_inventory.component_workspaces.len(),
        COMPONENT_WORKSPACES.len(),
        "the canonical inventory must declare every component workspace"
    );
    for (declared, (workspace, cargo_manifest)) in manifest
        .source_inventory
        .component_workspaces
        .iter()
        .zip(COMPONENT_WORKSPACES)
    {
        assert_eq!(declared.workspace, workspace);
        assert_eq!(declared.manifest, cargo_manifest);
        assert_eq!(
            workspace_names(&cargo_metadata(&root, cargo_manifest)).len(),
            declared.package_count,
            "{workspace} declared package count"
        );
    }
    // The split is a standing constraint, not an accident, so the contract has
    // to keep saying why it exists (wamn-1yj4).
    assert!(
        manifest
            .source_inventory
            .component_workspace_split_reason
            .contains("additive-only"),
        "the canonical inventory must state why the component workspaces are split"
    );

    let classified_root = roles
        .packages
        .iter()
        .filter(|package| package.workspace == ROOT_WORKSPACE)
        .map(|package| package.name.clone())
        .collect();
    let classified_components = roles
        .packages
        .iter()
        .filter(|package| is_component_workspace(&package.workspace))
        .map(|package| package.name.clone())
        .collect();
    assert_exact(
        "root package-role inventory",
        classified_root,
        root_names.clone(),
    );
    assert_exact(
        "component package-role inventory",
        classified_components,
        component_names.clone(),
    );

    for (tier_name, tier) in all_tiers(&manifest.tiers) {
        assert_sorted_unique(&format!("{tier_name}.root_packages"), &tier.root_packages);
        assert_sorted_unique(
            &format!("{tier_name}.component_packages"),
            &tier.component_packages,
        );
        assert_sorted_unique(
            &format!("{tier_name}.non_cargo_inputs"),
            &tier.non_cargo_inputs,
        );
        assert!(
            !tier.command_semantics.trim().is_empty(),
            "{tier_name} must document command semantics"
        );
        assert!(
            names(&tier.root_packages).is_subset(&root_names),
            "{tier_name} names an unknown root package"
        );
        assert!(
            names(&tier.component_packages).is_subset(&component_names),
            "{tier_name} names an unknown component package"
        );
    }

    assert_exact(
        "full_ci root coverage",
        names(&manifest.tiers.full_ci.root_packages),
        root_names,
    );
    assert_exact(
        "full_ci component coverage",
        names(&manifest.tiers.full_ci.component_packages),
        component_names,
    );
    assert_exact(
        "classified non-Cargo inputs",
        names(&manifest.source_inventory.non_cargo_inputs),
        roles
            .non_cargo_inputs
            .iter()
            .map(|input| input.path.clone())
            .collect(),
    );
    assert!(manifest.measurement.is_object());
}

#[test]
fn workspace_tier_membership_matches_live_classification() {
    let root = repository_root();
    let manifest: WorkspaceTierManifest = read_json(&root, TIER_MANIFEST);
    let roles: PackageRoleManifest = read_json(&root, PACKAGE_ROLES_MANIFEST);
    let root_metadata = cargo_metadata(&root, ROOT_MANIFEST);
    let component_metadata = component_metadata(&root);

    let expected_fast = roles
        .packages
        .iter()
        .filter(|package| {
            package.workspace == ROOT_WORKSPACE && !matches!(package.role, Role::Test | Role::Poc)
        })
        .map(|package| package.name.clone())
        .collect();
    assert_exact(
        "fast_developer_native",
        names(&manifest.tiers.fast_developer_native.root_packages),
        expected_fast,
    );

    let expected_product_components = roles
        .packages
        .iter()
        .filter(|package| {
            is_component_workspace(&package.workspace)
                && package.role == Role::Component
                && package.deployable
        })
        .map(|package| package.name.clone())
        .collect();
    assert_exact(
        "product_components",
        names(&manifest.tiers.product_components.component_packages),
        expected_product_components,
    );

    let mut expected_contract = roles
        .packages
        .iter()
        .filter(|package| package.workspace == ROOT_WORKSPACE && package.role == Role::Contract)
        .map(|package| package.name.clone())
        .collect::<BTreeSet<_>>();
    expected_contract.insert("wamn-proof-conformance".to_owned());
    assert_exact(
        "contract_conformance",
        names(&manifest.tiers.contract_conformance.root_packages),
        expected_contract,
    );

    let expected_system_root = roles
        .packages
        .iter()
        .filter(|package| {
            package.workspace == ROOT_WORKSPACE
                && (package.deployable || package.role == Role::Test)
        })
        .map(|package| package.name.clone())
        .collect();
    assert_exact(
        "deployed_system_proof root packages",
        names(&manifest.tiers.deployed_system_proof.root_packages),
        expected_system_root,
    );
    assert_exact(
        "deployed_system_proof component packages",
        names(&manifest.tiers.deployed_system_proof.component_packages),
        component_member_names(&component_metadata),
    );

    let expected_release_root = roles
        .packages
        .iter()
        .filter(|package| package.workspace == ROOT_WORKSPACE && package.deployable)
        .map(|package| package.name.clone())
        .collect();
    let expected_release_components = roles
        .packages
        .iter()
        .filter(|package| is_component_workspace(&package.workspace) && package.deployable)
        .map(|package| package.name.clone())
        .collect();
    assert_exact(
        "release root packages",
        names(&manifest.tiers.release.root_packages),
        expected_release_root,
    );
    assert_exact(
        "release component packages",
        names(&manifest.tiers.release.component_packages),
        expected_release_components,
    );
    for package in &manifest.tiers.release.root_packages {
        assert!(
            package_target_kinds(&root_metadata, package).contains("bin"),
            "release root package {package} has no executable target"
        );
    }
    for package in &manifest.tiers.release.component_packages {
        let (_, owner) = component_metadata
            .iter()
            .find(|(_, workspace)| workspace_names(workspace).contains(package))
            .unwrap_or_else(|| {
                panic!("release component package {package} is in no component workspace")
            });
        let kinds = package_target_kinds(owner, package);
        assert!(
            kinds.contains("cdylib") || kinds.contains("bin"),
            "release component package {package} has no wasm artifact target"
        );
    }

    let non_cargo_inputs = roles
        .non_cargo_inputs
        .iter()
        .map(|input| input.path.clone())
        .collect::<BTreeSet<_>>();
    assert_exact(
        "full_ci non-Cargo inputs",
        names(&manifest.tiers.full_ci.non_cargo_inputs),
        non_cargo_inputs.clone(),
    );
    assert_exact(
        "deployed_system_proof non-Cargo inputs",
        names(&manifest.tiers.deployed_system_proof.non_cargo_inputs),
        non_cargo_inputs,
    );
    assert!(manifest.tiers.release.non_cargo_inputs.is_empty());

    let root_names = workspace_names(&root_metadata);
    // The closure reaches path dependencies that are NOT root workspace members:
    // since wamn-10yt.10.29 the guest-consumed rlibs are members of the
    // components workspace and the root consumes them by path. A root tier list
    // names root members, so only the root-member part of the closure is in
    // scope here; the components side is asserted by its own tier lists below.
    let fast_closure = path_dependency_closure(
        &root_metadata,
        &names(&manifest.tiers.fast_developer_native.root_packages),
    )
    .intersection(&root_names)
    .cloned()
    .collect::<BTreeSet<_>>();
    assert!(
        fast_closure.is_subset(&names(&manifest.tiers.fast_developer_native.root_packages)),
        "fast developer selectors have a root path dependency outside the selected production set"
    );
    let selected_products = names(&manifest.tiers.product_components.component_packages);
    for (workspace, owner) in &component_metadata {
        let selected = selected_products
            .intersection(&workspace_names(owner))
            .cloned()
            .collect::<BTreeSet<_>>();
        if selected.is_empty() {
            continue;
        }
        let product_root_dependencies = path_dependency_closure(owner, &selected)
            .intersection(&root_names)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert!(
            product_root_dependencies
                .is_subset(&names(&manifest.tiers.fast_developer_native.root_packages)),
            "{workspace} product components depend on a root package outside \
             the fast production set"
        );
    }
}

#[test]
fn bare_cargo_commands_select_exact_defaults_and_workspace_remains_exhaustive() {
    let root = repository_root();
    let manifest: WorkspaceTierManifest = read_json(&root, TIER_MANIFEST);
    let root_metadata = cargo_metadata(&root, ROOT_MANIFEST);
    let component_metadata = component_metadata(&root);

    assert_eq!(
        manifest.selection.mechanism,
        "root-default-members-plus-named-explicit-selectors"
    );
    assert!(manifest.selection.default_members_selected);
    assert!(!manifest.selection.reason.trim().is_empty());
    assert_eq!(
        manifest.selection.package_classification_source,
        PACKAGE_ROLES_MANIFEST
    );
    assert!(
        manifest
            .selection
            .membership_evidence
            .contains("cargo metadata")
    );
    assert_eq!(
        default_member_paths(&root, &root_metadata),
        ROOT_DEFAULT_MEMBER_PATHS
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>(),
        "root bare Cargo defaults must match the ratified paths and charter order"
    );
    for (workspace, owner) in &component_metadata {
        assert_exact(
            &format!("{workspace} bare Cargo default membership"),
            default_member_names(owner),
            workspace_names(owner),
        );
    }

    assert_eq!(
        manifest.bare_cargo_semantics.len(),
        COMPONENT_WORKSPACES.len() + 1
    );
    let semantics = manifest
        .bare_cargo_semantics
        .iter()
        .map(|entry| (entry.working_directory.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    // Derived from the ratified path list, never restated as a second literal:
    // cfcf90a6 deleted crates/execution/flow-engine and the flow-invocation
    // package went with it, and both restated counts outlived the paths they
    // counted (wamn-0h0g.15.137). The live workspace-member count is pinned
    // against the manifest by `workspace_tier_inventory_matches_live_cargo_metadata`.
    assert_eq!(
        default_member_names(&root_metadata).len(),
        ROOT_DEFAULT_MEMBER_PATHS.len()
    );

    // The counts are MEASURED, never restated from a literal: comparing a
    // hardcoded number against the manifest's own prose checks JSON against
    // JSON and passes while both are wrong. `contains` is a substring test, so
    // it also has to match a whole token — "6" is a substring of "16"
    // (wamn-0h0g.15.137).
    let mut counted = vec![(".".to_owned(), default_member_names(&root_metadata).len())];
    for ((_, cargo_manifest), (_, owner)) in COMPONENT_WORKSPACES.iter().zip(&component_metadata) {
        counted.push((
            Path::new(cargo_manifest)
                .parent()
                .expect("a Cargo manifest always has a workspace directory")
                .to_string_lossy()
                .into_owned(),
            workspace_names(owner).len(),
        ));
    }
    for (working_directory, package_count) in counted {
        let entry = semantics
            .get(working_directory.as_str())
            .unwrap_or_else(|| panic!("missing bare Cargo semantics for {working_directory}"));
        assert_eq!(entry.commands, ["cargo build", "cargo check", "cargo test"]);
        let stated = package_count.to_string();
        assert!(
            entry
                .selected_packages
                .split_whitespace()
                .any(|token| token == stated),
            "{working_directory} bare semantics must state its live package count {stated}, \
             got {:?}",
            entry.selected_packages
        );
        assert!(!entry.qualification.trim().is_empty());
    }
}

#[test]
fn release_tier_requires_sr17_sr26_identity_join() {
    let root = repository_root();
    let manifest: WorkspaceTierManifest = read_json(&root, TIER_MANIFEST);
    let identity = &manifest.release_identity;
    let required = [
        "artifact_sha256",
        "cargo_lock_sha256",
        "oci_manifest_digest",
        "proof_artifact_sha256",
        "proof_oci_manifest_digest",
        "proof_evidence_sha256",
        "proof_source_revision",
        "source_revision",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();

    assert_eq!(
        identity.membership_source,
        "architecture/package-roles.json packages where deployable is true"
    );
    assert!(!identity.defaults_are_evidence);
    assert!(identity.admission_requires_all_fields);
    assert!(identity.sr17_owner.starts_with("SR17 / wamn-"));
    assert!(identity.sr26_owner.starts_with("SR26 / wamn-"));
    assert_exact(
        "release identity join fields",
        names(&identity.required_join_fields),
        required,
    );
    assert!(
        identity.admission_rule.contains("fail closed"),
        "release admission must fail closed on an incomplete identity join"
    );
}
