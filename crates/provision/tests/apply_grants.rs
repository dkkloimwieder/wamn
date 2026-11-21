//! The apply login cannot assume the system owner.

use wamn_provision::apply::APPLY_GRANTS_SQL;

pub mod common;

/// The apply group stays a group, its login cannot assume the system owner, and the
/// login still locks the org membership rows that the grant projection reads.
const APPLY_AUTHORITY_SQL: &str = r"
    DO $denials$ BEGIN
        ASSERT (SELECT NOT rolcanlogin FROM pg_roles WHERE rolname='wamn_apply');
        ASSERT (SELECT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole AND NOT rolreplication AND NOT rolbypassrls FROM pg_roles WHERE rolname='wamn_apply_fixture');
        ASSERT NOT pg_has_role('wamn_apply_fixture', 'wamn_system', 'MEMBER');
    END $denials$;
    SET SESSION AUTHORIZATION wamn_apply_fixture;
    DO $role_denial$ BEGIN
        BEGIN
            SET ROLE wamn_system;
            RAISE EXCEPTION 'apply assumed the system owner';
        EXCEPTION WHEN insufficient_privilege THEN NULL; END;
        ASSERT current_user = 'wamn_apply_fixture';
    END $role_denial$;
    SELECT principal_id FROM identity.org_memberships ORDER BY principal_id FOR UPDATE;
    RESET SESSION AUTHORIZATION;
";

#[tokio::test]
#[ignore = "requires: docker"]
async fn apply_cannot_assume_the_system_owner() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = postgres.create_database("apply_grants").await.unwrap();
    common::database::system(database.as_str()).await;
    let batches: &[&str] = &[
        "CREATE ROLE wamn_apply NOLOGIN NOINHERIT; CREATE ROLE wamn_apply_fixture LOGIN IN ROLE wamn_apply",
        &wamn_provision::sql::ensure_platform_group_role_sql(),
        APPLY_GRANTS_SQL,
        APPLY_AUTHORITY_SQL,
    ];
    let mut setup_connection = wamn_postgres::connect(database.as_str()).await.unwrap();
    for batch in batches {
        sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
            .execute(&mut setup_connection)
            .await
            .unwrap();
    }
}
