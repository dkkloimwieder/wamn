//! Database constraints for project integrations and environment connections.

fn run(assertions: &str) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    database
        .execute(&[
            "DO $$ BEGIN IF to_regrole('wamn_system') IS NULL THEN \
             CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE; \
             END IF; END $$;",
            wamn_control_provision::sql::ensure_db_owner_role_sql(),
            &format!(
                "ALTER DATABASE \"{}\" OWNER TO wamn_system",
                database.name()
            ),
            "SET ROLE wamn_system",
            wamn_control_provision::SYSTEM_SCHEMA_SQL,
            include_str!("integration_schema/fixture.sql"),
            assertions,
        ])
        .expect("the system schema enforces the integration constraints");
}

#[test]
fn immutable_rows_allow_only_connection_cascade_deletion() {
    run(include_str!("integration_schema/immutable.sql"));
}

#[test]
fn one_connection_per_integration_and_environment() {
    run(
        "DO $$ BEGIN
           BEGIN
             INSERT INTO registry.connection
               (integration_id,org,project,env,integration_type,target_id,active_generation,service_principal_id)
             SELECT integration_id,org,project,env,integration_type,target_id,1,
                    '00000000-0000-0000-0000-000000000003'::uuid
             FROM registry.connection WHERE id='00000000-0000-0000-0000-000000000031';
             ASSERT false, 'a second connection for one integration and environment must refuse';
           EXCEPTION WHEN unique_violation THEN
             ASSERT SQLERRM LIKE '%connection_integration_id_env_key%', SQLERRM;
           END;
         END $$;",
    );
}

#[test]
fn active_generation_exists_and_credentials_belong_to_the_connection() {
    run("DO $$ BEGIN
           BEGIN
             UPDATE registry.connection SET active_generation=3
             WHERE id='00000000-0000-0000-0000-000000000031';
             SET CONSTRAINTS ALL IMMEDIATE;
             ASSERT false, 'a generation on another connection cannot satisfy the active reference';
           EXCEPTION WHEN foreign_key_violation THEN NULL; END;
           BEGIN
             INSERT INTO registry.connection_generation
               (connection_id,generation,credential_id,created_at,created_by)
             VALUES ('00000000-0000-0000-0000-000000000031',3,
                     '00000000-0000-0000-0000-000000000042',now(),
                     '00000000-0000-0000-0000-000000000001');
             ASSERT false, 'a generation cannot reference another connection credential';
           EXCEPTION WHEN foreign_key_violation THEN NULL; END;
         END $$;");
}

#[test]
fn active_generation_never_decreases() {
    run("DO $$ BEGIN
           BEGIN
             UPDATE registry.connection SET active_generation=1
             WHERE id='00000000-0000-0000-0000-000000000031';
             ASSERT false, 'an active generation cannot move backward';
           EXCEPTION WHEN SQLSTATE '55000' THEN NULL; END;
           UPDATE registry.connection SET active_generation=active_generation;
           INSERT INTO registry.connection_generation
             (connection_id,generation,parameters,created_at,created_by)
           VALUES ('00000000-0000-0000-0000-000000000031',3,'{}',now(),
                   '00000000-0000-0000-0000-000000000001');
           UPDATE registry.connection SET active_generation=3
           WHERE id='00000000-0000-0000-0000-000000000031';
           ASSERT (SELECT active_generation FROM registry.connection
                   WHERE id='00000000-0000-0000-0000-000000000031')=3;
         END $$;");
}

#[test]
fn active_resources_are_unique_per_target() {
    run(
        "DO $$ BEGIN
           UPDATE registry.connection SET resource_key='serial:device'
           WHERE id='00000000-0000-0000-0000-000000000031';
           BEGIN
             UPDATE registry.connection SET resource_key='serial:device'
             WHERE id='00000000-0000-0000-0000-000000000032';
             ASSERT false, 'two active connections cannot hold one resource on one target';
           EXCEPTION WHEN unique_violation THEN NULL; END;
           UPDATE registry.connection SET status='disabled'
           WHERE id='00000000-0000-0000-0000-000000000031';
           UPDATE registry.connection SET resource_key='serial:device'
           WHERE id='00000000-0000-0000-0000-000000000032';
           BEGIN
             UPDATE registry.connection SET status='active'
             WHERE id='00000000-0000-0000-0000-000000000031';
             ASSERT false, 'an active replacement prevents re-enabling the old resource holder';
           EXCEPTION WHEN unique_violation THEN NULL; END;
           UPDATE registry.project_envs SET target_id='00000000-0000-0000-0000-000000000052'
           WHERE org='acme' AND project='app' AND env='prod';
           UPDATE registry.connection SET env='prod', target_id='00000000-0000-0000-0000-000000000052',status='active'
           WHERE id='00000000-0000-0000-0000-000000000031';
         END $$;",
    );
}

#[test]
fn integration_and_external_system_names_never_change() {
    run("DO $$ BEGIN
           BEGIN
             UPDATE registry.integration SET name='changed'
             WHERE id='00000000-0000-0000-0000-000000000021';
             ASSERT false, 'an integration name cannot change';
           EXCEPTION WHEN SQLSTATE '55000' THEN NULL; END;
           BEGIN
             UPDATE registry.external_system SET name='changed';
             ASSERT false, 'an external system name cannot change';
           EXCEPTION WHEN SQLSTATE '55000' THEN NULL; END;
           UPDATE registry.integration SET name=name;
           UPDATE registry.external_system SET name=name;
         END $$;");
}

#[test]
fn project_identity_rejects_missing_parents_duplicate_names_and_empty_names() {
    run("DO $$ BEGIN
           BEGIN
             INSERT INTO registry.external_system (org,project,name) VALUES ('missing','app','erp');
             ASSERT false, 'an external system requires an existing project';
           EXCEPTION WHEN foreign_key_violation THEN NULL; END;
           BEGIN
             INSERT INTO registry.external_system (org,project,name) VALUES ('acme','app','erp');
             ASSERT false, 'a project identifies an external system by name';
           EXCEPTION WHEN unique_violation THEN NULL; END;
           BEGIN
             INSERT INTO registry.external_system (org,project,name) VALUES ('acme','app','');
             ASSERT false, 'an external system requires a name';
           EXCEPTION WHEN check_violation THEN NULL; END;
           BEGIN
             INSERT INTO registry.integration (org,project,name,integration_type,auth_type)
             VALUES ('missing','app','first','http','none');
             ASSERT false, 'an integration requires an existing project';
           EXCEPTION WHEN foreign_key_violation THEN NULL; END;
           BEGIN
             INSERT INTO registry.integration (org,project,name,integration_type,auth_type)
             VALUES ('acme','app','first','http','none');
             ASSERT false, 'a project identifies an integration by name';
           EXCEPTION WHEN unique_violation THEN NULL; END;
           BEGIN
             INSERT INTO registry.integration (org,project,name,integration_type,auth_type)
             VALUES ('acme','app','','http','none');
             ASSERT false, 'an integration requires a name';
           EXCEPTION WHEN check_violation THEN NULL; END;
         END $$;");
}

#[test]
fn integration_runtime_type_values_are_closed() {
    run(
        "DO $$ BEGIN
           INSERT INTO registry.integration_type (code,runtime_type,exclusive)
           VALUES ('host_type','host',false), ('wasm_type','wasm',false), ('native_type','native',false);
           BEGIN
             INSERT INTO registry.integration_type (code,runtime_type,exclusive)
             VALUES ('unsupported_type','component',false);
             ASSERT false, 'an integration runtime type must use a published runtime';
           EXCEPTION WHEN check_violation THEN NULL; END;
         END $$;",
    );
}
