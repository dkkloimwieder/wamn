//! Package component declarations.
//!
//! A package authors each base component digest once, in `wamn.json`. This
//! module reads the authored digests, which admission checks a composed base
//! against, and renders a `publication/components/*.json.in` template into the
//! declaration document that admission reads.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const COMPONENT_DECLARATION_PLACEHOLDER: &str = "__ENV_KEY__";
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
    let manifest = wamn_schema_generator::package_manifest_path(package_root);
    // A hand-written source manifest does not consume package build output.
    if manifest.parent() == Some(wamn_schema_generator::output_root(package_root).as_path()) {
        wamn_schema_generator::receipt::check_current(package_root).map_err(|source| {
            ComponentDeclarationError {
                type_: ComponentDeclarationErrorType::Read,
                path: package_root.to_owned(),
                detail: "".into(),
                source: Some(source.into()),
            }
        })?;
    }
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

/// Renders one authored component declaration into the document admission reads.
///
/// One placeholder, REQUIRED: `scope.env_key`, which the deployment fills.
/// An unfilled or hand-written value is a refusal naming the file.
pub fn render_declaration_document(
    template: &Path,
    env_key: &str,
) -> Result<Value, ComponentDeclarationError> {
    let bytes =
        fs::read(template).map_err(|source| ComponentDeclarationError::read(template, source))?;
    let mut document: Value = serde_json::from_slice(&bytes)
        .map_err(|source| ComponentDeclarationError::parse(template, source))?;
    let invalid = |detail: String| {
        ComponentDeclarationError::invalid(
            ComponentDeclarationErrorType::TemplateInvalid,
            template,
            detail,
        )
    };
    // The template names each operation by reference, and the version is
    // authored once, in the wamn.json of the package that holds the template.
    let (_, owners) = wamn_schema_generator::operation_reference::package_owners_of(template)
        .map_err(|error| invalid(error.context().to_owned()))?;
    wamn_schema_generator::operation_reference::resolve_declaration_document(
        &mut document,
        &owners,
    )
    .map_err(|error| invalid(error.context().to_owned()))?;
    let slot = document
        .pointer_mut("/scope/env_key")
        .ok_or_else(|| invalid("has no scope.env_key".to_owned()))?;
    if slot.as_str() != Some(COMPONENT_DECLARATION_PLACEHOLDER) {
        return Err(invalid(
            "must leave scope.env_key as the deployment placeholder".to_owned(),
        ));
    }
    *slot = Value::String(env_key.to_owned());
    // An input port can name its package's generated route schema instead of
    // copying it (wamn-4omo). The template sits at
    // `publication/components/<component>.json.in` under its package root,
    // and generation writes the entries of the generated operations there.
    let package_root = template
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new(""));
    let files = wamn_schema_generator::route_schema::PackageFiles::open(package_root)
        .map_err(|error| invalid(format!("cannot read its build: {error:#}")))?;
    files
        .publication(wamn_schema_generator::route_schema::GENERATED_COMPONENT_OPERATIONS)
        .and_then(|generated| {
            wamn_schema_generator::route_schema::merge_operations(&mut document, generated.as_ref())
        })
        .map_err(|error| invalid(format!("cannot take its generated operations: {error}")))?;
    wamn_schema_generator::route_schema::resolve_declaration(&mut document, &mut |reference| {
        files.schema(reference)
    })
    .map_err(|error| {
        invalid(format!(
            "names an input port schema it cannot read: {error}"
        ))
    })?;
    Ok(document)
}

/// Slot a palette declaration leaves for the store alias of its connection.
pub const COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER: &str = "__STORE_ALIAS__";

/// Renders one palette declaration (`apps/platform/*/*/declaration.json.in`)
/// into the document admission reads, for the package `scope` it is admitted
/// into (wamn-hw3n).
///
/// A palette component belongs to no package, so its template leaves the whole
/// scope as placeholders. A connection whose alias is
/// [`COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER`] takes `store_alias`, the
/// name the binding selection gives the store, as the operator passes it to
/// `bind-connection --store-alias` in the cluster.
pub fn render_palette_declaration(
    template: &Path,
    scope: &wamn_catalog::ComponentPackageScope,
    store_alias: Option<&str>,
) -> Result<Value, ComponentDeclarationError> {
    let bytes =
        fs::read(template).map_err(|source| ComponentDeclarationError::read(template, source))?;
    let mut document: Value = serde_json::from_slice(&bytes)
        .map_err(|source| ComponentDeclarationError::parse(template, source))?;
    let invalid = |detail: String| {
        ComponentDeclarationError::invalid(
            ComponentDeclarationErrorType::TemplateInvalid,
            template,
            detail,
        )
    };
    for (field, placeholder, value) in [
        ("env_key", COMPONENT_DECLARATION_PLACEHOLDER, &scope.env_key),
        ("package-id", "__PACKAGE_ID__", &scope.package_id),
        (
            "package-version",
            "__PACKAGE_VERSION__",
            &scope.package_version,
        ),
    ] {
        let slot = document
            .pointer_mut(&format!("/scope/{field}"))
            .ok_or_else(|| invalid(format!("has no scope.{field}")))?;
        if slot.as_str() != Some(placeholder) {
            return Err(invalid(format!(
                "must leave scope.{field} as the placeholder {placeholder}"
            )));
        }
        *slot = Value::String(value.clone());
    }
    for connection in document
        .get_mut("connections")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        let Some(alias) = connection.get_mut("store-alias") else {
            continue;
        };
        if alias.as_str() != Some(COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER) {
            continue;
        }
        let store_alias = store_alias.ok_or_else(|| {
            invalid(format!(
                "leaves a store alias as {COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER}, and no \
                 binding selection names a store alias for it"
            ))
        })?;
        *alias = Value::String(store_alias.to_owned());
    }
    Ok(document)
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

    /// A palette template renders into the package scope with the store alias
    /// of its binding selection, and states the packages it imports. An
    /// unnamed alias is refused (wamn-hw3n).
    #[test]
    fn a_palette_declaration_renders_its_scope_alias_and_imports() {
        let palette = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/platform");
        let scope = wamn_catalog::ComponentPackageScope {
            env_key: "tenant-a".to_owned(),
            package_id: "wamn_wms".to_owned(),
            package_version: "1.0.0".to_owned(),
        };
        let template = palette.join("execution/blob-put/declaration.json.in");
        let document = render_palette_declaration(&template, &scope, Some("labels"))
            .expect("the template renders");
        assert_eq!(document["scope"]["package-id"], "wamn_wms");
        assert_eq!(document["connections"][0]["store-alias"], "labels");
        assert_eq!(
            declared_platform_packages(&template, &document).expect("the declaration parses"),
            ["wamn:node", "wasmcloud:blobstore"]
        );
        let refusal = render_palette_declaration(&template, &scope, None)
            .expect_err("an unnamed store alias is refused");
        assert!(
            refusal.to_string().contains("no binding selection"),
            "the refusal names the missing selection: {refusal}"
        );
    }
}
