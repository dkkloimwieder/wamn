//! PostgreSQL test for the surviving run-queue authority matrix.

use std::io::Write;
use std::process::{Command, Output, Stdio};

use wamn_control_provision::{WorkloadRoleFamily, sql};
use wamn_run_state::authority_class::CURRENT_USER_ROLE_MEMBERSHIP_SQL;
use wamn_run_state::queue::{
    park_queued_run_sql, release_parked_run_sql, select_production_claim_sql,
};

const EXECUTOR_LOGIN: &str = "wamn_matrix_executor_login";
const MANAGEMENT_LOGIN: &str = "wamn_matrix_management_login";

fn psql(url: &str, script: &str) -> Output {
    let actor = wamn_control_provision::PlatformComponent::Provisioning.principal_id();
    let script =
        format!("SET app.user_id='{actor}'; SET app.operation='admin:run-state-fixture'; {script}");
    let mut child = Command::new("psql")
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-Atq", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run psql");
    if let Err(error) = child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
    {
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe,
            "write psql script: {error}"
        );
    }
    child.wait_with_output().expect("wait for psql")
}

fn success(url: &str, script: &str) -> String {
    let output = psql(url, script);
    assert!(
        output.status.success(),
        "psql failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("psql stdout is UTF-8")
}

fn assert_sqlstate(url: &str, script: &str, state: &str, message: &str) {
    let output = psql(url, &format!("\\set VERBOSITY verbose\n{script}"));
    assert!(!output.status.success(), "statement was admitted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(state), "SQLSTATE drifted:\n{stderr}");
    assert!(stderr.contains(message), "refusal drifted:\n{stderr}");
}

#[test]
fn surviving_authority_matrix_live() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog = wamn_catalog::CATALOG_SCHEMA_SQL;
    let app_schema = wamn_control_provision::APP_SCHEMA_SQL;
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .expect("read run-state DDL");
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .expect("read run-queue DDL");
    let database = success(&url, "SELECT current_database();")
        .trim()
        .to_string();
    let access_floor = format!(
        "REVOKE CONNECT, TEMPORARY ON DATABASE \"{database}\" FROM PUBLIC; \
         REVOKE CONNECT ON DATABASE \"{database}\" FROM wamn_app;"
    );
    // The production role builders retain the platform RLS membership.
    let executor_provision = sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::ExecutorPlatform,
        &database,
        EXECUTOR_LOGIN,
        "executor-test-password",
        "2099-01-01T00:00:00Z",
    );
    let management_provision = sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::ManagementAdmitter,
        &database,
        MANAGEMENT_LOGIN,
        "management-test-password",
        "2099-01-01T00:00:00Z",
    );
    let management_surface = sql::grant_management_admitter_surface_sql("wamn_run");

    success(
        &url,
        &format!(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               FOREACH role_name IN ARRAY ARRAY[ \
                 'wamn_app','wamn_control_author','wamn_scenario_author', \
                 'wamn_executor_platform','wamn_management_admitter', \
                 '{EXECUTOR_LOGIN}','{MANAGEMENT_LOGIN}' \
               ] LOOP \
                 IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname=role_name) THEN \
                   EXECUTE format('DROP OWNED BY %I', role_name); \
                   EXECUTE format('DROP ROLE %I', role_name); \
                 END IF; \
               END LOOP; \
             END $$; \
             CREATE ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_control_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             BEGIN; {catalog} {run_state} {run_queue} {app_schema} COMMIT; \
             {access_floor} \
             {executor_provision} \
             {management_provision}"
        ),
    );
    // Publication still needs tenant-key execution for its catalog index.
    success(
        &url,
        &format!(
            "GRANT UPDATE (durability_class) ON wamn_run.environment_policies \
               TO wamn_management_admitter; \
             {management_surface} \
             DO $$ BEGIN \
               ASSERT NOT pg_catalog.has_column_privilege( \
                 'wamn_management_admitter', 'wamn_run.environment_policies', \
                 'durability_class', 'UPDATE'); \
               ASSERT pg_catalog.has_function_privilege( \
                 'wamn_management_admitter', \
                 'wamn_authority.tenant_key(text)', 'EXECUTE'); \
               ASSERT NOT pg_catalog.has_schema_privilege( \
                 'wamn_management_admitter', 'wamn_authority', 'USAGE'); \
               ASSERT NOT pg_catalog.has_function_privilege( \
                 'wamn_management_admitter', \
                 'wamn_authority.current_tenant_key()', 'EXECUTE'); \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_authid \
                  WHERE rolname = 'wamn_management_admitter' \
                    AND NOT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                    AND NOT rolcreaterole AND NOT rolinherit AND NOT rolreplication \
                    AND NOT rolbypassrls AND rolpassword IS NULL); \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_authid \
                  WHERE rolname = '{MANAGEMENT_LOGIN}' \
                    AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                    AND NOT rolcreaterole AND rolinherit AND NOT rolreplication \
                    AND NOT rolbypassrls AND rolpassword IS NOT NULL \
                    AND rolvaliduntil IS NOT NULL); \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_auth_members AS membership \
                 JOIN pg_catalog.pg_roles AS parent ON parent.oid = membership.roleid \
                 JOIN pg_catalog.pg_roles AS child ON child.oid = membership.member \
                  WHERE parent.rolname = 'wamn_management_admitter' \
                    AND child.rolname = '{MANAGEMENT_LOGIN}' \
                    AND NOT membership.admin_option \
                    AND membership.inherit_option \
                    AND NOT membership.set_option); \
               ASSERT pg_catalog.has_database_privilege( \
                 '{MANAGEMENT_LOGIN}', current_database(), 'CONNECT'); \
               ASSERT NOT pg_catalog.has_database_privilege( \
                 '{MANAGEMENT_LOGIN}', current_database(), 'TEMPORARY'); \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_authid \
                  WHERE rolname = 'wamn_executor_platform' \
                    AND NOT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                    AND NOT rolcreaterole AND NOT rolinherit AND NOT rolreplication \
                    AND NOT rolbypassrls AND rolpassword IS NULL); \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_authid \
                  WHERE rolname = '{EXECUTOR_LOGIN}' \
                    AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                    AND NOT rolcreaterole AND rolinherit AND NOT rolreplication \
                    AND NOT rolbypassrls AND rolpassword IS NOT NULL \
                    AND rolvaliduntil IS NOT NULL); \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_auth_members AS membership \
                 JOIN pg_catalog.pg_roles AS parent ON parent.oid = membership.roleid \
                 JOIN pg_catalog.pg_roles AS child ON child.oid = membership.member \
                  WHERE parent.rolname = 'wamn_executor_platform' \
                    AND child.rolname = '{EXECUTOR_LOGIN}' \
                    AND NOT membership.admin_option \
                    AND membership.inherit_option \
                    AND NOT membership.set_option); \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_auth_members AS membership \
                 JOIN pg_catalog.pg_roles AS parent ON parent.oid = membership.roleid \
                 JOIN pg_catalog.pg_roles AS child ON child.oid = membership.member \
                  WHERE parent.rolname = 'wamn_platform' \
                    AND child.rolname = 'wamn_executor_platform' \
                    AND NOT membership.admin_option \
                    AND membership.inherit_option \
                    AND NOT membership.set_option); \
               ASSERT pg_catalog.pg_has_role( \
                 '{EXECUTOR_LOGIN}', 'wamn_platform', 'USAGE'); \
               ASSERT NOT pg_catalog.pg_has_role( \
                 '{EXECUTOR_LOGIN}', 'wamn_app', 'USAGE'); \
               ASSERT NOT pg_catalog.pg_has_role( \
                 '{MANAGEMENT_LOGIN}', 'wamn_app', 'USAGE'); \
             END $$;"
        ),
    );

    // Reapplying the executor grants must remove table and column over-grants.
    let executor_surface = sql::grant_executor_platform_surface_sql("wamn_run");
    success(
        &url,
        &format!(
            "GRANT INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA catalog \
               TO wamn_executor_platform; \
             GRANT UPDATE (input_json) ON wamn_run.runs TO wamn_executor_platform; \
             GRANT UPDATE (available_at) ON wamn_run.run_queue TO wamn_executor_platform; \
             {executor_surface} \
             GRANT USAGE ON SCHEMA wamn_run TO wamn_control_author; \
             INSERT INTO catalog.packages \
               (env_key,package_id,package_version,artifact_digest) \
             VALUES ('t1','cat','1.0.0','sha256:{manifest_hash}'); \
             INSERT INTO catalog.releases (env_key,manifest_digest,canonical_bytes) \
             VALUES ('t1','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','{{}}'); \
             INSERT INTO wamn_run.environment_policies \
               (env_key,expected_environment,durability_class) \
             VALUES ('t1','dev','standard'); \
             INSERT INTO wamn_run.runs \
               (env_key,run_id,flow_id,flow_version,package_id,manifest_digest,environment, \
                wiring_id,wiring_version,status,trigger_source,input_json,service_principal_id) \
             VALUES ('t1','run-1','legacy-flow',1,'cat','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','dev','legacy-wiring',1, \
                     'dispatched','automation','{{}}','00000000-0000-0000-0000-000000000001'); \
             INSERT INTO wamn_run.run_queue (env_key,run_id) VALUES ('t1','run-1');",
            manifest_hash = "a".repeat(64),
        ),
    );

    // THE EXECUTOR-PLATFORM DENIAL MATRIX, STATED AS TOTALS (`wamn-0h0g.22.31`).
    //
    // Each leg aggregates every privilege the family actually holds across a
    // whole schema and compares it to a LITERAL. A total is what makes one
    // assertion refuse in BOTH directions: a grant the builder stops emitting
    // drops out of the aggregate, and a grant it starts emitting appears in it.
    // A per-item `has_*_privilege` list could only ever catch one direction.
    //
    // THE LITERALS ARE WRITTEN OUT RATHER THAN DERIVED FROM
    // `EXECUTOR_PLATFORM_*` ON PURPOSE. Deriving them would make the constants
    // agree with themselves: a column added to the constant would widen the
    // grant and the expectation in the same edit, and no leg here could tell.
    // Written out, the constant and the claim about it are two documents, and
    // the surface cannot be widened without moving a named assertion.
    success(
        &url,
        "DO $$ DECLARE actual text; BEGIN \
           SELECT string_agg(c.relname || ':' || p, ',' ORDER BY c.relname || ':' || p) \
             INTO actual \
             FROM pg_catalog.pg_class AS c \
             JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
             CROSS JOIN unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE', \
                                     'TRUNCATE','REFERENCES','TRIGGER']) AS p \
            WHERE n.nspname = 'wamn_run' AND c.relkind IN ('r','p','v','m') \
              AND pg_catalog.has_table_privilege('wamn_executor_platform', c.oid, p); \
           ASSERT actual = 'effect_attempts:SELECT,environment_policies:SELECT,\
run_queue:DELETE,run_queue:SELECT,runs:SELECT', \
                  'run-plane TABLE grain drifted: ' || coalesce(actual, '<none>'); \
           SELECT string_agg(c.relname || ':' || p, ',' ORDER BY c.relname || ':' || p) \
             INTO actual \
             FROM pg_catalog.pg_class AS c \
             JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
             CROSS JOIN unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE', \
                                     'TRUNCATE','REFERENCES','TRIGGER']) AS p \
            WHERE n.nspname = 'catalog' AND c.relkind IN ('r','p','v','m') \
              AND pg_catalog.has_table_privilege('wamn_executor_platform', c.oid, p); \
           ASSERT actual = \
             'connection_bindings:SELECT,\
connection_generations:SELECT,connection_instances:SELECT,connection_requirements:SELECT,\
releases:SELECT,workflows:SELECT', \
                  'catalog TABLE grain drifted: ' || coalesce(actual, '<none>'); \
           SELECT string_agg(a.attname, ',' ORDER BY a.attname) INTO actual \
             FROM pg_catalog.pg_attribute AS a \
            WHERE a.attrelid = 'wamn_run.runs'::regclass AND a.attnum > 0 \
              AND NOT a.attisdropped \
              AND pg_catalog.has_column_privilege( \
                    'wamn_executor_platform', a.attrelid, a.attnum, 'UPDATE'); \
           ASSERT actual = \
             'caller_http_status,caller_outcome_hash,caller_outcome_json,caller_outcome_type,\
caller_release_node_id,caller_released_at,deadline_adjustments_json,fail_type,\
result_json,state_json,status,terminal_reason,updated_at', \
                  'runs UPDATE columns drifted: ' || coalesce(actual, '<none>'); \
           SELECT string_agg(a.attname, ',' ORDER BY a.attname) INTO actual \
             FROM pg_catalog.pg_attribute AS a \
            WHERE a.attrelid = 'wamn_run.run_queue'::regclass AND a.attnum > 0 \
              AND NOT a.attisdropped \
              AND pg_catalog.has_column_privilege( \
                    'wamn_executor_platform', a.attrelid, a.attnum, 'UPDATE'); \
           ASSERT actual = 'attempts,lease_expires_at,lease_generation,lease_owner', \
                  'run_queue UPDATE columns drifted: ' || coalesce(actual, '<none>'); \
           SELECT string_agg(a.attname, ',' ORDER BY a.attname) INTO actual \
             FROM pg_catalog.pg_attribute AS a \
            WHERE a.attrelid = 'wamn_run.runs'::regclass AND a.attnum > 0 \
              AND NOT a.attisdropped \
              AND pg_catalog.has_column_privilege( \
                    'wamn_executor_platform', a.attrelid, a.attnum, 'INSERT'); \
           ASSERT actual = \
             'durability_class,env_key,environment,event_depth,event_root_run_id,event_source_run_id,execution_principal_id,execution_role,\
idempotency_key,input_json,invoked_by,invoked_by_user_id,manifest_digest,node_digests_json,\
package_id,registration_id,service_principal_id,status,trigger_source,wiring_hash,wiring_id,wiring_version', \
                  'runs INSERT columns (the event and manual run grains) drifted: ' || coalesce(actual, '<none>'); \
           SELECT string_agg(a.attname, ',' ORDER BY a.attname) INTO actual \
             FROM pg_catalog.pg_attribute AS a \
            WHERE a.attrelid = 'wamn_run.run_queue'::regclass AND a.attnum > 0 \
              AND NOT a.attisdropped \
              AND pg_catalog.has_column_privilege( \
                    'wamn_executor_platform', a.attrelid, a.attnum, 'INSERT'); \
           ASSERT actual = 'env_key,run_id', \
                  'run_queue INSERT columns drifted: ' || coalesce(actual, '<none>'); \
           ASSERT pg_catalog.has_schema_privilege( \
                    'wamn_executor_platform', 'wamn_run', 'USAGE'); \
           ASSERT pg_catalog.has_schema_privilege( \
                    'wamn_executor_platform', 'catalog', 'USAGE'); \
           ASSERT NOT pg_catalog.has_schema_privilege( \
                    'wamn_executor_platform', 'wamn_run', 'CREATE'); \
           ASSERT NOT pg_catalog.has_schema_privilege( \
                    'wamn_executor_platform', 'catalog', 'CREATE'); \
           ASSERT pg_catalog.has_function_privilege('wamn_executor_platform', \
                    'wamn_authority.tenant_key(text)', 'EXECUTE'); \
           ASSERT NOT pg_catalog.has_schema_privilege( \
                    'wamn_executor_platform', 'wamn_authority', 'USAGE'); \
           ASSERT NOT pg_catalog.has_function_privilege('wamn_executor_platform', \
                    'wamn_authority.current_tenant_key()', 'EXECUTE'); \
         END $$;",
    );

    let claim = select_production_claim_sql();
    let claimed = success(
        &url,
        &format!(
            "BEGIN; SET LOCAL ROLE {EXECUTOR_LOGIN}; SET LOCAL app.env_key='t1'; \
             SET LOCAL search_path=wamn_run,catalog,public; \
             SELECT current_user; PREPARE matrix_claim(text[],text,text) AS {claim}; \
             EXECUTE matrix_claim(ARRAY['cat'],'dev','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'); ROLLBACK;"
        ),
    );
    assert!(claimed.contains(EXECUTOR_LOGIN));
    assert!(claimed.contains("run-1"));

    // The complete-grain CHECK makes a half candidate row unrepresentable, and
    // the trigger names every component-era pin.
    assert_sqlstate(
        &url,
        "INSERT INTO wamn_run.runs \
           (env_key,run_id,package_id,environment, \
            wiring_id,wiring_version,status,trigger_source,input_json) \
         VALUES ('t1','half-run','cat','dev','candidate',1, \
                 'dispatched','test-case','{}');",
        "23514",
        "runs_execution_grain_check",
    );
    assert_sqlstate(
        &url,
        "UPDATE wamn_run.runs SET binding_world_json='[]' \
          WHERE env_key='t1' AND run_id='run-1';",
        "55000",
        "run-admission-pin-immutable",
    );

    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE']) p \
             WHERE pg_catalog.has_table_privilege('wamn_app','wamn_run.run_queue',p)); \
         END $$;",
    );
    // Broad table grants do not confer executor membership.
    success(
        &url,
        &format!(
            "GRANT USAGE ON SCHEMA wamn_run, catalog TO {MANAGEMENT_LOGIN}; \
             GRANT SELECT, INSERT, UPDATE, DELETE \
               ON ALL TABLES IN SCHEMA catalog, wamn_run TO {MANAGEMENT_LOGIN};"
        ),
    );
    for denied_role in [
        MANAGEMENT_LOGIN,
        "wamn_app",
        "wamn_control_author",
        "wamn_scenario_author",
    ] {
        assert_eq!(
            success(
                &url,
                &format!(
                    "BEGIN; SET LOCAL ROLE {denied_role}; \
                     PREPARE authority_membership(text) AS {CURRENT_USER_ROLE_MEMBERSHIP_SQL}; \
                     EXECUTE authority_membership('wamn_executor_platform'); ROLLBACK;"
                ),
            ),
            "f\n",
            "{denied_role} must not hold executor membership"
        );
    }
}

/// R20 (1): the candidate select filters by the claiming host's release. The
/// FIFO head is pinned to release 2, and a host of release 1 claims the
/// release-1 run queued behind it instead of selecting the head. A host with
/// no release selects no pinned run.
#[test]
fn release_scoped_queue_claim_skips_another_releases_head_live() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog = wamn_catalog::CATALOG_SCHEMA_SQL;
    let app_schema = wamn_control_provision::APP_SCHEMA_SQL;
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .expect("read run-state DDL");
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .expect("read run-queue DDL");
    success(
        &url,
        &format!(
            "DO $$ DECLARE role_name text; BEGIN \
               FOREACH role_name IN ARRAY ARRAY[ \
                 'wamn_app','wamn_control_author','wamn_scenario_author' \
               ] LOOP \
                 IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname=role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN', role_name); \
                 END IF; \
               END LOOP; \
             END $$; \
             BEGIN; {catalog} {run_state} {run_queue} {app_schema} COMMIT; \
             INSERT INTO catalog.packages \
               (env_key,package_id,package_version,artifact_digest) \
             VALUES ('t1','cat','1.0.0','sha256:{manifest_hash}'); \
             INSERT INTO catalog.releases (env_key,manifest_digest,canonical_bytes) \
             VALUES ('t1','sha256:21621f6bd9bc2769154aa2938726e891ecbc7929cd7be941b60ec776703ed394','{{\"release\":1}}'), \
                    ('t1','sha256:89437c2ddcb5d585633413604661041fe6b5ee9d49ae96153fe24812f1a8c5c0','{{\"release\":2}}'); \
             INSERT INTO wamn_run.environment_policies \
               (env_key,expected_environment,durability_class) \
             VALUES ('t1','dev','standard'); \
             INSERT INTO wamn_run.runs \
               (env_key,run_id,flow_id,flow_version,package_id,manifest_digest,environment, \
                wiring_id,wiring_version,status,trigger_source,input_json,service_principal_id) \
             VALUES ('t1','run-b','legacy-flow',1,'cat',{release_2},'dev','legacy-wiring',1, \
                     'dispatched','automation','{{}}','00000000-0000-0000-0000-000000000001'), \
                    ('t1','run-a','legacy-flow',1,'cat',{release_1},'dev','legacy-wiring',1, \
                     'dispatched','automation','{{}}','00000000-0000-0000-0000-000000000001'); \
             INSERT INTO wamn_run.run_queue (env_key,run_id,available_at) \
             VALUES ('t1','run-b',now() - interval '2 minutes'), \
                    ('t1','run-a',now() - interval '1 minute');",
            manifest_hash = "a".repeat(64),
            release_1 = RELEASE_1,
            release_2 = RELEASE_2,
        ),
    );
    let claim = select_production_claim_sql();
    let head_for = |release: &str| {
        let selected = success(
            &url,
            &format!(
                "BEGIN; SET LOCAL app.env_key='t1'; \
                 SET LOCAL search_path=wamn_run,catalog,public; \
                 PREPARE release_claim(text[],text,text) AS {claim}; \
                 EXECUTE release_claim(ARRAY['cat'],'dev',{release}); ROLLBACK;"
            ),
        );
        selected
            .split('|')
            .next()
            .expect("psql prints the selected run first")
            .to_owned()
    };
    assert_eq!(
        head_for(RELEASE_1),
        "run-a",
        "release 1 claims behind the release-2 head"
    );
    assert_eq!(
        head_for(RELEASE_2),
        "run-b",
        "release 2 claims its own head"
    );
    assert_eq!(
        head_for("NULL"),
        "",
        "a host with no release claims no pinned run"
    );
    assert_eq!(
        head_for("'sha256:3333333333333333333333333333333333333333333333333333333333333333'"),
        "",
        "a release with no pinned run claims nothing"
    );

    // The selector supplies trusted event ancestry without rewriting author input.
    success(
        &url,
        &format!(
            "INSERT INTO wamn_run.runs \
               (env_key,run_id,flow_id,flow_version,package_id,manifest_digest,environment, \
                wiring_id,wiring_version,status,trigger_source,input_json, \
                event_source_run_id,event_root_run_id,event_depth, \
                execution_principal_id,execution_role,node_digests_json) \
             VALUES ('t1','projection','f',1,'cat',{RELEASE_1},'dev','w',1,'dispatched','event', \
               '{{\"value\":7,\"causation\":{{\"root\":\"forged\"}}}}', \
               'source','trusted-root',2,'00000000-0000-0000-0000-000000000002', \
               'workflow','{{\"node\":\"sha256:frozen\"}}'); \
             INSERT INTO wamn_run.run_queue(env_key,run_id,available_at) \
             VALUES ('t1','projection','2000-01-01'); \
             BEGIN; SET LOCAL app.env_key='t1'; SET LOCAL search_path=wamn_run; \
             DO $test$ DECLARE selected record; untouched jsonb; parked text; BEGIN \
               SELECT to_jsonb(q) INTO untouched FROM run_queue AS q WHERE run_id='projection'; \
               EXECUTE $claim${claim}$claim$ INTO STRICT selected USING ARRAY['cat'],'dev',{RELEASE_1}; \
               ASSERT selected.run_id='projection', 'the earliest eligible row wins'; \
               ASSERT selected.input_json::jsonb = \
                 '{{\"value\":7,\"causation\":{{\"run\":\"projection\",\"root\":\"trusted-root\",\"depth\":2}}}}'::jsonb, \
                 'trusted ancestry replaces author causation for execution'; \
               ASSERT (SELECT input_json FROM runs WHERE run_id='projection') = \
                 '{{\"value\":7,\"causation\":{{\"root\":\"forged\"}}}}'::jsonb, \
                 'execution input is transient'; \
               ASSERT selected.execution_principal_id='00000000-0000-0000-0000-000000000002' \
                 AND selected.execution_role='workflow' \
                 AND selected.node_digests_json::jsonb='{{\"node\":\"sha256:frozen\"}}'::jsonb, \
                 'the selector carries frozen execution identity'; \
               ASSERT NOT selected.router_caller_attached AND NOT selected.durable_caller_attached, \
                 'an event has no waiting caller'; \
               ASSERT (SELECT to_jsonb(q) FROM run_queue AS q WHERE run_id='projection')=untouched, \
                 'selection grants no lease'; \
               ASSERT (SELECT lease_generation FROM run_queue WHERE run_id='projection')=0, \
                 'an unclaimed queue row starts at generation zero'; \
               EXECUTE $park${park}$park$ INTO STRICT parked USING 'projection','dev'; \
               ASSERT parked='projection', 'the queue row parks'; \
               ASSERT (SELECT status FROM runs WHERE run_id='projection')='dispatched', \
                 'parking preserves run status'; \
               EXECUTE $claim${claim}$claim$ INTO STRICT selected USING ARRAY['cat'],'dev',{RELEASE_1}; \
               ASSERT selected.run_id='run-a', 'the selector skips a parked head'; \
               ASSERT selected.input_json::jsonb='{{}}'::jsonb, 'non-event input stays unchanged'; \
               ASSERT selected.service_principal_id='00000000-0000-0000-0000-000000000001', \
                 'the selector carries the automation principal'; \
               EXECUTE $park${park}$park$ INTO STRICT parked USING 'run-a','dev'; \
               EXECUTE $wake${wake}$wake$ INTO STRICT parked USING 'projection','dev'; \
               EXECUTE $claim${claim}$claim$ INTO STRICT selected USING ARRAY['cat'],'dev',{RELEASE_1}; \
               ASSERT selected.run_id='projection', 'a released park is eligible again'; \
             END $test$; ROLLBACK;",
            park = park_queued_run_sql(),
            wake = release_parked_run_sql(),
        ),
    );
    success(
        &url,
        &format!(
            "BEGIN; SET LOCAL app.env_key='t1'; SET LOCAL search_path=wamn_run; \
             UPDATE run_queue SET available_at='infinity'; \
             DO $test$ DECLARE source text; component bool; router bool; durable bool; \
               selected record; identity text; BEGIN \
               FOR source, component, router, durable IN VALUES \
                 ('http',false,true,true),('internal',false,true,true),('studio',false,true,true), \
                 ('scenario-draft',true,true,false),('test-case',true,true,false), \
                 ('test-case',false,false,false) LOOP \
                 identity := source || '-' || component::text; \
                 INSERT INTO runs (env_key,run_id,flow_id,flow_version,package_id,manifest_digest, \
                   environment,wiring_id,wiring_version,wiring_hash,binding_world_json, \
                   status,trigger_source) \
                 VALUES ('t1',identity,CASE WHEN component THEN NULL ELSE 'f' END, \
                   CASE WHEN component THEN NULL ELSE 1 END,'cat',{RELEASE_1},'dev','w',1, \
                   CASE WHEN component THEN 'sha256:' || repeat('d',64) END, \
                   CASE WHEN component THEN '[]'::jsonb END,'dispatched',source); \
                 INSERT INTO run_queue(env_key,run_id) VALUES ('t1',identity); \
                 EXECUTE $claim${claim}$claim$ INTO STRICT selected USING ARRAY['cat'],'dev',{RELEASE_1}; \
                 ASSERT selected.run_id=identity, 'one eligible row is selected'; \
                 ASSERT selected.router_caller_attached=router \
                    AND selected.durable_caller_attached=durable, \
                   format('caller classification differs for %s',identity); \
                 DELETE FROM run_queue WHERE run_id=identity; \
               END LOOP; \
             END $test$; ROLLBACK;"
        ),
    );
}

/// The two release digests of the release-scoped claim, as SQL literals.
const RELEASE_1: &str = "'sha256:21621f6bd9bc2769154aa2938726e891ecbc7929cd7be941b60ec776703ed394'";
const RELEASE_2: &str = "'sha256:89437c2ddcb5d585633413604661041fe6b5ee9d49ae96153fe24812f1a8c5c0'";
