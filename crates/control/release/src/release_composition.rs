//! The components and palette components of a release, derived from
//! the package manifests and the `build.json` of each package, whose
//! `outputs.components` names the bytes `wamn build` compiled.
//!
//! The dev loop calls this derivation. No second file lists the components
//! of a release.

pub(super) mod plan;

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use anyhow::anyhow;
use wamn_catalog::manifest::PackageManifest;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionErrorType {
    /// The inputs do not describe a release.
    Invalid,
    /// A file could not be read or parsed.
    Owner,
}

/// A refused derivation, with the operation that refused.
#[derive(Debug)]
pub struct CompositionError {
    pub type_: CompositionErrorType,
    pub operation: &'static str,
    pub detail: Box<str>,
    pub source: Option<anyhow::Error>,
}

impl CompositionError {
    fn invalid(operation: &'static str, detail: impl Into<Box<str>>) -> Self {
        Self {
            type_: CompositionErrorType::Invalid,
            operation,
            detail: detail.into(),
            source: None,
        }
    }

    fn owner(operation: &'static str, source: anyhow::Error) -> Self {
        Self {
            type_: CompositionErrorType::Owner,
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

/// A palette component admitted into the target of the package whose wiring
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
        let component = plan::selected_component(package)?;
        wamn_schema_generator::build_record::check_current(&package.root)
            .map_err(|source| CompositionError::owner("read the package build", source))?;
        let build_record = wamn_schema_generator::build_record::read(
            &wamn_schema_generator::output_root(&package.root),
        )
        .map_err(|source| CompositionError::owner("read the package build", source))?;
        selected.push(built_component(package, component, &build_record)?);
    }
    Ok(selected)
}

/// The palette components selected by the package build.
pub fn select_palette_artifacts(
    packages: &[PackageInput],
) -> Result<Vec<SelectedPaletteArtifact>, CompositionError> {
    let mut selected = Vec::new();
    for package in packages {
        wamn_schema_generator::build_record::check_current(&package.root)
            .map_err(|source| CompositionError::owner("read the package build", source))?;
        let build_record = wamn_schema_generator::build_record::read(
            &wamn_schema_generator::output_root(&package.root),
        )
        .map_err(|source| CompositionError::owner("read the package build", source))?;
        for entry in &build_record.outputs.components {
            if package.manifest.components.contains_key(&entry.name) {
                continue;
            }
            let (declaration, _) = wamn_package_control::package_artifact::platform_template(
                &package.root,
                &entry.name,
            )
            .map_err(|source| CompositionError::owner("find palette declaration", source))?;
            selected.push(SelectedPaletteArtifact {
                artifact: built_component(package, &entry.name, &build_record)?,
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
    build_record: &wamn_schema_generator::build_record::BuildRecord,
) -> Result<SelectedComponentArtifact, CompositionError> {
    let (path, entry) = plan::component_entry(package, component, build_record)?;
    let bytes = fs::read(&path).map_err(|source| {
        CompositionError::owner(
            "read built component",
            anyhow!(source).context(format!("read {}", path.display())),
        )
    })?;
    plan::built_component(package, component, path, entry, &bytes)
}
