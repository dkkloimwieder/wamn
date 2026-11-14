//! Persisted status vocabulary and canonical run-state DDL tests.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command as Proc, Stdio};

use serde_json::json;
use wamn_control_provision::{
    WorkloadRoleFamily, WorkloadRoleIdentity, sql as provision_sql, workload_login,
};
use wamn_run_state::{FailType, RunStatus};

// ---- status vocabularies ---------------------------------------------------

#[test]
fn status_sql_literals_round_trip() {
    for s in RunStatus::ALL {
        assert_eq!(RunStatus::from_sql(s.as_sql()), Some(s));
        assert_eq!(serde_json::to_value(s).unwrap(), json!(s.as_sql()));
        assert_eq!(
            serde_json::from_value::<RunStatus>(json!(s.as_sql())).unwrap(),
            s
        );
    }
    for k in FailType::ALL {
        assert_eq!(FailType::from_sql(k.as_sql()), Some(k));
    }
    assert_eq!(RunStatus::from_sql("nope"), None);
    assert!(serde_json::from_value::<RunStatus>(json!("nope")).is_err());
    // sample the wire literals the DDL CHECK constraints pin.
    assert_eq!(
        RunStatus::InfrastructureFailure.as_sql(),
        "infrastructure-failure"
    );
    assert_eq!(FailType::RetryExhausted.as_sql(), "retry-exhausted");
    assert_eq!(FailType::RunawayBudget.as_sql(), "runaway-budget");
}

#[test]
fn persisted_fail_type_vocabulary_is_exact_and_alias_free() {
    const EXPECTED: [&str; 11] = [
        "terminal",
        "retry-exhausted",
        "invalid-input",
        "runaway-budget",
        "depth-budget",
        "dispatch-budget",
        "unresolvable-name",
        "hash-invalid-bytes",
        "foreign-revision",
        "incompatible-contract",
        "unbound-requirement",
    ];

    assert_eq!(FailType::ALL.map(FailType::as_sql), EXPECTED);
    for literal in EXPECTED {
        let type_ = FailType::from_sql(literal).expect("frozen fail_type literal parses");
        assert_eq!(serde_json::to_value(type_).unwrap(), json!(literal));
        assert_eq!(
            serde_json::from_value::<FailType>(json!(literal)).unwrap(),
            type_
        );
    }

    for alias in [
        "depth_budget",
        "dispatch_budget",
        "unresolvable_name",
        "hash_invalid_bytes",
        "foreign_revision",
        "incompatible_contract",
        "unbound_requirement",
        "hash-invalid",
        "foreign-catalog-revision",
        "contract-incompatible",
        "requirement-unbound",
    ] {
        assert_eq!(FailType::from_sql(alias), None, "alias {alias:?} parsed");
        assert!(
            serde_json::from_value::<FailType>(json!(alias)).is_err(),
            "wire alias {alias:?} parsed"
        );
    }
}

// ---- live-apply acceptance test (optional) --------------------------------------------

fn live_database(url: &str) -> String {
    let output = std::process::Command::new("psql")
        .args(["-X", "-Atq", url, "-c", "SELECT current_database()"])
        .output()
        .expect("query the live run-state database name");
    assert!(
        output.status.success(),
        "querying the live run-state database name failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("the live run-state database name is UTF-8")
        .trim()
        .to_string()
}

/// Apply `deploy/sql/run-state.sql` to a test database and assert the project RLS
/// isolates rows, the idempotency index dedupes, and the INSTALLED run-status CHECK
/// admits exactly the crate's [`RunStatus`] vocabulary while refusing the retired
/// run-level `parked`. The installed `fail_type` CHECK admits exactly
/// [`FailType::ALL`], and the server answers each [`RUN_RECORD_CHECKS`] arm. The test runs as the superuser (the harness prepares an App
/// generation) and holds the process lock, because it changes cluster-wide roles.
#[test]
fn run_state_schema_applies_and_isolates_on_postgres() {
    /// The run-level status the queue park retired off the run row. The server must
    /// refuse it, not merely the file must omit it.
    const RETIRED_RUN_STATUS: &str = "parked";

    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let ddl = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../deploy/sql/run-state.sql"
    ))
    .expect("read deploy/sql/run-state.sql");
    let database = live_database(&url);
    let app_role = workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: "t1",
            database: &database,
        },
    )
    .expect("derive the live run-state App generation");
    let prepare_app = wamn_control_provision::test_database::login_sql(
        WorkloadRoleFamily::App,
        &database,
        &app_role,
        "run-state-store-test-password",
    );
    let remove_login = format!("DROP OWNED BY {app_role};");

    let mut script = String::new();
    // Prepare the production-shaped App identity and a fresh schema.
    writeln!(
        script,
        "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{app_role}') THEN \
           EXECUTE format('DROP OWNED BY %I', '{app_role}'); \
           EXECUTE format('DROP ROLE %I', '{app_role}'); \
         END IF; END $$;\n\
         DO $$ BEGIN \
            \
         END $$;\n\
         {prepare_app}\n\
         DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         CREATE SCHEMA catalog;\n\
         CREATE TABLE catalog.releases (\n\
           project_env_key text NOT NULL, manifest_digest text NOT NULL,\n\
           PRIMARY KEY (project_env_key, manifest_digest)\n\
         );\n\
         INSERT INTO catalog.releases VALUES\n\
           ('t1','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'), ('t2','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'),\n\
           ('t3','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'), ('t3','sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');"
    )
    .expect("writing to a String cannot fail");
    script.push_str(include_str!("../../../../deploy/sql/record-history.sql"));
    let actor = wamn_control_provision::PlatformComponent::Provisioning.principal_id();
    writeln!(
        script,
        "SET app.user_id='{actor}'; SET app.operation='admin:run-state-fixture';"
    )
    .unwrap();
    script.push_str(&ddl);
    script.push('\n');
    // Seed two projects as the superuser (bypasses RLS): each has one run.
    script.push_str(
        "INSERT INTO wamn_run.runs (\
           project_env_key, run_id, wiring_hash,binding_world_json, package_id, manifest_digest, environment,\
           wiring_id, wiring_version, status, idempotency_key\
         ) VALUES\
           ('t1','run-a','sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'run-state-fixture','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','test',\
            'fixture-wiring',1,'running','k-a'),\
           ('t2','run-b','sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'run-state-fixture','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','test',\
            'fixture-wiring',1,'running','k-b');\n",
    );
    // The generation's `current_user` derives project t1 and sees only that
    // project's run without trusting a settable identity.
    writeln!(
        script,
        "BEGIN;\n\
         SET LOCAL ROLE {app_role};\n\
         SET LOCAL search_path TO wamn_run;\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM runs) = 1, 't1 sees only its run'; END $$;\n\
         COMMIT;"
    )
    .expect("writing to a String cannot fail");
    // Intentional refusal fixture: the stable ACL role carries no project key,
    // so even a superuser's SET ROLE probe sees zero rows.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_run;\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM runs) = 0, 'stable role has no project'; END $$;\n\
         COMMIT;\n",
    );
    // The idempotency index rejects a duplicate (project, key); a different project
    // may reuse the same key.
    script.push_str(
        "DO $$ BEGIN \
           BEGIN \
             INSERT INTO wamn_run.runs (\
               project_env_key, run_id, wiring_hash,binding_world_json, package_id, manifest_digest, environment,\
               wiring_id, wiring_version, idempotency_key\
             ) VALUES ('t1','run-a2','sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'run-state-fixture','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','test',\
               'fixture-wiring',1,'k-a'); \
             ASSERT false, 'duplicate idempotency key must be rejected'; \
           EXCEPTION WHEN unique_violation THEN NULL; END; \
         END $$;\n\
         INSERT INTO wamn_run.runs (\
           project_env_key, run_id, wiring_hash,binding_world_json, package_id, manifest_digest, environment,\
           wiring_id, wiring_version, idempotency_key\
         ) VALUES ('t3','run-c','sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'run-state-fixture','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','test',\
           'fixture-wiring',1,'k-a');\n",
    );
    // Ask the INSTALLED status CHECK its own answer, twice over, instead of pinning the
    // declaration's text. First behaviourally: one INSERT per candidate, each in its own
    // subtransaction, recording `admitted` or the refusing SQLSTATE. Then exhaustively:
    // the literal set the server itself reports for the single-column CHECK on `status`,
    // which no behavioural probe can supply because it cannot enumerate a literal the
    // crate has never heard of.
    let candidates = RunStatus::ALL
        .into_iter()
        .map(RunStatus::as_sql)
        .chain(std::iter::once(RETIRED_RUN_STATUS))
        .map(|literal| format!("'{literal}'"))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        script,
        "CREATE TEMP TABLE status_probe (literal text PRIMARY KEY, answer text NOT NULL);\n\
         DO $status_probe$\n\
         DECLARE candidate text; ordinal int := 0;\n\
         BEGIN\n\
           FOREACH candidate IN ARRAY ARRAY[{candidates}] LOOP\n\
             ordinal := ordinal + 1;\n\
             BEGIN\n\
               INSERT INTO wamn_run.runs (\
                 project_env_key, run_id, wiring_hash,binding_world_json, package_id, manifest_digest,\
                 environment, wiring_id, wiring_version, status, idempotency_key\
               ) VALUES ('t3', 'status-probe-' || ordinal, 'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb, 'run-state-fixture', 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',\
                 'test', 'fixture-wiring', 1, candidate, 'status-probe-' || ordinal);\n\
               INSERT INTO status_probe VALUES (candidate, 'admitted');\n\
             EXCEPTION WHEN others THEN\n\
               INSERT INTO status_probe VALUES (candidate, SQLSTATE);\n\
             END;\n\
           END LOOP;\n\
         END\n\
         $status_probe$;\n\
         SELECT 'status-answer ' || literal || ' ' || answer FROM status_probe ORDER BY literal;\n\
         SELECT 'status-installed ' || string_agg(DISTINCT literal, ' ' ORDER BY literal)\n\
         FROM (\n\
           SELECT hit.parts[1] AS literal\n\
           FROM pg_constraint AS con\n\
           JOIN pg_attribute AS col\n\
             ON col.attrelid = con.conrelid AND col.attnum = con.conkey[1]\n\
           CROSS JOIN LATERAL regexp_matches(\
             pg_get_constraintdef(con.oid), '''([^'']*)''', 'g') AS hit(parts)\n\
           WHERE con.conrelid = 'wamn_run.runs'::regclass\n\
             AND con.contype = 'c'\n\
             AND cardinality(con.conkey) = 1\n\
             AND col.attname = 'status'\n\
         ) AS installed;"
    )
    .expect("writing to a String cannot fail");
    // The installed fail_type CHECK, asked for its literal set like the status one.
    script.push_str(
        "SELECT 'fail-type-installed ' || string_agg(DISTINCT literal, ' ' ORDER BY literal)\n\
         FROM (\n\
           SELECT hit.parts[1] AS literal\n\
           FROM pg_constraint AS con\n\
           JOIN pg_attribute AS col\n\
             ON col.attrelid = con.conrelid AND col.attnum = con.conkey[1]\n\
           CROSS JOIN LATERAL regexp_matches(\
             pg_get_constraintdef(con.oid), '''([^'']*)''', 'g') AS hit(parts)\n\
           WHERE con.conrelid = 'wamn_run.runs'::regclass\n\
             AND con.contype = 'c'\n\
             AND cardinality(con.conkey) = 1\n\
             AND col.attname = 'fail_type'\n\
         ) AS installed;\n",
    );
    script.push_str(RUN_RECORD_CHECKS);
    writeln!(
        script,
        "SET search_path = wamn_run; PREPARE prune(text,bigint) AS {}; \
         EXECUTE prune('t3', 7); \
         DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM runs WHERE run_id LIKE 'prune-old-%' \
                              AND project_env_key='t3' AND status IN \
                                  ('completed','failed','infrastructure-failure')), \
             'pruning removes every old terminal status'; \
           ASSERT (SELECT count(*) FROM runs WHERE run_id LIKE 'prune-%') = 5, \
             'pruning preserves active, recent and foreign history'; \
         END $$;",
        wamn_run_state::sql::prune_terminal_runs_sql()
    )
    .unwrap();
    // Terminal run history remains deletable.
    script.push_str(
        "UPDATE wamn_run.runs SET status='completed' \
           WHERE project_env_key='t1' AND run_id='run-a';\n\
         DELETE FROM wamn_run.runs WHERE project_env_key='t1' AND run_id='run-a';\n",
    );
    writeln!(
        script,
        "DROP SCHEMA wamn_run CASCADE; DROP SCHEMA catalog CASCADE;\n\
         {remove_login}\n\
         DROP ROLE \"{app_role}\";"
    )
    .expect("writing to a String cannot fail");

    let mut child = Proc::new("psql")
        .arg(&url)
        // `-A -t` so the probe's answers arrive as bare tagged lines.
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-A", "-t", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Carry the server's answers back across the Rust boundary.
    let stdout = String::from_utf8(out.stdout).expect("psql stdout is UTF-8");
    let mut answers = std::collections::BTreeMap::new();
    let mut installed = None;
    let mut fail_types = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("fail-type-installed ") {
            fail_types = Some(rest.to_string());
        }
        if let Some(rest) = line.strip_prefix("status-answer ") {
            let (literal, answer) = rest
                .split_once(' ')
                .expect("each status answer is `<literal> <answer>`");
            answers.insert(literal.to_string(), answer.to_string());
        } else if let Some(rest) = line.strip_prefix("status-installed ") {
            installed = Some(rest.to_string());
        }
    }

    for status in RunStatus::ALL {
        assert_eq!(
            answers.get(status.as_sql()).map(String::as_str),
            Some("admitted"),
            "the installed runs status CHECK refused the crate status {:?}",
            status.as_sql()
        );
    }
    // 23514 is check_violation: the constraint refused it, not a trigger or a type error.
    assert_eq!(
        answers.get(RETIRED_RUN_STATUS).map(String::as_str),
        Some("23514"),
        "the installed runs status CHECK must refuse the retired run-level \
         {RETIRED_RUN_STATUS:?} with check_violation"
    );

    let mut vocabulary = RunStatus::ALL.map(RunStatus::as_sql);
    vocabulary.sort_unstable();
    assert_eq!(
        installed.as_deref(),
        Some(vocabulary.join(" ").as_str()),
        "the installed runs status CHECK admits a different vocabulary than RunStatus::ALL"
    );

    let mut fail_vocabulary = FailType::ALL.map(FailType::as_sql);
    fail_vocabulary.sort_unstable();
    assert_eq!(
        fail_types.as_deref(),
        Some(fail_vocabulary.join(" ").as_str()),
        "the installed runs fail_type CHECK admits a different vocabulary than FailType::ALL"
    );
}

/// The run-record rules that no other live test asks the server about. Each
/// arm is a refused write, a trigger error or a `pg_catalog` fact.
const RUN_RECORD_CHECKS: &str = "\
-- Retired carriers stay absent, and the causation and admission carriers exist.
DO $$ BEGIN
  ASSERT NOT EXISTS (
    SELECT FROM pg_attribute
     WHERE attrelid = 'wamn_run.runs'::regclass AND NOT attisdropped
       AND attname IN ('replay_of', 'root_run_id', 'fail_node', 'fail_reason',
                       'release_version', 'payload_size', 'preview_head', 'redacted')
  ), 'runs carries a retired column';
  ASSERT to_regclass('wamn_run.runs_root') IS NULL, 'the retired runs_root index exists';
  ASSERT (SELECT string_agg(attname || ':' || format_type(atttypid, atttypmod), ' ' ORDER BY attname)
            FROM pg_attribute
           WHERE attrelid = 'wamn_run.runs'::regclass AND NOT attisdropped
             AND attname IN ('event_source_run_id', 'event_root_run_id', 'event_depth',
                             'manifest_digest'))
       = 'event_depth:integer event_root_run_id:text '
         'event_source_run_id:text manifest_digest:text',
    'runs carries the causation and release pin carriers';
END $$;

-- A run defaults to no capture and refuses an unsupported capture mode.
INSERT INTO wamn_run.runs (
  project_env_key, run_id, wiring_hash,binding_world_json, package_id, manifest_digest, environment,
  wiring_id, wiring_version, status, idempotency_key
) VALUES ('t3', 'record-probe', 'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb, 'run-state-fixture', 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', 'test',
          'fixture-wiring', 1, 'running', 'record-probe');
DO $$ BEGIN
  ASSERT (SELECT capture_mode FROM wamn_run.runs
           WHERE project_env_key = 't3' AND run_id = 'record-probe') = 'off',
    'an admitted run defaults to capture off';
END $$;
DO $$
DECLARE refused text;
BEGIN
  FOREACH refused IN ARRAY ARRAY[
    'capture_mode = ''bogus''',
    'capture_mode = ''full'''
  ] LOOP
    BEGIN
      EXECUTE format(
        'INSERT INTO wamn_run.runs (project_env_key, run_id, wiring_hash,binding_world_json, package_id, '
        'manifest_digest, environment, wiring_id, wiring_version, idempotency_key, '
        'capture_mode) VALUES (''t3'', ''record-refused'', ''sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd'',''[]''::jsonb, '
        '''run-state-fixture'', ''sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'', ''test'', ''fixture-wiring'', 1, ''record-refused'', %s)',
        split_part(refused, ' = ', 2));
      ASSERT false, format('a run with %s must be refused', refused);
    EXCEPTION WHEN check_violation THEN NULL; END;
  END LOOP;
END $$;

-- Every admission pin is immutable after insertion.
DO $$
DECLARE assignment text; refusal text;
BEGIN
  FOREACH assignment IN ARRAY ARRAY[
    'package_id = ''other''',
    'manifest_digest = ''sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855''', 'manifest_digest = NULL',
    'environment = ''other''', 'capture_mode = ''full''',
    'wiring_id = ''other''', 'wiring_version = 2',
    'wiring_hash = ''other''', 'binding_world_json = ''{}''::jsonb',
    'service_principal_id = ''00000000-0000-4000-8000-000000000001''::uuid'
  ] LOOP
    refusal := NULL;
    BEGIN
      EXECUTE 'UPDATE wamn_run.runs SET ' || assignment
        || ' WHERE project_env_key = ''t3'' AND run_id = ''record-probe''';
    EXCEPTION WHEN object_not_in_prerequisite_state THEN refusal := SQLERRM; END;
    ASSERT refusal = 'run-admission-pin-immutable',
      format('changing %s must be refused as run-admission-pin-immutable, got %s',
             assignment, coalesce(refusal, 'no refusal'));
  END LOOP;
END $$;

-- Only terminal history is deletable: a running run stays, and each
-- terminal status goes. The guard runs with its caller's rights and PUBLIC
-- cannot call it.
UPDATE wamn_run.runs SET status = 'running'
 WHERE project_env_key = 't3' AND run_id = 'record-probe';
DO $$
DECLARE refusal text;
BEGIN
  BEGIN
    DELETE FROM wamn_run.runs WHERE project_env_key = 't3' AND run_id = 'record-probe';
  EXCEPTION WHEN object_not_in_prerequisite_state THEN refusal := SQLERRM; END;
  ASSERT refusal = 'run-delete-nonterminal',
    format('a running run must not be deletable, got %s', coalesce(refusal, 'a delete'));
END $$;
DO $$
DECLARE terminal text;
BEGIN
  FOREACH terminal IN ARRAY ARRAY['completed', 'failed', 'infrastructure-failure'] LOOP
    INSERT INTO wamn_run.runs (
      project_env_key, run_id, wiring_hash,binding_world_json, package_id, manifest_digest, environment,
      wiring_id, wiring_version, status, idempotency_key
    ) VALUES ('t3', 'terminal-' || terminal, 'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb, 'run-state-fixture', 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', 'test',
              'fixture-wiring', 1, terminal, 'terminal-' || terminal);
    DELETE FROM wamn_run.runs WHERE project_env_key = 't3' AND run_id = 'terminal-' || terminal;
  END LOOP;
  ASSERT NOT (SELECT prosecdef FROM pg_proc
               WHERE oid = 'wamn_run.guard_terminal_run_delete()'::regprocedure),
    'the delete guard runs with its caller''s rights';
  ASSERT (SELECT proacl IS NOT NULL
             AND NOT EXISTS (SELECT FROM aclexplode(proacl) AS grant_row WHERE grant_row.grantee = 0)
            FROM pg_proc WHERE oid = 'wamn_run.guard_terminal_run_delete()'::regprocedure),
    'PUBLIC cannot execute the delete guard';
END $$;

-- The app role may read and prune runs and may not write them.
DO $$ BEGIN
  ASSERT has_table_privilege('wamn_app', 'wamn_run.runs', 'SELECT')
     AND has_table_privilege('wamn_app', 'wamn_run.runs', 'DELETE'),
    'wamn_app reads and prunes runs';
  ASSERT NOT has_table_privilege('wamn_app', 'wamn_run.runs', 'INSERT')
     AND NOT has_table_privilege('wamn_app', 'wamn_run.runs', 'UPDATE'),
    'wamn_app must not write runs';
END $$;

DO $$
DECLARE relation regclass; required text[]; column_name text;
BEGIN
  ASSERT to_regclass('wamn_run.invocation_admissions') IS NULL,
    'the retired admission table stays absent';
  ASSERT NOT EXISTS (
    SELECT FROM pg_attribute WHERE attrelid='wamn_run.runs'::regclass
      AND NOT attisdropped AND attname IN (
        'parent_run_id','parent_node_id','parent_occurrence','waiting_child_run_id',
        'waiting_child_occurrence','wait_generation','invoke_depth','invoke_root_run_id')
  ), 'retired execution lineage stays absent';
  FOR relation, required IN VALUES
    ('wamn_run.runs'::regclass, ARRAY['project_env_key','run_id','wiring_hash','binding_world_json',
      'status','trigger_source','capture_mode','input_json','result_json','state_json',
      'fail_type','updated_at','package_id','manifest_digest','attachment_id',
      'registration_id','caller_outcome_type','caller_outcome_json','caller_http_status',
      'caller_release_node_id','caller_outcome_hash','caller_released_at',
      'response_deadline_at','run_deadline_at','invocation_context',
      'admission_context_version','platform_revision','terminal_reason'])
  LOOP
    FOREACH column_name IN ARRAY required LOOP
      ASSERT EXISTS (SELECT FROM pg_attribute WHERE attrelid=relation
                      AND NOT attisdropped AND attname=column_name),
        format('%s lacks %s', relation, column_name);
    END LOOP;
  END LOOP;
END $$;

-- Event ancestry must be complete, and each coordinate stays immutable.
INSERT INTO wamn_run.runs (project_env_key,run_id,wiring_hash,binding_world_json,package_id,environment,
  wiring_id,wiring_version,trigger_source,event_source_run_id,event_root_run_id,event_depth)
VALUES ('t3','lineage-probe','sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'run-state-fixture','test','fixture-wiring',1,
  'event','source','root',1);
DO $$
DECLARE assignment text;
BEGIN
  FOREACH assignment IN ARRAY ARRAY['event_source_run_id = ''other''',
    'event_root_run_id = ''other''','event_depth = 2'] LOOP
    BEGIN
      EXECUTE 'UPDATE wamn_run.runs SET ' || assignment || ' WHERE run_id=''lineage-probe''';
      ASSERT false, 'event ancestry changed';
    EXCEPTION WHEN raise_exception THEN
      ASSERT SQLERRM='event causation lineage is immutable', SQLERRM;
    END;
  END LOOP;
  FOREACH assignment IN ARRAY ARRAY[
    '''event'',NULL,''root'',1', '''event'','''',''root'',1',
    '''event'',''source'',NULL,1', '''event'',''source'','''',1',
    '''event'',''source'',''root'',NULL'] LOOP
    BEGIN
      EXECUTE 'INSERT INTO wamn_run.runs (project_env_key,run_id,wiring_hash,binding_world_json,package_id,'
        || 'environment,wiring_id,wiring_version,trigger_source,event_source_run_id,'
        || 'event_root_run_id,event_depth) VALUES (''t3'',''bad-lineage'',''sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd'',''[]''::jsonb,'
        || '''run-state-fixture'',''test'',''fixture-wiring'',1,' || assignment || ')';
      ASSERT false, 'incomplete event ancestry was admitted';
    EXCEPTION WHEN check_violation THEN NULL; END;
  END LOOP;
END $$;

-- Retention spans terminal states, age, and projects.
INSERT INTO wamn_run.runs (project_env_key,run_id,wiring_hash,binding_world_json,package_id,environment,
  wiring_id,wiring_version,status,created_at)
SELECT project, 'prune-' || age || '-' || state, 'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'run-state-fixture','test',
  'fixture-wiring',1,state,now()-CASE age WHEN 'old' THEN interval '8 days' ELSE interval '1 day' END
FROM (VALUES ('t3','old','completed'),('t3','old','failed'),
  ('t3','old','infrastructure-failure'),('t3','old','dispatched'),('t3','old','running'),
  ('t3','recent','completed'),('t3','recent','failed'),('t1','old','completed'))
  AS fixtures(project,age,state);

";
