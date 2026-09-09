//! Native operator builds and their declared package inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::Output;

use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

/// Generated names belong to the authored package directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NativePackage {
    pub root: PathBuf,
    pub directory: String,
    pub cargo_package: String,
    pub binary: String,
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

/// Resolve generated names for the complete declared package closure.
pub(super) fn generated_packages(roots: &[PathBuf]) -> Result<Vec<NativePackage>, NativeTuiError> {
    let mut packages = BTreeMap::new();
    let mut cargo_names = BTreeSet::new();
    for root in roots {
        let directory = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                NativeTuiError::new(
                    "name generated operator package",
                    format!("{} has no UTF-8 directory name", root.display()),
                )
            })?;
        if root.components().any(|part| part == Component::ParentDir)
            || !directory
                .bytes()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase())
            || !directory.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err(NativeTuiError::new(
                "name generated operator package",
                format!("{directory:?} has no safe generated spelling"),
            ));
        }
        let slug = directory.replace('_', "-");
        let package = NativePackage {
            root: root.clone(),
            directory: directory.to_owned(),
            cargo_package: format!("wamn-generated-{slug}-tui"),
            binary: format!("wamn-{slug}-tui"),
        };
        if packages.contains_key(directory) || !cargo_names.insert(package.cargo_package.clone()) {
            return Err(NativeTuiError::new(
                "select generated operator package",
                format!("{directory:?} is ambiguous in the declared package closure"),
            ));
        }
        packages.insert(directory.to_owned(), package);
    }
    Ok(packages.into_values().collect())
}

/// Select exactly one authored directory from the declared package closure.
pub(super) fn select_package(
    roots: &[PathBuf],
    selector: &str,
) -> Result<NativePackage, NativeTuiError> {
    generated_packages(roots)?
        .into_iter()
        .find(|package| package.directory == selector)
        .ok_or_else(|| {
            NativeTuiError::new(
                "select generated operator package",
                format!("{selector:?} is not a package directory in the declared closure"),
            )
        })
}

/// Build all generated native clients through the root Cargo workspace.
pub(super) async fn build(
    repository_root: &Path,
    package_roots: &[PathBuf],
) -> Result<BTreeMap<String, PathBuf>, NativeTuiError> {
    let packages = generated_packages(package_roots)?;
    if packages.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut command = Command::new("cargo");
    command
        .current_dir(repository_root)
        .args([
            "build",
            "--locked",
            "--offline",
            "--bins",
            "--message-format=json",
        ])
        .kill_on_drop(true);
    for package in &packages {
        command.arg("-p").arg(&package.cargo_package);
    }
    let output = command.output().await.map_err(|source| {
        NativeTuiError::with_source(
            "build generated operator packages",
            "cannot start Cargo",
            source,
        )
    })?;
    require_success("build generated operator packages", &output)?;
    artifact_paths(&packages, &output.stdout)
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
    packages: &[NativePackage],
    stdout: &[u8],
) -> Result<BTreeMap<String, PathBuf>, NativeTuiError> {
    let expected = packages
        .iter()
        .map(|package| (package.binary.as_str(), package.directory.as_str()))
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
        let Some(directory) = expected.get(binary) else {
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
        if let Some(previous) = executables.insert((*directory).to_owned(), executable.clone())
            && previous != executable
        {
            return Err(NativeTuiError::new(
                "read native Cargo artifacts",
                format!("{binary} names multiple executable paths"),
            ));
        }
    }
    for package in packages {
        if !executables.contains_key(&package.directory) {
            return Err(NativeTuiError::new(
                "read native Cargo artifacts",
                format!("Cargo emitted no binary named {}", package.binary),
            ));
        }
    }
    Ok(executables)
}

/// Read normal and build dependencies without walking registry source trees.
pub(super) async fn native_dependency_roots(
    repository_root: &Path,
    selected_cargo_packages: &[String],
) -> Result<NativeWatchInputs, NativeTuiError> {
    let root = repository_root.canonicalize().map_err(|source| {
        NativeTuiError::with_source(
            "read native dependency roots",
            "cannot resolve the repository root",
            source,
        )
    })?;
    let output = Command::new("cargo")
        .current_dir(&root)
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|source| {
            NativeTuiError::with_source(
                "read native dependency roots",
                "cannot start Cargo metadata",
                source,
            )
        })?;
    require_success("read native dependency roots", &output)?;
    dependency_roots(&root, selected_cargo_packages, &output.stdout)
}

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<MetadataPackage>,
    resolve: Option<Resolve>,
}

#[derive(Debug, Deserialize)]
struct MetadataPackage {
    id: String,
    name: String,
    manifest_path: PathBuf,
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
    bytes: &[u8],
) -> Result<NativeWatchInputs, NativeTuiError> {
    let metadata: Metadata = serde_json::from_slice(bytes).map_err(|source| {
        NativeTuiError::with_source(
            "read native dependency roots",
            "Cargo metadata is invalid",
            source,
        )
    })?;
    let resolve = metadata.resolve.ok_or_else(|| {
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
    for name in selected {
        let candidates = metadata
            .packages
            .iter()
            .filter(|package| package.name == *name)
            .collect::<Vec<_>>();
        let [package] = candidates.as_slice() else {
            return Err(NativeTuiError::new(
                "read native dependency roots",
                format!("{name} is missing or ambiguous in Cargo metadata"),
            ));
        };
        pending.push(package.id.as_str());
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
        .map(|path| repository_root.join(path))
        .collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use serde_json::json;

    use super::*;

    fn roots() -> Vec<PathBuf> {
        [
            "/repo/packages/receiving",
            "/repo/packages/client_acme_receiving",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }

    #[test]
    fn selection_uses_the_declared_directory_and_emitter_spelling() {
        let selected =
            select_package(&roots(), "client_acme_receiving").expect("select exact directory");
        assert_eq!(
            selected.cargo_package,
            "wamn-generated-client-acme-receiving-tui"
        );
        assert_eq!(selected.binary, "wamn-client-acme-receiving-tui");
        assert_eq!(
            selected.root,
            Path::new("/repo/packages/client_acme_receiving")
        );
        assert!(select_package(&roots(), "wamn_receiving").is_err());
        assert!(select_package(&roots(), "../receiving").is_err());
        assert!(select_package(&roots(), "client-acme-receiving").is_err());
    }

    #[test]
    fn invalid_names_and_colliding_generated_names_refuse() {
        for root in [
            "/repo/packages/../receiving",
            "/repo/packages/Receiving",
            "/repo/packages/1receiving",
            "/repo/packages/réception",
        ] {
            assert!(
                generated_packages(&[PathBuf::from(root)]).is_err(),
                "{root}"
            );
        }
        for pair in [
            ["/a/receiving", "/b/receiving"],
            ["/a/client_acme", "/b/client-acme"],
        ] {
            assert!(generated_packages(&pair.map(PathBuf::from)).is_err());
        }
    }

    fn artifact(binary: &str, executable: &Value) -> Value {
        json!({"reason":"compiler-artifact", "target":{"name":binary,"kind":["bin"]}, "executable":executable})
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
        let packages = generated_packages(&roots()).expect("name packages");
        let mut messages = vec![
            json!({"reason":"compiler-message", "message":{"rendered":"a diagnostic"}}),
            artifact("unrelated", &json!("/elsewhere/unrelated")),
            artifact(
                "wamn-receiving-tui",
                &json!("/custom-target/debug/wamn-receiving-tui"),
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
            Path::new("/custom-target/debug/wamn-receiving-tui")
        );
        assert_eq!(found.len(), 2);
        messages.remove(3);
        assert!(artifact_paths(&packages, &lines(&messages)).is_err());
    }

    #[test]
    fn missing_relative_and_conflicting_executables_refuse() {
        let packages =
            generated_packages(&[PathBuf::from("/repo/packages/receiving")]).expect("name package");
        for executable in [Value::Null, json!("target/debug/wamn-receiving-tui")] {
            assert!(
                artifact_paths(
                    &packages,
                    &lines(&[artifact("wamn-receiving-tui", &executable)])
                )
                .is_err()
            );
        }
        let collision = [
            artifact("wamn-receiving-tui", &json!("/one/operator")),
            artifact("wamn-receiving-tui", &json!("/two/operator")),
        ];
        assert!(artifact_paths(&packages, &lines(&collision)).is_err());
        assert!(artifact_paths(&packages, b"not JSON\n").is_err());
    }

    fn graph() -> Value {
        json!({
            "packages":[
                {"id":"app","name":"wamn-generated-receiving-tui","manifest_path":"/repo/packages/receiving/generated/receiving-tui/Cargo.toml"},
                {"id":"client","name":"wamn-client","manifest_path":"/repo/crates/client/core/Cargo.toml"},
                {"id":"build","name":"local-builder","manifest_path":"/repo/crates/build/Cargo.toml"},
                {"id":"dev","name":"test-only","manifest_path":"/repo/test-support/test-only/Cargo.toml"},
                {"id":"registry","name":"remote-library","manifest_path":"/registry/remote-library/Cargo.toml"},
                {"id":"patched","name":"local-patch","manifest_path":"/repo/crates/local-patch/Cargo.toml"}
            ],
            "resolve":{"nodes":[
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
            &["wamn-generated-receiving-tui".to_owned()],
            &serde_json::to_vec(&graph()).expect("encode graph"),
        )
        .expect("read source dependency roots");
        assert_eq!(
            inputs.directories,
            [
                "/repo/crates/build",
                "/repo/crates/client/core",
                "/repo/crates/local-patch",
                "/repo/packages/receiving/generated/receiving-tui"
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
        let selected = ["wamn-generated-receiving-tui".to_owned()];
        assert!(
            dependency_roots(
                Path::new("/repo"),
                &["missing".to_owned()],
                &serde_json::to_vec(&graph()).expect("encode graph")
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
                &serde_json::to_vec(&incomplete).expect("encode graph")
            )
            .is_err()
        );
        incomplete["resolve"] = Value::Null;
        assert!(
            dependency_roots(
                Path::new("/repo"),
                &selected,
                &serde_json::to_vec(&incomplete).expect("encode graph")
            )
            .is_err()
        );
    }
}
