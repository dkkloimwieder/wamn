//! wamn-run-store tests — the run-state model, branch-aware replay
//! reconstruction, and partial-re-run planning, all pure (no cluster, no DB).
//! The live-apply test at the end applies `deploy/sql/run-state.sql` to a throwaway
//! Postgres and asserts RLS + the idempotency keys; it is gated on
//! `WAMN_RUN_STORE_PG_URL` and skips cleanly when unset (mirrors wamn-ddl/rls/seed).

use serde_json::{Value, json};
use wamn_flow::Flow;
use wamn_run_store::{
    FailKind, NodeErrorKind, NodeRunRecord, NodeRunStatus, ReconstructError, RerunError, RunRecord,
    RunStatus, plan_partial_rerun, plan_replay, reconstruct,
};
use wamn_runner::{NodeOutcome, Plan, ResumeError, Step};

fn flow(json_str: &str) -> Flow {
    Flow::from_json(json_str).expect("fixture flow parses")
}

/// A 4-node linear flow a -> b -> c -> d.
fn linear4() -> Flow {
    flow(
        r#"{"schema-version":"0.1","flow-id":"lin4","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"echo"},{"id":"b","type":"echo"},
                     {"id":"c","type":"echo"},{"id":"d","type":"echo"}],
            "edges":[{"from":"a","to":"b"},{"from":"b","to":"c"},{"from":"c","to":"d"}]}"#,
    )
}

/// A conditional branching into two independent two-node subtrees.
fn branchy() -> Flow {
    flow(
        r#"{"schema-version":"0.1","flow-id":"brc","version":1,
            "trigger":{"type":"manual"},"entry":"cond",
            "nodes":[{"id":"cond","type":"conditional"},
                     {"id":"y1","type":"pg-write"},{"id":"y2","type":"respond"},
                     {"id":"n1","type":"pg-write"},{"id":"n2","type":"respond"}],
            "edges":[{"from":"cond","from-port":"true","to":"y1"},
                     {"from":"y1","to":"y2"},
                     {"from":"cond","from-port":"false","to":"n1"},
                     {"from":"n1","to":"n2"}]}"#,
    )
}

/// Drive a reconstructed/seeded state to completion, collecting the ids of the
/// nodes actually dispatched (echo semantics: each emits `{"at": node}`).
fn drive_collect(plan: &Plan, st: &mut wamn_runner::RunState) -> Vec<String> {
    let mut seen = Vec::new();
    plan.drive(
        st,
        || 0,
        |_, _| {},
        |d| {
            seen.push(d.node.clone());
            NodeOutcome::ok(json!({ "at": d.node }))
        },
    );
    seen
}

// ---- reconstruction --------------------------------------------------------

#[test]
fn reconstruct_linear_resumes_at_the_killed_node() {
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    // Killed after b: a and b persisted, c/d not.
    let run = RunRecord::new("r1", "lin4", 1, json!({ "trig": 1 }));
    let node_runs = [
        NodeRunRecord::success("r1", "a", 0, "main", json!({ "at": "a" })),
        NodeRunRecord::success("r1", "b", 1, "main", json!({ "at": "b" })),
    ];
    let mut st = reconstruct(&plan, &run, &node_runs).unwrap();
    assert_eq!(st.step_seq(), 2);
    assert_eq!(drive_collect(&plan, &mut st), ["c", "d"]); // a/b not re-run
}

#[test]
fn reconstruct_ignores_running_and_parked_rows() {
    // A `running` row (in-flight when killed) and a `parked` row are outstanding,
    // not completed — reconstruction must NOT replay them.
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    let run = RunRecord::new("r1", "lin4", 1, json!({}));
    let node_runs = [
        NodeRunRecord::success("r1", "a", 0, "main", json!({ "at": "a" })),
        NodeRunRecord {
            status: NodeRunStatus::Running, // b was in flight; no emission
            output: None,
            output_port: None,
            ..NodeRunRecord::success("r1", "b", 1, "main", Value::Null)
        },
    ];
    let mut st = reconstruct(&plan, &run, &node_runs).unwrap();
    assert_eq!(st.step_seq(), 1); // only a folded
    assert_eq!(drive_collect(&plan, &mut st), ["b", "c", "d"]); // b re-dispatched
}

#[test]
fn reconstruct_is_branch_aware_kill_mid_branch_completes_the_right_branch() {
    // THE branch-aware kill-mid-branch -> resume proof at the store level:
    // cond took "false" and n1 committed, then the run was killed. Reconstruction
    // must place the frontier at n2 (the taken branch) and never touch y1/y2.
    let f = branchy();
    let plan = Plan::compile(&f).unwrap();
    let run = RunRecord::new("r1", "brc", 1, json!({}));
    let node_runs = [
        NodeRunRecord::success("r1", "cond", 0, "false", json!({ "picked": "false" })),
        NodeRunRecord::success("r1", "n1", 1, "main", json!({ "wrote": "n" })),
    ];
    let mut st = reconstruct(&plan, &run, &node_runs).unwrap();
    assert_eq!(drive_collect(&plan, &mut st), ["n2"]); // y1/y2 never run
}

#[test]
fn reconstruct_replays_an_error_routed_node() {
    // An error-routed node is persisted as a completed `error` row whose
    // output_port is `error` and whose output is the `{"error": …}` payload.
    let f = flow(
        r#"{"schema-version":"0.1","flow-id":"err","version":1,
            "trigger":{"type":"manual"},"entry":"a",
            "nodes":[{"id":"a","type":"http-call"},{"id":"h","type":"notify"},
                     {"id":"ok","type":"respond"}],
            "edges":[{"from":"a","to":"ok"},{"from":"a","from-port":"error","to":"h"}]}"#,
    );
    let plan = Plan::compile(&f).unwrap();
    let run = RunRecord::new("r1", "err", 1, json!({}));
    let node_runs = [NodeRunRecord {
        status: NodeRunStatus::Error,
        output_port: Some("error".into()),
        output: Some(json!({ "error": { "message": "boom" } })),
        error_kind: Some(NodeErrorKind::Terminal),
        ..NodeRunRecord::success("r1", "a", 0, "error", Value::Null)
    }];
    let mut st = reconstruct(&plan, &run, &node_runs).unwrap();
    assert_eq!(drive_collect(&plan, &mut st), ["h"]); // error branch, not "ok"
}

#[test]
fn reconstruct_capture_off_run_is_not_replayable() {
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    let run = RunRecord::new("r1", "lin4", 1, json!({}));
    // A completed success row with no captured output (9.6 capture off).
    let node_runs = [NodeRunRecord {
        output: None,
        ..NodeRunRecord::success("r1", "a", 0, "main", Value::Null)
    }];
    let err = reconstruct(&plan, &run, &node_runs).unwrap_err();
    assert_eq!(err, ReconstructError::CaptureOff { node: "a".into() });
}

#[test]
fn reconstruct_detects_history_drift() {
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    let run = RunRecord::new("r1", "lin4", 1, json!({}));
    // First recorded step names "b", but the flow dispatches "a" first.
    let node_runs = [NodeRunRecord::success("r1", "b", 0, "main", json!({}))];
    let err = reconstruct(&plan, &run, &node_runs).unwrap_err();
    assert_eq!(
        err,
        ReconstructError::Resume(ResumeError::Mismatch {
            recorded: "b".into(),
            dispatched: "a".into()
        })
    );
}

#[test]
fn reconstruct_sorts_by_seq_not_row_order() {
    // Rows arrive out of order; reconstruction sorts by `seq` before replaying.
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    let run = RunRecord::new("r1", "lin4", 1, json!({}));
    let node_runs = [
        NodeRunRecord::success("r1", "b", 1, "main", json!({ "at": "b" })),
        NodeRunRecord::success("r1", "a", 0, "main", json!({ "at": "a" })),
    ];
    let mut st = reconstruct(&plan, &run, &node_runs).unwrap();
    assert_eq!(drive_collect(&plan, &mut st), ["c", "d"]);
}

// ---- replay & partial re-run ----------------------------------------------

#[test]
fn plan_replay_mints_a_lineage_linked_run() {
    let orig = RunRecord::new("orig", "lin4", 3, json!({ "trig": "x" }));
    let replay = plan_replay(&orig, "replay-1").unwrap();
    assert_eq!(replay.run_id, "replay-1");
    assert_eq!(replay.replay_of.as_deref(), Some("orig"));
    assert_eq!(replay.root_run_id.as_deref(), Some("orig")); // original is its own root
    assert_eq!(replay.input, Some(json!({ "trig": "x" }))); // re-runs the same trigger
    assert_eq!(replay.status, RunStatus::Running);
    assert_eq!(replay.flow_version, 3);
    assert_eq!(replay.trigger_source.as_deref(), Some("replay"));
    assert!(replay.idempotency_key.is_none()); // a distinct execution
}

#[test]
fn plan_replay_of_a_replay_keeps_the_original_root() {
    let orig = RunRecord::new("orig", "lin4", 1, json!({ "n": 1 }));
    let first = plan_replay(&orig, "replay-1").unwrap();
    let second = plan_replay(&first, "replay-2").unwrap();
    assert_eq!(second.replay_of.as_deref(), Some("replay-1")); // immediate parent
    assert_eq!(second.root_run_id.as_deref(), Some("orig")); // chain root preserved
}

#[test]
fn plan_replay_requires_captured_input() {
    let mut orig = RunRecord::new("orig", "lin4", 1, json!({}));
    orig.input = None; // capture off
    let err = plan_replay(&orig, "replay-1").unwrap_err();
    assert_eq!(
        err,
        RerunError::InputNotCaptured {
            node: "(trigger)".into()
        }
    );
}

#[test]
fn partial_rerun_seeds_from_the_failed_nodes_captured_input() {
    let f = linear4();
    let plan = Plan::compile(&f).unwrap();
    let orig = RunRecord::new("orig", "lin4", 1, json!({ "trig": 1 }));
    // c failed; its captured input is recorded on the node-run.
    let node_runs = [
        NodeRunRecord::success("orig", "a", 0, "main", json!({ "at": "a" })),
        NodeRunRecord::success("orig", "b", 1, "main", json!({ "at": "b" })),
        NodeRunRecord {
            status: NodeRunStatus::Error,
            output: Some(json!({ "error": { "message": "transient" } })),
            output_port: Some("error".into()),
            input: Some(json!({ "captured": "c-input" })),
            error_kind: Some(NodeErrorKind::Retryable),
            ..NodeRunRecord::success("orig", "c", 2, "error", Value::Null)
        },
    ];
    let pr = plan_partial_rerun(&orig, &node_runs, "c", 0, "rerun-1").unwrap();
    assert_eq!(pr.seed_node, "c");
    assert_eq!(pr.seed_input, json!({ "captured": "c-input" }));
    assert_eq!(pr.run.replay_of.as_deref(), Some("orig"));
    assert_eq!(pr.run.root_run_id.as_deref(), Some("orig"));
    assert_eq!(pr.run.trigger_source.as_deref(), Some("partial-rerun"));

    // Driving the seeded run walks ONLY the downstream subtree (c, d) — a and b,
    // whose effects already committed, are not re-run.
    let mut st = plan
        .seed_at(&pr.run.run_id, &pr.seed_node, pr.seed_input.clone())
        .unwrap();
    // c sees its captured input.
    match plan.next(&mut st, 0) {
        Step::Dispatch(d) => {
            assert_eq!(d.node, "c");
            assert_eq!(d.payload, json!({ "captured": "c-input" }));
            plan.apply(&mut st, &d, NodeOutcome::ok(json!({ "at": "c" })), 0);
        }
        other => panic!("expected dispatch of c, got {other:?}"),
    }
    assert_eq!(drive_collect(&plan, &mut st), ["d"]);
}

#[test]
fn partial_rerun_unknown_node_run_is_rejected() {
    let orig = RunRecord::new("orig", "lin4", 1, json!({}));
    let node_runs = [NodeRunRecord::success("orig", "a", 0, "main", json!({}))];
    let err = plan_partial_rerun(&orig, &node_runs, "zzz", 0, "rerun-1").unwrap_err();
    assert_eq!(
        err,
        RerunError::NoSuchNodeRun {
            node: "zzz".into(),
            occurrence: 0
        }
    );
}

#[test]
fn partial_rerun_requires_captured_input() {
    let orig = RunRecord::new("orig", "lin4", 1, json!({}));
    // The node ran but its input was not captured (9.6 capture off).
    let node_runs = [NodeRunRecord::success(
        "orig",
        "c",
        2,
        "main",
        json!({ "at": "c" }),
    )];
    let err = plan_partial_rerun(&orig, &node_runs, "c", 0, "rerun-1").unwrap_err();
    assert_eq!(err, RerunError::InputNotCaptured { node: "c".into() });
}

// ---- status vocabularies ---------------------------------------------------

#[test]
fn status_sql_literals_round_trip() {
    for s in RunStatus::ALL {
        assert_eq!(RunStatus::from_sql(s.as_sql()), Some(s));
    }
    for s in NodeRunStatus::ALL {
        assert_eq!(NodeRunStatus::from_sql(s.as_sql()), Some(s));
    }
    for k in FailKind::ALL {
        assert_eq!(FailKind::from_sql(k.as_sql()), Some(k));
    }
    for k in NodeErrorKind::ALL {
        assert_eq!(NodeErrorKind::from_sql(k.as_sql()), Some(k));
    }
    assert_eq!(RunStatus::from_sql("nope"), None);
    // Spot-check the wire literals the DDL CHECK constraints pin.
    assert_eq!(
        RunStatus::InfrastructureFailure.as_sql(),
        "infrastructure-failure"
    );
    assert_eq!(NodeErrorKind::RateLimited.as_sql(), "rate-limited");
    assert_eq!(FailKind::RetryExhausted.as_sql(), "retry-exhausted");
    assert_eq!(FailKind::RunawayBudget.as_sql(), "runaway-budget");
}

#[test]
fn status_maps_from_the_engine_taxonomy() {
    assert_eq!(
        RunStatus::from(wamn_runner::RunStatus::Completed),
        RunStatus::Completed
    );
    assert_eq!(
        RunStatus::from(wamn_runner::RunStatus::Cancelled),
        RunStatus::Cancelled
    );
    assert_eq!(
        FailKind::from(wamn_runner::FailKind::InvalidInput),
        FailKind::InvalidInput
    );
    assert_eq!(
        FailKind::from(wamn_runner::FailKind::RunawayBudget),
        FailKind::RunawayBudget
    );
    let detail = wamn_runner::ErrorDetail::msg("x");
    assert_eq!(
        NodeErrorKind::from(&wamn_runner::NodeError::Retryable(detail.clone())),
        NodeErrorKind::Retryable
    );
    assert_eq!(
        NodeErrorKind::from(&wamn_runner::NodeError::Cancelled),
        NodeErrorKind::Cancelled
    );
}

#[test]
fn records_round_trip_as_json() {
    let mut run = RunRecord::new("r1", "f", 2, json!({ "n": 1 }));
    run.status = RunStatus::Failed;
    run.fail_kind = Some(FailKind::Terminal);
    run.fail_node = Some("w".into());
    run.replay_of = Some("orig".into());
    run.root_run_id = Some("orig".into());
    let s = serde_json::to_string(&run).unwrap();
    assert_eq!(serde_json::from_str::<RunRecord>(&s).unwrap(), run);
    // kebab-case keys on the wire.
    assert!(s.contains("\"flow-version\":2"));
    assert!(s.contains("\"replay-of\":\"orig\""));
    assert!(s.contains("\"fail-kind\":\"terminal\""));

    let nr = NodeRunRecord {
        status: NodeRunStatus::Error,
        error_kind: Some(NodeErrorKind::RateLimited),
        input: Some(json!({ "in": true })),
        ..NodeRunRecord::success("r1", "w", 2, "error", json!({ "error": {} }))
    };
    let s2 = serde_json::to_string(&nr).unwrap();
    assert_eq!(serde_json::from_str::<NodeRunRecord>(&s2).unwrap(), nr);
    assert!(s2.contains("\"error-kind\":\"rate-limited\""));
}

// ---- deploy/sql/run-state.sql drift guard --------------------------------------

#[test]
fn run_state_sql_matches_the_model() {
    let sql = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/sql/run-state.sql"
    ))
    .expect("read deploy/sql/run-state.sql");

    // The two tables + their tenant floor.
    assert!(sql.contains("CREATE TABLE wamn_run.runs"));
    assert!(sql.contains("CREATE TABLE wamn_run.node_runs"));
    assert!(sql.contains("FORCE ROW LEVEL SECURITY"));
    assert!(sql.contains("current_setting('app.tenant', true)"));
    assert!(sql.contains("GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.runs TO wamn_app"));
    // Lineage columns (immutable-replay design) + the loop-safe node-run key.
    assert!(sql.contains("replay_of"));
    assert!(sql.contains("root_run_id"));
    assert!(sql.contains("PRIMARY KEY (tenant_id, run_id, node_id, occurrence)"));
    assert!(sql.contains("runs_idempotency"));
    assert!(sql.contains("REFERENCES wamn_run.runs"));
    // The 5.14 dispatcher's cron anchor recovery (cron_last_run_sql: per-flow
    // max(run_id) over cron runs) is served by this partial index. The column
    // ORDER is load-bearing (equality prefix + max column last = backward
    // index-only scan), so pin the whole statement head, not just the name.
    assert!(
        sql.contains("CREATE INDEX runs_cron_anchor ON wamn_run.runs (tenant_id, flow_id, run_id)")
    );
    assert!(sql.contains("WHERE trigger_source = 'cron'"));
    // Reserved 5.10 / 9.6 seams.
    for seam in [
        "input_ref",
        "output_ref",
        "preview_head",
        "payload_hash",
        "capture_mode",
    ] {
        assert!(
            sql.contains(seam),
            "run-state.sql missing reserved seam {seam}"
        );
    }

    // Every status literal the CHECK constraints pin comes from the crate enums.
    for s in RunStatus::ALL {
        assert!(
            sql.contains(&format!("'{}'", s.as_sql())),
            "runs CHECK missing {}",
            s.as_sql()
        );
    }
    for s in NodeRunStatus::ALL {
        assert!(
            sql.contains(&format!("'{}'", s.as_sql())),
            "node_runs CHECK missing {}",
            s.as_sql()
        );
    }
    for k in NodeErrorKind::ALL {
        assert!(
            sql.contains(&format!("'{}'", k.as_sql())),
            "error_kind CHECK missing {}",
            k.as_sql()
        );
    }
    for k in FailKind::ALL {
        assert!(
            sql.contains(&format!("'{}'", k.as_sql())),
            "fail_kind CHECK missing {}",
            k.as_sql()
        );
    }
}

// ---- live-apply gate (optional) --------------------------------------------

/// Apply `deploy/sql/run-state.sql` to a throwaway Postgres and assert the tenant RLS
/// isolates rows, the idempotency index dedupes, and the FK cascades. Gated on
/// `WAMN_RUN_STORE_PG_URL` (a superuser URL — the harness provisions `wamn_app`);
/// skips cleanly when unset. Mirrors the wamn-ddl / wamn-rls / wamn-seed gates.
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
        "/../../deploy/sql/run-state.sql"
    ))
    .expect("read deploy/sql/run-state.sql");

    let mut script = String::new();
    // Provision wamn_app (NOSUPERUSER/NOBYPASSRLS, like production) + a fresh schema.
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_run CASCADE;\n",
    );
    script.push_str(&ddl);
    script.push('\n');
    // Seed two tenants as the superuser (bypasses RLS): tenant t1 has a run with
    // two node-runs, tenant t2 has one run — the RLS witness.
    script.push_str(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status, idempotency_key) \
           VALUES ('t1','run-a','f',1,'running','k-a'), ('t2','run-b','f',1,'running','k-b');\n\
         INSERT INTO wamn_run.node_runs (tenant_id, run_id, node_id, seq, status, output_port, output_json) \
           VALUES ('t1','run-a','n0',0,'success','main','{}'::jsonb), \
                  ('t1','run-a','n1',1,'success','main','{}'::jsonb);\n",
    );
    // As wamn_app under tenant t1: sees only t1's run + its two node-runs.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path TO wamn_run;\n\
         SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM runs) = 1, 't1 sees only its run'; \
               ASSERT (SELECT count(*) FROM node_runs) = 2, 't1 sees its 2 node-runs'; END $$;\n\
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
             INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, idempotency_key) \
               VALUES ('t1','run-a2','f',1,'k-a'); \
             ASSERT false, 'duplicate idempotency key must be rejected'; \
           EXCEPTION WHEN unique_violation THEN NULL; END; \
         END $$;\n\
         INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, idempotency_key) \
           VALUES ('t3','run-c','f',1,'k-a');\n",
    );
    // The FK cascades: deleting a run removes its node-runs.
    script.push_str(
        "DELETE FROM wamn_run.runs WHERE tenant_id='t1' AND run_id='run-a';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM wamn_run.node_runs WHERE run_id='run-a') = 0, \
               'FK ON DELETE CASCADE removed node-runs'; END $$;\n",
    );
    script.push_str("DROP SCHEMA wamn_run CASCADE;\n");

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
