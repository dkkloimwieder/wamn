use super::{Path, ensure};
use anyhow::Context as _;

/// The package and component repositories beside the release repository
/// `<registry>/<path>/releases`: `<registry>/<path>/packages` and
/// `<registry>/<path>/components`. Qualification reads the release's
/// packages and components there (docs/operations/delivery.md#release-qualification).
///
/// # Errors
///
/// When the release repository is not `<registry>/<path>/releases`.
pub fn sibling_bases(release_artifact_base: &str) -> anyhow::Result<(String, String)> {
    let parent = release_artifact_base
        .strip_suffix("/releases")
        .filter(|parent| parent.contains('/'))
        .with_context(|| {
            format!(
                "the release repository {release_artifact_base} is not <registry>/<path>/releases"
            )
        })?;
    Ok((format!("{parent}/packages"), format!("{parent}/components")))
}

pub(super) fn require_pinned_image(image: &str) -> anyhow::Result<()> {
    let (repository, digest) = image
        .rsplit_once("@sha256:")
        .context("candidate image must use an immutable repository@sha256:digest reference")?;
    ensure!(
        !repository.is_empty()
            && !repository.chars().any(char::is_whitespace)
            && digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "candidate image has an invalid immutable digest reference"
    );
    Ok(())
}

pub(super) fn require_manifest_path(path: &Path) -> anyhow::Result<()> {
    ensure!(
        path.is_absolute(),
        "candidate manifest path must be absolute"
    );
    Ok(())
}
