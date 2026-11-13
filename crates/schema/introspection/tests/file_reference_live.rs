//! File references cross the platform boundary through the approved UUID key.

use sqlx::Row as _;
use wamn_schema_introspection::postgres::{PostgresIntrospectionErrorType, read_catalog};

#[tokio::test]
async fn file_reference_is_introspected_and_other_platform_keys_are_refused() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();
    let reader = wamn_postgres::pool(database.url(), 1).await.unwrap();

    // Bootstrap creates the target and the migration owner. The application
    // migration runs with REFERENCES(id), with no platform write authority.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE SCHEMA platform; \
         CREATE TABLE platform.file (id uuid PRIMARY KEY, blob_key text UNIQUE); \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='file_reference_owner') \
           THEN CREATE ROLE file_reference_owner NOLOGIN NOSUPERUSER NOBYPASSRLS; \
         END IF; END $$; \
         GRANT CREATE ON DATABASE \"{}\" TO file_reference_owner;",
            database.name()
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("GRANT USAGE ON SCHEMA platform TO file_reference_owner; \
         GRANT REFERENCES (id) ON platform.file TO file_reference_owner; \
         SET ROLE file_reference_owner")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    let attributes = sqlx::query(
        "SELECT rolsuper, rolbypassrls, \
         has_table_privilege(current_user, 'platform.file', 'INSERT,UPDATE,DELETE') \
         FROM pg_roles WHERE rolname=current_user",
    )
    .fetch_one(&mut client)
    .await
    .unwrap();
    for column in 0..3 {
        assert!(!attributes.get::<bool, _>(column));
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (include_str!(
            "../../../control/environment/tests/support/file_package/migrations/0001_documents.sql"
        ))
        .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("RESET ROLE; REVOKE REFERENCES (id) ON platform.file FROM file_reference_owner; \
         REVOKE USAGE ON SCHEMA platform FROM file_reference_owner; SET ROLE file_reference_owner")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    let missing = sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("INSERT INTO file_fixture.document VALUES \
         ('00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002')")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap_err();
    assert_eq!(
        missing
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23503")
    );
    let catalog = read_catalog(&reader, &["file_fixture"]).await.unwrap();
    assert_eq!(catalog.tables().len(), 1);
    assert_eq!(catalog.tables()[0].name(), "document");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("RESET ROLE; GRANT USAGE ON SCHEMA platform TO file_reference_owner; \
         GRANT REFERENCES (blob_key) ON platform.file TO file_reference_owner; \
         SET ROLE file_reference_owner; ALTER TABLE file_fixture.document ADD COLUMN \
         raw_key text CONSTRAINT document_raw_key_fkey REFERENCES platform.file(blob_key)")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    let error = read_catalog(&reader, &["file_fixture"]).await.unwrap_err();
    assert_eq!(
        error.error_type(),
        PostgresIntrospectionErrorType::UnsupportedConstraint
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("ALTER TABLE file_fixture.document DROP COLUMN raw_key; RESET ROLE; \
         CREATE TABLE platform.other(id uuid PRIMARY KEY); \
         GRANT REFERENCES (id) ON platform.other TO file_reference_owner; \
         SET ROLE file_reference_owner; ALTER TABLE file_fixture.document ADD COLUMN \
         other_id uuid CONSTRAINT document_other_id_fkey REFERENCES platform.other(id)")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    let error = read_catalog(&reader, &["file_fixture"]).await.unwrap_err();
    assert_eq!(
        error.error_type(),
        PostgresIntrospectionErrorType::UnsupportedConstraint
    );
}
