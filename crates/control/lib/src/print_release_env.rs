//! Read one published release snapshot and derive the carrier its workloads take.
//!
//! The operator path that shipped with `wamn-cdky` is manual: read the digest
//! off `publish-release` stdout and hand-edit the template. This reader removes
//! the transcription step and nothing else. It reads the
//! `catalog.releases` row and re-derives release identity
//! from those exact bytes. It writes no manifest and mutates
//! nothing; a pod reading its own digest out of PostgreSQL is explicitly
//! refused (`wamn-duyl`) because it would roll a release without a rollout.

use anyhow::Context as _;
use tokio_postgres::NoTls;
use wamn_catalog::{ManifestDigest, ServingManifest};

use crate::push_release_manifest::select_snapshot;

/// Exact release identity carried into one workload deployment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseCarrier {
    /// Registry repository holding the immutable release artifact.
    pub artifact_base: String,
    /// Digest re-derived from the release snapshot's canonical bytes.
    pub manifest_digest: ManifestDigest,
}

/// Exact immutable release snapshot and its serving carrier.
#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseSnapshot {
    /// Canonical release document read from the snapshot row.
    pub manifest: ServingManifest,
    /// Carrier derived from those exact canonical bytes.
    pub carrier: ReleaseCarrier,
}

/// Read and derive the exact carrier for one published release.
pub async fn lookup_release_carrier(
    database_url: &str,
    tenant: &str,
    manifest_digest: &str,
    artifact_base: &str,
) -> anyhow::Result<ReleaseCarrier> {
    Ok(
        lookup_release_snapshot(database_url, tenant, manifest_digest, artifact_base)
            .await?
            .carrier,
    )
}

/// Read one canonical release snapshot and derive its carrier from the same bytes.
pub async fn lookup_release_snapshot(
    database_url: &str,
    tenant: &str,
    manifest_digest: &str,
    artifact_base: &str,
) -> anyhow::Result<ReleaseSnapshot> {
    let (mut client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to the release snapshot database")?;
    let connection_task = tokio::spawn(connection);
    let read = select_snapshot(&mut client, tenant, manifest_digest).await;
    let canonical_bytes = match read {
        Ok(canonical_bytes) => {
            drop(client);
            connection_task
                .await
                .context("join the release snapshot connection")?
                .context("drive the release snapshot connection")?;
            canonical_bytes
        }
        Err(error) => {
            connection_task.abort();
            return Err(error);
        }
    };
    // The digest is re-derived from the bytes rather than read from a second
    // column, exactly as the publisher does: one carrier of release identity.
    let (manifest, manifest_digest) = ServingManifest::from_canonical_bytes(&canonical_bytes)
        .context("the frozen release snapshot is not a canonical format-1 manifest")?;

    Ok(ReleaseSnapshot {
        manifest,
        carrier: ReleaseCarrier {
            artifact_base: artifact_base.to_owned(),
            manifest_digest,
        },
    })
}
