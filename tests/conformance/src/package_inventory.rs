//! The package half of every component inventory, derived rather than listed.
//!
//! `architecture/package-roles.json` declares the PLATFORM half only. A package
//! declares its own components in `packages/<package>/wamn.json`, and every
//! field of a component's role row follows from that declaration plus live
//! Cargo metadata, so a greenfield package that ships a component is authored
//! inside its own paths with no edit to any central inventory file
//! (wamn-10yt.10.39).
//!
//! The fence does not move. This is still an allowlist and not discovery:
//! nothing builds unless something declares it. What changed is WHO declares
//! the package half.
//!
//! One derivation, used by every guard. `tools/build-components` and
//! `tools/workspace-tier` derive the same set the same way in shell; a guard
//! that pinned the old hand-written rows instead would put the central edit
//! straight back.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

const TIER_MANIFEST: &str = "architecture/workspace-tiers.json";
const PACKAGE_ROLES_MANIFEST: &str = "architecture/package-roles.json";
const PACKAGE_ROOT: &str = "packages";
const PACKAGE_MANIFEST: &str = "wamn.json";

/// One component that a package declares, with its role row fully resolved.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DerivedComponent {
    /// The component workspace that actually holds the crate.
    pub workspace: String,
    /// That workspace's Cargo manifest, repository-relative.
    pub workspace_manifest: String,
    /// The Cargo package name: the declared component name, kebab-cased.
    pub name: String,
    /// The crate's own Cargo manifest, repository-relative.
    pub manifest_path: String,
    /// The declaring package's single model schema.
    pub bounded_context: String,
}

impl DerivedComponent {
    /// The package-role row this component contributes.
    ///
    /// Every field is derived: the role and target class follow from being a
    /// package component at all, a package component is deployable by
    /// definition, and the bounded context is the declaring package's own model
    /// schema.
    #[must_use]
    pub fn role_row(&self) -> Value {
        json!({
            "workspace": self.workspace,
            "name": self.name,
            "manifest_path": self.manifest_path,
            "role": "component",
            "target_class": "guest",
            "bounded_context": self.bounded_context,
            "deployable": true,
        })
    }
}

/// Every component name a package declares, kebab-cased.
///
/// Filesystem only, with no Cargo invocation: the tier and profile selections
/// need the names long before they need a crate.
#[must_use]
pub fn component_names(root: &Path) -> BTreeSet<String> {
    declared_components(root).into_keys().collect()
}

/// Every declared component paired with its declaring package's bounded
/// context.
///
/// A package's bounded context is the schema its models carry. That holds
/// exactly across the shipped packages, including the case where it is NOT the
/// component name: `packages/client_acme_receiving` declares the component
/// `client_acme_receiving` and its models carry `receiving`. A package whose
/// models carry more than one schema has no single bounded context, and a
/// package with no model has none at all; both refuse rather than guess.
fn declared_components(root: &Path) -> BTreeMap<String, String> {
    let mut declared = BTreeMap::new();
    let Ok(entries) = fs::read_dir(root.join(PACKAGE_ROOT)) else {
        return declared;
    };
    for entry in entries.flatten() {
        let manifest = entry.path().join(PACKAGE_MANIFEST);
        let Ok(source) = fs::read_to_string(&manifest) else {
            continue;
        };
        let document: Value = serde_json::from_str(&source)
            .unwrap_or_else(|error| panic!("failed to parse {}: {error}", manifest.display()));
        let Some(components) = document.get("components").and_then(Value::as_object) else {
            continue;
        };
        if components.is_empty() {
            continue;
        }

        let schemas = document
            .get("models")
            .and_then(Value::as_object)
            .map(|models| {
                models
                    .values()
                    .filter_map(|model| model.get("schema").and_then(Value::as_str))
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let context = match schemas.len() {
            1 => schemas
                .into_iter()
                .next()
                .expect("a one-element set has a first element"),
            0 => panic!(
                "{} declares a component but no model schema, so it has no bounded context",
                manifest.display()
            ),
            _ => panic!(
                "{} declares components and the model schemas {schemas:?}, so it has no single \
                 bounded context",
                manifest.display()
            ),
        };

        for name in components.keys() {
            declared.insert(name.replace('_', "-"), context.clone());
        }
    }
    declared
}

/// Every derived component, with its owning workspace and crate manifest taken
/// from live Cargo metadata.
///
/// The owner is whichever declared component workspace actually holds the
/// crate, so nothing here says where a package component must live.
#[must_use]
pub fn derived_components(root: &Path) -> Vec<DerivedComponent> {
    let workspaces = component_workspaces(root);
    let mut holders = BTreeMap::new();
    for (workspace, workspace_manifest) in &workspaces {
        for (name, manifest_path) in workspace_members(root, workspace_manifest) {
            holders.entry(name).or_insert_with(Vec::new).push((
                workspace.clone(),
                workspace_manifest.clone(),
                manifest_path,
            ));
        }
    }

    declared_components(root)
        .into_iter()
        .map(|(name, bounded_context)| {
            let owners = holders.get(&name).map(Vec::as_slice).unwrap_or_default();
            let [(workspace, workspace_manifest, manifest_path)] = owners else {
                panic!(
                    "{name} is declared by a package manifest but {} component workspace holds it",
                    if owners.is_empty() {
                        "no"
                    } else {
                        "more than one"
                    }
                );
            };
            DerivedComponent {
                workspace: workspace.clone(),
                workspace_manifest: workspace_manifest.clone(),
                name,
                manifest_path: manifest_path.clone(),
                bounded_context,
            }
        })
        .collect()
}

/// How many derived components live in the workspace this Cargo manifest owns.
///
/// The member counts the guards pin are the platform half; this is what a
/// workspace holds on top of it.
#[must_use]
pub fn derived_component_count(root: &Path, workspace_manifest: &str) -> usize {
    derived_components(root)
        .iter()
        .filter(|component| component.workspace_manifest == workspace_manifest)
        .count()
}

/// The whole package-role inventory: the declared platform half from
/// `architecture/package-roles.json`, plus the derived package half.
#[must_use]
pub fn package_roles(root: &Path) -> Value {
    let path = root.join(PACKAGE_ROLES_MANIFEST);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let mut document: Value = serde_json::from_str(&source)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    let packages = document
        .get_mut("packages")
        .and_then(Value::as_array_mut)
        .unwrap_or_else(|| panic!("{} must carry a packages array", path.display()));
    packages.extend(
        derived_components(root)
            .iter()
            .map(DerivedComponent::role_row),
    );
    document
}

/// The declared component workspaces, as `(workspace, manifest)` pairs.
fn component_workspaces(root: &Path) -> Vec<(String, String)> {
    let path = root.join(TIER_MANIFEST);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let document: Value = serde_json::from_str(&source)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    document
        .pointer("/source_inventory/component_workspaces")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{} must declare its component workspaces", path.display()))
        .iter()
        .map(|workspace| {
            let text = |field: &str| {
                workspace
                    .get(field)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("a component workspace in {} has no {field}", path.display())
                    })
                    .to_owned()
            };
            (text("workspace"), text("manifest"))
        })
        .collect()
}

/// Every member of one workspace, as `(package name, repository-relative
/// manifest)`.
fn workspace_members(root: &Path, workspace_manifest: &str) -> Vec<(String, String)> {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "metadata",
            "--manifest-path",
            workspace_manifest,
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .output()
        .unwrap_or_else(|error| {
            panic!("failed to run cargo metadata for {workspace_manifest}: {error}")
        });
    assert!(
        output.status.success(),
        "cargo metadata failed for {workspace_manifest}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid cargo metadata for {workspace_manifest}: {error}"));
    let members = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("cargo metadata for {workspace_manifest} listed no members"))
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();

    metadata
        .get("packages")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("cargo metadata for {workspace_manifest} listed no packages"))
        .iter()
        .filter(|package| {
            package
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| members.contains(id))
        })
        .map(|package| {
            let name = package
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("a member of {workspace_manifest} has no name"))
                .to_owned();
            let manifest_path = package
                .get("manifest_path")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name} has no manifest path"));
            let relative = Path::new(manifest_path)
                .strip_prefix(root)
                .unwrap_or_else(|_| panic!("{manifest_path} is outside {}", root.display()))
                .to_string_lossy()
                .replace('\\', "/");
            (name, relative)
        })
        .collect()
}
