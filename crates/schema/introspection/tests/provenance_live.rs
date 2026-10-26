//! Imported records keep the source namespace and its record identity.
use wamn_schema_introspection::postgres::read_catalog;

#[tokio::test]
async fn provenance_foreign_key_and_namespace_uniqueness_are_live_constraints() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let (client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let driver = tokio::spawn(driver);
    client.batch_execute("CREATE SCHEMA platform; CREATE TABLE platform.external_system(id uuid PRIMARY KEY); DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='provenance_owner') THEN CREATE ROLE provenance_owner NOLOGIN; END IF; END $$; CREATE SCHEMA rest_consumer_fixture AUTHORIZATION provenance_owner; GRANT USAGE ON SCHEMA platform TO provenance_owner; GRANT REFERENCES ON platform.external_system TO provenance_owner; INSERT INTO platform.external_system VALUES ('00000000-0000-4000-8000-000000000001'),('00000000-0000-4000-8000-000000000002'); SET ROLE provenance_owner;").await.unwrap();
    client
        .batch_execute(include_str!(
            "../../../../apps/rest_consumer_fixture/migrations/0001_initial.sql"
        ))
        .await
        .unwrap();
    client.batch_execute("RESET ROLE; REVOKE REFERENCES ON platform.external_system FROM provenance_owner; REVOKE USAGE ON SCHEMA platform FROM provenance_owner; SET ROLE provenance_owner;").await.unwrap();
    let insert = "INSERT INTO rest_consumer_fixture.item(key,name,external_system_id,external_id) VALUES ('42','item',$1::text::uuid,'42')";
    for source in [
        "00000000-0000-4000-8000-000000000001",
        "00000000-0000-4000-8000-000000000002",
    ] {
        client.execute(insert, &[&source]).await.unwrap();
    }
    let duplicate = client
        .execute(insert, &[&"00000000-0000-4000-8000-000000000001"])
        .await
        .unwrap_err();
    assert_eq!(
        duplicate.code(),
        Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    );
    let unknown = client
        .execute(insert, &[&"00000000-0000-4000-8000-000000000003"])
        .await
        .unwrap_err();
    assert_eq!(
        unknown.code(),
        Some(&tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION)
    );
    assert_eq!(client.query_one("SELECT count(DISTINCT external_system_id),count(DISTINCT external_id) FROM rest_consumer_fixture.item",&[]).await.unwrap().get::<_,i64>(0),2);
    client.batch_execute("RESET ROLE").await.unwrap();
    let schemas = ["rest_consumer_fixture"];
    read_catalog(&client, &schemas).await.unwrap();
    client.batch_execute("CREATE TABLE platform.other(id uuid PRIMARY KEY); ALTER TABLE rest_consumer_fixture.item ADD COLUMN other uuid CONSTRAINT item_other_fkey REFERENCES platform.other(id)").await.unwrap();
    assert!(read_catalog(&client, &schemas).await.is_err());
    driver.abort();
}
