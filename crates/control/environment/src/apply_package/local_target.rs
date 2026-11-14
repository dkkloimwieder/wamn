//! Working-tree SQL application on a recorded local target.

use anyhow::Context as _;
use sqlx::Row as _;
use wamn_catalog::package_schema::PackageDirectory;
use wamn_runtime::local_application::LocalTargetComment;

/// The digest of the local manifest bytes, independent of installed SQL state.
pub fn manifest_digest(directory: &PackageDirectory) -> String {
    wamn_digest::bytes_sha256(&directory.manifest_bytes)
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
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
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
    let statement: String = sqlx::query(COMMENT_ON_TARGET_SQL)
        .bind(comment.to_string())
        .fetch_one(&mut **tx)
        .await
        .context("render the local target comment")?
        .get(0);
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement.clone()))
        .execute(&mut **tx)
        .await
        .map(drop)
        .context("record the current manifest digest in the local target comment")?;
    Ok(true)
}
