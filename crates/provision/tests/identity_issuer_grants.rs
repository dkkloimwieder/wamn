//! The issuer changes identity state without changing project registry state.

pub mod common;
use common::grants;

#[tokio::test]
#[ignore = "requires: docker"]
async fn identity_issuer_refusals_and_catalog_rules() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let mut url = postgres
        .create_database(wamn_provision::identity_role::IDENTITY_ISSUER_DATABASE)
        .await
        .unwrap();
    common::database::system(url.as_str()).await;
    let mut admin = wamn_postgres::connect(url.as_str()).await.unwrap();
    let issuer = "https://identity.example.test";
    grants::execute(
        &mut admin,
        &common::database::issuer_login_sql(issuer, "issuer-test"),
    )
    .await;
    let login = wamn_provision::identity_role::identity_issuer_login(issuer).unwrap();
    url.set_username(login.as_str()).unwrap();
    url.set_password(Some("issuer-test")).unwrap();
    let mut connection = wamn_postgres::connect(url.as_str()).await.unwrap();
    let restricted: bool = sqlx::query_scalar("SELECT NOT (rolsuper OR rolbypassrls OR rolcreatedb OR rolcreaterole OR rolreplication) FROM pg_roles WHERE rolname=current_user")
        .fetch_one(&mut connection).await.unwrap();
    assert!(restricted, "the issuer can bypass database privileges");
    let writes: Vec<String> = sqlx::query_scalar(
        "SELECT c.oid::regclass::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
         WHERE n.nspname NOT LIKE 'pg_%' AND n.nspname NOT IN ('information_schema','identity')
           AND c.relkind IN ('r','p','v','m','f')
           AND (has_table_privilege($1,c.oid,'UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER')
                OR has_any_column_privilege($1,c.oid,'UPDATE,REFERENCES')
                OR (c.oid<>'platform.tx_identity'::regclass AND
                    (has_table_privilege($1,c.oid,'INSERT') OR has_any_column_privilege($1,c.oid,'INSERT'))))",
    ).bind(&login).fetch_all(&mut admin).await.unwrap();
    assert!(
        writes.is_empty(),
        "issuer writes outside identity storage: {writes:?}"
    );
    grants::execute(&mut connection, "SELECT id FROM identity.principals").await;
    for statement in [
        "SELECT * FROM registry.orgs",
        "DELETE FROM registry.projects",
        "SELECT * FROM platform.login_principal",
    ] {
        grants::refuses(url.as_str(), statement).await;
    }
    for attribute in ["SUPERUSER", "BYPASSRLS"] {
        grants::refuses(
            url.as_str(),
            &format!(
                "ALTER ROLE {} {attribute}",
                postgres_protocol::escape::escape_identifier(login.as_str())
            ),
        )
        .await;
    }
}
