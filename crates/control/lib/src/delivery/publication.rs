//! Publish only the immutable release that completed repository qualification,
//! and record the qualification that `env apply` reads.

use std::path::Path;

use anyhow::{Context as _, ensure};
use wamn_catalog::ServingManifest;

use super::Qualification;
use crate::print_release_env::{ReleaseSnapshot, lookup_release_snapshot};
use crate::push_release_manifest::{self, PushReleaseManifestRequest, PushedReleaseManifest};

/// Publish an already qualified candidate through the existing OCI publisher.
pub async fn publish(
    qualification: &Path,
    publication: &PushReleaseManifestRequest,
) -> anyhow::Result<PushedReleaseManifest> {
    let bytes = std::fs::read(qualification).context("read the qualification result")?;
    let qualification = Qualification::read(qualification)?;
    checked_snapshot(&qualification, publication).await?;
    // The existing publisher rereads the sealed snapshot, preserves exact
    // retries, and records its upload with the same source attribution.
    qualification.assert_artifacts()?;
    let pushed = push_release_manifest::push_release_manifest(
        publication,
        Some(&qualification.source_commit),
    )
    .await?;
    crate::publish_release::on_control_plane(&publication.control_database_url, async |control| {
        let control = control.transaction().await?;
        super::selection::record(&control, &qualification, &bytes).await?;
        control
            .commit()
            .await
            .context("commit the qualification record")
    })
    .await?;
    Ok(pushed)
}

pub(super) async fn checked_snapshot(
    qualification: &Qualification,
    args: &PushReleaseManifestRequest,
) -> anyhow::Result<ReleaseSnapshot> {
    qualification.require_pass()?;
    let (expected, expected_digest) = qualification.candidate.manifest()?;
    ensure!(
        qualification.candidate.tenant == args.tenant
            && qualification.candidate.environment == args.environment
            && expected_digest.as_str() == args.manifest_digest,
        "publication scope differs from the qualified release"
    );
    let snapshot = lookup_release_snapshot(
        &args.database_url,
        &args.tenant,
        &args.manifest_digest,
        &args.artifact_base,
    )
    .await?;
    require_same_manifest(&expected, &snapshot.manifest)?;
    Ok(snapshot)
}

fn require_same_manifest(
    expected: &ServingManifest,
    actual: &ServingManifest,
) -> anyhow::Result<()> {
    ensure!(
        expected.canonical_bytes() == actual.canonical_bytes(),
        "the published snapshot differs from the qualified release"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::require_same_manifest;
    use wamn_catalog::ServingManifest;

    mod vector {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs"
        ));
    }

    #[test]
    fn qualification_identity_cannot_be_reused_for_another_published_release() {
        let (expected, digest) =
            ServingManifest::from_canonical_bytes(vector::CANONICAL_BYTES).unwrap();
        assert_eq!(digest.as_str(), vector::DIGEST);
        require_same_manifest(&expected, &expected).unwrap();
        let mut changed = expected.clone();
        changed
            .release
            .packages
            .insert(wamn_catalog::PackageCoordinate::new("another_package", "1.0.0").unwrap());
        assert!(require_same_manifest(&expected, &changed).is_err());
    }
}
