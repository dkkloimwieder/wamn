//! The components and palette components of a release, derived from
//! the package manifests and the `build.json` of each package, whose
//! `outputs.components` names the bytes `wamn build` compiled.
//!
//! The dev loop and `wamn-ctl upgrade-environment` both call this derivation
//! (owner ruling of 2026-10-02 on `wamn-m511.5`). No second file lists the
//! components of a release.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use anyhow::anyhow;
use wamn_schema_generator::PackageManifest;

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
        wamn_schema_generator::receipt::check_current(&package.root)
            .map_err(|source| CompositionError::owner("read the package build", source))?;
        let receipt = wamn_schema_generator::receipt::read(&wamn_schema_generator::output_root(
            &package.root,
        ))
        .map_err(|source| CompositionError::owner("read the package build", source))?;
        selected.push(built_component(package, component, &receipt)?);
    }
    Ok(selected)
}

/// The palette components selected by the package build.
pub fn select_palette_artifacts(
    packages: &[PackageInput],
) -> Result<Vec<SelectedPaletteArtifact>, CompositionError> {
    let mut selected = Vec::new();
    for package in packages {
        wamn_schema_generator::receipt::check_current(&package.root)
            .map_err(|source| CompositionError::owner("read the package build", source))?;
        let receipt = wamn_schema_generator::receipt::read(&wamn_schema_generator::output_root(
            &package.root,
        ))
        .map_err(|source| CompositionError::owner("read the package build", source))?;
        for entry in &receipt.outputs.components {
            if package.manifest.components.contains_key(&entry.name) {
                continue;
            }
            let (declaration, _) =
                crate::package_artifact::platform_template(&package.root, &entry.name).map_err(
                    |source| CompositionError::owner("find palette declaration", source),
                )?;
            selected.push(SelectedPaletteArtifact {
                artifact: built_component(package, &entry.name, &receipt)?,
                declaration,
            });
        }
    }
    Ok(selected)
}

/// The component `component` that `outputs.components` of the package's
/// `build.json` records, checked against its digest.
fn built_component(
    package: &PackageInput,
    component: &str,
    receipt: &wamn_schema_generator::receipt::BuildReceipt,
) -> Result<SelectedComponentArtifact, CompositionError> {
    let output = wamn_schema_generator::output_root(&package.root);
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
