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
    pub const fn kind(&self) -> ComponentDeclarationErrorType {
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
    // An input port can name its package's generated route schema instead of
    // copying it (wamn-4omo). The template sits at
    // `publication/components/<component>.json.in` under its package root,
    // and generation writes the entries of the generated operations there.
    let package_root = template
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new(""));
    wamn_schema_generator::route_schema::read_generated_publication(
        package_root,
        wamn_schema_generator::route_schema::GENERATED_COMPONENT_OPERATIONS,
    )
    .and_then(|generated| {
        wamn_schema_generator::route_schema::merge_operations(&mut document, generated.as_ref())
    })
    .map_err(|error| invalid(format!("cannot take its generated operations: {error}")))?;
    wamn_schema_generator::route_schema::resolve_declaration(&mut document, &mut |reference| {
        wamn_schema_generator::route_schema::read_from_package(package_root, reference)
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
/// scope as placeholders. A connection whose alias is still
/// [`COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER`] is refused, because the
/// loop names no store alias for it.
pub fn render_palette_declaration(
    template: &Path,
    scope: &wamn_catalog::ComponentPackageScope,
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
        (
            "tenant-id",
            COMPONENT_DECLARATION_PLACEHOLDER,
            &scope.tenant_id,
        ),
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
    let unnamed = document
        .get("connections")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|connection| {
            connection.get("store-alias").and_then(Value::as_str)
                == Some(COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER)
        });
    if unnamed {
        return Err(invalid(format!(
            "leaves a store alias as {COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER}, and the \
             loop names no store alias for it"
        )));
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

    fn overlay_package_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/platform_fixture_overlay")
    }

    fn overlay_declaration_template() -> PathBuf {
        overlay_package_root()
            .join("publication/components")
            .join("fixture_overlay.json.in")
    }

    /// A declaration template with one dependency on the fixture base.
    ///
    /// The shipped overlay guest imports no base operation, so its template
    /// declares no dependency. The rendering rule is stated on this template,
    /// which leaves one `platform_fixture@2.0.0` digest as the placeholder.
    fn base_dependency_template(name: &str) -> PathBuf {
        let template = serde_json::json!({
            "scope": {
                "tenant-id": COMPONENT_DECLARATION_PLACEHOLDER,
                "package-id": "platform_fixture_overlay"
            },
            "component": "fixture_overlay",
            "interface-version": "0.1.0",
            "operations": {
                "platform-fixture-overlay:widget/get": {
                    "registered-operation": "platform-fixture-overlay:widget/get",
                    "dependencies": [{
                        "package": "platform_fixture",
                        "version": "2.0.0",
                        "digest": COMPONENT_DECLARATION_BASE_DIGEST_PLACEHOLDER,
                        "operation": "platform-fixture:widget/archive"
                    }],
                    "input-ports": [],
                    "output-ports": [],
                    "parameters": []
                }
            },
            "connections": []
        });
        // The template sits in a package, whose wamn.json owns the versions of
        // its operation references.
        let package = std::env::temp_dir().join(format!(
            "wamn-control-declaration-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(package.join("publication/components"))
            .expect("create the template package");
        fs::write(
            package.join(PACKAGE_MANIFEST),
            serde_json::to_vec_pretty(&serde_json::json!({
                "package": {"id": "platform_fixture_overlay", "version": "2.0.0"},
                "base_dependencies": {"base": {
                    "package": "platform_fixture",
                    "version": "2.0.0",
                    "digest": format!("sha256:{}", "0".repeat(64)),
                    "operations": ["widget.archive"]
                }}
            }))
            .expect("serialize the template package manifest"),
        )
        .expect("write the template package manifest");
        let path = package.join("publication/components/fixture_overlay.json.in");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&template).expect("serialize the template"),
        )
        .expect("write the base dependency template");
        path
    }

    /// The rendered declaration names the bytes this run built.
    ///
    /// A declaration template leaves its base dependency digest as a
    /// placeholder, and Generate never rewrites that file. Once an author edits
    /// the base package, the authored pin names bytes that no longer exist, and
    /// every consumer of the admitted fact resolves the dependency to zero
    /// components. The serving manifest's own validation is one of them, and it
    /// guards a persisted contract, so the document is made true rather than
    /// the validator made tolerant.
    #[test]
    fn a_disposable_target_renders_the_base_digest_it_built_into_the_declaration() {
        let template = base_dependency_template("disposable");
        let authored = authored_base_digests(&overlay_package_root())
            .expect("read the shipped overlay manifest pin");
        assert_eq!(
            authored.len(),
            1,
            "the shipped overlay authors exactly one base dependency digest"
        );
        let pin = authored["platform_fixture@2.0.0"].to_string();
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
            Box::<str>::from("platform_fixture@2.0.0"),
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
        let _ = fs::remove_dir_all(template.ancestors().nth(3).expect("the template package"));
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
        assert!(
            dependency_digests(&document)
                .iter()
                .all(|digest| digest == COMPONENT_DECLARATION_BASE_DIGEST_PLACEHOLDER),
            "the template leaves every dependency digest as the placeholder"
        );

        let hand_written = base_dependency_template("restated");
        let restated = fs::read_to_string(&hand_written)
            .expect("read the base dependency template")
            .replace(
                COMPONENT_DECLARATION_BASE_DIGEST_PLACEHOLDER,
                &authored["platform_fixture@2.0.0"],
            );
        fs::write(&hand_written, restated.as_bytes()).expect("write the control template");
        let refusal = render_declaration_document(&hand_written, "tenant-a", &authored)
            .expect_err("a restated digest is refused");
        let _ = fs::remove_dir_all(
            hand_written
                .ancestors()
                .nth(3)
                .expect("the template package"),
        );
        assert!(
            refusal.to_string().contains("authored once"),
            "the refusal names the single authored site: {refusal}"
        );
    }

    /// A palette template renders into the package scope and states the
    /// packages it imports. The blob-put template, whose store alias nothing
    /// names, is refused (wamn-hw3n).
    #[test]
    fn a_palette_declaration_renders_its_scope_and_imports() {
        let palette = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/platform");
        let scope = wamn_catalog::ComponentPackageScope {
            tenant_id: "tenant-a".to_owned(),
            package_id: "wamn_wms".to_owned(),
            package_version: "1.0.0".to_owned(),
        };
        let template = palette.join("execution/jsonata/declaration.json.in");
        let document = render_palette_declaration(&template, &scope).expect("the template renders");
        assert_eq!(document["scope"]["package-id"], "wamn_wms");
        assert_eq!(
            declared_platform_packages(&template, &document).expect("the declaration parses"),
            ["wamn:node"]
        );
        let refusal = render_palette_declaration(
            &palette.join("execution/blob-put/declaration.json.in"),
            &scope,
        )
        .expect_err("an unnamed store alias is refused");
        assert!(
            refusal.to_string().contains("names no store alias"),
            "the refusal names the missing alias: {refusal}"
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
