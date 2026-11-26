use sqlx::{PgConnection, Row as _};
use wamn_digest::Digest;
use wamn_package::PackageName;

struct InstalledPackageRow<'a> {
    package_id: String,
    artifact_digest: Option<&'a str>,
}

impl InstalledPackageRow<'_> {
    fn parse(self) -> Result<(PackageName, Option<Digest>), sqlx::Error> {
        let package =
            PackageName::try_from(self.package_id).map_err(|source| sqlx::Error::ColumnDecode {
                index: "0".into(),
                source: Box::new(source),
            })?;
        let digest = self
            .artifact_digest
            .map(str::parse)
            .transpose()
            .map_err(|source| sqlx::Error::ColumnDecode {
                index: "1".into(),
                source: anyhow::Error::new(source)
                    .context(format!(
                        "decode artifact digest for installed package {package}"
                    ))
                    .into(),
            })?;
        Ok((package, digest))
    }
}

/// Read the installed packages and their optional pinned artifacts.
///
/// # Errors
/// Returns a database error or a decode error for a malformed stored value.
pub async fn installed_package_artifacts(
    connection: &mut PgConnection,
) -> Result<Vec<(PackageName, Option<Digest>)>, sqlx::Error> {
    sqlx::query("SELECT package_id, artifact_digest FROM catalog.package")
        .try_map(|row: sqlx::postgres::PgRow| {
            InstalledPackageRow {
                package_id: row.try_get(0)?,
                artifact_digest: row.try_get(1)?,
            }
            .parse()
        })
        .fetch_all(connection)
        .await
}
