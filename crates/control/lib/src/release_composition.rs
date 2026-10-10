//! The components, palette components and wirings of a release, derived from
//! the package manifests and the `build.json` of each package, whose
//! `outputs.components` names the bytes `wamn build` compiled.
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
) -> Result<Vec<SelectedComponentArtifact>, CompositionError> {
    let mut selected = Vec::with_capacity(packages.len());
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
        selected.push(built_component(package, component)?);
    }
    Ok(selected)
}

/// The palette components that the wirings of each package name: each node
/// component that is not a component of the package.
pub fn select_palette_artifacts(
    packages: &[PackageInput],
) -> Result<Vec<SelectedPaletteArtifact>, CompositionError> {
    let mut named = BTreeSet::<(usize, String)>::new();
    let wirings = load_wirings(packages)?;
    for input in &wirings {
        let index = packages
            .iter()
            .position(|package| {
                *package.manifest.package.id == *input.package_id
                    && *package.manifest.package.version == *input.package_version
            })
            .expect("every wiring belongs to one of the packages");
        for node in input.wiring.nodes.values() {
            if !packages[index]
                .manifest
                .components
                .contains_key(&node.component)
            {
                named.insert((index, node.component.clone()));
            }
        }
    }
    let mut selected = Vec::with_capacity(named.len());
    for (index, component) in named {
        let package = &packages[index];
        let (declaration, _) =
            crate::package_artifact::platform_template(&package.root, &component)
                .map_err(|source| CompositionError::owner("find palette declaration", source))?;
        selected.push(SelectedPaletteArtifact {
            artifact: built_component(package, &component)?,
            declaration,
        });
    }
    Ok(selected)
}

/// The component `component` that `outputs.components` of the package's
/// `build.json` records, checked against its digest.
fn built_component(
    package: &PackageInput,
    component: &str,
) -> Result<SelectedComponentArtifact, CompositionError> {
    let output = wamn_schema_generator::output_root(&package.root);
    let receipt = wamn_schema_generator::receipt::read(&output)
        .map_err(|source| CompositionError::owner("read the package build", source))?;
    let entry = receipt
        .outputs
        .components
        .iter()
        .find(|entry| entry.name == component)
        .ok_or_else(|| {
            CompositionError::invalid(
                "select component artifact",
                format!(
                    "the build of {}@{} records no component {component}; run wamn build",
                    package.manifest.package.id, package.manifest.package.version
                ),
            )
        })?;
    let path = output.join(&entry.path);
    let bytes = fs::read(&path).map_err(|source| {
        CompositionError::owner(
            "read built component",
            anyhow!(source).context(format!("read {}", path.display())),
        )
    })?;
    let digest = wamn_engine::component_admission::component_digest(&bytes);
    if digest != entry.sha256 {
        return Err(CompositionError::invalid(
            "read built component",
            format!(
                "{} is {digest}, and build.json records {}; run wamn build",
                path.display(),
                entry.sha256
            ),
        ));
    }
    Ok(SelectedComponentArtifact {
        package_id: package.manifest.package.id.clone().into_boxed_str(),
        package_version: package.manifest.package.version.clone().into_boxed_str(),
        component: component.into(),
        path,
        digest: digest.into_boxed_str(),
    })
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
