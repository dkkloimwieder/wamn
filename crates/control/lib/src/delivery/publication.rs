//! Publish only the immutable release that completed repository qualification.

use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::NoTls;
use wamn_catalog::ServingManifest;

use super::Qualification;
use crate::print_release_env::{ReleaseSnapshot, lookup_release_snapshot};
use crate::push_release_manifest::{self, PublishedReleaseManifest, PushReleaseManifestRequest};

/// Publish an already qualified candidate through the existing OCI publisher.
pub async fn publish(
    qualification: &Path,
    publication: &PushReleaseManifestRequest,
) -> anyhow::Result<PublishedReleaseManifest> {
    let qualification = Qualification::read(qualification)?;
    checked_snapshot(&qualification, publication).await?;
    // The existing publisher rereads the sealed snapshot, preserves exact
    // retries, and records its upload with the same source attribution.
    qualification.assert_artifacts()?;
    push_release_manifest::push_release_manifest(publication, Some(&qualification.source_commit))
        .await
}

pub(super) async fn checked_snapshot(
    qualification: &Qualification,
    args: &PushReleaseManifestRequest,
) -> anyhow::Result<ReleaseSnapshot> {
    qualification.require_pass()?;
    let (expected, _) = qualification.candidate.manifest()?;
    ensure!(
        expected.release.tenant_id == args.tenant
            && expected.release.effective_release_id.get() == args.effective_release_id,
        "publication scope differs from the qualified release"
    );
    let snapshot = lookup_release_snapshot(
        &args.database_url,
        &args.tenant,
        args.effective_release_id,
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
        "the minted snapshot differs from the qualified release"
    );
    Ok(())
}

/// Require the existing upload fact before selecting or deploying its artifacts.
pub(super) async fn require_published(
    qualification: &Qualification,
    snapshot: &ReleaseSnapshot,
    args: &PushReleaseManifestRequest,
) -> anyhow::Result<()> {
    let (mut client, connection) = tokio_postgres::connect(&args.control_database_url, NoTls)
        .await
        .context("connect to the release publication control store")?;
    let connection = tokio::spawn(connection);
    let result = async {
        let transaction = client.transaction().await?;
        transaction
            .query_one("SELECT set_config('app.tenant', $1, true)", &[&args.tenant])
            .await?;
        let instance = transaction
            .query_opt(
                wamn_schema_control::attestation::read_environment_instance_sql(),
                &[&args.tenant],
            )
            .await?
            .map(|row| row.get::<_, String>(0))
            .unwrap_or_default();
        let release_id = i32::try_from(args.effective_release_id)?;
        let record = transaction
            .query_opt(
                wamn_schema_control::attestation::read_attestation_sql(),
                &[
                    &args.tenant,
                    &instance,
                    &release_id,
                    &args.org,
                    &args.project,
                    &snapshot.manifest.release.environment,
                ],
            )
            .await?
            .context("the selected release has no publication record")?;
        ensure!(
            record.get::<_, String>("deployed_manifest_hash")
                == snapshot.carrier.manifest_digest.as_str()
                && record.get::<_, Option<String>>("source_commit").as_deref()
                    == Some(qualification.source_commit.as_str()),
            "the publication record differs from the qualified release or source"
        );
        transaction.commit().await?;
        Ok(())
    }
    .await;
    connection.abort();
    result
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
    fn qualification_identity_cannot_be_reused_for_another_minted_release() {
        let (expected, digest) =
            ServingManifest::from_canonical_bytes(vector::CANONICAL_BYTES).unwrap();
        assert_eq!(digest.as_str(), vector::DIGEST);
        require_same_manifest(&expected, &expected).unwrap();
        let mut changed = expected.clone();
        changed.release.environment = "another-environment".to_owned();
        assert!(require_same_manifest(&expected, &changed).is_err());
        changed = expected.clone();
        changed.release.effective_release_id = wamn_catalog::EffectiveReleaseId::new(4).unwrap();
        assert!(require_same_manifest(&expected, &changed).is_err());
    }
}
