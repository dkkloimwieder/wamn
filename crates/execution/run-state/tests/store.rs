//! Persisted status vocabulary and canonical run-state DDL tests.

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

#[test]
fn postgres_fixture_has_no_retired_flow_runs_checkpoint_table() {
    let ddl = include_str!("../../../../deploy/sql/postgres-init.sql");

    assert!(!ddl.contains("CREATE TABLE s3.flow_runs"));
    assert!(!ddl.contains("flow_runs_tenant"));
}

// ---- deploy/sql/run-state.sql drift guard --------------------------------------

#[test]
fn run_state_sql_matches_the_model() {
    let sql = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../deploy/sql/run-state.sql"
    ))
    .expect("read deploy/sql/run-state.sql");

    // Run history and its tenant floor.
    assert!(sql.contains("CREATE TABLE wamn_run.runs"));
    assert!(sql.contains("FORCE ROW LEVEL SECURITY"));
    assert!(sql.contains("current_setting('app.tenant', true)"));
    // Retired rerun lineage is absent; trusted event causation and effect frame keys remain.
    assert!(!sql.contains("\n    replay_of       text"));
    assert!(!sql.contains("\n    root_run_id     text"));
    assert!(!sql.contains("CREATE INDEX runs_root "));
    assert!(sql.contains("event_source_run_id text"));
    assert!(sql.contains("event_root_run_id text"));
    assert!(sql.contains("event_depth      int"));
    for frame_column in [
        "frame_id",
        "parent_frame_id",
        "call_site_id",
        "current_plan_hash",
        "local_node_id",
    ] {
        assert!(
            sql.contains(frame_column),
            "run-state.sql missing frame column {frame_column}"
        );
    }
    for effect_fact in [
        "root_plan_hash",
        "current_plan_hash",
        "frame_id",
        "local_node_id",
        "source_artifact_hash",
        "requirement_name text NOT NULL",
        "UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence)",
    ] {
        assert!(
            sql.contains(effect_fact),
            "run-state.sql missing effect attempt fact {effect_fact}"
        );
    }
    assert!(sql.contains("runs_idempotency"));
    // The run-owned 9.6 admission fact.
    assert!(sql.contains("capture_mode    text NOT NULL DEFAULT 'off'"));
    assert!(sql.contains(
        "capture_mode <> 'full' OR trigger_source IS NOT DISTINCT FROM 'scenario-draft'"
    ));
    assert!(sql.contains("OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode"));
    // The durability-class carrier (wamn-0h0g.20.1): CHECK-frozen literals and
    // a fail-closed policy projection under the same admission pin. Producer
    // statements still cannot name the carrier; the local policy trigger is
    // its only admission-time writer.
    assert!(sql.contains("durability_class text NOT NULL DEFAULT 'standard'"));
    assert!(sql.contains("CONSTRAINT runs_durability_class_check"));
    assert!(sql.contains("CHECK (durability_class IN ('standard', 'durable'))"));
    assert!(sql.contains("OR NEW.durability_class IS DISTINCT FROM OLD.durability_class"));
    assert!(sql.contains("CREATE TABLE wamn_run.environment_policies"));
    assert!(sql.contains("expected_environment text NOT NULL"));
    assert!(sql.contains("PRIMARY KEY (tenant_id)"));
    assert!(sql.contains("ALTER TABLE wamn_run.environment_policies FORCE ROW LEVEL SECURITY"));
    assert!(sql.contains("CREATE POLICY environment_policies_tenant"));
    // Re-keyed onto `current_user` with the rest of the guest-reachable floor
    // (`wamn-0h0g.22.6.3`), and carrying the expression index without which the
    // derivation sequential-scans.
    // `wamn-0h0g.22.17` narrowed the floor to the guest and admitted the
    // platform families through one permissive arm. Both halves are asserted:
    // the floor alone would leave every platform principal reading ZERO ROWS
    // SILENTLY, which is the failure that bead exists to prevent.
    assert!(sql.contains(
        "FOR SELECT\nTO wamn_app\nUSING (wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key())"
    ));
    assert!(sql.contains(
        "CREATE POLICY environment_policies_platform ON wamn_run.environment_policies\n    AS PERMISSIVE FOR SELECT TO wamn_platform\n    USING (true)"
    ));
    assert!(sql.contains(
        "CREATE INDEX environment_policies_tkey\n    ON wamn_run.environment_policies ((wamn_authority.tenant_key(tenant_id)))"
    ));
    assert!(sql.contains("CREATE FUNCTION wamn_run.pin_run_durability_class()"));
    assert!(sql.contains("WHERE policy.tenant_id = NEW.tenant_id"));
    assert!(sql.contains("IF NOT FOUND THEN"));
    assert!(sql.contains("MESSAGE = 'environment-policy-not-converged'"));
    assert!(sql.contains("NEW.environment IS DISTINCT FROM projected_environment"));
    assert!(sql.contains("MESSAGE = 'environment-policy-environment-mismatch'"));
    assert!(!sql.contains("NEW.durability_class := COALESCE"));
    assert!(sql.contains("CREATE TRIGGER runs_pin_durability_class\nBEFORE INSERT"));
    assert!(sql.contains("GRANT SELECT ON TABLE wamn_run.environment_policies TO wamn_app"));
    assert!(!sql.contains("GRANT INSERT ON TABLE wamn_run.environment_policies TO wamn_app"));
    // RIDER 1 of the ruling: an unnamed column's transition arm silently never
    // fires, so the column-scoped trigger MUST name the class.
    assert!(sql.contains(
        "BEFORE UPDATE OF flow_id, flow_version, package_id, effective_release_id, environment,\n                 capture_mode, durability_class, wiring_id, wiring_version,\n                 wiring_hash, binding_world_json, manifest_digest"
    ));
    // The effective release is pinned at admission. The claim records only the
    // verified manifest digest, and every arm that reopens claimability clears
    // that digest — the classifier's pre-effect reclaim and the queue park
    // (wamn-0h0g.15.82). The guard is transition-constrained rather than
    // write-once: NULL -> value and value -> NULL are permitted, while value ->
    // value' never is (wamn-0h0g.15.55).
    assert!(sql.contains("    effective_release_id int NOT NULL,"));
    assert!(!sql.contains("    release_version int,"));
    assert!(sql.contains("    manifest_digest text,"));
    assert!(sql.contains("CONSTRAINT runs_release_record_check"));
    assert!(sql.contains(
        "IF OLD.manifest_digest IS NOT NULL THEN\n        IF NEW.manifest_digest IS NULL THEN"
    ));
    // The erasure arm cannot name its caller, so it proves nothing references
    // the digest being erased: still runnable and no effect attempt.
    assert!(sql.contains("IF NEW.status NOT IN ('dispatched', 'running')"));
    assert!(sql.contains("OR EXISTS (SELECT 1 FROM wamn_run.effect_attempts AS effect"));
    assert!(sql.contains("ELSIF NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest THEN"));
    assert!(sql.contains("MESSAGE = 'run-release-record-immutable'"));
    assert!(!sql.contains("GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.runs TO wamn_app"));
    assert!(sql.contains("GRANT SELECT, DELETE ON wamn_run.runs TO wamn_app;"));
    assert!(!sql.contains("payload_size  bigint"));
    assert!(!sql.contains("preview_head  text"));
    assert!(!sql.contains("redacted      boolean"));

    // Every status literal the CHECK constraints pin comes from the crate enums.
    for s in RunStatus::ALL {
        assert!(
            sql.contains(&format!("'{}'", s.as_sql())),
            "runs CHECK missing {}",
            s.as_sql()
        );
    }
    for k in FailKind::ALL {
        assert!(
            sql.contains(&format!("'{}'", k.as_sql())),
            "fail_kind CHECK missing {}",
            k.as_sql()
        );
    }

    let normalized = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized.contains(
            "fail_kind text CHECK (fail_kind IN ('terminal', 'retry-exhausted', 'invalid-input', \
             'runaway-budget', 'effect-uncertain', 'depth-budget', 'dispatch-budget', \
             'unresolvable-name', 'hash-invalid-bytes', 'foreign-revision', \
             'incompatible-contract', 'unbound-requirement')),"
        ),
        "runs.fail_kind CHECK must carry exactly the frozen vocabulary"
    );
}

#[test]
fn run_delete_is_guarded_by_the_exact_terminal_vocabulary() {
    let sql = include_str!("../../../../deploy/sql/run-state.sql");
    let (_, guard_and_rest) = sql
        .split_once("CREATE FUNCTION wamn_run.guard_terminal_run_delete()")
        .expect("run-state record declares the delete guard");
    let (guard, _) = guard_and_rest
        .split_once("REVOKE ALL ON FUNCTION wamn_run.guard_terminal_run_delete() FROM PUBLIC;")
        .expect("the ordinary trigger function is not publicly executable");

    assert!(
        guard.contains(
            "IF OLD.status NOT IN ('completed', 'failed', 'infrastructure-failure') THEN"
        )
    );
    assert!(guard.contains("ERRCODE = '55000'"));
    assert!(guard.contains("MESSAGE = 'run-delete-nonterminal'"));
    assert!(!guard.contains("effect-uncertain"));
    assert!(!guard.contains("SECURITY DEFINER"));
    assert!(sql.contains(
        "CREATE TRIGGER runs_terminal_delete_only\nBEFORE DELETE ON wamn_run.runs\nFOR EACH ROW EXECUTE FUNCTION wamn_run.guard_terminal_run_delete();"
    ));
}

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

/// Apply `deploy/sql/run-state.sql` to a throwaway Postgres and assert the tenant RLS
/// isolates rows, the idempotency index dedupes, and the INSTALLED run-status CHECK
/// admits exactly the crate's [`RunStatus`] vocabulary while refusing the retired
/// run-level `parked`. Gated on `WAMN_RUN_STORE_PG_URL` (a superuser URL — the harness
/// prepares an App generation); skips cleanly when unset.
#[test]
fn run_state_schema_applies_and_isolates_on_postgres() {
    /// The run-level status the queue park retired off the run row. The server must
    /// refuse it, not merely the file must omit it.
    const RETIRED_RUN_STATUS: &str = "parked";

    let Ok(url) = std::env::var("WAMN_RUN_STORE_PG_URL") else {
        eprintln!(
            "skipping run_state_schema_applies_and_isolates_on_postgres (set WAMN_RUN_STORE_PG_URL to run)"
        );
        return;
    };

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
        "run-state-store-proof-password",
        "2099-01-01T00:00:00Z",
    );
    let ensure_effect_writer = provision_sql::ensure_effect_writer_acl_role_sql();
    let retire_app = provision_sql::retire_workload_generation_sql(
        WorkloadRoleFamily::App,
        &database,
        &app_role,
    );
    let drain_app = provision_sql::terminate_workload_generation_sessions_sql(&app_role);

    let mut script = String::new();
    // Prepare the production-shaped App identity and a fresh schema.
    script.push_str(&format!(
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
         {ensure_effect_writer}\n\
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
           ('t3',1,'test','test-publisher');\n"
    ));
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
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE {app_role};\n\
         SET LOCAL search_path TO wamn_run;\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM runs) = 1, 't1 sees only its run'; END $$;\n\
         COMMIT;\n"
    ));
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
    script.push_str(&format!(
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
         ) AS installed;\n"
    ));
    // Terminal run history remains deletable.
    script.push_str(
        "UPDATE wamn_run.runs SET status='completed' \
           WHERE tenant_id='t1' AND run_id='run-a';\n\
         DELETE FROM wamn_run.runs WHERE tenant_id='t1' AND run_id='run-a';\n",
    );
    script.push_str(&format!(
        "DROP SCHEMA wamn_run CASCADE; DROP SCHEMA catalog CASCADE;\n\
         {retire_app}\n\
         {drain_app}\n\
         DROP ROLE \"{app_role}\";\n"
    ));

    use std::io::Write;
    use std::process::{Command as Proc, Stdio};
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
    for line in stdout.lines() {
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
}
