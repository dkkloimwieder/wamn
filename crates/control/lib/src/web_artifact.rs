//! Independent web artifacts and bucket uploads (docs/operations/web-upload.md).
//!
//! A standalone artifact packs `dist/` and canonical `web.json` in one tar layer.
//! Its metadata binds the client to the package's recorded manifest digest.
//!
//! `wamn web upload --platform-ui` uploads the local platform UI.
//! Application uploads copy the pinned artifact to its immutable bucket root.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{Attribute, Attributes, ObjectStore, ObjectStoreExt as _, PutMode, PutOptions};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
use wamn_execution_contract::{bytes_sha256, canonical_json_bytes};
use wamn_package_artifact::registry_credentials::read_registry_push_credentials;
use wamn_package_artifact::{Repository, ensure_digest, ensure_tag, registry_auth};
use wamn_schema_generator::build_record::{self, BuildRecord};

use crate::package_artifact::PackageRegistry;
use crate::web_scope::{SCOPE_CACHE, SCOPE_CONTENT_TYPE, SCOPE_FILE};

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
enum WebArtifactErrorKind {
    Build,
    Pack,
    Push,
    Pull,
    Upload,
}

/// A web artifact failure with its action, path or reference, and source.
#[derive(Debug)]
pub struct WebArtifactError {
    kind: WebArtifactErrorKind,
    context: String,
    source: anyhow::Error,
}

impl std::fmt::Display for WebArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "web artifact {:?} {}: {:#}",
            self.kind, self.context, self.source
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
    wamn_schema_generator::output_root(package_root).join(ARTIFACT_DIRECTORY)
}

fn web_manifest(record: &BuildRecord, compiled: &[u8]) -> anyhow::Result<WebManifest> {
    let digest = bytes_sha256(compiled);
    ensure!(
        digest == record.outputs.manifest,
        "package {}@{} wamn.json hashes to {digest}, but build.json records {}",
        record.package.id,
        record.package.version,
        record.outputs.manifest
    );
    let manifest = wamn_catalog::manifest::PackageManifest::from_slice(compiled)?;
    ensure!(
        manifest.package.id == record.package.id
            && manifest.package.version == record.package.version,
        "wamn.json identity differs from build.json for package {}@{}",
        record.package.id,
        record.package.version
    );
    let client = manifest.client_package.with_context(|| {
        format!(
            "package {}@{} has no client_package and no web artifact",
            record.package.id, record.package.version
        )
    })?;
    Ok(WebManifest {
        client_package: client.name,
        package: format!("{}@{}", record.package.id, record.package.version),
        contract_digest: record.outputs.manifest.clone(),
    })
}

/// Build browser files against the contract in the package's build record.
///
/// Web source changes do not require another package build. The compiled
/// manifest must still match the contract that `build.json` records.
///
/// # Errors
///
/// Refuses a missing build record, a changed manifest, no client, or a failed Vite build.
pub async fn build_web_artifact(package_root: &Path) -> Result<PathBuf, WebArtifactError> {
    async {
        let output = wamn_schema_generator::output_root(package_root);
        let record = build_record::read(&output)?;
        let path = output.join("wamn.json");
        let compiled = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let manifest = web_manifest(&record, &compiled)?;
        let root = artifact_output(package_root);
        std::fs::create_dir_all(&root)?;
        let sidecar = root.join(WEB_MANIFEST_FILE);
        if sidecar.exists() {
            std::fs::remove_file(&sidecar)?;
        }
        wamn_schema_generator::build::build_web_client(package_root, &root.join("dist")).await?;
        std::fs::write(&sidecar, manifest.to_bytes())
            .with_context(|| format!("write {}", sidecar.display()))?;
        Ok(root)
    }
    .await
    .map_err(|source| WebArtifactError {
        kind: WebArtifactErrorKind::Build,
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
        kind: WebArtifactErrorKind::Pack,
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
    ensure_digest(&metadata.contract_digest)?;
    let (package, version) = metadata
        .package
        .split_once('@')
        .context("web.json package must be <id>@<version>")?;
    ensure!(
        !package.is_empty() && !version.is_empty(),
        "web.json package must be <id>@<version>"
    );
    let tag = format!("{package}-web-{version}");
    ensure_tag(&tag)?;
    let mut files = std::collections::BTreeMap::new();
    artifact_files(root, Path::new("dist"), &mut files)?;
    ensure!(
        files.contains_key(Path::new("dist/index.html")),
        "{} holds no dist/index.html",
        root.display()
    );
    files.insert(PathBuf::from(WEB_MANIFEST_FILE), bytes);
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
        "artifactType": WEB_ARTIFACT_TYPE,
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
    let kind = std::fs::symlink_metadata(&path)?.file_type();
    ensure!(
        kind.is_dir() || kind.is_file(),
        "web artifact path {} is not a regular file or directory",
        path.display()
    );
    if kind.is_dir() {
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
    async {
        let base = wamn_catalog::parse_component_artifact_base(&registry.artifact_base)?;
        let credentials =
            read_registry_push_credentials(&registry.registry_auth_file, base.registry())?;
        let repository =
            Repository::with_auth(registry, &packed.tag, registry_auth(credentials.as_ref()))?;
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
    }
    .await
    .map_err(|source| WebArtifactError {
        kind: WebArtifactErrorKind::Push,
        context: format!("{}:{}", registry.artifact_base, packed.tag),
        source,
    })
}

/// Pull a pinned web artifact and read its canonical metadata without claiming it.
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
            manifest.artifact_type.as_deref() == Some(WEB_ARTIFACT_TYPE),
            "{digest} is not a {WEB_ARTIFACT_TYPE} web artifact"
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
        Ok((read_web_manifest(&tar)?, tar))
    }
    .await
    .map_err(|source| WebArtifactError {
        kind: WebArtifactErrorKind::Pull,
        context: format!("{}@{digest}", registry.artifact_base),
        source,
    })
}

fn read_web_manifest(tar: &[u8]) -> anyhow::Result<WebManifest> {
    use std::io::Read as _;

    let mut archive = tar::Archive::new(tar);
    let mut metadata = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() != Path::new(WEB_MANIFEST_FILE) {
            continue;
        }
        ensure!(metadata.is_none(), "web artifact repeats web.json");
        ensure!(
            entry.header().entry_type().is_file(),
            "web.json must be a regular file"
        );
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        let web: WebManifest = serde_json::from_slice(&bytes)?;
        ensure!(bytes == web.to_bytes(), "web.json is not canonical JSON");
        ensure_digest(&web.contract_digest)?;
        ensure!(
            !web.client_package.is_empty(),
            "web.json has no client_package"
        );
        let (package, version) = web
            .package
            .split_once('@')
            .context("web.json package must be <id>@<version>")?;
        ensure!(
            !package.is_empty() && !version.is_empty(),
            "web.json package must be <id>@<version>"
        );
        metadata = Some(web);
    }
    metadata.context("web artifact has no web.json")
}

/// A built file name carries its content hash, so a browser keeps it.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";
/// The index names the current files, and the service worker `sw.js` keeps
/// its name across builds, so a browser asks again at every load.
const INDEX_CACHE: &str = "no-cache";
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
    org: &str,
    bucket: &str,
) -> Result<UploadedClient, WebArtifactError> {
    async {
        let sink = Sink::parse(bucket)?;
        let store = sink.store()?;
        let (package, root, written) =
            upload_web_to_store(registry, digest, org, sink.prefix, store.as_ref()).await?;
        Ok(UploadedClient {
            package,
            location: format!("{}{}/{root}/", sink.scheme.prefix(), sink.bucket),
            written,
        })
    }
    .await
    .map_err(|source| WebArtifactError {
        kind: WebArtifactErrorKind::Upload,
        context: format!("{}@{digest} to {bucket}", registry.artifact_base),
        source,
    })
}

async fn upload_web_to_store(
    registry: &PackageRegistry,
    digest: &str,
    org: &str,
    prefix: &str,
    store: &dyn ObjectStore,
) -> anyhow::Result<(String, String, bool)> {
    ensure!(!org.is_empty(), "web upload requires an org");
    let (metadata, tar) = pull_web(registry, digest).await?;
    let (package, _) = metadata.package.split_once('@').expect("verified web.json");
    ensure_tag(package)?;
    let project = client_project(&metadata.client_package)?;
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
        put(
            store,
            &root,
            &name,
            bytes,
            cache_control(&name)?,
            content_type(&name)?,
        )
        .await?;
    }
    put(
        store,
        &root,
        SCOPE_FILE,
        crate::web_scope::scope_file_bytes(org, project),
        SCOPE_CACHE,
        SCOPE_CONTENT_TYPE,
    )
    .await?;
    put(
        store,
        &root,
        "index.html",
        index,
        INDEX_CACHE,
        content_type("index.html")?,
    )
    .await?;
    Ok((package.to_owned(), root, true))
}

fn web_dist_files(tar: &[u8]) -> anyhow::Result<std::collections::BTreeMap<String, Vec<u8>>> {
    use std::io::Read as _;

    let mut archive = tar::Archive::new(tar);
    let mut files = std::collections::BTreeMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path == Path::new(WEB_MANIFEST_FILE) {
            continue;
        }
        ensure!(
            path.components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
            "invalid web artifact path {}",
            path.display()
        );
        let relative = path
            .strip_prefix("dist")
            .with_context(|| format!("web artifact path {} is outside dist", path.display()))?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        ensure!(
            entry.header().entry_type().is_file(),
            "web artifact path {} is not a regular file",
            path.display()
        );
        let name = relative
            .to_str()
            .context("web artifact paths must be UTF-8")?;
        cache_control(name)?;
        content_type(name)?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        ensure!(
            files.insert(name.to_owned(), bytes).is_none(),
            "web artifact repeats {name}"
        );
    }
    ensure!(
        files.contains_key("index.html"),
        "web artifact has no dist/index.html"
    );
    Ok(files)
}

/// The project that a client package `@wamn/<project>-client` names.
pub fn client_project(client: &str) -> anyhow::Result<&str> {
    client
        .strip_prefix("@wamn/")
        .and_then(|rest| rest.strip_suffix("-client"))
        .filter(|project| !project.is_empty())
        .with_context(|| format!("the client package {client} is not @wamn/<project>-client"))
}

fn cache_control(relative: &str) -> anyhow::Result<&'static str> {
    if relative == "index.html" || relative == "sw.js" {
        Ok(INDEX_CACHE)
    } else if relative.starts_with("assets/") {
        Ok(ASSET_CACHE)
    } else {
        bail!("the build wrote {relative}, which has no declared cache rule")
    }
}

/// The platform UI has one bucket and one prefix per platform version.
pub const PLATFORM_UI_BUCKET: &str = "gs://wamn-dev-web";

/// Upload the local platform UI only when its digest matches the stamped chart.
/// The current directory is the repository whose platform UI was built.
pub async fn upload_platform_ui(
    repository: &Path,
    chart: &Path,
    org: &str,
    bucket: &str,
) -> anyhow::Result<String> {
    let (dist, root) = platform_ui_source(repository, chart)?;
    let sink = Sink::parse(bucket)?;
    let root = [sink.prefix.trim_matches('/'), root.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let store = sink.store()?;
    write_platform_ui(store.as_ref(), &root, &dist, org).await?;
    Ok(format!("{}{}/{root}/", sink.scheme.prefix(), sink.bucket))
}

pub(crate) fn platform_ui_source(
    repository: &Path,
    chart: &Path,
) -> anyhow::Result<(PathBuf, String)> {
    use crate::platform_build::{
        BUILD_RECORD_FILE, PlatformBuildRecord, platform_ui_digest, platform_ui_dist,
    };
    let build_record_path = chart.join(BUILD_RECORD_FILE);
    let build_record: PlatformBuildRecord =
        serde_json::from_slice(&std::fs::read(&build_record_path).with_context(|| {
            format!(
                "read {}; run `wamn-ctl build-platform`",
                build_record_path.display()
            )
        })?)?;
    let version = crate::release_chart::chart_version(chart)?;
    let platform =
        crate::release_chart::chart_version(&repository.join("deploy/platform/release"))?;
    ensure!(
        version == platform,
        "the stamped chart version {version} differs from platform {platform}"
    );
    let dist = platform_ui_dist(repository);
    ensure!(
        platform_ui_digest(&dist)? == build_record.platform_ui,
        "the platform UI build {} differs from the platform build record {}",
        dist.display(),
        build_record_path.display()
    );
    Ok((dist, format!("platform-ui/{version}")))
}

async fn write_platform_ui(
    store: &dyn ObjectStore,
    root: &str,
    dist: &Path,
    org: &str,
) -> anyhow::Result<()> {
    ensure!(!org.is_empty(), "the platform UI requires an org");
    let mut files = Vec::new();
    collect(dist, &mut files)?;
    write_files(
        store,
        root,
        dist,
        &mut files,
        &crate::web_scope::platform_scope_file_bytes(org),
    )
    .await
}

/// Write every built file create-only, then `config.json`, and the index
/// last, so no reader meets an index whose files are absent.
pub async fn write_files(
    store: &dyn ObjectStore,
    root: &str,
    dist: &Path,
    files: &mut [PathBuf],
    scope: &[u8],
) -> anyhow::Result<()> {
    files.sort_by_key(|file| file == &dist.join("index.html"));
    for file in files.iter() {
        if file == &dist.join("index.html") {
            put(
                store,
                root,
                SCOPE_FILE,
                scope.to_vec(),
                SCOPE_CACHE,
                SCOPE_CONTENT_TYPE,
            )
            .await?;
        }
        let relative = file
            .strip_prefix(dist)?
            .to_str()
            .context("a built file name is UTF-8")?;
        let cache = cache_control(relative)?;
        put(
            store,
            root,
            relative,
            std::fs::read(file)?,
            cache,
            content_type(relative)?,
        )
        .await?;
    }
    Ok(())
}

/// Write one object create-only with its Cache-Control and content type. An
/// existing object with equal bytes is the write of an earlier run that
/// stopped, so it stays. An existing object with other bytes refuses.
async fn put(
    store: &dyn ObjectStore,
    root: &str,
    relative: &str,
    bytes: Vec<u8>,
    cache: &'static str,
    content_type: &'static str,
) -> anyhow::Result<()> {
    let mut attributes = Attributes::new();
    attributes.insert(Attribute::CacheControl, cache.into());
    attributes.insert(Attribute::ContentType, content_type.into());
    let key = object_store::path::Path::from(format!("{root}/{relative}"));
    let written = store
        .put_opts(
            &key,
            bytes.clone().into(),
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
                existing.as_ref() == bytes.as_slice(),
                "write {key}; an existing object with other bytes is never replaced"
            );
            Ok(())
        }
        written => written
            .map(|_| ())
            .with_context(|| format!("write {key}; an existing object is never replaced")),
    }
}

/// The object store that a bucket value names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// `s3://`, an S3 API endpoint, as kind uses.
    S3,
    /// `gs://`, Google Cloud Storage with Application Default Credentials.
    Gcs,
}

impl Scheme {
    pub fn prefix(self) -> &'static str {
        match self {
            Self::S3 => "s3://",
            Self::Gcs => "gs://",
        }
    }
}

/// A parsed bucket value, `s3://<bucket>[/<prefix>]` or `gs://<bucket>[/<prefix>]`.
#[derive(Debug, PartialEq, Eq)]
pub struct Sink<'a> {
    pub scheme: Scheme,
    pub bucket: &'a str,
    pub prefix: &'a str,
}

impl<'a> Sink<'a> {
    pub fn parse(value: &'a str) -> anyhow::Result<Self> {
        let (scheme, rest) = [Scheme::S3, Scheme::Gcs]
            .into_iter()
            .find_map(|scheme| {
                value
                    .strip_prefix(scheme.prefix())
                    .map(|rest| (scheme, rest))
            })
            .context("--bucket must be s3://<bucket>[/<prefix>] or gs://<bucket>[/<prefix>]")?;
        let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        Ok(Self {
            scheme,
            bucket,
            prefix,
        })
    }

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

fn content_type(name: &str) -> anyhow::Result<&'static str> {
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    Ok(match extension {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => bail!("the build wrote {name}, which has no declared content type"),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use wamn_execution_contract::{bytes_sha256, canonical_json_bytes};
    use wamn_package_artifact::PackageRegistry;
    use wamn_package_artifact::registry_pull::test_server::Server;
    use wamn_schema_generator::build_record::{
        self, BuildGenerator, BuildInputs, BuildOutputs, BuildPackage, BuildRecord,
    };
    use wamn_test_infrastructure::scratch::ScratchRoot;

    use super::{
        Scheme, Sink, WEB_MANIFEST_FILE, WebPushDisposition, artifact_output, build_web_artifact,
        collect, pack_web_artifact, pull_web_artifact, push_web_artifact, read_web_manifest,
        web_manifest, write_files,
    };

    fn build_fixture(client: Option<&str>) -> (BuildRecord, Vec<u8>) {
        let mut manifest = serde_json::json!({
            "package": {"id": "web_fixture", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "web_fixture_data_access", "state": "satisfied"},
            "models": {}, "components": {}, "connections": {},
        });
        if let Some(client) = client {
            manifest["client_package"] = serde_json::json!({"name": client});
        }
        let compiled = canonical_json_bytes(&manifest);
        let record = BuildRecord {
            schema: build_record::BUILD_RECORD_SCHEMA.to_owned(),
            package: BuildPackage {
                id: "web_fixture".into(),
                version: "1.0.0".into(),
            },
            inputs: BuildInputs {
                digest: bytes_sha256(b"package inputs"),
                generation_digest: bytes_sha256(b"generation inputs"),
                files: BTreeMap::new(),
            },
            dependencies: Vec::new(),
            generator: BuildGenerator {
                id: "fixture".into(),
                toolchain: "fixture".into(),
                sqlx: "0.9.0".into(),
            },
            outputs: BuildOutputs {
                manifest: bytes_sha256(&compiled),
                files: BTreeMap::new(),
                components: Vec::new(),
            },
        };
        (record, compiled)
    }

    fn write_build(scratch: &ScratchRoot, record: &BuildRecord, compiled: &[u8]) -> PathBuf {
        let package = scratch.path().join("apps/web_fixture");
        fs::create_dir_all(&package).unwrap();
        let output = wamn_schema_generator::output_root(&package);
        fs::create_dir_all(&output).unwrap();
        fs::write(
            output.join(build_record::BUILD_RECORD_FILE),
            record.to_bytes(),
        )
        .unwrap();
        fs::write(output.join("wamn.json"), compiled).unwrap();
        package
    }

    #[test]
    fn web_json_uses_the_record_and_its_verified_manifest() {
        let scratch = ScratchRoot::create().unwrap();
        let (record, compiled) = build_fixture(Some("@wamn/web-fixture-client"));
        let package = write_build(&scratch, &record, &compiled);
        let read = build_record::read(&wamn_schema_generator::output_root(&package)).unwrap();
        let metadata = web_manifest(&read, &compiled).unwrap();
        assert_eq!(
            metadata.to_bytes(),
            canonical_json_bytes(&serde_json::json!({
                "client_package": "@wamn/web-fixture-client",
                "package": "web_fixture@1.0.0",
                "contract_digest": record.outputs.manifest,
            }))
        );
    }

    #[tokio::test]
    async fn web_build_refuses_a_manifest_that_differs_from_the_build_record() {
        let scratch = ScratchRoot::create().unwrap();
        let (record, mut compiled) = build_fixture(Some("@wamn/web-fixture-client"));
        compiled.push(b' ');
        let package = write_build(&scratch, &record, &compiled);
        let error = build_web_artifact(&package).await.unwrap_err().to_string();
        assert!(error.contains("web_fixture@1.0.0"), "{error}");
        assert!(error.contains(&record.outputs.manifest), "{error}");
        assert!(error.contains(&bytes_sha256(&compiled)), "{error}");
        assert!(!artifact_output(&package).exists());
    }

    #[tokio::test]
    async fn web_build_refuses_a_package_without_a_client() {
        let scratch = ScratchRoot::create().unwrap();
        let (record, compiled) = build_fixture(None);
        let package = write_build(&scratch, &record, &compiled);
        let error = build_web_artifact(&package).await.unwrap_err().to_string();
        assert!(
            error.contains("package web_fixture@1.0.0 has no client_package"),
            "{error}"
        );
        assert!(!artifact_output(&package).exists());
    }

    #[tokio::test]
    async fn web_build_rebuilds_changed_web_source_without_changing_the_package_record() {
        let scratch = ScratchRoot::create().unwrap();
        let (record, compiled) = build_fixture(Some("@wamn/web-fixture-client"));
        let package = write_build(&scratch, &record, &compiled);
        let source = package.join("web");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join("package.json"),
            r#"{"scripts":{"build":"node build.cjs"}}"#,
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
        assert_eq!(
            fs::read(output.join(WEB_MANIFEST_FILE)).unwrap(),
            web_manifest(&record, &compiled).unwrap().to_bytes()
        );
        fs::write(source.join("index.html"), "other").unwrap();
        build_web_artifact(&package).await.unwrap();
        assert_ne!(first.digest, pack_web_artifact(&output).unwrap().digest);
        assert_eq!(
            build_record::read(&wamn_schema_generator::output_root(&package)).unwrap(),
            record
        );
        fs::write(source.join("build.cjs"), "process.exit(1);").unwrap();
        assert!(build_web_artifact(&package).await.is_err());
        assert!(!output.join(WEB_MANIFEST_FILE).exists());
    }

    fn artifact_fixture(root: &Path, reversed: bool) {
        fs::create_dir_all(root.join("dist/assets")).unwrap();
        let mut files = vec![
            ("dist/index.html", b"index".as_slice()),
            ("dist/assets/app.js", b"script"),
        ];
        if reversed {
            files.reverse();
        }
        for (name, bytes) in files {
            let path = root.join(name);
            fs::write(&path, bytes).unwrap();
            fs::File::open(&path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(
                    SystemTime::UNIX_EPOCH + Duration::from_secs(if reversed { 900 } else { 100 }),
                ))
                .unwrap();
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(if reversed { 0o755 } else { 0o600 }),
            )
            .unwrap();
        }
        let (record, compiled) = build_fixture(Some("@wamn/web-fixture-client"));
        fs::write(
            root.join(WEB_MANIFEST_FILE),
            web_manifest(&record, &compiled).unwrap().to_bytes(),
        )
        .unwrap();
    }

    #[test]
    fn web_pack_is_independent_of_directory_order_mtime_and_mode() {
        let scratch = ScratchRoot::create().unwrap();
        let first = scratch.path().join("machine-a");
        let second = scratch.path().join("machine-b");
        artifact_fixture(&first, false);
        artifact_fixture(&second, true);
        let packed = pack_web_artifact(&first).unwrap();
        let other = pack_web_artifact(&second).unwrap();
        assert_eq!(packed.digest, other.digest);
        assert_eq!(packed.tar, other.tar);
        assert_eq!(packed.manifest, other.manifest);
        assert_eq!(packed.tag, "web_fixture-web-1.0.0");
        let mut archive = tar::Archive::new(&packed.tar[..]);
        let paths = archive
            .entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let header = entry.header();
                assert_eq!(header.mtime().unwrap(), 0);
                assert_eq!(header.uid().unwrap(), 0);
                assert_eq!(header.gid().unwrap(), 0);
                assert_eq!(header.mode().unwrap(), 0o644);
                assert_eq!(header.username().unwrap(), Some(""));
                assert_eq!(header.groupname().unwrap(), Some(""));
                entry.path().unwrap().into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            ["dist/assets/app.js", "dist/index.html", "web.json"].map(PathBuf::from)
        );
        fs::write(second.join("dist/assets/app.js"), b"scripX").unwrap();
        assert_ne!(packed.digest, pack_web_artifact(&second).unwrap().digest);
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
        assert!(
            pack_web_artifact(&root)
                .unwrap_err()
                .to_string()
                .contains("not canonical JSON")
        );
        fs::write(root.join(WEB_MANIFEST_FILE), metadata).unwrap();
        fs::remove_file(root.join("dist/index.html")).unwrap();
        assert!(
            pack_web_artifact(&root)
                .unwrap_err()
                .to_string()
                .contains("no dist/index.html")
        );
        fs::write(root.join("dist/index.html"), "index").unwrap();
        std::os::unix::fs::symlink(root.join(WEB_MANIFEST_FILE), root.join("dist/link")).unwrap();
        assert!(
            pack_web_artifact(&root)
                .unwrap_err()
                .to_string()
                .contains("not a regular file or directory")
        );
    }

    fn fixture_registry(scratch: &ScratchRoot, server: &Server) -> PackageRegistry {
        let auth = scratch.path().join("registry-auth.json");
        fs::write(
            &auth,
            serde_json::to_vec(&serde_json::json!({"auths": {server.reference.registry(): {
                "username": "fixture", "password": "fixture"
            }}}))
            .unwrap(),
        )
        .unwrap();
        PackageRegistry {
            artifact_base: format!("{}/web", server.reference.registry()),
            registry_auth_file: auth,
            insecure_registry: true,
            oci_ca_paths: Vec::new(),
        }
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
        assert_eq!(first.package, "web_fixture@1.0.0");
        assert_eq!(server.requests(), 4);
    }

    #[tokio::test]
    async fn web_pull_refuses_absence_and_manifest_or_layer_digest_mismatches() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack_web_artifact(scratch.path()).unwrap();
        for (responses, digest, reason) in [
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
            let error = pull_web_artifact(&fixture_registry(&scratch, &server), &digest)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(reason), "{error}");
            assert!(error.contains(&digest), "{error}");
        }
    }

    #[test]
    fn web_metadata_requires_one_canonical_web_json() {
        let mut empty = tar::Builder::new(Vec::new());
        empty.finish().unwrap();
        assert!(read_web_manifest(&empty.into_inner().unwrap()).is_err());
        for (bytes, copies, reason) in [
            (b"{}\n".to_vec(), 1, "missing field"),
            (
                [
                    include_bytes!("../tests/support/web-artifact/web.json").as_slice(),
                    b"\n",
                ]
                .concat(),
                1,
                "not canonical",
            ),
            (
                include_bytes!("../tests/support/web-artifact/web.json").to_vec(),
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
            let error = read_web_manifest(&archive.into_inner().unwrap()).unwrap_err();
            assert!(error.to_string().contains(reason), "{error:#}");
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
        assert_eq!(result.digest, packed.digest);
        assert_eq!(server.requests(), 1);
    }

    #[tokio::test]
    async fn web_push_refuses_a_different_digest_and_names_the_tag_and_both_digests() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack_web_artifact(scratch.path()).unwrap();
        fs::write(scratch.path().join("dist/index.html"), "other").unwrap();
        let changed = pack_web_artifact(scratch.path()).unwrap();
        let server = Server::start(vec![(200, packed.manifest.clone())]).await;
        let error = push_web_artifact(&changed, &fixture_registry(&scratch, &server))
            .await
            .unwrap_err()
            .to_string();
        for detail in [&packed.tag, &packed.digest, &changed.digest] {
            assert!(error.contains(detail), "{error}");
        }
        assert_eq!(server.requests(), 1);
    }

    use object_store::memory::InMemory;
    use object_store::{ObjectStore as _, ObjectStoreExt as _};

    const HEAD: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SCOPE: &[u8] = br#"{"org":"acme","project":"receiving"}"#;

    #[tokio::test]
    async fn two_environments_with_the_same_web_digest_share_one_completed_upload() {
        use crate::environment::document::EnvironmentDocument;
        use futures_util::TryStreamExt as _;

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
        let first: EnvironmentDocument = serde_json::from_value(serde_json::json!({
            "org": "acme", "project": "web-fixture", "env": "dev", "target": "dev",
            "release": HEAD, "route_host": "dev.example.com", "policy": "dev",
            "applications": {}, "client": {"package": "web_fixture", "web": packed.digest},
        }))
        .unwrap();
        let mut second = first.clone();
        second.env = "staging".into();
        second.route_host = "staging.example.com".into();
        let store = InMemory::new();
        let upload = |environment: &EnvironmentDocument| {
            (
                environment.client.as_ref().unwrap().web.clone(),
                environment.org.clone(),
            )
        };
        let (web, org) = upload(&first);
        let (package, root, written) =
            super::upload_web_to_store(&registry, &web, &org, "prefix", &store)
                .await
                .unwrap();
        assert!(written);
        assert_eq!(package, "web_fixture");
        assert_eq!(
            root,
            format!("prefix/clients/web_fixture/{}", &packed.digest[7..])
        );
        let before = store.list(None).try_collect::<Vec<_>>().await.unwrap();
        assert_eq!(before.len(), 3);
        let scope = store
            .get(&object_store::path::Path::from(format!(
                "{root}/config.json"
            )))
            .await
            .unwrap();
        assert_eq!(
            scope.bytes().await.unwrap().as_ref(),
            br#"{"org":"acme","project":"web-fixture"}"#
        );
        let (web, org) = upload(&second);
        let (other_package, other_root, written) =
            super::upload_web_to_store(&registry, &web, &org, "prefix", &store)
                .await
                .unwrap();
        assert!(!written);
        assert_eq!(other_package, package);
        assert_eq!(other_root, root);
        assert_eq!(
            store.list(None).try_collect::<Vec<_>>().await.unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn an_existing_index_skips_all_writes_even_when_an_asset_is_absent() {
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
        let store = InMemory::new();
        let (_, root, written) =
            super::upload_web_to_store(&registry, &packed.digest, "acme", "", &store)
                .await
                .unwrap();
        assert!(written);
        let asset = object_store::path::Path::from(format!("{root}/assets/app.js"));
        store.delete(&asset).await.unwrap();
        let scope = object_store::path::Path::from(format!("{root}/config.json"));
        store.put(&scope, "sentinel".into()).await.unwrap();
        let (_, _, written) =
            super::upload_web_to_store(&registry, &packed.digest, "acme", "", &store)
                .await
                .unwrap();
        assert!(!written);
        assert!(store.head(&asset).await.is_err());
        assert_eq!(
            store
                .get(&scope)
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"sentinel"
        );
    }

    #[tokio::test]
    async fn upload_refuses_digest_mismatches_and_non_web_artifacts_before_any_write() {
        use futures_util::TryStreamExt as _;

        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack_web_artifact(scratch.path()).unwrap();
        let mut non_web: serde_json::Value = serde_json::from_slice(&packed.manifest).unwrap();
        non_web["artifactType"] = serde_json::json!("application/vnd.wamn.package.v1");
        let non_web = canonical_json_bytes(&non_web);
        for (responses, digest, reason) in [
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
            let error = super::upload_web_to_store(
                &fixture_registry(&scratch, &server),
                &digest,
                "acme",
                "",
                &store,
            )
            .await
            .unwrap_err();
            assert!(format!("{error:#}").contains(reason), "{error:#}");
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

    #[tokio::test]
    async fn a_rerun_keeps_equal_objects_and_the_service_worker_is_not_cached() {
        let dist = std::env::temp_dir().join(format!("wamn-web-rerun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dist);
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(dist.join("index.html"), "index").unwrap();
        std::fs::write(dist.join("sw.js"), "worker").unwrap();
        std::fs::write(dist.join("assets/main.css"), "style").unwrap();
        let store = InMemory::new();
        // An earlier run wrote one asset, then stopped.
        store
            .put(
                &object_store::path::Path::from(format!("{HEAD}/assets/main.css")),
                "style".into(),
            )
            .await
            .unwrap();
        let mut files = Vec::new();
        collect(&dist, &mut files).unwrap();
        write_files(&store, HEAD, &dist, &mut files, SCOPE)
            .await
            .expect("the rerun keeps the equal asset and writes the rest");
        let worker = store
            .get(&object_store::path::Path::from(format!("{HEAD}/sw.js")))
            .await
            .unwrap();
        assert_eq!(
            worker
                .attributes
                .get(&object_store::Attribute::CacheControl)
                .map(AsRef::as_ref),
            Some(super::INDEX_CACHE)
        );
        assert_eq!(&worker.bytes().await.unwrap()[..], b"worker");
        std::fs::remove_dir_all(&dist).unwrap();
    }

    #[tokio::test]
    async fn an_existing_object_is_never_replaced() {
        let dist = std::env::temp_dir().join(format!("wamn-web-upload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dist);
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(dist.join("index.html"), "first").unwrap();
        std::fs::write(dist.join("assets/app.js"), "first").unwrap();
        let store = InMemory::new();
        let mut files = Vec::new();
        collect(&dist, &mut files).unwrap();
        write_files(&store, HEAD, &dist, &mut files, SCOPE)
            .await
            .expect("the first upload writes");

        std::fs::write(dist.join("index.html"), "second").unwrap();
        std::fs::write(dist.join("assets/app.js"), "second").unwrap();
        let error = write_files(&store, HEAD, &dist, &mut files, SCOPE)
            .await
            .expect_err("the second upload refuses");
        assert!(
            error
                .to_string()
                .starts_with(&format!("write {HEAD}/assets/app.js")),
            "{error}"
        );
        for (name, bytes) in [
            ("index.html", &b"first"[..]),
            ("assets/app.js", b"first"),
            ("config.json", SCOPE),
        ] {
            let stored = store
                .get(&object_store::path::Path::from(format!("{HEAD}/{name}")))
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            assert_eq!(&stored[..], bytes, "{name}");
        }
        std::fs::remove_dir_all(&dist).unwrap();
    }

    #[tokio::test]
    async fn platform_ui_upload_uses_the_build_record_and_a_version_prefix_with_org_only() {
        use crate::platform_build::{
            BUILD_RECORD_FILE, PlatformBuildRecord, platform_ui_digest, platform_ui_dist,
        };
        let repository =
            std::env::temp_dir().join(format!("wamn-platform-upload-{}", std::process::id()));
        let dist = platform_ui_dist(&repository);
        let chart = repository.join("chart");
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::create_dir_all(&chart).unwrap();
        std::fs::write(dist.join("index.html"), b"platform index").unwrap();
        std::fs::write(dist.join("assets/ui.js"), b"platform script").unwrap();
        std::fs::write(chart.join("Chart.yaml"), "version: 0.1.7").unwrap();
        let platform = repository.join("deploy/platform/release");
        std::fs::create_dir_all(&platform).unwrap();
        std::fs::write(platform.join("Chart.yaml"), "version: 0.1.7").unwrap();
        std::fs::write(
            chart.join(BUILD_RECORD_FILE),
            serde_json::to_vec(&PlatformBuildRecord {
                platform_ui: platform_ui_digest(&dist).unwrap(),
                stage_durations_ms: std::collections::BTreeMap::new(),
            })
            .unwrap(),
        )
        .unwrap();
        let (source, root) = super::platform_ui_source(&repository, &chart).unwrap();
        assert_eq!(source, dist);
        assert_eq!(root, "platform-ui/0.1.7");
        assert_eq!(super::PLATFORM_UI_BUCKET, "gs://wamn-dev-web");
        let store = InMemory::new();
        super::write_platform_ui(&store, &root, &source, "dkk")
            .await
            .unwrap();
        for (name, bytes) in [
            ("index.html", &b"platform index"[..]),
            ("assets/ui.js", &b"platform script"[..]),
            ("config.json", &br#"{"org":"dkk"}"#[..]),
        ] {
            let stored = store
                .get(&object_store::path::Path::from(format!("{root}/{name}")))
                .await
                .unwrap();
            assert_eq!(stored.bytes().await.unwrap().as_ref(), bytes);
        }
        std::fs::write(dist.join("assets/ui.js"), b"changed").unwrap();
        assert!(
            super::platform_ui_source(&repository, &chart)
                .unwrap_err()
                .to_string()
                .contains("differs from the platform build record")
        );
        std::fs::remove_file(chart.join(BUILD_RECORD_FILE)).unwrap();
        assert!(
            super::platform_ui_source(&repository, &chart)
                .unwrap_err()
                .to_string()
                .contains("run `wamn-ctl build-platform`")
        );
        std::fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn the_sink_scheme_selects_the_builder() {
        assert_eq!(
            Sink::parse("gs://wamn-dev-web/clients").unwrap(),
            Sink {
                scheme: Scheme::Gcs,
                bucket: "wamn-dev-web",
                prefix: "clients",
            }
        );
        assert_eq!(
            Sink::parse("s3://web").unwrap(),
            Sink {
                scheme: Scheme::S3,
                bucket: "web",
                prefix: "",
            }
        );
        assert!(Sink::parse("https://web").is_err());
    }
}
