//! Package artifacts: the `push-package` verb and the package source of
//! `apply-package` (owner rulings of 2026-10-02 on `wamn-zua8.3`).
//!
//! A package artifact is one OCI artifact with one tar layer. The layer holds
//! the authored package as pushed: `wamn.k`, `generated/wamn.json`,
//! `migrations/`, `publication/`, `web/dist/`,
//! `generated/platform-policy/data-access.json`, and the frozen inputs of
//! upgrade qualification, `generated/contracts/`, `generated/sql/` and
//! `generated/package-identity.json`. `push-package` adds
//! `publication/components.json`, the name and SHA-256 of every component that
//! the package owns or that its wirings name, from the build index of
//! `tools/build-components`, and the declaration template of each platform
//! component its wirings name. Its tag is
//! `<package_id>-<version>` under an explicit `<registry>/<repository>` base.
//! The tar has sorted paths, a zero mtime and a fixed owner and mode, so one
//! tree packs to one digest. `catalog.package_artifacts` in the control
//! database records the digest of each package version.

use std::collections::BTreeMap;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, bail, ensure};
use oci_client::client::{Config, ImageLayer};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use tokio_postgres::Client as PgClient;
use wamn_engine::component_artifact::parse_component_artifact_base;
use wamn_runtime::component_artifact_source::read_ca_bundles;
use wamn_runtime::registry_credentials::read_registry_credentials;
use wamn_runtime::release_manifest_artifact::{
    RELEASE_MANIFEST_CONFIG_BYTES, RELEASE_MANIFEST_CONFIG_MEDIA_TYPE,
};
use wamn_schema_generator::{AUTHORED_MANIFEST, COMPILED_MANIFEST, PackageManifest};

use crate::push_release_manifest::{artifact_is_absent, registry_auth, registry_client};

/// OCI artifact type and layer media type of a package artifact.
pub const PACKAGE_ARTIFACT_MEDIA_TYPE: &str = "application/vnd.wamn.package.v1.tar";

/// The package paths the layer carries. `wamn.k` and `generated/wamn.json`
/// are required. A directory that a package does not have is left out.
pub const PACKAGE_ARTIFACT_PATHS: [&str; 10] = [
    AUTHORED_MANIFEST,
    COMPILED_MANIFEST,
    "migrations",
    "publication",
    "web/dist",
    "generated/platform-policy/data-access.json",
    "generated/contracts",
    "generated/sql",
    "generated/package-identity.json",
    COMPONENT_LIST,
];

/// The components the package owns or its wirings name, as
/// `[{"name", "sha256"}]` sorted by name. `push-package` writes it into the
/// layer, never into the package tree.
pub const COMPONENT_LIST: &str = "publication/components.json";

/// The build index that `tools/build-components` writes for the packages under
/// `apps/`, relative to a package root.
pub const COMPONENT_INDEX: &str = "../target/components.json";

/// The platform component declarations, relative to a package root under
/// `apps/`. Each `<area>/<crate>/declaration.json.in` names its component.
pub const PLATFORM_DIRECTORY: &str = "../platform";

/// One entry of [`COMPONENT_LIST`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ListedComponent {
    /// The component name of `generated/wamn.json` or of a wiring node.
    pub name: String,
    /// Lowercase hex SHA-256 of the built `.wasm` file.
    pub sha256: String,
}

/// One entry of the build index [`COMPONENT_INDEX`].
#[derive(Debug, serde::Deserialize)]
struct IndexedComponent {
    name: String,
    file: PathBuf,
    sha256: String,
    recorded_at: u64,
}

/// One package tree packed into its layer.
#[derive(Debug)]
pub struct PackedPackage {
    /// Package id from `generated/wamn.json`.
    pub package_id: String,
    /// Package version from `generated/wamn.json`.
    pub version: String,
    /// The tar layer.
    pub bytes: Vec<u8>,
    /// `sha256:<hex>` of the layer.
    pub digest: String,
}

impl PackedPackage {
    /// The artifact tag, `<package_id>-<version>`.
    pub fn tag(&self) -> String {
        format!("{}-{}", self.package_id, self.version)
    }
}

/// Pack the package at `root` into its deterministic tar layer.
///
/// With `index`, the build index of `tools/build-components`, the layer gains
/// [`COMPONENT_LIST`] for every component that `generated/wamn.json` or a
/// wiring of `publication/wirings/` names, and the declaration template of
/// each platform component as `publication/components/<name>.json.in`. A
/// missing index, a name without an entry, and an entry whose file is missing
/// or newer than its record refuse. Without it, the tree packs as it is,
/// which is how an unpacked artifact is checked.
pub fn pack_package(root: &Path, index: Option<&Path>) -> anyhow::Result<PackedPackage> {
    ensure!(
        root.join(AUTHORED_MANIFEST).is_file(),
        "{} has no {AUTHORED_MANIFEST}; push-package takes an authored package",
        root.display()
    );
    let manifest_path = root.join(COMPILED_MANIFEST);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest = PackageManifest::from_slice(&manifest_bytes)
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    let mut files = BTreeMap::new();
    for path in PACKAGE_ARTIFACT_PATHS {
        collect_files(root, path, &mut files)?;
    }
    let mut entries = files
        .iter()
        .map(|(name, path)| {
            let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
            Ok((name.clone(), data))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    if let Some(index) = index {
        ensure!(
            !entries.contains_key(COMPONENT_LIST),
            "{} has {COMPONENT_LIST}; push-package writes it",
            root.display()
        );
        let wired = wiring_components(root)?;
        let platform = wired
            .iter()
            .filter(|name| !manifest.components.contains_key(*name))
            .collect::<Vec<_>>();
        for name in &platform {
            let template = format!("publication/components/{name}.json.in");
            ensure!(
                !entries.contains_key(&template),
                "{} has {template}; push-package copies the platform template",
                root.display()
            );
            entries.insert(template, platform_template(root, name)?);
        }
        let names = manifest
            .components
            .keys()
            .chain(platform.iter().copied())
            .collect::<std::collections::BTreeSet<_>>();
        let listed = indexed_components(index, &names)?;
        let mut list = serde_json::to_vec_pretty(&listed).context("encode the component list")?;
        list.push(b'\n');
        entries.insert(COMPONENT_LIST.to_owned(), list);
    }
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in &entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        builder
            .append_data(&mut header, name, data.as_slice())
            .with_context(|| format!("add {name} to the package layer"))?;
    }
    let bytes = builder.into_inner().context("finish the package layer")?;
    let packed = PackedPackage {
        package_id: manifest.package.id,
        version: manifest.package.version,
        digest: sha256_digest(&bytes),
        bytes,
    };
    ensure_tag(&packed.tag())?;
    Ok(packed)
}

/// The component names of the nodes of every wiring in `publication/wirings/`.
fn wiring_components(root: &Path) -> anyhow::Result<std::collections::BTreeSet<String>> {
    let directory = root.join("publication/wirings");
    let mut names = std::collections::BTreeSet::new();
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
            names.insert(component.to_owned());
        }
    }
    Ok(names)
}

/// The declaration template of the platform component `name`: the one
/// `declaration.json.in` under [`PLATFORM_DIRECTORY`] whose `component` is
/// `name`.
fn platform_template(root: &Path, name: &str) -> anyhow::Result<Vec<u8>> {
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
                found.push(bytes);
            }
        }
    }
    match <[Vec<u8>; 1]>::try_from(found) {
        Ok([bytes]) => Ok(bytes),
        Err(found) => bail!(
            "{} platform declarations name component {name}, not one",
            found.len()
        ),
    }
}

/// The entries of the build index at `index` for `names`. A missing index, a
/// name without an entry, and an entry whose file is missing or newer than its
/// record refuse.
fn indexed_components(
    index: &Path,
    names: &std::collections::BTreeSet<&String>,
) -> anyhow::Result<Vec<ListedComponent>> {
    let bytes = std::fs::read(index).with_context(|| {
        format!(
            "read the build index {}; run tools/build-components first",
            index.display()
        )
    })?;
    let indexed: Vec<IndexedComponent> = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse the build index {}", index.display()))?;
    names
        .iter()
        .map(|name| {
            let entry = indexed
                .iter()
                .find(|entry| &entry.name == *name)
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
            Ok(ListedComponent {
                name: (*name).clone(),
                sha256: entry.sha256.clone(),
            })
        })
        .collect()
}

/// Add every regular file under `relative` to `files`, keyed by its path
/// relative to `root` with `/` separators.
fn collect_files(
    root: &Path,
    relative: &str,
    files: &mut BTreeMap<String, PathBuf>,
) -> anyhow::Result<()> {
    let path = root.join(relative);
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

    /// The manifest of the tag, or `None` when the tag does not exist.
    async fn manifest(&self) -> anyhow::Result<Option<OciImageManifest>> {
        match self
            .client
            .pull_image_manifest(&self.reference, &self.auth)
            .await
        {
            Ok((manifest, _)) => Ok(Some(manifest)),
            Err(error) if artifact_is_absent(&error) => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("read package artifact {}", self.reference))
            }
        }
    }

    /// The layer digest of the tag, or `None` when the tag does not exist.
    async fn layer_digest(&self) -> anyhow::Result<Option<String>> {
        let Some(manifest) = self.manifest().await? else {
            return Ok(None);
        };
        Ok(Some(self.verified_layer(&manifest)?.digest.clone()))
    }

    fn verified_layer<'a>(
        &self,
        manifest: &'a OciImageManifest,
    ) -> anyhow::Result<&'a oci_client::manifest::OciDescriptor> {
        ensure!(
            manifest.artifact_type.as_deref() == Some(PACKAGE_ARTIFACT_MEDIA_TYPE)
                && manifest.layers.len() == 1
                && manifest.layers[0].media_type == PACKAGE_ARTIFACT_MEDIA_TYPE,
            "{} is not a package artifact with one {PACKAGE_ARTIFACT_MEDIA_TYPE} layer",
            self.reference
        );
        Ok(&manifest.layers[0])
    }

    async fn push(&self, bytes: &[u8]) -> anyhow::Result<()> {
        let layer = ImageLayer::new(bytes.to_vec(), PACKAGE_ARTIFACT_MEDIA_TYPE.to_owned(), None);
        let config = Config::new(
            RELEASE_MANIFEST_CONFIG_BYTES.to_vec(),
            RELEASE_MANIFEST_CONFIG_MEDIA_TYPE.to_owned(),
            None,
        );
        let mut manifest = OciImageManifest::build(std::slice::from_ref(&layer), &config, None);
        manifest.media_type = Some(OCI_IMAGE_MEDIA_TYPE.to_owned());
        manifest.artifact_type = Some(PACKAGE_ARTIFACT_MEDIA_TYPE.to_owned());
        self.client
            .push(
                &self.reference,
                std::slice::from_ref(&layer),
                config,
                &self.auth,
                Some(manifest),
            )
            .await
            .with_context(|| format!("push package artifact {}", self.reference))?;
        Ok(())
    }

    async fn pull_layer(&self, digest: &str) -> anyhow::Result<Vec<u8>> {
        let manifest = self
            .manifest()
            .await?
            .with_context(|| format!("package artifact {} does not exist", self.reference))?;
        let layer = self.verified_layer(&manifest)?;
        ensure!(
            layer.digest == digest,
            "package artifact {} has digest {}, but catalog.package_artifacts records {digest}",
            self.reference,
            layer.digest
        );
        let mut bytes = Vec::new();
        self.client
            .pull_blob(&self.reference, layer, &mut bytes)
            .await
            .with_context(|| format!("pull package artifact {}", self.reference))?;
        ensure!(
            sha256_digest(&bytes) == digest,
            "the layer of package artifact {} does not hash to {digest}",
            self.reference
        );
        Ok(bytes)
    }
}

/// Inputs of one `push-package` run.
#[derive(Debug)]
pub struct PushPackageRequest {
    /// Root of the authored package.
    pub package: PathBuf,
    /// Registry and control database.
    pub registry: PackageRegistry,
    /// The source commit recorded with the artifact.
    pub source_commit: Option<String>,
}

/// Whether the run pushed the artifact or found the same bytes under its tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagePushDisposition {
    /// The run pushed the artifact.
    Pushed,
    /// The tag already held the same layer.
    AlreadyPresent,
}

/// Result of one `push-package` run.
#[derive(Debug)]
pub struct PushedPackage {
    /// The artifact tag.
    pub tag: String,
    /// `sha256:<hex>` of the layer.
    pub digest: String,
    /// Whether the run pushed.
    pub disposition: PackagePushDisposition,
}

/// Pack the package, push it under `<package_id>-<version>`, and record its
/// digest in `catalog.package_artifacts`.
///
/// The verb reads the control record before the push. A recorded or pushed
/// tag with another digest refuses and names both digests. The same bytes
/// push nothing and leave the record unchanged.
pub async fn push_package(request: &PushPackageRequest) -> anyhow::Result<PushedPackage> {
    if let Some(source_commit) = &request.source_commit {
        ensure!(
            !source_commit.is_empty() && !source_commit.chars().any(char::is_whitespace),
            "source commit must be one nonempty value"
        );
    }
    let packed = pack_package(
        &request.package,
        Some(&request.package.join(COMPONENT_INDEX)),
    )?;
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
            let disposition = match repository.layer_digest().await? {
                Some(existing) if existing == packed.digest => {
                    PackagePushDisposition::AlreadyPresent
                }
                Some(existing) => bail!(
                    "package artifact {} holds {existing}, but the package packs to {}; tags are immutable",
                    repository.reference,
                    packed.digest
                ),
                None => {
                    repository.push(&packed.bytes).await?;
                    ensure!(
                        repository.layer_digest().await?.as_deref() == Some(packed.digest.as_str()),
                        "pushed package artifact {} is not readable",
                        repository.reference
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

/// Open a package source. An artifact is fetched only when its digest equals
/// the digest that `catalog.package_artifacts` records, and is unpacked into a
/// private temporary directory.
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
    let repository = Repository::open(&registry, &tag)?;
    let bytes =
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
            let digest: String = row.get(0);
            repository.pull_layer(&digest).await
        })
        .await?;

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
    tar::Archive::new(bytes.as_slice())
        .unpack(opened.root())
        .with_context(|| format!("unpack package artifact {tag}"))?;
    let unpacked = pack_package(opened.root(), None)?;
    ensure!(
        unpacked.tag() == tag,
        "package artifact {tag} holds package {}",
        unpacked.tag()
    );
    Ok(opened)
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
        let manifest = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../apps/wamn_receiving/generated/wamn.json"),
        )
        .unwrap();
        write(&root, "wamn.k", b"# authored\n");
        write(&root, "generated/wamn.json", &manifest);
        write(&root, "generated/platform-policy/data-access.json", b"{}\n");
        write(&root, "generated/contracts/receipt.json", b"{}\n");
        write(&root, "generated/sql/receipt.sql", b"SELECT 1;\n");
        write(&root, "generated/package-identity.json", b"{}\n");
        write(&root, "migrations/0001_initial.sql", b"SELECT 1;\n");
        write(&root, "publication/attachments.json", b"{}\n");
        write(
            &root,
            "publication/wirings/store.json",
            b"{\"nodes\": {\"a\": {\"component\": \"receiving\"}, \"b\": {\"component\": \"blob-put\"}}}\n",
        );
        write(&root, "web/dist/index.html", b"<html></html>\n");
        write(&root, "web/src/main.ts", b"left out\n");
        write(&root, "generated/client/left-out.ts", b"left out\n");
        root
    }

    fn pack(root: &Path) -> anyhow::Result<PackedPackage> {
        pack_package(root, Some(&root.join(COMPONENT_INDEX)))
    }

    fn remove(root: &Path) {
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    fn sha256(bytes: &[u8]) -> String {
        hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes))
    }

    #[test]
    fn two_packs_of_the_same_tree_give_one_digest() {
        let root = package_tree("same");
        let first = pack(&root).unwrap();
        // A newer mtime does not change the layer.
        std::fs::write(root.join("migrations/0001_initial.sql"), b"SELECT 1;\n").unwrap();
        let second = pack(&root).unwrap();
        assert_eq!(first.digest, second.digest);
        assert_eq!(first.tag(), "wamn_receiving-2.1.0");

        let mut archive = tar::Archive::new(first.bytes.as_slice());
        let mut contents = BTreeMap::new();
        let entries: Vec<(String, u32, u64, u64)> = archive
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap().to_string_lossy().into_owned();
                let mut data = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut data).unwrap();
                contents.insert(path.clone(), data);
                let header = entry.header();
                (
                    path,
                    header.mode().unwrap(),
                    header.uid().unwrap(),
                    header.mtime().unwrap(),
                )
            })
            .collect();
        let paths = [
            "generated/contracts/receipt.json",
            "generated/package-identity.json",
            "generated/platform-policy/data-access.json",
            "generated/sql/receipt.sql",
            "generated/wamn.json",
            "migrations/0001_initial.sql",
            "publication/attachments.json",
            "publication/components.json",
            "publication/components/blob-put.json.in",
            "publication/wirings/store.json",
            "wamn.k",
            "web/dist/index.html",
        ];
        assert_eq!(entries, paths.map(|path| (path.to_owned(), 0o644, 0, 0)));
        assert_eq!(
            String::from_utf8(contents[COMPONENT_LIST].clone()).unwrap(),
            format!(
                "[\n  {{\n    \"name\": \"blob-put\",\n    \"sha256\": \"{}\"\n  }},\n  {{\n    \"name\": \"receiving\",\n    \"sha256\": \"{}\"\n  }}\n]\n",
                sha256(b"blob-put"),
                sha256(b"receiving")
            )
        );
        assert_eq!(
            contents["publication/components/blob-put.json.in"],
            b"{\"component\": \"blob-put\"}\n"
        );
        remove(&root);
    }

    #[test]
    fn a_changed_file_changes_the_digest() {
        let root = package_tree("changed");
        let first = pack(&root).unwrap();
        std::fs::write(root.join("web/dist/index.html"), b"<html>2</html>\n").unwrap();
        assert_ne!(first.digest, pack(&root).unwrap().digest);
        remove(&root);
    }

    #[test]
    fn the_build_index_refuses_a_missing_or_stale_component() {
        let root = package_tree("index");
        let apps = root.parent().unwrap().to_path_buf();

        std::fs::remove_file(apps.join("target/built/receiving.wasm")).unwrap();
        let error = format!("{:#}", pack(&root).unwrap_err());
        assert!(
            error.contains("component receiving has no built file"),
            "{error}"
        );

        write(&apps, "target/built/receiving.wasm", b"receiving");
        write_index(&apps, 0);
        let error = format!("{:#}", pack(&root).unwrap_err());
        assert!(
            error.contains("newer than its build index record"),
            "{error}"
        );

        write(
            &root,
            "publication/wirings/shape.json",
            b"{\"nodes\": {\"a\": {\"component\": \"label-render\"}}}\n",
        );
        let error = format!("{:#}", pack(&root).unwrap_err());
        assert!(
            error.contains("0 platform declarations name component label-render"),
            "{error}"
        );

        std::fs::remove_file(root.join("publication/wirings/shape.json")).unwrap();
        std::fs::remove_file(apps.join("target/components.json")).unwrap();
        let error = format!("{:#}", pack(&root).unwrap_err());
        assert!(
            error.contains("run tools/build-components first"),
            "{error}"
        );
        remove(&root);
    }

    #[test]
    fn an_unpacked_artifact_packs_to_its_own_digest() {
        let root = package_tree("unpacked");
        let packed = pack(&root).unwrap();
        let unpacked = root.parent().unwrap().join("unpacked");
        tar::Archive::new(packed.bytes.as_slice())
            .unpack(&unpacked)
            .unwrap();
        assert_eq!(pack_package(&unpacked, None).unwrap().digest, packed.digest);
        let error = format!("{:#}", pack(&unpacked).unwrap_err());
        assert!(error.contains("push-package writes it"), "{error}");
        remove(&root);
    }
}
