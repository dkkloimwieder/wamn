//! Persisted status vocabulary and canonical run-state DDL tests.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command as Proc, Stdio};

use serde_json::json;
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql as provision_sql,
    workload_generation_role,
};
use wamn_run_state::{EffectUncertainFailure, FailKind, RunStatus};

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
    for k in FailKind::ALL {
        assert_eq!(FailKind::from_sql(k.as_sql()), Some(k));
    }
    assert_eq!(RunStatus::from_sql("nope"), None);
    assert!(serde_json::from_value::<RunStatus>(json!("nope")).is_err());
    // Spot-check the wire literals the DDL CHECK constraints pin.
    assert_eq!(
        RunStatus::InfrastructureFailure.as_sql(),
        "infrastructure-failure"
    );
    assert_eq!(RunStatus::EffectUncertain.as_sql(), "effect-uncertain");
    assert!(!RunStatus::EffectUncertain.is_terminal());
    assert_eq!(FailKind::RetryExhausted.as_sql(), "retry-exhausted");
    assert_eq!(FailKind::RunawayBudget.as_sql(), "runaway-budget");
}

#[test]
fn persisted_fail_kind_vocabulary_is_exact_and_alias_free() {
    const EXPECTED: [&str; 12] = [
        "terminal",
        "retry-exhausted",
        "invalid-input",
        "runaway-budget",
        "effect-uncertain",
        "depth-budget",
        "dispatch-budget",
        "unresolvable-name",
        "hash-invalid-bytes",
        "foreign-revision",
        "incompatible-contract",
        "unbound-requirement",
    ];

    assert_eq!(FailKind::ALL.map(FailKind::as_sql), EXPECTED);
    for literal in EXPECTED {
        let kind = FailKind::from_sql(literal).expect("frozen fail_kind literal parses");
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(literal));
        assert_eq!(
            serde_json::from_value::<FailKind>(json!(literal)).unwrap(),
            kind
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
        assert_eq!(FailKind::from_sql(alias), None, "alias {alias:?} parsed");
        assert!(
            serde_json::from_value::<FailKind>(json!(alias)).is_err(),
            "wire alias {alias:?} parsed"
        );
    }
}

#[test]
fn effect_uncertain_failure_has_one_exact_non_committal_shape() {
    let failure = EffectUncertainFailure::new("run-17").unwrap();
    let bytes = failure.canonical_json_bytes();

    assert_eq!(failure.code(), "effect-uncertain");
    assert_eq!(failure.run_id(), "run-17");
    assert_eq!(bytes, br#"{"code":"effect-uncertain","run_id":"run-17"}"#);
    assert_eq!(
        failure.canonical_json_hash(),
        "sha256:3a751d2bbfd752e219d547bfbc1de84bf3e69baebad45b268c622b4dea0c87d1"
    );
    assert_eq!(
        serde_json::from_slice::<EffectUncertainFailure>(&bytes).unwrap(),
        failure
    );

    for malformed in [
        br#"{"code":"unknown","run_id":"run-17"}"#.as_slice(),
        br#"{"code":"effect-uncertain"}"#.as_slice(),
        br#"{"code":"effect-uncertain","run_id":""}"#.as_slice(),
        br#"{"code":"effect-uncertain","run_id":"run-17","occurred":true}"#.as_slice(),
        br#"{"code":"effect-uncertain","run_id":17}"#.as_slice(),
    ] {
        assert!(serde_json::from_slice::<EffectUncertainFailure>(malformed).is_err());
    }

    let empty = EffectUncertainFailure::new("").unwrap_err();
    assert_eq!(empty.run_id(), "");
    assert_eq!(
        empty.to_string(),
        "effect-uncertain failure run_id must not be empty"
    );

    let whitespace = EffectUncertainFailure::new(" ").unwrap();
    assert_eq!(whitespace.run_id(), " ");
    assert_eq!(
        serde_json::from_str::<EffectUncertainFailure>(
            r#"{"code":"effect-uncertain","run_id":" "}"#,
        )
        .unwrap(),
        whitespace
    );
}

// `canonical_status_ddl_mirrors_include_effect_uncertain_without_run_level_parked` lived here.
// It pinned the run-status CHECK as a substring of two checked-in files and went red the moment
// one of them — `deploy/sql/postgres-init.sql`, which declares no `runs` table at all — stopped
// carrying that text: a formatting-shaped false red proving neither the installed constraint nor
// its vocabulary. Its successor is the server-answer arm of
// `run_state_schema_applies_and_isolates_on_postgres` below, which asks the installed constraint
// itself.

// ---- live-apply gate (optional) --------------------------------------------

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

/// Apply `deploy/sql/run-state.sql` to a test database and assert the tenant RLS
/// isolates rows, the idempotency index dedupes, and the INSTALLED run-status CHECK
/// admits exactly the crate's [`RunStatus`] vocabulary while refusing the retired
/// run-level `parked`. The installed `fail_kind` CHECK admits exactly
/// [`FailKind::ALL`], and the server answers each [`RUN_RECORD_CHECKS`] arm. The test runs as the superuser (the harness prepares an App
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
    let app_role = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: "t1",
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("derive the live run-state App generation");
    let prepare_app = provision_sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        &database,
        &app_role,
        "run-state-store-test-password",
        "2099-01-01T00:00:00Z",
    );
    let retire_app = provision_sql::retire_workload_generation_sql(
        WorkloadRoleFamily::App,
        &database,
        &app_role,
    );
    let drain_app = provision_sql::terminate_workload_generation_sessions_sql(&app_role);

    let mut script = String::new();
    // Prepare the production-shaped App identity and a fresh schema.
    writeln!(
        script,
        "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{app_role}') THEN \
           EXECUTE format('DROP OWNED BY %I', '{app_role}'); \
           EXECUTE format('DROP ROLE %I', '{app_role}'); \
         END IF; END $$;\n\
         DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_scenario_author') THEN \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $$;\n\
         {prepare_app}\n\
         DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         CREATE SCHEMA catalog;\n\
         CREATE TABLE catalog.effective_releases (\n\
           tenant_id text NOT NULL, effective_release_id int NOT NULL,\n\
           environment text NOT NULL, verified_publisher_principal text NOT NULL,\n\
           PRIMARY KEY (tenant_id, effective_release_id)\n\
         );\n\
         INSERT INTO catalog.effective_releases VALUES\n\
           ('t1',1,'test','test-publisher'), ('t2',1,'test','test-publisher'),\n\
           ('t3',1,'test','test-publisher');"
    )
    .expect("writing to a String cannot fail");
    script.push_str(&ddl);
    script.push('\n');
    script.push_str(
        "INSERT INTO wamn_run.environment_policies \
           (tenant_id, expected_environment, durability_class) VALUES \
           ('t1', 'test', 'standard'), \
           ('t2', 'test', 'standard'), \
           ('t3', 'test', 'standard');\n",
    );
    // Seed two tenants as the superuser (bypasses RLS): each has one run.
    script.push_str(
        "INSERT INTO wamn_run.runs (\
           tenant_id, run_id, flow_id, flow_version, package_id, effective_release_id, environment,\
           wiring_id, wiring_version, status, idempotency_key\
         ) VALUES\
           ('t1','run-a','f',1,'run-state-fixture',1,'test',\
            'fixture-wiring',1,'running','k-a'),\
           ('t2','run-b','f',1,'run-state-fixture',1,'test',\
            'fixture-wiring',1,'running','k-b');\n",
    );
    // The generation's `current_user` derives tenant t1 and sees only that
    // tenant's run without trusting a settable claim.
    writeln!(
        script,
        "BEGIN;\n\
         SET LOCAL ROLE {app_role};\n\
         SET LOCAL search_path TO wamn_run;\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM runs) = 1, 't1 sees only its run'; END $$;\n\
         COMMIT;"
    )
    .expect("writing to a String cannot fail");
    // Intentional refusal fixture: the stable ACL role carries no tenant key,
    // so even a superuser's SET ROLE probe sees zero rows.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_run;\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM runs) = 0, 'stable role has no tenant'; END $$;\n\
         COMMIT;\n",
    );
    // The idempotency index rejects a duplicate (tenant, key); a different tenant
    // may reuse the same key.
    script.push_str(
        "DO $$ BEGIN \
           BEGIN \
             INSERT INTO wamn_run.runs (\
               tenant_id, run_id, flow_id, flow_version, package_id, effective_release_id, environment,\
               wiring_id, wiring_version, idempotency_key\
             ) VALUES ('t1','run-a2','f',1,'run-state-fixture',1,'test',\
               'fixture-wiring',1,'k-a'); \
             ASSERT false, 'duplicate idempotency key must be rejected'; \
           EXCEPTION WHEN unique_violation THEN NULL; END; \
         END $$;\n\
         INSERT INTO wamn_run.runs (\
           tenant_id, run_id, flow_id, flow_version, package_id, effective_release_id, environment,\
           wiring_id, wiring_version, idempotency_key\
         ) VALUES ('t3','run-c','f',1,'run-state-fixture',1,'test',\
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
                 tenant_id, run_id, flow_id, flow_version, package_id, effective_release_id,\
                 environment, wiring_id, wiring_version, status, idempotency_key\
               ) VALUES ('t3', 'status-probe-' || ordinal, 'f', 1, 'run-state-fixture', 1,\
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
    // The installed fail_kind CHECK, asked for its literal set like the status one.
    script.push_str(
        "SELECT 'fail-kind-installed ' || string_agg(DISTINCT literal, ' ' ORDER BY literal)\n\
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
             AND col.attname = 'fail_kind'\n\
         ) AS installed;\n",
    );
    script.push_str(RUN_RECORD_CHECKS);
    // The environment policy floor: the App generation reads its own tenant's
    // row only and cannot write one.
    writeln!(
        script,
        "BEGIN;\n\
         SET LOCAL ROLE {app_role};\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM wamn_run.environment_policies) = 1,\n\
             'the App generation reads only its own environment policy';\n\
           ASSERT (SELECT tenant_id FROM wamn_run.environment_policies) = 't1',\n\
             'the App generation reads its own tenant''s policy';\n\
           BEGIN\n\
             INSERT INTO wamn_run.environment_policies\n\
               (tenant_id, expected_environment, durability_class) VALUES ('t4', 'test', 'standard');\n\
             ASSERT false, 'the App generation must not write an environment policy';\n\
           EXCEPTION WHEN insufficient_privilege THEN NULL; END;\n\
         END $$;\n\
         COMMIT;"
    )
    .expect("writing to a String cannot fail");
    // Terminal run history remains deletable.
    script.push_str(
        "UPDATE wamn_run.runs SET status='completed' \
           WHERE tenant_id='t1' AND run_id='run-a';\n\
         DELETE FROM wamn_run.runs WHERE tenant_id='t1' AND run_id='run-a';\n",
    );
    writeln!(
        script,
        "DROP SCHEMA wamn_run CASCADE; DROP SCHEMA catalog CASCADE;\n\
         {retire_app}\n\
         {drain_app}\n\
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
    let mut fail_kinds = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("fail-kind-installed ") {
            fail_kinds = Some(rest.to_string());
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

    let mut fail_vocabulary = FailKind::ALL.map(FailKind::as_sql);
    fail_vocabulary.sort_unstable();
    assert_eq!(
        fail_kinds.as_deref(),
        Some(fail_vocabulary.join(" ").as_str()),
        "the installed runs fail_kind CHECK admits a different vocabulary than FailKind::ALL"
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
                             'effective_release_id', 'manifest_digest'))
       = 'effective_release_id:integer event_depth:integer event_root_run_id:text '
         'event_source_run_id:text manifest_digest:text',
    'runs carries the causation and release record carriers';
  ASSERT (SELECT attnotnull FROM pg_attribute
           WHERE attrelid = 'wamn_run.runs'::regclass AND attname = 'effective_release_id'),
    'every run pins its effective release';
END $$;

-- One effect attempt per frame node occurrence.
DO $$ BEGIN
  ASSERT EXISTS (
    SELECT FROM pg_constraint AS con
     WHERE con.conrelid = 'wamn_run.effect_attempts'::regclass AND con.contype = 'u'
       AND (SELECT array_agg(att.attname::text ORDER BY key.ordinality)
              FROM unnest(con.conkey) WITH ORDINALITY AS key(attnum, ordinality)
              JOIN pg_attribute AS att
                ON att.attrelid = con.conrelid AND att.attnum = key.attnum)
           = ARRAY['tenant_id', 'run_id', 'frame_id', 'local_node_id', 'occurrence']
  ), 'effect_attempts is unique per tenant, run, frame, node and occurrence';
END $$;

-- An admitted run defaults to no capture and the standard class, and refuses
-- a class or capture outside its vocabulary.
INSERT INTO wamn_run.runs (
  tenant_id, run_id, flow_id, flow_version, package_id, effective_release_id, environment,
  wiring_id, wiring_version, status, idempotency_key
) VALUES ('t3', 'record-probe', 'f', 1, 'run-state-fixture', 1, 'test',
          'fixture-wiring', 1, 'running', 'record-probe');
DO $$ BEGIN
  ASSERT (SELECT capture_mode || '/' || durability_class FROM wamn_run.runs
           WHERE tenant_id = 't3' AND run_id = 'record-probe') = 'off/standard',
    'an admitted run defaults to capture off and the standard class';
END $$;
DO $$
DECLARE refused text;
BEGIN
  FOREACH refused IN ARRAY ARRAY[
    'durability_class = ''bogus''',
    'capture_mode = ''bogus''',
    'capture_mode = ''full'''
  ] LOOP
    BEGIN
      EXECUTE format(
        'INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, package_id, '
        'effective_release_id, environment, wiring_id, wiring_version, idempotency_key, '
        'capture_mode, durability_class) VALUES (''t3'', ''record-refused'', ''f'', 1, '
        '''run-state-fixture'', 1, ''test'', ''fixture-wiring'', 1, ''record-refused'', %s, %s)',
        CASE WHEN refused LIKE 'capture_mode%' THEN split_part(refused, ' = ', 2) ELSE '''off''' END,
        CASE WHEN refused LIKE 'durability_class%' THEN split_part(refused, ' = ', 2) ELSE '''standard''' END);
      ASSERT false, format('a run with %s must be refused', refused);
    EXCEPTION WHEN check_violation THEN NULL; END;
  END LOOP;
END $$;

-- Every admission pin is immutable after insertion.
DO $$
DECLARE assignment text; refusal text;
BEGIN
  FOREACH assignment IN ARRAY ARRAY[
    'flow_id = ''g''', 'flow_version = 2', 'package_id = ''other''',
    'effective_release_id = 2', 'environment = ''other''', 'capture_mode = ''full''',
    'durability_class = ''durable''', 'wiring_id = ''other''', 'wiring_version = 2',
    'wiring_hash = ''other''', 'binding_world_json = ''{}''::jsonb',
    'service_principal_id = ''00000000-0000-4000-8000-000000000001''::uuid'
  ] LOOP
    refusal := NULL;
    BEGIN
      EXECUTE 'UPDATE wamn_run.runs SET ' || assignment
        || ' WHERE tenant_id = ''t3'' AND run_id = ''record-probe''';
    EXCEPTION WHEN object_not_in_prerequisite_state THEN refusal := SQLERRM; END;
    ASSERT refusal = 'run-admission-pin-immutable',
      format('changing %s must be refused as run-admission-pin-immutable, got %s',
             assignment, coalesce(refusal, 'no refusal'));
  END LOOP;
END $$;

-- Only terminal history is deletable: effect-uncertain stays, and each
-- terminal status goes. The guard runs with its caller's rights and PUBLIC
-- cannot call it.
UPDATE wamn_run.runs SET status = 'effect-uncertain'
 WHERE tenant_id = 't3' AND run_id = 'record-probe';
DO $$
DECLARE refusal text;
BEGIN
  BEGIN
    DELETE FROM wamn_run.runs WHERE tenant_id = 't3' AND run_id = 'record-probe';
  EXCEPTION WHEN object_not_in_prerequisite_state THEN refusal := SQLERRM; END;
  ASSERT refusal = 'run-delete-nonterminal',
    format('an effect-uncertain run must not be deletable, got %s', coalesce(refusal, 'a delete'));
END $$;
DO $$
DECLARE terminal text;
BEGIN
  FOREACH terminal IN ARRAY ARRAY['completed', 'failed', 'infrastructure-failure'] LOOP
    INSERT INTO wamn_run.runs (
      tenant_id, run_id, flow_id, flow_version, package_id, effective_release_id, environment,
      wiring_id, wiring_version, status, idempotency_key
    ) VALUES ('t3', 'terminal-' || terminal, 'f', 1, 'run-state-fixture', 1, 'test',
              'fixture-wiring', 1, terminal, 'terminal-' || terminal);
    DELETE FROM wamn_run.runs WHERE tenant_id = 't3' AND run_id = 'terminal-' || terminal;
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

-- The environment policy relation: one row per tenant, forced row security,
-- a platform policy that reads every row, and an index on the tenant key.
DO $$ BEGIN
  BEGIN
    INSERT INTO wamn_run.environment_policies (tenant_id, expected_environment, durability_class)
      VALUES ('t1', 'test', 'durable');
    ASSERT false, 'a second environment policy for one tenant must be refused';
  EXCEPTION WHEN unique_violation THEN NULL; END;
  ASSERT (SELECT relrowsecurity AND relforcerowsecurity FROM pg_class
           WHERE oid = 'wamn_run.environment_policies'::regclass),
    'environment_policies forces row security';
  ASSERT EXISTS (
    SELECT FROM pg_index
     WHERE indrelid = 'wamn_run.environment_policies'::regclass
       AND pg_get_indexdef(indexrelid) LIKE '%tenant_key(tenant_id)%'
  ), 'environment_policies indexes its tenant key';
  ASSERT EXISTS (
    SELECT FROM pg_policy
     WHERE polrelid = 'wamn_run.environment_policies'::regclass
       AND polpermissive AND polcmd = 'r'
       AND polroles = ARRAY['wamn_platform'::regrole::oid]
       AND pg_get_expr(polqual, polrelid) = 'true'
  ), 'the platform family reads every environment policy';
END $$;
";
