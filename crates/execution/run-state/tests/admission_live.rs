//! Ignored PostgreSQL test for the surviving run-queue authority matrix.

use std::io::Write;
use std::process::{Command, Output, Stdio};

use wamn_control_provision::{WorkloadRoleFamily, sql};
use wamn_run_state::authority_class::CURRENT_USER_ROLE_MEMBERSHIP_SQL;
use wamn_run_state::queue::select_production_claim_sql;

const EXECUTOR_LOGIN: &str = "wamn_matrix_executor_login";
const MANAGEMENT_LOGIN: &str = "wamn_matrix_management_login";

fn psql(url: &str, script: &str) -> Output {
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
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn surviving_authority_matrix_live() {
    let url = std::env::var("WAMN_RUN_STORE_PG_URL")
        .expect("set WAMN_RUN_STORE_PG_URL to the throwaway superuser database");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog = wamn_catalog::CATALOG_SCHEMA_SQL;
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .expect("read run-state DDL");
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .expect("read run-queue DDL");
    let database = success(&url, "SELECT current_database();")
        .trim()
        .to_string();
    let access_floor = sql::grant_connect_on_database_sql(&database);
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
                 'wamn_app','wamn_control_author','wamn_scenario_author','wamn_effect_writer', \
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
             CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             BEGIN; {catalog} {run_state} {run_queue} COMMIT; \
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
               (tenant_id,package_id,package_version,manifest_sha256) \
             VALUES ('t1','cat','1.0.0','sha256:{manifest_hash}'); \
             INSERT INTO catalog.effective_releases \
               (tenant_id,effective_release_id,environment,verified_publisher_principal) \
             VALUES ('t1',1,'dev','test-publisher'); \
             INSERT INTO catalog.effective_release_packages \
               (tenant_id,effective_release_id,package_id,package_version) \
             VALUES ('t1',1,'cat','1.0.0'); \
             INSERT INTO wamn_run.environment_policies \
               (tenant_id,expected_environment,durability_class) \
             VALUES ('t1','dev','standard'); \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
                wiring_id,wiring_version,status,trigger_source,input_json) \
             VALUES ('t1','run-1','legacy-flow',1,'cat',1,'dev','legacy-wiring',1, \
                     'dispatched','automation','{{}}'); \
             INSERT INTO wamn_run.run_queue (tenant_id,run_id) VALUES ('t1','run-1');",
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
           ASSERT actual = 'effect_attempts:SELECT,run_queue:DELETE,run_queue:SELECT,runs:SELECT', \
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
             'component_library:SELECT,connection_bindings:SELECT,\
connection_generations:SELECT,connection_instances:SELECT,connection_requirements:SELECT,\
effective_release_packages:SELECT,release_components:SELECT,\
release_manifest_v3_snapshots:SELECT,wirings:SELECT', \
                  'catalog TABLE grain drifted: ' || coalesce(actual, '<none>'); \
           SELECT string_agg(a.attname, ',' ORDER BY a.attname) INTO actual \
             FROM pg_catalog.pg_attribute AS a \
            WHERE a.attrelid = 'wamn_run.runs'::regclass AND a.attnum > 0 \
              AND NOT a.attisdropped \
              AND pg_catalog.has_column_privilege( \
                    'wamn_executor_platform', a.attrelid, a.attnum, 'UPDATE'); \
           ASSERT actual = \
             'caller_http_status,caller_outcome_hash,caller_outcome_json,caller_outcome_kind,\
caller_release_node_id,caller_released_at,fail_kind,manifest_digest,\
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
            "BEGIN; SET LOCAL ROLE {EXECUTOR_LOGIN}; SET LOCAL app.tenant='t1'; \
             SET LOCAL search_path=wamn_run,catalog,public; \
             SELECT current_user; PREPARE matrix_claim(text[],text) AS {claim}; \
             EXECUTE matrix_claim(ARRAY['cat'],'dev'); ROLLBACK;"
        ),
    );
    assert!(claimed.contains(EXECUTOR_LOGIN));
    assert!(claimed.contains("run-1"));

    // The complete-grain CHECK makes a half candidate row unrepresentable, and
    // the trigger names every component-era pin.
    assert_sqlstate(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,package_id,effective_release_id,environment, \
            wiring_id,wiring_version,status,trigger_source,input_json) \
         VALUES ('t1','half-run','cat',1,'dev','candidate',1, \
                 'dispatched','test-case','{}');",
        "23514",
        "runs_execution_grain_check",
    );
    assert_sqlstate(
        &url,
        "UPDATE wamn_run.runs SET binding_world_json='[]' \
          WHERE tenant_id='t1' AND run_id='run-1';",
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
        "wamn_effect_writer",
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
