//! Working-tree SQL application on a recorded local target.

use anyhow::Context as _;
use tokio_postgres::Transaction;
use wamn_runtime::local_application::LocalTargetComment;
use wamn_schema_control::PackageDirectory;

/// The digest of the local manifest bytes, independent of installed SQL state.
pub fn manifest_digest(directory: &PackageDirectory) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(
            &ring::digest::SHA256,
            &directory.manifest_bytes
        ))
    )
}

const COMMENT_ON_TARGET_SQL: &str = "SELECT pg_catalog.format(\
     'COMMENT ON DATABASE %I IS %L', pg_catalog.current_database(), $1::text)";

/// Record the current manifest digest of one package coordinate in the local target comment.
///
/// The caller owns the comment, so one transaction can record several
/// coordinates and keep every digest it already wrote. The local target applies
/// its working tree and records no artifact digest, so the comment names the
/// `wamn.json` the running application came from ([`manifest_digest`]).
pub async fn record_manifest(
    tx: &Transaction<'_>,
    comment: &mut LocalTargetComment,
    coordinate: &str,
    manifest_digest: &str,
) -> anyhow::Result<bool> {
    if comment.manifests.get(coordinate).map(String::as_str) == Some(manifest_digest) {
        return Ok(false);
    }
    comment
        .manifests
        .insert(coordinate.to_owned(), manifest_digest.to_owned());
    let statement: String = tx
        .query_one(COMMENT_ON_TARGET_SQL, &[&comment.to_string()])
        .await
        .context("render the local target comment")?
        .get(0);
    tx.batch_execute(&statement)
        .await
        .context("record the current manifest digest in the local target comment")?;
    Ok(true)
}
