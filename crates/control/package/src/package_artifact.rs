//! Package artifacts: the `push-package` verb and the package source of
//! `apply-package` and `publish-release` (docs/operations/building.md#reading-a-pushed-package
//! ).
//!
//! `push-package` verifies an authored package before it pushes anything:
//! the policy contract of `generated/wamn.json` is satisfied, the migrations
//! plan, and every component admits against its built bytes, its declaration
//! and the generated statement facts. A component is one the package owns; `outputs.components` of the package's
//! `build.json` names its bytes. A package without a `build.json` refuses: run `wamn build`.
//! It reads the tag `<package_id>-<version>` first: a tag with another digest
//! refuses, and the same digest is already present, so nothing is pushed
//! again. Then it pushes each component as its own OCI artifact under its
//! digest, pushes the package artifact, and reads its manifest back. No
//! database records the package: the artifact's existence is the proof of its
//! tests (R11).
//!
//! A package artifact is one OCI image manifest with artifact type
//! [`PACKAGE_ARTIFACT_TYPE`], the empty config, and these layers in order:
//! the exact `generated/wamn.json` bytes, then three tars (`migrations`,
//! `publication`, `sources`), then one canonical component descriptor
//! per component, sorted by name. Its tag is `<package_id>-<version>` under an
//! explicit `<registry>/<repository>` base. Its digest is the SHA-256 of the
//! manifest bytes. Each tar has sorted paths, a zero mtime and a fixed owner
//! and mode, so one tree packs to one digest. An unpacked artifact packs again
//! offline to the same digest.

pub(super) mod plan;
pub(crate) use plan::component_connection_names;
use plan::{
    image_manifest, package_coordinate, physical_path, refuse_workflow_declarations, tar_layer,
};

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Component as PathComponent, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, bail, ensure};
use oci_client::Reference;
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
use wamn_catalog::{
    AdmittedComponent, ComponentConnection, ComponentDeclaration, ComponentDescriptor,
    ComponentPackageCoordinate, component_artifact_reference,
};
use wamn_compiler::AUTHORED_MANIFEST;
use wamn_compiler::manifest::{PackageManifest, PolicyContractState};
use wamn_engine::component_artifact::component_artifact_config_bytes;
use wamn_package_artifact::registry_credentials::read_registry_push_credentials;

use crate::component_admission::{AdmitComponentRequest, admit_component, publish_and_verify};
use crate::component_declaration::authored_base_digests;
use wamn_digest::bytes_sha256;
pub use wamn_package_artifact::{
    COMPONENT_DESCRIPTOR_MEDIA_TYPE, NoPackageArtifact, PACKAGE_ARTIFACT_TYPE, PackageRegistry,
    PackedLayer,
};
use wamn_package_artifact::{Repository, ensure_tag};

/// Media type of the three tar layers.
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

/// The tar layers in order: title and the logical package paths each carries.
/// A path that a package does not have is left out, and a layer with no file is
/// an empty tar. A `generated/` path names files of the
/// package's output root (`physical_path`).
pub const PACKAGE_TAR_LAYERS: [(&str, &[&str]); 3] = [
    ("migrations", &["migrations", "history"]),
    ("publication", &["generated/publication"]),
    (
        "sources",
        &[
            AUTHORED_MANIFEST,
            "command",
            "query",
            wamn_compiler::REQUIRED_SCHEMA_CONTRACT_PATH,
            "generated/sql",
            "generated/routes",
        ],
    ),
];

/// Where an unpacked artifact keeps its component descriptors, one
/// `<name>.json` each.
pub const DESCRIPTOR_DIRECTORY: &str = "descriptors";

/// The project that admission writes into a component target. A package
/// artifact is project-free, and a descriptor drops the target.
const ADMISSION_PROJECT_ENV_KEY: &str = "package-artifact";

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
    let descriptors = descriptor_files(root)?
        .into_iter()
        .map(|(descriptor, bytes)| DescriptorFile {
            name: descriptor.component,
            digest: descriptor.component_digest,
            bytes,
        })
        .collect();
    pack_layers(root, descriptors)
}

/// The component descriptors of the unpacked artifact at `root`, sorted by
/// component name: the admitted facts that `publish-release` composes into a
/// release (docs/architecture/components.md#grouping-and-publication).
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

/// Component bytes and declared connections for package installation.
pub(crate) async fn connection_components(
    root: &Path,
    manifest: &PackageManifest,
    source: Option<&wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig>,
) -> anyhow::Result<Vec<(String, Vec<String>, Vec<u8>)>> {
    let descriptors = read_descriptors(root)?;
    if !descriptors.is_empty() {
        use wamn_engine::artifact_source::ArtifactSource as _;
        use wamn_runtime::component_artifact_source::ComponentArtifactSource;
        let target = ComponentPackageCoordinate::new(
            ADMISSION_PROJECT_ENV_KEY,
            &manifest.package.id,
            &manifest.package.version,
        )?;
        let mut result = Vec::new();
        for descriptor in descriptors {
            let names = component_connection_names(
                descriptor
                    .operations
                    .values()
                    .flat_map(|operation| &operation.connections),
            );
            if names.is_empty() {
                continue;
            }
            let source = ComponentArtifactSource::new(
                source
                    .context("set WAMN_COMPONENT_ARTIFACT_BASE for package install")?
                    .clone(),
            )?;
            let facts = descriptor.into_admitted(target.clone());
            let bytes = source.pull_verified(&facts.component).await?;
            result.push((facts.component.component_digest, names, bytes));
        }
        return Ok(result);
    }
    let output = wamn_compiler::output_root(root);
    let build_record = wamn_compiler::build_record::read(&output)?;
    source_components(root, manifest, &build_record)?
        .into_iter()
        .map(|source| {
            let document = crate::component_declaration::render_package_declaration(
                root,
                &source.name,
                ADMISSION_PROJECT_ENV_KEY,
            )?;
            let declaration: ComponentDeclaration = serde_json::from_value(document)?;
            let bytes = std::fs::read(source.file).context("read package component binary")?;
            let names = component_connection_names(
                declaration
                    .operations
                    .values()
                    .flat_map(|operation| &operation.connections),
            );
            Ok((source.sha256, names, bytes))
        })
        .collect()
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
            wamn_digest::canonical_json_bytes(
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
#[cfg(any(test, feature = "test-util"))]
pub fn fixture_package_root(root: &Path) -> anyhow::Result<crate::package_source::PackageRoot> {
    Ok(crate::package_source::PackageRoot {
        path: root.to_owned(),
        artifact_digest: Some(
            wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(root)?,
        ),
    })
}

/// The package artifact digest of the package at `root`, the key of its stage
/// evidence and stage progress (docs/operations/building.md#reading-a-pushed-package): the
/// offline manifest digest of the tree as it is ([`pack_package`]).
pub fn package_artifact_digest(root: &Path) -> anyhow::Result<String> {
    Ok(pack_package(root)?.digest)
}

/// Pack the compiled output with component descriptor layers.
fn pack_layers(root: &Path, mut descriptors: Vec<DescriptorFile>) -> anyhow::Result<PackedPackage> {
    let manifest_path = wamn_compiler::package_manifest_path(root);
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
        let entries = files
            .iter()
            .map(|(name, path)| {
                let data =
                    std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
                Ok((name.clone(), data))
            })
            .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
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
    let manifest = image_manifest(&layers, &annotations, OCI_IMAGE_MEDIA_TYPE);
    Ok(PackedPackage {
        package_id,
        version,
        digest: bytes_sha256(&manifest),
        layers,
        manifest,
    })
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

/// One component named by the compiled package.
#[derive(Debug)]
struct SourceComponent {
    name: String,
    file: PathBuf,
    sha256: String,
}
fn source_components(
    root: &Path,
    manifest: &PackageManifest,
    record: &wamn_compiler::build_record::BuildRecord,
) -> anyhow::Result<Vec<SourceComponent>> {
    let output = wamn_compiler::output_root(root);
    manifest
        .components
        .keys()
        .map(|name| {
            let entry = record
                .outputs
                .components
                .iter()
                .find(|entry| &entry.name == name)
                .with_context(|| format!("build.json records no component {name}"))?;
            Ok(SourceComponent {
                name: name.clone(),
                file: output.join(&entry.path),
                sha256: entry.sha256.clone(),
            })
        })
        .collect()
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

/// An authored package that passed every test of `push-package`, packed.
#[derive(Debug)]
struct VerifiedPackage {
    packed: PackedPackage,
    components: Vec<VerifiedComponent>,
}

/// Every test of `push-package`, before anything is pushed: the policy
/// contract is satisfied, the migrations plan, and every component admits.
fn verify_package(root: &Path) -> anyhow::Result<VerifiedPackage> {
    ensure!(
        root.join(AUTHORED_MANIFEST).is_file(),
        "{} has no {AUTHORED_MANIFEST}; push-package takes an authored package",
        root.display()
    );
    let build_record = wamn_compiler::build_record::read(&wamn_compiler::output_root(root))?;
    let manifest_path = wamn_compiler::package_manifest_path(root);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    refuse_workflow_declarations(&manifest_bytes)?;
    let manifest = PackageManifest::from_slice(&manifest_bytes)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    let contract = &manifest.required_platform_policy_contract;
    ensure!(
        contract.state == PolicyContractState::Satisfied,
        "{}@{} requires platform policy contract {}, which is not satisfied; nothing was pushed",
        manifest.package.id,
        manifest.package.version,
        contract.id
    );
    let directory = crate::package_source::read_package_directory(root)?;
    wamn_catalog::package_schema::package_metadata(&directory)
        .context("plan the package migrations")?;

    let base_digests = authored_base_digests(root).context("read the authored base digests")?;
    let mut descriptors = Vec::new();
    let mut components = Vec::new();
    for source in source_components(root, &manifest, &build_record)? {
        let document = crate::component_declaration::render_package_declaration(
            root,
            &source.name,
            ADMISSION_PROJECT_ENV_KEY,
        )?;
        let platform_packages = OWNED_PLATFORM_PACKAGES.map(str::to_owned).to_vec();
        let mut connections: Vec<ComponentConnection> =
            serde_json::from_value::<ComponentDeclaration>(document.clone())?
                .operations
                .into_values()
                .flat_map(|operation| operation.connections)
                .filter_map(|connection| match connection {
                    wamn_catalog::OperationConnection::Host(connection) => Some(connection),
                    wamn_catalog::OperationConnection::Import(_) => None,
                })
                .collect();
        connections.sort_by(|left, right| left.store_alias.cmp(&right.store_alias));
        connections.dedup();
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
                    .filter_map(|connection| connection.store_alias.name())
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
        let digest = bytes_sha256(&bytes);
        ensure!(
            digest == facts.component_digest && digest == source.sha256,
            "component {} is {digest}, but admission saw {} and build.json records {}; run wamn build again",
            source.name,
            facts.component_digest,
            source.sha256
        );
        let descriptor = admission.descriptor();
        descriptors.push(DescriptorFile {
            name: source.name.clone(),
            digest: digest.clone(),
            bytes: wamn_digest::canonical_json_bytes(
                &serde_json::to_value(&descriptor).context("encode a component descriptor")?,
            ),
        });
        components.push(VerifiedComponent { facts, bytes });
    }
    Ok(VerifiedPackage {
        packed: pack_layers(root, descriptors)?,
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
            .push_blob(&repository.reference, bytes.to_vec(), &bytes_sha256(bytes))
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

/// Make sure of the package, then push its components and its package artifact
/// under `<package_id>-<version>`.
///
/// Every test runs before the first push, and the tag is read before
/// anything is pushed: a tag that holds another digest refuses and names both
/// digests, and a tag that holds the same digest is already present, so
/// nothing is pushed again. Tags are immutable. No database records the
/// package; the artifact's existence is the proof of its tests (R11).
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
        let path = self.root.join(wamn_compiler::COMPILED_MANIFEST);
        std::fs::read(&path).with_context(|| format!("read {}", path.display()))
    }

    /// The package root with the digest this opener holds in memory, the one
    /// carrier of the digest of an opened artifact. The digest is the one the
    /// registry records; a directory has none.
    pub fn package_root(&self) -> crate::package_source::PackageRoot {
        crate::package_source::PackageRoot {
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
/// `generated/` directory as its build output.
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
        &root.join(wamn_compiler::COMPILED_MANIFEST),
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

    /// The completed `build.json` of the package at `root`, with
    /// the components `receiving` and `blob-put` in its build output.
    fn write_build_record(root: &Path) {
        use wamn_compiler::build_record;
        let output = wamn_compiler::output_root(root);
        let components = ["blob-put", "receiving"]
            .map(|name| {
                let path = format!("components/{name}.wasm");
                write(&output, &path, name.as_bytes());
                build_record::BuildComponent {
                    name: name.to_owned(),
                    path,
                    sha256: bytes_sha256(name.as_bytes()),
                }
            })
            .to_vec();
        let build_record = build_record::BuildRecord {
            schema: build_record::BUILD_RECORD_SCHEMA.to_owned(),
            package: build_record::BuildPackage {
                id: "wamn_receiving".to_owned(),
                version: "2.1.0".to_owned(),
            },

            dependencies: Vec::new(),
            generator: build_record::BuildGenerator {
                id: "generator".to_owned(),
                toolchain: "toolchain".to_owned(),
                sqlx: "0.9.0".to_owned(),
            },
            outputs: build_record::BuildOutputs {
                manifest: bytes_sha256(b"manifest"),
                files: BTreeMap::new(),
                components,
            },
        };
        write(
            &output,
            build_record::BUILD_RECORD_FILE,
            &build_record.to_bytes(),
        );
    }

    fn receiving_manifest() -> Vec<u8> {
        std::fs::read(wamn_compiler::package_manifest_path(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving"),
        ))
        .unwrap()
    }

    /// A compiled package fixture with a completed build record.
    fn package_tree(name: &str) -> PathBuf {
        let apps = std::env::temp_dir().join(format!(
            "wamn-package-artifact-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&apps);
        let root = apps.join("wamn_receiving");
        write(&root, "wamn.k", b"# authored\n");
        write(&root, "generated/wamn.json", &receiving_manifest());
        write(
            &root,
            wamn_compiler::REQUIRED_SCHEMA_CONTRACT_PATH,
            b"{\"tables\": []}\n",
        );
        write(&root, "generated/sql/receipt.sql", b"SELECT 1;\n");
        write(&root, "migrations/0001_initial.sql", b"SELECT 1;\n");
        write(&root, "generated/publication/attachments.json", b"{}\n");
        write(&root, "generated/web/index.html", b"<html></html>\n");
        write(&root, "web/dist/stale.html", b"left out\n");
        write(&root, "web/src/main.ts", b"left out\n");
        write(&root, "generated/client/left-out.ts", b"left out\n");
        write_build_record(&root);
        root
    }

    fn remove(root: &Path) {
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    /// A canonical descriptor of the component `receiving`.
    fn descriptor() -> DescriptorFile {
        let digest = bytes_sha256(b"receiving");
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
            bytes: wamn_digest::canonical_json_bytes(&serde_json::to_value(&descriptor).unwrap()),
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
        assert_eq!(first.digest, bytes_sha256(&first.manifest));
        assert_eq!(first.tag(), "wamn_receiving-2.2.11");

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
        for (index, (_, roots)) in PACKAGE_TAR_LAYERS.iter().enumerate() {
            let mut files = BTreeMap::new();
            for relative in *roots {
                collect_files(&root, relative, &mut files).unwrap();
            }
            assert_eq!(paths(index + 1), files.into_keys().collect::<Vec<_>>());
        }

        let manifest: serde_json::Value = serde_json::from_slice(&first.manifest).unwrap();
        assert_eq!(manifest["schemaVersion"], 2);
        assert_eq!(manifest["mediaType"], OCI_IMAGE_MEDIA_TYPE);
        assert_eq!(manifest["artifactType"], PACKAGE_ARTIFACT_TYPE);
        assert_eq!(
            manifest["config"],
            serde_json::json!({
                "mediaType": EMPTY_CONFIG_MEDIA_TYPE,
                "digest": bytes_sha256(b"{}"),
                "size": 2,
            })
        );
        assert_eq!(
            manifest["annotations"],
            serde_json::json!({
                "wamn.package.id": "wamn_receiving",
                "wamn.package.version": "2.2.11",
            })
        );
        remove(&root);
    }

    #[test]
    fn a_changed_file_changes_the_digest() {
        let root = package_tree("changed");
        let first = pack_package(&root).unwrap();
        std::fs::write(root.join("generated/sql/receipt.sql"), b"SELECT 2;\n").unwrap();
        assert_ne!(first.digest, pack_package(&root).unwrap().digest);
        remove(&root);
    }

    #[test]
    fn web_edits_leave_the_package_digest_unchanged() {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let root = scratch.path().join("wamn_receiving");
        write(&root, "wamn.k", b"# package\n");
        write(
            &root,
            "generated/wamn.json",
            br#"{"package":{"id":"wamn_receiving","version":"2.1.0"},"required_platform_policy_contract":{"id":"access","state":"satisfied"},"models":{},"connections":{},"components":{}}"#,
        );
        write(&root, "generated/web/index.html", b"old output\n");
        write(&root, "web/src/main.ts", b"old source\n");
        write_build_record(&root);
        let first = pack_package(&root).unwrap();
        assert_eq!(first.layers.len(), 4, "one manifest and three tar layers");
        std::fs::write(root.join("web/src/main.ts"), b"changed source\n").unwrap();
        std::fs::write(root.join("generated/web/index.html"), b"changed output\n").unwrap();
        let web = wamn_compiler::output_root(&root).join("web-artifact/dist");
        write(&web, "index.html", b"standalone output\n");
        // Record the changed compiled output.
        write_build_record(&root);
        assert_eq!(first.digest, pack_package(&root).unwrap().digest);
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
                credentials: None,
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
        let descriptor = descriptor();
        let packed = pack_layers(&root, vec![descriptor.clone()]).unwrap();
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
        let repacked = pack_package(&unpacked).unwrap();
        assert_eq!(repacked.digest, packed.digest);
        assert_eq!(repacked.manifest, packed.manifest);
        remove(&root);
    }
}
