//! Pure construction of the native effect-provider recipe revision.
//!
//! This module is shared by `build.rs` and the repository conformance proof.
//! It is deliberately not linked into the runtime library: production uses only
//! the build-generated literal and never recomputes identity from a checkout.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub const DOMAIN: &str = "wamn.effect-provider-revision.v1";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionError {
    message: String,
}

impl RevisionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for RevisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RevisionError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub resolution: ResolutionRecipe,
    pub local_packages: Vec<LocalPackage>,
    pub external_packages: Vec<ExternalPackage>,
    pub workspace_inputs: Vec<SemanticRecord>,
    pub executor_composition: SourceRoot,
    pub assets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionRecipe {
    pub cargo_version: String,
    pub locked: bool,
    pub offline: bool,
    pub package_scoped: bool,
    pub root_package: String,
    pub projection_root: String,
    pub target: String,
    pub edge_kinds: Vec<String>,
    pub feature_unification: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalPackage {
    pub name: String,
    pub version: String,
    pub root: String,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalPackage {
    pub name: String,
    pub version: String,
    pub source_kind: ExternalSourceKind,
    pub source: String,
    pub revision: Option<String>,
    pub checksum: Option<String>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalSourceKind {
    Registry,
    Git,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRecord {
    pub tag: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRoot {
    pub name: String,
    pub version: String,
    pub root: String,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionInput {
    pub tag: String,
    pub value: Vec<u8>,
}

pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, RevisionError> {
    let manifest: Manifest = serde_json::from_slice(bytes)
        .map_err(|error| RevisionError::new(format!("parse provider manifest: {error}")))?;
    validate_manifest(&manifest)?;
    let canonical = canonical_manifest_bytes(&manifest)?;
    if canonical != bytes {
        return Err(RevisionError::new(
            "provider manifest is not canonical pretty JSON",
        ));
    }
    Ok(manifest)
}

pub fn canonical_manifest_bytes(manifest: &Manifest) -> Result<Vec<u8>, RevisionError> {
    validate_manifest(manifest)?;
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| RevisionError::new(format!("encode provider manifest: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn validate_manifest(manifest: &Manifest) -> Result<(), RevisionError> {
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(RevisionError::new(format!(
            "provider manifest schema must be {SCHEMA_VERSION}"
        )));
    }
    let recipe = &manifest.resolution;
    if recipe.cargo_version != "1.97.0"
        || !recipe.locked
        || !recipe.offline
        || !recipe.package_scoped
        || recipe.root_package != "wamn-executor"
        || recipe.projection_root != "wamn-execution-host"
        || recipe.target != "all"
        || recipe.edge_kinds != ["normal"]
        || recipe.feature_unification != "union-per-full-package-identity-across-resolver-units"
    {
        return Err(RevisionError::new(
            "provider manifest resolution recipe is not canonical v1",
        ));
    }

    require_sorted_unique_by(
        &manifest.local_packages,
        |package| format!("{}@{}", package.name, package.version),
        "local packages",
    )?;
    if manifest.local_packages.is_empty() {
        return Err(RevisionError::new(
            "provider manifest has no local packages",
        ));
    }
    let mut governed_roots = BTreeSet::new();
    for package in &manifest.local_packages {
        require_nonempty(&package.name, "local package name")?;
        require_nonempty(&package.version, "local package version")?;
        validate_relative_path(&package.root)?;
        if !governed_roots.insert(package.root.as_str()) {
            return Err(RevisionError::new(format!(
                "duplicate governed local root: {}",
                package.root
            )));
        }
        require_sorted_unique(&package.features, "local package features")?;
    }
    reject_nested_roots(&governed_roots)?;

    require_sorted_unique_by(
        &manifest.external_packages,
        external_identity,
        "external packages",
    )?;
    for package in &manifest.external_packages {
        validate_external(package)?;
    }

    require_sorted_unique_by(
        &manifest.workspace_inputs,
        |record| record.tag.clone(),
        "workspace inputs",
    )?;
    for record in &manifest.workspace_inputs {
        validate_tag(&record.tag)?;
    }

    let composition = &manifest.executor_composition;
    if composition.name != "wamn-executor"
        || composition.root != "services/executor"
        || composition.version != "0.1.0"
    {
        return Err(RevisionError::new(
            "executor composition root is not canonical v1",
        ));
    }
    validate_relative_path(&composition.root)?;
    if governed_roots.iter().any(|root| {
        composition.root.as_str() == *root
            || path_is_within(&composition.root, root)
            || path_is_within(root, &composition.root)
    }) {
        return Err(RevisionError::new(format!(
            "executor composition overlaps a local root: {}",
            composition.root
        )));
    }
    require_sorted_unique(&composition.features, "executor features")?;

    require_sorted_unique(&manifest.assets, "asset paths")?;
    for asset in &manifest.assets {
        validate_relative_path(asset)?;
        if governed_roots
            .iter()
            .any(|root| path_is_governed_source(asset, root, true, true))
            || path_is_governed_source(asset, &composition.root, false, false)
        {
            return Err(RevisionError::new(format!(
                "asset path is already governed by a source root: {asset}"
            )));
        }
    }
    Ok(())
}

pub fn collect_revision_inputs(
    repository_root: &Path,
    manifest: &Manifest,
) -> Result<Vec<RevisionInput>, RevisionError> {
    validate_manifest(manifest)?;
    let mut inputs = Vec::new();
    push_text(&mut inputs, "manifest/schema-version", "1");
    push_text(
        &mut inputs,
        "recipe/cargo-version",
        &manifest.resolution.cargo_version,
    );
    push_text(
        &mut inputs,
        "recipe/locked",
        &manifest.resolution.locked.to_string(),
    );
    push_text(
        &mut inputs,
        "recipe/offline",
        &manifest.resolution.offline.to_string(),
    );
    push_text(
        &mut inputs,
        "recipe/package-scoped",
        &manifest.resolution.package_scoped.to_string(),
    );
    push_text(
        &mut inputs,
        "recipe/root-package",
        &manifest.resolution.root_package,
    );
    push_text(
        &mut inputs,
        "recipe/projection-root",
        &manifest.resolution.projection_root,
    );
    push_text(&mut inputs, "recipe/target", &manifest.resolution.target);
    for edge_kind in &manifest.resolution.edge_kinds {
        push_text(
            &mut inputs,
            &format!("recipe/edge-kind/{edge_kind}"),
            edge_kind,
        );
    }
    push_text(
        &mut inputs,
        "recipe/feature-unification",
        &manifest.resolution.feature_unification,
    );

    for package in &manifest.local_packages {
        let identity = format!("{}@{}", package.name, package.version);
        push_text(
            &mut inputs,
            &format!("local/{identity}/root"),
            &package.root,
        );
        for feature in &package.features {
            push_text(
                &mut inputs,
                &format!("local/{identity}/feature/{feature}"),
                feature,
            );
        }
        collect_source_root(repository_root, &package.root, true, &mut inputs)?;
    }

    for package in &manifest.external_packages {
        collect_external_inputs(package, &mut inputs);
    }
    for record in &manifest.workspace_inputs {
        push_text(
            &mut inputs,
            &format!("workspace/{}", record.tag),
            &record.value,
        );
    }

    let composition = &manifest.executor_composition;
    let identity = format!("{}@{}", composition.name, composition.version);
    push_text(
        &mut inputs,
        &format!("composition/{identity}/root"),
        &composition.root,
    );
    for feature in &composition.features {
        push_text(
            &mut inputs,
            &format!("composition/{identity}/feature/{feature}"),
            feature,
        );
    }
    collect_source_root(repository_root, &composition.root, false, &mut inputs)?;

    for asset in &manifest.assets {
        collect_file(repository_root, asset, "asset", &mut inputs)?;
    }
    inputs.sort_by(|left, right| left.tag.as_bytes().cmp(right.tag.as_bytes()));
    reject_duplicate_input_tags(&inputs)?;
    Ok(inputs)
}

pub fn preimage(inputs: &[RevisionInput]) -> Result<Vec<u8>, RevisionError> {
    let mut ordered = inputs.to_vec();
    ordered.sort_by(|left, right| left.tag.as_bytes().cmp(right.tag.as_bytes()));
    reject_duplicate_input_tags(&ordered)?;
    let mut bytes = Vec::new();
    frame(&mut bytes, DOMAIN.as_bytes());
    for input in ordered {
        validate_tag(&input.tag)?;
        frame(&mut bytes, input.tag.as_bytes());
        frame(&mut bytes, &input.value);
    }
    Ok(bytes)
}

pub fn revision(inputs: &[RevisionInput]) -> Result<String, RevisionError> {
    let digest = Sha256::digest(preimage(inputs)?);
    let mut literal = String::with_capacity(71);
    literal.push_str("sha256:");
    for byte in digest {
        use fmt::Write as _;
        write!(literal, "{byte:02x}")
            .map_err(|error| RevisionError::new(format!("format revision: {error}")))?;
    }
    Ok(literal)
}

fn collect_source_root(
    repository_root: &Path,
    relative_root: &str,
    include_build_script: bool,
    inputs: &mut Vec<RevisionInput>,
) -> Result<(), RevisionError> {
    let root = repository_root.join(relative_root);
    reject_symlink_components(repository_root, relative_root)?;
    require_directory(&root, relative_root)?;
    collect_file(
        repository_root,
        &format!("{relative_root}/Cargo.toml"),
        "file",
        inputs,
    )?;
    if include_build_script {
        let build_script = root.join("build.rs");
        match fs::symlink_metadata(&build_script) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(RevisionError::new(format!(
                    "governed path is a symlink: {}",
                    build_script.display()
                )));
            }
            Ok(metadata) if metadata.is_file() => collect_file(
                repository_root,
                &format!("{relative_root}/build.rs"),
                "file",
                inputs,
            )?,
            Ok(_) => {
                return Err(RevisionError::new(format!(
                    "governed build.rs is not a regular file: {}",
                    build_script.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(RevisionError::new(format!(
                    "inspect {}: {error}",
                    build_script.display()
                )));
            }
        }
    }
    for directory in ["src", "wit"] {
        if !include_build_script && directory == "wit" {
            continue;
        }
        let path = root.join(directory);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(RevisionError::new(format!(
                    "governed path is a symlink: {}",
                    path.display()
                )));
            }
            Ok(metadata) if metadata.is_dir() => {
                collect_directory(repository_root, &path, inputs)?;
            }
            Ok(_) => {
                return Err(RevisionError::new(format!(
                    "governed source root is not a directory: {}",
                    path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(RevisionError::new(format!(
                    "inspect {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn collect_directory(
    repository_root: &Path,
    directory: &Path,
    inputs: &mut Vec<RevisionInput>,
) -> Result<(), RevisionError> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| RevisionError::new(format!("read {}: {error}", directory.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| RevisionError::new(format!("read {}: {error}", directory.display())))?;
    entries.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });
    for entry in entries {
        let entry_path = entry.path();
        let name = entry.file_name();
        if name.to_str().is_none() {
            return Err(RevisionError::new(format!(
                "governed path is not UTF-8: {}",
                entry_path.display()
            )));
        }
        let file_type = entry.file_type().map_err(|error| {
            RevisionError::new(format!("inspect {}: {error}", entry_path.display()))
        })?;
        if file_type.is_symlink() {
            return Err(RevisionError::new(format!(
                "governed path is a symlink: {}",
                entry_path.display()
            )));
        }
        if file_type.is_dir() {
            collect_directory(repository_root, &entry_path, inputs)?;
        } else if file_type.is_file() {
            let relative = entry_path.strip_prefix(repository_root).map_err(|_| {
                RevisionError::new(format!(
                    "governed file escaped repository: {}",
                    entry_path.display()
                ))
            })?;
            let relative = normalized_path(relative)?;
            collect_file(repository_root, &relative, "file", inputs)?;
        } else {
            return Err(RevisionError::new(format!(
                "governed path is not a regular file: {}",
                entry_path.display()
            )));
        }
    }
    Ok(())
}

fn collect_file(
    repository_root: &Path,
    relative: &str,
    kind: &str,
    inputs: &mut Vec<RevisionInput>,
) -> Result<(), RevisionError> {
    validate_relative_path(relative)?;
    reject_symlink_components(repository_root, relative)?;
    let path = repository_root.join(relative);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| RevisionError::new(format!("inspect {}: {error}", path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RevisionError::new(format!(
            "governed path is not a regular file: {}",
            path.display()
        )));
    }
    let value = fs::read(&path)
        .map_err(|error| RevisionError::new(format!("read {}: {error}", path.display())))?;
    inputs.push(RevisionInput {
        tag: format!("{kind}/{relative}"),
        value,
    });
    Ok(())
}

fn collect_external_inputs(package: &ExternalPackage, inputs: &mut Vec<RevisionInput>) {
    let identity_digest = Sha256::digest(
        format!(
            "{}\0{}\0{:?}\0{}\0{}",
            package.name,
            package.version,
            package.source_kind,
            package.source,
            package.revision.as_deref().unwrap_or_default()
        )
        .as_bytes(),
    );
    let identity = format!(
        "{}@{}-{}",
        package.name,
        package.version,
        hex_lower(&identity_digest)
    );
    let prefix = format!("external/{identity}");
    push_text(inputs, &format!("{prefix}/name"), &package.name);
    push_text(inputs, &format!("{prefix}/version"), &package.version);
    push_text(
        inputs,
        &format!("{prefix}/source-kind"),
        match package.source_kind {
            ExternalSourceKind::Registry => "registry",
            ExternalSourceKind::Git => "git",
        },
    );
    push_text(inputs, &format!("{prefix}/source"), &package.source);
    push_text(
        inputs,
        &format!("{prefix}/revision"),
        package.revision.as_deref().unwrap_or_default(),
    );
    push_text(
        inputs,
        &format!("{prefix}/checksum"),
        package.checksum.as_deref().unwrap_or_default(),
    );
    for feature in &package.features {
        push_text(inputs, &format!("{prefix}/feature/{feature}"), feature);
    }
}

fn validate_external(package: &ExternalPackage) -> Result<(), RevisionError> {
    require_nonempty(&package.name, "external package name")?;
    require_nonempty(&package.version, "external package version")?;
    require_sorted_unique(&package.features, "external package features")?;
    match package.source_kind {
        ExternalSourceKind::Registry => {
            if !package.source.starts_with("registry+") || package.revision.is_some() {
                return Err(RevisionError::new(format!(
                    "registry package {} has noncanonical source/revision",
                    package.name
                )));
            }
            let checksum = package.checksum.as_deref().ok_or_else(|| {
                RevisionError::new(format!("registry package {} lacks checksum", package.name))
            })?;
            require_lower_hex(checksum, 64, "registry checksum")?;
        }
        ExternalSourceKind::Git => {
            if package.source.starts_with("git+")
                || package.source.contains('?')
                || package.source.contains('#')
                || package.checksum.is_some()
            {
                return Err(RevisionError::new(format!(
                    "git package {} has noncanonical source/checksum",
                    package.name
                )));
            }
            if !package.source.starts_with("https://") {
                return Err(RevisionError::new(format!(
                    "git package {} must use a canonical HTTPS URL",
                    package.name
                )));
            }
            require_lower_hex(
                package.revision.as_deref().unwrap_or_default(),
                40,
                "git revision",
            )?;
        }
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), RevisionError> {
    require_nonempty(path, "path")?;
    let bytes = path.as_bytes();
    if path.contains('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(RevisionError::new(format!(
            "path is not lexically slash-normalized: {path}"
        )));
    }
    let value = Path::new(path);
    if value.is_absolute()
        || value.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::CurDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(RevisionError::new(format!(
            "path is not normalized repository-relative UTF-8: {path}"
        )));
    }
    Ok(())
}

fn reject_nested_roots(roots: &BTreeSet<&str>) -> Result<(), RevisionError> {
    for left in roots {
        for right in roots {
            if left != right && path_is_within(left, right) {
                return Err(RevisionError::new(format!(
                    "governed local roots overlap: {left} is within {right}"
                )));
            }
        }
    }
    Ok(())
}

fn path_is_within(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

fn path_is_governed_source(
    path: &str,
    root: &str,
    include_build_script: bool,
    include_wit: bool,
) -> bool {
    path == format!("{root}/Cargo.toml")
        || (include_build_script && path == format!("{root}/build.rs"))
        || path_is_within(path, &format!("{root}/src"))
        || (include_wit && path_is_within(path, &format!("{root}/wit")))
}

fn normalized_path(path: &Path) -> Result<String, RevisionError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(RevisionError::new(format!(
                "governed path is not repository-relative: {}",
                path.display()
            )));
        };
        parts.push(part.to_str().ok_or_else(|| {
            RevisionError::new(format!("governed path is not UTF-8: {}", path.display()))
        })?);
    }
    let value = parts.join("/");
    validate_relative_path(&value)?;
    Ok(value)
}

fn validate_tag(tag: &str) -> Result<(), RevisionError> {
    require_nonempty(tag, "record tag")?;
    if tag.contains('\0') || tag.contains('\\') {
        return Err(RevisionError::new(format!(
            "record tag is not canonical: {tag:?}"
        )));
    }
    if tag
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(RevisionError::new(format!(
            "record tag contains path traversal: {tag:?}"
        )));
    }
    Ok(())
}

fn require_nonempty(value: &str, label: &str) -> Result<(), RevisionError> {
    if value.is_empty() {
        return Err(RevisionError::new(format!("{label} must not be empty")));
    }
    Ok(())
}

fn require_lower_hex(value: &str, length: usize, label: &str) -> Result<(), RevisionError> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RevisionError::new(format!(
            "{label} must be exactly {length} lowercase hex characters"
        )));
    }
    Ok(())
}

fn require_sorted_unique(values: &[String], label: &str) -> Result<(), RevisionError> {
    require_sorted_unique_by(values, Clone::clone, label)
}

fn require_sorted_unique_by<T, F>(values: &[T], key: F, label: &str) -> Result<(), RevisionError>
where
    F: Fn(&T) -> String,
{
    let keys = values.iter().map(key).collect::<Vec<_>>();
    if keys
        .windows(2)
        .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
    {
        return Err(RevisionError::new(format!(
            "{label} must be byte-sorted and unique"
        )));
    }
    Ok(())
}

fn external_identity(package: &ExternalPackage) -> String {
    format!(
        "{}@{}|{:?}|{}|{}",
        package.name,
        package.version,
        package.source_kind,
        package.source,
        package.revision.as_deref().unwrap_or_default()
    )
}

fn reject_duplicate_input_tags(inputs: &[RevisionInput]) -> Result<(), RevisionError> {
    let mut seen = BTreeSet::new();
    for input in inputs {
        if !seen.insert(input.tag.as_str()) {
            return Err(RevisionError::new(format!(
                "duplicate revision record tag: {}",
                input.tag
            )));
        }
    }
    Ok(())
}

fn require_directory(path: &Path, relative: &str) -> Result<(), RevisionError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| RevisionError::new(format!("inspect {}: {error}", path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RevisionError::new(format!(
            "governed root is not a directory: {relative}"
        )));
    }
    Ok(())
}

fn reject_symlink_components(repository_root: &Path, relative: &str) -> Result<(), RevisionError> {
    validate_relative_path(relative)?;
    let mut current = repository_root.to_path_buf();
    for component in relative.split('/') {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            RevisionError::new(format!("inspect {}: {error}", current.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(RevisionError::new(format!(
                "governed path traverses a symlink: {}",
                current.display()
            )));
        }
    }
    Ok(())
}

fn push_text(inputs: &mut Vec<RevisionInput>, tag: &str, value: &str) {
    inputs.push(RevisionInput {
        tag: tag.to_string(),
        value: value.as_bytes().to_vec(),
    });
}

fn frame(bytes: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("record length fits u64");
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write as _;
        write!(value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

pub fn governed_watch_paths(manifest: &Manifest) -> Result<Vec<PathBuf>, RevisionError> {
    validate_manifest(manifest)?;
    let mut paths = Vec::new();
    for package in &manifest.local_packages {
        paths.push(PathBuf::from(&package.root));
    }
    paths.push(PathBuf::from(&manifest.executor_composition.root));
    paths.extend(manifest.assets.iter().map(PathBuf::from));
    Ok(paths)
}
