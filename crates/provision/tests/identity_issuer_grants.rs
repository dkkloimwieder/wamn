//! The issuer changes identity state without changing project registry state.

#[path = "support/grants.rs"]
pub mod grants;

#[tokio::test]
#[ignore = "requires: postgres"]
async fn identity_issuer_refusals_and_catalog_rules() {
    let mut server = wamn_test_postgres::start(&[]).unwrap();
    let database = server
        .create_database(wamn_provision::identity_role::IDENTITY_ISSUER_DATABASE)
        .await
        .unwrap();
    wamn_provision::test_database::system(database.url()).await;
    let mut admin = wamn_postgres::connect(database.url()).await.unwrap();
    let issuer = "https://identity.example.test";
    grants::execute(
        &mut admin,
        &wamn_provision::test_database::issuer_login_sql(issuer, "issuer-test").unwrap(),
    )
    .await;
    let login = wamn_provision::identity_role::identity_issuer_login(issuer).unwrap();
    let mut url = url::Url::parse(database.url()).unwrap();
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
        "DELETE FROM identity.pats",
        "DELETE FROM registry.projects",
        "UPDATE identity.pats SET token_hash=repeat('f',64)",
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
