//! Package artifacts contain the published `wamn.json` and a migrations tar.
//! Publication admits the build's components and pins the stock OCI images.

pub(super) mod plan;
use wamn_package::manifest::deploy::PublishedComponent;
use wamn_package::manifest::PolicyContractState;
use wamn_package::PackageCoordinate;
use plan::{
    image_manifest, package_coordinate, refuse_workflow_declarations, tar_layer, validate_append,
};

use std::collections::BTreeMap;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, bail, ensure};
use oci_client::Reference;
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
use wamn_oci_reference::component_artifact_reference;
use wamn_component_policy::ComponentDescriptor;
use wamn_component_policy::PackageManifest;
use wamn_digest::Digest;
use wamn_oci::package_artifact::registry_credentials::read_registry_credentials;

use crate::component_admission::admit;
use crate::component_declaration::package_declaration;
use wamn_digest::bytes_sha256;
use wamn_oci::package_artifact::{CREATED_ANNOTATION, PulledPackage, Repository, is_dev_tag};
pub use wamn_oci::package_artifact::{
    NoPackageArtifact, PACKAGE_ARTIFACT_TYPE, PACKAGE_MANIFEST_MEDIA_TYPE, PACKAGE_TAR_MEDIA_TYPE,
    PackageRegistry, PackedLayer,
};

const EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
const EMPTY_CONFIG: &[u8] = b"{}";
const TITLE_ANNOTATION: &str = "org.opencontainers.image.title";
const CREATED_FILE: &str = "artifact-created-at";
const PACKAGE_ID_ANNOTATION: &str = "wamn.package.id";
const PACKAGE_VERSION_ANNOTATION: &str = "wamn.package.version";
const MANIFEST_TITLE: &str = "wamn.json";
const MIGRATIONS_TITLE: &str = "migrations";
const MIGRATION_PATHS: [&str; 2] = ["migrations", "history"];

/// The platform packages every package-owned component is admitted with,
/// beside the packages of its own declared connections.
const OWNED_PLATFORM_PACKAGES: [&str; 2] = ["wamn:node", "wamn:postgres"];

/// One package tree packed into its layers and image manifest.
#[derive(Debug)]
pub struct PackedPackage {
    /// Package id from the package manifest.
    pub package_id: wamn_package::PackageName,
    /// Package version from the package manifest.
    pub version: wamn_package::package_version::PackageVersion,
    /// The layers in order.
    pub layers: Vec<PackedLayer>,
    /// The image manifest bytes.
    pub manifest: Vec<u8>,
    /// `sha256:<hex>` of the image manifest bytes: the artifact digest.
    pub digest: wamn_digest::Digest,
}

impl PackedPackage {
    /// The artifact tag, `<package_id>-<version>`.
    ///
    /// # Errors
    /// Refuses a coordinate that cannot be used as an OCI tag.
    pub fn tag(
        &self,
    ) -> Result<
        wamn_oci::package_artifact::tag::OciTag,
        wamn_oci::package_artifact::tag::ParseOciTagError,
    > {
        format!("{}-{}", self.package_id, self.version).parse()
    }
}

/// Pack an unpacked artifact offline, preserving its development timestamp.
pub fn pack_package(root: &Path) -> anyhow::Result<PackedPackage> {
    let path = wamn_codegen::package_manifest_path(root);
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let mut packed = pack_layers(bytes, migrations_layer(&migration_files(root)?)?)?;
    if is_dev_tag(&packed.tag()?) && root.join(CREATED_FILE).is_file() {
        stamp_created(
            &mut packed,
            &std::fs::read_to_string(root.join(CREATED_FILE))?,
        )?;
    }
    Ok(packed)
}

fn stamp_created(packed: &mut PackedPackage, created: &str) -> anyhow::Result<()> {
    chrono::DateTime::parse_from_rfc3339(created).context("invalid dev publication time")?;
    let mut manifest: serde_json::Value = serde_json::from_slice(&packed.manifest)?;
    manifest["annotations"][CREATED_ANNOTATION] = created.into();
    packed.manifest = wamn_digest::canonical_json_bytes(&manifest);
    packed.digest = bytes_sha256(&packed.manifest);
    Ok(())
}

/// Read the exact authored and generated SQL files that the package carries.
fn migration_files(root: &Path) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    let root = crate::package_source::migration_root(root);
    let mut files = BTreeMap::new();
    for path in MIGRATION_PATHS {
        collect_files(&root, path, &mut files)?;
    }
    files
        .into_iter()
        .map(|(name, path)| {
            let data = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            Ok((name, data))
        })
        .collect()
}

fn migrations_layer(entries: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<PackedLayer> {
    Ok(PackedLayer {
        media_type: PACKAGE_TAR_MEDIA_TYPE.to_owned(),
        annotations: BTreeMap::from([(TITLE_ANNOTATION.to_owned(), MIGRATIONS_TITLE.to_owned())]),
        bytes: tar_layer(MIGRATIONS_TITLE, entries)?,
    })
}

/// Pack the published package document with its migrations tar.
fn pack_layers(manifest_bytes: Vec<u8>, migrations: PackedLayer) -> anyhow::Result<PackedPackage> {
    let (package_id, version) = package_coordinate(&manifest_bytes)?;
    let layers = vec![
        PackedLayer {
            media_type: PACKAGE_MANIFEST_MEDIA_TYPE.to_owned(),
            annotations: BTreeMap::from([(TITLE_ANNOTATION.to_owned(), MANIFEST_TITLE.to_owned())]),
            bytes: manifest_bytes,
        },
        migrations,
    ];
    let annotations = BTreeMap::from([
        (PACKAGE_ID_ANNOTATION.to_owned(), package_id.to_string()),
        (PACKAGE_VERSION_ANNOTATION.to_owned(), version.to_string()),
    ]);
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

/// One admitted component and its exact bytes, before the registry assigns its image digest.
#[derive(Debug)]
pub struct VerifiedComponent {
    /// The admitted component descriptor.
    pub descriptor: ComponentDescriptor,
    bytes: Box<[u8]>,
}

/// A finished build checked offline, before any registry call.
#[derive(Debug)]
pub struct VerifiedPackage {
    coordinate: PackageCoordinate,
    /// The compiled manifest; admission has taken out its declarations.
    pub manifest: PackageManifest,
    /// The admitted components in manifest order.
    pub components: Vec<VerifiedComponent>,
    migrations: BTreeMap<String, Vec<u8>>,
}

/// Check a finished build offline and admit its components.
///
/// The compiled manifest, each component and each migration file must match
/// the fingerprint of the finished build.
///
/// # Errors
/// When the build is not finished or differs from its fingerprint, or a
/// component is refused.
pub fn verify_package(root: &Path) -> anyhow::Result<VerifiedPackage> {
    let output = wamn_codegen::output_root(root);
    let finished = wamn_codegen::fingerprint::read(&output)?;
    let directory = crate::package_source::read_package_directory(root)?;
    wamn_codegen::fingerprint::verify_digest(
        &output,
        &finished,
        Path::new("wamn.json"),
        bytes_sha256(&directory.manifest_bytes),
    )?;
    refuse_workflow_declarations(&directory.manifest_bytes)?;
    let coordinate = wamn_package::package_schema::package_metadata::<
        wamn_component_policy::ComponentDescriptor,
    >(&directory)
    .context("plan the package migrations")?
    .coordinate;
    let mut manifest = PackageManifest::from_slice(&directory.manifest_bytes)
        .context("read the compiled package manifest")?;
    drop(directory);
    let components = admit_components(&output, &finished, &mut manifest)?;
    let migrations = migration_files(root)?;
    let expected_migrations = finished
        .outputs
        .keys()
        .filter(|path| {
            MIGRATION_PATHS
                .iter()
                .any(|directory| path.starts_with(directory))
        })
        .count();
    ensure!(
        expected_migrations == migrations.len(),
        "migration files differ from the finished build"
    );
    for (path, bytes) in &migrations {
        wamn_codegen::fingerprint::verify_digest(
            &output,
            &finished,
            Path::new(path),
            bytes_sha256(bytes),
        )?;
    }
    Ok(VerifiedPackage {
        coordinate,
        manifest,
        components,
        migrations,
    })
}

fn admit_components(
    output: &Path,
    finished: &wamn_codegen::fingerprint::Fingerprint,
    manifest: &mut PackageManifest,
) -> anyhow::Result<Vec<VerifiedComponent>> {
    let contract = &manifest.required_platform_policy_contract;
    ensure!(
        contract.state == PolicyContractState::Satisfied,
        "{}@{} requires platform policy contract {}, which is not satisfied",
        manifest.package.id,
        manifest.package.version,
        contract.id
    );
    let base_digests = manifest
        .base_dependencies
        .values()
        .map(|base| base.digest)
        .collect::<std::collections::BTreeSet<_>>();
    let mut components = Vec::new();
    let names = manifest
        .components
        .keys()
        .cloned()
        .map(Into::into)
        .collect::<Vec<wamn_package::canonical_name::CanonicalName>>();
    for name in names {
        let target = PackageCoordinate::new(
            manifest.package.id.clone(),
            manifest.package.version.clone(),
        )?;
        let declaration = package_declaration(manifest, &name, target)?;
        let path = wamn_build::component_path(name.as_str());
        let file = output.join(&path);
        let bytes = std::fs::read(&file).with_context(|| format!("read {}", file.display()))?;
        let connection_packages = manifest.components[name.as_str()]
            .connections
            .iter()
            .filter_map(|connection| manifest.connections.get(connection))
            .filter_map(|declared| declared.interface.split_once('/'))
            .map(|(package, _)| package);
        let (descriptor, _) = admit(
            manifest,
            &bytes,
            declaration,
            OWNED_PLATFORM_PACKAGES
                .into_iter()
                .chain(connection_packages)
                .map(str::to_owned)
                .collect(),
            base_digests.clone(),
        )
        .with_context(|| format!("admit component {name}"))?;
        wamn_codegen::fingerprint::verify_digest(
            output, finished, &path, descriptor.component_digest,
        )?;
        if !manifest.connections.is_empty() {
            let declaration = &manifest.components[name.as_str()];
            crate::connection_requirement::validate_connections(
                &bytes,
                manifest,
                &declaration.connections,
            )?;
        }
        components.push(VerifiedComponent {
            descriptor,
            bytes: bytes.into_boxed_slice(),
        });
    }
    ensure!(
        manifest
            .deploy
            .as_ref()
            .is_some_and(|deploy| deploy.declarations.is_empty()),
        "compiled declarations do not match the package components"
    );
    Ok(components)
}

/// Push every blob of `packed` to `repository`, then its manifest bytes.
async fn push_artifact(repository: &Repository, packed: PackedPackage) -> anyhow::Result<()> {
    for bytes in packed
        .layers
        .into_iter()
        .map(|layer| layer.bytes)
        .chain([EMPTY_CONFIG.to_vec()])
    {
        let digest = bytes_sha256(&bytes).to_string();
        repository
            .client
            .push_blob(&repository.reference, bytes, &digest)
            .await
            .with_context(|| format!("push a blob of package artifact {}", repository.reference))?;
    }
    repository
        .client
        .push_manifest_raw(
            &repository.reference,
            packed.manifest,
            hyper::header::HeaderValue::from_static(OCI_IMAGE_MEDIA_TYPE),
        )
        .await
        .with_context(|| format!("push package artifact {}", repository.reference))?;
    Ok(())
}

/// Inputs of one `push-package` run.
#[derive(Debug)]
pub struct PushPackageRequest {
    /// The source package root or its finished build directory.
    pub package: PathBuf,
    /// The package registry.
    pub registry: PackageRegistry,
    /// Explicit `<registry>/<repository>` base for component artifacts, in the
    /// registry of `registry`.
    pub component_artifact_base: String,
}

/// Published identity and byte pins that later packages in this push can reuse.
#[derive(Debug)]
pub struct PushedPackage {
    pub coordinate: PackageCoordinate,
    pub digest: Digest,
    images: BTreeMap<Digest, Digest>,
}

/// Publish a verified build and return its OCI digest.
///
/// A tag names the latest published digest. The same digest is a no-op. Before
/// replacing a tag, check that its current migrations remain an unchanged
/// prefix. SQLx still refuses checksum changes when it applies migrations.
/// Bases and components already published by this operation supply their
/// digests directly.
pub async fn push_package(
    request: &PushPackageRequest,
    bases: &BTreeMap<PackageCoordinate, PushedPackage>,
) -> anyhow::Result<PushedPackage> {
    let VerifiedPackage {
        coordinate,
        mut manifest,
        components,
        migrations: files,
    } = verify_package(&request.package)?;
    let mut base_packages = BTreeMap::new();
    for (alias, base) in &manifest.base_dependencies {
        let coordinate = PackageCoordinate::new(base.package.clone(), base.version.clone())?;
        let published = bases.get(&coordinate).with_context(|| {
            format!("publish base {coordinate:?} before the package that uses it")
        })?;
        ensure!(
            published.images.len() == 1 && published.images.contains_key(&base.digest),
            "base {coordinate:?} does not supply its pinned component {}",
            base.digest
        );
        base_packages.insert(alias.clone(), published.digest);
    }
    let tag = format!(
        "{}-{}",
        coordinate.package_id(),
        coordinate.package_version()
    )
    .parse::<wamn_oci::package_artifact::tag::OciTag>()?;
    let repository = Repository::open(&request.registry, &tag)?;
    let migrations = migrations_layer(&files)?;
    let (existing, created) = if let Some((raw, digest)) = repository.manifest().await? {
        let (previous, previous_created) = repository
            .predecessor_migrations(&raw, &bytes_sha256(&migrations.bytes))
            .await?;
        if let Some(previous) = previous {
            validate_append(&previous.bytes, &files)
                .with_context(|| format!("append check for package tag {tag}"))?;
        }
        (Some(digest), previous_created)
    } else {
        (None, None)
    };
    drop(files);
    let created = if is_dev_tag(&tag) {
        Some(if existing.is_some() {
            created.context("dev package has no publication time")?
        } else {
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
    } else {
        None
    };

    let deploy = manifest
        .deploy
        .as_mut()
        .context("build the package before publishing it")?;
    deploy.base_packages = base_packages;
    deploy.components.clear();
    let mut images = BTreeMap::new();
    for component in components {
        let descriptor = &component.descriptor;
        let digest = descriptor.component_digest;
        let known = images.get(&digest).copied().or_else(|| {
            bases
                .values()
                .find_map(|package| package.images.get(&digest).copied())
        });
        let known = if known.is_none() && existing.is_some() {
            component_image(&request.registry, &request.component_artifact_base, &digest).await?
        } else {
            known
        };
        let image_digest = if let Some(image) = known {
            image
        } else {
            push_component_artifact(
                &request.registry,
                &request.component_artifact_base,
                &component,
            )
            .await?
        };
        images.insert(digest, image_digest);
        deploy.components.insert(
            descriptor.component.clone(),
            PublishedComponent {
                descriptor: component.descriptor,
                image_digest,
            },
        );
    }
    validate_published(&manifest)?;
    let mut packed = pack_layers(
        wamn_digest::canonical_json_bytes(&serde_json::to_value(manifest)?),
        migrations,
    )?;
    if let Some(created) = created {
        stamp_created(&mut packed, &created)?;
    }
    let digest = packed.digest;
    if existing != Some(digest) {
        push_artifact(&repository, packed).await?;
    }
    Ok(PushedPackage {
        coordinate,
        digest,
        images,
    })
}

/// Check that a published manifest supplies exactly the deploy pins it declares.
fn validate_published(manifest: &PackageManifest) -> anyhow::Result<()> {
    let deploy = manifest
        .deploy
        .as_ref()
        .context("package has no compiled deploy section")?;
    ensure!(
        deploy.declarations.is_empty(),
        "published package carries unadmitted declarations"
    );
    ensure!(
        manifest
            .components
            .keys()
            .map(wamn_package::manifest::source_name::SourceName::as_str)
            .eq(deploy
                .components
                .keys()
                .map(wamn_package::canonical_name::CanonicalName::as_str))
            && deploy
                .components
                .iter()
                .all(|(name, component)| *name == component.descriptor.component),
        "published component pins do not match package components"
    );
    ensure!(
        manifest
            .base_dependencies
            .keys()
            .eq(deploy.base_packages.keys()),
        "published base pins do not match package dependencies"
    );
    Ok(())
}

/// Read an existing stock image pin by its byte tag before publishing a replacement.
async fn component_image(
    registry: &PackageRegistry,
    artifact_base: &str,
    digest: &Digest,
) -> anyhow::Result<Option<Digest>> {
    let artifact = component_artifact_reference(artifact_base, digest)?;
    let credentials = read_registry_credentials(&registry.registry_auth_file, artifact.registry())?;
    let transport = PackageRegistry {
        artifact_base: artifact_base.to_owned(),
        credentials: None,
        registry_auth_file: PathBuf::new(),
        insecure_registry: registry.insecure_registry,
        oci_ca_paths: registry.oci_ca_paths.clone(),
    };
    let tag: wamn_oci::package_artifact::tag::OciTag = artifact.tag().parse()?;
    let auth = wamn_oci::package_artifact::registry_auth(credentials.as_ref());
    let repository = Repository::with_auth(&transport, &tag, auth)?;
    let Some((bytes, image)) = repository.manifest().await? else {
        return Ok(None);
    };
    let manifest: oci_client::manifest::OciImageManifest = serde_json::from_slice(&bytes)?;
    ensure!(
        matches!(manifest.layers.as_slice(), [layer]
            if layer.media_type == "application/wasm"
                && layer.digest.parse::<Digest>().ok().as_ref() == Some(digest)),
        "component image {artifact} differs from its byte pin"
    );
    Ok(Some(image))
}

/// Push one admitted component as a stock application/wasm artifact under its byte digest.
async fn push_component_artifact(
    registry: &PackageRegistry,
    artifact_base: &str,
    component: &VerifiedComponent,
) -> anyhow::Result<Digest> {
    let artifact =
        component_artifact_reference(artifact_base, &component.descriptor.component_digest)
            .context("derive the component artifact reference")?;
    let reference = Reference::with_tag(
        artifact.registry().to_owned(),
        artifact.repository().to_owned(),
        artifact.tag().to_owned(),
    );
    let credentials = read_registry_credentials(&registry.registry_auth_file, artifact.registry())
        .context("load the component registry push credential")?;
    wash_runtime::oci::set_extra_ca_certificates(&registry.oci_ca_paths)?;
    let digest = wash_runtime::oci::push_component(
        &reference.to_string(),
        &component.bytes,
        wash_runtime::oci::OciConfig {
            credentials: credentials.as_ref().map(|credentials| {
                (
                    credentials.username().to_owned(),
                    credentials.password().to_owned(),
                )
            }),
            insecure: registry.insecure_registry,
            ..Default::default()
        },
        None,
    )
    .await?;
    Ok(digest.parse()?)
}

/// An opened package source. An unpacked artifact lives in a private
/// temporary directory that is removed when this value drops.
#[derive(Debug)]
pub struct OpenedPackage {
    root: PathBuf,
    digest: Option<wamn_digest::Digest>,
}

impl OpenedPackage {
    /// The package root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The package root with the digest this opener holds in memory, the one
    /// carrier of the digest of an opened artifact. The digest is the one the
    /// registry records; a directory has none.
    pub fn package_root(&self) -> crate::package_source::PackageRoot {
        crate::package_source::PackageRoot {
            path: self.root.clone(),
            artifact_digest: self.digest,
        }
    }
}

impl Drop for OpenedPackage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

static UNPACK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Open the package artifact of `package_id@version` from the registry.
///
/// # Errors
///
/// [`NoPackageArtifact`] when the registry holds no artifact for the package,
/// or an error when the artifact cannot be fetched or does not unpack to its
/// digest.
pub async fn open_package(
    registry: &PackageRegistry,
    package_id: &wamn_package::PackageName,
    version: &wamn_package::package_version::PackageVersion,
) -> anyhow::Result<OpenedPackage> {
    let tag = format!("{package_id}-{version}").parse::<wamn_oci::package_artifact::tag::OciTag>()?;
    let pulled = Repository::open(registry, &tag)?
        .pull()
        .await?
        .ok_or_else(|| NoPackageArtifact {
            package_id: package_id.to_owned(),
            version: version.clone(),
        })?;
    unpack_artifact(&tag, &pulled)
}

/// Open published package bytes by digest, independently of mutable tags.
pub async fn open_package_digest(
    registry: &PackageRegistry,
    digest: &wamn_digest::Digest,
) -> anyhow::Result<(OpenedPackage, PackageManifest)> {
    let pulled = Repository::open(registry, digest)?
        .pull()
        .await?
        .with_context(|| format!("package artifact {digest} is absent"))?;
    let layer = pulled
        .layers
        .first()
        .context("package has no manifest layer")?;
    let manifest = PackageManifest::from_slice(&layer.bytes)?;
    let identity = &manifest.package;
    let tag = format!("{}-{}", identity.id, identity.version).parse()?;
    Ok((unpack_artifact(&tag, &pulled)?, manifest))
}

/// Unpack the `layers` of the artifact `tag`, whose manifest digest is
/// `digest`, into a private temporary directory, which must pack again to
/// `digest`.
fn unpack_artifact(
    tag: &wamn_oci::package_artifact::tag::OciTag,
    pulled: &PulledPackage,
) -> anyhow::Result<OpenedPackage> {
    let PulledPackage {
        digest,
        layers,
        annotations,
    } = pulled;
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
    };
    unpack_layers(opened.root(), layers)
        .with_context(|| format!("unpack package artifact {tag}"))?;
    if is_dev_tag(tag) {
        let created = annotations
            .get(CREATED_ANNOTATION)
            .context("dev package has no publication time")?;
        write_file(&opened.root().join(CREATED_FILE), created.as_bytes())?;
    }
    let unpacked = pack_package(opened.root())?;
    let unpacked_tag = unpacked.tag()?;
    ensure!(
        &unpacked_tag == tag,
        "package artifact {tag} holds package {unpacked_tag}"
    );
    ensure!(
        unpacked.digest == *digest,
        "package artifact {tag} unpacks to a tree that packs to {}, not {digest}",
        unpacked.digest
    );
    Ok(opened)
}

/// Unpack the package document and its SQL files into a private directory.
fn unpack_layers(root: &Path, layers: &[PackedLayer]) -> anyhow::Result<()> {
    let [manifest, migrations] = layers else {
        bail!(
            "package artifact must have two layers, got {}",
            layers.len()
        );
    };
    let package = PackageManifest::from_slice(&manifest.bytes)?;
    validate_published(&package)?;
    for component in package
        .deploy
        .iter()
        .flat_map(|deploy| deploy.components.values())
    {
        component
            .descriptor
            .verify_projection(&package.package.id, &package.package.version)
            .with_context(|| {
                format!(
                    "verify the stored admission of {}",
                    component.descriptor.component
                )
            })?;
    }
    write_file(&root.join(wamn_codegen::COMPILED_MANIFEST), &manifest.bytes)?;
    let mut archive = tar::Archive::new(migrations.bytes.as_slice());
    for entry in archive.entries().context("read the migrations tar")? {
        let mut entry = entry.context("read a migration entry")?;
        ensure!(
            entry.header().entry_type() == tar::EntryType::Regular,
            "the migrations tar holds an entry that is not a regular file"
        );
        let path = plan::migration_path(&entry)?.into_owned();
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut data)
            .with_context(|| format!("read migration {}", path.display()))?;
        write_file(&root.join(path), &data)?;
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
    #[tokio::test]
    async fn dev_artifact_keeps_its_publication_time_when_opened_and_repacked() -> anyhow::Result<()>
    {
        use wamn_test_infrastructure::publish::Server;
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
        let root = scratch.path().join("package");
        let version = "1.0.0-dev.h0123456789ab";
        write(
            &root,
            "generated/wamn.json",
            &serde_json::to_vec(&serde_json::json!({
                "package": {"id": "orders", "version": version},
                "required_platform_policy_contract": {"id": "access", "state": "satisfied"},
                "models": {}, "connections": {}, "components": {}, "deploy": {"routes": {}}
            }))?,
        );
        write(&root, CREATED_FILE, b"2026-11-01T00:00:00Z");
        let packed = pack_package(&root)?;
        let manifest: oci_client::manifest::OciImageManifest =
            serde_json::from_slice(&packed.manifest)?;
        let annotations = manifest.annotations.context("published annotations")?;
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(
                annotations
                    .get(CREATED_ANNOTATION)
                    .context("publication time")?
            )?,
            chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z")?,
        );
        let responses = std::iter::once((200, packed.manifest))
            .chain(packed.layers.into_iter().map(|layer| (200, layer.bytes)))
            .collect();
        let server = Server::start(responses).await;
        let authority = server.reference.registry();
        let auth = scratch.path().join("auth.json");
        std::fs::write(
            &auth,
            serde_json::to_vec(&serde_json::json!({"auths":{
                authority: {"username":"fixture", "password":"fixture"}
            }}))?,
        )?;
        let registry = PackageRegistry {
            artifact_base: format!("{authority}/packages"),
            credentials: None,
            registry_auth_file: auth,
            insecure_registry: true,
            oci_ca_paths: Vec::new(),
        };
        let opened = open_package(&registry, &"orders".parse().unwrap(), &version.parse()?).await?;
        assert_eq!(
            (pack_package(opened.root())?.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            (packed.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap()
        );
        Ok(())
    }

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
        let first = pack_package(&root).unwrap();
        assert_eq!(first.layers.len(), 2, "one manifest and one migrations tar");
        std::fs::write(root.join("web/src/main.ts"), b"changed source\n").unwrap();
        std::fs::write(root.join("generated/web/index.html"), b"changed output\n").unwrap();
        let web = wamn_codegen::output_root(&root).join("web-artifact/dist");
        write(&web, "index.html", b"standalone output\n");
        assert_eq!(
            (first.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            (pack_package(&root).unwrap().digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap()
        );
    }

}
