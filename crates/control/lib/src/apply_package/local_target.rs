//! The local-target exception to manifest drift and to the release seal.

use anyhow::Context as _;
use tokio_postgres::Transaction;
use wamn_runtime::local_application::{LocalTargetComment, read_local_target_comment};

/// The release seal trigger compares this setting with the full database comment.
const LIFT_RELEASE_SEAL_SQL: &str =
    "SELECT pg_catalog.set_config('wamn.local_target_comment', $1, true)";
const COMMENT_ON_TARGET_SQL: &str = "SELECT pg_catalog.format(\
     'COMMENT ON DATABASE %I IS %L', pg_catalog.current_database(), $1::text)";

/// Check the marker inside the apply transaction and lift the seal for that transaction.
pub(super) async fn lift_release_seal(
    tx: &Transaction<'_>,
    tenant: &str,
    environment: &str,
) -> anyhow::Result<LocalTargetComment> {
    let comment = read_local_target_comment(tx, tenant, environment).await?;
    tx.query_one(LIFT_RELEASE_SEAL_SQL, &[&comment.to_string()])
        .await
        .context("lift the release seal for this local target transaction")?;
    Ok(comment)
}

/// Record the current manifest hash of one package coordinate in the local target comment.
pub(super) async fn record_manifest(
    tx: &Transaction<'_>,
    mut comment: LocalTargetComment,
    coordinate: &str,
    manifest_sha256: &str,
) -> anyhow::Result<bool> {
    if comment.manifests.get(coordinate).map(String::as_str) == Some(manifest_sha256) {
        return Ok(false);
    }
    comment
        .manifests
        .insert(coordinate.to_owned(), manifest_sha256.to_owned());
    let statement: String = tx
        .query_one(COMMENT_ON_TARGET_SQL, &[&comment.to_string()])
        .await
        .context("render the local target comment")?
        .get(0);
    tx.batch_execute(&statement)
        .await
        .context("record the current manifest hash in the local target comment")?;
    Ok(true)
}
