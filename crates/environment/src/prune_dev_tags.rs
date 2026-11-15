//! Expire unpinned development package tags after seven days.

use anyhow::Context as _;
use chrono::Utc;
use wamn_package_artifact::Repository;

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
    let repository = Repository::open(
        &releases,
        &"latest"
            .parse::<wamn_package_artifact::tag::OciTag>()
            .unwrap(),
    )?;
    let mut manifests = Vec::new();
    for tag in repository.tags().await? {
        let digest = format!("sha256:{tag}");
        if let Ok(digest) = digest.parse::<wamn_digest::Digest>() {
            manifests
                .push(crate::environment::analyse::pull_release_manifest(platform, &digest).await?);
        }
    }
    for tag in wamn_publish::development::prune(&packages, &manifests, Utc::now()).await? {
        tracing::info!(%tag, "removed expired development package tag");
    }
    Ok(())
}
