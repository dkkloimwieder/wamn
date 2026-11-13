//! Issuer authority over the real database surface.
use wamn_control_provision::identity_role::identity_issuer_login;

#[tokio::test]
async fn targeted_issuer_grants_execute_on_postgres() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let mut server = wamn_test_postgres::start(&[])?;
    let database = server.create_database("wamn_system")?;
    let mut admin = wamn_postgres::connect(database.url()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("{} CREATE ROLE wamn_system NOLOGIN NOINHERIT; GRANT CREATE ON DATABASE wamn_system TO wamn_system; SET ROLE wamn_system; {} RESET ROLE;", wamn_control_provision::sql::ensure_db_owner_role_sql(), wamn_control_provision::SYSTEM_SCHEMA_SQL))).execute(&mut admin).await?;
    let issuer = "https://issuer.example.test";
    let role = identity_issuer_login(issuer)?;
    let sql = wamn_control_provision::test_database::issuer_login_sql(issuer, "fixture-password")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&mut admin)
        .await?;
    let mut url = url::Url::parse(database.url())?;
    url.set_username(&role).unwrap();
    url.set_password(Some("fixture-password")).unwrap();
    let mut issuer = wamn_postgres::connect(url.as_str()).await?;
    let restricted: bool = sqlx::query_scalar("SELECT NOT rolsuper AND NOT rolbypassrls AND NOT rolcreaterole FROM pg_roles WHERE rolname=current_user").fetch_one(&mut issuer).await?;
    assert!(restricted);
    assert!(
        sqlx::query("SELECT id FROM identity.principals")
            .fetch_all(&mut issuer)
            .await
            .is_ok()
    );
    assert!(
        sqlx::query("SELECT * FROM registry.orgs")
            .fetch_all(&mut issuer)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM identity.pats")
            .execute(&mut issuer)
            .await
            .is_err()
    );
    Ok(())
}
