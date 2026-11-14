//! Database constraints for project integrations and environment connections.

async fn run(database: url::Url, assertions: &str) {
    let _lock = wamn_test_postgres::lock();
    {
        let batches: &[&str] = &[
            "DO $$ BEGIN IF to_regrole('wamn_system') IS NULL THEN \
             CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE; \
             END IF; END $$;",
            wamn_control_provision::sql::ensure_db_owner_role_sql(),
            &format!(
                "ALTER DATABASE \"{}\" OWNER TO wamn_system",
                database.path().trim_start_matches('/')
            ),
            "SET ROLE wamn_system",
            wamn_control_provision::SYSTEM_SCHEMA_SQL,
            include_str!("integration_schema/fixture.sql"),
            assertions,
        ];
        let mut setup_connection = wamn_postgres::connect(database.as_str()).await.unwrap();
        for batch in batches {
            sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
                .execute(&mut setup_connection)
                .await
                .expect("the system schema enforces the integration constraints");
        }
    }
}

#[sqlx::test(migrations = false)]
async fn immutable_rows_allow_only_connection_cascade_deletion(pool: sqlx::PgPool) {
    run(
        wamn_test_postgres::url(&pool),
        include_str!("integration_schema/immutable.sql"),
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn one_connection_per_integration_and_environment(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool),
        "DO $$ BEGIN
           BEGIN
             INSERT INTO registry.connection
               (integration_id,org,project,env,integration_type,target_id,active_generation,service_principal_id)
             SELECT integration_id,org,project,env,integration_type,target_id,1,
                    '00000000-0000-0000-0000-000000000003'::uuid
             FROM registry.connection WHERE id='00000000-0000-0000-0000-000000000031';
             ASSERT false, 'a second connection for one integration and environment must refuse';
           EXCEPTION WHEN unique_violation THEN
             ASSERT SQLERRM LIKE '%connection_integration_id_project_env_key%', SQLERRM;
           END;
         END $$;",
    ).await;
}

#[sqlx::test(migrations = false)]
async fn active_generation_exists_and_credentials_belong_to_the_connection(pool: sqlx::PgPool) {
    run(
        wamn_test_postgres::url(&pool),
        "DO $$ BEGIN
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
         END $$;",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn active_generation_never_decreases(pool: sqlx::PgPool) {
    run(
        wamn_test_postgres::url(&pool),
        "DO $$ BEGIN
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
         END $$;",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn active_resources_are_unique_per_target(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool),
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
           UPDATE registry.environments SET target_id='00000000-0000-0000-0000-000000000052'
           WHERE org='acme' AND project='app' AND env='prod';
           UPDATE registry.connection SET env='prod', target_id='00000000-0000-0000-0000-000000000052',status='active'
           WHERE id='00000000-0000-0000-0000-000000000031';
         END $$;",
    ).await;
}

#[sqlx::test(migrations = false)]
async fn integration_and_external_system_names_never_change(pool: sqlx::PgPool) {
    run(
        wamn_test_postgres::url(&pool),
        "DO $$ BEGIN
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
         END $$;",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn project_identity_rejects_missing_parents_duplicate_names_and_empty_names(
    pool: sqlx::PgPool,
) {
    run(
        wamn_test_postgres::url(&pool),
        "DO $$ BEGIN
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
         END $$;",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn integration_runtime_type_values_are_closed(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool),
        "DO $$ BEGIN
           INSERT INTO registry.integration_type (code,runtime_type,exclusive)
           VALUES ('host_type','host',false), ('wasm_type','wasm',false), ('native_type','native',false);
           BEGIN
             INSERT INTO registry.integration_type (code,runtime_type,exclusive)
             VALUES ('unsupported_type','component',false);
             ASSERT false, 'an integration runtime type must use a published runtime';
           EXCEPTION WHEN check_violation THEN NULL; END;
         END $$;",
    ).await;
}

#[sqlx::test(migrations = false)]
async fn credentials_refuse_the_removed_store(pool: sqlx::PgPool) {
    run(
        wamn_test_postgres::url(&pool),
        "DO $$ BEGIN
      BEGIN
        INSERT INTO identity.credential(id,connection_id,store,ref,version,created_at,created_by)
        VALUES ('00000000-0000-0000-0000-000000000043','00000000-0000-0000-0000-000000000031',
                'file','new','new',now(),'00000000-0000-0000-0000-000000000001');
        ASSERT false, 'the removed credential store must refuse';
      EXCEPTION WHEN check_violation THEN NULL; END;
    END $$;",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn retired_material_ages_from_the_first_replacement(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool), "BEGIN;
      INSERT INTO identity.credential(id,connection_id,store,ref,version,created_at,created_by)
      VALUES ('00000000-0000-0000-0000-000000000043','00000000-0000-0000-0000-000000000031',
              'k8s','new','new',now(),'00000000-0000-0000-0000-000000000001');
      -- The owner seeds historical generations before the insert stamp resumes.
      ALTER TABLE registry.connection_generation DISABLE TRIGGER wamn_record_history_stamp;
      INSERT INTO registry.connection_generation(connection_id,generation,credential_id,created_at,created_by)
      VALUES ('00000000-0000-0000-0000-000000000031',3,'00000000-0000-0000-0000-000000000043',
              now()-interval '6 minutes','00000000-0000-0000-0000-000000000001'),
             ('00000000-0000-0000-0000-000000000031',4,'00000000-0000-0000-0000-000000000043',
              now(),'00000000-0000-0000-0000-000000000001');
      ALTER TABLE registry.connection_generation ENABLE TRIGGER wamn_record_history_stamp;
      UPDATE registry.connection SET active_generation=4 WHERE id='00000000-0000-0000-0000-000000000031';
      COMMIT;
      DO $$ BEGIN
        ASSERT (SELECT count(*) FROM registry.retired_connection_credential('acme'))=1;
        ASSERT (SELECT ref FROM registry.retired_connection_credential('acme'))='first';
        ASSERT NOT EXISTS (SELECT FROM registry.retired_connection_credential('other'));
      END $$;").await;
}

#[sqlx::test(migrations = false)]
async fn retired_material_stays_during_the_five_minute_grace(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool), "BEGIN;
      INSERT INTO identity.credential(id,connection_id,store,ref,version,created_at,created_by)
      VALUES ('00000000-0000-0000-0000-000000000043','00000000-0000-0000-0000-000000000031',
              'k8s','new','new',now(),'00000000-0000-0000-0000-000000000001');
      -- The owner seeds historical generations before the insert stamp resumes.
      ALTER TABLE registry.connection_generation DISABLE TRIGGER wamn_record_history_stamp;
      INSERT INTO registry.connection_generation(connection_id,generation,credential_id,created_at,created_by)
      VALUES ('00000000-0000-0000-0000-000000000031',3,'00000000-0000-0000-0000-000000000043',
              now()-interval '4 minutes','00000000-0000-0000-0000-000000000001');
      ALTER TABLE registry.connection_generation ENABLE TRIGGER wamn_record_history_stamp;
      UPDATE registry.connection SET active_generation=3 WHERE id='00000000-0000-0000-0000-000000000031';
      COMMIT;
      DO $$ BEGIN
        ASSERT NOT EXISTS (SELECT FROM registry.retired_connection_credential('acme'));
      END $$;").await;
}

async fn publish_refuses(
    pool: &sqlx::PgPool,
    schema: &str,
    operations: &str,
    digest: &str,
    code: &str,
    message: &str,
) {
    run(wamn_test_postgres::url(pool), &format!("DO $$ BEGIN
        BEGIN
            PERFORM registry.publish_integration('{code}',2,{digest},'{schema}'::jsonb,'{operations}',ARRAY['wamn:connection/http@0.1.0'],'{{}}');
            ASSERT false, 'invalid publication must refuse';
        EXCEPTION WHEN invalid_parameter_value THEN
            ASSERT SQLERRM LIKE '{message}%', SQLERRM;
        END;
    END $$;")).await;
}

#[sqlx::test(migrations = false)]
async fn publish_refuses_removal_before_deprecation(pool: sqlx::PgPool) {
    publish_refuses(
        &pool,
        r#"{"type":"object","properties":{}}"#,
        "wamn:connection/http",
        "NULL",
        "http",
        "field_removal_requires_deprecation",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn publish_refuses_a_new_required_field(pool: sqlx::PgPool) {
    publish_refuses(
        &pool,
        r#"{"type":"object","properties":{"primary-authority":{"type":"string","immutable":true},"tls-verification":{"type":"string"},"new":{"type":"string"}},"required":["new"]}"#,
        "wamn:connection/http",
        "NULL",
        "http",
        "required_field_requires_optional_predecessor",
    ).await;
}

#[sqlx::test(migrations = false)]
async fn publish_refuses_a_changed_json_type(pool: sqlx::PgPool) {
    publish_refuses(
        &pool,
        r#"{"type":"object","properties":{"primary-authority":{"type":"integer","immutable":true},"tls-verification":{"type":"string"}}}"#,
        "wamn:connection/http",
        "NULL",
        "http",
        "field_type_changed",
    ).await;
}

#[sqlx::test(migrations = false)]
async fn publish_refuses_reserved_auth_parameters(pool: sqlx::PgPool) {
    publish_refuses(
        &pool,
        r#"{"type":"object","properties":{"auth":{"type":"object"}}}"#,
        "wamn:connection/http",
        "NULL",
        "http",
        "reserved_auth_field",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn publish_refuses_http_auth_with_another_interface(pool: sqlx::PgPool) {
    publish_refuses(
        &pool,
        r#"{"type":"object","properties":{}}"#,
        "example:serial/client",
        "NULL",
        "http",
        "http_auth_requires_http_interface",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn publish_refuses_wasm_without_a_digest(pool: sqlx::PgPool) {
    publish_refuses(
        &pool,
        r#"{"type":"object","properties":{}}"#,
        "wamn:modbus/client",
        "NULL",
        "modbus",
        "digest_required",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn publish_refuses_native_without_a_digest(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool),
        r#"INSERT INTO registry.integration_type(code,runtime_type,exclusive) VALUES ('native_test','native',false);
        DO $$ BEGIN
            BEGIN
                PERFORM registry.publish_integration('native_test',1,NULL,'{"type":"object","properties":{}}','example:native/client',ARRAY['example:native/call@1.0.0'],'{}');
                ASSERT false, 'native publication needs a digest';
            EXCEPTION WHEN invalid_parameter_value THEN ASSERT SQLERRM='digest_required', SQLERRM; END;
        END $$;"#,
    ).await;
}

#[sqlx::test(migrations = false)]
async fn connection_refuses_integration_deletion(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool), "DO $$ DECLARE result jsonb; BEGIN
        result := registry.configure_integration('acme','integration.delete','{\"project\":\"app\",\"name\":\"first\"}');
        ASSERT result->>'error'='referenced' AND result->>'field'='connection',result::text;
        ASSERT EXISTS(SELECT FROM registry.integration WHERE name='first');
    END $$;").await;
}

#[sqlx::test(migrations = false)]
async fn integration_refuses_external_system_deletion(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool), "DO $$ DECLARE result jsonb; BEGIN
        result := registry.configure_external_system('acme','external_system.delete','{\"project\":\"app\",\"name\":\"erp\"}');
        ASSERT result->>'error'='referenced' AND result->>'field'='integration',result::text;
        ASSERT EXISTS(SELECT FROM registry.external_system WHERE name='erp');
    END $$;").await;
}

#[sqlx::test(migrations = false)]
async fn apply_cannot_write_system_connection_authority(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool), "DO $$ DECLARE relation text; privilege text; BEGIN
        FOREACH relation IN ARRAY ARRAY['connection','integration','connection_generation'] LOOP
            FOREACH privilege IN ARRAY ARRAY['INSERT','UPDATE','DELETE','TRUNCATE'] LOOP
                ASSERT NOT has_table_privilege('wamn_db_owner','registry.'||relation,privilege), relation||':'||privilege;
            END LOOP;
            ASSERT NOT has_any_column_privilege('wamn_db_owner','registry.'||relation,'INSERT,UPDATE'),relation;
        END LOOP;
    END $$;").await;
}

#[sqlx::test(migrations = false)]
async fn environment_delete_removes_its_connections_and_disables_their_principals(
    pool: sqlx::PgPool,
) {
    run(wamn_test_postgres::url(&pool), &format!(
        "DO $test$ DECLARE removed bigint; expected bigint; BEGIN
           SELECT count(*) INTO expected FROM identity.credential k
             JOIN registry.connection c ON c.id=k.connection_id
             WHERE c.org='acme' AND c.project='app' AND c.env='dev';
           EXECUTE $sql${}$sql$ INTO removed USING 'acme','app','dev';
           ASSERT removed=expected;
           DELETE FROM registry.environment_projects WHERE org='acme' AND project='app' AND env='dev';
           ASSERT NOT EXISTS(SELECT FROM registry.connection);
           ASSERT NOT EXISTS(SELECT FROM registry.connection_generation);
           ASSERT NOT EXISTS(SELECT FROM identity.credential);
           ASSERT (SELECT array_agg(status||':'||(disabled_at IS NOT NULL) ORDER BY subject) FROM identity.principals
                   WHERE subject IN ('first','second','third'))
                  =ARRAY['disabled:true','disabled:true','active:false'];
           ASSERT EXISTS(SELECT FROM registry.integration WHERE name='first');
           ASSERT EXISTS(SELECT FROM registry.external_system WHERE name='erp');
         END $test$;",
        wamn_registry::sql::delete_environment_connections_sql()
    )).await;
}

#[sqlx::test(migrations = false)]
async fn environment_row_delete_cascades_to_its_connections(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool), "DO $$ BEGIN
           DELETE FROM registry.environment_projects WHERE org='acme' AND project='app' AND env='dev';
           ASSERT NOT EXISTS(SELECT FROM registry.connection);
           ASSERT NOT EXISTS(SELECT FROM identity.credential);
         END $$;").await;
}

fn oauth_rows() -> &'static str {
    "INSERT INTO identity.oauth_token(connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,updated_at)
     SELECT id,'\\x01','\\x02','\\x03','file:test',now()+interval '1 hour',now() FROM registry.connection;
     INSERT INTO identity.oauth_authorization(state,connection_id,generation,code_verifier,wrapped_key,nonce,key_id,created_by,expires_at)
     SELECT id::text,id,active_generation,'\\x04','\\x03','\\x02','file:test',service_principal_id,now()+interval '10 minutes'
     FROM registry.connection;"
}

async fn oauth_rotation(
    database: url::Url,
    auth: &str,
    secret_changed: bool,
    auth_changed: bool,
    expected: &str,
    rollback: bool,
) {
    let parameters = if auth_changed {
        r#"{"auth":{"client_id":"new"}}"#
    } else {
        r#"{"tls-verification":"disabled"}"#
    };
    let stored = if secret_changed {
        r#", "stored_credential":{"id":"00000000-0000-0000-0000-000000000043","store":"k8s","ref":"next","version":"1"}"#
    } else {
        ""
    };
    run(database, &format!(
        "BEGIN;
         UPDATE registry.integration SET auth_type='{auth}' WHERE name='first';
         {}
         -- Keep the fixture outside the transaction that this test rolls back.
         COMMIT;
         BEGIN;
         DO $$ DECLARE result jsonb; BEGIN
           result := registry.configure_connection('acme','connection.finish',
             '{{\"project\":\"app\",\"env\":\"dev\",\"name\":\"first\",\"parameters\":{parameters}{stored}}}');
           ASSERT result->>'error' IS NULL,result::text;
           ASSERT (SELECT active_generation FROM registry.connection WHERE id='00000000-0000-0000-0000-000000000031')=3;
           {expected}
           ASSERT NOT EXISTS(SELECT FROM identity.oauth_authorization WHERE connection_id='00000000-0000-0000-0000-000000000031');
           ASSERT EXISTS(SELECT FROM identity.oauth_authorization WHERE connection_id='00000000-0000-0000-0000-000000000032');
           ASSERT EXISTS(SELECT FROM identity.oauth_token WHERE connection_id='00000000-0000-0000-0000-000000000032');
         END $$;
         {}",
        oauth_rows(),
        if rollback { "ROLLBACK; DO $$ BEGIN
           ASSERT (SELECT active_generation FROM registry.connection WHERE id='00000000-0000-0000-0000-000000000031')=2;
           ASSERT (SELECT count(*) FROM identity.oauth_token)=2;
           ASSERT (SELECT count(*) FROM identity.oauth_authorization)=2;
         END $$;" } else { "COMMIT;" },
    )).await;
}

const TOKEN_DELETED: &str = "ASSERT NOT EXISTS(SELECT FROM identity.oauth_token WHERE connection_id='00000000-0000-0000-0000-000000000031');";

#[tokio::test]
async fn oauth_client_credentials_secret_rotation_deletes_token() {
    let mut case_server = wamn_test_postgres::start(&[]).unwrap();
    for auth in ["oauth2_client_credentials", "oauth2_client_credentials_jwt"] {
        oauth_rotation(
            url::Url::parse(
                case_server
                    .create_database(&format!("case_{}", uuid::Uuid::new_v4().simple()))
                    .await
                    .unwrap()
                    .url(),
            )
            .unwrap(),
            auth,
            true,
            false,
            TOKEN_DELETED,
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn oauth_client_credentials_auth_rotation_deletes_token() {
    let mut case_server = wamn_test_postgres::start(&[]).unwrap();
    for auth in ["oauth2_client_credentials", "oauth2_client_credentials_jwt"] {
        oauth_rotation(
            url::Url::parse(
                case_server
                    .create_database(&format!("case_{}", uuid::Uuid::new_v4().simple()))
                    .await
                    .unwrap()
                    .url(),
            )
            .unwrap(),
            auth,
            false,
            true,
            TOKEN_DELETED,
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn oauth_authorization_code_auth_rotation_deletes_token() {
    let mut case_server = wamn_test_postgres::start(&[]).unwrap();
    for secret_changed in [false, true] {
        oauth_rotation(
            url::Url::parse(
                case_server
                    .create_database(&format!("case_{}", uuid::Uuid::new_v4().simple()))
                    .await
                    .unwrap()
                    .url(),
            )
            .unwrap(),
            "oauth2_authorization_code",
            secret_changed,
            true,
            TOKEN_DELETED,
            false,
        )
        .await;
    }
}

#[sqlx::test(migrations = false)]
async fn oauth_authorization_code_secret_rotation_expires_token(pool: sqlx::PgPool) {
    oauth_rotation(wamn_test_postgres::url(&pool), "oauth2_authorization_code", true, false,
        "ASSERT (SELECT expires_at='-infinity'::timestamptz AND ciphertext='\\x01'::bytea AND nonce='\\x02'::bytea AND wrapped_key='\\x03'::bytea AND key_id='file:test'
         FROM identity.oauth_token WHERE connection_id='00000000-0000-0000-0000-000000000031');", false).await;
}

#[tokio::test]
async fn oauth_unchanged_auth_preserves_token_and_discards_pending_authorization() {
    let mut case_server = wamn_test_postgres::start(&[]).unwrap();
    for auth in [
        "oauth2_client_credentials",
        "oauth2_client_credentials_jwt",
        "oauth2_authorization_code",
    ] {
        oauth_rotation(
            url::Url::parse(case_server.create_database(&format!("case_{}", uuid::Uuid::new_v4().simple())).await.unwrap().url()).unwrap(),
            auth,
            false,
            false,
            "ASSERT (SELECT expires_at>now() FROM identity.oauth_token WHERE connection_id='00000000-0000-0000-0000-000000000031');",
            false,
        ).await;
    }
}

#[sqlx::test(migrations = false)]
async fn oauth_rotation_rollback_restores_tokens_and_pending_authorization(pool: sqlx::PgPool) {
    oauth_rotation(
        wamn_test_postgres::url(&pool),
        "oauth2_client_credentials",
        true,
        false,
        TOKEN_DELETED,
        true,
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn oauth_rows_cascade_with_connection(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool), &format!("{}
        DELETE FROM registry.connection WHERE id='00000000-0000-0000-0000-000000000031';
        DO $$ BEGIN
          ASSERT NOT EXISTS(SELECT FROM identity.oauth_token WHERE connection_id='00000000-0000-0000-0000-000000000031');
          ASSERT NOT EXISTS(SELECT FROM identity.oauth_authorization WHERE connection_id='00000000-0000-0000-0000-000000000031');
          ASSERT (SELECT count(*) FROM identity.oauth_token)=1;
          ASSERT (SELECT count(*) FROM identity.oauth_authorization)=1;
        END $$;", oauth_rows())).await;
}

#[sqlx::test(migrations = false)]
async fn oauth_tables_refuse_application_and_administration(pool: sqlx::PgPool) {
    run(wamn_test_postgres::url(&pool), &format!(
        "RESET ROLE;
         DO $$ DECLARE role_name text; BEGIN
           FOREACH role_name IN ARRAY ARRAY['wamn_app','wamn_administration'] LOOP
             IF to_regrole(role_name) IS NULL THEN EXECUTE format('CREATE ROLE %I NOLOGIN',role_name); END IF;
           END LOOP;
         END $$;
         {} {}
         DO $$ DECLARE role_name text; relation text; statement text; BEGIN
           FOREACH role_name IN ARRAY ARRAY['wamn_app','wamn_administration'] LOOP
             FOREACH relation IN ARRAY ARRAY['oauth_token','oauth_authorization'] LOOP
               FOREACH statement IN ARRAY ARRAY['SELECT * FROM identity.','DELETE FROM identity.','UPDATE identity.'] LOOP
                 EXECUTE format('SET LOCAL ROLE %I',role_name);
                 BEGIN
                   EXECUTE statement || quote_ident(relation) || CASE WHEN statement='UPDATE identity.' THEN ' SET key_id=key_id' ELSE '' END;
                   RAISE EXCEPTION 'OAuth table access must refuse';
                 EXCEPTION WHEN insufficient_privilege THEN NULL; END;
                 RESET ROLE;
               END LOOP;
               ASSERT NOT has_table_privilege(role_name,'identity.'||relation,'INSERT,SELECT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER');
               ASSERT NOT has_any_column_privilege(role_name,'identity.'||relation,'SELECT,INSERT,UPDATE,REFERENCES');
             END LOOP;
           END LOOP;
           FOREACH role_name IN ARRAY ARRAY['wamn_control','wamn_conn_reader'] LOOP
             FOREACH relation IN ARRAY ARRAY['oauth_token','oauth_authorization'] LOOP
               ASSERT has_schema_privilege(role_name,'identity','USAGE');
               ASSERT has_table_privilege(role_name,'identity.'||relation,'SELECT') AND has_table_privilege(role_name,'identity.'||relation,'INSERT')
                  AND has_table_privilege(role_name,'identity.'||relation,'UPDATE') AND has_table_privilege(role_name,'identity.'||relation,'DELETE');
             END LOOP;
           END LOOP;
         END $$;",
        wamn_control_provision::sql::grant_control_surface_sql(),
        wamn_control_provision::sql::grant_connection_reader_surface_sql(),
    )).await;
}
