use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ROOT_WORKSPACE: &str = "root";
const COMPONENT_WORKSPACE: &str = "components";
const ROOT_MANIFEST: &str = "Cargo.toml";
const COMPONENT_MANIFEST: &str = "components/Cargo.toml";
const TIER_MANIFEST: &str = "architecture/workspace-tiers.json";
const PACKAGE_ROLES_MANIFEST: &str = "architecture/package-roles.json";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceTierManifest {
    schema_version: u32,
    selection: Selection,
    source_inventory: SourceInventory,
    tiers: Tiers,
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
    component_workspace: WorkspaceInventory,
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
    role: Role,
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

fn names(values: &[String]) -> BTreeSet<String> {
    values.iter().cloned().collect()
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
fn workspace_tier_inventory_matches_live_cargo_metadata() {
    let root = repository_root();
    let manifest: WorkspaceTierManifest = read_json(&root, TIER_MANIFEST);
    let roles: PackageRoleManifest = read_json(&root, PACKAGE_ROLES_MANIFEST);
    let root_metadata = cargo_metadata(&root, ROOT_MANIFEST);
    let component_metadata = cargo_metadata(&root, COMPONENT_MANIFEST);
    let root_names = workspace_names(&root_metadata);
    let component_names = workspace_names(&component_metadata);

    assert_eq!(manifest.schema_version, 1);
    assert_eq!(
        manifest.source_inventory.root_workspace.manifest,
        ROOT_MANIFEST
    );
    assert_eq!(
        manifest.source_inventory.component_workspace.manifest,
        COMPONENT_MANIFEST
    );
    assert_eq!(
        root_names.len(),
        manifest.source_inventory.root_workspace.package_count
    );
    assert_eq!(
        component_names.len(),
        manifest.source_inventory.component_workspace.package_count
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
        .filter(|package| package.workspace == COMPONENT_WORKSPACE)
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
    let component_metadata = cargo_metadata(&root, COMPONENT_MANIFEST);

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
            package.workspace == COMPONENT_WORKSPACE
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
        workspace_names(&component_metadata),
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
        .filter(|package| package.workspace == COMPONENT_WORKSPACE && package.deployable)
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
        let kinds = package_target_kinds(&component_metadata, package);
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

    let fast_closure = path_dependency_closure(
        &root_metadata,
        &names(&manifest.tiers.fast_developer_native.root_packages),
    );
    assert!(
        fast_closure.is_subset(&names(&manifest.tiers.fast_developer_native.root_packages)),
        "fast developer selectors have a root path dependency outside the selected production set"
    );
    let product_closure = path_dependency_closure(
        &component_metadata,
        &names(&manifest.tiers.product_components.component_packages),
    );
    let root_names = workspace_names(&root_metadata);
    let product_root_dependencies = product_closure
        .intersection(&root_names)
        .cloned()
        .collect::<BTreeSet<_>>();
    assert!(
        product_root_dependencies
            .is_subset(&names(&manifest.tiers.fast_developer_native.root_packages)),
        "product components depend on a root package outside the fast production set"
    );
}

#[test]
fn bare_cargo_commands_remain_exhaustive() {
    let root = repository_root();
    let manifest: WorkspaceTierManifest = read_json(&root, TIER_MANIFEST);
    let root_metadata = cargo_metadata(&root, ROOT_MANIFEST);
    let component_metadata = cargo_metadata(&root, COMPONENT_MANIFEST);

    assert_eq!(manifest.selection.mechanism, "named-explicit-selectors");
    assert!(!manifest.selection.default_members_selected);
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
    assert_exact(
        "root bare Cargo default membership",
        default_member_names(&root_metadata),
        workspace_names(&root_metadata),
    );
    assert_exact(
        "component bare Cargo default membership",
        default_member_names(&component_metadata),
        workspace_names(&component_metadata),
    );

    assert_eq!(manifest.bare_cargo_semantics.len(), 2);
    let semantics = manifest
        .bare_cargo_semantics
        .iter()
        .map(|entry| (entry.working_directory.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    for (working_directory, package_count) in [(".", 47), ("components", 18)] {
        let entry = semantics
            .get(working_directory)
            .unwrap_or_else(|| panic!("missing bare Cargo semantics for {working_directory}"));
        assert_eq!(entry.commands, ["cargo build", "cargo check", "cargo test"]);
        assert!(
            entry.selected_packages.contains(&package_count.to_string()),
            "{working_directory} bare semantics must state its live package count"
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
        "proof_receipt_sha256",
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
