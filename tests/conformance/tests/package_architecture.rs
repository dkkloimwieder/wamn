use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ROOT_WORKSPACE: &str = "root";
/// The guests live in more than one Cargo workspace. Feature unification is
/// additive-only inside one invocation, so the `no_std` palette guests are
/// isolated from the members that reach `serde_json/std` (wamn-0h0g.11.56).
const COMPONENT_WORKSPACES: [(&str, &str, usize); 2] = [
    ("components", "components/Cargo.toml", 7),
    ("components-no-std", "components/no-std/Cargo.toml", 2),
];
const ROOT_MEMBER_COUNT: usize = 36;

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum TargetClass {
    Guest,
    Shared,
    Native,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum NativeKind {
    RuntimeInfrastructure,
    Protocol,
    Toolchain,
    OperatorTool,
    Capability,
    TransitionRuntime,
    ProofInfrastructure,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePolicy {
    native_kind: NativeKind,
    native_reason: String,
    owner: String,
    credential_failure_scope: String,
    tested_boundary: String,
    exit_condition: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackagePolicy {
    workspace: String,
    name: String,
    manifest_path: String,
    role: Role,
    target_class: TargetClass,
    bounded_context: String,
    deployable: bool,
    native: Option<NativePolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NonCargoInput {
    name: String,
    path: String,
    role: Role,
    target_class: TargetClass,
    bounded_context: String,
    deployable: bool,
    release_class: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptedEdge {
    from_manifest: String,
    to_manifest: String,
    dependency_kind: String,
    target: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionException {
    id: String,
    from_manifest: String,
    to_manifest: String,
    dependency_kind: String,
    target: String,
    owner: String,
    expiry_bead: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphDelta {
    accepted_edges: Vec<AcceptedEdge>,
    transition_exceptions: Vec<TransitionException>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchitectureManifest {
    schema_version: String,
    graph_delta: GraphDelta,
    packages: Vec<PackagePolicy>,
    non_cargo_inputs: Vec<NonCargoInput>,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
    resolve: Option<CargoResolve>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    manifest_path: String,
}

#[derive(Debug, Deserialize)]
struct CargoResolve {
    nodes: Vec<CargoNode>,
}

#[derive(Debug, Deserialize)]
struct CargoNode {
    id: String,
    deps: Vec<CargoDependency>,
}

#[derive(Debug, Deserialize)]
struct CargoDependency {
    pkg: String,
    dep_kinds: Vec<CargoDependencyKind>,
}

#[derive(Debug, Deserialize)]
struct CargoDependencyKind {
    kind: Option<String>,
    target: Option<String>,
}

#[derive(Clone, Debug)]
struct GraphEdge {
    from_manifest: String,
    to_manifest: String,
    dependency_kind: String,
    target: String,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn architecture_manifest_path(root: &Path) -> PathBuf {
    root.join("architecture/package-roles.json")
}

fn read_architecture_manifest(root: &Path) -> ArchitectureManifest {
    let path = architecture_manifest_path(root);
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

fn cargo_metadata(root: &Path, manifest_path: &str) -> CargoMetadata {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "metadata",
            "--manifest-path",
            manifest_path,
            "--locked",
            "--offline",
            "--format-version",
            "1",
        ])
        .output()
        .unwrap_or_else(|error| {
            panic!("failed to run cargo metadata for {manifest_path}: {error}")
        });
    assert!(
        output.status.success(),
        "cargo metadata failed for {manifest_path}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid cargo metadata for {manifest_path}: {error}"))
}

fn relative_manifest(root: &Path, manifest_path: &str) -> String {
    Path::new(manifest_path)
        .strip_prefix(root)
        .unwrap_or_else(|_| panic!("{manifest_path} is outside {}", root.display()))
        .to_string_lossy()
        .replace('\\', "/")
}

fn package_by_id(metadata: &CargoMetadata) -> BTreeMap<&str, &CargoPackage> {
    metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect()
}

fn workspace_manifest_paths(root: &Path, metadata: &CargoMetadata) -> BTreeSet<String> {
    let packages = package_by_id(metadata);
    metadata
        .workspace_members
        .iter()
        .map(|id| {
            let package = packages
                .get(id.as_str())
                .unwrap_or_else(|| panic!("workspace member {id} missing from cargo packages"));
            relative_manifest(root, &package.manifest_path)
        })
        .collect()
}

fn graph_edges(root: &Path, metadata: &CargoMetadata) -> Vec<GraphEdge> {
    let packages = package_by_id(metadata);
    let workspace_members = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let resolve = metadata
        .resolve
        .as_ref()
        .expect("cargo metadata must include the resolved dependency graph");
    let mut edges = Vec::new();

    for node in &resolve.nodes {
        if !workspace_members.contains(node.id.as_str()) {
            continue;
        }
        let Some(from_package) = packages.get(node.id.as_str()) else {
            panic!("resolved source {} missing from cargo packages", node.id);
        };
        let from_manifest = relative_manifest(root, &from_package.manifest_path);

        for dependency in &node.deps {
            let Some(to_package) = packages.get(dependency.pkg.as_str()) else {
                panic!(
                    "resolved dependency {} missing from cargo packages",
                    dependency.pkg
                );
            };
            let Ok(to_relative) = Path::new(&to_package.manifest_path).strip_prefix(root) else {
                continue;
            };
            let to_manifest = to_relative.to_string_lossy().replace('\\', "/");
            for dependency_kind in &dependency.dep_kinds {
                edges.push(GraphEdge {
                    from_manifest: from_manifest.clone(),
                    to_manifest: to_manifest.clone(),
                    dependency_kind: dependency_kind
                        .kind
                        .clone()
                        .unwrap_or_else(|| "normal".to_owned()),
                    target: dependency_kind
                        .target
                        .clone()
                        .unwrap_or_else(|| "all".to_owned()),
                });
            }
        }
    }

    edges
}

fn nonempty(value: &str) -> bool {
    !value.trim().is_empty()
}

fn validate_manifest_shape(root: &Path, manifest: &ArchitectureManifest) -> Vec<String> {
    let mut errors = Vec::new();
    if manifest.schema_version != "0.1" {
        errors.push(format!(
            "unsupported package architecture schema version {}",
            manifest.schema_version
        ));
    }

    let mut paths = BTreeSet::new();
    for package in &manifest.packages {
        if !paths.insert(package.manifest_path.as_str()) {
            errors.push(format!(
                "duplicate package manifest classification: {}",
                package.manifest_path
            ));
        }
        if package.workspace != ROOT_WORKSPACE
            && !COMPONENT_WORKSPACES
                .iter()
                .any(|(workspace, _, _)| *workspace == package.workspace)
        {
            errors.push(format!(
                "{} has unknown workspace {}",
                package.manifest_path, package.workspace
            ));
        }
        if !nonempty(&package.name) || !nonempty(&package.bounded_context) {
            errors.push(format!(
                "{} must name its package and bounded context",
                package.manifest_path
            ));
        }
        if !root.join(&package.manifest_path).is_file() {
            errors.push(format!(
                "{} does not identify an existing Cargo manifest",
                package.manifest_path
            ));
        }

        let requires_native_record =
            package.deployable && package.target_class == TargetClass::Native;
        match (&package.native, requires_native_record) {
            (None, true) => errors.push(format!(
                "undocumented native deployable: {}",
                package.manifest_path
            )),
            (Some(_), false) => errors.push(format!(
                "{} has native deployment metadata but is not a native deployable",
                package.manifest_path
            )),
            (Some(native), true) => {
                let required = [
                    native.native_reason.as_str(),
                    native.owner.as_str(),
                    native.credential_failure_scope.as_str(),
                    native.tested_boundary.as_str(),
                    native.exit_condition.as_str(),
                ];
                if required.iter().any(|value| !nonempty(value)) {
                    errors.push(format!(
                        "{} has incomplete native deployment metadata",
                        package.manifest_path
                    ));
                }
            }
            (None, false) => {}
        }
    }

    match manifest
        .packages
        .iter()
        .find(|package| package.manifest_path == "services/host/Cargo.toml")
    {
        Some(host)
            if host.role == Role::Native
                && host.deployable
                && host.native.as_ref().is_some_and(|native| {
                    native.native_kind == NativeKind::RuntimeInfrastructure
                }) => {}
        Some(_) => errors.push(
            "wamn-host must be a native deployable classified as runtime-infrastructure".to_owned(),
        ),
        None => errors.push("wamn-host package classification is missing".to_owned()),
    }

    for accepted in &manifest.graph_delta.accepted_edges {
        if [
            accepted.from_manifest.as_str(),
            accepted.to_manifest.as_str(),
            accepted.dependency_kind.as_str(),
            accepted.target.as_str(),
            accepted.reason.as_str(),
        ]
        .iter()
        .any(|value| !nonempty(value))
        {
            errors.push("accepted graph edges must be exact and documented".to_owned());
        }
    }

    let mut exception_ids = BTreeSet::new();
    for exception in &manifest.graph_delta.transition_exceptions {
        if !exception_ids.insert(exception.id.as_str()) {
            errors.push(format!(
                "duplicate transition exception id: {}",
                exception.id
            ));
        }
        if [
            exception.id.as_str(),
            exception.from_manifest.as_str(),
            exception.to_manifest.as_str(),
            exception.dependency_kind.as_str(),
            exception.target.as_str(),
            exception.owner.as_str(),
            exception.expiry_bead.as_str(),
            exception.reason.as_str(),
        ]
        .iter()
        .any(|value| !nonempty(value))
            || !exception.expiry_bead.starts_with("wamn-")
        {
            errors.push(format!(
                "transition exception {} must be exact, owner-bound, and expiry-bead-bound",
                exception.id
            ));
        }
    }

    let mut non_cargo_paths = BTreeSet::new();
    for input in &manifest.non_cargo_inputs {
        if !non_cargo_paths.insert(input.path.as_str()) {
            errors.push(format!("duplicate non-Cargo input: {}", input.path));
        }
        if [
            input.name.as_str(),
            input.path.as_str(),
            input.bounded_context.as_str(),
            input.release_class.as_str(),
        ]
        .iter()
        .any(|value| !nonempty(value))
            || !root.join(&input.path).is_dir()
        {
            errors.push(format!(
                "non-Cargo input {} has incomplete or invalid release classification",
                input.path
            ));
        }
        if input.role != Role::Test || input.target_class != TargetClass::Guest || input.deployable
        {
            errors.push(format!(
                "non-Cargo sample {} must remain guest test input, not a deployable",
                input.path
            ));
        }
    }

    errors
}

fn validate_workspace_completeness(
    root: &Path,
    manifest: &ArchitectureManifest,
    workspace: &str,
    metadata: &CargoMetadata,
) -> Vec<String> {
    let actual = workspace_manifest_paths(root, metadata);
    let expected = manifest
        .packages
        .iter()
        .filter(|package| package.workspace == workspace)
        .map(|package| package.manifest_path.clone())
        .collect::<BTreeSet<_>>();
    let package_names = metadata
        .packages
        .iter()
        .filter_map(|package| {
            Path::new(&package.manifest_path)
                .strip_prefix(root)
                .ok()
                .map(|path| (path.to_string_lossy().replace('\\', "/"), &package.name))
        })
        .collect::<BTreeMap<_, _>>();
    let policies = manifest
        .packages
        .iter()
        .map(|package| (package.manifest_path.as_str(), package.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut errors = Vec::new();

    for path in actual.difference(&expected) {
        errors.push(format!(
            "{workspace} workspace package is unclassified: {path}"
        ));
    }
    for path in expected.difference(&actual) {
        errors.push(format!(
            "{workspace} classification is not a workspace member: {path}"
        ));
    }
    for path in actual.intersection(&expected) {
        if package_names.get(path).map(|name| name.as_str()) != policies.get(path.as_str()).copied()
        {
            errors.push(format!("{path} package name does not match cargo metadata"));
        }
    }

    errors
}

fn is_production(package: &PackagePolicy) -> bool {
    !matches!(package.role, Role::Test | Role::Poc)
}

fn matches_edge(
    from_manifest: &str,
    to_manifest: &str,
    dependency_kind: &str,
    target: &str,
    edge: &GraphEdge,
) -> bool {
    from_manifest == edge.from_manifest
        && to_manifest == edge.to_manifest
        && dependency_kind == edge.dependency_kind
        && target == edge.target
}

fn validate_graph(
    packages: &BTreeMap<String, PackagePolicy>,
    graph_delta: &GraphDelta,
    edges: &[GraphEdge],
) -> Vec<String> {
    let mut errors = Vec::new();
    let mut used_exceptions = BTreeSet::new();

    for edge in edges {
        let Some(from) = packages.get(&edge.from_manifest) else {
            errors.push(format!(
                "resolved repository package is unclassified: {}",
                edge.from_manifest
            ));
            continue;
        };
        let Some(to) = packages.get(&edge.to_manifest) else {
            errors.push(format!(
                "resolved repository path dependency is unclassified: {} -> {}",
                edge.from_manifest, edge.to_manifest
            ));
            continue;
        };
        let production_edge = matches!(edge.dependency_kind.as_str(), "normal" | "build");
        if !production_edge {
            continue;
        }

        let mut violations = Vec::new();

        if from.role == Role::Contract
            && matches!(
                to.role,
                Role::Persistence | Role::Adapter | Role::Component | Role::Native
            )
        {
            violations.push("contract-to-effect-or-persistence");
        }
        if from.role == Role::Core
            && matches!(to.role, Role::Adapter | Role::Component | Role::Native)
        {
            violations.push("core-to-concrete-effect");
        }
        if from.deployable && to.deployable {
            violations.push("peer-composition-root-import");
        }
        if (from.target_class == TargetClass::Guest || from.role == Role::Component)
            && to.target_class == TargetClass::Native
        {
            violations.push("guest-to-native-contamination");
        }
        if is_production(from) && matches!(to.role, Role::Test | Role::Poc) {
            violations.push("production-to-test-or-poc");
        }
        if violations.is_empty() {
            continue;
        }

        let exception = graph_delta.transition_exceptions.iter().find(|exception| {
            matches_edge(
                &exception.from_manifest,
                &exception.to_manifest,
                &exception.dependency_kind,
                &exception.target,
                edge,
            )
        });
        if let Some(exception) = exception {
            used_exceptions.insert(exception.id.clone());
            continue;
        }

        errors.push(format!(
            "{} -> {} [{} target={}] violates {}",
            edge.from_manifest,
            edge.to_manifest,
            edge.dependency_kind,
            edge.target,
            violations.join(",")
        ));
    }

    validate_guest_transitive_closure(packages, edges, &mut errors);

    for accepted in &graph_delta.accepted_edges {
        if !edges.iter().any(|edge| {
            matches_edge(
                &accepted.from_manifest,
                &accepted.to_manifest,
                &accepted.dependency_kind,
                &accepted.target,
                edge,
            )
        }) {
            errors.push(format!(
                "accepted edge pin is absent: {} -> {} [{} target={}]",
                accepted.from_manifest,
                accepted.to_manifest,
                accepted.dependency_kind,
                accepted.target
            ));
        }
    }

    for exception in &graph_delta.transition_exceptions {
        if !used_exceptions.contains(exception.id.as_str()) {
            errors.push(format!(
                "stale transition exception {} does not match a forbidden resolved edge",
                exception.id
            ));
        }
    }

    errors
}

fn validate_guest_transitive_closure(
    packages: &BTreeMap<String, PackagePolicy>,
    edges: &[GraphEdge],
    errors: &mut Vec<String>,
) {
    let mut adjacency = BTreeMap::<&str, Vec<&GraphEdge>>::new();
    for edge in edges {
        if matches!(edge.dependency_kind.as_str(), "normal" | "build")
            && packages.contains_key(&edge.from_manifest)
            && packages.contains_key(&edge.to_manifest)
        {
            adjacency.entry(&edge.from_manifest).or_default().push(edge);
        }
    }

    for source in packages.values().filter(|package| {
        package.target_class == TargetClass::Guest || package.role == Role::Component
    }) {
        let mut pending = adjacency
            .get(source.manifest_path.as_str())
            .into_iter()
            .flatten()
            .map(|edge| {
                (
                    edge.to_manifest.as_str(),
                    vec![source.manifest_path.as_str(), edge.to_manifest.as_str()],
                )
            })
            .collect::<Vec<_>>();
        let mut visited = BTreeSet::new();

        while let Some((current, path)) = pending.pop() {
            if !visited.insert(current) {
                continue;
            }
            let target = packages
                .get(current)
                .expect("transitive adjacency contains only classified packages");
            if path.len() > 2 && target.target_class == TargetClass::Native {
                errors.push(format!(
                    "guest-to-native-transitive-contamination: {}",
                    path.join(" -> ")
                ));
                continue;
            }
            for next_edge in adjacency.get(current).into_iter().flatten() {
                let mut next_path = path.clone();
                next_path.push(next_edge.to_manifest.as_str());
                pending.push((next_edge.to_manifest.as_str(), next_path));
            }
        }
    }
}

fn package_map(manifest: &ArchitectureManifest) -> BTreeMap<String, PackagePolicy> {
    manifest
        .packages
        .iter()
        .cloned()
        .map(|package| (package.manifest_path.clone(), package))
        .collect()
}

fn assert_no_errors(context: &str, errors: Vec<String>) {
    assert!(errors.is_empty(), "{context}:\n{}", errors.join("\n"));
}

fn synthetic_package(
    name: &str,
    manifest_path: &str,
    role: Role,
    target_class: TargetClass,
    deployable: bool,
) -> PackagePolicy {
    PackagePolicy {
        workspace: ROOT_WORKSPACE.to_owned(),
        name: name.to_owned(),
        manifest_path: manifest_path.to_owned(),
        role,
        target_class,
        bounded_context: "synthetic".to_owned(),
        deployable,
        native: None,
    }
}

fn synthetic_graph_delta() -> GraphDelta {
    GraphDelta {
        accepted_edges: Vec::new(),
        transition_exceptions: Vec::new(),
    }
}

fn synthetic_edge(from: &str, to: &str, dependency_kind: &str) -> GraphEdge {
    GraphEdge {
        from_manifest: from.to_owned(),
        to_manifest: to.to_owned(),
        dependency_kind: dependency_kind.to_owned(),
        target: "all".to_owned(),
    }
}

#[test]
fn manifest_classifies_both_workspaces_and_non_cargo_release_inputs() {
    let root = repository_root();
    let manifest = read_architecture_manifest(&root);
    let root_metadata = cargo_metadata(&root, "Cargo.toml");

    assert_no_errors(
        "package architecture manifest shape",
        validate_manifest_shape(&root, &manifest),
    );
    assert_no_errors(
        "root workspace package completeness",
        validate_workspace_completeness(&root, &manifest, ROOT_WORKSPACE, &root_metadata),
    );
    assert_eq!(
        workspace_manifest_paths(&root, &root_metadata).len(),
        ROOT_MEMBER_COUNT
    );
    for (workspace, cargo_manifest, member_count) in COMPONENT_WORKSPACES {
        let component_metadata = cargo_metadata(&root, cargo_manifest);
        assert_no_errors(
            &format!("{workspace} workspace package completeness"),
            validate_workspace_completeness(&root, &manifest, workspace, &component_metadata),
        );
        assert_eq!(
            workspace_manifest_paths(&root, &component_metadata).len(),
            member_count,
            "{workspace} member count"
        );
    }
    assert!(manifest.non_cargo_inputs.is_empty());
}

#[test]
fn real_workspaces_satisfy_package_architecture() {
    let root = repository_root();
    let manifest = read_architecture_manifest(&root);
    let packages = package_map(&manifest);
    let root_metadata = cargo_metadata(&root, "Cargo.toml");
    let mut edges = graph_edges(&root, &root_metadata);
    for (_, cargo_manifest, _) in COMPONENT_WORKSPACES {
        edges.extend(graph_edges(&root, &cargo_metadata(&root, cargo_manifest)));
    }

    assert_no_errors(
        "resolved root and component workspace architecture",
        validate_graph(&packages, &manifest.graph_delta, &edges),
    );
    // Every edge has to pass the ordinary rules above.
    assert!(
        manifest.graph_delta.accepted_edges.is_empty(),
        "an accepted graph edge reappeared without a guard naming it"
    );
}

#[test]
fn peer_composition_root_import_is_rejected() {
    let from = synthetic_package(
        "service-a",
        "services/a/Cargo.toml",
        Role::Native,
        TargetClass::Native,
        true,
    );
    let to = synthetic_package(
        "service-b",
        "services/b/Cargo.toml",
        Role::Native,
        TargetClass::Native,
        true,
    );
    let packages = BTreeMap::from([
        (from.manifest_path.clone(), from),
        (to.manifest_path.clone(), to),
    ]);
    let errors = validate_graph(
        &packages,
        &synthetic_graph_delta(),
        &[synthetic_edge(
            "services/a/Cargo.toml",
            "services/b/Cargo.toml",
            "normal",
        )],
    );

    assert!(errors[0].contains("peer-composition-root-import"));
}

#[test]
fn accepted_edge_pin_cannot_waive_a_forbidden_edge() {
    let core = synthetic_package(
        "core",
        "crates/core/Cargo.toml",
        Role::Core,
        TargetClass::Shared,
        false,
    );
    let effect = synthetic_package(
        "effect",
        "crates/effect/Cargo.toml",
        Role::Adapter,
        TargetClass::Native,
        false,
    );
    let packages = BTreeMap::from([
        (core.manifest_path.clone(), core),
        (effect.manifest_path.clone(), effect),
    ]);
    let delta = GraphDelta {
        accepted_edges: vec![AcceptedEdge {
            from_manifest: "crates/core/Cargo.toml".to_owned(),
            to_manifest: "crates/effect/Cargo.toml".to_owned(),
            dependency_kind: "normal".to_owned(),
            target: "all".to_owned(),
            reason: "synthetic positive pin".to_owned(),
        }],
        transition_exceptions: Vec::new(),
    };
    let errors = validate_graph(
        &packages,
        &delta,
        &[synthetic_edge(
            "crates/core/Cargo.toml",
            "crates/effect/Cargo.toml",
            "normal",
        )],
    );

    assert!(
        errors
            .iter()
            .any(|error| error.contains("core-to-concrete-effect"))
    );
}

#[test]
fn guest_to_native_contamination_is_rejected() {
    let from = synthetic_package(
        "guest",
        "components/guest/Cargo.toml",
        Role::Component,
        TargetClass::Guest,
        true,
    );
    let to = synthetic_package(
        "native-adapter",
        "crates/native/Cargo.toml",
        Role::Adapter,
        TargetClass::Native,
        false,
    );
    let packages = BTreeMap::from([
        (from.manifest_path.clone(), from),
        (to.manifest_path.clone(), to),
    ]);
    let errors = validate_graph(
        &packages,
        &synthetic_graph_delta(),
        &[synthetic_edge(
            "components/guest/Cargo.toml",
            "crates/native/Cargo.toml",
            "normal",
        )],
    );

    assert!(errors[0].contains("guest-to-native-contamination"));
}

#[test]
fn guest_to_native_transitive_contamination_is_rejected() {
    let guest = synthetic_package(
        "guest",
        "components/guest/Cargo.toml",
        Role::Component,
        TargetClass::Guest,
        true,
    );
    let shared = synthetic_package(
        "shared-adapter",
        "crates/shared/Cargo.toml",
        Role::Adapter,
        TargetClass::Shared,
        false,
    );
    let native = synthetic_package(
        "native-adapter",
        "crates/native/Cargo.toml",
        Role::Adapter,
        TargetClass::Native,
        false,
    );
    let packages = BTreeMap::from([
        (guest.manifest_path.clone(), guest),
        (shared.manifest_path.clone(), shared),
        (native.manifest_path.clone(), native),
    ]);
    let errors = validate_graph(
        &packages,
        &synthetic_graph_delta(),
        &[
            synthetic_edge(
                "components/guest/Cargo.toml",
                "crates/shared/Cargo.toml",
                "normal",
            ),
            synthetic_edge(
                "crates/shared/Cargo.toml",
                "crates/native/Cargo.toml",
                "normal",
            ),
        ],
    );

    assert!(
        errors
            .iter()
            .any(|error| error.contains("guest-to-native-transitive-contamination"))
    );
}

#[test]
fn production_to_test_support_normal_and_build_edges_are_rejected() {
    let from = synthetic_package(
        "production",
        "crates/production/Cargo.toml",
        Role::Adapter,
        TargetClass::Native,
        false,
    );
    let to = synthetic_package(
        "test-support",
        "test-support/helper/Cargo.toml",
        Role::Test,
        TargetClass::Native,
        false,
    );
    let packages = BTreeMap::from([
        (from.manifest_path.clone(), from),
        (to.manifest_path.clone(), to),
    ]);
    let errors = validate_graph(
        &packages,
        &synthetic_graph_delta(),
        &[
            synthetic_edge(
                "crates/production/Cargo.toml",
                "test-support/helper/Cargo.toml",
                "normal",
            ),
            synthetic_edge(
                "crates/production/Cargo.toml",
                "test-support/helper/Cargo.toml",
                "build",
            ),
        ],
    );

    assert_eq!(errors.len(), 2);
    assert!(errors.iter().any(|error| error.contains("[normal")));
    assert!(errors.iter().any(|error| error.contains("[build")));
    assert!(
        errors
            .iter()
            .all(|error| error.contains("production-to-test-or-poc"))
    );
}

#[test]
fn undocumented_native_deployable_is_rejected() {
    let root = repository_root();
    let mut manifest = read_architecture_manifest(&root);
    let host = manifest
        .packages
        .iter_mut()
        .find(|package| package.name == "wamn-host")
        .expect("wamn-host classification");
    host.native = None;

    let errors = validate_manifest_shape(&root, &manifest);

    assert!(
        errors
            .iter()
            .any(|error| error.contains("undocumented native deployable"))
    );
}

#[test]
fn core_and_contract_to_concrete_effect_are_rejected() {
    let core = synthetic_package(
        "core",
        "crates/core/Cargo.toml",
        Role::Core,
        TargetClass::Shared,
        false,
    );
    let contract = synthetic_package(
        "contract",
        "crates/contract/Cargo.toml",
        Role::Contract,
        TargetClass::Shared,
        false,
    );
    let effect = synthetic_package(
        "effect",
        "crates/effect/Cargo.toml",
        Role::Adapter,
        TargetClass::Native,
        false,
    );
    let packages = BTreeMap::from([
        (core.manifest_path.clone(), core),
        (contract.manifest_path.clone(), contract),
        (effect.manifest_path.clone(), effect),
    ]);
    let errors = validate_graph(
        &packages,
        &synthetic_graph_delta(),
        &[
            synthetic_edge(
                "crates/core/Cargo.toml",
                "crates/effect/Cargo.toml",
                "normal",
            ),
            synthetic_edge(
                "crates/contract/Cargo.toml",
                "crates/effect/Cargo.toml",
                "normal",
            ),
        ],
    );

    assert_eq!(errors.len(), 2);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("core-to-concrete-effect"))
    );
    assert!(
        errors
            .iter()
            .any(|error| error.contains("contract-to-effect-or-persistence"))
    );
}
