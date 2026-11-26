//! File row queries and their administration transactions.

use sqlx::Row as _;
use uuid::Uuid;
use wamn_blobstore::file_route::{
    Exposure, FileError, FileExposure, FileInsert, FileRecord, FileRefusal, failed, refused,
    valid_media_type,
};
use wamn_identity_protocol::{AuthorityClass, PrincipalId};
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::{ApplicationName, ProjectId};

use wamn_postgres::WamnPostgres;

use crate::operation_sql::administration_transaction;

const FILE_COLUMNS: &str =
    "id::text, connection_id::text, media_type, size, exposure, url, blob_key";

/// Hold a file's session lock on the configured platform login until drop.
pub(crate) async fn lock(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    id: uuid::Uuid,
) -> Result<Box<dyn Send>, FileError> {
    let (connection, policy) = postgres
        .checkout_platform(project, AuthorityClass::Platform)
        .await
        .map_err(|error| failed("administration", anyhow::anyhow!(error.to_string())))?;
    // A session lock must never return to a pool still holding its lock.
    let mut connection = connection.detach();
    sqlx::query("SELECT set_config('lock_timeout', $1, false)")
        .bind(format!("{}ms", policy.statement_timeout_ms))
        .execute(&mut connection)
        .await
        .map_err(|error| failed("administration", error))?;
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(format!("wamn:file:{id}"))
        .execute(&mut connection)
        .await
        .map_err(|error| failed("administration", error))?;
    Ok(Box::new(connection))
}

pub(crate) async fn record(
    postgres: &WamnPostgres,
    project: &ProjectId,
    principal: &PrincipalId,
    operation: &CanonicalName,
    application: &wamn_registry::ApplicationName,
    id: Uuid,
) -> Result<FileRecord, FileError> {
    let sql =
        format!("SELECT {FILE_COLUMNS} FROM platform.file WHERE id = $1::text::uuid AND app = $2");
    administration_transaction(postgres, project, principal, operation, async |client| {
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id.to_string())
            .bind(application.as_str())
            .fetch_optional(client)
            .await
            .map_err(file_sql_error)?
            .ok_or_else(|| refused(FileRefusal::NotFound, "file_id"))?;
        decode_record(&row)
    })
    .await
    .map_err(|error| failed("catalog", error))?
}

pub(crate) async fn insert(
    postgres: &WamnPostgres,
    project: &ProjectId,
    principal: &PrincipalId,
    operation: &CanonicalName,
    file: FileInsert<'_>,
) -> Result<(), FileError> {
    administration_transaction(postgres, project, principal, operation, async |client| {
        sqlx::query(
            "INSERT INTO platform.file (id, app, connection_id, name, media_type, size, sha256, blob_key, exposure) VALUES ($1::text::uuid,$2,$3::text::uuid,$4,$5,$6,$7,$8,'app')",
        )
        .bind(file.id.to_string())
        .bind(file.application.as_str())
        .bind(file.connection_id)
        .bind(file.name)
        .bind(file.media_type)
        .bind(file.size)
        .bind(file.sha256.to_string())
        .bind(file.blob_key)
        .execute(client)
        .await
        .map_err(file_sql_error)?;
        Ok(())
    })
    .await
    .map_err(|error| failed("catalog", error))?
}

pub(crate) async fn delete(
    postgres: &WamnPostgres,
    project: &ProjectId,
    principal: &PrincipalId,
    operation: &CanonicalName,
    application: &ApplicationName,
    id: Uuid,
) -> Result<(), FileError> {
    administration_transaction(postgres, project, principal, operation, async |client| {
        let deleted =
            sqlx::query("DELETE FROM platform.file WHERE id = $1::text::uuid AND app = $2")
                .bind(id.to_string())
                .bind(application.as_str())
                .execute(client)
                .await
                .map_err(file_sql_error)?;
        if deleted.rows_affected() != 1 {
            return Err(refused(FileRefusal::NotFound, "file_id"));
        }
        Ok(())
    })
    .await
    .map_err(|error| failed("catalog", error))?
}

pub(crate) async fn set_exposure(
    postgres: &WamnPostgres,
    project: &ProjectId,
    principal: &PrincipalId,
    operation: &CanonicalName,
    file: FileExposure<'_>,
) -> Result<Result<(), FileError>, FileError> {
    administration_transaction(postgres, project, principal, operation, async |client| {
        sqlx::query("UPDATE platform.file SET exposure = $3, url = $4 WHERE id = $1::text::uuid AND app = $2 AND (exposure IS DISTINCT FROM $3 OR url IS DISTINCT FROM $4)")
            .bind(file.id.to_string())
            .bind(file.application.as_str())
            .bind(match file.exposure {
                Exposure::Application => "app",
                other => other.literal(),
            })
            .bind(file.url)
            .execute(client)
            .await
            .map_err(file_sql_error)?;
        Ok(())
    })
    .await
    .map_err(|error| failed("catalog", error))
}

fn file_sql_error(error: sqlx::Error) -> FileError {
    if error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .as_deref()
        == Some("23503")
    {
        refused(FileRefusal::Referenced, "file_id").with_source(error)
    } else {
        failed("catalog", error)
    }
}

fn decode_record(row: &sqlx::postgres::PgRow) -> Result<FileRecord, FileError> {
    let id: String = row.try_get(0).map_err(file_sql_error)?;
    let exposure: String = row.try_get(4).map_err(file_sql_error)?;
    let file = FileRecord {
        id: id.parse().map_err(|error| failed("file_id", error))?,
        connection_id: row.try_get(1).map_err(file_sql_error)?,
        media_type: row.try_get(2).map_err(file_sql_error)?,
        size: row.try_get(3).map_err(file_sql_error)?,
        exposure: match exposure.as_str() {
            "private" => Exposure::Private,
            "app" => Exposure::Application,
            "public" => Exposure::Public,
            _ => return Err(failed("exposure", anyhow::anyhow!("invalid file exposure"))),
        },
        url: row.try_get(5).map_err(file_sql_error)?,
        blob_key: row.try_get(6).map_err(file_sql_error)?,
    };
    if file.size < 0
        || !valid_media_type(&file.media_type)
        || (file.exposure == Exposure::Public) != file.url.is_some()
    {
        return Err(failed(
            "catalog",
            anyhow::anyhow!("file catalog contains invalid security metadata"),
        ));
    }
    Ok(file)
}
