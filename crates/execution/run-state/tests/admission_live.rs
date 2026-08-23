//! Ignored PostgreSQL proof for the surviving run-queue authority matrix.

use std::io::Write;
use std::process::{Command, Output, Stdio};

use wamn_run_state::admission::{RunStateSchema, management_admission_transaction};
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
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
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

fn assert_refusal(url: &str, script: &str, message: &str) {
    let output = psql(url, &format!("\\set VERBOSITY verbose\n{script}"));
    assert!(
        !output.status.success(),
        "cross-class statement was admitted"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("42501"), "SQLSTATE drifted:\n{stderr}");
    assert!(
        stderr.contains(message),
        "refusal literal drifted:\n{stderr}"
    );
}

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn surviving_authority_matrix_live() {
    let url = std::env::var("WAMN_RUN_STORE_PG_URL")
        .expect("set WAMN_RUN_STORE_PG_URL to the throwaway superuser database");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .expect("read catalog DDL");
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .expect("read run-state DDL");
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .expect("read run-queue DDL");

    success(
        &url,
        &format!(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               FOREACH role_name IN ARRAY ARRAY[ \
                 'wamn_app','wamn_scenario_author','wamn_effect_writer', \
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
             CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_executor_platform NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_management_admitter NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE {EXECUTOR_LOGIN} LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE {MANAGEMENT_LOGIN} LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOREPLICATION NOBYPASSRLS; \
             GRANT wamn_executor_platform TO {EXECUTOR_LOGIN} WITH SET FALSE, ADMIN FALSE; \
             GRANT wamn_management_admitter TO {MANAGEMENT_LOGIN} WITH SET FALSE, ADMIN FALSE; \
             {catalog} {run_state} {run_queue}"
        ),
    );

    // Test-only union grants make the current_user guards, rather than an
    // earlier generic ACL error, the named pairwise refusal witness. Production
    // role creation and exact positive grants remain with their owning cutovers.
    success(
        &url,
        &format!(
            "GRANT USAGE ON SCHEMA wamn_run, catalog \
               TO wamn_executor_platform, wamn_management_admitter; \
             GRANT SELECT, INSERT, UPDATE, DELETE \
               ON ALL TABLES IN SCHEMA catalog, wamn_run \
               TO wamn_executor_platform, wamn_management_admitter; \
             INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ('t1','cat',1,'dev','0.1','applied'); \
             INSERT INTO catalog.release_manifests (tenant_id,catalog_id,catalog_version) \
             VALUES ('t1','cat',1); \
             INSERT INTO catalog.catalog_heads \
               (tenant_id,catalog_id,environment,applied_catalog_version) \
             VALUES ('t1','cat','dev',1); \
             INSERT INTO wamn_run.environment_policies \
               (tenant_id,expected_environment,durability_class) \
             VALUES ('t1','dev','standard'); \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
                wiring_id,wiring_version,status,trigger_source,input_json) \
             VALUES ('t1','run-1','flow',1,'cat',1,'dev','wiring',1, \
                     'dispatched','automation','{{}}'); \
             INSERT INTO wamn_run.run_queue (tenant_id,run_id) VALUES ('t1','run-1');"
        ),
    );

    let claim = select_production_claim_sql();
    let management = management_admission_transaction(&RunStateSchema::default());
    let management_lock = management
        .lock_head()
        .replace("$1", "'cat'")
        .replace("$2", "'dev'");
    let management_admit = format!(
        "PREPARE management_admit(\
           text,text,text,int,text,int,text,text,text,text,timestamptz,text,text,int,text\
         ) AS {}; \
         EXECUTE management_admit(\
           'draft-run','cat','dev',1,'flow',1,'management-proof','{{}}','{{}}',\
           'proof-revision',statement_timestamp()+interval '1 minute',\
           'proof-command',NULL,NULL,'missing-wiring'\
         );",
        management.admit()
    );

    // Each class succeeds through its exact ordinary production statement while
    // current_user remains the opaque test login, not the stable ACL role.
    let claimed = success(
        &url,
        &format!(
            "BEGIN; SET LOCAL ROLE {EXECUTOR_LOGIN}; SET LOCAL app.tenant='t1'; \
             SELECT current_user; {claim}; ROLLBACK;"
        ),
    );
    assert!(claimed.contains(EXECUTOR_LOGIN));
    assert!(claimed.contains("run-1"));

    let locked = success(
        &url,
        &format!(
            "BEGIN; SET LOCAL ROLE {MANAGEMENT_LOGIN}; SET LOCAL app.tenant='t1'; \
             SELECT current_user; {management_lock}; ROLLBACK;"
        ),
    );
    assert!(locked.contains(MANAGEMENT_LOGIN));
    assert!(locked.contains('1'));
    let admitted = success(
        &url,
        &format!(
            "BEGIN; SET LOCAL ROLE {MANAGEMENT_LOGIN}; SET LOCAL app.tenant='t1'; \
             SELECT current_user; {management_admit} ROLLBACK;"
        ),
    );
    assert!(admitted.contains(MANAGEMENT_LOGIN));
    assert!(admitted.contains("inactive-wiring"));

    // Every ordered cross-class statement attempt reaches the exact 42501 arm.
    assert_refusal(
        &url,
        &format!("BEGIN; SET LOCAL ROLE {MANAGEMENT_LOGIN}; SET LOCAL app.tenant='t1'; {claim};"),
        "executor-platform-authority-required",
    );
    assert_refusal(
        &url,
        &format!(
            "BEGIN; SET LOCAL ROLE {EXECUTOR_LOGIN}; SET LOCAL app.tenant='t1'; \
             {management_lock};"
        ),
        "management-admission-authority-required",
    );
    assert_refusal(
        &url,
        &format!(
            "BEGIN; SET LOCAL ROLE {EXECUTOR_LOGIN}; SET LOCAL app.tenant='t1'; \
             {management_admit}"
        ),
        "management-admission-authority-required",
    );

    // The retired guest ACL has no queue privilege and both guarded classes
    // refuse it by the same frozen literals.
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE']) p \
             WHERE pg_catalog.has_table_privilege('wamn_app','wamn_run.run_queue',p)); \
         END $$;",
    );
    assert_refusal(
        &url,
        "BEGIN; SET LOCAL ROLE wamn_app; \
         SELECT wamn_run.require_executor_platform_authority();",
        "executor-platform-authority-required",
    );
    assert_refusal(
        &url,
        "BEGIN; SET LOCAL ROLE wamn_app; \
         SELECT wamn_run.require_management_admission_authority();",
        "management-admission-authority-required",
    );
}
