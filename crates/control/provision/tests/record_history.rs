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
        &wamn_control_provision::sql::ensure_control_author_acl_role_sql(),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $$ BEGIN EXECUTE format('ALTER DATABASE %I OWNER TO wamn_system',current_database()); END $$; SET ROLE wamn_system;",
        CONTROL_BOOTSTRAP_SQL[0], CONTROL_BOOTSTRAP_SQL[1], "RESET ROLE",
    ]).unwrap();
    database.execute(&[
        "BEGIN",
        &bind_platform_principal_sql(PlatformComponent::Provisioning),
        r"
        INSERT INTO catalog.tenant_environments (env_key,org,project,env,instance_suffix,disposable,environment_instance) VALUES ('history','org','project','dev','abcdefgh',false,'');
        INSERT INTO catalog.authoring_command_audit (env_key,command_id,command_type,principal_id,principal_type,principal_subject,effective_role,org,project,environment,target_ref,request_hash,outcome_bytes) VALUES ('history','command','gate','actor','user','actor','project-author','org','project','dev','target','sha256:'||repeat('a',64),'outcome');
        INSERT INTO wamn_run.gate_reports (env_key,wiring_hash,passed,summary) VALUES ('history','sha256:'||repeat('a',64),true,'{}');
        INSERT INTO wamn_authority.author_login_tenants (login_identity,env_key,org_id,project_id,environment) VALUES ('login','history','org','project','dev');
    ",
        "COMMIT",
    ]).unwrap();
    let actor = PlatformComponent::Provisioning.principal_id();
    for relation in [
        "catalog.tenant_environments",
        "catalog.authoring_command_audit",
        "wamn_run.gate_reports",
        "wamn_authority.author_login_tenants",
    ] {
        database.execute(&[&format!("DO $$ BEGIN ASSERT (SELECT count(*)=1 AND bool_and(created_by='{actor}'::uuid AND updated_by='{actor}'::uuid AND created_at=updated_at AND isfinite(created_at)) FROM {relation}), '{relation}: stamps'; END $$;")]).unwrap();
    }
    // The production SQL writers stamp their rows with the transaction actor.
    database.execute(&["BEGIN", &bind_platform_principal_sql(PlatformComponent::Provisioning), "SET LOCAL app.env_key='history';", &format!("PREPARE projection(text,text,text,text,text,boolean) AS {}; EXECUTE projection('production','org','project','dev','abcdefgh',false);", wamn_control_provision::sql::insert_env_key_environment_sql()), &format!("PREPARE mapping(text,text,text,text,text) AS {}; EXECUTE mapping('production-login','production','org','project','dev');", wamn_control_provision::sql::upsert_control_author_env_key_mapping_sql()), "COMMIT"]).unwrap();
    for (relation, predicate) in [
        ("catalog.tenant_environments", "env_key='production'"),
        (
            "wamn_authority.author_login_tenants",
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
            wamn_schema_control::ensure_scenario_author_role_sql(),
            wamn_catalog::CATALOG_SCHEMA_SQL,
            include_str!("../../../../deploy/sql/run-state.sql"),
            include_str!("../../../../deploy/sql/run-queue.sql"),
        ])
        .unwrap();
    database.execute(&[
        "BEGIN",
        &bind_platform_principal_sql(PlatformComponent::Provisioning),
        r"
        INSERT INTO wamn_run.environment_policies (env_key,expected_environment,durability_class) VALUES ('history','dev','standard');
        INSERT INTO wamn_run.runs (env_key,run_id,flow_id,flow_version,package_id,environment) VALUES ('history','run','flow',1,'package','dev');
        INSERT INTO wamn_run.effect_attempts (env_key,attempt_id,run_id,root_plan_hash,current_plan_hash,local_node_id,source_artifact_hash,requirement_name,occurrence,seq,generation_fact_type,attempt_deadline_at,attempt_input_ref) VALUES ('history','11111111-1111-4111-8111-111111111111','run','sha256:'||repeat('a',64),'sha256:'||repeat('a',64),'node','sha256:'||repeat('a',64),'database',0,0,'not-required',now()+interval '1 hour','input');
        INSERT INTO wamn_run.effect_attempt_dispatches (env_key,attempt_id,attempt_started_at,run_id,frame_id,local_node_id,occurrence) SELECT env_key,attempt_id,attempt_started_at,run_id,frame_id,local_node_id,occurrence FROM wamn_run.effect_attempts;
        INSERT INTO wamn_run.effect_attempt_outcomes (env_key,attempt_id,dispatched_at,outcome_status) SELECT env_key,attempt_id,dispatched_at,'success' FROM wamn_run.effect_attempt_dispatches;
        INSERT INTO wamn_run.operator_run_actions (env_key,correlation_id,run_id,action_type,basis,evidence_ref,principal,principal_type) VALUES ('history','action','run','terminalize-effect-uncertain','external-evidence','case','operator','database-role');
    ",
        "INSERT INTO wamn_run.run_queue(env_key,run_id) VALUES ('history','run');",
        "COMMIT",
    ]).unwrap();
    let actor = PlatformComponent::Provisioning.principal_id();
    for relation in [
        "wamn_run.environment_policies",
        "wamn_run.effect_attempts",
        "wamn_run.effect_attempt_dispatches",
        "wamn_run.effect_attempt_outcomes",
        "wamn_run.operator_run_actions",
        "wamn_run.run_queue",
    ] {
        database.execute(&[&format!("DO $$ BEGIN ASSERT (SELECT count(*)=1 AND bool_and(created_by='{actor}'::uuid AND updated_by='{actor}'::uuid AND created_at=updated_at AND isfinite(created_at)) FROM {relation}), '{relation}: stamps'; END $$;")]).unwrap();
    }

    // The retained operator evidence stays immutable and denies guest writes.
    database.execute(&[r"
        DO $$
        DECLARE role_name text; verb text; refusal text;
        BEGIN
            FOREACH role_name IN ARRAY ARRAY['wamn_app', 'wamn_scenario_author'] LOOP
                FOREACH verb IN ARRAY ARRAY['INSERT', 'UPDATE', 'DELETE'] LOOP
                    ASSERT NOT has_table_privilege(role_name, 'wamn_run.operator_run_actions', verb),
                        format('%s has no action %s', role_name, verb);
                END LOOP;
            END LOOP;
            FOREACH verb IN ARRAY ARRAY[
                'UPDATE wamn_run.operator_run_actions SET evidence_ref=evidence_ref WHERE run_id=''run''',
                'DELETE FROM wamn_run.operator_run_actions WHERE run_id=''run'''
            ] LOOP
                refusal := NULL;
                BEGIN
                    EXECUTE verb;
                EXCEPTION WHEN object_not_in_prerequisite_state THEN refusal := SQLERRM; END;
                ASSERT refusal = 'operator-run-action-immutable',
                    format('operator action must stay immutable, got %s', coalesce(refusal, 'no refusal'));
            END LOOP;
        END $$;
    "]).unwrap();
}
