//! Package component declarations.
//!
//! This module reads compiled base digests and component declarations. It
//! supplies the deployment coordinate that admission reads.

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
) -> Result<BTreeMap<Box<str>, Box<str>>, ComponentDeclarationError> {
    let manifest = wamn_compiler::package_manifest_path(package_root);
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
        digests.insert(coordinate.into_boxed_str(), Box::<str>::from(digest));
    }
    Ok(digests)
}

/// Read one final component declaration and supply its deployment coordinate.
pub fn render_package_declaration(
    package_root: &Path,
    component: &str,
    project_env_key: &str,
) -> Result<Value, ComponentDeclarationError> {
    let invalid = |source: String| {
        ComponentDeclarationError::invalid(
            ComponentDeclarationErrorType::ManifestInvalid,
            package_root,
            source,
        )
    };
    let mut declarations = wamn_compiler::publication::read_components(package_root)
        .map_err(|source| invalid(format!("read compiled component declarations: {source:#}")))?;
    let declaration = declarations
        .remove(component)
        .ok_or_else(|| invalid(format!("component {component} was not compiled")))?;
    let target = wamn_catalog::ComponentPackageCoordinate::new(
        project_env_key,
        declaration.target.package_id(),
        declaration.target.package_version(),
    )
    .map_err(|source| invalid(source.to_string()))?;
    let document = wamn_catalog::ComponentDeclaration {
        target,
        component: declaration.component,
        interface_version: declaration.interface_version,
        operations: declaration.operations,
        connections: declaration.connections,
    };
    serde_json::to_value(document)
        .map_err(|source| ComponentDeclarationError::parse(package_root, source))
}

/// Slot a palette declaration leaves for the store alias of its connection.
pub const COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER: &str = "__STORE_ALIAS__";

/// Renders one palette declaration (`apps/platform/*/*/declaration.json.in`)
/// into the document admission reads, for the package `target` it is admitted
/// into (wamn-hw3n).
///
/// A palette component belongs to no package, so its template leaves the whole
/// target as placeholders. A connection whose alias is
/// [`COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER`] takes `store_alias`, the
/// name the binding selection gives the store, as the operator passes it to
/// the environment document in the cluster.
pub fn render_palette_declaration(
    template: &Path,
    target: &wamn_catalog::ComponentPackageCoordinate,
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
    let declaration: wamn_catalog::ComponentDeclaration = serde_json::from_value(document.clone())
        .map_err(|source| ComponentDeclarationError::parse(template, source))?;
    let mut packages = std::collections::BTreeSet::new();
    for operation in declaration.operations.keys() {
        let (package, _) = operation.split_once('/').ok_or_else(|| {
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

    /// A palette template renders into the package target with the store alias
    /// of its binding selection, and states the packages it imports. An
    /// unnamed alias is refused (wamn-hw3n).
    #[test]
    fn a_palette_declaration_renders_its_target_alias_and_imports() {
        let palette = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/platform");
        let target = wamn_catalog::ComponentPackageCoordinate {
            project_env_key: "org-a".to_owned(),
            package_id: "wamn_wms".to_owned(),
            package_version: "1.0.0".to_owned(),
        };
        let template = palette.join("execution/blob-put/declaration.json.in");
        let document = render_palette_declaration(&template, &target, Some("labels"))
            .expect("the template renders");
        assert_eq!(document["target"]["package-id"], "wamn_wms");
        assert_eq!(document["connections"][0]["store-alias"], "labels");
        assert_eq!(
            declared_platform_packages(&template, &document).expect("the declaration parses"),
            ["wamn:node", "wasmcloud:blobstore"]
        );
        let refusal = render_palette_declaration(&template, &target, None)
            .expect_err("an unnamed store alias is refused");
        assert!(
            refusal.to_string().contains("no binding selection"),
            "the refusal names the missing selection: {refusal}"
        );
    }
}
