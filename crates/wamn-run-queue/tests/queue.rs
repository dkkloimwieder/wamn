//! wamn-run-queue (5.14) tests: the pure claim/lease/janitor/reconcile decisions,
//! the SQL builders' shape, the `deploy/run-queue.sql` drift guard, and an
//! optional live-apply gate (the SKIP-LOCKED claim predicate, lease-expiry
//! reclaim, janitor sweep, RLS isolation, and FK cascade on a real Postgres).

use wamn_run_queue::{
    ClaimState, JanitorVerdict, QueueEntry, RunStatus, claim_batch_sql, claim_state, dequeue_sql,
    enqueue_sql, is_claimable, janitor_sweep_sql, janitor_verdict, lease_deadline, lease_live,
    mark_running_sql, next_reconcile, orphans, park_sql, plan_claim, reconcile_due,
    renew_lease_sql, should_renew, write_ahead_run_sql,
};

// ---- claim eligibility -----------------------------------------------------

#[test]
fn claim_state_classifies_ready_leased_parked() {
    // Visible since 50, no lease -> Ready at 100.
    let ready = QueueEntry::ready("t1", "r", 50, 20);
    assert_eq!(claim_state(&ready, 100), ClaimState::Ready);
    assert!(is_claimable(&ready, 100));

    // available_at in the future -> Parked (delayed / parked-wake / backoff).
    let parked = QueueEntry::ready("t1", "r", 500, 20);
    assert_eq!(claim_state(&parked, 100), ClaimState::Parked);
    assert!(!is_claimable(&parked, 100));

    // A live lease -> Leased; once it expires the row is Ready again (reclaim).
    let leased = QueueEntry {
        lease_owner: Some("A".into()),
        lease_expires_at: Some(400),
        ..QueueEntry::ready("t1", "r", 50, 20)
    };
    assert_eq!(claim_state(&leased, 100), ClaimState::Leased);
    assert!(!is_claimable(&leased, 100));
    assert_eq!(claim_state(&leased, 400), ClaimState::Ready); // boundary: expiry == now
    assert_eq!(claim_state(&leased, 500), ClaimState::Ready);

    // Budget spent + lease expired -> Exhausted (the claim path leaves it for the
    // janitor; without this it would be re-claimed forever and never retired).
    let exhausted = QueueEntry {
        lease_owner: Some("dead".into()),
        lease_expires_at: Some(80),
        attempts: 20,
        ..QueueEntry::ready("t1", "r", 50, 20)
    };
    assert_eq!(claim_state(&exhausted, 100), ClaimState::Exhausted);
    assert!(!is_claimable(&exhausted, 100));
    // But a live lease wins over exhaustion (the runner may still complete it).
    let busy = QueueEntry {
        lease_expires_at: Some(500),
        ..exhausted.clone()
    };
    assert_eq!(claim_state(&busy, 100), ClaimState::Leased);
}

#[test]
fn plan_claim_orders_by_available_then_run_id_and_limits() {
    let rows = vec![
        QueueEntry::ready("t1", "b", 200, 20),
        QueueEntry::ready("t1", "a", 100, 20),
        QueueEntry::ready("t1", "z", 100, 20), // same available_at as "a" -> run_id breaks the tie
        QueueEntry::ready("t1", "parked", 9_999, 20),
        QueueEntry {
            lease_owner: Some("X".into()),
            lease_expires_at: Some(9_999),
            ..QueueEntry::ready("t1", "leased", 100, 20)
        },
    ];

    // limit 1 -> the earliest-available, run_id-first claimable row.
    let one = plan_claim(&rows, 1_000, 1, 60_000);
    assert_eq!(one.claimed.len(), 1);
    assert_eq!(one.claimed[0].run_id, "a");
    assert_eq!(one.claimed[0].attempts, 1); // bumped
    assert_eq!(one.claimed[0].lease_expires_at, 1_000 + 60_000);

    // limit 10 -> all three Ready rows in (available_at, run_id) order; the
    // parked and leased rows are skipped.
    let all = plan_claim(&rows, 1_000, 10, 60_000);
    let ids: Vec<&str> = all.claimed.iter().map(|c| c.run_id.as_str()).collect();
    assert_eq!(ids, ["a", "z", "b"]);
}

// ---- leases ----------------------------------------------------------------

#[test]
fn lease_liveness_and_renewal() {
    assert!(lease_live(100, Some(200)));
    assert!(!lease_live(200, Some(200))); // boundary: expiry == now is not live
    assert!(!lease_live(100, None));
    assert_eq!(lease_deadline(100, 30_000), 30_100);

    // Renew once inside the last `renew_before` window.
    assert!(!should_renew(100, 200, 50)); // 100ms left > 50ms window
    assert!(should_renew(160, 200, 50)); // 40ms left <= 50ms window
}

// ---- janitor ---------------------------------------------------------------

#[test]
fn janitor_verdict_and_orphans() {
    let grace = 1_000;
    // Live lease -> leave it.
    let live = QueueEntry {
        lease_expires_at: Some(5_000),
        attempts: 20,
        ..QueueEntry::ready("t1", "live", 0, 20)
    };
    assert_eq!(janitor_verdict(&live, 1_000, grace), JanitorVerdict::Live);

    // Expired but retries remain -> reclaimable, not orphaned.
    let retry = QueueEntry {
        lease_expires_at: Some(1_000),
        attempts: 3,
        ..QueueEntry::ready("t1", "retry", 0, 20)
    };
    assert_eq!(
        janitor_verdict(&retry, 10_000, grace),
        JanitorVerdict::Reclaimable
    );

    // Expired past grace AND budget spent -> orphaned.
    let orphan = QueueEntry {
        lease_expires_at: Some(1_000),
        attempts: 20,
        ..QueueEntry::ready("t1", "orphan", 0, 20)
    };
    assert_eq!(
        janitor_verdict(&orphan, 1_000 + grace, grace),
        JanitorVerdict::Orphaned
    );
    // Just inside grace -> still reclaimable (not yet given up).
    assert_eq!(
        janitor_verdict(&orphan, 1_000 + grace - 1, grace),
        JanitorVerdict::Reclaimable
    );
    // A never-leased row is never orphaned.
    let fresh = QueueEntry::ready("t1", "fresh", 0, 20);
    assert_eq!(
        janitor_verdict(&fresh, 10_000, grace),
        JanitorVerdict::Reclaimable
    );

    let rows = vec![live, retry, orphan, fresh];
    let o = orphans(&rows, 1_000 + grace, grace);
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].run_id, "orphan");
}

// ---- reconciliation --------------------------------------------------------

#[test]
fn reconcile_cadence() {
    assert!(!reconcile_due(1_000, 900, 200)); // 100 < 200
    assert!(reconcile_due(1_100, 900, 200)); // 200 >= 200
    assert_eq!(next_reconcile(900, 200), 1_100);
}

// ---- SQL builders ----------------------------------------------------------

#[test]
fn claim_sql_is_skip_locked_and_bounded() {
    let sql = claim_batch_sql(25);
    assert!(sql.contains("FOR UPDATE SKIP LOCKED"));
    assert!(sql.contains("LIMIT 25"));
    assert!(sql.contains("ORDER BY c.available_at, c.run_id"));
    assert!(sql.contains("c.available_at <= now()"));
    assert!(sql.contains("c.lease_expires_at IS NULL OR c.lease_expires_at <= now()"));
    // The redelivery-budget guard: a spent row is left for the janitor, not re-leased.
    assert!(sql.contains("c.attempts < c.max_attempts"));
    assert!(sql.contains("attempts = q.attempts + 1"));
    assert!(sql.contains("RETURNING q.run_id, q.attempts, q.lease_expires_at"));
}

#[test]
fn plan_claim_skips_budget_spent_rows() {
    // A visible, lease-expired row whose budget is spent is NOT claimed (it awaits
    // the janitor); a sibling with retries left is.
    let rows = vec![
        QueueEntry {
            lease_owner: Some("dead".into()),
            lease_expires_at: Some(500),
            attempts: 5,
            ..QueueEntry::ready("t1", "spent", 100, 5)
        },
        QueueEntry {
            lease_owner: Some("dead".into()),
            lease_expires_at: Some(500),
            attempts: 2,
            ..QueueEntry::ready("t1", "retryable", 100, 5)
        },
    ];
    let plan = plan_claim(&rows, 1_000, 10, 60_000);
    let ids: Vec<&str> = plan.claimed.iter().map(|c| c.run_id.as_str()).collect();
    assert_eq!(ids, ["retryable"]);
}

#[test]
fn lifecycle_sql_uses_run_status_literals() {
    // The queue drives the 5.7 run lifecycle: the literals in the SQL are exactly
    // RunStatus::as_sql, so a rename of the vocabulary can't silently desync.
    assert!(write_ahead_run_sql().contains(&format!("'{}'", RunStatus::Dispatched.as_sql())));
    let mr = mark_running_sql();
    assert!(mr.contains(&format!("status = '{}'", RunStatus::Running.as_sql())));
    assert!(mr.contains(&format!("status = '{}'", RunStatus::Dispatched.as_sql())));
    let sweep = janitor_sweep_sql();
    assert!(sweep.contains(&format!("'{}'", RunStatus::InfrastructureFailure.as_sql())));
    // The sweep is a plain CTE, not a locking claim.
    assert!(!sweep.contains("SKIP LOCKED"));
}

#[test]
fn enqueue_and_maintenance_sql_are_tenant_scoped_and_parameterized() {
    for sql in [
        write_ahead_run_sql(),
        enqueue_sql(),
        mark_running_sql(),
        renew_lease_sql(),
        dequeue_sql(),
        park_sql(),
    ] {
        assert!(
            sql.contains("current_setting('app.tenant', true)"),
            "not tenant-scoped: {sql}"
        );
        assert!(sql.contains('$'), "no bound params: {sql}");
    }
    // Enqueue is idempotent on redelivery; the janitor sweep co-updates runs.
    assert!(enqueue_sql().contains("ON CONFLICT (tenant_id, run_id) DO NOTHING"));
    assert!(write_ahead_run_sql().contains("ON CONFLICT (tenant_id, run_id) DO NOTHING"));
    assert!(janitor_sweep_sql().contains("DELETE FROM run_queue"));
    assert!(janitor_sweep_sql().contains("UPDATE runs"));
}

// ---- record JSON round-trip ------------------------------------------------

#[test]
fn queue_entry_round_trips_as_kebab_json() {
    let e = QueueEntry {
        partition_key: Some("site-7".into()),
        priority: 5,
        lease_owner: Some("replica-2".into()),
        lease_expires_at: Some(1_700_000_000_000),
        attempts: 2,
        ..QueueEntry::ready("t1", "run-9", 1_699_999_999_000, 20)
    };
    let json = serde_json::to_string(&e).unwrap();
    assert!(json.contains("\"partition-key\":\"site-7\""));
    assert!(json.contains("\"lease-expires-at\":1700000000000"));
    assert!(json.contains("\"max-attempts\":20"));
    assert_eq!(serde_json::from_str::<QueueEntry>(&json).unwrap(), e);

    // A ready row omits the optional lease fields.
    let ready = QueueEntry::ready("t1", "r", 0, 20);
    let rj = serde_json::to_string(&ready).unwrap();
    assert!(!rj.contains("lease-owner"));
    assert!(!rj.contains("partition-key"));
}

// ---- deploy/run-queue.sql drift guard --------------------------------------

#[test]
fn run_queue_sql_matches_the_model() {
    let sql = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/run-queue.sql"
    ))
    .expect("read deploy/run-queue.sql");

    // The queue table + its tenant floor + the FK into the 5.7 run state.
    assert!(sql.contains("CREATE TABLE wamn_run.run_queue"));
    // Both are needed: ENABLE turns RLS on, FORCE applies it to the table owner too.
    assert!(sql.contains("ENABLE ROW LEVEL SECURITY"));
    assert!(sql.contains("FORCE ROW LEVEL SECURITY"));
    assert!(sql.contains("current_setting('app.tenant', true)"));
    assert!(sql.contains("GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.run_queue TO wamn_app"));
    assert!(sql.contains("REFERENCES wamn_run.runs (tenant_id, run_id) ON DELETE CASCADE"));
    assert!(sql.contains("PRIMARY KEY (tenant_id, run_id)"));
    // The claim/lease machinery columns the SQL builders read/write.
    for col in [
        "partition_key",
        "priority",
        "available_at",
        "lease_owner",
        "lease_expires_at",
        "attempts",
        "max_attempts",
    ] {
        assert!(sql.contains(col), "run-queue.sql missing column {col}");
    }
}

// ---- live-apply gate (optional) --------------------------------------------

/// Apply `deploy/run-state.sql` + `deploy/run-queue.sql` to a throwaway Postgres
/// and assert the queue's real behaviour: the `SKIP LOCKED` claim predicate
/// (Ready claimed, Parked/Leased skipped), lease-expiry reclaim, the janitor sweep
/// (orphan → `infrastructure-failure` + dequeued), tenant RLS isolation, and the
/// FK cascade from `runs`. Gated on `WAMN_RUN_QUEUE_PG_URL` (a superuser URL — the
/// harness provisions `wamn_app`); skips cleanly when unset. Mirrors the
/// wamn-run-store / wamn-ddl / wamn-rls gates. (True concurrent-claimer contention
/// is the queuebench gate; this asserts the schema + predicate on one session.)
#[test]
fn run_queue_schema_applies_and_claims_on_postgres() {
    let Ok(url) = std::env::var("WAMN_RUN_QUEUE_PG_URL") else {
        eprintln!(
            "skipping run_queue_schema_applies_and_claims_on_postgres (set WAMN_RUN_QUEUE_PG_URL to run)"
        );
        return;
    };

    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let run_state = std::fs::read_to_string(format!("{root}/deploy/run-state.sql"))
        .expect("read deploy/run-state.sql");
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/run-queue.sql"))
        .expect("read deploy/run-queue.sql");

    // Exercise the REAL builders (not hand-copied SQL) via PREPARE/EXECUTE, so a
    // bug in claim_batch_sql / janitor_sweep_sql is caught here.
    let claim_sql = claim_batch_sql(10);
    let janitor_sql = janitor_sweep_sql();

    let mut script = String::new();
    // Provision wamn_app (NOSUPERUSER/NOBYPASSRLS, like production) + a fresh schema.
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS wamn_run CASCADE;\n",
    );
    script.push_str(&run_state);
    script.push('\n');
    script.push_str(&run_queue);
    script.push('\n');

    // Seed (superuser bypasses RLS): eight t1 runs + a t2 witness, and a run_queue
    // row per run spanning every claim state — including a reclaimable row
    // (expired lease, retries left) and a budget-spent row (expired lease, budget
    // spent) so the janitor's orphan-vs-reclaimable check and the claim's
    // budget guard are both exercised.
    script.push_str(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
           ('t1','rq-ready','f',1,'dispatched'), \
           ('t1','rq-parked','f',1,'dispatched'), \
           ('t1','rq-leased','f',1,'dispatched'), \
           ('t1','rq-expired','f',1,'dispatched'), \
           ('t1','rq-orphan','f',1,'dispatched'), \
           ('t1','rq-reclaim','f',1,'dispatched'), \
           ('t1','rq-spent','f',1,'dispatched'), \
           ('t1','rq-healthy','f',1,'dispatched'), \
           ('t2','rq-other','f',1,'dispatched');\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, available_at, lease_owner, lease_expires_at, attempts, max_attempts) VALUES \
           ('t1','rq-ready',   now() - interval '1 min', NULL,  NULL,                     0,  20), \
           ('t1','rq-parked',  now() + interval '1 hour',NULL,  NULL,                     0,  20), \
           ('t1','rq-leased',  now() - interval '1 min','X',    now() + interval '1 hour', 1,  20), \
           ('t1','rq-expired', now() - interval '1 min','dead', now() - interval '1 min',  0,  20), \
           ('t1','rq-orphan',  now() - interval '3 hour','dead',now() - interval '2 hour', 20, 20), \
           ('t1','rq-reclaim', now() - interval '1 min','dead', now() - interval '1 min',  1,  20), \
           ('t1','rq-spent',   now() - interval '1 min','dead', now() - interval '1 min',  20, 20), \
           ('t1','rq-healthy', now(),                    NULL,  NULL,                     0,  20), \
           ('t2','rq-other',   now(),                    NULL,  NULL,                     0,  20);\n",
    );

    // RLS isolation: t1 sees its eight queue rows; no claim -> zero.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM run_queue) = 8, 't1 sees its 8 queue rows'; END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run;\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM run_queue) = 0, 'no tenant claim denies all'; END $$;\n\
         COMMIT;\n",
    );

    // Janitor sweep FIRST (before the claim would touch the rows), running the REAL
    // janitor_sweep_sql() via PREPARE/EXECUTE with a 1-hour grace: only rq-orphan
    // (budget spent, lease expired 2h ago) is retired; rq-reclaim (retries left) and
    // rq-spent (budget spent but within grace) and rq-healthy are all kept.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE janitor_stmt (bigint) AS {janitor_sql};\n\
         EXECUTE janitor_stmt(3600000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='rq-orphan') = 0, 'orphan dequeued'; \
           ASSERT (SELECT status FROM runs WHERE run_id='rq-orphan') = 'infrastructure-failure', 'orphan marked infra-failure'; \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='rq-reclaim') = 1, 'reclaimable (retries left) NOT swept'; \
           ASSERT (SELECT status FROM runs WHERE run_id='rq-reclaim') = 'dispatched', 'reclaimable run untouched'; \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='rq-spent') = 1, 'budget-spent within grace NOT swept'; \
           ASSERT (SELECT status FROM runs WHERE run_id='rq-healthy') = 'dispatched', 'healthy run untouched'; \
         END $$;\n\
         COMMIT;\n"
    ));

    // The REAL SKIP LOCKED claim via PREPARE/EXECUTE: takes the Ready rows
    // (rq-ready, rq-expired, rq-reclaim, rq-healthy); skips rq-parked (future),
    // rq-leased (still 'X'), and rq-spent (budget spent -> left for the janitor).
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE claim_stmt (text, bigint) AS {claim_sql};\n\
         EXECUTE claim_stmt('c1', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM run_queue WHERE lease_owner='c1') = 4, 'claimed the 4 Ready rows'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='rq-leased') = 'X', 'live lease not stolen'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='rq-parked') IS NULL, 'parked row not claimed'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='rq-spent') = 'dead', 'budget-spent row not claimed'; \
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='rq-expired') = 1, 'expired row reclaimed + bumped'; \
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='rq-reclaim') = 2, 'reclaimable row reclaimed + bumped'; \
         END $$;\n\
         COMMIT;\n"
    ));

    // FK cascade: deleting a run removes its queue row.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         DELETE FROM runs WHERE run_id='rq-ready';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM run_queue WHERE run_id='rq-ready') = 0, \
               'FK ON DELETE CASCADE removed the queue row'; END $$;\n\
         COMMIT;\n",
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
