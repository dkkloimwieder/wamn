//! Publish and upload the shared platform UI without application package metadata.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use wamn_digest::canonical_json_bytes;
use wamn_oci_reference::OciTag;

use wamn_release::web_artifact::{
    PackedWebArtifact, WEB_MANIFEST_FILE, WebArtifactError, WebArtifactErrorType, content_type,
    validate_path,
};

use super::{artifact_files, pack_archive};

pub(super) const ARTIFACT_TYPE: &str = "application/vnd.wamn.platform-ui.v1";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: OciTag,
}

/// Pack UI files and UI metadata into the deterministic web archive format.
///
/// # Errors
/// Refuses missing indexes, links, or unsupported browser files.
pub fn pack(dist: &Path, version: &OciTag) -> Result<PackedWebArtifact, WebArtifactError> {
    (|| {
        let mut files = BTreeMap::new();
        artifact_files(dist, Path::new(""), &mut files)?;
        ensure!(
            files.contains_key(Path::new("index.html")),
            "platform UI has no index.html"
        );
        let mut archive = BTreeMap::new();
        for (name, bytes) in files {
            let relative = name.to_str().context("platform UI paths must be UTF-8")?;
            validate_path(relative)?;
            content_type(relative)?;
            archive.insert(Path::new("dist").join(name), bytes);
        }
        archive.insert(
            PathBuf::from(WEB_MANIFEST_FILE),
            canonical_json_bytes(&serde_json::to_value(Metadata {
                version: version.clone(),
            })?),
        );
        pack_archive(version.clone(), archive, ARTIFACT_TYPE)
    })()
    .map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Pack,
        context: dist.display().to_string(),
        source,
    })
}

pub(super) fn read_metadata(tar: &[u8]) -> anyhow::Result<()> {
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
        ensure!(
            bytes == canonical_json_bytes(&serde_json::to_value(metadata)?),
            "web.json is not canonical JSON"
        );
        found = true;
    }
    ensure!(found, "platform UI has no web.json");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::RegistryWebArtifacts;
    use super::*;
    use futures_util::TryStreamExt as _;
    use object_store::{ObjectStore as _, ObjectStoreExt as _, memory::InMemory};
    use wamn_digest::bytes_sha256;
    use wamn_storage::web_artifact::upload_platform_ui_to_store;
    use wamn_test_infrastructure::publish::{Server, artifact_fixture, fixture_registry};
    use wamn_test_infrastructure::scratch::ScratchRoot;

    #[test]
    fn publication_is_deterministic_and_contains_only_ui_files_and_metadata() {
        let scratch = ScratchRoot::create().unwrap();
        let first = scratch.path().join("first");
        let second = scratch.path().join("second");
        artifact_fixture(&first, false);
        artifact_fixture(&second, true);
        let packed = pack(&first.join("dist"), &"1.2.3".parse().unwrap()).unwrap();
        let other = pack(&second.join("dist"), &"1.2.3".parse().unwrap()).unwrap();
        assert_eq!(
            (packed.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            (other.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap()
        );
        std::fs::write(second.join("dist/index.html"), "changed").unwrap();
        assert_ne!(
            (packed.digest.to_string())
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            (pack(&second.join("dist"), &"1.2.3".parse().unwrap())
                .unwrap()
                .digest
                .to_string())
            .parse::<wamn_digest::Digest>()
            .unwrap()
        );
        std::fs::write(second.join("dist/config.json"), "org data").unwrap();
        assert!(pack(&second.join("dist"), &"1.2.3".parse().unwrap()).is_err());
    }

    #[tokio::test]
    async fn missing_corrupt_and_application_artifacts_write_nothing() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack(&scratch.path().join("dist"), &"1.2.3".parse().unwrap()).unwrap();
        let application = crate::web_artifact::pack_web_artifact(scratch.path()).unwrap();
        for (responses, digest) in [
            (vec![(404, Vec::new())], &packed.digest),
            (
                vec![(200, packed.manifest.clone())],
                &"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .parse::<wamn_digest::Digest>()
                    .unwrap(),
            ),
            (
                vec![(200, packed.manifest.clone()), (200, b"corrupt".to_vec())],
                &packed.digest,
            ),
            (vec![(200, application.manifest)], &application.digest),
        ] {
            let server = Server::start(responses).await;
            let store = InMemory::new();
            assert!(
                upload_platform_ui_to_store(
                    &RegistryWebArtifacts(&fixture_registry(&scratch, &server)),
                    digest,
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
    async fn partial_upload_keeps_equal_objects_and_conflicts_never_publish_the_index() {
        let scratch = ScratchRoot::create().unwrap();
        artifact_fixture(scratch.path(), false);
        let packed = pack(&scratch.path().join("dist"), &"1.2.3".parse().unwrap()).unwrap();
        for conflict in [true, false] {
            let server = Server::start(vec![
                (200, packed.manifest.clone()),
                (200, packed.tar.clone()),
            ])
            .await;
            let store = InMemory::new();
            let root = format!("platform-ui/sha256-{}", packed.digest.hex());
            let mut archive = tar::Archive::new(packed.tar.as_slice());
            let mut asset = archive
                .entries()
                .unwrap()
                .map(Result::unwrap)
                .find(|entry| {
                    let path = entry.path().unwrap();
                    path.starts_with("dist") && path.as_ref() != Path::new("dist/index.html")
                })
                .unwrap();
            let path = asset
                .path()
                .unwrap()
                .strip_prefix("dist")
                .unwrap()
                .to_path_buf();
            let mut original = Vec::new();
            if conflict {
                original.extend_from_slice(b"conflict");
            } else {
                std::io::Read::read_to_end(&mut asset, &mut original).unwrap();
            }
            let original_digest = bytes_sha256(&original);
            let key = object_store::path::Path::from(format!("{root}/{}", path.display()));
            store.put(&key, original.into()).await.unwrap();
            let result = upload_platform_ui_to_store(
                &RegistryWebArtifacts(&fixture_registry(&scratch, &server)),
                &packed.digest,
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
                original_digest
            );
        }
    }
}
