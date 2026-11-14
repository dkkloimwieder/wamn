//! Expire unpinned development package tags after seven days.

use std::collections::BTreeSet;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use wamn_catalog::ServingManifest;
use wamn_package_artifact::{
    CREATED_ANNOTATION, PACKAGE_ARTIFACT_TYPE, PackageRegistry, Repository, ensure_digest,
    is_dev_tag,
};

pub(crate) async fn prune_published(platform: &crate::environment::Platform) -> anyhow::Result<()> {
    let packages = platform
        .registry(
            platform
                .package_artifact_base
                .as_deref()
                .context("package artifact base is absent")?,
        )
        .await?;
    let releases = platform.registry(&platform.release_artifact_base).await?;
    let repository = Repository::open(&releases, "latest")?;
    let mut manifests = Vec::new();
    for tag in repository.tags().await? {
        let digest = format!("sha256:{tag}");
        if ensure_digest(&digest).is_ok() {
            manifests
                .push(crate::environment::analyse::pull_release_manifest(platform, &digest).await?);
        }
    }
    for tag in prune(&packages, &manifests, Utc::now()).await? {
        tracing::info!(tag, "removed expired development package tag");
    }
    Ok(())
}

/// Delete expired dev tags except coordinates pinned by any published release.
/// Missing publication times leave old data untouched. Registry errors stop pruning.
pub async fn prune(
    registry: &PackageRegistry,
    releases: &[ServingManifest],
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<String>> {
    let pinned: BTreeSet<_> = releases
        .iter()
        .flat_map(|release| &release.release.packages)
        .map(|package| format!("{}-{}", package.package_id(), package.package_version()))
        .collect();
    let repository = Repository::open(registry, "latest")?;
    let mut manifests = std::collections::BTreeMap::<_, (Vec<String>, Vec<u8>)>::new();
    for tag in repository.tags().await? {
        if let Some((bytes, digest)) = Repository::open(registry, &tag)?.manifest().await? {
            manifests
                .entry(digest)
                .or_insert_with(|| (Vec::new(), bytes))
                .0
                .push(tag);
        }
    }
    let mut removed = Vec::new();
    for (digest, (tags, bytes)) in manifests {
        if tags
            .iter()
            .any(|tag| !is_dev_tag(tag) || pinned.contains(tag))
        {
            continue;
        }
        let manifest: oci_client::manifest::OciImageManifest = serde_json::from_slice(&bytes)?;
        if manifest.artifact_type.as_deref() != Some(PACKAGE_ARTIFACT_TYPE) {
            continue;
        }
        let Some(created) = manifest
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(CREATED_ANNOTATION))
        else {
            continue;
        };
        let created =
            DateTime::parse_from_rfc3339(created).context("invalid dev publication time")?;
        if created < now - chrono::Duration::days(7) {
            repository.delete_manifest(&digest).await?;
            removed.extend(tags);
        }
    }
    Ok(removed)
}
