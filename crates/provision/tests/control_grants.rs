//! Control authority stays within its org and cannot publish connection bindings.

#[path = "support/grants.rs"]
mod grants;
use wamn_control_provision::WorkloadRoleFamily;

#[sqlx::test(migrations = false)]
async fn control_refusals_and_catalog_rules(pool: sqlx::PgPool) {
    let (mut fixture, _lock) = grants::family(&pool, WorkloadRoleFamily::Control).await;
    for statement in [
        "INSERT INTO catalog.release_connection DEFAULT VALUES",
        "DELETE FROM catalog.connection_binding",
    ] {
        grants::refuses(&fixture.url, statement).await;
    }
    for org in ["acme", "other"] {
        grants::execute(
            &mut fixture.admin,
            &wamn_control_provision::test_database::seed_org_project_env_sql(
                &org.parse().unwrap(),
                &"orders".parse().unwrap(),
                &"dev".parse().unwrap(),
            ),
        )
        .await;
    }
    let mut control = wamn_postgres::connect(&fixture.url).await.unwrap();
    grants::execute(&mut control, r#"
        BEGIN;
        INSERT INTO platform.tx_identity(principal_id,role,application)
            VALUES('770df186-ac15-579e-b46b-c297cae2011b','','fixture');
        DO $isolation$ BEGIN
            ASSERT EXISTS (SELECT FROM registry.projects WHERE org='acme');
            ASSERT NOT EXISTS (SELECT FROM registry.projects WHERE org<>'acme');
            INSERT INTO registry.projects(org,id) VALUES ('acme','allowed');
            BEGIN
                INSERT INTO registry.projects(org,id) VALUES ('other','refused');
                RAISE EXCEPTION 'cross-org write succeeded';
            EXCEPTION WHEN insufficient_privilege THEN NULL; END;
            BEGIN
                PERFORM registry.configure_external_system('other','external_system.list','{"project":"orders"}'::jsonb);
                RAISE EXCEPTION 'cross-org function succeeded';
            EXCEPTION WHEN insufficient_privilege THEN NULL; END;
        END $isolation$;
        COMMIT;
    "#).await;
}
