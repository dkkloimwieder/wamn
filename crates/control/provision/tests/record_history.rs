//! Fresh schemas stamp records and production writers bind their actor.

use wamn_control_provision::{
    CONTROL_BOOTSTRAP_SQL, PlatformComponent, bind_platform_principal_sql,
};

#[test]
fn fresh_control_history_stamps_production_writes() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    database.execute(&[
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$;",

        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $$ BEGIN EXECUTE format('ALTER DATABASE %I OWNER TO wamn_system',current_database()); END $$; SET ROLE wamn_system;",
        CONTROL_BOOTSTRAP_SQL[0], CONTROL_BOOTSTRAP_SQL[1], "RESET ROLE",
    ]).unwrap();
    database.execute(&[
        "BEGIN",
        &bind_platform_principal_sql(PlatformComponent::Provisioning),
        r"
        INSERT INTO catalog.project_environments (project_env_key,org,project,env,instance_suffix,disposable,environment_instance) VALUES ('history','org','project','dev','abcdefgh',false,'');
        INSERT INTO wamn_authority.author_login_projects (login_identity,project_env_key,org_id,project_id,environment) VALUES ('login','history','org','project','dev');
    ",
        "COMMIT",
    ]).unwrap();
    let actor = PlatformComponent::Provisioning.principal_id();
    for relation in [
        "catalog.project_environments",
        "wamn_authority.author_login_projects",
    ] {
        database.execute(&[&format!("DO $$ BEGIN ASSERT (SELECT count(*)=1 AND bool_and(created_by='{actor}'::uuid AND updated_by='{actor}'::uuid AND created_at=updated_at AND isfinite(created_at)) FROM {relation}), '{relation}: stamps'; END $$;")]).unwrap();
    }
    // The production SQL writers stamp their rows with the transaction actor.
    database.execute(&["BEGIN", &bind_platform_principal_sql(PlatformComponent::Provisioning), "SET LOCAL app.project_env_key='history';", &format!("PREPARE projection(text,text,text,text,text,boolean) AS {}; EXECUTE projection('production','org','project','dev','abcdefgh',false);", wamn_control_provision::sql::insert_project_env_key_environment_sql()), &format!("PREPARE mapping(text,text,text,text,text) AS {}; EXECUTE mapping('production-login','production','org','project','dev');", wamn_control_provision::sql::upsert_control_author_project_env_key_mapping_sql()), "COMMIT"]).unwrap();
    for (relation, predicate) in [
        (
            "catalog.project_environments",
            "project_env_key='production'",
        ),
        (
            "wamn_authority.author_login_projects",
            "login_identity='production-login'",
        ),
    ] {
        database.execute(&[&format!("DO $$ BEGIN ASSERT (SELECT count(*)=1 AND bool_and(created_by='{actor}'::uuid AND updated_by='{actor}'::uuid AND created_at=updated_at AND isfinite(created_at)) FROM {relation} WHERE {predicate}); END $$;")]).unwrap();
    }
}

#[test]
fn fresh_run_history_stamps_rows() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    database
        .execute(&[
            &wamn_control_provision::sql::ensure_app_acl_role_sql(),
            wamn_catalog::CATALOG_SCHEMA_SQL,
            include_str!("../../../../deploy/sql/run-state.sql"),
            include_str!("../../../../deploy/sql/run-queue.sql"),
        ])
        .unwrap();
    database.execute(&[
        "BEGIN",
        &bind_platform_principal_sql(PlatformComponent::Provisioning),
        r"
        INSERT INTO wamn_run.environment_policies (project_env_key,expected_environment) VALUES ('history','dev');
        INSERT INTO wamn_run.runs (project_env_key,run_id,wiring_id,wiring_version,wiring_hash,binding_world_json,package_id,environment) VALUES ('history','run','fixture',1,'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'package','dev');
    ",
        "INSERT INTO wamn_run.run_queue(project_env_key,run_id) VALUES ('history','run');",
        "COMMIT",
    ]).unwrap();
    let actor = PlatformComponent::Provisioning.principal_id();
    for relation in ["wamn_run.environment_policies", "wamn_run.run_queue"] {
        database.execute(&[&format!("DO $$ BEGIN ASSERT (SELECT count(*)=1 AND bool_and(created_by='{actor}'::uuid AND updated_by='{actor}'::uuid AND created_at=updated_at AND isfinite(created_at)) FROM {relation}), '{relation}: stamps'; END $$;")]).unwrap();
    }
}
