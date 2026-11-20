//! Package component declarations.
//!
//! This module reads compiled base digests and component declarations. It
//! supplies the coordinate used by the declaration consumer.

pub(super) mod plan;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const COMPONENT_DECLARATION_PLACEHOLDER: &str = "__PROJECT_ENV_KEY__";
/// File name of the package manifest at a package root.
pub const PACKAGE_MANIFEST: &str = "wamn.json";

/// Stable category of a component declaration failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComponentDeclarationErrorType {
    /// The manifest or template file could not be read.
    Read,
    /// The manifest or template file is not JSON.
    Parse,
    /// A manifest base dependency does not carry a package, version and digest.
    ManifestInvalid,
    /// The template does not leave its placeholders for the render to fill.
    TemplateInvalid,
}

/// Contextual failure to read authored digests or render a declaration.
#[derive(Debug)]
pub struct ComponentDeclarationError {
    type_: ComponentDeclarationErrorType,
    path: PathBuf,
    detail: Box<str>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ComponentDeclarationError {
    fn read(path: &Path, source: std::io::Error) -> Self {
        Self {
            type_: ComponentDeclarationErrorType::Read,
            path: path.to_owned(),
            detail: "".into(),
            source: Some(Box::new(source)),
        }
    }

    fn parse(path: &Path, source: serde_json::Error) -> Self {
        Self {
            type_: ComponentDeclarationErrorType::Parse,
            path: path.to_owned(),
            detail: "".into(),
            source: Some(Box::new(source)),
        }
    }

    fn invalid(
        type_: ComponentDeclarationErrorType,
        path: &Path,
        detail: impl Into<Box<str>>,
    ) -> Self {
        Self {
            type_,
            path: path.to_owned(),
            detail: detail.into(),
            source: None,
        }
    }

    /// Stable failure category.
    pub const fn error_type(&self) -> ComponentDeclarationErrorType {
        self.type_
    }

    /// The manifest or template this failure names.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Display for ComponentDeclarationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.type_ {
            ComponentDeclarationErrorType::Read => {
                write!(formatter, "read {}", self.path.display())
            }
            ComponentDeclarationErrorType::Parse => {
                write!(formatter, "parse {}", self.path.display())
            }
            ComponentDeclarationErrorType::ManifestInvalid
            | ComponentDeclarationErrorType::TemplateInvalid => {
                write!(formatter, "{} {}", self.path.display(), self.detail)
            }
        }
    }
}

impl Error for ComponentDeclarationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// The ONE authored site for a package's base component digests.
///
/// This reads `base_dependencies[*].digest`, keyed by the base's
/// `package@version` coordinate. A disposable target layers the digest of a
/// base it built over the pin (wamn-10yt.48) before it hands the digests to
/// admission. A package that declares no base dependency yields an empty map.
pub fn authored_base_digests(
    package_root: &Path,
) -> Result<BTreeMap<Box<str>, wamn_digest::Digest>, ComponentDeclarationError> {
    let manifest = wamn_codegen::package_manifest_path(package_root);
    let bytes =
        fs::read(&manifest).map_err(|source| ComponentDeclarationError::read(&manifest, source))?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|source| ComponentDeclarationError::parse(&manifest, source))?;
    let Some(dependencies) = document.get("base_dependencies").and_then(Value::as_object) else {
        return Ok(BTreeMap::new());
    };
    let mut digests = BTreeMap::new();
    for (alias, dependency) in dependencies {
        let coordinate = dependency
            .get("package")
            .and_then(Value::as_str)
            .zip(dependency.get("version").and_then(Value::as_str))
            .map(|(package, version)| format!("{package}@{version}"));
        let (Some(coordinate), Some(digest)) =
            (coordinate, dependency.get("digest").and_then(Value::as_str))
        else {
            return Err(ComponentDeclarationError::invalid(
                ComponentDeclarationErrorType::ManifestInvalid,
                &manifest,
                format!("base dependency {alias} must carry a package, version and digest"),
            ));
        };
        let digest = digest.parse().map_err(|source| {
            ComponentDeclarationError::invalid(
                ComponentDeclarationErrorType::ManifestInvalid,
                &manifest,
                format!("base dependency {alias}: {source}"),
            )
        })?;
        digests.insert(coordinate.into_boxed_str(), digest);
    }
    Ok(digests)
}

/// Read one final component declaration and supply its deployment coordinate.
pub fn render_package_declaration(
    package_root: &Path,
    component: &wamn_package::canonical_name::CanonicalName,
    project_env_key: &wamn_registry::ProjectEnvKey,
) -> Result<Value, ComponentDeclarationError> {
    let path = wamn_codegen::package_manifest_path(package_root);
    let bytes = fs::read(&path).map_err(|source| ComponentDeclarationError::read(&path, source))?;
    let mut manifest =
        wamn_component_policy::PackageManifest::from_slice(&bytes).map_err(|source| {
            ComponentDeclarationError::invalid(
                ComponentDeclarationErrorType::ManifestInvalid,
                &path,
                source.to_string(),
            )
        })?;
    let invalid = |source: &dyn Error| {
        ComponentDeclarationError::invalid(
            ComponentDeclarationErrorType::ManifestInvalid,
            &path,
            source.to_string(),
        )
    };
    let target = wamn_component_policy::ComponentPackageCoordinate::new(
        project_env_key,
        manifest.package.id.clone(),
        manifest.package.version.clone(),
    )
    .map_err(|source| invalid(&source))?;
    let declaration = package_declaration(&mut manifest, component, target)
        .map_err(|source| invalid(source.as_ref()))?;
    serde_json::to_value(declaration)
        .map_err(|source| ComponentDeclarationError::parse(&path, source))
}

/// Take a build's declaration and attach the supplied coordinate.
pub(crate) fn package_declaration<T>(
    manifest: &mut wamn_component_policy::PackageManifest,
    component: &wamn_package::canonical_name::CanonicalName,
    target: T,
) -> anyhow::Result<wamn_package::ComponentDeclaration<T>> {
    use anyhow::Context as _;
    let declaration = manifest
        .deploy
        .as_mut()
        .and_then(|deploy| deploy.declarations.remove(component))
        .with_context(|| format!("component {component} was not compiled"))?;
    Ok(wamn_package::ComponentDeclaration {
        target,
        data_change: manifest
            .components
            .get(component.as_str())
            .is_some_and(|component| component.data_change.is_some()),
        component: component.to_owned(),
        interface_version: declaration.interface_version,
        operations: declaration.operations,
        connections: declaration.connections,
    })
}

/// Resolve a palette declaration into concrete connection names for a package fixture.
pub fn render_palette_declaration(
    template: &Path,
    target: &wamn_component_policy::ComponentPackageCoordinate,
    store_alias: Option<&str>,
) -> Result<Value, ComponentDeclarationError> {
    let bytes =
        fs::read(template).map_err(|source| ComponentDeclarationError::read(template, source))?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|source| ComponentDeclarationError::parse(template, source))?;
    plan::render_palette_declaration(template, document, target, store_alias)
}

/// The platform packages a rendered declaration states it imports: the WIT
/// package of each operation interface, and the import package of each
/// connection type. Admission grants exactly these (wamn-hw3n).
pub fn declared_platform_packages(
    template: &Path,
    document: &Value,
) -> Result<Vec<String>, ComponentDeclarationError> {
    let declaration: wamn_package::ComponentDeclaration<
        wamn_component_policy::ComponentPackageCoordinate,
    > = serde_json::from_value(document.clone())
        .map_err(|source| ComponentDeclarationError::parse(template, source))?;
    let mut packages = std::collections::BTreeSet::new();
    for operation in declaration.operations.keys() {
        let (package, _) = operation.as_str().split_once('/').ok_or_else(|| {
            ComponentDeclarationError::invalid(
                ComponentDeclarationErrorType::TemplateInvalid,
                template,
                format!("names operation {operation}, which is not a WIT interface"),
            )
        })?;
        packages.insert(package.to_owned());
    }
    for connection in &declaration.connections {
        packages.insert(connection.requirement_type.import_package().to_owned());
    }
    Ok(packages.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configured palette connection requires a binding selection.
    #[test]
    fn a_palette_declaration_requires_a_binding_selection() {
        let palette = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../guests");
        let target = wamn_component_policy::ComponentPackageCoordinate {
            project_env_key: "org-a--dev--project".parse().unwrap(),
            package_id: "wamn_wms".parse().unwrap(),
            package_version: "1.0.0".parse().unwrap(),
        };
        let template = palette.join("blob-put/declaration.json.in");
        let refusal = render_palette_declaration(&template, &target, None)
            .expect_err("an unnamed store alias is refused");
        assert_eq!(
            refusal.error_type(),
            ComponentDeclarationErrorType::TemplateInvalid
        );
    }
}
