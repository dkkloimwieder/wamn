//! Persisted status vocabulary and canonical run-state DDL tests.

use serde_json::json;
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

#[test]
fn canonical_status_ddl_mirrors_include_effect_uncertain_without_run_level_parked() {
    let exact_check = r#"CHECK (status IN ('dispatched', 'running', 'completed', 'failed',
                          'infrastructure-failure', 'effect-uncertain'))"#;
    for ddl in [
        include_str!("../../../../deploy/sql/run-state.sql"),
        include_str!("../../../../deploy/sql/postgres-init.sql"),
    ] {
        assert!(ddl.contains(exact_check));
        assert!(!ddl.contains("'parked'"));
    }
}

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
    assert!(sql.contains("REFERENCES wamn_run.runs"));
    // Reserved 5.10 seams and the run-owned 9.6 admission fact.
    for seam in ["input_ref", "output_ref", "output_size", "payload_hash"] {
        assert!(
            sql.contains(seam),
            "run-state.sql missing reserved seam {seam}"
        );
    }
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
    assert!(sql.contains("FOR SELECT\nUSING (tenant_id = NULLIF(current_setting('app.tenant'"));
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
        "BEFORE UPDATE OF catalog_id, catalog_version, environment, execution_bundle_hash, capture_mode,\n                 durability_class, release_version, manifest_digest"
    ));
    // The claim-time release record: NULL at admission, written by the claiming
    // worker, and cleared again by EVERY arm that reopens claimability — the
    // classifier's pre-effect reclaim and the queue park (wamn-0h0g.15.82). The
    // guard is transition-constrained rather than write-once — NULL -> value and
    // value -> NULL are permitted, value -> value' never is (wamn-0h0g.15.55).
    assert!(sql.contains("    release_version int,"));
    assert!(sql.contains("    manifest_digest text,"));
    assert!(sql.contains("CONSTRAINT runs_release_record_check"));
    assert!(sql.contains(
        "IF OLD.release_version IS NOT NULL OR OLD.manifest_digest IS NOT NULL THEN\n        IF NEW.release_version IS NULL AND NEW.manifest_digest IS NULL THEN"
    ));
    // The erasure arm cannot name its caller, so it proves nothing references
    // the pair being erased: still runnable and no effect attempt.
    assert!(sql.contains("IF NEW.status NOT IN ('dispatched', 'running')"));
    assert!(sql.contains("OR EXISTS (SELECT 1 FROM wamn_run.effect_attempts AS effect"));
    assert!(sql.contains(
        "ELSIF NEW.release_version IS DISTINCT FROM OLD.release_version\n           OR NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest THEN"
    ));
    assert!(sql.contains("MESSAGE = 'run-release-record-immutable'"));
    assert!(!sql.contains("GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.runs TO wamn_app"));
    let runs_grants = sql
        .split_once("GRANT SELECT, DELETE ON wamn_run.runs TO wamn_app;")
        .expect("runs read/delete grant is explicit")
        .1
        .split_once("GRANT SELECT ON wamn_run.runs TO wamn_scenario_author;")
        .expect("runs author read grant follows app column grants")
        .0;
    assert!(runs_grants.contains("GRANT INSERT ("));
    assert!(runs_grants.contains("), UPDATE ("));
    assert!(!runs_grants.contains("capture_mode"));
    // The class carrier is withheld from the guest-visible role in BOTH sets:
    // an admission that could choose its own durability class would buy the
    // premium crash floor without a policy saying so.
    assert!(!runs_grants.contains("durability_class"));
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

/// Apply `deploy/sql/run-state.sql` to a throwaway Postgres and assert the tenant RLS
/// isolates rows and the idempotency index dedupes. Gated on
/// `WAMN_RUN_STORE_PG_URL` (a superuser URL — the harness provisions `wamn_app`);
/// skips cleanly when unset. Mirrors the wamn-schema-compiler / wamn-schema-compiler / wamn-schema-compiler gates.
#[test]
fn run_state_schema_applies_and_isolates_on_postgres() {
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

    let mut script = String::new();
    // Provision wamn_app (NOSUPERUSER/NOBYPASSRLS, like production) + a fresh schema.
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         CREATE SCHEMA catalog;\n\
         CREATE TABLE catalog.release_manifests (\n\
           tenant_id text NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL,\n\
           PRIMARY KEY (tenant_id, catalog_id, catalog_version)\n\
         );\n\
         CREATE TABLE catalog.execution_bundles (\n\
           tenant_id text NOT NULL, execution_bundle_hash text NOT NULL,\n\
           PRIMARY KEY (tenant_id, execution_bundle_hash)\n\
         );\n\
         INSERT INTO catalog.release_manifests VALUES\n\
           ('t1','run-state-fixture',1), ('t2','run-state-fixture',1),\n\
           ('t3','run-state-fixture',1);\n\
         INSERT INTO catalog.execution_bundles VALUES\n\
           ('t1','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'),\n\
           ('t2','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'),\n\
           ('t3','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a');\n",
    );
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
           tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, environment,\
           execution_bundle_hash, status, idempotency_key\
         ) VALUES\
           ('t1','run-a','f',1,'run-state-fixture',1,'test',\
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',\
            'running','k-a'),\
           ('t2','run-b','f',1,'run-state-fixture',1,'test',\
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',\
            'running','k-b');\n",
    );
    // As wamn_app under tenant t1: sees only t1's run.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_run;\n\
         SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM runs) = 1, 't1 sees only its run'; END $$;\n\
         COMMIT;\n",
    );
    // No claim -> zero rows (safe default).
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_run;\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM runs) = 0, 'no tenant claim denies all'; END $$;\n\
         COMMIT;\n",
    );
    // The idempotency index rejects a duplicate (tenant, key); a different tenant
    // may reuse the same key.
    script.push_str(
        "DO $$ BEGIN \
           BEGIN \
             INSERT INTO wamn_run.runs (\
               tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, environment,\
               execution_bundle_hash, idempotency_key\
             ) VALUES ('t1','run-a2','f',1,'run-state-fixture',1,'test',\
               'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',\
               'k-a'); \
             ASSERT false, 'duplicate idempotency key must be rejected'; \
           EXCEPTION WHEN unique_violation THEN NULL; END; \
         END $$;\n\
         INSERT INTO wamn_run.runs (\
           tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, environment,\
           execution_bundle_hash, idempotency_key\
         ) VALUES ('t3','run-c','f',1,'run-state-fixture',1,'test',\
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',\
           'k-a');\n",
    );
    // Terminal run history remains deletable.
    script.push_str(
        "UPDATE wamn_run.runs SET status='completed' \
           WHERE tenant_id='t1' AND run_id='run-a';\n\
         DELETE FROM wamn_run.runs WHERE tenant_id='t1' AND run_id='run-a';\n",
    );
    script.push_str("DROP SCHEMA wamn_run CASCADE; DROP SCHEMA catalog CASCADE;\n");

    use std::io::Write;
    use std::process::{Command as Proc, Stdio};
    let mut child = Proc::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
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
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}
