//! Publish and upload the shared platform UI without application package metadata.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use object_store::{ObjectStore, ObjectStoreExt as _};
use wamn_digest::canonical_json_bytes;
use wamn_package_artifact::{PackageRegistry, ensure_tag};

use super::{
    INDEX_CACHE, PackedWebArtifact, Sink, UploadedClient, WEB_MANIFEST_FILE, WebArtifactError,
    WebArtifactErrorType, artifact_files, cache_control, content_type, pack_archive, pull_archive,
    put, web_dist_files,
};
use crate::web_config::{
    CONFIG_CACHE, CONFIG_CONTENT_TYPE, CONFIG_FILE, platform_config_file_bytes,
};

const ARTIFACT_TYPE: &str = "application/vnd.wamn.platform-ui.v1";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: String,
}

/// Pack UI files and UI metadata into the deterministic web archive format.
///
/// # Errors
/// Refuses invalid versions, missing indexes, links, or unsupported browser files.
pub fn pack(dist: &Path, version: &str) -> Result<PackedWebArtifact, WebArtifactError> {
    (|| {
        ensure_tag(version)?;
        let mut files = BTreeMap::new();
        artifact_files(dist, Path::new(""), &mut files)?;
        ensure!(
            files.contains_key(Path::new("index.html")),
            "platform UI has no index.html"
        );
        let mut archive = BTreeMap::new();
        for (name, bytes) in files {
            let relative = name.to_str().context("platform UI paths must be UTF-8")?;
            cache_control(relative)?;
            content_type(relative)?;
            archive.insert(Path::new("dist").join(name), bytes);
        }
        archive.insert(
            PathBuf::from(WEB_MANIFEST_FILE),
            canonical_json_bytes(&serde_json::to_value(Metadata {
                version: version.into(),
            })?),
        );
        pack_archive(version.into(), archive, ARTIFACT_TYPE)
    })()
    .map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Pack,
        context: dist.display().to_string(),
        source,
    })
}

fn read_metadata(tar: &[u8]) -> anyhow::Result<()> {
    let mut found = false;
    for entry in tar::Archive::new(tar).entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() != Path::new(WEB_MANIFEST_FILE) {
            continue;
        }
        ensure!(!found, "platform UI repeats web.json");
        ensure!(
            entry.header().entry_type().is_file(),
            "web.json must be a regular file"
        );
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        let metadata: Metadata = serde_json::from_slice(&bytes)?;
        ensure_tag(&metadata.version)?;
        ensure!(
            bytes == canonical_json_bytes(&serde_json::to_value(metadata)?),
            "web.json is not canonical JSON"
        );
        found = true;
    }
    ensure!(found, "platform UI has no web.json");
    Ok(())
}

/// Upload a verified platform UI digest without reading a checkout or build output.
///
/// # Errors
/// Refuses invalid artifacts, failed transfers, or conflicting bucket objects.
pub async fn upload(
    registry: &PackageRegistry,
    digest: &str,
    org: &str,
    bucket: &str,
) -> Result<UploadedClient, WebArtifactError> {
    async {
        let sink = Sink::parse(bucket)?;
        let store = sink.store()?;
        let (root, written) =
            upload_to_store(registry, digest, org, sink.prefix, store.as_ref()).await?;
        Ok(UploadedClient {
            package: "platform-ui".into(),
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

/// Copy a verified artifact to an object store, with org configuration outside its prefix.
///
/// # Errors
/// Refuses invalid artifacts before writing, or conflicting existing objects.
pub async fn upload_to_store(
    registry: &PackageRegistry,
    digest: &str,
    org: &str,
    prefix: &str,
    store: &dyn ObjectStore,
) -> anyhow::Result<(String, bool)> {
    ensure_tag(org)?;
    let tar = pull_archive(registry, digest, ARTIFACT_TYPE).await?;
    read_metadata(&tar)?;
    let mut files = web_dist_files(&tar)?;
    let index = files
        .remove("index.html")
        .expect("verified web archive has an index");
    let base = prefix.trim_matches('/');
    let base = if base.is_empty() {
        String::new()
    } else {
        format!("{base}/")
    };
    let root = format!("{base}platform-ui/sha256-{}", &digest[7..]);
    let config_root = format!("{base}platform-ui/orgs/{org}");
    put(
        store,
        &config_root,
        CONFIG_FILE,
        platform_config_file_bytes(org),
        CONFIG_CACHE,
        CONFIG_CONTENT_TYPE,
    )
    .await?;
    match store.head(&format!("{root}/index.html").into()).await {
        Ok(_) => return Ok((root, false)),
        Err(object_store::Error::NotFound { .. }) => {}
        Err(error) => return Err(error).context("read the platform UI completion index"),
    }
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
        "index.html",
        index,
        INDEX_CACHE,
        content_type("index.html")?,
    )
    .await?;
    Ok((root, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::TryStreamExt as _;
    use object_store::memory::InMemory;
    use wamn_digest::bytes_sha256;
    use wamn_package_artifact::registry_pull::test_server::Server;
    use wamn_test_infrastructure::scratch::ScratchRoot;

    use super::super::test_support::{artifact_fixture, fixture_registry};

    #[test]
    fn publication_is_deterministic_and_contains_only_ui_files_and_metadata() {
        let scratch = ScratchRoot::create().unwrap();
        let first = scratch.path().join("first");
        let second = scratch.path().join("second");
        artifact_fixture(&first, false);
        artifact_fixture(&second, true);
        let packed = pack(&first.join("dist"), "1.2.3").unwrap();
        let other = pack(&second.join("dist"), "1.2.3").unwrap();
        assert_eq!(packed.digest, other.digest);
        assert_eq!(packed.manifest, other.manifest);
        assert_eq!(packed.tar, other.tar);
        let mut expected = BTreeMap::new();
        artifact_files(&first, Path::new("dist"), &mut expected).unwrap();
        expected.insert(
            PathBuf::from(WEB_MANIFEST_FILE),
            canonical_json_bytes(&serde_json::json!({"version":"1.2.3"})),
        );
        let actual = tar::Archive::new(packed.tar.as_slice())
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                assert_eq!(entry.header().mtime().unwrap(), 0);
                assert_eq!(entry.header().mode().unwrap(), 0o644);
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                (entry.path().unwrap().into_owned(), bytes)
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(actual, expected);
        std::fs::write(second.join("dist/index.html"), "changed").unwrap();
        assert_ne!(
            packed.digest,
            pack(&second.join("dist"), "1.2.3").unwrap().digest
        );
        std::fs::write(second.join("dist/config.json"), "org data").unwrap();
        assert!(pack(&second.join("dist"), "1.2.3").is_err());
    }

    #[tokio::test]
    async fn missing_corrupt_and_application_artifacts_write_nothing() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack(&scratch.path().join("dist"), "1.2.3").unwrap();
        let application = super::super::pack_web_artifact(scratch.path()).unwrap();
        for (responses, digest) in [
            (vec![(404, Vec::new())], packed.digest.as_str()),
            (
                vec![(200, packed.manifest.clone())],
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            (
                vec![(200, packed.manifest.clone()), (200, b"corrupt".to_vec())],
                packed.digest.as_str(),
            ),
            (
                vec![(200, application.manifest)],
                application.digest.as_str(),
            ),
        ] {
            let server = Server::start(responses).await;
            let store = InMemory::new();
            assert!(
                upload_to_store(
                    &fixture_registry(&scratch, &server),
                    digest,
                    "acme",
                    "",
                    &store
                )
                .await
                .is_err()
            );
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
    async fn repeated_uploads_share_only_verified_files_and_keep_org_configuration_separate() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        std::fs::write(scratch.path().join("dist/sw.js"), "worker").unwrap();
        let first = pack(&scratch.path().join("dist"), "1.2.3").unwrap();
        std::fs::write(scratch.path().join("dist/index.html"), "changed").unwrap();
        let second = pack(&scratch.path().join("dist"), "1.2.3").unwrap();
        let mut responses = Vec::new();
        for packed in [&first, &first, &first, &second] {
            responses.extend([(200, packed.manifest.clone()), (200, packed.tar.clone())]);
        }
        let server = Server::start(responses).await;
        let registry = fixture_registry(&scratch, &server);
        let store = InMemory::new();
        let (root, written) = upload_to_store(&registry, &first.digest, "acme", "prefix", &store)
            .await
            .unwrap();
        assert!(written);
        for org in ["acme", "other"] {
            let (same_root, written) =
                upload_to_store(&registry, &first.digest, org, "prefix", &store)
                    .await
                    .unwrap();
            assert_eq!(same_root, root);
            assert!(!written);
            let target = store
                .get(&format!("prefix/platform-ui/orgs/{org}/config.json").into())
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            assert_eq!(target.as_ref(), platform_config_file_bytes(org));
        }
        // Derive the expected browser objects from the artifact, including new assets.
        let files = web_dist_files(&first.tar).unwrap();
        let stored = store
            .list(Some(&object_store::path::Path::from(root.clone())))
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(stored.len(), files.len());
        for (name, bytes) in files {
            let result = store.get(&format!("{root}/{name}").into()).await.unwrap();
            assert_eq!(
                result
                    .attributes
                    .get(&object_store::Attribute::CacheControl)
                    .map(AsRef::as_ref),
                Some(cache_control(&name).unwrap())
            );
            assert_eq!(result.bytes().await.unwrap().as_ref(), bytes);
        }
        assert!(
            store
                .head(&format!("{root}/config.json").into())
                .await
                .is_err()
        );
        let (changed_root, written) =
            upload_to_store(&registry, &second.digest, "acme", "prefix", &store)
                .await
                .unwrap();
        assert!(written);
        assert_ne!(root, changed_root);
        assert!(root.ends_with(&first.digest.replace(':', "-")));
        assert!(changed_root.ends_with(&second.digest.replace(':', "-")));
    }

    #[tokio::test]
    async fn partial_upload_keeps_equal_objects_and_conflicts_never_publish_the_index() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack(&scratch.path().join("dist"), "1.2.3").unwrap();
        for conflict in [true, false] {
            let server = Server::start(vec![
                (200, packed.manifest.clone()),
                (200, packed.tar.clone()),
            ])
            .await;
            let store = InMemory::new();
            let root = format!("platform-ui/{}", packed.digest.replace(':', "-"));
            let files = web_dist_files(&packed.tar).unwrap();
            let (asset, bytes) = files
                .iter()
                .find(|(name, _)| name.as_str() != "index.html")
                .unwrap();
            let original = if conflict {
                b"conflict".to_vec()
            } else {
                bytes.clone()
            };
            let key = object_store::path::Path::from(format!("{root}/{asset}"));
            store.put(&key, original.clone().into()).await.unwrap();
            let result = upload_to_store(
                &fixture_registry(&scratch, &server),
                &packed.digest,
                "acme",
                "",
                &store,
            )
            .await;
            assert_eq!(result.is_err(), conflict);
            assert_eq!(
                store
                    .head(&format!("{root}/index.html").into())
                    .await
                    .is_err(),
                conflict
            );
            assert_eq!(
                bytes_sha256(&store.get(&key).await.unwrap().bytes().await.unwrap()),
                bytes_sha256(&original)
            );
        }
    }
}
