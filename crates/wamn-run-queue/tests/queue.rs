//! wamn-run-queue (5.14) tests: the pure claim/lease/janitor/reconcile decisions,
//! the SQL builders' shape, the `deploy/run-queue.sql` drift guard, and an
//! optional live-apply gate (the SKIP-LOCKED claim predicate, lease-expiry
//! reclaim, janitor sweep, RLS isolation, and FK cascade on a real Postgres).

use std::collections::HashSet;

use wamn_run_queue::{
    ClaimState, DEFAULT_MAX_INTERVAL_MS, DEFAULT_MIN_INTERVAL_MS, JanitorVerdict, OutboxRow,
    PartitionOwner, QueueEntry, RowEventFlow, RunStatus, acquire_partitions_sql, active_flows_sql,
    claim_batch_sql, claim_partition_head_sql, claim_state, cron_firing, cron_last_run_sql,
    cron_tick_of, dequeue_sql, due_tick, enqueue_sql, gc_orphan_partitions_sql, is_claimable,
    janitor_sweep_sql, janitor_verdict, lease_deadline, lease_live, mark_running_sql, match_outbox,
    mint_cron_run_id, next_fire, next_interval, next_reconcile, orphans, outbox_ack_sql,
    outbox_insert_sql, outbox_poll_sql, park_sql, parked_due_sql, partition_lease_live, plan_ack,
    plan_acquire, plan_claim, plan_partition_claim, reconcile_due, release_partition_sql,
    renew_lease_sql, renew_partition_sql, should_renew, write_ahead_run_sql,
    write_ahead_triggered_run_sql,
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
    // The global claim leaves partitioned runs to the per-partition path.
    assert!(sql.contains("c.partition_key IS NULL"));
}

#[test]
fn partition_sql_builders_are_shaped_and_tenant_scoped() {
    // Acquire: PK-arbitrated INSERT ... ON CONFLICT, only stealing an expired lease.
    let acq = acquire_partitions_sql(5);
    assert!(acq.contains("INSERT INTO partition_owner"));
    assert!(acq.contains("SELECT DISTINCT q.partition_key FROM run_queue q"));
    assert!(acq.contains("q.partition_key IS NOT NULL"));
    assert!(acq.contains("ON CONFLICT (tenant_id, partition_key) DO UPDATE"));
    // Only an expired partition lease may be stolen (the arbitration guard).
    assert!(acq.contains("WHERE o.lease_expires_at <= now()"));
    assert!(acq.contains("LIMIT 5"));
    assert!(acq.contains("RETURNING o.partition_key"));

    // Claim head: owned partitions only, one-in-flight + head-first, SKIP LOCKED.
    let claim = claim_partition_head_sql(8);
    assert!(claim.contains("JOIN partition_owner AS o"));
    assert!(claim.contains("o.lease_owner = $1 AND o.lease_expires_at > now()"));
    assert!(claim.contains("c.partition_key IS NOT NULL"));
    // The NOT EXISTS reduces each partition to a single head candidate, which is
    // what makes FOR UPDATE OF c (no DISTINCT) legal. Its two disjuncts are the two
    // ordering guards: a live-leased sibling (one-in-flight) and an earlier ready
    // sibling (head-first). The behavioral live-apply gate proves the in-flight
    // branch is the SOLE blocker of a successor while its head is live-leased.
    assert!(claim.contains("NOT EXISTS"));
    assert!(claim.contains("b.lease_expires_at IS NOT NULL AND b.lease_expires_at > now()"));
    assert!(claim.contains("(b.available_at, b.run_id) < (c.available_at, c.run_id)"));
    assert!(claim.contains("FOR UPDATE OF c SKIP LOCKED"));
    assert!(claim.contains("LIMIT 8"));
    assert!(claim.contains("RETURNING q.run_id, q.partition_key, q.attempts, q.lease_expires_at"));

    // Acquire / renew / release / gc carry an explicit tenant claim. (The head
    // claim, like the global claim_batch_sql, is tenant-scoped purely by RLS on
    // run_queue + partition_owner — it writes no explicit app.tenant literal.)
    for sql in [
        acquire_partitions_sql(1),
        renew_partition_sql(),
        release_partition_sql(),
        gc_orphan_partitions_sql(),
    ] {
        assert!(
            sql.contains("current_setting('app.tenant', true)"),
            "not tenant-scoped: {sql}"
        );
    }
    // The head claim relies on RLS, not an explicit tenant literal (like claim_batch_sql).
    assert!(!claim_partition_head_sql(1).contains("current_setting"));
    assert!(renew_partition_sql().contains("lease_owner = $3"));
    assert!(release_partition_sql().contains("DELETE FROM partition_owner"));
    assert!(release_partition_sql().contains("lease_owner = $2"));
    // GC removes only expired leases whose partition has drained.
    let gc = gc_orphan_partitions_sql();
    assert!(gc.contains("o.lease_expires_at <= now()"));
    assert!(gc.contains("NOT EXISTS"));
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
fn global_claim_skips_partitioned_rows() {
    // The global claim only takes unpartitioned runs; a partitioned run (even a
    // ready one) is left for the per-partition ownership path so it is never
    // dispatched out of order.
    let rows = vec![
        QueueEntry::ready("t1", "plain", 100, 20),
        QueueEntry::ready_partition("t1", "part-run", "site-1", 100, 20),
    ];
    let plan = plan_claim(&rows, 1_000, 10, 60_000);
    let ids: Vec<&str> = plan.claimed.iter().map(|c| c.run_id.as_str()).collect();
    assert_eq!(ids, ["plain"]);
}

// ---- per-partition ownership -----------------------------------------------

#[test]
fn partition_lease_liveness() {
    let o = PartitionOwner::new("t1", "site-1", "replica-A", 500);
    assert!(partition_lease_live(&o, 100));
    assert!(!partition_lease_live(&o, 500)); // boundary: expiry == now is not live
    assert!(!partition_lease_live(&o, 600));
}

#[test]
fn plan_acquire_takes_unowned_keys_with_claimable_runs() {
    let rows = vec![
        // site-1: a claimable head -> acquirable.
        QueueEntry::ready_partition("t1", "s1-0", "site-1", 100, 20),
        QueueEntry::ready_partition("t1", "s1-1", "site-1", 100, 20),
        // site-2: live-owned by someone else -> NOT acquirable.
        QueueEntry::ready_partition("t1", "s2-0", "site-2", 100, 20),
        // site-3: only a parked run (future) -> no claimable head -> NOT acquirable.
        QueueEntry::ready_partition("t1", "s3-0", "site-3", 9_999, 20),
        // an unpartitioned run is never a partition to acquire.
        QueueEntry::ready("t1", "plain", 100, 20),
    ];
    let owners = vec![
        PartitionOwner::new("t1", "site-2", "other", 9_999), // live
        PartitionOwner::new("t1", "site-4", "stale", 50),    // expired -> irrelevant (no runs)
    ];

    let keys = plan_acquire(&rows, &owners, 1_000, 10);
    assert_eq!(keys, ["site-1"]); // site-2 live-owned, site-3 parked, plain unpartitioned

    // A site-2 whose lease has expired becomes acquirable again (failover).
    let expired = vec![PartitionOwner::new("t1", "site-2", "dead", 50)];
    let keys = plan_acquire(&rows, &expired, 1_000, 10);
    assert_eq!(keys, ["site-1", "site-2"]);

    // limit caps the number of partitions taken (key order).
    let one = plan_acquire(&rows, &expired, 1_000, 1);
    assert_eq!(one, ["site-1"]);
}

#[test]
fn plan_partition_claim_takes_head_of_owned_partitions_only() {
    let owned: HashSet<&str> = ["site-1", "site-2"].into_iter().collect();
    let rows = vec![
        // site-1: three ready runs -> only the head (s1-0) is claimable.
        QueueEntry::ready_partition("t1", "s1-0", "site-1", 100, 20),
        QueueEntry::ready_partition("t1", "s1-1", "site-1", 100, 20),
        QueueEntry::ready_partition("t1", "s1-2", "site-1", 100, 20),
        // site-2: an EARLIER run is in flight -> the whole partition is blocked.
        QueueEntry {
            lease_owner: Some("me".into()),
            lease_expires_at: Some(9_999),
            ..QueueEntry::ready_partition("t1", "s2-0", "site-2", 100, 20)
        },
        QueueEntry::ready_partition("t1", "s2-1", "site-2", 100, 20),
        // site-3: ready head, but NOT owned by this replica -> skipped.
        QueueEntry::ready_partition("t1", "s3-0", "site-3", 100, 20),
    ];

    let plan = plan_partition_claim(&rows, &owned, 1_000, 10, 60_000);
    let ids: Vec<&str> = plan.claimed.iter().map(|c| c.run_id.as_str()).collect();
    // site-1 head only; site-2 blocked (in-flight); site-3 not owned.
    assert_eq!(ids, ["s1-0"]);
    assert_eq!(plan.claimed[0].attempts, 1); // bumped
    assert_eq!(plan.claimed[0].lease_expires_at, 1_000 + 60_000);
}

#[test]
fn plan_partition_claim_advances_in_order_and_limits_across_partitions() {
    let owned: HashSet<&str> = ["a", "b"].into_iter().collect();
    // Both partitions have a free (no in-flight) head; a claim takes one head each.
    let rows = vec![
        QueueEntry::ready_partition("t1", "a-0", "a", 100, 20),
        QueueEntry::ready_partition("t1", "a-1", "a", 100, 20),
        QueueEntry::ready_partition("t1", "b-0", "b", 200, 20),
        QueueEntry::ready_partition("t1", "b-1", "b", 200, 20),
    ];
    let both = plan_partition_claim(&rows, &owned, 1_000, 10, 60_000);
    let mut ids: Vec<&str> = both.claimed.iter().map(|c| c.run_id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["a-0", "b-0"]); // the head of each owned partition

    // limit 1 -> the globally-earliest head (a-0, available_at 100 < b-0's 200).
    let one = plan_partition_claim(&rows, &owned, 1_000, 1, 60_000);
    assert_eq!(one.claimed.len(), 1);
    assert_eq!(one.claimed[0].run_id, "a-0");

    // Simulate a-0 done + dequeued: now a-1 is the head of partition a.
    let after: Vec<QueueEntry> = rows.into_iter().filter(|e| e.run_id != "a-0").collect();
    let next = plan_partition_claim(&after, &owned, 1_000, 1, 60_000);
    assert_eq!(next.claimed[0].run_id, "a-1"); // in order
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
    // The completion-vs-failover race guard: the status update only touches a
    // still-in-flight run, so a reclaimed-and-completed run is never relabeled.
    assert!(
        sweep.contains(&format!(
            "r.status IN ('{}', '{}')",
            RunStatus::Dispatched.as_sql(),
            RunStatus::Running.as_sql()
        )),
        "janitor sweep must guard the status update on a non-terminal run: {sweep}"
    );
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

// ---- trigger dispatcher: cron ------------------------------------------------

/// 2026-01-01 00:00:00 UTC.
const JAN1_2026: i64 = 1_767_225_600_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;

#[test]
fn cron_next_fire_is_strictly_after() {
    // F3's canonical nightly schedule (0 2 * * *).
    let two_am = JAN1_2026 + 2 * HOUR;
    assert_eq!(next_fire("0 2 * * *", JAN1_2026).unwrap(), two_am);
    // From exactly the tick, strictly-after means the NEXT day's tick.
    assert_eq!(next_fire("0 2 * * *", two_am).unwrap(), two_am + DAY);
    assert!(next_fire("not a cron", 0).is_err());
}

#[test]
fn cron_calendar_edges() {
    // Leap day: the next Feb 29 after 2026-01-01 is in 2028.
    let feb29_2028: i64 = 1_835_395_200_000;
    assert_eq!(next_fire("0 0 29 2 *", JAN1_2026).unwrap(), feb29_2028);
    // Day-of-month 31 skips 30-day months: from 2026-04-01 the next 31st is May 31.
    let apr1_2026 = JAN1_2026 + 90 * DAY;
    assert_eq!(
        next_fire("0 0 31 * *", apr1_2026).unwrap(),
        apr1_2026 + 60 * DAY
    );
}

#[test]
fn due_tick_fires_latest_and_collapses_misfires() {
    let schedule = "0 2 * * *";
    let first_tick = JAN1_2026 + 2 * HOUR;
    // Nothing due before the first tick after the anchor.
    assert_eq!(
        due_tick(schedule, JAN1_2026, JAN1_2026 + HOUR).unwrap(),
        None
    );
    // Exactly at the tick — and anywhere within its second — the same canonical
    // tick is due: replicas observing at different sub-second offsets agree.
    assert_eq!(
        due_tick(schedule, JAN1_2026, first_tick).unwrap(),
        Some(first_tick)
    );
    assert_eq!(
        due_tick(schedule, JAN1_2026, first_tick + 500).unwrap(),
        Some(first_tick)
    );
    // Misfire collapse: three nightly ticks missed (dispatcher down) -> only the
    // LATEST fires, one run per tick instant, no burst replay.
    let now = JAN1_2026 + 3 * DAY + 12 * HOUR;
    let latest = JAN1_2026 + 3 * DAY + 2 * HOUR;
    assert_eq!(due_tick(schedule, first_tick, now).unwrap(), Some(latest));
    // Re-anchored on the fired tick, nothing more is due until tomorrow.
    assert_eq!(due_tick(schedule, latest, now).unwrap(), None);
    assert!(due_tick("* * bogus", 0, 1).is_err());
    // A parseable but UNSATISFIABLE schedule (Feb 30 never exists) is an ERROR,
    // never a silent Ok(None): the driver quarantines it with a warning instead
    // of leaving a flow that never fires with zero diagnostics.
    assert!(due_tick("0 0 30 2 *", JAN1_2026, JAN1_2026 + DAY).is_err());
    assert!(next_fire("0 0 30 2 *", JAN1_2026).is_err());
}

#[test]
fn cron_run_ids_are_deterministic_and_ordered() {
    let a = mint_cron_run_id("escalate-stale-holds", JAN1_2026);
    let b = mint_cron_run_id("escalate-stale-holds", JAN1_2026 + DAY);
    assert_eq!(a, "escalate-stale-holds:cron:1767225600000");
    assert!(a < b); // zero-padded ticks: lexical order == chronological order
    assert_eq!(cron_tick_of("escalate-stale-holds", &a), Some(JAN1_2026));
    // Pre-1e12 ticks pad so the within-flow ordering property holds everywhere.
    let small = mint_cron_run_id("f", 42);
    assert_eq!(small, "f:cron:0000000000042");
    assert_eq!(cron_tick_of("f", &small), Some(42));
    // Non-cron ids don't parse back to a tick.
    assert_eq!(cron_tick_of("f", "f:outbox:42"), None);
    assert_eq!(cron_tick_of("f", "plain-run"), None);
    // The parse is EXACT-prefix, never suffix-based: a FOREIGN flow's id — even
    // one nesting ':cron:' inside its own flow id — must not read as this
    // flow's anchor (a wrong anchor silently skips a due tick).
    assert_eq!(cron_tick_of("a", "acron5:cron:0000000000042"), None);
    assert_eq!(cron_tick_of("a", "a:cron:5x:cron:0000000000042"), None);
    assert_eq!(
        cron_tick_of("a:cron:5x", "a:cron:5x:cron:0000000000042"),
        Some(42)
    );

    let f = cron_firing("escalate-stale-holds", 3, "0 2 * * *", JAN1_2026);
    assert_eq!(f.run_id, a);
    assert_eq!(f.flow_id, "escalate-stale-holds");
    assert_eq!(f.flow_version, 3);
    assert_eq!(f.trigger_source, "cron");
    let v: serde_json::Value = serde_json::from_str(&f.input_json).unwrap();
    assert_eq!(v["trigger"], "cron");
    assert_eq!(v["schedule"], "0 2 * * *");
    assert_eq!(v["fire-at-ms"], JAN1_2026);
}

// ---- trigger dispatcher: outbox ------------------------------------------------

#[test]
fn outbox_matching_fires_per_row_times_flow() {
    let rows = vec![
        OutboxRow {
            seq: 7,
            table: "dispositions".into(),
            event: "insert".into(),
            payload: Some("{\"id\": \"d-1\"}".to_string()),
        },
        OutboxRow {
            seq: 8,
            table: "receipts".into(),
            event: "update".into(),
            payload: None,
        },
        // No flow registered on deletes -> consumed with no firing.
        OutboxRow {
            seq: 9,
            table: "receipts".into(),
            event: "delete".into(),
            payload: None,
        },
    ];
    let flows = vec![
        RowEventFlow {
            flow_id: "disposition-recorded".into(),
            flow_version: 2,
            table: "dispositions".into(),
            event: "insert".into(),
        },
        // Two flows on the same (table, event): both fire per row.
        RowEventFlow {
            flow_id: "disposition-audit".into(),
            flow_version: 1,
            table: "dispositions".into(),
            event: "insert".into(),
        },
        RowEventFlow {
            flow_id: "receipt-updated".into(),
            flow_version: 1,
            table: "receipts".into(),
            event: "update".into(),
        },
    ];
    let firings = match_outbox(&rows, &flows);
    let ids: Vec<&str> = firings.iter().map(|f| f.run_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "disposition-recorded:outbox:7",
            "disposition-audit:outbox:7",
            "receipt-updated:outbox:8",
        ]
    );
    assert_eq!(firings[0].trigger_source, "outbox:7");
    let v: serde_json::Value = serde_json::from_str(&firings[0].input_json).unwrap();
    assert_eq!(v["trigger"], "row-event");
    assert_eq!(v["table"], "dispositions");
    assert_eq!(v["event"], "insert");
    assert_eq!(v["seq"], 7);
    assert_eq!(v["payload"]["id"], "d-1");
    // A payload-less event carries an explicit null (still a replayable input).
    let v8: serde_json::Value = serde_json::from_str(&firings[2].input_json).unwrap();
    assert_eq!(v8["payload"], serde_json::Value::Null);
    // No flows registered -> nothing fires (rows are acked as consumed-no-op).
    assert!(match_outbox(&rows, &[]).is_empty());
}

#[test]
fn outbox_payload_is_spliced_verbatim_no_float_round_trip() {
    // The platform's no-float rule end-to-end: a row_to_json payload carrying
    // an int8 beyond 2^53 and a >15-significant-digit exact decimal must reach
    // the run input BYTE-EXACT — a parse-through-f64 would rewrite both.
    let rows = vec![OutboxRow {
        seq: 1,
        table: "receipts".into(),
        event: "insert".into(),
        payload: Some("{\"big\": 9007199254740993, \"qty\": 12345678901234567.89}".to_string()),
    }];
    let flows = vec![RowEventFlow {
        flow_id: "f".into(),
        flow_version: 1,
        table: "receipts".into(),
        event: "insert".into(),
    }];
    let firings = match_outbox(&rows, &flows);
    assert_eq!(firings.len(), 1);
    assert!(firings[0].input_json.contains("9007199254740993"));
    assert!(firings[0].input_json.contains("12345678901234567.89"));
}

#[test]
fn plan_ack_holds_a_skipped_flows_events() {
    // A skew-held (table, event) stays pending; everything else — matched or
    // unmatched — acks.
    let rows = vec![
        OutboxRow {
            seq: 1,
            table: "dispositions".into(),
            event: "insert".into(),
            payload: None,
        },
        OutboxRow {
            seq: 2,
            table: "skewed".into(),
            event: "insert".into(),
            payload: None,
        },
        // Same table, different event -> NOT held.
        OutboxRow {
            seq: 3,
            table: "skewed".into(),
            event: "delete".into(),
            payload: None,
        },
    ];
    let held = vec![("skewed".to_string(), "insert".to_string())];
    assert_eq!(plan_ack(&rows, &held), [1, 3]);
    assert_eq!(plan_ack(&rows, &[]), [1, 2, 3]);
}

// ---- trigger dispatcher: adaptive cadence ---------------------------------------

#[test]
fn adaptive_interval_tightens_on_work_and_decays_to_max() {
    let (min, max) = (DEFAULT_MIN_INTERVAL_MS, DEFAULT_MAX_INTERVAL_MS);
    // Work snaps the cadence to the tight bound, from anywhere.
    assert_eq!(next_interval(max, true, min, max), min);
    assert_eq!(next_interval(min, true, min, max), min);
    // Idleness decays exponentially and caps at max (the reconciliation band).
    assert_eq!(next_interval(min, false, min, max), 2 * min);
    assert_eq!(next_interval(2 * min, false, min, max), 4 * min);
    assert_eq!(next_interval(20_000, false, min, max), max); // 40k clamps to 30k
    assert_eq!(next_interval(max, false, min, max), max);
    // A degenerate current clamps up into the band.
    assert_eq!(next_interval(0, false, min, max), min);
}

// ---- trigger dispatcher: SQL builders --------------------------------------------

#[test]
fn dispatcher_sql_builders_are_shaped_and_tenant_scoped() {
    // The triggered write-ahead persists the trigger payload (replayable input +
    // audit source) and stays idempotent — the exactly-once anchor.
    let wat = write_ahead_triggered_run_sql();
    assert!(wat.contains("trigger_source, input_json"));
    // ::text::jsonb, never a bare ::jsonb (the driver binds JSON as text).
    assert!(wat.contains("$5::text::jsonb"));
    assert!(wat.contains(&format!("'{}'", RunStatus::Dispatched.as_sql())));
    assert!(wat.contains("ON CONFLICT (tenant_id, run_id) DO NOTHING"));
    assert!(wat.contains("current_setting('app.tenant', true)"));

    // The poll: pending only, oldest-first, SKIP LOCKED (replica-disjoint batches).
    let poll = outbox_poll_sql(64);
    assert!(poll.contains("dispatched_at IS NULL"));
    assert!(poll.contains("ORDER BY seq"));
    assert!(!poll.contains("DESC")); // oldest-FIRST — a substring check alone would pass DESC
    assert!(poll.contains("FOR UPDATE SKIP LOCKED"));
    assert!(poll.contains("LIMIT 64"));

    let ack = outbox_ack_sql();
    assert!(ack.contains("SET dispatched_at = now()"));
    assert!(ack.contains("seq = ANY($1)"));
    assert!(ack.contains("current_setting('app.tenant', true)"));

    let ins = outbox_insert_sql();
    assert!(ins.contains("$3::text::jsonb"));
    assert!(ins.contains("current_setting('app.tenant', true)"));

    // Last-fired-tick recovery: FLOW-EXCLUSIVE (flow_id + trigger_source),
    // never a lexical run-id range — flow ids are unconstrained user text and
    // text ordering is collation-dependent, so a range scan can leak a foreign
    // flow's ids into the max (the runs table IS the dispatcher's cron state).
    let last = cron_last_run_sql();
    assert!(last.contains("max(run_id)"));
    assert!(last.contains("flow_id = $1"));
    assert!(last.contains("trigger_source = 'cron'"));
    assert!(!last.contains("run_id >="));
    assert!(last.contains("current_setting('app.tenant', true)"));

    // The registry scan: active flows only; the trigger lives in graph_json.
    let flows = active_flows_sql();
    assert!(flows.contains("WHERE active"));
    assert!(flows.contains("graph_json::text"));

    // The wake/reconciliation scan mirrors the claim predicate (due,
    // unleased-or-expired, budget remaining) but is strictly read-only.
    let wake = parked_due_sql(100);
    assert!(wake.contains("available_at <= now()"));
    assert!(wake.contains("lease_expires_at IS NULL OR lease_expires_at <= now()"));
    assert!(wake.contains("attempts < max_attempts"));
    assert!(wake.contains("ORDER BY available_at, run_id"));
    assert!(wake.contains("LIMIT 100"));
    assert!(!wake.contains("FOR UPDATE"));
    assert!(!wake.contains("UPDATE "));
}

#[test]
fn outbox_ddl_matches_the_model() {
    let sql = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/run-queue.sql"
    ))
    .expect("read deploy/run-queue.sql");

    assert!(sql.contains("CREATE TABLE wamn_run.outbox"));
    for col in [
        "seq",
        "table_name",
        "event",
        "payload",
        "created_at",
        "dispatched_at",
    ] {
        assert!(
            sql.contains(col),
            "run-queue.sql outbox missing column {col}"
        );
    }
    // The wamn-flow row-event vocabulary, verbatim.
    assert!(sql.contains("CHECK (event IN ('insert', 'update', 'delete'))"));
    assert!(sql.contains("PRIMARY KEY (tenant_id, seq)"));
    // The pending partial index the poll scans.
    assert!(sql.contains("CREATE INDEX outbox_pending"));
    assert!(sql.contains("WHERE dispatched_at IS NULL"));
    // House tenant floor.
    assert!(sql.contains("CREATE POLICY outbox_tenant ON wamn_run.outbox"));
    assert!(sql.contains("GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.outbox TO wamn_app"));
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

    // The per-partition ownership lease table + its tenant floor + PK.
    assert!(sql.contains("CREATE TABLE wamn_run.partition_owner"));
    assert!(sql.contains("PRIMARY KEY (tenant_id, partition_key)"));
    assert!(
        sql.contains(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.partition_owner TO wamn_app"
        )
    );
    assert!(sql.contains("CREATE POLICY partition_owner_tenant ON wamn_run.partition_owner"));
    // The partition index the acquire/claim path scans on.
    assert!(sql.contains("CREATE INDEX run_queue_partition"));
}

// ---- live-apply gate (optional) --------------------------------------------

/// Apply `deploy/run-state.sql` + `deploy/run-queue.sql` to a throwaway Postgres
/// and assert the queue's real behaviour: the `SKIP LOCKED` claim predicate
/// (Ready claimed, Parked/Leased skipped), lease-expiry reclaim, the janitor sweep
/// (orphan → `infrastructure-failure` + dequeued), tenant RLS isolation, the
/// FK cascade from `runs`, and the trigger dispatcher's outbox path (producer
/// insert → poll → co-transacted fire + ack, crash-rollback atomicity +
/// redelivery dedupe, cron last-tick recovery, the wake scan). Gated on
/// `WAMN_RUN_QUEUE_PG_URL` (a superuser URL — the harness provisions `wamn_app`);
/// skips cleanly when unset. Mirrors the wamn-run-store / wamn-ddl / wamn-rls
/// gates. (True concurrent contention is the queuebench/dispatchbench gates; this
/// asserts the schema + predicates on one session.)
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
    // bug in claim_batch_sql / janitor_sweep_sql / the partition builders is caught here.
    let claim_sql = claim_batch_sql(10);
    let janitor_sql = janitor_sweep_sql();
    let acquire_sql = acquire_partitions_sql(10);
    let claim_head_sql = claim_partition_head_sql(10);
    // The trigger dispatcher's builders (cron/outbox/wake).
    let insert_sql = outbox_insert_sql();
    let poll_sql = outbox_poll_sql(10);
    let ack_sql = outbox_ack_sql();
    let triggered_sql = write_ahead_triggered_run_sql();
    let last_run_sql = cron_last_run_sql();
    let parked_sql = parked_due_sql(50);

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
           ('t1','rq-completed','f',1,'completed'), \
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
           ('t1','rq-completed',now()- interval '3 hour','dead',now() - interval '2 hour', 20, 20), \
           ('t2','rq-other',   now(),                    NULL,  NULL,                     0,  20);\n",
    );

    // Partitioned runs: two ordered streams (site-a: pa-0<pa-1<pa-2, site-b:
    // pb-0<pb-1), all ready now (order within a key = run_id). These exercise the
    // per-partition ownership path; the global claim above skips them.
    script.push_str(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
           ('t1','pa-0','f',1,'dispatched'),('t1','pa-1','f',1,'dispatched'),('t1','pa-2','f',1,'dispatched'), \
           ('t1','pb-0','f',1,'dispatched'),('t1','pb-1','f',1,'dispatched');\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, partition_key, available_at, attempts, max_attempts) VALUES \
           ('t1','pa-0','site-a', now(), 0, 20), \
           ('t1','pa-1','site-a', now(), 0, 20), \
           ('t1','pa-2','site-a', now(), 0, 20), \
           ('t1','pb-0','site-b', now(), 0, 20), \
           ('t1','pb-1','site-b', now(), 0, 20);\n",
    );

    // RLS isolation: t1 sees its queue rows (9 unpartitioned rq-* + 5 partitioned
    // pa-*/pb-* = 14); no claim -> zero.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM run_queue) = 14, 't1 sees its 14 queue rows'; END $$;\n\
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
           ASSERT (SELECT status FROM runs WHERE run_id='rq-completed') = 'completed', 'janitor does NOT relabel a reclaimed-and-completed run (completion-vs-failover race guard)'; \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='rq-completed') = 0, 'a completed run''s stale queue row is still cleaned up'; \
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

    // Per-partition ownership via the REAL acquire_partitions_sql() +
    // claim_partition_head_sql() (PREPARE/EXECUTE). Replica R1 leases the two
    // partitions and claims the HEAD of each; a second replica R2 can neither steal a
    // live-owned partition nor claim its runs; and a partition advances in order only
    // once its head completes and dequeues.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE acquire_stmt (text, bigint) AS {acquire_sql};\n\
         PREPARE claimhead_stmt (text, bigint) AS {claim_head_sql};\n\
         EXECUTE acquire_stmt('R1', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM partition_owner WHERE lease_owner='R1') = 2, 'R1 leases site-a + site-b'; \
         END $$;\n\
         EXECUTE claimhead_stmt('R1', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='pa-0') = 'R1', 'site-a head pa-0 claimed'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='pb-0') = 'R1', 'site-b head pb-0 claimed'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='pa-1') IS NULL, 'pa-1 blocked (one in flight per key)'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='pa-2') IS NULL, 'pa-2 blocked behind the head'; \
         END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         EXECUTE acquire_stmt('R2', 60000);\n\
         EXECUTE claimhead_stmt('R2', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM partition_owner WHERE lease_owner='R2') = 0, 'R2 cannot steal a live-owned partition'; \
           ASSERT (SELECT count(*) FROM run_queue WHERE lease_owner='R2') = 0, 'R2 owns no partition, claims nothing'; \
         END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         -- pa-0 is now LIVE-leased by R1 (committed) and STILL queued. A second head\n\
         -- claim must NOT advance site-a: pa-1 is blocked SOLELY by the one-in-flight\n\
         -- guard (a live-leased pa-0 is no longer an 'earlier READY sibling', so the\n\
         -- head-first branch does not block pa-1). This fails if the in-flight branch\n\
         -- [claim_partition_head_sql: b.lease_expires_at IS NOT NULL AND > now()] is removed.\n\
         EXECUTE claimhead_stmt('R1', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='pa-0') = 'R1', 'pa-0 still R1 (live lease, not re-claimed)'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='pa-1') IS NULL, 'pa-1 blocked by one-in-flight while pa-0 live-leased-and-present'; \
         END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         DELETE FROM run_queue WHERE run_id='pa-0';\n\
         EXECUTE claimhead_stmt('R1', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='pa-1') = 'R1', 'site-a advances to pa-1 in order after pa-0 dequeues'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='pa-2') IS NULL, 'pa-2 still blocked behind pa-1'; \
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
    // The trigger dispatcher's outbox path via the REAL builders: a producer
    // inserts events in its own transaction (outbox_insert_sql); the dispatcher
    // polls pending rows oldest-first (outbox_poll_sql, SKIP LOCKED), fires the
    // triggered write-ahead (write_ahead_triggered_run_sql), and acks
    // (outbox_ack_sql) — all in ONE transaction. RLS isolates tenants' events.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE outbox_ins (text, text, text) AS {insert_sql};\n\
         EXECUTE outbox_ins('dispositions', 'insert', '{{\"id\": \"d-1\"}}');\n\
         EXECUTE outbox_ins('dispositions', 'insert', NULL);\n\
         COMMIT;\n\
         INSERT INTO wamn_run.outbox (tenant_id, table_name, event, payload) \
           VALUES ('t2', 'dispositions', 'insert', '{{\"id\": \"other\"}}');\n",
    ));
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE outbox_poll AS {poll_sql};\n\
         PREPARE outbox_ack (bigint[]) AS {ack_sql};\n\
         PREPARE triggered_stmt (text, text, int, text, text) AS {triggered_sql};\n\
         CREATE TEMP TABLE polled AS EXECUTE outbox_poll;\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM polled) = 2, 'poll returns t1''s 2 pending rows (t2''s is RLS-invisible)'; \
           ASSERT (SELECT count(*) FROM polled WHERE table_name='dispositions' AND event='insert') = 2, 'poll carries table+event'; \
           ASSERT (SELECT min(seq) FROM polled) = 1 AND (SELECT max(seq) FROM polled) = 2, 'poll is oldest-first over seq'; \
         END $$;\n\
         EXECUTE triggered_stmt('disposition-recorded:outbox:1', 'disposition-recorded', 1, 'outbox:1', '{{\"seq\": 1}}');\n\
         EXECUTE triggered_stmt('disposition-recorded:outbox:2', 'disposition-recorded', 1, 'outbox:2', '{{\"seq\": 2}}');\n\
         EXECUTE outbox_ack(ARRAY[1,2]::bigint[]);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM runs WHERE trigger_source='outbox:1' AND input_json IS NOT NULL) = 1, 'triggered write-ahead persists trigger_source + input_json'; \
           ASSERT (SELECT status FROM runs WHERE run_id='disposition-recorded:outbox:1') = 'dispatched', 'triggered run write-ahead is dispatched'; \
           ASSERT (SELECT count(*) FROM outbox WHERE dispatched_at IS NULL) = 0, 'both t1 rows acked'; \
         END $$;\n\
         EXECUTE triggered_stmt('disposition-recorded:outbox:1', 'disposition-recorded', 1, 'outbox:1', '{{\"seq\": 1}}');\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM runs WHERE run_id='disposition-recorded:outbox:1') = 1, 'a redelivered firing is a no-op (deterministic id + ON CONFLICT)'; \
         END $$;\n\
         COMMIT;\n",
    ));

    // Crash atomicity: a dispatcher that polls and fires but dies before commit
    // leaves NO half-state — the enqueue is retracted with the ack, the row stays
    // pending, and the redelivery re-mints the same run id (exactly-once).
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         EXECUTE outbox_ins('dispositions', 'insert', '{\"id\": \"d-4\"}');\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         CREATE TEMP TABLE polled_crash AS EXECUTE outbox_poll;\n\
         EXECUTE triggered_stmt('disposition-recorded:outbox:4', 'disposition-recorded', 1, 'outbox:4', '{\"seq\": 4}');\n\
         EXECUTE outbox_ack(ARRAY[4]::bigint[]);\n\
         ROLLBACK;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM runs WHERE run_id='disposition-recorded:outbox:4') = 0, 'crash before commit retracts the fire (no half-state)'; \
           ASSERT (SELECT count(*) FROM outbox WHERE seq=4 AND dispatched_at IS NULL) = 1, 'crash before commit redelivers the row'; \
         END $$;\n\
         CREATE TEMP TABLE polled_redeliver AS EXECUTE outbox_poll;\n\
         EXECUTE triggered_stmt('disposition-recorded:outbox:4', 'disposition-recorded', 1, 'outbox:4', '{\"seq\": 4}');\n\
         EXECUTE outbox_ack(ARRAY[4]::bigint[]);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM polled_redeliver WHERE seq=4) = 1, 'redelivery re-polls the unacked row'; \
           ASSERT (SELECT count(*) FROM runs WHERE run_id='disposition-recorded:outbox:4') = 1, 'redelivery fires exactly once'; \
           ASSERT (SELECT count(*) FROM outbox WHERE dispatched_at IS NULL) = 0, 'redelivered row acked'; \
         END $$;\n\
         COMMIT;\n",
    );

    // Cron last-fired-tick recovery: FLOW-EXCLUSIVE (flow_id + trigger_source
    // predicate, cron_last_run_sql) — foreign flows whose ids sort inside a
    // lexical range under the deployed collation (or literally nest ':cron:' in
    // their flow id) must never leak into another flow's anchor.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE last_stmt (text) AS {last_run_sql};\n\
         EXECUTE triggered_stmt('cronflow:cron:0000000000100', 'cronflow', 1, 'cron', '{{\"fire-at-ms\": 100}}');\n\
         EXECUTE triggered_stmt('cronflow:cron:0000000000200', 'cronflow', 1, 'cron', '{{\"fire-at-ms\": 200}}');\n\
         -- Foreign-anchor poison: a flow whose id embeds ':cron:' and a colon-free\n\
         -- neighbor that sorts inside 'cronflow's lexical range under en_US-style\n\
         -- collations, both with LATER ticks; and a non-cron run for the flow itself.\n\
         EXECUTE triggered_stmt('cronflow:cron:5x:cron:0000000000999', 'cronflow:cron:5x', 1, 'cron', '{{\"fire-at-ms\": 999}}');\n\
         EXECUTE triggered_stmt('cronflowx:cron:0000000000999', 'cronflowx', 1, 'cron', '{{\"fire-at-ms\": 999}}');\n\
         EXECUTE triggered_stmt('cronflow:outbox:7', 'cronflow', 1, 'outbox:7', '{{\"seq\": 7}}');\n\
         CREATE TEMP TABLE lastrun AS EXECUTE last_stmt('cronflow');\n\
         DO $$ BEGIN \
           ASSERT (SELECT max FROM lastrun) = 'cronflow:cron:0000000000200', 'last-fired recovery is flow-exclusive (no foreign/outbox leak)'; \
         END $$;\n\
         COMMIT;\n",
    ));

    // The wake/reconciliation scan (parked_due_sql): a due unleased row is
    // surfaced for a doorbell hint; a future (parked) or live-leased row is not.
    script.push_str(&format!(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
           ('t1','wk-due','f',1,'dispatched'), \
           ('t1','wk-future','f',1,'dispatched'), \
           ('t1','wk-leased','f',1,'dispatched');\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, available_at, lease_owner, lease_expires_at, attempts, max_attempts) VALUES \
           ('t1','wk-due',    now() - interval '1 min', NULL, NULL,                      0, 20), \
           ('t1','wk-future', now() + interval '1 hour',NULL, NULL,                      0, 20), \
           ('t1','wk-leased', now() - interval '1 min','W',   now() + interval '1 hour', 1, 20);\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE parked_stmt AS {parked_sql};\n\
         CREATE TEMP TABLE woken AS EXECUTE parked_stmt;\n\
         DO $$ BEGIN \
           ASSERT EXISTS (SELECT 1 FROM woken WHERE run_id='wk-due'), 'a due unleased row is surfaced for a wake hint'; \
           ASSERT NOT EXISTS (SELECT 1 FROM woken WHERE run_id='wk-future'), 'a still-parked (future) row is not woken'; \
           ASSERT NOT EXISTS (SELECT 1 FROM woken WHERE run_id='wk-leased'), 'a live-leased row is not woken'; \
         END $$;\n\
         COMMIT;\n",
    ));

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
