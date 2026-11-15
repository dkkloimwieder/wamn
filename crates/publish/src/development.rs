//! Content-derived development coordinates and expiry of unpinned development publications.

use std::collections::BTreeSet;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use wamn_catalog::ServingManifest;
use wamn_package_artifact::{
    CREATED_ANNOTATION, PACKAGE_ARTIFACT_TYPE, PackageRegistry, Repository, is_dev_tag,
};

/// Select the immutable development version for an authored version and content digest.
pub fn version(
    authored: &wamn_catalog::package_version::PackageVersion,
    digest: &wamn_digest::Digest,
) -> anyhow::Result<wamn_catalog::package_version::PackageVersion> {
    let base = authored
        .as_str()
        .split(['-', '+'])
        .next()
        .context("authored package version")?;
    let version = format!("{base}-dev.h{}", &digest.hex()[..12]);
    let tag = wamn_package_artifact::tag::OciTag::try_from(version)?;
    Ok(String::from(tag).try_into()?)
}

/// Delete expired dev tags except coordinates pinned by any published release.
/// Missing publication times leave old data untouched. Registry errors stop pruning.
pub async fn prune(
    registry: &PackageRegistry,
    releases: &[ServingManifest],
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<wamn_package_artifact::tag::OciTag>> {
    let pinned: BTreeSet<_> = releases
        .iter()
        .flat_map(|release| &release.release.packages)
        .map(|package| {
            format!("{}-{}", package.package_id(), package.package_version())
                .parse::<wamn_package_artifact::tag::OciTag>()
        })
        .collect::<Result<BTreeSet<_>, _>>()?
        .into_iter()
        .collect();
    let repository = Repository::open(
        registry,
        &"latest"
            .parse::<wamn_package_artifact::tag::OciTag>()
            .unwrap(),
    )?;
    let mut manifests =
        std::collections::BTreeMap::<_, (Vec<wamn_package_artifact::tag::OciTag>, Vec<u8>)>::new();
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
        if created <= now - chrono::Duration::days(7) {
            repository.delete_manifest(&digest).await?;
            removed.extend(tags);
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    #[test]
    fn development_versions_require_a_content_digest() {
        let digest = wamn_digest::bytes_sha256(b"source");
        let version = super::version(&"1.0.0".parse().unwrap(), &digest).unwrap();
        assert!(wamn_package_artifact::is_dev_tag(
            &version.to_string().parse().unwrap()
        ));
    }
}
