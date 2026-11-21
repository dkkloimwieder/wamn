//! Managed platform logins retain component identity without role escalation.

pub mod common;
use common::grants;
use wamn_provision::{
    PlatformComponent, WorkloadRoleFamily, migrations::MigrationDatabase,
};

#[tokio::test]
#[ignore = "requires: docker"]
async fn platform_security_definers_have_restricted_authority() {
    security_definer_rules(MigrationDatabase::System).await;
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn project_platform_security_definers_have_restricted_authority() {
    security_definer_rules(MigrationDatabase::Project).await;
}

async fn security_definer_rules(scope: MigrationDatabase) {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let url = postgres.create_database("platform_logins").await.unwrap();
    let mut admin = wamn_postgres::connect(url.as_str()).await.unwrap();
    let existing_schemas: Vec<String> =
        sqlx::query_scalar("SELECT nspname::text FROM pg_namespace")
            .fetch_all(&mut admin)
            .await
            .unwrap();
    let _fixture = match scope {
        MigrationDatabase::System => {
            common::database::system(url.as_str()).await;
            None
        }
        MigrationDatabase::Project => {
            Some(grants::prepare(url, WorkloadRoleFamily::Migration).await)
        }
    };

    // The product bootstrap defines the schema set, including schemas added later.
    let violations: Vec<String> = sqlx::query_scalar(
        "SELECT p.oid::regprocedure::text FROM pg_proc p
         JOIN pg_namespace n ON n.oid=p.pronamespace
         WHERE p.prosecdef AND n.nspname <> ALL($1::text[])
           AND (p.proowner <> (SELECT datdba FROM pg_database WHERE datname = current_database())
                OR NOT EXISTS (SELECT FROM pg_options_to_table(p.proconfig)
                               WHERE option_name='search_path' AND option_value='pg_catalog')
                OR EXISTS (SELECT FROM aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a
                           WHERE (a.grantee=0 AND a.privilege_type='EXECUTE')
                              OR (a.grantee<>p.proowner AND a.is_grantable)))",
    )
    .bind(&existing_schemas)
    .fetch_all(&mut admin)
    .await
    .unwrap();
    assert!(
        violations.is_empty(),
        "{scope:?} platform functions expose definer authority: {violations:?}"
    );
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn project_platform_logins_cannot_escalate() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let url = postgres.create_database("platform_logins").await.unwrap();
    let fixture = grants::prepare(url.clone(), WorkloadRoleFamily::Migration).await;
    platform_rules(&url, MigrationDatabase::Project).await;
    drop(fixture);
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn system_platform_logins_cannot_escalate() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let url = postgres.create_database("platform_logins").await.unwrap();
    common::database::system(url.as_str()).await;
    platform_rules(&url, MigrationDatabase::System).await;
}

async fn platform_rules(admin: &url::Url, scope: MigrationDatabase) {
    let url = common::database::platform_migration_input(admin.as_str(), scope).await;
    let mut connection = wamn_postgres::connect(&url).await.unwrap();
    scope.require_login(&mut connection).await.unwrap();
    let rows: Vec<(wamn_identity_protocol::LoginName, wamn_identity_protocol::PrincipalId, bool)> = sqlx::query_as(
        "SELECT p.login,p.principal_id,NOT (r.rolsuper OR r.rolbypassrls OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication)
         FROM platform.login_principal p JOIN pg_roles r ON r.rolname=p.login",
    ).fetch_all(&mut wamn_postgres::connect(admin.as_str()).await.unwrap()).await.unwrap();
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
