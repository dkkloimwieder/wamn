//! C4 file schema, grants, history, and reference constraints on real PostgreSQL.
//! Object transfer and HTTP exposure require the integrated native file routes.

use sqlx::Row as _;

mod common;
#[path = "support/files.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also supplies native host and MinIO helpers."
)]
mod files;

#[tokio::test]
#[ignore = "requires: docker"]
async fn application_file_writes_are_refused_and_history_uses_the_standard_contract()
-> anyhow::Result<()> {
    let mut fixture = files::fixture("file_authority").await?;
    files::metadata(&mut fixture.writer).await?;
    for statement in [
        "INSERT INTO platform.file DEFAULT VALUES",
        "UPDATE platform.file SET name='forged'",
        "DELETE FROM platform.file",
        "TRUNCATE platform.file",
        "INSERT INTO platform.file_history DEFAULT VALUES",
    ] {
        let error = sqlx::raw_sql(sqlx::AssertSqlSafe((statement).to_string()))
            .execute(&mut fixture.guest)
            .await
            .map(drop)
            .unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("42501"),
            "{statement}"
        );
    }
    let row = sqlx::query("SELECT app,connection_id::text,created_by::text, \
         updated_by::text,created_at=updated_at,exposure,url FROM platform.file WHERE id=$1::text::uuid").bind(files::FILE).fetch_one(&mut fixture
        .guest)
        .await?;
    assert_eq!(row.get::<String, _>(0), files::APPLICATION);
    assert_eq!(row.get::<String, _>(1), files::CONNECTION);
    assert_eq!(row.get::<String, _>(2), files::CALLER);
    assert_eq!(row.get::<String, _>(3), files::CALLER);
    assert!(row.get::<bool, _>(4));
    assert_eq!(row.get::<String, _>(5), "app");
    assert!(row.get::<Option<String>, _>(6).is_none());
    let entry =
        sqlx::query("SELECT changed_by::text,operation,after->>'id' FROM platform.file_history")
            .fetch_one(&mut fixture.observer)
            .await?;
    assert_eq!(entry.get::<String, _>(0), files::CALLER);
    assert_eq!(entry.get::<String, _>(1), "admin:file-fixture");
    assert_eq!(entry.get::<String, _>(2), files::FILE);
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn disabling_a_connection_preserves_file_references_and_referenced_delete_refuses()
-> anyhow::Result<()> {
    let mut fixture = files::fixture("file_references").await?;
    files::metadata(&mut fixture.writer).await?;
    sqlx::query(
        "INSERT INTO file_fixture.document (id,file_id) VALUES ($1::text::uuid,$1::text::uuid)",
    )
    .bind(files::FILE)
    .execute(&mut fixture.guest)
    .await
    .map(|result| result.rows_affected())?;
    let mut writer = files::transaction(
        &mut fixture.writer,
        &files::CALLER
            .parse::<wamn_identity_protocol::PrincipalId>()
            .unwrap(),
        "admin:file-fixture",
    )
    .await?;
    sqlx::query("UPDATE platform.connection SET active=false WHERE id=$1::text::uuid")
        .bind(files::CONNECTION)
        .execute(&mut *writer)
        .await
        .map(|result| result.rows_affected())?;
    writer.commit().await?;
    let row = sqlx::query(
        "SELECT c.active,f.connection_id::text FROM file_fixture.document d \
         JOIN platform.file f ON f.id=d.file_id JOIN platform.connection c ON c.id=f.connection_id",
    )
    .fetch_one(&mut fixture.guest)
    .await?;
    assert!(!row.get::<bool, _>(0));
    assert_eq!(row.get::<String, _>(1), files::CONNECTION);
    let mut writer = files::transaction(
        &mut fixture.writer,
        &files::CALLER
            .parse::<wamn_identity_protocol::PrincipalId>()
            .unwrap(),
        "admin:file-fixture",
    )
    .await?;
    let error = sqlx::query("DELETE FROM platform.file WHERE id=$1::text::uuid")
        .bind(files::FILE)
        .execute(&mut *writer)
        .await
        .map(|result| result.rows_affected())
        .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23503")
    );
    writer.rollback().await?;
    sqlx::query("DELETE FROM file_fixture.document")
        .execute(&mut fixture.guest)
        .await
        .map(|result| result.rows_affected())?;
    assert_eq!(
        sqlx::query("SELECT count(*) FROM platform.file")
            .fetch_one(&mut fixture.guest)
            .await?
            .get::<i64, _>(0),
        1
    );
    let mut writer = files::transaction(
        &mut fixture.writer,
        &files::CALLER
            .parse::<wamn_identity_protocol::PrincipalId>()
            .unwrap(),
        "admin:file-fixture",
    )
    .await?;
    assert_eq!(
        sqlx::query("DELETE FROM platform.file WHERE id=$1::text::uuid")
            .bind(files::FILE)
            .execute(&mut *writer)
            .await
            .map(|result| result.rows_affected())?,
        1
    );
    writer.commit().await?;
    // The migration owner needs no lasting platform privileges to keep the FK.
    let privilege = sqlx::query(
        "SELECT has_table_privilege(current_user, \
             (SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
              WHERE n.nspname='platform' AND c.relname='file'), 'REFERENCES')",
    )
    .fetch_one(&mut fixture.migrator)
    .await?;
    assert!(!privilege.get::<bool, _>(0));
    Ok(())
}

/// MinIO's preserved source time supplies aged objects without changing the clock.
/// Source: https://github.com/minio/minio/blob/76c06ff1d2cc844050fdeb1d54bf8fa70db72278/cmd/object-api-options.go
#[tokio::test]
#[ignore = "requires: docker"]
async fn aged_staging_expires_while_unlinked_metadata_and_aged_live_bytes_stay()
-> anyhow::Result<()> {
    use object_store::{
        ObjectStoreExt as _,
        aws::AmazonS3Builder,
        path::Path,
        signer::{Method, Signer as _},
    };
    let mut metadata = files::fixture("file_lifecycle").await?;
    files::metadata(&mut metadata.writer).await?;
    files::age_metadata(&mut metadata).await?;
    let minio = files::Minio::start().await?;
    let store = AmazonS3Builder::new()
        .with_endpoint(&minio.endpoint)
        .with_bucket_name("files-a")
        .with_region("us-east-1")
        .with_access_key_id("file-fixture")
        .with_secret_access_key(files::STORE_PASSWORD)
        .with_allow_http(true)
        .build()?;
    let live = Path::from(format!(
        "generation-one/dev/_wamn_files/live/{}",
        files::FILE
    ));
    let staging_id = uuid::Uuid::new_v4().to_string();
    let staging = Path::from(format!(
        "generation-one/dev/_wamn_files/staging/{staging_id}"
    ));
    let client = minio.client()?;
    for path in [&live, &staging] {
        let url = store
            .signed_url(Method::PUT, path, std::time::Duration::from_secs(60))
            .await?;
        let response = client
            .put(url)
            .header("x-minio-source-mtime", "2000-01-01T00:00:00Z")
            .body(b"aged fixture bytes".to_vec())
            .send()
            .await?;
        assert!(
            response.status().is_success(),
            "seed aged MinIO object: {}",
            response.text().await?
        );
    }
    let mut transaction = files::transaction(
        &mut metadata.writer,
        &files::CALLER
            .parse::<wamn_identity_protocol::PrincipalId>()
            .unwrap(),
        "admin:file-fixture",
    )
    .await?;
    sqlx::query("UPDATE platform.file SET size=$1,sha256=$2 WHERE id=$3::text::uuid")
        .bind(i64::try_from(b"aged fixture bytes".len())?)
        .bind(wamn_digest::bytes_sha256(b"aged fixture bytes").to_string())
        .bind(files::FILE)
        .execute(&mut *transaction)
        .await
        .map(|result| result.rows_affected())?;
    transaction.commit().await?;
    let row = sqlx::query("SELECT created_at FROM platform.file WHERE id=$1::text::uuid")
        .bind(files::FILE)
        .fetch_one(&mut metadata.guest)
        .await?;
    let created: chrono::DateTime<chrono::Utc> = row.get(0);
    assert!(created < chrono::Utc::now() - chrono::Duration::days(30));
    assert_eq!(
        sqlx::query("SELECT count(*) FROM file_fixture.document")
            .fetch_one(&mut metadata.guest)
            .await?
            .get::<i64, _>(0),
        0
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM platform.file WHERE id=$1::text::uuid")
            .bind(&staging_id)
            .fetch_one(&mut metadata.guest)
            .await?
            .get::<i64, _>(0),
        0
    );
    let cutoff = chrono::Utc::now() - chrono::Duration::days(2);
    assert!(store.head(&staging).await?.last_modified < cutoff);
    assert!(store.head(&live).await?.last_modified < cutoff);
    minio
        .mc(
            &[
                "ilm",
                "rule",
                "add",
                "--expire-days",
                "1",
                "--prefix",
                "generation-one/dev/_wamn_files/staging/",
                "file/files-a",
            ],
            b"",
        )
        .await?;
    let mut removed = false;
    for _ in 0..180 {
        match store.head(&staging).await {
            Err(object_store::Error::NotFound { .. }) => {
                removed = true;
                break;
            }
            Ok(_) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            Err(error) => return Err(error.into()),
        }
    }
    assert!(
        removed,
        "the lifecycle scanner must delete aged staging only"
    );
    assert_eq!(
        store.get(&live).await?.bytes().await?.as_ref(),
        b"aged fixture bytes"
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM platform.file WHERE id=$1::text::uuid")
            .bind(files::FILE)
            .fetch_one(&mut metadata.guest)
            .await?
            .get::<i64, _>(0),
        1
    );
    Ok(())
}
