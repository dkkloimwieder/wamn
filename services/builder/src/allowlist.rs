//! 5.5c — the dependency allowlist, enforced BEFORE the cargo build.
//!
//! A supply-chain gate: `cargo metadata` resolves the node crate's transitive
//! package set, and every crate NAME in it must be on a pinned policy
//! ([`policy/default-allowlist.toml`], the actual `sample-node` closure). A
//! denied dependency refuses the build, naming it ([`AllowlistError`]) — the
//! node never compiles, so an off-policy crate's `build.rs` never runs.
//!
//! The jco path has no cargo graph; its v0 rule is structural (a single ES
//! module, no npm dependency closure). An npm dependency allowlist beyond
//! single-module is a deferral.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use serde::Deserialize;
use tokio::process::Command;

/// The pinned default dependency allowlist, embedded so the binary carries it
/// with no external file.
pub const DEFAULT_ALLOWLIST_TOML: &str = include_str!("../policy/default-allowlist.toml");

#[derive(Debug, Deserialize)]
struct PolicyFile {
    allowed: Vec<String>,
}

/// The dependency allowlist policy: the set of crate NAMES a node's resolved
/// package set may contain. Version-agnostic (a supply-chain surface policy).
#[derive(Debug, Clone)]
pub struct Policy {
    /// The allowlisted crate names.
    pub allowed: BTreeSet<String>,
}

impl Policy {
    /// Parse a policy from TOML (`allowed = [ … ]`).
    pub fn from_toml(src: &str) -> anyhow::Result<Self> {
        let file: PolicyFile = toml::from_str(src).context("parse dependency allowlist TOML")?;
        Ok(Policy {
            allowed: file.allowed.into_iter().collect(),
        })
    }

    /// The built-in default policy (the embedded `sample-node` closure).
    pub fn default_policy() -> Self {
        Self::from_toml(DEFAULT_ALLOWLIST_TOML).expect("the built-in allowlist parses")
    }

    /// Load a policy from a TOML file on disk (overrides the default).
    pub async fn load(path: &Path) -> anyhow::Result<Self> {
        let src = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("read allowlist policy {}", path.display()))?;
        Self::from_toml(&src)
    }
}

/// A dependency-allowlist refusal (5.5c): the node's resolved package set
/// contains crate name(s) outside the policy.
#[derive(Debug)]
pub enum AllowlistError {
    /// The node crate transitively depends on package(s) not on the allowlist,
    /// named (sorted) in `denied`.
    DisallowedDependencies {
        /// The node crate being built.
        package: String,
        /// The off-allowlist dependency names, sorted and de-duplicated.
        denied: Vec<String>,
    },
}

impl std::fmt::Display for AllowlistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AllowlistError::DisallowedDependencies { package, denied } => write!(
                f,
                "node {package:?} depends on non-allowlisted crate(s) {denied:?} — the dependency \
                 allowlist (5.5c) refuses the build; add them to the policy only after review"
            ),
        }
    }
}

impl std::error::Error for AllowlistError {}

/// Pure check: every name in `resolved` must be on `policy.allowed`; the sorted,
/// de-duplicated offenders refuse. `package` names the node in the refusal.
/// The mutation-(b) target — bypassing this admits an off-policy dependency.
pub fn check_allowlist(
    resolved: &[String],
    policy: &Policy,
    package: &str,
) -> Result<(), AllowlistError> {
    let mut denied: Vec<String> = resolved
        .iter()
        .filter(|name| !policy.allowed.contains(*name))
        .cloned()
        .collect();
    denied.sort();
    denied.dedup();
    if denied.is_empty() {
        Ok(())
    } else {
        Err(AllowlistError::DisallowedDependencies {
            package: package.to_string(),
            denied,
        })
    }
}

// --- cargo metadata resolve-graph parsing (no cargo_metadata crate) ---------

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetaPkg>,
    resolve: Resolve,
}
#[derive(Deserialize)]
struct MetaPkg {
    id: String,
    name: String,
    /// The absolute path of the package's `Cargo.toml` — its ROOT (the parent)
    /// is where sibling files like `cases.json` (11.5) live. Reused from the
    /// single `cargo metadata` run so a workspace build discovers the crate dir
    /// (the `--source` is the workspace, not the crate).
    manifest_path: String,
}
#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<ResolveNode>,
}
#[derive(Deserialize)]
struct ResolveNode {
    id: String,
    deps: Vec<ResolveDep>,
}
#[derive(Deserialize)]
struct ResolveDep {
    pkg: String,
}

/// The crate NAMES of `package`'s transitive dependency closure, parsed from a
/// `cargo metadata` JSON document — EXCLUDING `package` itself (the allowlist
/// governs dependencies, not the node). Sorted + de-duplicated. Pub so the build
/// pipeline can run `cargo metadata` ONCE ([`cargo_metadata_json`]) and reuse the
/// document for both the allowlist and the manifest build.
pub fn closure_names(metadata_json: &str, package: &str) -> anyhow::Result<Vec<String>> {
    let meta: Metadata =
        serde_json::from_str(metadata_json).context("parse cargo metadata JSON")?;
    let root_id = meta
        .packages
        .iter()
        .find(|p| p.name == package)
        .map(|p| p.id.clone())
        .with_context(|| format!("package {package:?} not found in cargo metadata"))?;
    let nodes: HashMap<&str, &ResolveNode> = meta
        .resolve
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();
    let id_to_name: HashMap<&str, &str> = meta
        .packages
        .iter()
        .map(|p| (p.id.as_str(), p.name.as_str()))
        .collect();

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![root_id.clone()];
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        if let Some(node) = nodes.get(cur.as_str()) {
            for dep in &node.deps {
                stack.push(dep.pkg.clone());
            }
        }
    }

    let mut names: Vec<String> = seen
        .iter()
        .filter(|id| id.as_str() != root_id) // exclude the node crate itself
        .filter_map(|id| id_to_name.get(id.as_str()).map(|n| n.to_string()))
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// The filesystem path of `package`'s `Cargo.toml`, parsed from the SAME
/// `cargo metadata` document. The node crate's ROOT — the manifest's parent — is
/// where sibling files like `cases.json` (11.5) are discovered, because a
/// workspace build's `--source` is the workspace dir, not the crate dir.
pub fn package_manifest_path(metadata_json: &str, package: &str) -> anyhow::Result<PathBuf> {
    let meta: Metadata =
        serde_json::from_str(metadata_json).context("parse cargo metadata JSON")?;
    meta.packages
        .iter()
        .find(|p| p.name == package)
        .map(|p| PathBuf::from(&p.manifest_path))
        .with_context(|| format!("package {package:?} not found in cargo metadata"))
}

/// Run `cargo metadata --offline` for `manifest_path` and return the raw JSON. A
/// build sandbox is egress-denied, so the graph must resolve from the
/// vendored/cached index. Shared by the allowlist stage and the manifest build
/// (one metadata run per build).
pub async fn cargo_metadata_json(manifest_path: &Path) -> anyhow::Result<String> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--manifest-path",
        ])
        .arg(manifest_path)
        .output()
        .await
        .context("spawn cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).context("cargo metadata output is not UTF-8")
}

/// [`cargo_metadata_json`] + [`closure_names`] — `package`'s transitive
/// dependency closure names.
pub async fn resolved_package_names(
    manifest_path: &Path,
    package: &str,
) -> anyhow::Result<Vec<String>> {
    let json = cargo_metadata_json(manifest_path).await?;
    closure_names(&json, package)
}

/// jco v0: a node is a SINGLE ES module with no npm dependency closure. Assert
/// the source directory carries no `package.json` declaring dependencies (an npm
/// dependency allowlist beyond single-module is a deferral).
pub async fn assert_jco_single_module(source: &Path) -> anyhow::Result<()> {
    let pkg_json = source.join("package.json");
    if !tokio::fs::try_exists(&pkg_json).await.unwrap_or(false) {
        return Ok(());
    }
    let src = tokio::fs::read_to_string(&pkg_json)
        .await
        .with_context(|| format!("read {}", pkg_json.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&src).with_context(|| format!("parse {}", pkg_json.display()))?;
    for field in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        if let Some(obj) = value.get(field).and_then(|v| v.as_object())
            && !obj.is_empty()
        {
            bail!(
                "jco node {}: package.json declares {field:?} — v0 requires a single ES module \
                 with no npm dependency closure (an npm dependency allowlist is deferred)",
                source.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_parses_and_pins_the_sdk_path() {
        let policy = Policy::default_policy();
        // The load-bearing node-SDK path is pinned.
        assert!(policy.allowed.contains("wamn-node-sdk"));
        assert!(policy.allowed.contains("wamn-node-guest"));
        assert!(policy.allowed.contains("serde_json"));
        // A crate NOT in the sample-node closure is not on the list.
        assert!(!policy.allowed.contains("tokio"));
        assert!(!policy.allowed.contains("hex"));
    }

    #[test]
    fn allowed_closure_passes() {
        let policy = Policy::default_policy();
        let resolved = vec![
            "serde_json".to_string(),
            "wamn-node-sdk".to_string(),
            "wamn-node-guest".to_string(),
            "wit-bindgen".to_string(),
        ];
        assert!(check_allowlist(&resolved, &policy, "sample-node").is_ok());
    }

    /// MUTATION (b) TARGET. A resolved set carrying an off-allowlist crate
    /// (`hex`, `reqwest`) is REFUSED, naming the offenders sorted. Bypassing
    /// [`check_allowlist`] (returning `Ok`) admits them and flips this.
    #[test]
    fn disallowed_dependency_is_refused_by_name() {
        let policy = Policy::default_policy();
        let resolved = vec![
            "serde_json".to_string(),
            "reqwest".to_string(),
            "wamn-node-sdk".to_string(),
            "hex".to_string(),
        ];
        match check_allowlist(&resolved, &policy, "evil-node") {
            Err(AllowlistError::DisallowedDependencies { package, denied }) => {
                assert_eq!(package, "evil-node");
                assert_eq!(denied, vec!["hex".to_string(), "reqwest".to_string()]);
            }
            Ok(()) => {
                panic!("an off-allowlist dependency was ADMITTED — the supply-chain gate is open")
            }
        }
    }

    /// Minimal synthetic cargo-metadata doc: root -> a -> b (each with the
    /// `manifest_path` real cargo metadata always carries).
    const SYNTHETIC_METADATA: &str = r#"{
      "packages": [
        {"id":"root 0.1.0 (path+file:///r)","name":"root","manifest_path":"/r/samples/root/Cargo.toml"},
        {"id":"a 1.0.0 (registry)","name":"a","manifest_path":"/home/.cargo/a/Cargo.toml"},
        {"id":"b 2.0.0 (registry)","name":"b","manifest_path":"/home/.cargo/b/Cargo.toml"}
      ],
      "resolve": {
        "nodes": [
          {"id":"root 0.1.0 (path+file:///r)","deps":[{"pkg":"a 1.0.0 (registry)"}]},
          {"id":"a 1.0.0 (registry)","deps":[{"pkg":"b 2.0.0 (registry)"}]},
          {"id":"b 2.0.0 (registry)","deps":[]}
        ]
      }
    }"#;

    #[test]
    fn closure_names_excludes_root_and_walks_deps() {
        assert_eq!(
            closure_names(SYNTHETIC_METADATA, "root").unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn package_manifest_path_reads_the_named_crate_root() {
        // 11.5: the crate ROOT (the manifest's parent) is where cases.json lives.
        let path = package_manifest_path(SYNTHETIC_METADATA, "root").unwrap();
        assert_eq!(path, PathBuf::from("/r/samples/root/Cargo.toml"));
        assert_eq!(
            path.parent().unwrap().join("cases.json"),
            PathBuf::from("/r/samples/root/cases.json")
        );
        // An unknown package is an error (not a silent skip).
        assert!(package_manifest_path(SYNTHETIC_METADATA, "nope").is_err());
    }
}
