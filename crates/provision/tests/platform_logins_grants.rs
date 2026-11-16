//! Managed platform logins retain component identity without role escalation.

#[path = "support/grants.rs"]
pub mod grants;
use wamn_control_provision::{
    PlatformComponent, WorkloadRoleFamily, migrations::MigrationDatabase,
};

#[sqlx::test(migrations = false)]
async fn platform_security_definers_have_restricted_authority(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let existing_schemas: Vec<String> =
        sqlx::query_scalar("SELECT nspname::text FROM pg_namespace")
            .fetch_all(&pool)
            .await
            .unwrap();
    let url = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(url.as_str()).await;

    // The product bootstrap defines the schema set, including schemas added later.
    let violations: Vec<String> = sqlx::query_scalar(
        "SELECT p.oid::regprocedure::text FROM pg_proc p
         JOIN pg_namespace n ON n.oid=p.pronamespace
         WHERE p.prosecdef AND n.nspname <> ALL($1::text[])
           AND (p.proowner <> 'wamn_system'::regrole
                OR NOT EXISTS (SELECT FROM pg_options_to_table(p.proconfig)
                               WHERE option_name='search_path' AND option_value='pg_catalog')
                OR EXISTS (SELECT FROM aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a
                           WHERE (a.grantee=0 AND a.privilege_type='EXECUTE')
                              OR (a.grantee<>p.proowner AND a.is_grantable)))",
    )
    .bind(&existing_schemas)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        violations.is_empty(),
        "platform functions expose definer authority: {violations:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn project_platform_logins_cannot_escalate(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let fixture = grants::prepare(&pool, WorkloadRoleFamily::Migration).await;
    platform_rules(&pool, MigrationDatabase::Project).await;
    drop(fixture);
}

#[sqlx::test(migrations = false)]
async fn system_platform_logins_cannot_escalate(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let url = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(url.as_str()).await;
    platform_rules(&pool, MigrationDatabase::System).await;
}

async fn platform_rules(pool: &sqlx::PgPool, scope: MigrationDatabase) {
    let admin = wamn_test_postgres::url(pool);
    let url =
        wamn_control_provision::test_database::platform_migration_input(admin.as_str(), scope)
            .await
            .unwrap();
    let mut connection = wamn_postgres::connect(&url).await.unwrap();
    scope.require_login(&mut connection).await.unwrap();
    let rows: Vec<(wamn_identity_protocol::LoginName, wamn_identity_protocol::PrincipalId, bool)> = sqlx::query_as(
        "SELECT p.login,p.principal_id,NOT (r.rolsuper OR r.rolbypassrls OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication)
         FROM platform.login_principal p JOIN pg_roles r ON r.rolname=p.login",
    ).fetch_all(pool).await.unwrap();
    assert!(
        !rows.is_empty(),
        "the product must install its component login mapping"
    );
    for (login, principal, restricted) in rows {
        assert!(
            restricted,
            "platform login {login} can bypass database privileges"
        );
        assert!(
            PlatformComponent::ALL
                .into_iter()
                .any(|component| component.principal_id() == principal),
            "{login} maps to an unknown platform principal: {principal}"
        );
    }
    let login = url::Url::parse(&url).unwrap();
    for attribute in ["SUPERUSER", "BYPASSRLS"] {
        grants::refuses(
            &url,
            &format!(
                "ALTER ROLE {} {attribute}",
                postgres_protocol::escape::escape_identifier(login.username())
            ),
        )
        .await;
    }
}
