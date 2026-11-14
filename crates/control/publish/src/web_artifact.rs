//! Independent web artifacts and bucket uploads (docs/operations/web-upload.md).
//!
//! A standalone artifact packs `dist/` and canonical `web.json` in one tar layer.
//! Its metadata binds the client to the package's recorded manifest digest.
//!
//! `wamn web upload --platform-ui` uploads a pinned platform UI artifact.
//! Application uploads copy the pinned artifact to its immutable bucket root.

pub(super) mod plan;
pub mod platform_ui;
pub use plan::client_project;
use plan::{content_type, read_web_manifest, validate_path, web_dist_files, web_manifest};

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{Attribute, Attributes, ObjectStore, ObjectStoreExt as _, PutMode, PutOptions};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
use wamn_compiler::build_record::{self, BuildRecord};
use wamn_digest::{bytes_sha256, canonical_json_bytes};
use wamn_package_artifact::registry_credentials::read_registry_push_credentials;
use wamn_package_artifact::{Repository, ensure_digest, ensure_tag, registry_auth};

use crate::package_artifact::PackageRegistry;

/// Standalone output below a package's build directory.
const ARTIFACT_DIRECTORY: &str = "web-artifact";
/// The web artifact's metadata, beside `dist/`.
pub const WEB_MANIFEST_FILE: &str = "web.json";
/// OCI artifact type for an independent web client.
pub const WEB_ARTIFACT_TYPE: &str = "application/vnd.wamn.web.v1";
/// Media type of the tar that contains `dist/` and `web.json`.
pub const WEB_TAR_MEDIA_TYPE: &str = "application/vnd.wamn.web.v1.tar";
const EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
const EMPTY_CONFIG: &[u8] = b"{}";

/// The client identity and package contract that accompany the browser files.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebManifest {
    pub client_package: String,
    /// The version from the package's `web/package.json`.
    pub version: String,
    /// `<package id>@<package version>` from the package build record.
    pub package: String,
    /// `outputs.manifest`, the digest of the compiled canonical `wamn.json`.
    pub contract_digest: String,
}

impl WebManifest {
    /// Canonical JSON bytes, independent of field order and whitespace.
    pub fn to_bytes(&self) -> Vec<u8> {
        canonical_json_bytes(&serde_json::to_value(self).expect("web metadata serializes"))
    }
}

#[derive(Debug)]
enum WebArtifactErrorType {
    Build,
    Pack,
    Push,
    Pull,
    Upload,
}

/// A web artifact failure with its action, path or reference, and source.
#[derive(Debug)]
pub struct WebArtifactError {
    type_: WebArtifactErrorType,
    context: String,
    source: anyhow::Error,
}

impl std::fmt::Display for WebArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "web artifact {:?} {}: {:#}",
            self.type_, self.context, self.source
        )
    }
}

impl std::error::Error for WebArtifactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// The directory that holds `dist/` and `web.json` for one standalone build.
pub fn artifact_output(package_root: &Path) -> PathBuf {
    wamn_compiler::output_root(package_root).join(ARTIFACT_DIRECTORY)
}

/// Build browser files against the contract in the package's build record.
///
/// Web source changes do not require another package build. The compiled
/// manifest must still match the contract that `build.json` records.
/// The web version comes from the package's `web/package.json`.
///
/// # Errors
///
/// Refuses a missing build record or web version, a changed manifest, no client,
/// or a failed Vite build.
pub async fn build_web_artifact(package_root: &Path) -> Result<PathBuf, WebArtifactError> {
    async {
        let output = wamn_compiler::output_root(package_root);
        let record = build_record::read(&output)?;
        let path = output.join("wamn.json");
        let compiled = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let path = package_root.join("web/package.json");
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let package: serde_json::Value =
            serde_json::from_slice(&bytes).with_context(|| format!("read {}", path.display()))?;
        let version = package
            .get("version")
            .and_then(serde_json::Value::as_str)
            .filter(|version| !version.is_empty())
            .with_context(|| format!("{} has no nonempty version", path.display()))?;
        let manifest = web_manifest(&record, &compiled, version.to_owned())?;
        let root = artifact_output(package_root);
        std::fs::create_dir_all(&root)?;
        let sidecar = root.join(WEB_MANIFEST_FILE);
        if sidecar.exists() {
            std::fs::remove_file(&sidecar)?;
        }
        wamn_build::build_web_client(package_root, &root.join("dist")).await?;
        std::fs::write(&sidecar, manifest.to_bytes())
            .with_context(|| format!("write {}", sidecar.display()))?;
        Ok(root)
    }
    .await
    .map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Build,
        context: package_root.display().to_string(),
        source,
    })
}

/// One deterministic tar and its canonical OCI image manifest.
#[derive(Debug)]
pub struct PackedWebArtifact {
    pub tag: String,
    pub digest: String,
    pub manifest: Vec<u8>,
    pub tar: Vec<u8>,
}

/// Pack only `dist/` and canonical `web.json` from a standalone build directory.
///
/// # Errors
///
/// Refuses invalid metadata, a missing index, links, special files, or unreadable files.
pub fn pack_web_artifact(root: &Path) -> Result<PackedWebArtifact, WebArtifactError> {
    pack_web(root).map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Pack,
        context: root.display().to_string(),
        source,
    })
}

fn pack_web(root: &Path) -> anyhow::Result<PackedWebArtifact> {
    let metadata_path = root.join(WEB_MANIFEST_FILE);
    ensure!(
        std::fs::symlink_metadata(&metadata_path)?
            .file_type()
            .is_file(),
        "web.json must be a regular file"
    );
    let bytes = std::fs::read(&metadata_path)?;
    let metadata: WebManifest = serde_json::from_slice(&bytes)?;
    ensure!(
        bytes == metadata.to_bytes(),
        "web.json is not canonical JSON"
    );
    ensure!(
        !metadata.client_package.is_empty(),
        "web.json has no client_package"
    );
    ensure!(!metadata.version.is_empty(), "web.json has no version");
    ensure_digest(&metadata.contract_digest)?;
    let (package, version) = metadata
        .package
        .split_once('@')
        .context("web.json package must be <id>@<version>")?;
    ensure!(
        !package.is_empty() && !version.is_empty(),
        "web.json package must be <id>@<version>"
    );
    let tag = format!("{package}-web-{}", metadata.version);
    ensure_tag(&tag)?;
    let mut files = std::collections::BTreeMap::new();
    artifact_files(root, Path::new("dist"), &mut files)?;
    ensure!(
        files.contains_key(Path::new("dist/index.html")),
        "{} holds no dist/index.html",
        root.display()
    );
    files.insert(PathBuf::from(WEB_MANIFEST_FILE), bytes);
    pack_archive(tag, files, WEB_ARTIFACT_TYPE)
}

fn pack_archive(
    tag: String,
    files: std::collections::BTreeMap<PathBuf, Vec<u8>>,
    artifact_type: &str,
) -> anyhow::Result<PackedWebArtifact> {
    let mut archive = tar::Builder::new(Vec::new());
    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(u64::try_from(bytes.len())?);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        archive.append_data(&mut header, path, bytes.as_slice())?;
    }
    let tar = archive.into_inner()?;
    let manifest = canonical_json_bytes(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_IMAGE_MEDIA_TYPE,
        "artifactType": artifact_type,
        "config": {"mediaType": EMPTY_CONFIG_MEDIA_TYPE, "digest": bytes_sha256(EMPTY_CONFIG), "size": EMPTY_CONFIG.len()},
        "layers": [{"mediaType": WEB_TAR_MEDIA_TYPE, "digest": bytes_sha256(&tar), "size": tar.len()}],
    }));
    Ok(PackedWebArtifact {
        tag,
        digest: bytes_sha256(&manifest),
        manifest,
        tar,
    })
}

fn artifact_files(
    root: &Path,
    relative: &Path,
    files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
) -> anyhow::Result<()> {
    let path = root.join(relative);
    let type_ = std::fs::symlink_metadata(&path)?.file_type();
    ensure!(
        type_.is_dir() || type_.is_file(),
        "web artifact path {} is not a regular file or directory",
        path.display()
    );
    if type_.is_dir() {
        for entry in std::fs::read_dir(&path)? {
            artifact_files(root, &relative.join(entry?.file_name()), files)?;
        }
    } else {
        relative
            .to_str()
            .context("web artifact paths must be UTF-8")?;
        files.insert(
            relative.to_owned(),
            std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        );
    }
    Ok(())
}

/// Whether the tag was created or already held the same artifact digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebPushDisposition {
    Pushed,
    AlreadyPresent,
}

/// The immutable tag and digest after a successful web push.
#[derive(Debug)]
pub struct PushedWebArtifact {
    pub tag: String,
    pub digest: String,
    pub disposition: WebPushDisposition,
}

/// Push a web artifact, or accept the same digest without any writes.
///
/// # Errors
///
/// Refuses different content under the tag, invalid registry inputs, or failed transfers.
pub async fn push_web_artifact(
    packed: &PackedWebArtifact,
    registry: &PackageRegistry,
) -> Result<PushedWebArtifact, WebArtifactError> {
    let repository = (|| {
        let base = wamn_catalog::parse_component_artifact_base(&registry.artifact_base)?;
        let credentials =
            read_registry_push_credentials(&registry.registry_auth_file, base.registry())?;
        Repository::with_auth(registry, &packed.tag, registry_auth(credentials.as_ref()))
    })()
    .map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Push,
        context: format!("{}:{}", registry.artifact_base, packed.tag),
        source,
    })?;
    push_archive(packed, &repository).await
}

/// Publish a packed browser artifact through an authenticated repository.
///
/// # Errors
/// Refuses a conflicting tag or a failed transfer.
pub async fn push_archive(
    packed: &PackedWebArtifact,
    repository: &Repository,
) -> Result<PushedWebArtifact, WebArtifactError> {
    async {
        let disposition = match repository.manifest().await? {
            Some((_, digest)) if digest == packed.digest => WebPushDisposition::AlreadyPresent,
            Some((_, digest)) => bail!(
                "web artifact {} holds {digest}, but the web build packs to {}. Tags are immutable and nothing was pushed",
                repository.reference,
                packed.digest
            ),
            None => {
                for bytes in [&packed.tar[..], EMPTY_CONFIG] {
                    repository.client
                        .push_blob(&repository.reference, bytes.to_vec(), &bytes_sha256(bytes))
                        .await
                        .with_context(|| format!("push a blob of web artifact {}", repository.reference))?;
                }
                repository.client
                    .push_manifest_raw(
                        &repository.reference,
                        packed.manifest.clone(),
                        reqwest::header::HeaderValue::from_static(OCI_IMAGE_MEDIA_TYPE),
                    )
                    .await
                    .with_context(|| format!("push web artifact {}", repository.reference))?;
                let pulled = repository.manifest().await?;
                ensure!(
                    pulled.as_ref().map(|(_, digest)| digest.as_str()) == Some(packed.digest.as_str()),
                    "pushed web artifact {} reads back as {}, not {}",
                    repository.reference,
                    pulled.as_ref().map_or("nothing", |(_, digest)| digest.as_str()),
                    packed.digest
                );
                WebPushDisposition::Pushed
            }
        };
        Ok(PushedWebArtifact {
            tag: packed.tag.clone(),
            digest: packed.digest.clone(),
            disposition,
        })
    }.await.map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Push,
        context: repository.reference.to_string(),
        source,
    })
}

/// Pull a pinned web artifact and read its canonical metadata without taking ownership.
///
/// # Errors
///
/// Refuses an absent artifact, a digest mismatch, or invalid web metadata.
pub async fn pull_web_artifact(
    registry: &PackageRegistry,
    digest: &str,
) -> Result<WebManifest, WebArtifactError> {
    pull_web(registry, digest)
        .await
        .map(|(metadata, _)| metadata)
}

async fn pull_web(
    registry: &PackageRegistry,
    digest: &str,
) -> Result<(WebManifest, Vec<u8>), WebArtifactError> {
    async {
        let tar = pull_archive(registry, digest, WEB_ARTIFACT_TYPE).await?;
        Ok((read_web_manifest(&tar)?, tar))
    }
    .await
    .map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Pull,
        context: format!("{}@{digest}", registry.artifact_base),
        source,
    })
}

async fn pull_archive(
    registry: &PackageRegistry,
    digest: &str,
    artifact_type: &str,
) -> anyhow::Result<Vec<u8>> {
    ensure_digest(digest)?;
    let repository = Repository::open(registry, digest)?;
    let (bytes, _) = repository
        .manifest()
        .await?
        .with_context(|| format!("web artifact {digest} does not exist"))?;
    ensure!(
        bytes_sha256(&bytes) == digest,
        "web artifact manifest does not hash to {digest}"
    );
    let manifest: OciImageManifest = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest.artifact_type.as_deref() == Some(artifact_type),
        "{digest} is not a {artifact_type} web artifact"
    );
    ensure!(
        manifest.layers.len() == 1 && manifest.layers[0].media_type == WEB_TAR_MEDIA_TYPE,
        "web artifact {digest} must have one {WEB_TAR_MEDIA_TYPE} layer"
    );
    let layer = &manifest.layers[0];
    let tar = wamn_package_artifact::registry_pull::blob(
        &repository.client,
        &repository.reference,
        layer,
        tokio::time::Instant::now() + std::time::Duration::from_secs(30),
    )
    .await?;
    ensure!(
        bytes_sha256(&tar) == layer.digest,
        "web artifact {digest} layer does not hash to {}",
        layer.digest
    );
    Ok(tar)
}

/// The package, destination, and completion state of an artifact upload.
#[derive(Debug)]
pub struct UploadedClient {
    pub package: String,
    pub location: String,
    pub written: bool,
}

/// Copy a pinned web artifact to the bucket without replacing existing objects.
///
/// # Errors
///
/// Refuses invalid artifacts, registry failures, or conflicting bucket objects.
pub async fn upload_web_artifact(
    registry: &PackageRegistry,
    digest: &str,
    bucket: &str,
) -> Result<UploadedClient, WebArtifactError> {
    async {
        let sink = Sink::parse(bucket)?;
        let store = sink.store()?;
        let (package, root, written) =
            upload_web_to_store(registry, digest, sink.prefix, store.as_ref()).await?;
        Ok(UploadedClient {
            package,
            location: format!("{}{}/{root}/", sink.scheme.prefix(), sink.bucket),
            written,
        })
    }
    .await
    .map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Upload,
        context: format!("{}@{digest} to {bucket}", registry.artifact_base),
        source,
    })
}

pub async fn upload_web_to_store(
    registry: &PackageRegistry,
    digest: &str,
    prefix: &str,
    store: &dyn ObjectStore,
) -> anyhow::Result<(String, String, bool)> {
    let (metadata, tar) = pull_web(registry, digest).await?;
    let (package, _) = metadata.package.split_once('@').expect("verified web.json");
    ensure_tag(package)?;
    let root = [prefix.trim_matches('/'), "clients", package, &digest[7..]]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let index = object_store::path::Path::from(format!("{root}/index.html"));
    match store.head(&index).await {
        Ok(_) => return Ok((package.to_owned(), root, false)),
        Err(object_store::Error::NotFound { .. }) => {}
        Err(error) => return Err(error).context("read the web upload completion index"),
    }
    let mut files = web_dist_files(&tar)?;
    let index = files
        .remove("index.html")
        .context("web artifact has no dist/index.html")?;
    for (name, bytes) in files {
        put(store, &root, &name, bytes, content_type(&name)?).await?;
    }
    put(
        store,
        &root,
        "index.html",
        index,
        content_type("index.html")?,
    )
    .await?;
    Ok((package.to_owned(), root, true))
}

/// Write one object create-only with its content type. An
/// existing object with equal bytes is the write of an earlier run that
/// stopped, so it stays. An existing object with other bytes refuses.
async fn put(
    store: &dyn ObjectStore,
    root: &str,
    relative: &str,
    bytes: Vec<u8>,
    content_type: &'static str,
) -> anyhow::Result<()> {
    let mut attributes = Attributes::new();
    attributes.insert(Attribute::ContentType, content_type.into());
    let key = object_store::path::Path::from(format!("{root}/{relative}"));
    let bytes = object_store::PutPayload::from(bytes);
    let written = store
        .put_opts(
            &key,
            bytes.clone(),
            PutOptions {
                mode: PutMode::Create,
                attributes,
                ..PutOptions::default()
            },
        )
        .await;
    match written {
        Err(
            object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. },
        ) => {
            let existing = store
                .get(&key)
                .await
                .with_context(|| format!("read the existing {key}"))?
                .bytes()
                .await
                .with_context(|| format!("read the existing {key}"))?;
            ensure!(
                existing
                    .as_ref()
                    .iter()
                    .copied()
                    .eq(bytes.iter().flat_map(|part| part.iter().copied())),
                "write {key}; an existing object with other bytes is never replaced"
            );
            Ok(())
        }
        written => written
            .map(|_| ())
            .with_context(|| format!("write {key}; an existing object is never replaced")),
    }
}

pub use plan::Scheme;

pub use plan::Sink;

impl Sink<'_> {
    /// For `s3://` the `AWS_*` variables supply the endpoint and the
    /// credentials. For `gs://` the `GOOGLE_*` variables and Application
    /// Default Credentials do.
    pub fn store(&self) -> anyhow::Result<Box<dyn ObjectStore>> {
        Ok(match self.scheme {
            Scheme::S3 => Box::new(
                AmazonS3Builder::from_env()
                    .with_bucket_name(self.bucket)
                    .build()
                    .context("configure the bucket from the AWS_* variables")?,
            ),
            Scheme::Gcs => Box::new(
                GoogleCloudStorageBuilder::from_env()
                    .with_bucket_name(self.bucket)
                    .build()
                    .context(
                        "configure the bucket from the GOOGLE_* variables and \
                         Application Default Credentials",
                    )?,
            ),
        })
    }
}

/// Every file under `directory`.
pub fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)
        .with_context(|| format!("read the build directory {}", directory.display()))?
    {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "test-util"))]
pub mod test_support;

#[cfg(test)]
mod tests {
    use super::test_support::{artifact_fixture, build_fixture, fixture_registry};
    use std::fs;
    use std::path::PathBuf;

    use wamn_compiler::build_record::{self, BuildRecord};
    use wamn_digest::{bytes_sha256, canonical_json_bytes};
    use wamn_package_artifact::registry_pull::test_server::Server;
    use wamn_test_infrastructure::scratch::ScratchRoot;

    use super::{
        Sink, WEB_MANIFEST_FILE, WebPushDisposition, build_web_artifact, pack_web_artifact,
        pull_web_artifact, push_web_artifact, read_web_manifest,
    };

    fn write_build(scratch: &ScratchRoot, record: &BuildRecord, compiled: &[u8]) -> PathBuf {
        let package = scratch.path().join("apps/web_fixture");
        fs::create_dir_all(package.join("web")).unwrap();
        fs::write(package.join("web/package.json"), r#"{"version":"0.2.0"}"#).unwrap();
        let output = wamn_compiler::output_root(&package);
        fs::create_dir_all(&output).unwrap();
        fs::write(
            output.join(build_record::BUILD_RECORD_FILE),
            record.to_bytes(),
        )
        .unwrap();
        fs::write(output.join("wamn.json"), compiled).unwrap();
        package
    }

    #[tokio::test]
    async fn web_build_refuses_a_manifest_that_differs_from_the_build_record() {
        let scratch = ScratchRoot::create().unwrap();
        let (record, mut compiled) = build_fixture(Some("@wamn/web-fixture-client"));
        compiled.push(b' ');
        let package = write_build(&scratch, &record, &compiled);
        let _error = build_web_artifact(&package).await.unwrap_err().to_string();
    }

    #[tokio::test]
    async fn web_build_refuses_a_package_without_a_client() {
        let scratch = ScratchRoot::create().unwrap();
        let (record, compiled) = build_fixture(None);
        let package = write_build(&scratch, &record, &compiled);
        let _error = build_web_artifact(&package).await.unwrap_err().to_string();
    }

    #[tokio::test]
    async fn web_build_refuses_a_missing_or_empty_web_version_and_names_package_json() {
        let scratch = ScratchRoot::create().unwrap();
        let (record, compiled) = build_fixture(Some("@wamn/web-fixture-client"));
        let package = write_build(&scratch, &record, &compiled);
        let path = package.join("web/package.json");
        for contents in [None, Some("{}"), Some(r#"{"version":""}"#)] {
            if let Some(contents) = contents {
                fs::write(&path, contents).unwrap();
            } else {
                fs::remove_file(&path).unwrap();
            }
            let _error = build_web_artifact(&package).await.unwrap_err().to_string();
        }
    }

    #[tokio::test]
    async fn web_build_rebuilds_changed_web_source_without_changing_the_package_record() {
        let scratch = ScratchRoot::create().unwrap();
        let (record, compiled) = build_fixture(Some("@wamn/web-fixture-client"));
        let package = write_build(&scratch, &record, &compiled);
        let source = package.join("web");
        fs::write(
            source.join("package.json"),
            r#"{"version":"0.2.0","scripts":{"build":"node build.cjs"}}"#,
        )
        .unwrap();
        fs::write(
            source.join("pnpm-workspace.yaml"),
            "verifyDepsBeforeRun: false\n",
        )
        .unwrap();
        fs::write(
            source.join("build.cjs"),
            r"
const fs = require('node:fs');
const args = process.argv.slice(2);
if (!args.includes('--emptyOutDir')) process.exit(1);
const dist = args[args.indexOf('--outDir') + 1];
fs.rmSync(dist, {recursive: true, force: true});
fs.mkdirSync(dist, {recursive: true});
fs.copyFileSync('index.html', dist + '/index.html');
",
        )
        .unwrap();
        fs::write(source.join("index.html"), "first").unwrap();
        let output = build_web_artifact(&package).await.unwrap();
        let first = pack_web_artifact(&output).unwrap();

        fs::write(source.join("index.html"), "other").unwrap();
        fs::write(
            source.join("package.json"),
            r#"{"version":"0.2.1","scripts":{"build":"node build.cjs"}}"#,
        )
        .unwrap();
        build_web_artifact(&package).await.unwrap();
        let second = pack_web_artifact(&output).unwrap();

        assert_ne!(
            wamn_catalog::ArtifactHash::parse(first.digest.to_string()).unwrap(),
            wamn_catalog::ArtifactHash::parse(second.digest.to_string()).unwrap()
        );
        let metadata = read_web_manifest(&second.tar).unwrap();

        assert_eq!(
            wamn_catalog::ArtifactHash::parse(metadata.contract_digest.to_string()).unwrap(),
            wamn_catalog::ArtifactHash::parse(record.outputs.manifest.to_string()).unwrap()
        );

        assert_eq!(
            build_record::read(&wamn_compiler::output_root(&package)).unwrap(),
            record
        );
        fs::write(source.join("build.cjs"), "process.exit(1);").unwrap();
        assert!(build_web_artifact(&package).await.is_err());
    }

    #[test]
    fn web_pack_and_pull_refuse_a_missing_or_empty_web_version() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let path = scratch.path().join(WEB_MANIFEST_FILE);
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for version in [None, Some("")] {
            if let Some(version) = version {
                metadata["version"] = serde_json::json!(version);
            } else {
                metadata.as_object_mut().unwrap().remove("version");
            }
            let bytes = canonical_json_bytes(&metadata);
            fs::write(&path, &bytes).unwrap();
            let _error = pack_web_artifact(scratch.path()).unwrap_err().to_string();

            let mut archive = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            archive
                .append_data(&mut header, WEB_MANIFEST_FILE, &bytes[..])
                .unwrap();
            let _error = read_web_manifest(&archive.into_inner().unwrap()).unwrap_err();
        }
    }

    #[test]
    fn web_pack_refuses_noncanonical_metadata_missing_index_and_links() {
        let scratch = ScratchRoot::create().unwrap();
        let root = scratch.path().join("artifact");
        artifact_fixture(&root, false);
        let metadata = fs::read(root.join(WEB_MANIFEST_FILE)).unwrap();
        let mut changed = metadata.clone();
        changed.push(b'\n');
        fs::write(root.join(WEB_MANIFEST_FILE), changed).unwrap();
        assert!(pack_web_artifact(&root).is_err());
        let mut invalid: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
        invalid["version"] = serde_json::json!("bad/version");
        fs::write(root.join(WEB_MANIFEST_FILE), canonical_json_bytes(&invalid)).unwrap();
        assert!(pack_web_artifact(&root).is_err());
        fs::write(root.join(WEB_MANIFEST_FILE), metadata).unwrap();
        fs::remove_file(root.join("dist/index.html")).unwrap();
        assert!(pack_web_artifact(&root).is_err());
        fs::write(root.join("dist/index.html"), "index").unwrap();
        std::os::unix::fs::symlink(root.join(WEB_MANIFEST_FILE), root.join("dist/link")).unwrap();
        assert!(pack_web_artifact(&root).is_err());
    }

    #[tokio::test]
    async fn web_pull_reads_verified_metadata_by_digest_without_writes() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack_web_artifact(scratch.path()).unwrap();
        let server = Server::start(vec![
            (200, packed.manifest.clone()),
            (200, packed.tar.clone()),
            (200, packed.manifest.clone()),
            (200, packed.tar.clone()),
        ])
        .await;
        let registry = fixture_registry(&scratch, &server);
        let first = pull_web_artifact(&registry, &packed.digest).await.unwrap();
        let second = pull_web_artifact(&registry, &packed.digest).await.unwrap();
        assert_eq!(first, second);

        assert_eq!(server.requests(), 4);
    }

    #[tokio::test]
    async fn web_pull_refuses_absence_and_manifest_or_layer_digest_mismatches() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack_web_artifact(scratch.path()).unwrap();
        for (responses, digest, _reason) in [
            (
                vec![(404, Vec::new())],
                packed.digest.clone(),
                "does not exist",
            ),
            (
                vec![(200, packed.manifest.clone())],
                bytes_sha256(b"another manifest"),
                "Invalid digest",
            ),
            (
                vec![(200, packed.manifest.clone()), (200, b"wrong tar".to_vec())],
                packed.digest.clone(),
                "Invalid digest",
            ),
        ] {
            let server = Server::start(responses).await;
            let _error = pull_web_artifact(&fixture_registry(&scratch, &server), &digest)
                .await
                .unwrap_err()
                .to_string();
        }
    }

    #[test]
    fn web_metadata_requires_one_canonical_web_json() {
        let mut empty = tar::Builder::new(Vec::new());
        empty.finish().unwrap();
        assert!(read_web_manifest(&empty.into_inner().unwrap()).is_err());
        for (bytes, copies, _reason) in [
            (b"{}\n".to_vec(), 1, "missing field"),
            (
                [
                    include_bytes!("../../environment/tests/support/web-artifact/web.json")
                        .as_slice(),
                    b"\n",
                ]
                .concat(),
                1,
                "not canonical",
            ),
            (
                include_bytes!("../../environment/tests/support/web-artifact/web.json").to_vec(),
                2,
                "repeats web.json",
            ),
        ] {
            let mut archive = tar::Builder::new(Vec::new());
            for _ in 0..copies {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                archive
                    .append_data(&mut header, WEB_MANIFEST_FILE, &bytes[..])
                    .unwrap();
            }
            let _error = read_web_manifest(&archive.into_inner().unwrap()).unwrap_err();
        }
    }

    #[tokio::test]
    async fn web_push_accepts_the_same_digest_without_writes() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack_web_artifact(scratch.path()).unwrap();
        let server = Server::start(vec![(200, packed.manifest.clone())]).await;
        let result = push_web_artifact(&packed, &fixture_registry(&scratch, &server))
            .await
            .unwrap();
        assert_eq!(result.disposition, WebPushDisposition::AlreadyPresent);
        assert_eq!(
            wamn_catalog::ArtifactHash::parse(result.digest.to_string()).unwrap(),
            wamn_catalog::ArtifactHash::parse(packed.digest.to_string()).unwrap()
        );
        assert_eq!(server.requests(), 1);
    }

    #[tokio::test]
    async fn web_push_refuses_a_different_digest_and_names_the_tag_and_both_digests() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack_web_artifact(scratch.path()).unwrap();
        fs::write(scratch.path().join("dist/index.html"), "other").unwrap();
        let changed = pack_web_artifact(scratch.path()).unwrap();
        assert_eq!(packed.tag, changed.tag);
        assert_ne!(
            wamn_catalog::ArtifactHash::parse(packed.digest.to_string()).unwrap(),
            wamn_catalog::ArtifactHash::parse(changed.digest.to_string()).unwrap()
        );
        let server = Server::start(vec![(200, packed.manifest.clone())]).await;
        let _error = push_web_artifact(&changed, &fixture_registry(&scratch, &server))
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(server.requests(), 1);
    }

    use object_store::ObjectStore as _;
    use object_store::memory::InMemory;

    #[tokio::test]
    async fn upload_refuses_digest_mismatches_and_non_web_artifacts_before_any_write() {
        use futures_util::TryStreamExt as _;

        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack_web_artifact(scratch.path()).unwrap();
        let mut non_web: serde_json::Value = serde_json::from_slice(&packed.manifest).unwrap();
        non_web["artifactType"] = serde_json::json!("application/vnd.wamn.package.v1");
        let non_web = canonical_json_bytes(&non_web);
        for (responses, digest, _reason) in [
            (
                vec![(200, packed.manifest.clone())],
                bytes_sha256(b"another manifest"),
                "Invalid digest",
            ),
            (
                vec![(200, packed.manifest.clone()), (200, b"wrong tar".to_vec())],
                packed.digest.clone(),
                "Invalid digest",
            ),
            (
                vec![(200, non_web.clone())],
                bytes_sha256(&non_web),
                "is not a",
            ),
        ] {
            let server = Server::start(responses).await;
            let store = InMemory::new();
            let _error = super::upload_web_to_store(
                &fixture_registry(&scratch, &server),
                &digest,
                "",
                &store,
            )
            .await
            .unwrap_err();

            assert!(
                store
                    .list(None)
                    .try_collect::<Vec<_>>()
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn the_sink_refuses_an_unsupported_scheme() {
        assert!(Sink::parse("https://web").is_err());
    }
}
