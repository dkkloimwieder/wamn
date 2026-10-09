//! Publish only the immutable release that completed repository qualification.

use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::NoTls;
use wamn_catalog::ServingManifest;

use super::Qualification;
use crate::print_release_env::{ReleaseSnapshot, lookup_release_snapshot};
use crate::push_release_manifest::{self, PushReleaseManifestRequest, PushedReleaseManifest};

/// Publish an already qualified candidate through the existing OCI publisher.
pub async fn publish(
    qualification: &Path,
    publication: &PushReleaseManifestRequest,
) -> anyhow::Result<PushedReleaseManifest> {
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

/// Whether a publication record matches the release that a select or a
/// deployment names. The manifest digest always counts. A deployment also
/// requires the source commit of its qualification. A select passes `None`,
/// because its gate is the qualification's package set and image digests, and
/// the attestation's commit stays a record (owner ruling of 2026-10-03 on
/// `wamn-1wou`).
fn publication_matches(
    record_manifest_digest: &str,
    record_source_commit: Option<&str>,
    manifest_digest: &str,
    source_commit: Option<&str>,
) -> bool {
    record_manifest_digest == manifest_digest
        && source_commit.is_none_or(|source_commit| record_source_commit == Some(source_commit))
}

/// Require the existing upload fact before selecting or deploying its artifacts.
pub(super) async fn require_published(
    source_commit: Option<&str>,
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
        let record = transaction
            .query_opt(
                wamn_schema_control::attestation::read_attestation_sql(),
                &[
                    &args.tenant,
                    &instance,
                    &snapshot.carrier.manifest_digest.as_str(),
                    &args.org,
                    &args.project,
                    &args.environment,
                ],
            )
            .await?
            .context("the selected release has no publication record")?;
        ensure!(
            publication_matches(
                &record.get::<_, String>("deployed_manifest_hash"),
                record.get::<_, Option<String>>("source_commit").as_deref(),
                snapshot.carrier.manifest_digest.as_str(),
                source_commit,
            ),
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
    use super::{publication_matches, require_same_manifest};
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

    #[test]
    fn a_select_passes_when_the_attestation_names_another_commit() {
        let digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        let attested = "a".repeat(40);
        let qualified = "b".repeat(40);
        // select-release: the attestation's commit is a record, not a gate.
        assert!(publication_matches(digest, Some(&attested), digest, None));
        assert!(publication_matches(digest, None, digest, None));
        // deploy-release keeps its source check.
        assert!(!publication_matches(
            digest,
            Some(&attested),
            digest,
            Some(&qualified)
        ));
        assert!(publication_matches(
            digest,
            Some(&attested),
            digest,
            Some(&attested)
        ));
        // The manifest digest counts for both.
        let other = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        assert!(!publication_matches(other, Some(&attested), digest, None));
    }
}
