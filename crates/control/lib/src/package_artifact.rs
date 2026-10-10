//! Package artifacts: the `push-package` verb and the package source of
//! `apply-package` and `publish-release` (docs/plan/platform-deploy.md §7.2,
//! contract A).
//!
//! `push-package` verifies an authored package before it pushes anything:
//! the policy contract of `generated/wamn.json` is satisfied, the migrations
//! plan, and every component admits against its built bytes, its declaration
//! and the generated statement facts. A component is one the package owns or a
//! palette component its wirings name; the build index of
//! `tools/build-components` names its bytes. Then it pushes each component as
//! its own OCI artifact under its digest, pushes the package artifact, reads
//! its manifest back, and records the manifest digest in
//! `catalog.package_artifacts` last.
//!
//! A package artifact is one OCI image manifest with artifact type
//! [`PACKAGE_ARTIFACT_TYPE`], the empty config, and these layers in order:
//! the exact `generated/wamn.json` bytes, then four tars (`migrations`,
//! `publication`, `sources`, `web`), then one canonical component descriptor
//! per component, sorted by name. Its tag is `<package_id>-<version>` under an
//! explicit `<registry>/<repository>` base. Its digest is the SHA-256 of the
//! manifest bytes. Each tar has sorted paths, a zero mtime and a fixed owner
//! and mode, so one tree packs to one digest. An unpacked artifact packs again
//! offline to the same digest.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Component as PathComponent, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, bail, ensure};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use tokio_postgres::Client as PgClient;
use wamn_catalog::{
    AdmittedComponent, ComponentConnection, ComponentDeclaration, ComponentDescriptor,
    ComponentPackageScope,
};
use wamn_engine::component_artifact::{
    component_artifact_config_bytes, component_artifact_reference, parse_component_artifact_base,
};
use wamn_runtime::component_artifact_source::read_ca_bundles;
use wamn_runtime::registry_credentials::{
    read_registry_credentials, read_registry_push_credentials,
};
use wamn_schema_generator::{AUTHORED_MANIFEST, PackageManifest, PolicyContractState};

use crate::component_declaration::{
    authored_base_digests, declared_platform_packages, render_declaration_document,
    render_palette_declaration,
};
use crate::push_component::{AdmitComponentRequest, admit_component, publish_and_verify};
use crate::push_release_manifest::{artifact_is_absent, registry_auth, registry_client};

/// Artifact type of a package artifact.
pub const PACKAGE_ARTIFACT_TYPE: &str = "application/vnd.wamn.package.v2";

/// Media type of the four tar layers.
pub const PACKAGE_TAR_MEDIA_TYPE: &str = "application/vnd.wamn.package.v1.tar";

/// Media type of layer 0, the exact `generated/wamn.json` bytes.
pub const PACKAGE_MANIFEST_MEDIA_TYPE: &str = "application/vnd.wamn.package.manifest.v1+json";

/// Media type of a component descriptor layer.
pub const COMPONENT_DESCRIPTOR_MEDIA_TYPE: &str =
    "application/vnd.wamn.component.descriptor.v1+json";

const EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
const EMPTY_CONFIG: &[u8] = b"{}";
const TITLE_ANNOTATION: &str = "org.opencontainers.image.title";
const PACKAGE_ID_ANNOTATION: &str = "wamn.package.id";
const PACKAGE_VERSION_ANNOTATION: &str = "wamn.package.version";
const COMPONENT_NAME_ANNOTATION: &str = "wamn.component.name";
const COMPONENT_DIGEST_ANNOTATION: &str = "wamn.component.digest";

/// Title of layer 0.
const MANIFEST_TITLE: &str = "wamn.json";

/// The tar layers in order: title and the package paths each carries. A path
/// that a package does not have is left out, and a layer with no file is an
/// empty tar.
pub const PACKAGE_TAR_LAYERS: [(&str, &[&str]); 4] = [
    ("migrations", &["migrations"]),
    ("publication", &["publication"]),
    (
        "sources",
        &[
            AUTHORED_MANIFEST,
            "command",
            "query",
            "generated/platform-policy/data-access.json",
            "generated/contracts",
            "generated/sql",
            "generated/publication",
            "generated/routes",
            "generated/package-identity.json",
        ],
    ),
    ("web", &["web/dist"]),
];

/// Where an unpacked artifact keeps its component descriptors, one
/// `<name>.json` each.
pub const DESCRIPTOR_DIRECTORY: &str = "descriptors";

/// The build index that `tools/build-components` writes for the packages under
/// `apps/`, relative to a package root.
pub const COMPONENT_INDEX: &str = "../target/components.json";

/// The platform component declarations, relative to a package root under
/// `apps/`. Each `<area>/<crate>/declaration.json.in` names its component.
pub const PLATFORM_DIRECTORY: &str = "../platform";

/// The tenant that admission writes into a component scope. A package
/// artifact is tenant-free, and a descriptor drops the scope.
const ADMISSION_TENANT: &str = "package-artifact";

/// The platform packages a package-owned component is admitted with.
const OWNED_PLATFORM_PACKAGES: [&str; 2] = ["wamn:node", "wamn:postgres"];

/// One entry of the build index [`COMPONENT_INDEX`].
#[derive(Debug, serde::Deserialize)]
struct IndexedComponent {
    name: String,
    file: PathBuf,
    sha256: String,
    recorded_at: u64,
}

/// One layer of a package artifact.
#[derive(Debug, Clone)]
pub struct PackedLayer {
    /// The layer media type.
    pub media_type: String,
    /// The layer annotations.
    pub annotations: BTreeMap<String, String>,
    /// The layer bytes.
    pub bytes: Vec<u8>,
}

/// One package tree packed into its layers and image manifest.
#[derive(Debug)]
pub struct PackedPackage {
    /// Package id from the package manifest.
    pub package_id: String,
    /// Package version from the package manifest.
    pub version: String,
    /// The layers in order.
    pub layers: Vec<PackedLayer>,
    /// The image manifest bytes.
    pub manifest: Vec<u8>,
    /// `sha256:<hex>` of the image manifest bytes: the artifact digest.
    pub digest: String,
}

impl PackedPackage {
    /// The artifact tag, `<package_id>-<version>`.
    pub fn tag(&self) -> String {
        format!("{}-{}", self.package_id, self.version)
    }
}

/// One component descriptor layer: the component name, its digest and the
/// canonical descriptor bytes.
#[derive(Debug, Clone)]
struct DescriptorFile {
    name: String,
    digest: String,
    bytes: Vec<u8>,
}

/// Pack the tree at `root` as it is, offline.
///
/// The descriptors are the files of [`DESCRIPTOR_DIRECTORY`], which only an
/// unpacked artifact has. So a source tree packs to a digest without
/// descriptor layers, which differs from the pushed digest, and an unpacked
/// artifact packs to the digest it was pushed under.
pub fn pack_package(root: &Path) -> anyhow::Result<PackedPackage> {
    let descriptors = descriptor_files(root)?
        .into_iter()
        .map(|(descriptor, bytes)| DescriptorFile {
            name: descriptor.component,
            digest: descriptor.component_digest,
            bytes,
        })
        .collect();
    pack_layers(root, &BTreeMap::new(), descriptors)
}

/// The component descriptors of the unpacked artifact at `root`, sorted by
/// component name: the admitted facts that `publish-release` composes into a
/// release (docs/plan/platform-deploy.md §8.1).
///
/// # Errors
///
/// When a descriptor file cannot be read, does not parse, is not named after
/// its component, or is not canonical JSON.
pub fn read_descriptors(root: &Path) -> anyhow::Result<Vec<ComponentDescriptor>> {
    let mut descriptors = descriptor_files(root)?
        .into_iter()
        .map(|(descriptor, _)| descriptor)
        .collect::<Vec<_>>();
    descriptors.sort_by(|left, right| left.component.cmp(&right.component));
    Ok(descriptors)
}

/// Each file of [`DESCRIPTOR_DIRECTORY`] under `root`, parsed, with its exact
/// bytes. The bytes are the canonical JSON of the parsed descriptor, so a
/// release that carries the parsed descriptor carries the layer bytes.
fn descriptor_files(root: &Path) -> anyhow::Result<Vec<(ComponentDescriptor, Vec<u8>)>> {
    let directory = root.join(DESCRIPTOR_DIRECTORY);
    let mut descriptors = Vec::new();
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    for entry in entries.into_iter().flatten() {
        let path = entry
            .with_context(|| format!("read {}", directory.display()))?
            .path();
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let descriptor: ComponentDescriptor = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse component descriptor {}", path.display()))?;
        ensure!(
            path.file_name().and_then(|name| name.to_str())
                == Some(format!("{}.json", descriptor.component).as_str()),
            "{} describes component {}",
            path.display(),
            descriptor.component
        );
        ensure!(
            wamn_execution_contract::canonical_json_bytes(
                &serde_json::to_value(&descriptor).context("encode a component descriptor")?
            ) == bytes,
            "component descriptor {} is not canonical JSON",
            path.display()
        );
        descriptors.push((descriptor, bytes));
    }
    Ok(descriptors)
}

/// The package artifact digest of the package at `root`, the key of its stage
/// evidence and stage progress (docs/plan/platform-deploy.md §10.2): the
/// offline manifest digest of the tree as it is ([`pack_package`]).
pub fn package_artifact_digest(root: &Path) -> anyhow::Result<String> {
    Ok(pack_package(root)?.digest)
}

/// Pack the tree at `root` with `publication` files added to the publication
/// layer and `descriptors` as the descriptor layers.
fn pack_layers(
    root: &Path,
    publication: &BTreeMap<String, Vec<u8>>,
    mut descriptors: Vec<DescriptorFile>,
) -> anyhow::Result<PackedPackage> {
    let manifest_path = wamn_schema_generator::package_manifest_path(root);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let (package_id, version) = package_coordinate(&manifest_bytes)
        .with_context(|| format!("read the package of {}", manifest_path.display()))?;
    ensure_tag(&format!("{package_id}-{version}"))?;

    let mut layers = vec![PackedLayer {
        media_type: PACKAGE_MANIFEST_MEDIA_TYPE.to_owned(),
        annotations: BTreeMap::from([(TITLE_ANNOTATION.to_owned(), MANIFEST_TITLE.to_owned())]),
        bytes: manifest_bytes,
    }];
    for (title, paths) in PACKAGE_TAR_LAYERS {
        let mut files = BTreeMap::new();
        for path in paths {
            collect_files(root, path, &mut files)?;
        }
        let mut entries = files
            .iter()
            .map(|(name, path)| {
                let data =
                    std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
                Ok((name.clone(), data))
            })
            .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
        if title == "publication" {
            for (name, data) in publication {
                ensure!(
                    !entries.contains_key(name),
                    "{} has {name}; push-package copies the platform template",
                    root.display()
                );
                entries.insert(name.clone(), data.clone());
            }
        }
        layers.push(PackedLayer {
            media_type: PACKAGE_TAR_MEDIA_TYPE.to_owned(),
            annotations: BTreeMap::from([(TITLE_ANNOTATION.to_owned(), title.to_owned())]),
            bytes: tar_layer(title, &entries)?,
        });
    }
    descriptors.sort_by(|left, right| left.name.cmp(&right.name));
    let mut annotations = BTreeMap::from([
        (PACKAGE_ID_ANNOTATION.to_owned(), package_id.clone()),
        (PACKAGE_VERSION_ANNOTATION.to_owned(), version.clone()),
    ]);
    for descriptor in descriptors {
        ensure!(
            annotations
                .insert(
                    format!("wamn.component.{}", descriptor.name),
                    descriptor.digest.clone()
                )
                .is_none(),
            "two descriptors name component {}",
            descriptor.name
        );
        layers.push(PackedLayer {
            media_type: COMPONENT_DESCRIPTOR_MEDIA_TYPE.to_owned(),
            annotations: BTreeMap::from([
                (COMPONENT_NAME_ANNOTATION.to_owned(), descriptor.name),
                (COMPONENT_DIGEST_ANNOTATION.to_owned(), descriptor.digest),
            ]),
            bytes: descriptor.bytes,
        });
    }
    let manifest = image_manifest(&layers, &annotations);
    Ok(PackedPackage {
        package_id,
        version,
        digest: sha256_digest(&manifest),
        layers,
        manifest,
    })
}

/// The canonical image manifest bytes of `layers`.
fn image_manifest(layers: &[PackedLayer], annotations: &BTreeMap<String, String>) -> Vec<u8> {
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_IMAGE_MEDIA_TYPE,
        "artifactType": PACKAGE_ARTIFACT_TYPE,
        "config": {
            "mediaType": EMPTY_CONFIG_MEDIA_TYPE,
            "digest": sha256_digest(EMPTY_CONFIG),
            "size": EMPTY_CONFIG.len(),
        },
        "layers": layers
            .iter()
            .map(|layer| serde_json::json!({
                "mediaType": layer.media_type,
                "digest": sha256_digest(&layer.bytes),
                "size": layer.bytes.len(),
                "annotations": layer.annotations,
            }))
            .collect::<Vec<_>>(),
        "annotations": annotations,
    });
    wamn_execution_contract::canonical_json_bytes(&manifest)
}

/// The package id and version of a package manifest.
fn package_coordinate(manifest: &[u8]) -> anyhow::Result<(String, String)> {
    let document: serde_json::Value = serde_json::from_slice(manifest)?;
    let field = |pointer: &str| {
        document
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .with_context(|| format!("the manifest has no {pointer}"))
    };
    Ok((field("/package/id")?, field("/package/version")?))
}

/// One deterministic tar of `entries`, keyed by package-relative path.
fn tar_layer(title: &str, entries: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        builder
            .append_data(&mut header, name, data.as_slice())
            .with_context(|| format!("add {name} to the {title} layer"))?;
    }
    builder
        .into_inner()
        .with_context(|| format!("finish the {title} layer"))
}

/// The file of the package-relative `logical` path. A `generated/` path names
/// a file of the package's output root.
fn physical_path(root: &Path, logical: &str) -> PathBuf {
    match Path::new(logical).strip_prefix("generated") {
        Ok(output) => wamn_schema_generator::output_root(root).join(output),
        Err(_) => root.join(logical),
    }
}

/// Add every regular file under `relative` to `files`, keyed by its path
/// relative to `root` with `/` separators.
fn collect_files(
    root: &Path,
    relative: &str,
    files: &mut BTreeMap<String, PathBuf>,
) -> anyhow::Result<()> {
    let path = physical_path(root, relative);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if metadata.is_file() {
        files.insert(relative.to_owned(), path);
    } else if metadata.is_dir() {
        for entry in std::fs::read_dir(&path).with_context(|| format!("read {}", path.display()))? {
            let entry = entry.with_context(|| format!("read {}", path.display()))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|name| anyhow::anyhow!("{} is not UTF-8", name.to_string_lossy()))?;
            collect_files(root, &format!("{relative}/{name}"), files)?;
        }
    } else {
        bail!(
            "{} is neither a file nor a directory; a package artifact carries neither links nor devices",
            path.display()
        );
    }
    Ok(())
}

fn sha256_digest(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes))
    )
}

/// Refuse a tag that the OCI distribution grammar does not admit.
fn ensure_tag(tag: &str) -> anyhow::Result<()> {
    let bytes = tag.as_bytes();
    ensure!(
        !bytes.is_empty()
            && bytes.len() <= 128
            && (bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_')
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')),
        "package tag {tag:?} is not an OCI tag"
    );
    Ok(())
}

/// Where the declaration of one component comes from.
#[derive(Debug)]
enum ComponentTemplate {
    /// `publication/components/<name>.json.in` of the package.
    Owned(PathBuf),
    /// The platform declaration of a palette component that a wiring names,
    /// with the one store alias the wirings give it.
    Palette {
        path: PathBuf,
        bytes: Vec<u8>,
        store_alias: Option<String>,
    },
}

/// One component the package pushes: its build index entry and its template.
#[derive(Debug)]
struct SourceComponent {
    name: String,
    file: PathBuf,
    sha256: String,
    template: ComponentTemplate,
}

/// The components that `manifest` owns or a wiring of `publication/wirings/`
/// names, sorted by name, with their build index entries. A missing index, a
/// name without an entry, an entry whose file is missing or newer than its
/// record, a palette component without exactly one platform declaration, and
/// a palette component with two store aliases refuse.
fn source_components(
    root: &Path,
    manifest: &PackageManifest,
) -> anyhow::Result<Vec<SourceComponent>> {
    let wired = wiring_components(root)?;
    let mut templates = BTreeMap::new();
    for name in manifest.components.keys() {
        templates.insert(
            name.clone(),
            ComponentTemplate::Owned(
                root.join("publication/components")
                    .join(format!("{name}.json.in")),
            ),
        );
    }
    for (name, aliases) in &wired {
        if templates.contains_key(name) {
            continue;
        }
        ensure!(
            aliases.len() <= 1,
            "the wirings give palette component {name} {} store aliases {aliases:?}, not one",
            aliases.len()
        );
        let (path, bytes) = platform_template(root, name)?;
        templates.insert(
            name.clone(),
            ComponentTemplate::Palette {
                path,
                bytes,
                store_alias: aliases.first().cloned(),
            },
        );
    }
    let index = root.join(COMPONENT_INDEX);
    let bytes = std::fs::read(&index).with_context(|| {
        format!(
            "read the build index {}; run tools/build-components first",
            index.display()
        )
    })?;
    let indexed: Vec<IndexedComponent> = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse the build index {}", index.display()))?;
    templates
        .into_iter()
        .map(|(name, template)| {
            let entry = indexed
                .iter()
                .find(|entry| entry.name == name)
                .with_context(|| {
                    format!("the build index {} has no component {name}", index.display())
                })?;
            let modified = std::fs::metadata(&entry.file)
                .and_then(|metadata| metadata.modified())
                .with_context(|| {
                    format!("component {name} has no built file {}", entry.file.display())
                })?
                .duration_since(std::time::UNIX_EPOCH)
                .context("read a file time")?
                .as_secs();
            ensure!(
                modified <= entry.recorded_at,
                "component {name} at {} is newer than its build index record; run tools/build-components again",
                entry.file.display()
            );
            Ok(SourceComponent {
                file: entry.file.clone(),
                sha256: entry.sha256.clone(),
                name,
                template,
            })
        })
        .collect()
}

/// The component names of the nodes of every wiring in `publication/wirings/`,
/// each with the `params.store_alias` values its nodes give it.
fn wiring_components(root: &Path) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let directory = root.join("publication/wirings");
    let mut names = BTreeMap::<String, BTreeSet<String>>::new();
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    for entry in entries {
        let path = entry
            .with_context(|| format!("read {}", directory.display()))?
            .path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let wiring: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        for node in wiring
            .get("nodes")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flat_map(|nodes| nodes.values())
        {
            let component = node
                .get("component")
                .and_then(serde_json::Value::as_str)
                .with_context(|| format!("a node of {} names no component", path.display()))?;
            let aliases = names.entry(component.to_owned()).or_default();
            if let Some(alias) = node
                .pointer("/params/store_alias")
                .and_then(serde_json::Value::as_str)
            {
                aliases.insert(alias.to_owned());
            }
        }
    }
    Ok(names)
}

/// The declaration template of the platform component `name`: the one
/// `declaration.json.in` under [`PLATFORM_DIRECTORY`] whose `component` is
/// `name`.
fn platform_template(root: &Path, name: &str) -> anyhow::Result<(PathBuf, Vec<u8>)> {
    let platform = root.join(PLATFORM_DIRECTORY);
    let mut found = Vec::new();
    for area in
        std::fs::read_dir(&platform).with_context(|| format!("read {}", platform.display()))?
    {
        let area = area
            .with_context(|| format!("read {}", platform.display()))?
            .path();
        if !area.is_dir() {
            continue;
        }
        for crate_directory in
            std::fs::read_dir(&area).with_context(|| format!("read {}", area.display()))?
        {
            let path = crate_directory
                .with_context(|| format!("read {}", area.display()))?
                .path()
                .join("declaration.json.in");
            if !path.is_file() {
                continue;
            }
            let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let declaration: serde_json::Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?;
            if declaration
                .get("component")
                .and_then(serde_json::Value::as_str)
                == Some(name)
            {
                found.push((path, bytes));
            }
        }
    }
    match <[(PathBuf, Vec<u8>); 1]>::try_from(found) {
        Ok([template]) => Ok(template),
        Err(found) => bail!(
            "{} platform declarations name component {name}, not one",
            found.len()
        ),
    }
}

/// A rendered declaration in a private file, removed on drop.
#[derive(Debug)]
struct DeclarationFile(PathBuf);

static DECLARATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl DeclarationFile {
    fn write(document: &serde_json::Value) -> anyhow::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "wamn-package-declaration-{}-{}.json",
            std::process::id(),
            DECLARATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("create {}", path.display()))?;
        let file_guard = Self(path);
        file.write_all(&serde_json::to_vec(document)?)
            .context("write the rendered declaration")?;
        Ok(file_guard)
    }
}

impl Drop for DeclarationFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One admitted component and its exact bytes.
#[derive(Debug)]
struct VerifiedComponent {
    facts: AdmittedComponent,
    bytes: Vec<u8>,
}

/// An authored package that passed every check of `push-package`, packed.
#[derive(Debug)]
struct VerifiedPackage {
    packed: PackedPackage,
    components: Vec<VerifiedComponent>,
}

/// Every check of `push-package`, before anything is pushed: the policy
/// contract is satisfied, the migrations plan, and every component admits.
fn verify_package(root: &Path) -> anyhow::Result<VerifiedPackage> {
    ensure!(
        root.join(AUTHORED_MANIFEST).is_file(),
        "{} has no {AUTHORED_MANIFEST}; push-package takes an authored package",
        root.display()
    );
    let manifest_path = wamn_schema_generator::package_manifest_path(root);
    let manifest = PackageManifest::from_slice(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?,
    )
    .with_context(|| format!("parse {}", manifest_path.display()))?;
    let contract = &manifest.required_platform_policy_contract;
    ensure!(
        contract.state == PolicyContractState::Satisfied,
        "{}@{} requires platform policy contract {}, which is not satisfied; nothing was pushed",
        manifest.package.id,
        manifest.package.version,
        contract.id
    );
    let directory = crate::apply_package::read_package_directory(root)?;
    wamn_schema_control::plan_package_migrations(&directory, None)
        .context("plan the package migrations")?;

    let base_digests = authored_base_digests(root).context("read the authored base digests")?;
    let scope = ComponentPackageScope::new(
        ADMISSION_TENANT,
        manifest.package.id.clone(),
        manifest.package.version.clone(),
    )
    .context("name the admission scope")?;
    let mut publication = BTreeMap::new();
    let mut descriptors = Vec::new();
    let mut components = Vec::new();
    for source in source_components(root, &manifest)? {
        let (document, platform_packages) = match &source.template {
            ComponentTemplate::Owned(template) => (
                render_declaration_document(template, ADMISSION_TENANT, &base_digests)?,
                OWNED_PLATFORM_PACKAGES.map(str::to_owned).to_vec(),
            ),
            ComponentTemplate::Palette {
                path,
                bytes,
                store_alias,
            } => {
                publication.insert(
                    format!("publication/components/{}.json.in", source.name),
                    bytes.clone(),
                );
                let document = render_palette_declaration(path, &scope, store_alias.as_deref())?;
                let packages = declared_platform_packages(path, &document)?;
                (document, packages)
            }
        };
        let mut connections: Vec<ComponentConnection> =
            serde_json::from_value::<ComponentDeclaration>(document.clone())
                .with_context(|| format!("parse the declaration of component {}", source.name))?
                .connections;
        connections.sort_by(|left, right| left.store_alias.cmp(&right.store_alias));
        let declaration = DeclarationFile::write(&document)?;
        let admission = admit_component(AdmitComponentRequest {
            package: root.to_owned(),
            component_bytes: source.file.clone(),
            declaration: declaration.0.clone(),
            admitted_platform_packages: platform_packages,
        })
        .with_context(|| format!("admit component {}", source.name))?;
        let facts = admission.facts().clone();
        let mut required = admission
            .requirements()
            .iter()
            .map(wamn_catalog::ComponentConnectionRequirement::store_alias)
            .collect::<Vec<_>>();
        required.sort_unstable();
        ensure!(
            required
                == connections
                    .iter()
                    .map(|connection| connection.store_alias.as_str())
                    .collect::<Vec<_>>(),
            "component {} admits connections {required:?}, not its declared ones",
            source.name
        );
        ensure!(
            facts.component == source.name,
            "the declaration of component {} names component {}",
            source.name,
            facts.component
        );
        let bytes = std::fs::read(&source.file)
            .with_context(|| format!("read {}", source.file.display()))?;
        let digest = sha256_digest(&bytes);
        ensure!(
            digest == facts.component_digest && digest == format!("sha256:{}", source.sha256),
            "component {} is {digest}, but admission saw {} and the build index records sha256:{}",
            source.name,
            facts.component_digest,
            source.sha256
        );
        let descriptor = ComponentDescriptor::new(facts.clone(), connections);
        descriptors.push(DescriptorFile {
            name: source.name.clone(),
            digest: digest.clone(),
            bytes: wamn_execution_contract::canonical_json_bytes(
                &serde_json::to_value(&descriptor).context("encode a component descriptor")?,
            ),
        });
        components.push(VerifiedComponent { facts, bytes });
    }
    Ok(VerifiedPackage {
        packed: pack_layers(root, &publication, descriptors)?,
        components,
    })
}

/// The registry inputs that `push-package` and `apply-package
/// --package-artifact` share.
#[derive(Debug, Clone)]
pub struct PackageRegistry {
    /// Explicit `<registry>/<repository>` base for package artifacts.
    pub artifact_base: String,
    /// `.dockerconfigjson` file carrying the registry credential.
    pub registry_auth_file: PathBuf,
    /// Use plain HTTP for exactly the registry in `artifact_base`.
    pub insecure_registry: bool,
    /// PEM CA bundles trusted for the registry.
    pub oci_ca_paths: Vec<PathBuf>,
    /// Owner URL of the control database that holds `catalog.package_artifacts`.
    pub control_database_url: String,
}

/// One open registry repository and the tag of one package.
struct Repository {
    client: OciClient,
    auth: RegistryAuth,
    reference: Reference,
}

impl Repository {
    fn open(registry: &PackageRegistry, tag: &str) -> anyhow::Result<Self> {
        ensure_tag(tag)?;
        let base = parse_component_artifact_base(&registry.artifact_base)
            .context("read the package artifact base")?;
        let credentials = read_registry_credentials(&registry.registry_auth_file, base.registry())
            .with_context(|| format!("load the credential for registry {}", base.registry()))?;
        let ca_bundles = read_ca_bundles(&registry.oci_ca_paths)
            .with_context(|| format!("read CA bundles for registry {}", base.registry()))?;
        let client = registry_client(base.registry(), registry.insecure_registry, ca_bundles)?;
        Ok(Self {
            client,
            auth: registry_auth(Some(&credentials)),
            reference: Reference::with_tag(
                base.registry().to_owned(),
                base.repository().to_owned(),
                tag.to_owned(),
            ),
        })
    }

    /// The manifest bytes and digest of the tag, or `None` when the tag does
    /// not exist.
    async fn manifest(&self) -> anyhow::Result<Option<(Vec<u8>, String)>> {
        match self
            .client
            .pull_manifest_raw(&self.reference, &self.auth, &[OCI_IMAGE_MEDIA_TYPE])
            .await
        {
            Ok((bytes, digest)) => {
                ensure!(
                    sha256_digest(&bytes) == digest,
                    "the manifest of package artifact {} does not hash to {digest}",
                    self.reference
                );
                Ok(Some((bytes.to_vec(), digest)))
            }
            Err(error) if artifact_is_absent(&error) => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("read package artifact {}", self.reference))
            }
        }
    }

    /// Push every blob of `packed`, then its manifest bytes.
    async fn push(&self, packed: &PackedPackage) -> anyhow::Result<()> {
        for bytes in packed
            .layers
            .iter()
            .map(|layer| layer.bytes.as_slice())
            .chain([EMPTY_CONFIG])
        {
            self.client
                .push_blob(&self.reference, bytes.to_vec(), &sha256_digest(bytes))
                .await
                .with_context(|| format!("push a blob of package artifact {}", self.reference))?;
        }
        self.client
            .push_manifest_raw(
                &self.reference,
                packed.manifest.clone(),
                reqwest::header::HeaderValue::from_static(OCI_IMAGE_MEDIA_TYPE),
            )
            .await
            .with_context(|| format!("push package artifact {}", self.reference))?;
        Ok(())
    }

    /// The layers of the artifact under the tag, whose manifest digest must be
    /// `digest`.
    async fn pull(&self, digest: &str) -> anyhow::Result<Vec<PackedLayer>> {
        let (bytes, found) = self
            .manifest()
            .await?
            .with_context(|| format!("package artifact {} does not exist", self.reference))?;
        ensure!(
            found == digest,
            "package artifact {} has digest {found}, but catalog.package_artifacts records {digest}",
            self.reference
        );
        let manifest: OciImageManifest = serde_json::from_slice(&bytes).with_context(|| {
            format!("parse the manifest of package artifact {}", self.reference)
        })?;
        ensure!(
            manifest.artifact_type.as_deref() == Some(PACKAGE_ARTIFACT_TYPE)
                && manifest.layers.len() > PACKAGE_TAR_LAYERS.len(),
            "{} is not a {PACKAGE_ARTIFACT_TYPE} package artifact",
            self.reference
        );
        let mut layers = Vec::new();
        for layer in &manifest.layers {
            layers.push(self.pull_layer(layer).await?);
        }
        Ok(layers)
    }

    async fn pull_layer(&self, layer: &OciDescriptor) -> anyhow::Result<PackedLayer> {
        let mut bytes = Vec::new();
        self.client
            .pull_blob(&self.reference, layer, &mut bytes)
            .await
            .with_context(|| format!("pull a layer of package artifact {}", self.reference))?;
        ensure!(
            sha256_digest(&bytes) == layer.digest,
            "a layer of package artifact {} does not hash to {}",
            self.reference,
            layer.digest
        );
        Ok(PackedLayer {
            media_type: layer.media_type.clone(),
            annotations: layer
                .annotations
                .clone()
                .unwrap_or_default()
                .into_iter()
                .collect(),
            bytes,
        })
    }
}

/// Inputs of one `push-package` run.
#[derive(Debug)]
pub struct PushPackageRequest {
    /// Root of the authored package.
    pub package: PathBuf,
    /// Registry and control database.
    pub registry: PackageRegistry,
    /// Explicit `<registry>/<repository>` base for component artifacts, in the
    /// registry of `registry`.
    pub component_artifact_base: String,
    /// The source commit recorded with the artifact.
    pub source_commit: Option<String>,
}

/// Whether the run pushed the artifact or found the same bytes under its tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagePushDisposition {
    /// The run pushed the artifact.
    Pushed,
    /// The tag already held the same artifact.
    AlreadyPresent,
}

/// Result of one `push-package` run.
#[derive(Debug)]
pub struct PushedPackage {
    /// The artifact tag.
    pub tag: String,
    /// `sha256:<hex>` of the image manifest.
    pub digest: String,
    /// Whether the run pushed.
    pub disposition: PackagePushDisposition,
}

/// Verify the package, push its components and its package artifact under
/// `<package_id>-<version>`, and record the artifact digest in
/// `catalog.package_artifacts`.
///
/// Every check runs before the first push. A recorded or pushed tag with
/// another digest refuses and names both digests. The same artifact pushes no
/// package manifest and leaves the record unchanged. The record is written
/// last, after every push is read back.
pub async fn push_package(request: &PushPackageRequest) -> anyhow::Result<PushedPackage> {
    if let Some(source_commit) = &request.source_commit {
        ensure!(
            !source_commit.is_empty() && !source_commit.chars().any(char::is_whitespace),
            "source commit must be one nonempty value"
        );
    }
    let verified = verify_package(&request.package)?;
    let packed = &verified.packed;
    let tag = packed.tag();
    let repository = Repository::open(&request.registry, &tag)?;
    crate::publish_release::on_control_plane(
        &request.registry.control_database_url,
        async |control| {
            let recorded = recorded_digest(control, &packed.package_id, &packed.version).await?;
            if let Some(recorded) = &recorded {
                ensure!(
                    *recorded == packed.digest,
                    "catalog.package_artifacts records {recorded} for {tag}, but the package packs to {}; nothing was pushed",
                    packed.digest
                );
            }
            for component in &verified.components {
                push_component_artifact(
                    &request.registry,
                    &request.component_artifact_base,
                    component,
                )
                .await?;
            }
            let disposition = match repository.manifest().await? {
                Some((_, existing)) if existing == packed.digest => {
                    PackagePushDisposition::AlreadyPresent
                }
                Some((_, existing)) => bail!(
                    "package artifact {} holds {existing}, but the package packs to {}; tags are immutable",
                    repository.reference,
                    packed.digest
                ),
                None => {
                    repository.push(packed).await?;
                    let pulled = repository.manifest().await?;
                    ensure!(
                        pulled.as_ref().map(|(_, digest)| digest.as_str())
                            == Some(packed.digest.as_str()),
                        "pushed package artifact {} reads back as {}, not {}",
                        repository.reference,
                        pulled.as_ref().map_or("nothing", |(_, digest)| digest.as_str()),
                        packed.digest
                    );
                    PackagePushDisposition::Pushed
                }
            };
            if recorded.is_none() {
                control
                    .execute(
                        "INSERT INTO catalog.package_artifacts \
                           (package_id, version, digest, source_commit) \
                         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                        &[
                            &packed.package_id,
                            &packed.version,
                            &packed.digest,
                            &request.source_commit,
                        ],
                    )
                    .await
                    .context("record the package artifact")?;
                let stored = recorded_digest(control, &packed.package_id, &packed.version).await?;
                ensure!(
                    stored.as_deref() == Some(packed.digest.as_str()),
                    "catalog.package_artifacts records {} for {tag}, but the push was {}",
                    stored.as_deref().unwrap_or("nothing"),
                    packed.digest
                );
            }
            Ok(PushedPackage {
                tag: tag.clone(),
                digest: packed.digest.clone(),
                disposition,
            })
        },
    )
    .await
}

/// Push one admitted component under its digest and pull it back through the
/// production puller.
async fn push_component_artifact(
    registry: &PackageRegistry,
    artifact_base: &str,
    component: &VerifiedComponent,
) -> anyhow::Result<()> {
    let artifact = component_artifact_reference(artifact_base, &component.facts.component_digest)
        .context("derive the component artifact reference")?;
    let reference = Reference::with_tag(
        artifact.registry().to_owned(),
        artifact.repository().to_owned(),
        artifact.tag().to_owned(),
    );
    let credentials =
        read_registry_push_credentials(&registry.registry_auth_file, artifact.registry())
            .context("load the component registry push credential")?;
    publish_and_verify(
        &reference,
        artifact_base,
        registry.insecure_registry,
        &registry.oci_ca_paths,
        &component.bytes,
        &component_artifact_config_bytes(&component.facts),
        &component.facts,
        credentials.as_ref(),
    )
    .await
}

async fn recorded_digest(
    control: &PgClient,
    package_id: &str,
    version: &str,
) -> anyhow::Result<Option<String>> {
    Ok(control
        .query_opt(
            "SELECT digest FROM catalog.package_artifacts WHERE package_id = $1 AND version = $2",
            &[&package_id, &version],
        )
        .await
        .context("read catalog.package_artifacts")?
        .map(|row| row.get(0)))
}

/// Where `apply-package` reads a package from: one of `--package` or
/// `--package-artifact`.
#[derive(Debug)]
pub enum PackageSource {
    /// A local package directory.
    Directory(PathBuf),
    /// The artifact `<package_id>-<version>` in a registry.
    Artifact {
        /// The artifact tag.
        tag: String,
        /// Registry and control database.
        registry: PackageRegistry,
    },
}

/// An opened package source. An unpacked artifact lives in a private
/// temporary directory that is removed when this value drops.
#[derive(Debug)]
pub struct OpenedPackage {
    root: PathBuf,
    temporary: bool,
}

impl OpenedPackage {
    /// The package root.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for OpenedPackage {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

static UNPACK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Open a package source. An artifact is fetched only when its manifest digest
/// equals the digest that `catalog.package_artifacts` records. It is unpacked
/// into a private temporary directory, which must pack again to that digest.
pub async fn open_package_source(source: PackageSource) -> anyhow::Result<OpenedPackage> {
    let (tag, registry) = match source {
        PackageSource::Directory(root) => {
            return Ok(OpenedPackage {
                root,
                temporary: false,
            });
        }
        PackageSource::Artifact { tag, registry } => (tag, registry),
    };
    let digest =
        crate::publish_release::on_control_plane(&registry.control_database_url, async |control| {
            let rows = control
                .query(
                    "SELECT digest FROM catalog.package_artifacts \
                      WHERE package_id || '-' || version = $1",
                    &[&tag],
                )
                .await
                .context("read catalog.package_artifacts")?;
            let [row] = rows.as_slice() else {
                bail!(
                    "catalog.package_artifacts records {} artifacts for {tag}, not one",
                    rows.len()
                );
            };
            Ok(row.get::<_, String>(0))
        })
        .await?;
    open_artifact(&registry, &tag, &digest).await
}

/// Open the verified package artifact of `package_id@version`, the one
/// `catalog.package_artifacts` records with its `verified_at` (R11 (2)).
///
/// # Errors
///
/// When no verified artifact is recorded for the package (run
/// `push-package`), or the artifact cannot be fetched or does not unpack to
/// its digest.
pub async fn open_verified_package(
    registry: &PackageRegistry,
    package_id: &str,
    version: &str,
) -> anyhow::Result<OpenedPackage> {
    let digest =
        crate::publish_release::on_control_plane(&registry.control_database_url, async |control| {
            let row = control
                .query_opt(
                    "SELECT digest, verified_at IS NOT NULL FROM catalog.package_artifacts \
                      WHERE package_id = $1 AND version = $2",
                    &[&package_id, &version],
                )
                .await
                .context("read catalog.package_artifacts")?;
            match row {
                Some(row) if row.get::<_, bool>(1) => Ok(row.get::<_, String>(0)),
                _ => bail!(
                    "package {package_id}@{version} has no verified artifact in \
                     catalog.package_artifacts; run push-package"
                ),
            }
        })
        .await?;
    open_artifact(registry, &format!("{package_id}-{version}"), &digest).await
}

/// Fetch the artifact `tag`, whose manifest digest must be `digest`, and
/// unpack it into a private temporary directory, which must pack again to
/// `digest`.
async fn open_artifact(
    registry: &PackageRegistry,
    tag: &str,
    digest: &str,
) -> anyhow::Result<OpenedPackage> {
    let layers = Repository::open(registry, tag)?.pull(digest).await?;
    let root = std::env::temp_dir().join(format!(
        "wamn-package-{}-{}",
        std::process::id(),
        UNPACK_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .with_context(|| format!("create {}", root.display()))?;
    let opened = OpenedPackage {
        root,
        temporary: true,
    };
    unpack_layers(opened.root(), &layers)
        .with_context(|| format!("unpack package artifact {tag}"))?;
    let unpacked = pack_package(opened.root())?;
    ensure!(
        unpacked.tag() == tag,
        "package artifact {tag} holds package {}",
        unpacked.tag()
    );
    ensure!(
        unpacked.digest == digest,
        "package artifact {tag} unpacks to a tree that packs to {}, not {digest}",
        unpacked.digest
    );
    Ok(opened)
}

/// Unpack `layers` into `root`: layer 0 to `generated/wamn.json`, the tars in
/// place, and each descriptor to `descriptors/<name>.json`.
fn unpack_layers(root: &Path, layers: &[PackedLayer]) -> anyhow::Result<()> {
    let tar_count = PACKAGE_TAR_LAYERS.len();
    ensure!(
        layers.len() > tar_count,
        "a package artifact has at least {} layers, not {}",
        tar_count + 1,
        layers.len()
    );
    ensure!(
        layers[0].media_type == PACKAGE_MANIFEST_MEDIA_TYPE,
        "layer 0 is {}, not {PACKAGE_MANIFEST_MEDIA_TYPE}",
        layers[0].media_type
    );
    write_file(
        &physical_path(root, wamn_schema_generator::COMPILED_MANIFEST),
        &layers[0].bytes,
    )?;
    for layer in &layers[1..=tar_count] {
        ensure!(
            layer.media_type == PACKAGE_TAR_MEDIA_TYPE,
            "a tar layer is {}, not {PACKAGE_TAR_MEDIA_TYPE}",
            layer.media_type
        );
        let mut archive = tar::Archive::new(layer.bytes.as_slice());
        for entry in archive.entries().context("read a tar layer")? {
            let mut entry = entry.context("read a tar entry")?;
            ensure!(
                entry.header().entry_type() == tar::EntryType::Regular,
                "a tar layer holds an entry that is not a regular file"
            );
            let logical = entry
                .path()
                .context("read a tar entry path")?
                .to_str()
                .context("a tar entry path is not UTF-8")?
                .to_owned();
            ensure!(
                Path::new(&logical)
                    .components()
                    .all(|part| matches!(part, PathComponent::Normal(_))),
                "tar entry {logical:?} is not a package-relative path"
            );
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut data)
                .with_context(|| format!("read tar entry {logical}"))?;
            write_file(&physical_path(root, &logical), &data)?;
        }
    }
    for layer in &layers[tar_count + 1..] {
        ensure!(
            layer.media_type == COMPONENT_DESCRIPTOR_MEDIA_TYPE,
            "a descriptor layer is {}, not {COMPONENT_DESCRIPTOR_MEDIA_TYPE}",
            layer.media_type
        );
        let name = layer
            .annotations
            .get(COMPONENT_NAME_ANNOTATION)
            .context("a descriptor layer names no component")?;
        ensure!(
            !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..",
            "a descriptor layer names component {name:?}"
        );
        write_file(
            &root.join(DESCRIPTOR_DIRECTORY).join(format!("{name}.json")),
            &layer.bytes,
        )?;
    }
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, bytes: &[u8]) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// The build index of `apps/target` with `receiving` and `blob-put`, each
    /// recorded at `recorded_at`.
    fn write_index(apps: &Path, recorded_at: u64) {
        let entries = ["receiving", "blob-put"].map(|name| {
            let file = apps.join(format!("target/built/{}.wasm", name.replace('-', "_")));
            serde_json::json!({
                "name": name,
                "crate": name,
                "file": file,
                "sha256": hex::encode(ring::digest::digest(&ring::digest::SHA256, name.as_bytes())),
                "recorded_at": recorded_at,
            })
        });
        write(
            apps,
            "target/components.json",
            &serde_json::to_vec(&entries).unwrap(),
        );
    }

    fn receiving_manifest() -> Vec<u8> {
        std::fs::read(wamn_schema_generator::package_manifest_path(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving"),
        ))
        .unwrap()
    }

    /// A package root under `apps`, whose wiring names its own component and
    /// the platform component `blob-put`, with the build index that
    /// `tools/build-components` writes beside it.
    fn package_tree(name: &str) -> PathBuf {
        let apps = std::env::temp_dir().join(format!(
            "wamn-package-artifact-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&apps);
        write(&apps, "target/built/receiving.wasm", b"receiving");
        write(&apps, "target/built/blob_put.wasm", b"blob-put");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        write_index(&apps, now);
        write(
            &apps,
            "platform/execution/blob-put/declaration.json.in",
            b"{\"component\": \"blob-put\"}\n",
        );
        write(
            &apps,
            "platform/execution/jsonata/declaration.json.in",
            b"{\"component\": \"jsonata\"}\n",
        );
        let root = apps.join("wamn_receiving");
        write(&root, "wamn.k", b"# authored\n");
        write(&root, "generated/wamn.json", &receiving_manifest());
        write(&root, "generated/platform-policy/data-access.json", b"{}\n");
        write(&root, "generated/contracts/receipt.json", b"{}\n");
        write(&root, "generated/sql/receipt.sql", b"SELECT 1;\n");
        write(&root, "generated/package-identity.json", b"{}\n");
        write(&root, "migrations/0001_initial.sql", b"SELECT 1;\n");
        write(&root, "publication/attachments.json", b"{}\n");
        write(
            &root,
            "publication/wirings/store.json",
            b"{\"nodes\": {\"a\": {\"component\": \"receiving\"}, \"b\": {\"component\": \"blob-put\", \"params\": {\"store_alias\": \"labels\"}}}}\n",
        );
        write(&root, "web/dist/index.html", b"<html></html>\n");
        write(&root, "web/src/main.ts", b"left out\n");
        write(&root, "generated/client/left-out.ts", b"left out\n");
        root
    }

    fn manifest(root: &Path) -> PackageManifest {
        PackageManifest::from_slice(
            &std::fs::read(wamn_schema_generator::package_manifest_path(root)).unwrap(),
        )
        .unwrap()
    }

    fn remove(root: &Path) {
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    /// A canonical descriptor of the component `receiving`.
    fn descriptor() -> DescriptorFile {
        let digest = sha256_digest(b"receiving");
        let descriptor: ComponentDescriptor = serde_json::from_value(serde_json::json!({
            "component": "receiving",
            "interface-version": "0.1.0",
            "component-digest": digest,
            "operations": {},
            "imports": [],
            "imports-fingerprint": format!("sha256:{}", "b".repeat(64)),
            "effects": [],
            "connections": [],
        }))
        .unwrap();
        DescriptorFile {
            name: "receiving".to_owned(),
            digest,
            bytes: wamn_execution_contract::canonical_json_bytes(
                &serde_json::to_value(&descriptor).unwrap(),
            ),
        }
    }

    fn tar_entries(bytes: &[u8]) -> Vec<(String, u32, u64, u64)> {
        tar::Archive::new(bytes)
            .entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let header = entry.header();
                (
                    entry.path().unwrap().to_string_lossy().into_owned(),
                    header.mode().unwrap(),
                    header.uid().unwrap(),
                    header.mtime().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn two_packs_of_the_same_tree_give_one_digest() {
        let root = package_tree("same");
        let first = pack_package(&root).unwrap();
        // A newer mtime does not change the artifact.
        std::fs::write(root.join("migrations/0001_initial.sql"), b"SELECT 1;\n").unwrap();
        let second = pack_package(&root).unwrap();
        assert_eq!(first.digest, second.digest);
        assert_eq!(first.digest, sha256_digest(&first.manifest));
        assert_eq!(first.tag(), "wamn_receiving-2.1.0");

        let layout = first
            .layers
            .iter()
            .map(|layer| {
                (
                    layer.media_type.as_str(),
                    layer.annotations[TITLE_ANNOTATION].as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            layout,
            [
                (PACKAGE_MANIFEST_MEDIA_TYPE, "wamn.json"),
                (PACKAGE_TAR_MEDIA_TYPE, "migrations"),
                (PACKAGE_TAR_MEDIA_TYPE, "publication"),
                (PACKAGE_TAR_MEDIA_TYPE, "sources"),
                (PACKAGE_TAR_MEDIA_TYPE, "web"),
            ]
        );
        // Layer 0 is the exact compiled manifest, not a re-encoding.
        assert_eq!(first.layers[0].bytes, receiving_manifest());
        let paths = |index: usize| {
            tar_entries(&first.layers[index].bytes)
                .into_iter()
                .map(|(path, mode, uid, mtime)| {
                    assert_eq!((mode, uid, mtime), (0o644, 0, 0), "{path}");
                    path
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(1), ["migrations/0001_initial.sql"]);
        assert_eq!(
            paths(2),
            [
                "publication/attachments.json",
                "publication/wirings/store.json"
            ]
        );
        assert_eq!(
            paths(3),
            [
                "generated/contracts/receipt.json",
                "generated/package-identity.json",
                "generated/platform-policy/data-access.json",
                "generated/sql/receipt.sql",
                "wamn.k",
            ]
        );
        assert_eq!(paths(4), ["web/dist/index.html"]);

        let manifest: serde_json::Value = serde_json::from_slice(&first.manifest).unwrap();
        assert_eq!(manifest["schemaVersion"], 2);
        assert_eq!(manifest["mediaType"], OCI_IMAGE_MEDIA_TYPE);
        assert_eq!(manifest["artifactType"], PACKAGE_ARTIFACT_TYPE);
        assert_eq!(
            manifest["config"],
            serde_json::json!({
                "mediaType": EMPTY_CONFIG_MEDIA_TYPE,
                "digest": sha256_digest(b"{}"),
                "size": 2,
            })
        );
        assert_eq!(
            manifest["annotations"],
            serde_json::json!({
                "wamn.package.id": "wamn_receiving",
                "wamn.package.version": "2.1.0",
            })
        );
        remove(&root);
    }

    #[test]
    fn a_changed_file_changes_the_digest() {
        let root = package_tree("changed");
        let first = pack_package(&root).unwrap();
        std::fs::write(root.join("web/dist/index.html"), b"<html>2</html>\n").unwrap();
        assert_ne!(first.digest, pack_package(&root).unwrap().digest);
        remove(&root);
    }

    #[test]
    fn the_build_index_refuses_a_missing_or_stale_component() {
        let root = package_tree("index");
        let apps = root.parent().unwrap().to_path_buf();
        let manifest = manifest(&root);

        let listed = source_components(&root, &manifest).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|component| component.name.as_str())
                .collect::<Vec<_>>(),
            ["blob-put", "receiving"]
        );
        let ComponentTemplate::Palette {
            bytes, store_alias, ..
        } = &listed[0].template
        else {
            panic!("blob-put is a palette component");
        };
        assert_eq!(bytes, b"{\"component\": \"blob-put\"}\n");
        assert_eq!(store_alias.as_deref(), Some("labels"));

        std::fs::remove_file(apps.join("target/built/receiving.wasm")).unwrap();
        let error = format!("{:#}", source_components(&root, &manifest).unwrap_err());
        assert!(
            error.contains("component receiving has no built file"),
            "{error}"
        );

        write(&apps, "target/built/receiving.wasm", b"receiving");
        write_index(&apps, 0);
        let error = format!("{:#}", source_components(&root, &manifest).unwrap_err());
        assert!(
            error.contains("newer than its build index record"),
            "{error}"
        );

        write(
            &root,
            "publication/wirings/shape.json",
            b"{\"nodes\": {\"a\": {\"component\": \"label-render\"}}}\n",
        );
        let error = format!("{:#}", source_components(&root, &manifest).unwrap_err());
        assert!(
            error.contains("0 platform declarations name component label-render"),
            "{error}"
        );

        std::fs::remove_file(root.join("publication/wirings/shape.json")).unwrap();
        std::fs::remove_file(apps.join("target/components.json")).unwrap();
        let error = format!("{:#}", source_components(&root, &manifest).unwrap_err());
        assert!(
            error.contains("run tools/build-components first"),
            "{error}"
        );
        remove(&root);
    }

    #[test]
    fn two_store_aliases_for_one_palette_component_refuse() {
        let root = package_tree("aliases");
        write(
            &root,
            "publication/wirings/other.json",
            b"{\"nodes\": {\"a\": {\"component\": \"blob-put\", \"params\": {\"store_alias\": \"archive\"}}}}\n",
        );
        let error = format!(
            "{:#}",
            source_components(&root, &manifest(&root)).unwrap_err()
        );
        assert!(
            error.contains("palette component blob-put 2 store aliases"),
            "{error}"
        );
        remove(&root);
    }

    #[tokio::test]
    async fn an_unsatisfied_policy_contract_refuses_before_any_push() {
        let root = package_tree("policy");
        let mut document: serde_json::Value =
            serde_json::from_slice(&receiving_manifest()).unwrap();
        document["required_platform_policy_contract"]["state"] = serde_json::json!("unsatisfied");
        write(
            &root,
            "generated/wamn.json",
            &serde_json::to_vec(&document).unwrap(),
        );
        // No registry and no database exist: the refusal comes first.
        let error = push_package(&PushPackageRequest {
            package: root.clone(),
            registry: PackageRegistry {
                artifact_base: "registry.invalid/wamn/packages".to_owned(),
                registry_auth_file: root.join("missing-auth.json"),
                insecure_registry: false,
                oci_ca_paths: Vec::new(),
                control_database_url: "postgres://nobody@127.0.0.1:1/none".to_owned(),
            },
            component_artifact_base: "registry.invalid/wamn/components".to_owned(),
            source_commit: None,
        })
        .await
        .unwrap_err();
        let error = format!("{error:#}");
        assert!(
            error.contains(
                "requires platform policy contract receiving_data_access, which is not satisfied"
            ),
            "{error}"
        );
        remove(&root);
    }

    #[test]
    fn an_unpacked_artifact_packs_to_its_own_digest() {
        let root = package_tree("unpacked");
        let publication = BTreeMap::from([(
            "publication/components/blob-put.json.in".to_owned(),
            b"{\"component\": \"blob-put\"}\n".to_vec(),
        )]);
        let descriptor = descriptor();
        let packed = pack_layers(&root, &publication, vec![descriptor.clone()]).unwrap();
        // A source tree has no descriptor layers, so its digest differs.
        assert_ne!(pack_package(&root).unwrap().digest, packed.digest);

        let last = packed.layers.last().unwrap();
        assert_eq!(last.media_type, COMPONENT_DESCRIPTOR_MEDIA_TYPE);
        assert_eq!(
            last.annotations,
            BTreeMap::from([
                (COMPONENT_NAME_ANNOTATION.to_owned(), "receiving".to_owned()),
                (
                    COMPONENT_DIGEST_ANNOTATION.to_owned(),
                    descriptor.digest.clone()
                ),
            ])
        );
        let manifest: serde_json::Value = serde_json::from_slice(&packed.manifest).unwrap();
        assert_eq!(
            manifest["annotations"]["wamn.component.receiving"],
            descriptor.digest
        );

        let unpacked = root.parent().unwrap().join("unpacked");
        unpack_layers(&unpacked, &packed.layers).unwrap();
        assert_eq!(
            std::fs::read(unpacked.join("generated/wamn.json")).unwrap(),
            receiving_manifest()
        );
        assert_eq!(
            std::fs::read(unpacked.join("descriptors/receiving.json")).unwrap(),
            descriptor.bytes
        );
        assert!(
            unpacked
                .join("publication/components/blob-put.json.in")
                .is_file()
        );
        let repacked = pack_package(&unpacked).unwrap();
        assert_eq!(repacked.digest, packed.digest);
        assert_eq!(repacked.manifest, packed.manifest);
        remove(&root);
    }
}
