//! Run-queue authority through real database operations.
use std::io::Write;
use std::process::{Command, Output, Stdio};
use wamn_control_provision::{WorkloadRoleFamily, sql};
use wamn_run_state::queue::select_production_lease_sql;
const EXECUTOR_LOGIN: &str = "wamn_matrix_executor_login";

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
    let base_grants = format!(
        "REVOKE CONNECT, TEMPORARY ON DATABASE \"{database}\" FROM PUBLIC; \
         REVOKE CONNECT ON DATABASE \"{database}\" FROM wamn_app;"
    );
    // The production role builders retain the platform RLS membership.
    let executor_provision = wamn_control_provision::test_database::login_sql(
        WorkloadRoleFamily::ExecutorPlatform,
        &database,
        EXECUTOR_LOGIN,
        "executor-test-password",
    );
    success(
        &url,
        &format!(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               FOREACH role_name IN ARRAY ARRAY[ \
                 'wamn_app', \
                 'wamn_executor_platform', \
                 '{EXECUTOR_LOGIN}' \
               ] LOOP \
                 IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname=role_name) THEN \
                   EXECUTE format('DROP OWNED BY %I', role_name); \
                   EXECUTE format('DROP ROLE %I', role_name); \
                 END IF; \
               END LOOP; \
             END $$; \
             CREATE ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
              \
              \
             BEGIN; {catalog} {run_state} {run_queue} {app_schema} COMMIT; \
             {base_grants} \
             {executor_provision}"
        ),
    );
    // Publication still needs project-key execution for its catalog index.
    success(
        &url,
        &format!(
            "DO $$ BEGIN \
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
              \
             INSERT INTO catalog.packages ( project_env_key,package_id,package_version,artifact_digest) VALUES ( 't1','cat','1.0.0','sha256:{manifest_hash}'); \
             INSERT INTO catalog.releases (project_env_key,manifest_digest,canonical_bytes) \
             VALUES ('t1','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','{{}}'); \
             INSERT INTO wamn_run.runs \
               (project_env_key,run_id,wiring_hash,package_id,manifest_digest,environment, \
                wiring_id,wiring_version,status,trigger_source,input_json,execution_principal_id,execution_role,invoked_by,invoked_by_user_id,node_digests_json) \
             VALUES ('t1','run-1','sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','cat','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','dev','legacy-wiring',1, \
                     'dispatched','manual','{{}}','00000000-0000-0000-0000-000000000001','admin','manual','00000000-0000-0000-0000-000000000001','{{}}'); \
             INSERT INTO wamn_run.run_queue (project_env_key,run_id) VALUES ('t1','run-1');",
            manifest_hash = "a".repeat(64),
        ),
    );

    let lease = select_production_lease_sql();
    success(
        &url,
        &format!(
            "BEGIN; SET LOCAL ROLE {EXECUTOR_LOGIN}; \
        SET LOCAL app.project_env_key='t1'; SET LOCAL search_path=wamn_run; \
        DO $test$ DECLARE selected record; BEGIN \
            EXECUTE $lease${lease}$lease$ INTO STRICT selected USING ARRAY['cat'],'dev', \
                'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'; \
            ASSERT selected.run_id IS NOT NULL; \
        END $test$; ROLLBACK;"
        ),
    );
    for statement in [
        "SELECT * FROM wamn_run.run_queue",
        "UPDATE wamn_run.runs SET status='completed'",
        "INSERT INTO wamn_run.run_queue (project_env_key,run_id) VALUES ('t1','run-1')",
    ] {
        let output = psql(
            &url,
            &format!("BEGIN; SET LOCAL ROLE wamn_app; {statement}; ROLLBACK;"),
        );
        assert!(
            !output.status.success(),
            "guest was allowed to operate the durable queue"
        );
    }
    // A foreign injected project cannot select this queue, even with executor privileges.
    success(
        &url,
        &format!(
            "BEGIN; SET LOCAL ROLE {EXECUTOR_LOGIN}; \
        SET LOCAL app.project_env_key='other'; SET LOCAL search_path=wamn_run; \
        DO $test$ DECLARE selected record; BEGIN \
            EXECUTE $lease${lease}$lease$ INTO selected USING ARRAY['cat'],'dev', \
                'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'; \
            ASSERT selected.run_id IS NULL; \
        END $test$; ROLLBACK;"
        ),
    );
}
