//! Ignored PostgreSQL proof for the surviving run-queue authority matrix.

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::thread;

use wamn_control_provision::{WorkloadRoleFamily, sql};
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

fn assert_sqlstate(url: &str, script: &str, state: &str, message: &str) {
    let output = psql(url, &format!("\\set VERBOSITY verbose\n{script}"));
    assert!(!output.status.success(), "statement was admitted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(state), "SQLSTATE drifted:\n{stderr}");
    assert!(stderr.contains(message), "refusal drifted:\n{stderr}");
}

fn management_prepares() -> String {
    let management = management_admission_transaction(&RunStateSchema::default());
    format!(
        "PREPARE management_lock(text,text,text,int) AS {}; \
         PREPARE management_admit(\
           text,text,text,int,text,text,text,text,timestamptz,text,text,int,\
           text,int,text,text,text\
         ) AS {};",
        management.lock_producer(),
        management.admit(),
    )
}

fn test_case_admission(
    report_id: &str,
    ordinal: i32,
    run_id: &str,
    wiring_id: &str,
    wiring_hash: &str,
    gate_report_id: &str,
    prior_binding_world: Option<&str>,
) -> String {
    let prior = prior_binding_world
        .map(|world| format!("$binding_world${world}$binding_world$"))
        .unwrap_or_else(|| "NULL".to_string());
    format!(
        "EXECUTE management_lock('test-case',NULL,'{report_id}',{ordinal}); \
         EXECUTE management_admit(\
           'test-case','cat','dev',1,'{run_id}','{{}}','{{}}','proof-revision',\
           '2099-01-01T00:00:00Z',NULL,'{report_id}',{ordinal},'{wiring_id}',1,\
           '{wiring_hash}','{gate_report_id}',{prior}\
         );"
    )
}

fn as_management(script: &str) -> String {
    format!(
        "BEGIN; SET LOCAL ROLE {MANAGEMENT_LOGIN}; SET LOCAL app.tenant='t1'; \
         SELECT current_user; {} {script} COMMIT;",
        management_prepares(),
    )
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
    let database = success(&url, "SELECT current_database();")
        .trim()
        .to_string();
    let access_floor = sql::grant_connect_on_database_sql(&database);
    let management_provision = sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::ManagementAdmitter,
        &database,
        MANAGEMENT_LOGIN,
        "management-proof-password",
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
             CREATE ROLE wamn_executor_platform NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE {EXECUTOR_LOGIN} LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOREPLICATION NOBYPASSRLS; \
             GRANT wamn_executor_platform TO {EXECUTOR_LOGIN} WITH SET FALSE, ADMIN FALSE; \
             BEGIN; {catalog} {run_state} {run_queue} COMMIT; \
             {access_floor} \
             {management_provision}"
        ),
    );
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
             END $$;"
        ),
    );

    let component_digest = format!("sha256:{}", "a".repeat(64));
    let imports_fingerprint = format!("sha256:{}", "b".repeat(64));
    let wiring_hash = format!("sha256:{}", "c".repeat(64));
    let race_wiring_hash = format!("sha256:{}", "d".repeat(64));

    // The executor keeps test-only union grants so its wrong-class management
    // attempt reaches the current_user guard. Management uses only the exact
    // production surface above. The two candidate rows share one component
    // with two requirements so array ordering is observable.
    success(
        &url,
        &format!(
            "GRANT USAGE ON SCHEMA wamn_run, catalog \
               TO wamn_executor_platform; \
             GRANT USAGE ON SCHEMA wamn_run TO wamn_control_author; \
             GRANT SELECT, INSERT, UPDATE, DELETE \
               ON ALL TABLES IN SCHEMA catalog, wamn_run \
               TO wamn_executor_platform; \
             INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ('t1','cat',1,'dev','0.1','applied'); \
             INSERT INTO catalog.releases \
               (tenant_id,catalog_id,catalog_version) VALUES ('t1','cat',1); \
             INSERT INTO catalog.component_library \
               (tenant_id,catalog_id,catalog_version,component,interface_version,operation, \
                component_digest,imports,imports_fingerprint,input_ports,output_ports,parameters) \
             VALUES ('t1','cat',1,'entity','0.1','create','{component_digest}', \
                     '[]','{imports_fingerprint}','[]','[]','[]'); \
             INSERT INTO catalog.wirings \
               (tenant_id,catalog_id,wiring_id,version,gated_catalog_version, \
                graph_json,wiring_hash,gate_report_id) VALUES \
               ('t1','cat','candidate',1,1, \
                '{{\"format-version\":\"0.1\",\"wiring-id\":\"candidate\",\"version\":1, \
                   \"entry\":\"node\",\"nodes\":{{\"node\":{{\"component\":\"entity\", \
                   \"interface-version\":\"0.1\",\"operation\":\"create\"}}}}}}', \
                '{wiring_hash}','report-a'), \
               ('t1','cat','race',1,1, \
                '{{\"format-version\":\"0.1\",\"wiring-id\":\"race\",\"version\":1, \
                   \"entry\":\"node\",\"nodes\":{{\"node\":{{\"component\":\"entity\", \
                   \"interface-version\":\"0.1\",\"operation\":\"create\"}}}}}}', \
                '{race_wiring_hash}','report-race'); \
             INSERT INTO catalog.connection_requirements \
               (tenant_id,component_digest,store_alias,requirement_json,requirement_hash) VALUES \
               ('t1','{component_digest}','z-store','{{\"requirement-type\":\"http\"}}','req-z'), \
               ('t1','{component_digest}','a-store','{{\"requirement-type\":\"http\"}}','req-a'); \
             INSERT INTO catalog.connection_instances \
               (tenant_id,environment,instance_id,requirement_type,contract) VALUES \
               ('t1','dev','instance-z','http','wamn:http/0.1'), \
               ('t1','dev','instance-a','http','wamn:http/0.1'); \
             INSERT INTO catalog.connection_generations \
               (tenant_id,environment,instance_id,generation,definition_json, \
                definition_hash,credential_set_handle) VALUES \
               ('t1','dev','instance-z',1,'{{\"base-url\":\"https://z.invalid\"}}', \
                'definition-z-1','credential-z-1'), \
               ('t1','dev','instance-a',1,'{{\"base-url\":\"https://a.invalid\"}}', \
                'definition-a-1','credential-a-1'); \
             UPDATE catalog.connection_instances \
                SET active_generation=1,revision=1,updated_at=clock_timestamp()+interval '1 second'; \
             INSERT INTO catalog.connection_bindings \
               (tenant_id,catalog_id,catalog_version,component_digest,store_alias, \
                environment,instance_id,binding_status,validation_status,validation_hash) VALUES \
               ('t1','cat',1,'{component_digest}','z-store','dev','instance-z', \
                'active','valid','validation-z'), \
               ('t1','cat',1,'{component_digest}','a-store','dev','instance-a', \
                'active','valid','validation-a'); \
             INSERT INTO wamn_run.environment_policies \
               (tenant_id,expected_environment,durability_class) \
             VALUES ('t1','dev','standard'); \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
                wiring_id,wiring_version,status,trigger_source,input_json) \
             VALUES ('t1','run-1','legacy-flow',1,'cat',1,'dev','legacy-wiring',1, \
                     'dispatched','automation','{{}}'); \
             INSERT INTO wamn_run.run_queue (tenant_id,run_id) VALUES ('t1','run-1');"
        ),
    );

    let claim = select_production_claim_sql();
    let claimed = success(
        &url,
        &format!(
            "BEGIN; SET LOCAL ROLE {EXECUTOR_LOGIN}; SET LOCAL app.tenant='t1'; \
             SET LOCAL search_path=wamn_run,catalog,public; \
             SELECT current_user; PREPARE matrix_claim(text,text) AS {claim}; \
             EXECUTE matrix_claim('cat','dev'); ROLLBACK;"
        ),
    );
    assert!(claimed.contains(EXECUTOR_LOGIN));
    assert!(claimed.contains("run-1"));

    let ordinal_zero = test_case_admission(
        "report-a",
        0,
        "case-run-0",
        "candidate",
        &wiring_hash,
        "report-a",
        None,
    );
    let admitted = success(&url, &as_management(&ordinal_zero));
    let admitted_row = admitted
        .lines()
        .find(|line| line.starts_with("admitted|case-run-0|"))
        .expect("candidate admission returns its frozen world");
    let binding_world = admitted_row
        .splitn(3, '|')
        .nth(2)
        .expect("binding-world result column");
    let binding_world_value: serde_json::Value =
        serde_json::from_str(binding_world).expect("binding world is JSON");
    let aliases = binding_world_value
        .as_array()
        .expect("binding world is an array")
        .iter()
        .map(|fact| fact["store-alias"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(aliases, ["a-store", "z-store"]);
    assert!(!binding_world.contains("base-url"));

    let duplicate = success(&url, &as_management(&ordinal_zero));
    assert!(duplicate.contains(&format!("duplicate|case-run-0|{binding_world}")));

    // The report is not guest echo: it must be the gate report on the exact
    // candidate row, with its own refusal literal and no mutation.
    let gate_mismatch = success(
        &url,
        &as_management(&test_case_admission(
            "wrong-report",
            0,
            "wrong-report-run",
            "candidate",
            &wiring_hash,
            "report-a",
            None,
        )),
    );
    assert!(gate_mismatch.contains("gate-report-mismatch||"));

    // Rotate one mutable instance pointer. The already-admitted ordinal still
    // recovers its frozen world, while a later ordinal exact-comparing that
    // trusted prior world refuses before a run or queue row is inserted.
    success(
        &url,
        "INSERT INTO catalog.connection_generations \
           (tenant_id,environment,instance_id,generation,definition_json, \
            definition_hash,credential_set_handle) \
         VALUES ('t1','dev','instance-a',2,'{\"base-url\":\"https://a2.invalid\"}', \
                 'definition-a-2','credential-a-2'); \
         UPDATE catalog.connection_instances \
            SET active_generation=2,revision=revision+1, \
                updated_at=clock_timestamp()+interval '1 second' \
          WHERE tenant_id='t1' AND environment='dev' AND instance_id='instance-a';",
    );
    let recovered = success(&url, &as_management(&ordinal_zero));
    assert!(recovered.contains(&format!("duplicate|case-run-0|{binding_world}")));
    let drift = success(
        &url,
        &as_management(&test_case_admission(
            "report-a",
            1,
            "case-run-1",
            "candidate",
            &wiring_hash,
            "report-a",
            Some(binding_world),
        )),
    );
    assert!(drift.contains("binding-world-drift||"));
    assert_eq!(
        success(
            &url,
            "SELECT count(*) FROM wamn_run.runs WHERE run_id='case-run-1'; \
             SELECT count(*) FROM wamn_run.run_queue WHERE run_id='case-run-1';"
        ),
        "0\n0\n"
    );

    // The complete-grain CHECK makes a half candidate row unrepresentable, and
    // the trigger names every component-era pin.
    assert_sqlstate(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,catalog_id,catalog_version,environment, \
            wiring_id,wiring_version,status,trigger_source,input_json) \
         VALUES ('t1','half-run','cat',1,'dev','candidate',1, \
                 'dispatched','test-case','{}');",
        "23514",
        "runs_execution_grain_check",
    );
    assert_sqlstate(
        &url,
        "UPDATE wamn_run.runs SET binding_world_json='[]' \
          WHERE tenant_id='t1' AND run_id='case-run-0';",
        "55000",
        "run-admission-pin-immutable",
    );

    // Two simultaneous first admissions serialize on the DB-derived producer
    // key. One creates the ordinary run/queue pair and the other observes the
    // same row and world; neither can return a key-only duplicate.
    let race = as_management(&test_case_admission(
        "report-race",
        0,
        "race-run",
        "race",
        &race_wiring_hash,
        "report-race",
        None,
    ));
    let (race_a, race_b) = thread::scope(|scope| {
        let left = scope.spawn(|| success(&url, &race));
        let right = scope.spawn(|| success(&url, &race));
        (left.join().unwrap(), right.join().unwrap())
    });
    let combined = format!("{race_a}\n{race_b}");
    assert!(combined.contains("admitted|race-run|"));
    assert!(combined.contains("duplicate|race-run|"));
    assert_eq!(
        success(
            &url,
            "SELECT count(*) FROM wamn_run.runs WHERE run_id='race-run'; \
             SELECT count(*) FROM wamn_run.run_queue WHERE run_id='race-run';"
        ),
        "1\n1\n"
    );

    // Every ordered cross-class attempt reaches the exact current_user guard.
    assert_refusal(
        &url,
        &format!(
            "BEGIN; \
             GRANT USAGE ON SCHEMA wamn_run, catalog TO {MANAGEMENT_LOGIN}; \
             GRANT SELECT, INSERT, UPDATE, DELETE \
               ON ALL TABLES IN SCHEMA catalog, wamn_run TO {MANAGEMENT_LOGIN}; \
             SET LOCAL ROLE {MANAGEMENT_LOGIN}; SET LOCAL app.tenant='t1'; \
             SET LOCAL search_path=wamn_run,catalog,public; \
             PREPARE wrong_claim(text,text) AS {claim}; \
             EXECUTE wrong_claim('cat','dev');"
        ),
        "executor-platform-authority-required",
    );
    assert_refusal(
        &url,
        &format!(
            "BEGIN; SET LOCAL ROLE {EXECUTOR_LOGIN}; SET LOCAL app.tenant='t1'; \
             {} EXECUTE management_lock('test-case',NULL,'report-a',0);",
            management_prepares(),
        ),
        "management-admission-authority-required",
    );

    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE']) p \
             WHERE pg_catalog.has_table_privilege('wamn_app','wamn_run.run_queue',p)); \
         END $$;",
    );
    // The management-admitter row is distinct from every author/guest writer.
    // `wamn_app` is the guest SQL principal; neither it nor the host-side
    // author/effect roles can cross either surviving run-queue authority guard.
    for denied_role in [
        "wamn_app",
        "wamn_control_author",
        "wamn_scenario_author",
        "wamn_effect_writer",
    ] {
        assert_refusal(
            &url,
            &format!(
                "BEGIN; SET LOCAL ROLE {denied_role}; \
                 SELECT wamn_run.require_executor_platform_authority();"
            ),
            "executor-platform-authority-required",
        );
        assert_refusal(
            &url,
            &format!(
                "BEGIN; SET LOCAL ROLE {denied_role}; \
                 SELECT wamn_run.require_management_admission_authority();"
            ),
            "management-admission-authority-required",
        );
    }
}
