//! Publish and upload the shared platform UI without application package metadata.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use wamn_digest::canonical_json_bytes;
use wamn_oci_reference::OciTag;

use crate::web_artifact::{
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
    use super::*;
    use wamn_test_infrastructure::publish::artifact_fixture;
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
}
