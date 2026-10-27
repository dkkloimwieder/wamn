//! File references cross the platform boundary through the approved UUID key.

use tokio_postgres::error::SqlState;
use wamn_schema_introspection::postgres::{PostgresIntrospectionErrorType, read_catalog};

#[tokio::test]
async fn file_reference_is_introspected_and_other_platform_keys_are_refused() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let (client, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let driver = tokio::spawn(connection);
    // Bootstrap creates the target and the migration owner. The application
    // migration runs with REFERENCES(id), with no platform write authority.
    client
        .batch_execute(&format!(
            "CREATE SCHEMA platform; \
         CREATE TABLE platform.file (id uuid PRIMARY KEY, blob_key text UNIQUE); \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='file_reference_owner') \
           THEN CREATE ROLE file_reference_owner NOLOGIN NOSUPERUSER NOBYPASSRLS; \
         END IF; END $$; \
         GRANT CREATE ON DATABASE \"{}\" TO file_reference_owner;",
            database.name()
        ))
        .await
        .unwrap();
    client
        .batch_execute(
            "GRANT USAGE ON SCHEMA platform TO file_reference_owner; \
         GRANT REFERENCES (id) ON platform.file TO file_reference_owner; \
         SET ROLE file_reference_owner",
        )
        .await
        .unwrap();
    let attributes = client
        .query_one(
            "SELECT rolsuper, rolbypassrls, \
         has_table_privilege(current_user, 'platform.file', 'INSERT,UPDATE,DELETE') \
         FROM pg_roles WHERE rolname=current_user",
            &[],
        )
        .await
        .unwrap();
    for column in 0..3 {
        assert!(!attributes.get::<_, bool>(column));
    }
    client
        .batch_execute(include_str!(
            "../../../control/lib/tests/support/file_package/migrations/0001_documents.sql"
        ))
        .await
        .unwrap();
    client
        .batch_execute(
            "RESET ROLE; REVOKE REFERENCES (id) ON platform.file FROM file_reference_owner; \
         REVOKE USAGE ON SCHEMA platform FROM file_reference_owner; SET ROLE file_reference_owner",
        )
        .await
        .unwrap();
    let missing = client
        .batch_execute(
            "INSERT INTO file_fixture.document VALUES \
         ('00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002')",
        )
        .await
        .unwrap_err();
    assert_eq!(missing.code(), Some(&SqlState::FOREIGN_KEY_VIOLATION));
    let catalog = read_catalog(&client, &["file_fixture"]).await.unwrap();
    assert_eq!(catalog.tables().len(), 1);
    assert_eq!(catalog.tables()[0].name(), "document");
    let schema = String::from_utf8(catalog.canonical_json_bytes()).unwrap();
    assert!(schema.contains("document_file_id_fkey"));
    assert!(schema.contains("platform"));
    client
        .batch_execute(
            "RESET ROLE; GRANT USAGE ON SCHEMA platform TO file_reference_owner; \
         GRANT REFERENCES (blob_key) ON platform.file TO file_reference_owner; \
         SET ROLE file_reference_owner; ALTER TABLE file_fixture.document ADD COLUMN \
         raw_key text CONSTRAINT document_raw_key_fkey REFERENCES platform.file(blob_key)",
        )
        .await
        .unwrap();
    let error = read_catalog(&client, &["file_fixture"]).await.unwrap_err();
    assert_eq!(
        error.error_type(),
        PostgresIntrospectionErrorType::UnsupportedConstraint
    );
    client
        .batch_execute(
            "ALTER TABLE file_fixture.document DROP COLUMN raw_key; RESET ROLE; \
         CREATE TABLE platform.other(id uuid PRIMARY KEY); \
         GRANT REFERENCES (id) ON platform.other TO file_reference_owner; \
         SET ROLE file_reference_owner; ALTER TABLE file_fixture.document ADD COLUMN \
         other_id uuid CONSTRAINT document_other_id_fkey REFERENCES platform.other(id)",
        )
        .await
        .unwrap();
    let error = read_catalog(&client, &["file_fixture"]).await.unwrap_err();
    assert_eq!(
        error.error_type(),
        PostgresIntrospectionErrorType::UnsupportedConstraint
    );
    driver.abort();
}
