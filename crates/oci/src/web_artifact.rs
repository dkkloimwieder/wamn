//! Build and transfer digest-pinned browser artifacts.

pub mod platform_ui;

use anyhow::{Context as _, bail, ensure};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
use std::path::{Path, PathBuf};
use wamn_codegen::fingerprint;
use wamn_digest::{bytes_sha256, canonical_json_bytes};
use wamn_oci_reference::OciTag;
use wamn_release::web_artifact::{
    PackedWebArtifact, PushedWebArtifact, WEB_MANIFEST_FILE, WebArtifactError,
    WebArtifactErrorType, WebArtifactReader, WebManifest, WebPushDisposition,
};

use crate::package_artifact::registry_credentials::read_registry_credentials;
use crate::package_artifact::{PackageRegistry, Repository, registry_auth};

/// Standalone output below a package's build directory.
const ARTIFACT_DIRECTORY: &str = "web-artifact";
/// OCI artifact type for an independent web client.
pub const WEB_ARTIFACT_TYPE: &str = "application/vnd.wamn.web.v1";
/// Media type of the tar that contains `dist/` and `web.json`.
pub const WEB_TAR_MEDIA_TYPE: &str = "application/vnd.wamn.web.v1.tar";
const EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
const EMPTY_CONFIG: &[u8] = b"{}";

fn web_manifest(
    compiled: &[u8],
    digest: wamn_digest::Digest,
    version: String,
) -> anyhow::Result<WebManifest> {
    let manifest = wamn_component_policy::PackageManifest::from_slice(compiled)?;
    let client = manifest.client_package.with_context(|| {
        format!(
            "package {}@{} has no client_package and no web artifact",
            manifest.package.id, manifest.package.version
        )
    })?;
    Ok(WebManifest {
        client_package: client.name,
        version,
        package: wamn_package::PackageCoordinate::new(
            manifest.package.id,
            manifest.package.version,
        )?,
        contract_digest: digest,
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

        ensure!(!web.version.is_empty(), "web.json has no version");
        metadata = Some(web);
    }
    metadata.context("web artifact has no web.json")
}

/// The directory that holds `dist/` and `web.json` for one standalone build.
pub fn artifact_output(package_root: &Path) -> PathBuf {
    wamn_codegen::output_root(package_root).join(ARTIFACT_DIRECTORY)
}

/// Build browser files against the finished package's compiled contract.
/// The web version comes from the package's `web/package.json`.
///
/// # Errors
///
/// Refuses a missing fingerprint, changed manifest, missing web version or
/// client, or a failed Vite build.
pub async fn build_web_artifact(
    package_root: &Path,
    compile: impl AsyncFn(&Path, &Path) -> anyhow::Result<()>,
) -> Result<PathBuf, WebArtifactError> {
    async {
        let output = wamn_codegen::output_root(package_root);
        let finished = fingerprint::read(&output)?;
        let (compiled, digest) =
            fingerprint::read_artifact(&output, &finished, Path::new("wamn.json"))?;
        let path = package_root.join("web/package.json");
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let package: serde_json::Value =
            serde_json::from_slice(&bytes).with_context(|| format!("read {}", path.display()))?;
        let version = package
            .get("version")
            .and_then(serde_json::Value::as_str)
            .filter(|version| !version.is_empty())
            .with_context(|| format!("{} has no nonempty version", path.display()))?;
        let manifest = web_manifest(&compiled, digest, version.to_owned())?;
        let root = artifact_output(package_root);
        std::fs::create_dir_all(&root)?;
        let sidecar = root.join(WEB_MANIFEST_FILE);
        if sidecar.exists() {
            std::fs::remove_file(&sidecar)?;
        }
        compile(package_root, &root.join("dist")).await?;
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
    ensure!(!metadata.version.is_empty(), "web.json has no version");

    let package = metadata.package.package_id();
    let tag = format!("{package}-web-{}", metadata.version);
    let tag = tag.parse::<OciTag>()?;
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
    tag: OciTag,
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
        let base = wamn_oci_reference::parse_component_artifact_base(&registry.artifact_base)?;
        let credentials = read_registry_credentials(&registry.registry_auth_file, base.registry())?;
        Repository::with_auth(registry, &packed.tag, registry_auth(credentials.as_ref()))
    })()
    .map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Push,
        context: format!("{}:{}", registry.artifact_base, packed.tag),
        source,
    })?;
    push_archive(packed, &repository).await
}

/// Publish a packed browser artifact through an open repository.
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
                        .push_blob(&repository.reference, bytes.to_vec(), &bytes_sha256(bytes).to_string())
                        .await
                        .with_context(|| format!("push a blob of web artifact {}", repository.reference))?;
                }
                repository.client
                    .push_manifest_raw(
                        &repository.reference,
                        packed.manifest.clone(),
                        http::HeaderValue::from_static(OCI_IMAGE_MEDIA_TYPE),
                    )
                    .await
                    .with_context(|| format!("push web artifact {}", repository.reference))?;
                let pulled = repository.manifest().await?;
                ensure!(
                    pulled.as_ref().map(|(_, digest)| *digest) == Some(packed.digest),
                    "pushed web artifact {} reads back as {}, not {}",
                    repository.reference,
                    pulled.as_ref().map_or_else(|| "nothing".to_owned(), |(_, digest)| digest.to_string()),
                    packed.digest
                );
                WebPushDisposition::Pushed
            }
        };
        Ok(PushedWebArtifact {
            tag: packed.tag.clone(),
            digest: packed.digest,
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
    digest: &wamn_digest::Digest,
) -> Result<WebManifest, WebArtifactError> {
    pull_web(registry, digest)
        .await
        .map(|(metadata, _)| metadata)
}

async fn pull_web(
    registry: &PackageRegistry,
    digest: &wamn_digest::Digest,
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
    digest: &wamn_digest::Digest,
    artifact_type: &str,
) -> anyhow::Result<Vec<u8>> {
    let repository = Repository::open(registry, digest)?;
    let (bytes, _) = repository
        .manifest()
        .await?
        .with_context(|| format!("web artifact {digest} does not exist"))?;
    ensure!(
        bytes_sha256(&bytes) == *digest,
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
    let tar = crate::package_artifact::registry_pull::blob(
        &repository.client,
        &repository.reference,
        layer,
        tokio::time::Instant::now() + std::time::Duration::from_secs(30),
    )
    .await?;
    ensure!(
        bytes_sha256(&tar) == layer.digest.parse::<wamn_digest::Digest>()?,
        "web artifact {digest} layer does not hash to {}",
        layer.digest
    );
    Ok(tar)
}

/// Read verified browser artifacts from the configured registry.
#[derive(Debug)]
pub struct RegistryWebArtifacts<'a>(pub &'a PackageRegistry);

#[async_trait::async_trait]
impl WebArtifactReader for RegistryWebArtifacts<'_> {
    fn artifact_base(&self) -> &str {
        &self.0.artifact_base
    }

    async fn read_web(
        &self,
        digest: &wamn_digest::Digest,
    ) -> Result<(WebManifest, Vec<u8>), WebArtifactError> {
        pull_web(self.0, digest).await
    }

    async fn read_platform_ui(&self, digest: &wamn_digest::Digest) -> anyhow::Result<Vec<u8>> {
        let tar = pull_archive(self.0, digest, platform_ui::ARTIFACT_TYPE).await?;
        platform_ui::read_metadata(&tar)?;
        Ok(tar)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use wamn_codegen::fingerprint::{self, Fingerprint};
    use wamn_digest::{bytes_sha256, canonical_json_bytes};
    use wamn_test_infrastructure::publish::{
        Server, WEB_MANIFEST, artifact_fixture, fixture_registry,
    };
    use wamn_test_infrastructure::scratch::ScratchRoot;

    use super::{
        RegistryWebArtifacts, WEB_MANIFEST_FILE, WebPushDisposition, build_web_artifact,
        pack_web_artifact, pull_web_artifact, push_web_artifact, read_web_manifest,
    };

    fn build_fixture(client: Option<&str>) -> (Fingerprint, Vec<u8>) {
        let mut manifest = serde_json::json!({
            "package": {"id": "web_fixture", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "web_fixture_data_access", "state": "satisfied"},
            "models": {}, "components": {}, "connections": {},
        });
        if let Some(client) = client {
            manifest["client_package"] = serde_json::json!({"name": client});
        }
        let compiled = canonical_json_bytes(&manifest);
        let record = Fingerprint {
            input: bytes_sha256(b"fixture"),
            outputs: BTreeMap::from([("wamn.json".into(), bytes_sha256(&compiled))]),
        };
        (record, compiled)
    }

    fn write_build(scratch: &ScratchRoot, record: &Fingerprint, compiled: &[u8]) -> PathBuf {
        let package = scratch.path().join("apps/web_fixture");
        fs::create_dir_all(package.join("web")).unwrap();
        fs::write(package.join("web/package.json"), r#"{"version":"0.2.0"}"#).unwrap();
        let output = wamn_codegen::output_root(&package);
        fs::create_dir_all(&output).unwrap();
        fingerprint::write(&output, record).unwrap();
        fs::write(output.join("wamn.json"), compiled).unwrap();
        package
    }

    #[tokio::test]
    async fn web_build_refuses_a_manifest_that_differs_from_the_finished_build() {
        let scratch = ScratchRoot::create().unwrap();
        let (record, mut compiled) = build_fixture(Some("@wamn/web-fixture-client"));
        compiled.push(b' ');
        let package = write_build(&scratch, &record, &compiled);
        let _error = build_web_artifact(&package, wamn_build::build_web_client)
            .await
            .unwrap_err()
            .to_string();
    }

    #[tokio::test]
    async fn web_build_refuses_a_package_without_a_client() {
        let scratch = ScratchRoot::create().unwrap();
        let (record, compiled) = build_fixture(None);
        let package = write_build(&scratch, &record, &compiled);
        let _error = build_web_artifact(&package, wamn_build::build_web_client)
            .await
            .unwrap_err()
            .to_string();
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
            let _error = build_web_artifact(&package, wamn_build::build_web_client)
                .await
                .unwrap_err()
                .to_string();
        }
    }

    #[tokio::test]
    async fn web_build_rebuilds_changed_web_source_without_changing_the_build_fingerprint() {
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
        let output = build_web_artifact(&package, wamn_build::build_web_client)
            .await
            .unwrap();
        let first = pack_web_artifact(&output).unwrap();

        fs::write(source.join("index.html"), "other").unwrap();
        fs::write(
            source.join("package.json"),
            r#"{"version":"0.2.1","scripts":{"build":"node build.cjs"}}"#,
        )
        .unwrap();
        build_web_artifact(&package, wamn_build::build_web_client)
            .await
            .unwrap();
        let second = pack_web_artifact(&output).unwrap();

        assert_ne!(
            (first.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            (second.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap()
        );
        let metadata = read_web_manifest(&second.tar).unwrap();

        assert_eq!(
            (metadata.contract_digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            (record.outputs[Path::new("wamn.json")].to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap()
        );

        assert_eq!(
            fingerprint::read(&wamn_codegen::output_root(&package)).unwrap(),
            record
        );
        fs::write(source.join("build.cjs"), "process.exit(1);").unwrap();
        assert!(
            build_web_artifact(&package, wamn_build::build_web_client)
                .await
                .is_err()
        );
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
            (vec![(404, Vec::new())], packed.digest, "does not exist"),
            (
                vec![(200, packed.manifest.clone())],
                bytes_sha256(b"another manifest"),
                "Invalid digest",
            ),
            (
                vec![(200, packed.manifest.clone()), (200, b"wrong tar".to_vec())],
                packed.digest,
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
            ([WEB_MANIFEST, b"\n"].concat(), 1, "not canonical"),
            (WEB_MANIFEST.to_vec(), 2, "repeats web.json"),
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
            (result.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            (packed.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap()
        );
        assert_eq!(server.requests(), 1);
    }

    #[tokio::test]
    async fn web_push_refuses_a_conflicting_publication() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack_web_artifact(scratch.path()).unwrap();
        fs::write(scratch.path().join("dist/index.html"), "other").unwrap();
        let changed = pack_web_artifact(scratch.path()).unwrap();
        assert_ne!(
            (packed.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            (changed.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap()
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
                packed.digest,
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
            let _error = wamn_release::web_artifact::upload::upload_web_to_store(
                &RegistryWebArtifacts(&fixture_registry(&scratch, &server)),
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
}
