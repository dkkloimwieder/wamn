//! Package artifacts: the `push-package` verb and the package source of
//! `apply-package` and `publish-release` (docs/plan/platform-deploy.md §7.2,
//! contract A).
//!
//! `push-package` verifies an authored package before it pushes anything:
//! the policy contract of `generated/wamn.json` is satisfied, the migrations
//! plan, and every component admits against its built bytes, its declaration
//! and the generated statement facts. A component is one the package owns or a
//! palette component its wirings name; `outputs.components` of the package's
//! `build.json` names its bytes. A package without a `build.json`, or with one
//! whose `inputs.digest` differs from the tree, refuses: run `wamn build`.
//! It reads the tag `<package_id>-<version>` first: a tag with another digest
//! refuses, and the same digest is already present, so nothing is pushed
//! again. Then it pushes each component as its own OCI artifact under its
//! digest, pushes the package artifact, and reads its manifest back. No
//! database records the package: the artifact's existence is the proof of its
//! checks (R11).
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
use oci_client::Reference;
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
use wamn_catalog::{
    AdmittedComponent, ComponentConnection, ComponentDeclaration, ComponentDescriptor,
    ComponentPackageScope, component_artifact_reference,
};
use wamn_engine::component_artifact::component_artifact_config_bytes;
use wamn_package_artifact::registry_credentials::read_registry_push_credentials;
use wamn_schema_generator::{AUTHORED_MANIFEST, PackageManifest, PolicyContractState};

use crate::component_admission::{AdmitComponentRequest, admit_component, publish_and_verify};
use crate::component_declaration::{
    authored_base_digests, declared_platform_packages, render_declaration_document,
    render_palette_declaration,
};
pub use wamn_package_artifact::{
    COMPONENT_DESCRIPTOR_MEDIA_TYPE, NoPackageArtifact, PACKAGE_ARTIFACT_TYPE, PackageRegistry,
    PackedLayer,
};
use wamn_package_artifact::{Repository, ensure_tag, sha256_digest};

/// Media type of the four tar layers.
pub const PACKAGE_TAR_MEDIA_TYPE: &str = "application/vnd.wamn.package.v1.tar";

/// Media type of layer 0, the exact `generated/wamn.json` bytes.
pub const PACKAGE_MANIFEST_MEDIA_TYPE: &str = "application/vnd.wamn.package.manifest.v1+json";

const EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
const EMPTY_CONFIG: &[u8] = b"{}";
const TITLE_ANNOTATION: &str = "org.opencontainers.image.title";
const PACKAGE_ID_ANNOTATION: &str = "wamn.package.id";
const PACKAGE_VERSION_ANNOTATION: &str = "wamn.package.version";
const COMPONENT_NAME_ANNOTATION: &str = "wamn.component.name";
const COMPONENT_DIGEST_ANNOTATION: &str = "wamn.component.digest";

/// Title of layer 0.
const MANIFEST_TITLE: &str = "wamn.json";

/// The logical path of the web client build in the `web` layer. Its files lie
/// in the web build of the package's output root (§7.1 step 8, C10).
const WEB_LAYER_PATH: &str = "web/dist";

/// The tar layers in order: title and the logical package paths each carries.
/// A path that a package does not have is left out, and a layer with no file is
/// an empty tar. A `generated/` path and the `web/dist` path name files of the
/// package's output root (`physical_path`).
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
            wamn_schema_generator::REQUIRED_SCHEMA_CONTRACT_PATH,
            "generated/sql",
            "generated/publication",
            "generated/routes",
        ],
    ),
    ("web", &[WEB_LAYER_PATH]),
];

/// Where an unpacked artifact keeps its component descriptors, one
/// `<name>.json` each.
pub const DESCRIPTOR_DIRECTORY: &str = "descriptors";

/// The platform component declarations, relative to a package root under
/// `apps/`. Each `<area>/<crate>/declaration.json.in` names its component.
pub const PLATFORM_DIRECTORY: &str = "../platform";

/// The tenant that admission writes into a component scope. A package
/// artifact is tenant-free, and a descriptor drops the scope.
const ADMISSION_TENANT: &str = "package-artifact";

/// The platform packages a package-owned component is admitted with.
const OWNED_PLATFORM_PACKAGES: [&str; 2] = ["wamn:node", "wamn:postgres"];

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
    wamn_schema_generator::receipt::check_current(root)?;
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

/// The package root of a fixture tree that a unit test applies as a release,
/// with the stand-in artifact digest of
/// [`wamn_test_infrastructure::fixture_digest::fixture_artifact_digest`].
#[cfg(test)]
pub(crate) fn fixture_package_root(
    root: &Path,
) -> anyhow::Result<crate::apply_package::PackageRoot> {
    Ok(crate::apply_package::PackageRoot {
        path: root.to_owned(),
        artifact_digest: Some(
            wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(root)?,
        ),
    })
}

/// The committed record of a package's tag and built artifact digest, beside
/// its `wamn.k` (wamn-vl6im). Registry tags are immutable, so a package whose
/// bytes change under an unchanged tag cannot be pushed again.
pub const PACKAGE_DIGEST_FILE: &str = "package.digest";

/// Pack the built package at `root` and write `<tag> <digest>` to its
/// [`PACKAGE_DIGEST_FILE`]. Returns the line that it wrote.
///
/// # Errors
///
/// When the package does not pack or the file cannot be written.
pub fn record_package_digest(root: &Path) -> anyhow::Result<String> {
    let packed = pack_package(root)?;
    let line = format!("{} {}\n", packed.tag(), packed.digest);
    let path = root.join(PACKAGE_DIGEST_FILE);
    std::fs::write(&path, &line).with_context(|| format!("write {}", path.display()))?;
    Ok(line)
}

/// Refuse a built package whose tag or digest differs from its committed
/// [`PACKAGE_DIGEST_FILE`].
///
/// # Errors
///
/// When the package does not pack, the file is absent or malformed, the
/// digest changed under the same tag, or the tag changed and the file was not
/// written again.
pub fn check_package_digest(root: &Path) -> anyhow::Result<()> {
    let packed = pack_package(root)?;
    let path = root.join(PACKAGE_DIGEST_FILE);
    let recorded = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "read {}; run wamn build --record-digest {}",
            path.display(),
            root.display()
        )
    })?;
    digest_refusal(&path, &recorded, &packed.tag(), &packed.digest)
}

fn digest_refusal(path: &Path, recorded: &str, tag: &str, digest: &str) -> anyhow::Result<()> {
    let Some((recorded_tag, recorded_digest)) = recorded
        .strip_suffix('\n')
        .and_then(|line| line.split_once(' '))
    else {
        bail!("{} must hold one line `<tag> <digest>`", path.display());
    };
    ensure!(
        recorded_tag != tag || recorded_digest == digest,
        "package {tag} packs to {digest}, but {} records {recorded_digest} for the same tag; \
         registry tags are immutable, so bump the version in wamn.k and run wamn build \
         --record-digest",
        path.display()
    );
    ensure!(
        recorded_tag == tag,
        "package {tag} packs to {digest}, but {} records {recorded_tag}; run wamn build \
         --record-digest",
        path.display()
    );
    Ok(())
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
/// a file of the package's output root, and a [`WEB_LAYER_PATH`] path a file of
/// the output root's web build.
fn physical_path(root: &Path, logical: &str) -> PathBuf {
    let logical = Path::new(logical);
    if let Ok(output) = logical.strip_prefix("generated") {
        wamn_schema_generator::output_root(root).join(output)
    } else if let Ok(web) = logical.strip_prefix(WEB_LAYER_PATH) {
        wamn_schema_generator::output_root(root)
            .join(wamn_schema_generator::build::WEB_OUTPUT)
            .join(web)
    } else {
        root.join(logical)
    }
}

/// The web client build of the package tree at `root`: the web build of its
/// output root, which a source tree and an unpacked artifact both have there.
pub fn web_build(root: &Path) -> PathBuf {
    physical_path(root, WEB_LAYER_PATH)
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

/// One component the package pushes: its `build.json` entry and its template.
#[derive(Debug)]
struct SourceComponent {
    name: String,
    file: PathBuf,
    sha256: String,
    template: ComponentTemplate,
}

/// The components that `manifest` owns or a wiring of `publication/wirings/`
/// names, sorted by name, with their entries in `outputs.components` of the
/// package's `build.json`. A missing receipt, a receipt whose `inputs.digest`
/// differs from the tree, a name without an entry, a palette component without
/// exactly one platform declaration, and a palette component with two store
/// aliases refuse. The bytes of each entry are checked against its digest when
/// they are read (§7.2: never by timestamp).
fn source_components(
    root: &Path,
    manifest: &PackageManifest,
    receipt: &wamn_schema_generator::receipt::BuildReceipt,
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
    let output = wamn_schema_generator::output_root(root);
    templates
        .into_iter()
        .map(|(name, template)| {
            let entry = receipt
                .outputs
                .components
                .iter()
                .find(|entry| entry.name == name)
                .with_context(|| {
                    format!(
                        "build.json of {} records no component {name}; run wamn build again",
                        root.display()
                    )
                })?;
            Ok(SourceComponent {
                file: output.join(&entry.path),
                sha256: entry.sha256.clone(),
                name,
                template,
            })
        })
        .collect()
}

/// The component names of the nodes of every wiring in `publication/wirings/`,
/// each with the `params.store_alias` values its nodes give it.
pub(crate) fn wiring_components(root: &Path) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
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
            // A cross-package node resolves through the release, not this package.
            if node.get("operation-dependency").is_some() {
                continue;
            }
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
pub(crate) fn platform_template(root: &Path, name: &str) -> anyhow::Result<(PathBuf, Vec<u8>)> {
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

/// Refuse package declarations before the reader drops retired fields.
fn refuse_workflow_declarations(bytes: &[u8]) -> anyhow::Result<()> {
    let manifest: serde_json::Value = serde_json::from_slice(bytes)?;
    ensure!(
        manifest.get("workflows").is_none(),
        "a package cannot declare workflows; author workflows in the environment"
    );
    if let Some(operations) = manifest
        .get("custom_operations")
        .and_then(serde_json::Value::as_object)
    {
        for (name, operation) in operations {
            ensure!(
                operation.get("registration").is_none(),
                "operation {name:?} cannot declare a registration; author an event workflow in the environment"
            );
        }
    }
    Ok(())
}

/// Every check of `push-package`, before anything is pushed: the policy
/// contract is satisfied, the migrations plan, and every component admits.
fn verify_package(root: &Path) -> anyhow::Result<VerifiedPackage> {
    ensure!(
        root.join(AUTHORED_MANIFEST).is_file(),
        "{} has no {AUTHORED_MANIFEST}; push-package takes an authored package",
        root.display()
    );
    let receipt = wamn_schema_generator::receipt::read_current(
        root,
        &wamn_schema_generator::output_root(root),
    )?;
    let manifest_path = wamn_schema_generator::package_manifest_path(root);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    refuse_workflow_declarations(&manifest_bytes)?;
    let manifest = PackageManifest::from_slice(&manifest_bytes)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    crate::component_admission::check_package_backfill(&manifest)?;
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
    for source in source_components(root, &manifest, &receipt)? {
        let (document, platform_packages) = match &source.template {
            ComponentTemplate::Owned(template) => (
                render_declaration_document(template, ADMISSION_TENANT)?,
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
            base_digests: base_digests.values().map(ToString::to_string).collect(),
        })
        .with_context(|| format!("admit component {}", source.name))?;
        let facts = admission.facts().clone();
        let statement_instances = admission.statement_instances().clone();
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
            digest == facts.component_digest && digest == source.sha256,
            "component {} is {digest}, but admission saw {} and build.json records {}; run wamn build again",
            source.name,
            facts.component_digest,
            source.sha256
        );
        let descriptor = ComponentDescriptor::new(facts.clone(), connections)
            .with_statement_instances(statement_instances);
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

/// Push every blob of `packed` to `repository`, then its manifest bytes.
async fn push_artifact(repository: &Repository, packed: &PackedPackage) -> anyhow::Result<()> {
    for bytes in packed
        .layers
        .iter()
        .map(|layer| layer.bytes.as_slice())
        .chain([EMPTY_CONFIG])
    {
        repository
            .client
            .push_blob(&repository.reference, bytes.to_vec(), &sha256_digest(bytes))
            .await
            .with_context(|| format!("push a blob of package artifact {}", repository.reference))?;
    }
    repository
        .client
        .push_manifest_raw(
            &repository.reference,
            packed.manifest.clone(),
            reqwest::header::HeaderValue::from_static(OCI_IMAGE_MEDIA_TYPE),
        )
        .await
        .with_context(|| format!("push package artifact {}", repository.reference))?;
    Ok(())
}

/// Inputs of one `push-package` run.
#[derive(Debug)]
pub struct PushPackageRequest {
    /// Root of the authored package.
    pub package: PathBuf,
    /// The package registry.
    pub registry: PackageRegistry,
    /// Explicit `<registry>/<repository>` base for component artifacts, in the
    /// registry of `registry`.
    pub component_artifact_base: String,
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

/// Check the package, then push its components and its package artifact
/// under `<package_id>-<version>`.
///
/// Every check runs before the first push, and the tag is read before
/// anything is pushed: a tag that holds another digest refuses and names both
/// digests, and a tag that holds the same digest is already present, so
/// nothing is pushed again. Tags are immutable. No database records the
/// package; the artifact's existence is the proof of its checks (R11).
pub async fn push_package(request: &PushPackageRequest) -> anyhow::Result<PushedPackage> {
    let verified = verify_package(&request.package)?;
    let packed = &verified.packed;
    let tag = packed.tag();
    let repository = Repository::open(&request.registry, &tag)?;
    let disposition = match repository.manifest().await? {
        Some((_, existing)) if existing == packed.digest => PackagePushDisposition::AlreadyPresent,
        Some((_, existing)) => bail!(
            "package artifact {} holds {existing}, but the package packs to {}; tags are immutable, and nothing was pushed",
            repository.reference,
            packed.digest
        ),
        None => {
            for component in &verified.components {
                push_component_artifact(
                    &request.registry,
                    &request.component_artifact_base,
                    component,
                )
                .await?;
            }
            push_artifact(&repository, packed).await?;
            let pulled = repository.manifest().await?;
            ensure!(
                pulled.as_ref().map(|(_, digest)| digest.as_str()) == Some(packed.digest.as_str()),
                "pushed package artifact {} reads back as {}, not {}",
                repository.reference,
                pulled
                    .as_ref()
                    .map_or("nothing", |(_, digest)| digest.as_str()),
                packed.digest
            );
            PackagePushDisposition::Pushed
        }
    };
    Ok(PushedPackage {
        tag,
        digest: packed.digest.clone(),
        disposition,
    })
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
        /// The package registry.
        registry: PackageRegistry,
    },
}

/// An opened package source. An unpacked artifact lives in a private
/// temporary directory that is removed when this value drops.
#[derive(Debug)]
pub struct OpenedPackage {
    root: PathBuf,
    digest: Option<String>,
    temporary: bool,
}

impl OpenedPackage {
    /// The package root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The package's `wamn.json`: the bytes of the artifact's layer 0, as
    /// the opener unpacked them.
    ///
    /// # Errors
    ///
    /// When the unpacked file cannot be read.
    pub fn manifest(&self) -> anyhow::Result<Vec<u8>> {
        let path = self.root.join(wamn_schema_generator::COMPILED_MANIFEST);
        std::fs::read(&path).with_context(|| format!("read {}", path.display()))
    }

    /// The package root with the digest this opener holds in memory, the one
    /// carrier of the digest of an opened artifact. The digest is the one the
    /// registry records; a directory has none.
    pub fn package_root(&self) -> crate::apply_package::PackageRoot {
        crate::apply_package::PackageRoot {
            path: self.root.clone(),
            artifact_digest: self.digest.clone(),
        }
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

/// Open a package source. An artifact is pulled from the registry under its
/// tag and unpacked into a private temporary directory, which must pack again
/// to the digest the registry holds.
pub async fn open_package_source(source: PackageSource) -> anyhow::Result<OpenedPackage> {
    let (tag, registry) = match source {
        PackageSource::Directory(root) => {
            return Ok(OpenedPackage {
                root,
                digest: None,
                temporary: false,
            });
        }
        PackageSource::Artifact { tag, registry } => (tag, registry),
    };
    let (digest, layers) = Repository::open(&registry, &tag)?
        .pull()
        .await?
        .with_context(|| format!("package artifact {tag} does not exist; run push-package"))?;
    unpack_artifact(&tag, &digest, &layers)
}

/// Open the package artifact of `package_id@version` from the registry.
///
/// # Errors
///
/// [`NoPackageArtifact`] when the registry holds no artifact for the package,
/// or an error when the artifact cannot be fetched or does not unpack to its
/// digest.
pub async fn open_package(
    registry: &PackageRegistry,
    package_id: &str,
    version: &str,
) -> anyhow::Result<OpenedPackage> {
    let (digest, layers) =
        wamn_package_artifact::pull_package(registry, package_id, version).await?;
    unpack_artifact(&format!("{package_id}-{version}"), &digest, &layers)
}

/// Unpack the `layers` of the artifact `tag`, whose manifest digest is
/// `digest`, into a private temporary directory, which must pack again to
/// `digest`.
fn unpack_artifact(
    tag: &str,
    digest: &str,
    layers: &[PackedLayer],
) -> anyhow::Result<OpenedPackage> {
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
        digest: Some(digest.to_owned()),
        temporary: true,
    };
    unpack_layers(opened.root(), layers)
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
/// place, and each descriptor to `descriptors/<name>.json`. An unpacked tree
/// keeps the `generated/` layer paths, and the output root resolver reads its
/// `generated/` directory as its build output. So the `web/dist` entries go to
/// the web build of that output root, `generated/web`.
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
        &root.join(wamn_schema_generator::COMPILED_MANIFEST),
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
    #[test]
    fn publication_refuses_retired_declarations_before_manifest_reading_drops_them() {
        for document in [
            serde_json::json!({"workflows": {}}),
            serde_json::json!({"custom_operations": {"changed": {"registration": null}}}),
        ] {
            assert!(
                super::refuse_workflow_declarations(&serde_json::to_vec(&document).unwrap())
                    .is_err()
            );
        }
        super::refuse_workflow_declarations(br#"{"custom_operations":{}}"#).unwrap();
    }

    use super::*;

    fn write(root: &Path, relative: &str, bytes: &[u8]) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// The `build.json` of the package at `root`, current for its tree, with
    /// the components `receiving` and `blob-put` in its build output.
    fn write_receipt(root: &Path) {
        use wamn_schema_generator::receipt;
        let output = wamn_schema_generator::output_root(root);
        let components = ["blob-put", "receiving"]
            .map(|name| {
                let path = format!("components/{name}.wasm");
                write(&output, &path, name.as_bytes());
                receipt::ReceiptComponent {
                    name: name.to_owned(),
                    path,
                    sha256: sha256_digest(name.as_bytes()),
                }
            })
            .to_vec();
        let receipt = receipt::BuildReceipt {
            schema: receipt::RECEIPT_SCHEMA.to_owned(),
            package: receipt::ReceiptPackage {
                id: "wamn_receiving".to_owned(),
                version: "2.1.0".to_owned(),
            },
            inputs: receipt::ReceiptInputs {
                digest: receipt::input_digest(root).unwrap(),
                files: BTreeMap::new(),
            },
            dependencies: Vec::new(),
            generator: receipt::ReceiptGenerator {
                id: "generator".to_owned(),
                toolchain: "toolchain".to_owned(),
                sqlx: "0.9.0".to_owned(),
            },
            outputs: receipt::ReceiptOutputs {
                manifest: sha256_digest(b"manifest"),
                files: BTreeMap::new(),
                components,
                web: None,
            },
        };
        write(&output, receipt::RECEIPT_FILE, &receipt.to_bytes());
    }

    fn receiving_manifest() -> Vec<u8> {
        wamn_schema_generator::receipt::check_current(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving"),
        )
        .unwrap();
        std::fs::read(wamn_schema_generator::package_manifest_path(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving"),
        ))
        .unwrap()
    }

    /// A package root under `apps`, whose wiring names its own component and
    /// the platform component `blob-put`, with a current `build.json`.
    fn package_tree(name: &str) -> PathBuf {
        let apps = std::env::temp_dir().join(format!(
            "wamn-package-artifact-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&apps);
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
        write(
            &root,
            wamn_schema_generator::REQUIRED_SCHEMA_CONTRACT_PATH,
            b"{\"tables\": []}\n",
        );
        write(&root, "generated/sql/receipt.sql", b"SELECT 1;\n");
        write(&root, "migrations/0001_initial.sql", b"SELECT 1;\n");
        write(&root, "publication/attachments.json", b"{}\n");
        write(
            &root,
            "publication/wirings/store.json",
            b"{\"nodes\": {\"a\": {\"component\": \"receiving\"}, \"b\": {\"component\": \"blob-put\", \"params\": {\"store_alias\": \"labels\"}}}}\n",
        );
        write(&root, "generated/web/index.html", b"<html></html>\n");
        write(&root, "web/dist/stale.html", b"left out\n");
        write(&root, "web/src/main.ts", b"left out\n");
        write(&root, "generated/client/left-out.ts", b"left out\n");
        write_receipt(&root);
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
    fn a_package_digest_refuses_new_bytes_under_the_same_tag_and_a_stale_record() {
        let path = Path::new("apps/p/package.digest");
        let refusal = |recorded: &str, tag: &str, digest: &str| {
            digest_refusal(path, recorded, tag, digest)
                .err()
                .map(|error| error.to_string())
        };
        assert_eq!(refusal("p-1.0.0 sha256:a\n", "p-1.0.0", "sha256:a"), None);
        let changed = refusal("p-1.0.0 sha256:a\n", "p-1.0.0", "sha256:b").unwrap();
        assert!(
            changed.contains("immutable") && changed.contains("bump the version"),
            "{changed}"
        );
        let stale = refusal("p-1.0.0 sha256:a\n", "p-1.0.1", "sha256:b").unwrap();
        assert!(
            stale.contains("records p-1.0.0") && stale.contains("--record-digest"),
            "{stale}"
        );
        let bumped = refusal("p-1.0.0 sha256:a\n", "p-1.0.1", "sha256:a").unwrap();
        assert!(bumped.contains("--record-digest"), "{bumped}");
        assert!(refusal("p-1.0.0 sha256:a", "p-1.0.0", "sha256:a").is_some());
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
        assert_eq!(first.tag(), "wamn_receiving-2.2.8");

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
                "generated/platform-policy/data-access.json",
                "generated/required-schema-contract.json",
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
                "wamn.package.version": "2.2.8",
            })
        );
        remove(&root);
    }

    #[test]
    fn a_changed_file_changes_the_digest() {
        let root = package_tree("changed");
        let first = pack_package(&root).unwrap();
        std::fs::write(root.join("generated/web/index.html"), b"<html>2</html>\n").unwrap();
        assert_ne!(first.digest, pack_package(&root).unwrap().digest);
        remove(&root);
    }

    fn current_source_components(
        root: &Path,
        manifest: &PackageManifest,
    ) -> anyhow::Result<Vec<SourceComponent>> {
        let receipt = wamn_schema_generator::receipt::read_current(
            root,
            &wamn_schema_generator::output_root(root),
        )?;
        source_components(root, manifest, &receipt)
    }

    #[test]
    fn the_build_refuses_a_stale_receipt_or_a_missing_component() {
        let root = package_tree("index");
        let manifest = manifest(&root);

        let listed = current_source_components(&root, &manifest).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|component| component.name.as_str())
                .collect::<Vec<_>>(),
            ["blob-put", "receiving"]
        );
        assert_eq!(listed[1].sha256, sha256_digest(b"receiving"));
        let ComponentTemplate::Palette {
            bytes, store_alias, ..
        } = &listed[0].template
        else {
            panic!("blob-put is a palette component");
        };
        assert_eq!(bytes, b"{\"component\": \"blob-put\"}\n");
        assert_eq!(store_alias.as_deref(), Some("labels"));

        write(&root, "migrations/0002_more.sql", b"SELECT 2;\n");
        let error = format!(
            "{:#}",
            current_source_components(&root, &manifest).unwrap_err()
        );
        assert!(error.contains("is stale"), "{error}");

        write(
            &root,
            "publication/wirings/shape.json",
            b"{\"nodes\": {\"a\": {\"component\": \"jsonata\"}}}\n",
        );
        write_receipt(&root);
        let error = format!(
            "{:#}",
            current_source_components(&root, &manifest).unwrap_err()
        );
        assert!(error.contains("records no component jsonata"), "{error}");

        write(
            &root,
            "publication/wirings/shape.json",
            b"{\"nodes\": {\"a\": {\"component\": \"label-render\"}}}\n",
        );
        write_receipt(&root);
        let error = format!(
            "{:#}",
            current_source_components(&root, &manifest).unwrap_err()
        );
        assert!(
            error.contains("0 platform declarations name component label-render"),
            "{error}"
        );

        write(
            &root,
            "publication/wirings/shape.json",
            b"{\"nodes\": {\"a\": {\"component\": \"consumer\", \"operation-dependency\": {\"alias\": \"consumer\", \"operation\": \"item.record\"}}}}\n",
        );
        assert!(
            !wiring_components(&root).unwrap().contains_key("consumer"),
            "a cross-package node resolves through the release"
        );

        std::fs::remove_file(root.join("publication/wirings/shape.json")).unwrap();
        std::fs::remove_file(
            wamn_schema_generator::output_root(&root)
                .join(wamn_schema_generator::receipt::RECEIPT_FILE),
        )
        .unwrap();
        let error = format!(
            "{:#}",
            current_source_components(&root, &manifest).unwrap_err()
        );
        assert!(error.contains("run wamn build first"), "{error}");
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
        write_receipt(&root);
        let error = format!(
            "{:#}",
            current_source_components(&root, &manifest(&root)).unwrap_err()
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
        // No registry exists: the refusal comes first.

        let error = push_package(&PushPackageRequest {
            package: root.clone(),
            registry: PackageRegistry {
                artifact_base: "registry.invalid/wamn/packages".to_owned(),
                registry_auth_file: root.join("missing-auth.json"),
                insecure_registry: false,
                oci_ca_paths: Vec::new(),
            },
            component_artifact_base: "registry.invalid/wamn/components".to_owned(),
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
        assert_eq!(web_build(&unpacked), unpacked.join("generated/web"));
        assert!(unpacked.join("generated/web/index.html").is_file());
        assert_eq!(
            std::fs::read(
                wamn_schema_generator::output_root(&unpacked).join("required-schema-contract.json")
            )
            .unwrap(),
            b"{\"tables\": []}\n"
        );
        assert!(!unpacked.join("web").exists());
        let repacked = pack_package(&unpacked).unwrap();
        assert_eq!(repacked.digest, packed.digest);
        assert_eq!(repacked.manifest, packed.manifest);
        remove(&root);
    }
}
