//! The components, palette components and wirings of a release, derived from
//! the package manifests and the build plan of `tools/build-components`.
//!
//! The dev loop and `wamn-ctl upgrade-environment` both call this derivation
//! (owner ruling of 2026-10-02 on `wamn-m511.5`). No second file lists the
//! components of a release.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use anyhow::anyhow;
use serde::Deserialize;
use serde_json::Value;
use wamn_catalog::WiringDocument;
use wamn_schema_generator::PackageManifest;

/// The wiring documents of a package, relative to its root.
pub const PACKAGE_WIRINGS: &str = "publication/wirings";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionErrorKind {
    /// The inputs do not describe a release.
    Invalid,
    /// A file could not be read or parsed.
    Owner,
}

/// A refused derivation, with the operation that refused.
#[derive(Debug)]
pub struct CompositionError {
    pub kind: CompositionErrorKind,
    pub operation: &'static str,
    pub detail: Box<str>,
    pub source: Option<anyhow::Error>,
}

impl CompositionError {
    fn invalid(operation: &'static str, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind: CompositionErrorKind::Invalid,
            operation,
            detail: detail.into(),
            source: None,
        }
    }

    fn owner(operation: &'static str, source: anyhow::Error) -> Self {
        Self {
            kind: CompositionErrorKind::Owner,
            operation,
            detail: format!("{source:#}").into_boxed_str(),
            source: Some(source),
        }
    }
}

impl fmt::Display for CompositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

impl Error for CompositionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(AsRef::as_ref)
    }
}

#[derive(Debug, Deserialize)]
pub struct ComponentBuildPlan {
    pub virtualization: ComponentVirtualizationPlan,
    pub palette: Vec<PaletteArtifactPlan>,
}

/// One palette component that a wiring of a selected package names, as
/// `tools/build-components` found and built it (wamn-hw3n).
#[derive(Clone, Debug, Deserialize)]
pub struct PaletteArtifactPlan {
    pub component: String,
    pub declaration: PathBuf,
    pub artifact: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct ComponentVirtualizationPlan {
    pub artifacts: Vec<ComponentArtifactPlan>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ComponentArtifactPlan {
    pub package: String,
    pub output: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SelectedComponentArtifact {
    pub package_id: Box<str>,
    pub package_version: Box<str>,
    pub component: Box<str>,
    pub path: PathBuf,
    pub digest: Box<str>,
}

/// A palette component admitted into the scope of the package whose wiring
/// names it.
#[derive(Clone, Debug)]
pub struct SelectedPaletteArtifact {
    pub artifact: SelectedComponentArtifact,
    pub declaration: PathBuf,
}

#[derive(Clone, Debug)]
pub struct PackageInput {
    pub root: PathBuf,
    pub manifest: PackageManifest,
}

#[derive(Clone, Debug)]
pub struct WiringInput {
    /// The wiring document file.
    pub path: PathBuf,
    pub package_id: Box<str>,
    pub package_version: Box<str>,
    pub document: Value,
    pub wiring: WiringDocument,
}

pub fn select_component_artifacts(
    packages: &[PackageInput],
    plan: &[ComponentArtifactPlan],
) -> Result<Vec<SelectedComponentArtifact>, CompositionError> {
    let mut selected = Vec::with_capacity(packages.len());
    let mut build_packages = BTreeSet::new();
    for package in packages {
        if package.manifest.components.len() != 1 {
            return Err(CompositionError::invalid(
                "select package component artifact",
                format!(
                    "{}@{} must declare exactly one component for the POC loop",
                    package.manifest.package.id, package.manifest.package.version
                ),
            ));
        }
        let component = package
            .manifest
            .components
            .keys()
            .next()
            .expect("one package component was required above");
        let build_package = canonical_component_build_package(component);
        if !build_packages.insert(build_package.clone()) {
            return Err(CompositionError::invalid(
                "select package component artifact",
                format!("more than one package derives build identity {build_package}"),
            ));
        }
        let matches = plan
            .iter()
            .filter(|artifact| artifact.package == build_package)
            .collect::<Vec<_>>();
        let [artifact] = matches.as_slice() else {
            return Err(CompositionError::invalid(
                "select package component artifact",
                format!(
                    "{}@{} component {} derived build package {} with {} artifact matches",
                    package.manifest.package.id,
                    package.manifest.package.version,
                    component,
                    build_package,
                    matches.len()
                ),
            ));
        };
        let bytes = fs::read(&artifact.output).map_err(|source| {
            CompositionError::owner(
                "read virtualized component output",
                anyhow!(source).context(format!("read {}", artifact.output.display())),
            )
        })?;
        if bytes.is_empty() {
            return Err(CompositionError::invalid(
                "read virtualized component output",
                format!("{} is empty", artifact.output.display()),
            ));
        }
        selected.push(SelectedComponentArtifact {
            package_id: package.manifest.package.id.clone().into_boxed_str(),
            package_version: package.manifest.package.version.clone().into_boxed_str(),
            component: component.clone().into_boxed_str(),
            path: artifact.output.clone(),
            digest: wamn_engine::component_admission::component_digest(&bytes).into_boxed_str(),
        });
    }
    Ok(selected)
}

/// The palette components that the wirings of each package name. A node
/// component that the plan does not list as palette is a package component, or
/// Gate refuses it.
pub fn select_palette_artifacts(
    packages: &[PackageInput],
    plan: &[PaletteArtifactPlan],
) -> Result<Vec<SelectedPaletteArtifact>, CompositionError> {
    let mut named = BTreeSet::<(Box<str>, Box<str>, usize)>::new();
    let wirings = load_wirings(packages)?;
    for input in &wirings {
        for node in input.wiring.nodes.values() {
            if let Some(palette) = plan
                .iter()
                .position(|palette| palette.component == node.component)
            {
                named.insert((
                    input.package_id.clone(),
                    input.package_version.clone(),
                    palette,
                ));
            }
        }
    }
    let mut selected = Vec::with_capacity(named.len());
    for (package_id, package_version, palette) in named {
        let palette = &plan[palette];
        let bytes = fs::read(&palette.artifact).map_err(|source| {
            CompositionError::owner(
                "read palette component output",
                anyhow!(source).context(format!("read {}", palette.artifact.display())),
            )
        })?;
        if bytes.is_empty() {
            return Err(CompositionError::invalid(
                "read palette component output",
                format!("{} is empty", palette.artifact.display()),
            ));
        }
        selected.push(SelectedPaletteArtifact {
            artifact: SelectedComponentArtifact {
                package_id,
                package_version,
                component: palette.component.clone().into_boxed_str(),
                path: palette.artifact.clone(),
                digest: wamn_engine::component_admission::component_digest(&bytes).into_boxed_str(),
            },
            declaration: palette.declaration.clone(),
        });
    }
    Ok(selected)
}

pub fn canonical_component_build_package(component: &str) -> String {
    component.replace('_', "-")
}

/// The wiring documents of every package. A package whose operations are all
/// routes has no wiring directory.
pub fn load_wirings(packages: &[PackageInput]) -> Result<Vec<WiringInput>, CompositionError> {
    let mut inputs = Vec::new();
    for package in packages {
        let directory = package.root.join(PACKAGE_WIRINGS);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(CompositionError::owner(
                    "read package wiring directory",
                    anyhow!(source).context(format!("read {}", directory.display())),
                ));
            }
        };
        let mut paths = entries
            .map(|entry| {
                entry.map(|entry| entry.path()).map_err(|source| {
                    CompositionError::owner("read package wiring entry", source.into())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        for path in paths {
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let document = wamn_schema_generator::operation_reference::read_authored_document(
                &path,
                wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
            )
            .map_err(|source| {
                CompositionError::owner(
                    "read package wiring",
                    anyhow!(source).context(format!("read {}", path.display())),
                )
            })?;
            let wiring = WiringDocument::parse(&document).map_err(|source| {
                CompositionError::owner("validate package wiring", source.into())
            })?;
            inputs.push(WiringInput {
                path: path.clone(),
                package_id: package.manifest.package.id.clone().into_boxed_str(),
                package_version: package.manifest.package.version.clone().into_boxed_str(),
                document,
                wiring,
            });
        }
    }
    Ok(inputs)
}

/// The store alias of a palette component, from `params.store_alias` of the
/// wiring nodes of its package that name it. Two nodes that give two aliases
/// refuse.
pub fn wiring_store_alias(
    wirings: &[WiringInput],
    artifact: &SelectedComponentArtifact,
) -> Result<Option<String>, CompositionError> {
    let mut found: Option<String> = None;
    for input in wirings.iter().filter(|input| {
        input.package_id == artifact.package_id && input.package_version == artifact.package_version
    }) {
        for (node, value) in &input.wiring.nodes {
            if *value.component != *artifact.component {
                continue;
            }
            let Some(wired) = value.params.get("store_alias").and_then(Value::as_str) else {
                continue;
            };
            match &found {
                Some(alias) if alias != wired => {
                    return Err(CompositionError::invalid(
                        "read store alias",
                        format!(
                            "node {node:?} of wiring {:?} gives {}::{} the store alias {wired:?}, \
                             and another node gives {alias:?}",
                            input.wiring.wiring_id, artifact.package_id, artifact.component
                        ),
                    ));
                }
                _ => found = Some(wired.to_owned()),
            }
        }
    }
    Ok(found)
}
