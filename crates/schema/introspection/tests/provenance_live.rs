//! Imported records keep the source namespace and its record identity.
use wamn_schema_introspection::postgres::read_catalog;

use sqlx::Row as _;
#[tokio::test]
async fn provenance_foreign_key_and_namespace_uniqueness_are_live_constraints() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    let reader = wamn_postgres::pool(database.url(), 1).await.unwrap();
    sqlx::raw_sql("CREATE SCHEMA platform; CREATE TABLE platform.external_system(id uuid PRIMARY KEY); DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='provenance_owner') THEN CREATE ROLE provenance_owner NOLOGIN; END IF; END $$; CREATE SCHEMA rest_consumer_fixture AUTHORIZATION provenance_owner; GRANT USAGE ON SCHEMA platform TO provenance_owner; GRANT REFERENCES ON platform.external_system TO provenance_owner; INSERT INTO platform.external_system VALUES ('00000000-0000-4000-8000-000000000001'),('00000000-0000-4000-8000-000000000002'); SET ROLE provenance_owner;").execute(&mut client).await.map(drop).unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (include_str!("../../../../apps/rest_consumer_fixture/migrations/0001_initial.sql"))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql("RESET ROLE; REVOKE REFERENCES ON platform.external_system FROM provenance_owner; REVOKE USAGE ON SCHEMA platform FROM provenance_owner; SET ROLE provenance_owner;").execute(&mut client).await.map(drop).unwrap();
    let insert = "INSERT INTO rest_consumer_fixture.item(key,name,external_system_id,external_id) VALUES ('42','item',$1::text::uuid,'42')";
    for source in [
        "00000000-0000-4000-8000-000000000001",
        "00000000-0000-4000-8000-000000000002",
    ] {
        sqlx::query(sqlx::AssertSqlSafe((insert).to_string()))
            .bind(&source)
            .execute(&mut client)
            .await
            .map(|result| result.rows_affected())
            .unwrap();
    }
    let duplicate = sqlx::query(sqlx::AssertSqlSafe((insert).to_string()))
        .bind(&"00000000-0000-4000-8000-000000000001")
        .execute(&mut client)
        .await
        .map(|result| result.rows_affected())
        .unwrap_err();
    assert_eq!(
        duplicate
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505")
    );
    let unknown = sqlx::query(sqlx::AssertSqlSafe((insert).to_string()))
        .bind(&"00000000-0000-4000-8000-000000000003")
        .execute(&mut client)
        .await
        .map(|result| result.rows_affected())
        .unwrap_err();
    assert_eq!(
        unknown
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23503")
    );
    assert_eq!(sqlx::query("SELECT count(DISTINCT external_system_id),count(DISTINCT external_id) FROM rest_consumer_fixture.item").fetch_one(&mut client).await.unwrap().get::<i64, _>(0),2);
    sqlx::raw_sql("RESET ROLE")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let schemas = ["rest_consumer_fixture"];
    read_catalog(&reader, &schemas).await.unwrap();
    sqlx::raw_sql("CREATE TABLE platform.other(id uuid PRIMARY KEY); ALTER TABLE rest_consumer_fixture.item ADD COLUMN other uuid CONSTRAINT item_other_fkey REFERENCES platform.other(id)").execute(&mut client).await.map(drop).unwrap();
    assert!(read_catalog(&reader, &schemas).await.is_err());
}
