//! Package component declarations.
//!
//! A package authors each base component digest once, in `wamn.json`, and
//! leaves placeholders in `publication/components/*.json.in`. This module reads
//! the authored digests and renders a template into the declaration document
//! that admission reads.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const COMPONENT_DECLARATION_PLACEHOLDER: &str = "__TENANT_ID__";
/// Slot a declaration template leaves for the base digest `wamn.json` authors.
pub const COMPONENT_DECLARATION_BASE_DIGEST_PLACEHOLDER: &str = "__BASE_DIGEST__";
/// File name of the package manifest at a package root.
pub const PACKAGE_MANIFEST: &str = "wamn.json";

/// Stable category of a component declaration failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComponentDeclarationErrorKind {
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
    kind: ComponentDeclarationErrorKind,
    path: PathBuf,
    detail: Box<str>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ComponentDeclarationError {
    fn read(path: &Path, source: std::io::Error) -> Self {
        Self {
            kind: ComponentDeclarationErrorKind::Read,
            path: path.to_owned(),
            detail: "".into(),
            source: Some(Box::new(source)),
        }
    }

    fn parse(path: &Path, source: serde_json::Error) -> Self {
        Self {
            kind: ComponentDeclarationErrorKind::Parse,
            path: path.to_owned(),
            detail: "".into(),
            source: Some(Box::new(source)),
        }
    }

    fn invalid(
        kind: ComponentDeclarationErrorKind,
        path: &Path,
        detail: impl Into<Box<str>>,
    ) -> Self {
        Self {
            kind,
            path: path.to_owned(),
            detail: detail.into(),
            source: None,
        }
    }

    /// Stable failure category.
    pub const fn kind(&self) -> ComponentDeclarationErrorKind {
        self.kind
    }

    /// The manifest or template this failure names.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Display for ComponentDeclarationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ComponentDeclarationErrorKind::Read => {
                write!(formatter, "read {}", self.path.display())
            }
            ComponentDeclarationErrorKind::Parse => {
                write!(formatter, "parse {}", self.path.display())
            }
            ComponentDeclarationErrorKind::ManifestInvalid
            | ComponentDeclarationErrorKind::TemplateInvalid => {
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
/// # wamn-10yt.50
///
/// `publication/components/*.json.in` used to carry a second HAND-WRITTEN copy
/// of the pin `wamn.json` already carries, and nothing in the tree said the two
/// had to agree. This reads the only value an author now writes,
/// `base_dependencies[*].digest`, keyed by the `package@version` coordinate a
/// declaration names its dependency by. A package that declares no base
/// dependency yields an empty map, and its template must then declare none.
pub fn authored_base_digests(
    package_root: &Path,
) -> Result<BTreeMap<Box<str>, Box<str>>, ComponentDeclarationError> {
    let manifest = package_root.join(PACKAGE_MANIFEST);
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
                ComponentDeclarationErrorKind::ManifestInvalid,
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
/// Two placeholders, both REQUIRED: `scope.tenant-id`, which the deployment
/// fills, and every operation dependency's `digest`, which `base_digests`
/// fills. An unfilled or hand-written value is a refusal naming the file, so a
/// second authored copy of a digest cannot re-enter the template unnoticed
/// (wamn-10yt.50).
///
/// `base_digests` carries the authored pin, with the digest of a base component
/// THIS RUN built layered over it for a disposable target (wamn-10yt.48).
/// Generate never rewrites the template, so once an author edits the base
/// package the pin names bytes that no longer exist; every consumer of the
/// admitted fact then resolves the dependency to zero components, including the
/// serving manifest's own validation, which guards a persisted contract and
/// must stay strict. So the DOCUMENT is made true rather than the validator
/// made tolerant.
pub fn render_declaration_document(
    template: &Path,
    tenant: &str,
    base_digests: &BTreeMap<Box<str>, Box<str>>,
) -> Result<Value, ComponentDeclarationError> {
    let bytes =
        fs::read(template).map_err(|source| ComponentDeclarationError::read(template, source))?;
    let mut document: Value = serde_json::from_slice(&bytes)
        .map_err(|source| ComponentDeclarationError::parse(template, source))?;
    let invalid = |detail: String| {
        ComponentDeclarationError::invalid(
            ComponentDeclarationErrorKind::TemplateInvalid,
            template,
            detail,
        )
    };
    let slot = document
        .pointer_mut("/scope/tenant-id")
        .ok_or_else(|| invalid("has no scope.tenant-id".to_owned()))?;
    if slot.as_str() != Some(COMPONENT_DECLARATION_PLACEHOLDER) {
        return Err(invalid(
            "must leave scope.tenant-id as the deployment placeholder".to_owned(),
        ));
    }
    *slot = Value::String(tenant.to_owned());
    if let Some(operations) = document
        .get_mut("operations")
        .and_then(Value::as_object_mut)
    {
        for operation in operations.values_mut() {
            let Some(dependencies) = operation
                .get_mut("dependencies")
                .and_then(Value::as_array_mut)
            else {
                continue;
            };
            for dependency in dependencies {
                let coordinate = dependency
                    .get("package")
                    .and_then(Value::as_str)
                    .zip(dependency.get("version").and_then(Value::as_str))
                    .map(|(package, version)| format!("{package}@{version}"))
                    .ok_or_else(|| {
                        invalid(
                            "declares an operation dependency without a package and version"
                                .to_owned(),
                        )
                    })?;
                if dependency.get("digest").and_then(Value::as_str)
                    != Some(COMPONENT_DECLARATION_BASE_DIGEST_PLACEHOLDER)
                {
                    return Err(invalid(format!(
                        "must leave the {coordinate} dependency digest as \
                         {COMPONENT_DECLARATION_BASE_DIGEST_PLACEHOLDER}; the digest is \
                         authored once, in {PACKAGE_MANIFEST}"
                    )));
                }
                let digest = base_digests.get(coordinate.as_str()).ok_or_else(|| {
                    invalid(format!(
                        "depends on {coordinate}, which no {PACKAGE_MANIFEST} \
                         base_dependencies entry pins"
                    ))
                })?;
                dependency["digest"] = Value::String(digest.to_string());
            }
        }
    }
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay_package_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/client_acme_receiving")
    }

    fn overlay_declaration_template() -> PathBuf {
        overlay_package_root()
            .join("publication/components")
            .join("client_acme_receiving.json.in")
    }

    /// The rendered declaration names the bytes this run built.
    ///
    /// The shipped overlay template leaves its base dependency digest as a
    /// placeholder, and Generate never rewrites that file. Once an author edits
    /// the base package, the authored pin names bytes that no longer exist, and
    /// every consumer of the admitted fact resolves the dependency to zero
    /// components. The serving manifest's own validation is one of them, and it
    /// guards a persisted contract, so the document is made true rather than
    /// the validator made tolerant.
    #[test]
    fn a_disposable_target_renders_the_base_digest_it_built_into_the_declaration() {
        let template = overlay_declaration_template();
        let authored = authored_base_digests(&overlay_package_root())
            .expect("read the shipped overlay manifest pin");
        assert_eq!(
            authored.len(),
            1,
            "the shipped overlay authors exactly one base dependency digest"
        );
        let pin = authored["wamn_receiving@1.0.0"].to_string();
        let built = format!("sha256:{}", "7".repeat(64));
        assert_ne!(pin, built);

        let durable = render_declaration_document(&template, "tenant-a", &authored)
            .expect("render for a durable target");
        assert_eq!(
            dependency_digests(&durable),
            vec![pin],
            "a durable target admits the pin exactly as the manifest authors it"
        );

        let mut base_digests = authored.clone();
        base_digests.insert(
            Box::<str>::from("wamn_receiving@1.0.0"),
            Box::<str>::from(built.as_str()),
        );
        let disposable = render_declaration_document(&template, "tenant-a", &base_digests)
            .expect("render for a disposable target");
        assert_eq!(
            dependency_digests(&disposable),
            vec![built.clone()],
            "a disposable target names the base it just built"
        );
        assert_eq!(
            disposable
                .pointer("/scope/tenant-id")
                .and_then(Value::as_str),
            Some("tenant-a"),
            "the tenant placeholder is still filled"
        );
    }

    /// The digest is authored ONCE, and no second copy can hide in the tree.
    ///
    /// wamn-10yt.50: the template used to carry its own hand-written copy of
    /// the manifest pin. A rendered declaration is now unsatisfiable unless the
    /// template leaves the slot empty, so a reintroduced literal is refused at
    /// the render rather than drifting silently.
    #[test]
    fn the_shipped_template_carries_no_second_copy_of_the_base_digest() {
        let template = overlay_declaration_template();
        let bytes = fs::read(&template).expect("read the shipped declaration template");
        let authored = authored_base_digests(&overlay_package_root())
            .expect("read the shipped overlay manifest pin");
        for digest in authored.values() {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(digest.as_ref()),
                "{} must not restate the authored digest {digest}",
                template.display()
            );
        }

        let document: Value =
            serde_json::from_slice(&bytes).expect("parse the shipped declaration template");
        assert_eq!(
            dependency_digests(&document),
            vec![COMPONENT_DECLARATION_BASE_DIGEST_PLACEHOLDER.to_owned()],
            "the template leaves every dependency digest as the placeholder"
        );

        let restated = String::from_utf8_lossy(&bytes).replace(
            COMPONENT_DECLARATION_BASE_DIGEST_PLACEHOLDER,
            &authored["wamn_receiving@1.0.0"],
        );
        let hand_written = std::env::temp_dir().join(format!(
            "wamn-control-declaration-{}.json.in",
            std::process::id()
        ));
        fs::write(&hand_written, restated.as_bytes()).expect("write the control template");
        let refusal = render_declaration_document(&hand_written, "tenant-a", &authored)
            .expect_err("a restated digest is refused");
        let _ = fs::remove_file(&hand_written);
        assert!(
            refusal.to_string().contains("authored once"),
            "the refusal names the single authored site: {refusal}"
        );
    }

    fn dependency_digests(document: &Value) -> Vec<String> {
        document
            .get("operations")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|operations| operations.values())
            .filter_map(|operation| operation.get("dependencies"))
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(|dependency| dependency.get("digest"))
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }
}
