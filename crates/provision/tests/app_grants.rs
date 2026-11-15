//! Least privilege through the authenticated login and catalog rules.

#[path = "support/grants.rs"]
mod grants;
use wamn_control_provision::WorkloadRoleFamily;

#[sqlx::test(migrations = false)]
async fn application_logins_cannot_reach_mapped_platform_logins(pool: sqlx::PgPool) {
    use wamn_control_provision::{WorkloadRoleIdentity, workload_login};

    let (mut fixture, _lock) = grants::family(&pool, WorkloadRoleFamily::App).await;
    let database_url = wamn_test_postgres::url(&pool);
    let database = database_url.path().trim_start_matches('/');
    let query_login = workload_login(
        WorkloadRoleFamily::Query,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &"t1--dev--project".parse().unwrap(),
            database,
        },
    )
    .unwrap();
    grants::execute(
        &mut fixture.admin,
        &wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::Query,
            database,
            &query_login,
            "grants-test",
        ),
    )
    .await;
    wamn_control_provision::test_database::platform_migration_input(
        database_url.as_str(),
        wamn_control_provision::migrations::MigrationDatabase::Project,
    )
    .await
    .unwrap();

    let violations: Vec<(
        wamn_identity_protocol::LoginName,
        wamn_identity_protocol::LoginName,
    )> = sqlx::query_as(
        "WITH RECURSIVE reachable(application_login, reachable_role) AS (\
           SELECT login.oid, membership.roleid FROM pg_roles login \
           JOIN pg_auth_members membership ON membership.member=login.oid \
           WHERE login.rolcanlogin AND (starts_with(login.rolname::text,'wamn_app_') \
                                    OR starts_with(login.rolname::text,'wamn_query_')) \
           UNION \
           SELECT reachable.application_login, membership.roleid FROM reachable \
           JOIN pg_auth_members membership ON membership.member=reachable.reachable_role\
         ) SELECT application.rolname::text, target.rolname::text FROM reachable \
         JOIN pg_roles application ON application.oid=reachable.application_login \
         JOIN pg_roles target ON target.oid=reachable.reachable_role \
         JOIN platform.login_principal mapped ON mapped.login=target.rolname",
    )
    .fetch_all(&mut fixture.admin)
    .await
    .unwrap();
    assert!(
        violations.is_empty(),
        "application logins hold membership of mapped platform logins: {violations:?}"
    );
}
