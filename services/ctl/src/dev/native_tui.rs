//! Native operator builds and their declared package inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::Output;

use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use wamn_schema_generator::PackageManifest;
use wamn_schema_generator::client_tui::read_operator;

/// Operator targets belong to the declared component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NativePackage {
    pub component: String,
    pub manifest_path: PathBuf,
}

/// Native source trees and exact workspace files that can require a rebuild.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NativeWatchInputs {
    pub directories: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
}

/// A native build or package selection failure with its owning operation.
#[derive(Debug)]
pub(super) struct NativeTuiError {
    operation: &'static str,
    detail: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl NativeTuiError {
    fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        operation: &'static str,
        detail: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            operation,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for NativeTuiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

impl Error for NativeTuiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Resolve operator targets for the complete declared package closure.
pub(super) fn operator_packages(roots: &[PathBuf]) -> Result<Vec<NativePackage>, NativeTuiError> {
    let mut packages = BTreeMap::new();
    for root in roots {
        let path = root.join("wamn.json");
        let bytes = std::fs::read(&path).map_err(|source| {
            NativeTuiError::with_source(
                "read operator component",
                path.display().to_string(),
                source,
            )
        })?;
        let manifest = PackageManifest::from_slice(&bytes).map_err(|source| {
            NativeTuiError::with_source(
                "read operator component",
                path.display().to_string(),
                source,
            )
        })?;
        for component in manifest.components.keys() {
            if root.components().any(|part| part == Component::ParentDir)
                || !component
                    .bytes()
                    .next()
                    .is_some_and(|first| first.is_ascii_lowercase())
                || !component.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
            {
                return Err(NativeTuiError::new(
                    "name generated operator package",
                    format!("{component:?} has no safe generated spelling"),
                ));
            }
            let operator = read_operator(root, component).map_err(|source| {
                NativeTuiError::with_source(
                    "read declared UI operator",
                    root.join("ui/Cargo.toml").display().to_string(),
                    source,
                )
            })?;
            let manifest_path = if operator.is_some() {
                root.join("ui/Cargo.toml")
            } else {
                root.join("generated")
                    .join(format!("{component}-tui/Cargo.toml"))
            };
            let package = NativePackage {
                component: component.to_owned(),
                manifest_path,
            };
            if packages.contains_key(component) {
                return Err(NativeTuiError::new(
                    "select operator package",
                    format!("{component:?} is ambiguous in the declared package closure"),
                ));
            }
            packages.insert(component.to_owned(), package);
        }
    }
    Ok(packages.into_values().collect())
}

/// Select one declared component from the package closure.
pub(super) fn select_component(
    roots: &[PathBuf],
    selector: &str,
) -> Result<NativePackage, NativeTuiError> {
    operator_packages(roots)?
        .into_iter()
        .find(|package| package.component == selector)
        .ok_or_else(|| {
            NativeTuiError::new(
                "select operator package",
                format!("{selector:?} is not a component in the declared package closure"),
            )
        })
}

/// Build each selected operator through its own Cargo manifest.
pub(super) async fn build(
    package_roots: &[PathBuf],
) -> Result<BTreeMap<String, PathBuf>, NativeTuiError> {
    let packages = operator_packages(package_roots)?;
    let mut executables = BTreeMap::new();
    for package in &packages {
        let metadata = read_metadata(&package.manifest_path, true).await?;
        let target = operator_target(package, &metadata)?;
        let output = Command::new("cargo")
            .current_dir(
                target
                    .manifest_path
                    .parent()
                    .expect("Cargo manifest has a parent"),
            )
            .args([
                "build",
                "--locked",
                "--offline",
                "--message-format=json",
                "--manifest-path",
            ])
            .arg(&target.manifest_path)
            .arg("--package")
            .arg(&target.package_id)
            .arg("--bin")
            .arg(&target.binary)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| {
                NativeTuiError::with_source("build operator packages", "cannot start Cargo", source)
            })?;
        require_success("build operator packages", &output)?;
        executables.extend(artifact_paths(&[target], &output.stdout)?);
    }
    Ok(executables)
}

#[derive(Debug)]
struct OperatorTarget {
    component: String,
    manifest_path: PathBuf,
    package_id: String,
    cargo_package: String,
    binary: String,
}

fn operator_target(
    selected: &NativePackage,
    metadata: &Metadata,
) -> Result<OperatorTarget, NativeTuiError> {
    let manifest = selected.manifest_path.canonicalize().map_err(|source| {
        NativeTuiError::with_source(
            "read native operator target",
            selected.manifest_path.display().to_string(),
            source,
        )
    })?;
    let package = metadata
        .packages
        .iter()
        .find(|package| package.manifest_path == manifest)
        .ok_or_else(|| {
            NativeTuiError::new(
                "read native operator target",
                format!("Cargo metadata omitted {}", manifest.display()),
            )
        })?;
    let binaries = package
        .targets
        .iter()
        .filter(|target| target.kind.iter().any(|kind| kind == "bin"))
        .collect::<Vec<_>>();
    let [binary] = binaries.as_slice() else {
        return Err(NativeTuiError::new(
            "read native operator target",
            format!("{} must declare exactly one binary", manifest.display()),
        ));
    };
    Ok(OperatorTarget {
        component: selected.component.clone(),
        manifest_path: manifest,
        package_id: package.id.clone(),
        cargo_package: package.name.clone(),
        binary: binary.name.clone(),
    })
}

fn require_success(operation: &'static str, output: &Output) -> Result<(), NativeTuiError> {
    if output.status.success() {
        return Ok(());
    }
    let mut detail = format!("Cargo exited with {}", output.status);
    for line in output.stdout.split(|byte| *byte == b'\n') {
        if let Ok(message) = serde_json::from_slice::<Value>(line)
            && let Some(rendered) = message.pointer("/message/rendered").and_then(Value::as_str)
        {
            detail.push('\n');
            detail.push_str(rendered);
        }
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        detail.push('\n');
        detail.push_str(stderr.trim_end());
    }
    Err(NativeTuiError::new(operation, detail))
}

fn artifact_paths(
    packages: &[OperatorTarget],
    stdout: &[u8],
) -> Result<BTreeMap<String, PathBuf>, NativeTuiError> {
    let expected = packages
        .iter()
        .map(|package| {
            (
                (package.package_id.as_str(), package.binary.as_str()),
                package.component.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut executables = BTreeMap::new();
    for line in stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let message: Value = serde_json::from_slice(line).map_err(|source| {
            NativeTuiError::with_source(
                "read native Cargo artifacts",
                "Cargo emitted invalid JSON",
                source,
            )
        })?;
        if message.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(binary) = message.pointer("/target/name").and_then(Value::as_str) else {
            continue;
        };
        let Some(package_id) = message.get("package_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(component) = expected.get(&(package_id, binary)) else {
            continue;
        };
        let is_binary = message
            .pointer("/target/kind")
            .and_then(Value::as_array)
            .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")));
        if !is_binary {
            continue;
        }
        let executable = message
            .get("executable")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| {
                NativeTuiError::new(
                    "read native Cargo artifacts",
                    format!("{binary} has no executable path"),
                )
            })?;
        if !executable.is_absolute() {
            return Err(NativeTuiError::new(
                "read native Cargo artifacts",
                format!("{binary} has a relative executable path"),
            ));
        }
        if let Some(previous) = executables.insert((*component).to_owned(), executable.clone())
            && previous != executable
        {
            return Err(NativeTuiError::new(
                "read native Cargo artifacts",
                format!("{binary} names multiple executable paths"),
            ));
        }
    }
    for package in packages {
        if !executables.contains_key(&package.component) {
            return Err(NativeTuiError::new(
                "read native Cargo artifacts",
                format!(
                    "Cargo emitted no binary named {} for package {}",
                    package.binary, package.cargo_package
                ),
            ));
        }
    }
    Ok(executables)
}

/// Read normal and build dependencies without walking registry source trees.
pub(super) async fn native_dependency_roots(
    repository_root: &Path,
    selected: &[NativePackage],
) -> Result<NativeWatchInputs, NativeTuiError> {
    let root = repository_root.canonicalize().map_err(|source| {
        NativeTuiError::with_source(
            "read native dependency roots",
            "cannot resolve the repository root",
            source,
        )
    })?;
    let mut directories = BTreeSet::new();
    let mut files = BTreeSet::new();
    for package in selected {
        let metadata = read_metadata(&package.manifest_path, false).await?;
        let target = operator_target(package, &metadata)?;
        let inputs = dependency_roots(&root, &[target.package_id], &metadata)?;
        directories.extend(inputs.directories);
        files.extend(inputs.files);
        for parent in target
            .manifest_path
            .parent()
            .into_iter()
            .flat_map(Path::ancestors)
            .take_while(|parent| parent.starts_with(&root))
        {
            // Cargo and rustup also read configuration above the invocation directory.
            files.extend(
                [
                    ".cargo/config",
                    ".cargo/config.toml",
                    "rust-toolchain",
                    "rust-toolchain.toml",
                ]
                .map(|path| parent.join(path)),
            );
        }
    }
    Ok(NativeWatchInputs {
        directories: directories.into_iter().collect(),
        files: files.into_iter().collect(),
    })
}

async fn read_metadata(manifest: &Path, no_deps: bool) -> Result<Metadata, NativeTuiError> {
    let manifest = manifest.canonicalize().map_err(|source| {
        NativeTuiError::with_source(
            "read native Cargo metadata",
            manifest.display().to_string(),
            source,
        )
    })?;
    let mut command = Command::new("cargo");
    command
        .current_dir(manifest.parent().expect("Cargo manifest has a parent"))
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(manifest)
        .kill_on_drop(true);
    if no_deps {
        command.arg("--no-deps");
    }
    let output = command.output().await.map_err(|source| {
        NativeTuiError::with_source(
            "read native Cargo metadata",
            "cannot start Cargo metadata",
            source,
        )
    })?;
    require_success("read native Cargo metadata", &output)?;
    serde_json::from_slice(&output.stdout).map_err(|source| {
        NativeTuiError::with_source(
            "read native Cargo metadata",
            "Cargo metadata is invalid",
            source,
        )
    })
}

#[derive(Debug, Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    packages: Vec<MetadataPackage>,
    resolve: Option<Resolve>,
}

#[derive(Debug, Deserialize)]
struct MetadataPackage {
    id: String,
    name: String,
    manifest_path: PathBuf,
    #[serde(default)]
    targets: Vec<MetadataTarget>,
}

#[derive(Debug, Deserialize)]
struct MetadataTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Resolve {
    nodes: Vec<ResolveNode>,
}

#[derive(Debug, Deserialize)]
struct ResolveNode {
    id: String,
    deps: Vec<Dependency>,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    pkg: String,
    dep_kinds: Vec<DependencyKind>,
}

#[derive(Debug, Deserialize)]
struct DependencyKind {
    kind: Option<String>,
}

fn dependency_roots(
    repository_root: &Path,
    selected: &[String],
    metadata: &Metadata,
) -> Result<NativeWatchInputs, NativeTuiError> {
    let resolve = metadata.resolve.as_ref().ok_or_else(|| {
        NativeTuiError::new(
            "read native dependency roots",
            "Cargo metadata has no resolved graph",
        )
    })?;
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    let nodes = resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    let mut pending = Vec::new();
    for id in selected {
        if !packages.contains_key(id.as_str()) {
            return Err(NativeTuiError::new(
                "read native dependency roots",
                format!("{id} is missing from Cargo metadata"),
            ));
        }
        pending.push(id.as_str());
    }
    let mut visited = BTreeSet::new();
    let mut directories = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let package = packages.get(id).ok_or_else(|| {
            NativeTuiError::new(
                "read native dependency roots",
                format!("Cargo metadata omitted package {id}"),
            )
        })?;
        let parent = package.manifest_path.parent().ok_or_else(|| {
            NativeTuiError::new(
                "read native dependency roots",
                format!("{} has no manifest directory", package.name),
            )
        })?;
        if parent.starts_with(repository_root)
            && !parent.components().any(|part| part == Component::ParentDir)
        {
            directories.insert(parent.to_owned());
        }
        let node = nodes.get(id).ok_or_else(|| {
            NativeTuiError::new(
                "read native dependency roots",
                format!("Cargo metadata omitted resolved node {id}"),
            )
        })?;
        for dependency in &node.deps {
            if dependency
                .dep_kinds
                .iter()
                .any(|kind| kind.kind.is_none() || kind.kind.as_deref() == Some("build"))
            {
                pending.push(dependency.pkg.as_str());
            }
        }
    }
    Ok(NativeWatchInputs {
        directories: directories.into_iter().collect(),
        // Keep absent configuration files in the set so creation also invalidates.
        files: [
            "Cargo.toml",
            "Cargo.lock",
            ".cargo/config",
            ".cargo/config.toml",
            "rust-toolchain",
            "rust-toolchain.toml",
        ]
        .into_iter()
        .map(|path| metadata.workspace_root.join(path))
        .collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use serde_json::json;

    use super::*;

    fn roots() -> Vec<PathBuf> {
        ["wamn_receiving", "client_acme_receiving"]
            .into_iter()
            .map(|name| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../apps")
                    .join(name)
                    .canonicalize()
                    .expect("resolve source package root")
            })
            .collect()
    }

    #[tokio::test]
    async fn selection_uses_the_declared_component_and_emitter_spelling() {
        let selected =
            select_component(&roots(), "client_acme_receiving").expect("select exact component");
        let target = operator_target(
            &selected,
            &read_metadata(&selected.manifest_path, true).await.unwrap(),
        )
        .unwrap();
        assert_eq!(
            target.cargo_package,
            "wamn-generated-client-acme-receiving-tui"
        );
        assert_eq!(target.binary, "wamn-client-acme-receiving-tui");
        assert!(selected.manifest_path.starts_with(&roots()[1]));
        let receiving =
            select_component(&roots(), "receiving").expect("select Receiving composition");
        let target = operator_target(
            &receiving,
            &read_metadata(&receiving.manifest_path, true).await.unwrap(),
        )
        .unwrap();
        assert_eq!(target.cargo_package, "wamn-receiving-tui");
        assert_eq!(target.binary, "wamn-receiving");
        let wms_root = roots()[0].parent().unwrap().join("wamn_wms");
        let wms =
            select_component(&[wms_root], "wms").expect("select WMS by its declared component");
        let target = operator_target(
            &wms,
            &read_metadata(&wms.manifest_path, true).await.unwrap(),
        )
        .unwrap();
        assert_eq!(target.cargo_package, "wamn-generated-wms-tui");
        assert_eq!(target.binary, "wamn-wms-tui");
        assert!(select_component(&roots(), "wamn_receiving").is_err());
        assert!(select_component(&roots(), "../receiving").is_err());
        assert!(select_component(&roots(), "client-acme-receiving").is_err());
    }

    #[test]
    fn component_names_survive_a_directory_rename_and_collisions_refuse() {
        let root = std::env::temp_dir().join(format!("wamn-native-names-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create renamed app directory");
        let mut manifest: Value = serde_json::from_slice(
            &std::fs::read(roots()[0].join("wamn.json")).expect("read source manifest"),
        )
        .expect("parse source manifest");
        for names in [vec!["receiving"], vec!["receiving", "dispatch"]] {
            manifest["components"] = names
                .iter()
                .map(|name| ((*name).to_owned(), json!({"connections":["postgres"]})))
                .collect::<serde_json::Map<_, _>>()
                .into();
            std::fs::write(
                root.join("wamn.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            let packages = operator_packages(&[root.clone()]).expect("read declared components");
            assert_eq!(packages.len(), names.len());
            let selected = select_component(&[root.clone()], "receiving").unwrap();
            assert_eq!(
                selected.manifest_path,
                root.join("generated/receiving-tui/Cargo.toml")
            );
            assert!(selected.manifest_path.starts_with(&root));
            assert!(operator_packages(&[root.clone(), roots()[0].clone()]).is_err());
        }
        for names in [vec!["Receiving"], vec!["1receiving"], vec!["../receiving"]] {
            manifest["components"] = names
                .iter()
                .map(|name| ((*name).to_owned(), json!({"connections":["postgres"]})))
                .collect::<serde_json::Map<_, _>>()
                .into();
            std::fs::write(
                root.join("wamn.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            assert!(operator_packages(&[root.clone()]).is_err());
        }
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    fn artifact(binary: &str, executable: &Value) -> Value {
        json!({"reason":"compiler-artifact", "package_id":binary, "target":{"name":binary,"kind":["bin"]}, "executable":executable})
    }

    fn artifact_targets() -> Vec<OperatorTarget> {
        [
            ("receiving", "wamn-receiving"),
            ("client_acme_receiving", "wamn-client-acme-receiving-tui"),
        ]
        .into_iter()
        .map(|(component, binary)| OperatorTarget {
            component: component.to_owned(),
            manifest_path: PathBuf::from("/repo/Cargo.toml"),
            package_id: binary.to_owned(),
            cargo_package: "selected-package".to_owned(),
            binary: binary.to_owned(),
        })
        .collect()
    }

    fn lines(messages: &[Value]) -> Vec<u8> {
        let mut output = String::new();
        for message in messages {
            writeln!(output, "{message}").expect("write Cargo message fixture");
        }
        output.into_bytes()
    }

    #[test]
    fn artifacts_use_cargo_paths_and_require_every_selected_binary() {
        let packages = artifact_targets();
        let mut messages = vec![
            json!({"reason":"compiler-message", "message":{"rendered":"a diagnostic"}}),
            artifact("unrelated", &json!("/elsewhere/unrelated")),
            artifact(
                "wamn-receiving",
                &json!("/custom-target/debug/wamn-receiving"),
            ),
            artifact(
                "wamn-client-acme-receiving-tui",
                &json!("/custom-target/debug/wamn-client-acme-receiving-tui"),
            ),
            json!({"reason":"build-finished", "success":true}),
        ];
        let found =
            artifact_paths(&packages, &lines(&messages)).expect("read all emitted binaries");
        assert_eq!(
            found["receiving"],
            Path::new("/custom-target/debug/wamn-receiving")
        );
        assert_eq!(found.len(), 2);
        messages[3]["package_id"] = json!("unselected-package-with-the-same-binary");
        assert!(artifact_paths(&packages, &lines(&messages)).is_err());
        messages.remove(3);
        assert!(artifact_paths(&packages, &lines(&messages)).is_err());
    }

    #[test]
    fn missing_relative_and_conflicting_executables_refuse() {
        let packages = artifact_targets().into_iter().take(1).collect::<Vec<_>>();
        for executable in [Value::Null, json!("target/debug/wamn-receiving")] {
            assert!(
                artifact_paths(
                    &packages,
                    &lines(&[artifact("wamn-receiving", &executable)])
                )
                .is_err()
            );
        }
        let collision = [
            artifact("wamn-receiving", &json!("/one/operator")),
            artifact("wamn-receiving", &json!("/two/operator")),
        ];
        assert!(artifact_paths(&packages, &lines(&collision)).is_err());
        assert!(artifact_paths(&packages, b"not JSON\n").is_err());
    }

    fn graph() -> Value {
        json!({
            "workspace_root":"/repo",
            "packages":[
                {"id":"operator","name":"wamn-receiving-tui","manifest_path":"/repo/apps/wamn_receiving/ui/Cargo.toml"},
                {"id":"app","name":"wamn-generated-receiving-tui","manifest_path":"/repo/apps/wamn_receiving/generated/receiving-tui/Cargo.toml"},
                {"id":"client","name":"wamn-client","manifest_path":"/repo/crates/client/core/Cargo.toml"},
                {"id":"build","name":"local-builder","manifest_path":"/repo/crates/build/Cargo.toml"},
                {"id":"dev","name":"test-only","manifest_path":"/repo/test-support/test-only/Cargo.toml"},
                {"id":"registry","name":"remote-library","manifest_path":"/registry/remote-library/Cargo.toml"},
                {"id":"patched","name":"local-patch","manifest_path":"/repo/crates/local-patch/Cargo.toml"}
            ],
            "resolve":{"nodes":[
                {"id":"operator","deps":[{"pkg":"app","dep_kinds":[{"kind":null}]}]},
                {"id":"app","deps":[
                    {"pkg":"client","dep_kinds":[{"kind":null}]},
                    {"pkg":"build","dep_kinds":[{"kind":"build"}]},
                    {"pkg":"dev","dep_kinds":[{"kind":"dev"}]},
                    {"pkg":"registry","dep_kinds":[{"kind":null}]}
                ]},
                {"id":"client","deps":[]},
                {"id":"build","deps":[]},
                {"id":"dev","deps":[]},
                {"id":"registry","deps":[{"pkg":"patched","dep_kinds":[{"kind":null}]}]},
                {"id":"patched","deps":[{"pkg":"client","dep_kinds":[{"kind":"build"}]}]}
            ]}
        })
    }

    #[test]
    fn watch_graph_follows_normal_and_build_dependencies_inside_the_repository() {
        let inputs = dependency_roots(
            Path::new("/repo"),
            &["operator".to_owned()],
            &serde_json::from_value(graph()).expect("decode graph"),
        )
        .expect("read source dependency roots");
        assert_eq!(
            inputs.directories,
            [
                "/repo/apps/wamn_receiving/generated/receiving-tui",
                "/repo/apps/wamn_receiving/ui",
                "/repo/crates/build",
                "/repo/crates/client/core",
                "/repo/crates/local-patch"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            inputs.files,
            [
                "/repo/Cargo.toml",
                "/repo/Cargo.lock",
                "/repo/.cargo/config",
                "/repo/.cargo/config.toml",
                "/repo/rust-toolchain",
                "/repo/rust-toolchain.toml"
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn a_missing_selection_or_incomplete_resolved_graph_refuses() {
        let selected = ["operator".to_owned()];
        assert!(
            dependency_roots(
                Path::new("/repo"),
                &["missing".to_owned()],
                &serde_json::from_value(graph()).expect("decode graph")
            )
            .is_err()
        );
        let mut incomplete = graph();
        incomplete["resolve"]["nodes"]
            .as_array_mut()
            .expect("nodes")
            .pop();
        assert!(
            dependency_roots(
                Path::new("/repo"),
                &selected,
                &serde_json::from_value(incomplete.clone()).expect("decode graph")
            )
            .is_err()
        );
        incomplete["resolve"] = Value::Null;
        assert!(
            dependency_roots(
                Path::new("/repo"),
                &selected,
                &serde_json::from_value(incomplete.clone()).expect("decode graph")
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn an_independent_app_workspace_owns_its_target_and_native_watches() {
        let root =
            std::env::temp_dir().join(format!("wamn-native-app-workspace-{}", std::process::id()));
        let app = root.join("different-directory");
        let ui = app.join("ui");
        let generated = app.join("generated/receiving-tui");
        std::fs::create_dir_all(ui.join("src")).unwrap();
        std::fs::create_dir_all(generated.join("src")).unwrap();
        std::fs::copy(roots()[0].join("wamn.json"), app.join("wamn.json")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = []\nexclude = [\"different-directory\"]\n",
        )
        .unwrap();
        std::fs::write(
            app.join("Cargo.toml"),
            "[workspace]\nmembers = [\"ui\", \"generated/receiving-tui\"]\nresolver = \"3\"\n",
        )
        .unwrap();
        std::fs::write(
            ui.join("Cargo.toml"),
            r#"[package]
name = "warehouse-desk"
version = "0.1.0"
edition = "2024"
[[bin]]
name = "dock-screen"
path = "src/main.rs"
[dependencies]
screens = { package = "wamn-generated-receiving-tui", path = "../generated/receiving-tui" }
"#,
        )
        .unwrap();
        std::fs::write(generated.join("Cargo.toml"), "[package]\nname = \"wamn-generated-receiving-tui\"\nversion = \"0.1.0\"\nedition = \"2024\"\n").unwrap();
        std::fs::write(ui.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(generated.join("src/lib.rs"), "pub fn screen() {}\n").unwrap();
        std::fs::write(
            app.join("Cargo.lock"),
            r#"version = 4
[[package]]
name = "wamn-generated-receiving-tui"
version = "0.1.0"
[[package]]
name = "warehouse-desk"
version = "0.1.0"
dependencies = ["wamn-generated-receiving-tui"]
"#,
        )
        .unwrap();
        let selected = select_component(&[app.clone()], "receiving").unwrap();
        let metadata = read_metadata(&selected.manifest_path, false).await.unwrap();
        assert_eq!(metadata.workspace_root, app);
        let target = operator_target(&selected, &metadata).unwrap();
        assert_eq!(target.cargo_package, "warehouse-desk");
        assert_eq!(target.binary, "dock-screen");
        assert_eq!(target.manifest_path, ui.join("Cargo.toml"));
        let executables = build(&[app.clone()])
            .await
            .expect("build the independent app");
        assert!(executables["receiving"].is_file());
        assert!(
            std::process::Command::new(&executables["receiving"])
                .status()
                .unwrap()
                .success()
        );
        let inputs = native_dependency_roots(&root, &[selected]).await.unwrap();
        assert_eq!(inputs.directories, [generated, ui]);
        assert!(inputs.files.contains(&app.join("Cargo.toml")));
        assert!(inputs.files.contains(&app.join("Cargo.lock")));
        assert!(inputs.files.contains(&app.join(".cargo/config.toml")));
        assert!(!inputs.files.contains(&root.join("Cargo.lock")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
