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

#[test]
fn credential_columns_hold_only_references_and_audit_metadata() {
    run("DO $$ BEGIN
      ASSERT (SELECT array_agg(attname::text ORDER BY attnum) FROM pg_attribute
              WHERE attrelid='identity.credential'::regclass AND attnum>0 AND NOT attisdropped)
          = ARRAY['id','connection_id','store','ref','version','hint','created_at','created_by'];
    END $$;");
}

#[test]
fn retired_material_ages_from_the_first_replacement() {
    run("BEGIN;
      INSERT INTO identity.credential(id,connection_id,store,ref,version,created_at,created_by)
      VALUES ('00000000-0000-0000-0000-000000000043','00000000-0000-0000-0000-000000000031',
              'file','new','new',now(),'00000000-0000-0000-0000-000000000001');
      INSERT INTO registry.connection_generation(connection_id,generation,credential_id,created_at,created_by)
      VALUES ('00000000-0000-0000-0000-000000000031',3,'00000000-0000-0000-0000-000000000043',
              now()-interval '6 minutes','00000000-0000-0000-0000-000000000001'),
             ('00000000-0000-0000-0000-000000000031',4,'00000000-0000-0000-0000-000000000043',
              now(),'00000000-0000-0000-0000-000000000001');
      UPDATE registry.connection SET active_generation=4 WHERE id='00000000-0000-0000-0000-000000000031';
      COMMIT;
      DO $$ BEGIN
        ASSERT (SELECT count(*) FROM registry.retired_connection_credential('acme'))=1;
        ASSERT (SELECT ref FROM registry.retired_connection_credential('acme'))='first';
        ASSERT NOT EXISTS (SELECT FROM registry.retired_connection_credential('other'));
      END $$;");
}

#[test]
fn retired_material_stays_during_the_five_minute_grace() {
    run("BEGIN;
      INSERT INTO identity.credential(id,connection_id,store,ref,version,created_at,created_by)
      VALUES ('00000000-0000-0000-0000-000000000043','00000000-0000-0000-0000-000000000031',
              'file','new','new',now(),'00000000-0000-0000-0000-000000000001');
      INSERT INTO registry.connection_generation(connection_id,generation,credential_id,created_at,created_by)
      VALUES ('00000000-0000-0000-0000-000000000031',3,'00000000-0000-0000-0000-000000000043',
              now()-interval '4 minutes','00000000-0000-0000-0000-000000000001');
      UPDATE registry.connection SET active_generation=3 WHERE id='00000000-0000-0000-0000-000000000031';
      COMMIT;
      DO $$ BEGIN
        ASSERT NOT EXISTS (SELECT FROM registry.retired_connection_credential('acme'));
      END $$;");
}

fn publish_refuses(schema: &str, operations: &str, digest: &str, code: &str, message: &str) {
    run(&format!("DO $$ BEGIN
        BEGIN
            PERFORM registry.publish_integration('{code}',2,{digest},'{schema}'::jsonb,'{operations}',ARRAY['wamn:connection/http@0.1.0']);
            ASSERT false, 'invalid publication must refuse';
        EXCEPTION WHEN invalid_parameter_value THEN
            ASSERT SQLERRM LIKE '{message}%', SQLERRM;
        END;
    END $$;"));
}

#[test]
fn publish_refuses_removal_before_deprecation() {
    publish_refuses(
        r#"{"type":"object","properties":{}}"#,
        "wamn:connection/http",
        "NULL",
        "http",
        "field_removal_requires_deprecation",
    );
}

#[test]
fn publish_refuses_a_new_required_field() {
    publish_refuses(
        r#"{"type":"object","properties":{"primary-authority":{"type":"string","immutable":true},"tls-verification":{"type":"string"},"new":{"type":"string"}},"required":["new"]}"#,
        "wamn:connection/http",
        "NULL",
        "http",
        "required_field_requires_optional_predecessor",
    );
}

#[test]
fn publish_refuses_a_changed_json_type() {
    publish_refuses(
        r#"{"type":"object","properties":{"primary-authority":{"type":"integer","immutable":true},"tls-verification":{"type":"string"}}}"#,
        "wamn:connection/http",
        "NULL",
        "http",
        "field_type_changed",
    );
}

#[test]
fn publish_refuses_reserved_auth_parameters() {
    publish_refuses(
        r#"{"type":"object","properties":{"auth":{"type":"object"}}}"#,
        "wamn:connection/http",
        "NULL",
        "http",
        "reserved_auth_field",
    );
}

#[test]
fn publish_refuses_http_auth_with_another_interface() {
    publish_refuses(
        r#"{"type":"object","properties":{}}"#,
        "example:serial/client",
        "NULL",
        "http",
        "http_auth_requires_http_interface",
    );
}

#[test]
fn publish_refuses_wasm_without_a_digest() {
    publish_refuses(
        r#"{"type":"object","properties":{}}"#,
        "wamn:modbus/client",
        "NULL",
        "modbus",
        "digest_required",
    );
}

#[test]
fn publish_refuses_native_without_a_digest() {
    run(
        r#"INSERT INTO registry.integration_type(code,runtime_type,exclusive) VALUES ('native_test','native',false);
        DO $$ BEGIN
            BEGIN
                PERFORM registry.publish_integration('native_test',1,NULL,'{"type":"object","properties":{}}','example:native/client',ARRAY['example:native/call@1.0.0']);
                ASSERT false, 'native publication needs a digest';
            EXCEPTION WHEN invalid_parameter_value THEN ASSERT SQLERRM='digest_required', SQLERRM; END;
        END $$;"#,
    );
}

#[test]
fn connection_refuses_integration_deletion() {
    run("DO $$ DECLARE result jsonb; BEGIN
        result := registry.configure_integration('acme','integration.delete','{\"project\":\"app\",\"name\":\"first\"}');
        ASSERT result->>'error'='referenced' AND result->>'field'='connection',result::text;
        ASSERT EXISTS(SELECT FROM registry.integration WHERE name='first');
    END $$;");
}

#[test]
fn integration_refuses_external_system_deletion() {
    run("DO $$ DECLARE result jsonb; BEGIN
        result := registry.configure_external_system('acme','external_system.delete','{\"project\":\"app\",\"name\":\"erp\"}');
        ASSERT result->>'error'='referenced' AND result->>'field'='integration',result::text;
        ASSERT EXISTS(SELECT FROM registry.external_system WHERE name='erp');
    END $$;");
}

#[test]
fn apply_cannot_write_system_connection_authority() {
    run("DO $$ DECLARE relation text; privilege text; BEGIN
        FOREACH relation IN ARRAY ARRAY['connection','integration','connection_generation'] LOOP
            FOREACH privilege IN ARRAY ARRAY['INSERT','UPDATE','DELETE','TRUNCATE'] LOOP
                ASSERT NOT has_table_privilege('wamn_db_owner','registry.'||relation,privilege), relation||':'||privilege;
            END LOOP;
            ASSERT NOT has_any_column_privilege('wamn_db_owner','registry.'||relation,'INSERT,UPDATE'),relation;
        END LOOP;
    END $$;");
}
