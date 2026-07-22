//! wamn-run-queue (5.14) tests: the pure claim/lease/janitor/reconcile decisions,
//! the SQL builders' shape, the `deploy/sql/run-queue.sql` drift guard, and an
//! optional live-apply gate (the SKIP-LOCKED claim predicate, lease-expiry
//! reclaim, janitor sweep, RLS isolation, and FK cascade on a real Postgres).

use std::collections::HashSet;

use wamn_run_queue::{
    Cadence, ClaimState, CronError, DEFAULT_MAX_INTERVAL_MS, DEFAULT_MIN_INTERVAL_MS,
    JanitorVerdict, PartitionOwner, PartitionPolicy, QueueEntry, RunStatus, acquire_partitions_sql,
    active_flows_sql, claim_batch_sql, claim_dispatch_sql, claim_partition_head_sql, claim_state,
    complete_dequeue_sql, cron_anchor_sql, cron_firing, cron_last_run_sql, cron_tick_of,
    dead_letter_dequeue_sql, dead_letters_on_terminal, dequeue_sql, due_tick, enqueue_evt_sql,
    enqueue_evt_with_policy_sql, enqueue_sql, enqueue_with_policy_sql, gc_orphan_partitions_sql,
    is_claimable, janitor_sweep_sql, janitor_verdict, lease_deadline, lease_live, mark_running_sql,
    mint_cron_run_id, mint_evt_run_id, next_fire, next_reconcile, orphans, park_sql,
    parked_due_sql, partition_lease_live, plan_acquire, plan_claim, plan_partition_claim,
    reconcile_due, record_error_and_renew_sql, record_success_and_renew_sql, release_partition_sql,
    renew_lease_sql, renew_partition_sql, should_renew, upsert_cron_anchor_sql,
    write_ahead_run_sql, write_ahead_triggered_run_sql,
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
    assert_eq!(one.claimed[0].attempts, 0); // never leased -> first claim is FREE (crash evidence only)
    assert_eq!(one.claimed[0].lease_expires_at, 1_000 + 60_000);

    // limit 10 -> all three Ready rows in (available_at, run_id) order; the
    // parked and leased rows are skipped.
    let all = plan_claim(&rows, 1_000, 10, 60_000);
    let ids: Vec<&str> = all.claimed.iter().map(|c| c.run_id.as_str()).collect();
    assert_eq!(ids, ["a", "z", "b"]);
}

#[test]
fn plan_claim_and_partition_orders_carry_the_stream_seq_tiebreak() {
    // E4 (adopted at l5i9.17): the model mirrors the SQL's
    // (available_at, stream_seq, run_id) — a numerically-earlier stream position
    // dispatches first even when its run_id sorts lexically LATER. The run_ids
    // here are deliberately inverted against the seqs so a model that still
    // sorts (available_at, run_id) fails.
    let rows = vec![
        QueueEntry::ready("t1", "z-first", 100, 20).with_stream_seq(1),
        QueueEntry::ready("t1", "a-second", 100, 20).with_stream_seq(2),
    ];
    let all = plan_claim(&rows, 1_000, 10, 60_000);
    let ids: Vec<&str> = all.claimed.iter().map(|c| c.run_id.as_str()).collect();
    assert_eq!(
        ids,
        ["z-first", "a-second"],
        "numeric stream_seq outranks lexical run_id"
    );

    // The partition head orders carry the same tiebreak: under `blocking`
    // (stream order (enqueued_at, stream_seq, run_id)) the seq-1 row is the head
    // and seq-2 stays blocked behind it.
    let key_rows = vec![
        QueueEntry::ready_partition("t1", "z-first", "k", 100, 20).with_stream_seq(1),
        QueueEntry::ready_partition("t1", "a-second", "k", 100, 20).with_stream_seq(2),
    ];
    let owned: HashSet<&str> = HashSet::from(["k"]);
    let head = plan_partition_claim(&key_rows, &owned, 1_000, 10, 60_000);
    assert_eq!(head.claimed.len(), 1, "one in flight per partition");
    assert_eq!(
        head.claimed[0].run_id, "z-first",
        "the blocking head is the numerically-earliest stream position"
    );
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

#[test]
fn blocking_partition_orphan_wedges_instead_of_being_reaped() {
    let grace = 1_000;
    // An orphan-shaped row (expired lease past grace, budget spent) that belongs
    // to a blocking-policy partition is Wedged, not Orphaned (D20): reaping it
    // would silently release a key the flow chose to keep ordered.
    let wedged = QueueEntry {
        lease_expires_at: Some(1_000),
        attempts: 20,
        ..QueueEntry::ready_partition("t1", "pw", "site", 0, 20)
    };
    assert_eq!(
        janitor_verdict(&wedged, 1_000 + grace, grace),
        JanitorVerdict::Wedged
    );
    // The SAME shape under leapfrog IS reaped — the key releases on exhaustion.
    let leap = QueueEntry {
        lease_expires_at: Some(1_000),
        attempts: 20,
        ..QueueEntry::ready_partition("t1", "lp", "site2", 0, 20)
    }
    .with_policy(PartitionPolicy::Leapfrog);
    assert_eq!(
        janitor_verdict(&leap, 1_000 + grace, grace),
        JanitorVerdict::Orphaned
    );
    // An unpartitioned orphan is reaped regardless of the (inert) policy field.
    let unpart = QueueEntry {
        lease_expires_at: Some(1_000),
        attempts: 20,
        ..QueueEntry::ready("t1", "u", 0, 20)
    };
    assert_eq!(
        janitor_verdict(&unpart, 1_000 + grace, grace),
        JanitorVerdict::Orphaned
    );
    // `orphans()` excludes the wedged row — only the leapfrog + unpartitioned
    // orphans are swept.
    let rows = vec![wedged, leap, unpart];
    let swept: Vec<&str> = orphans(&rows, 1_000 + grace, grace)
        .iter()
        .map(|e| e.run_id.as_str())
        .collect();
    assert_eq!(swept, ["lp", "u"]);
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
    // E4: stream_seq (a numeric BIGINT) sits AHEAD of the text run_id in the claim
    // key, so evt runs (<flow>:evt:<stream_seq>) dispatch by numeric stream
    // position — f1:evt:10 must not sort before f1:evt:9. This pin is the
    // load-bearing drift-guard: with every row at stream_seq 0 today the runtime
    // ordering is unchanged, so only the string can catch a dropped tiebreak.
    assert!(sql.contains("ORDER BY c.available_at, c.stream_seq, c.run_id"));
    assert!(sql.contains("c.available_at <= now()"));
    assert!(sql.contains("c.lease_expires_at IS NULL OR c.lease_expires_at <= now()"));
    // The redelivery-budget guard: a spent row is left for the janitor, UNLESS its
    // lease was released by a park (NULL) — a woken budget-spent run wakes and
    // completes (wamn-fqg.7), it is not wedged invisible. The `OR ... IS NULL` disjunct
    // is the load-bearing drift-guard: the runtime gates are insensitive to some
    // SQL-builder mutations, so this shape assert pins the predicate.
    assert!(sql.contains("c.attempts < c.max_attempts OR c.lease_expires_at IS NULL"));
    // Crash-evidence increment: attempts bumps ONLY on an expired-lease reclaim (the
    // predicate established expired-or-NULL, so IS NOT NULL = the prior owner died).
    // A first claim and a park->wake re-claim (park releases the lease) are free —
    // an unconditional `+ 1` here burns the redelivery budget on every wake.
    assert!(sql.contains(
        "attempts = q.attempts + CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END"
    ));
    assert!(sql.contains("RETURNING q.run_id, q.attempts, q.lease_expires_at"));
    // The global claim leaves partitioned runs to the per-partition path.
    assert!(sql.contains("c.partition_key IS NULL"));
    // R8b-b: the shared claim scan carries the EXPLICIT tenant predicate (inert —
    // RLS injects the identical filter — but defense-in-depth, matching the
    // ack/prune/insert builders). Both claim paths get it via global_claim_cte.
    assert!(sql.contains("c.tenant_id = current_setting('app.tenant', true)"));
    // The locking scan lives in a CTE fenced `AS MATERIALIZED` so `FOR UPDATE SKIP
    // LOCKED LIMIT n` evaluates EXACTLY ONCE. Neither `WHERE (pk) IN (subquery)` nor a
    // plain `FROM (subquery)` derived table is a fence: the planner can put the LockRows
    // scan on the inner side of a nested-loop join and re-execute it per outer row, and
    // each rescan's SKIP LOCKED advances to fresh rows — over-leasing the whole batch on
    // a bounded claim (wamn-fqg.4; seen through the plugin's cached prepared-statement
    // path — a plain FROM-join did NOT fix it). This shape assert is the load-bearing
    // drift-guard: the over-claim is plan/timing-dependent so the runtime gate is FLAKY,
    // but `AS MATERIALIZED` structurally leases exactly `limit`, so pin it — dropping the
    // fence (or reverting to `IN`/plain-`FROM`) is caught here.
    assert!(sql.contains("WITH claimed AS MATERIALIZED ("));
    assert!(sql.contains("q.tenant_id = claimed.tenant_id AND q.run_id = claimed.run_id"));
    // Neither of the two bug shapes: not a `WHERE (pk) IN (subquery)`, and not a bare
    // `FROM (subquery)` derived table — both let the LockRows scan rescan and over-lease.
    assert!(!sql.contains(") IN ("));
    assert!(!sql.contains("FROM ("));
}

/// The fqg.18 combined claim/checkpoint/complete builders: each composes the
/// existing single-purpose statements — the composition (not re-derivation) is
/// the drift-guard, pinned by asserting the composed text CONTAINS the source
/// builder's text verbatim, plus the load-bearing clauses of each tail.
#[test]
fn combined_claim_and_checkpoint_builders_compose_the_split_statements() {
    // claim_dispatch = the claim scan (shared fragment with claim_batch_sql) +
    // the mark-running guard + the dispatch read + the run's PERSISTED
    // flow_version (wamn-cox: the plan-cache probe pins the run's own version,
    // not whatever is active now).
    let cd = claim_dispatch_sql();
    for pin in [
        // The claim scan, verbatim from the shared fragment (fence + predicate).
        "WITH claimed AS MATERIALIZED (",
        "c.partition_key IS NULL",
        "c.available_at <= now()",
        "c.lease_expires_at IS NULL OR c.lease_expires_at <= now()",
        "c.attempts < c.max_attempts OR c.lease_expires_at IS NULL",
        "ORDER BY c.available_at, c.stream_seq, c.run_id",
        "FOR UPDATE SKIP LOCKED",
        "LIMIT 1",
        "attempts = q.attempts + CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END",
        // The mark-running arm (the mark_running_sql guard, in-statement).
        "SET status = 'running'",
        "AND r.status = 'dispatched'",
        // The dispatch read + the PERSISTED flow_version (the plan-cache input).
        "r.flow_id, r.input_json::text, r.flow_version AS flow_version",
    ] {
        assert!(cd.contains(pin), "claim_dispatch_sql missing: {pin}");
    }
    // The 4th column is the run's OWN version column, never a max-over-active
    // subselect (the wamn-cox pin: a resume must not re-derive the active version).
    assert!(
        !cd.contains("SELECT max(f.version)") && !cd.contains("f.active"),
        "claim_dispatch_sql must project runs.flow_version, not the active-version probe: {cd}"
    );
    assert!(!cd.contains(") IN ("));
    assert!(!cd.contains("FROM ("));
    // The scan is shared with claim_batch_sql structurally — same fragment, so
    // the batch claim's exact scan text appears inside the combined claim.
    let batch = claim_batch_sql(1);
    let scan_start = batch.find("SELECT c.tenant_id").unwrap();
    let scan_end = batch.find("LIMIT 1").unwrap() + "LIMIT 1".len();
    assert!(
        cd.contains(&batch[scan_start..scan_end]),
        "claim_dispatch_sql and claim_batch_sql have drifted apart on the claim scan"
    );

    // complete_dequeue = update_run_completed_sql (fqg.2 unconditional override)
    // + dequeue_sql, sharing $1 — one atomic statement.
    let cq = complete_dequeue_sql();
    assert!(
        cq.contains(&wamn_run_store::sql::update_run_completed_sql()),
        "complete_dequeue_sql no longer composes update_run_completed_sql verbatim"
    );
    assert!(
        cq.contains(&dequeue_sql()),
        "complete_dequeue_sql no longer composes dequeue_sql verbatim"
    );

    // record+renew = the 5.7 checkpoint insert verbatim + the owner-guarded
    // renew tail; param numbering pinned ($8/$9 success, $9/$10 error).
    let rs = record_success_and_renew_sql();
    assert!(
        rs.contains(&wamn_run_store::sql::insert_node_run_success_sql()),
        "record_success_and_renew_sql no longer composes insert_node_run_success_sql verbatim"
    );
    assert!(rs.contains("$8::bigint * interval '1 millisecond'"));
    assert!(rs.contains("AND run_id = $1 AND lease_owner = $9"));
    assert!(!rs.contains("$10"));
    let re = record_error_and_renew_sql();
    assert!(
        re.contains(&wamn_run_store::sql::insert_node_run_error_sql()),
        "record_error_and_renew_sql no longer composes insert_node_run_error_sql verbatim"
    );
    assert!(re.contains("$9::bigint * interval '1 millisecond'"));
    assert!(re.contains("AND run_id = $1 AND lease_owner = $10"));
    assert!(!re.contains("$11"));
}

// ---- wamn-v8cv: the terminal dead-letter dequeue ---------------------------

/// The composed terminal dequeue (D20 dead-letter + continue): the plain
/// dequeue rides VERBATIM (shared `$1`), the ledger insert is scoped to
/// blocking-partitioned rows in the SAME statement, and the arity is exactly
/// `$1` run_id + `$2` reason. A mutation that widens the predicate (any
/// partitioned row), narrows it (leapfrog), or drops the ledger half fails here
/// by string; the live gate proves the behaviour.
#[test]
fn dead_letter_dequeue_composes_the_ledger_insert_with_the_dequeue() {
    let dl = dead_letter_dequeue_sql();
    assert!(
        dl.contains(&dequeue_sql()),
        "dead_letter_dequeue_sql no longer composes dequeue_sql verbatim"
    );
    assert!(dl.contains(
        "INSERT INTO run_dead_letters (tenant_id, run_id, partition_key, flow_id, reason)"
    ));
    // Blocking-partitioned only — the strict-ordering promise is what the marker
    // records; unpartitioned/leapfrog rows degenerate to the plain dequeue.
    assert!(dl.contains("q.partition_key IS NOT NULL"));
    assert!(dl.contains("q.partition_policy = 'blocking'"));
    assert!(!dl.contains("'leapfrog'"));
    // flow_id rides from the run's own row; redelivery collapses on the PK.
    assert!(dl.contains("JOIN runs AS r ON r.tenant_id = q.tenant_id AND r.run_id = q.run_id"));
    assert!(dl.contains("ON CONFLICT (tenant_id, run_id) DO NOTHING"));
    // R8b-b: the explicit tenant predicate, like every other builder.
    assert!(dl.contains("q.tenant_id = current_setting('app.tenant', true)"));
    // Arity: $1 (shared run_id) + $2 (reason), nothing beyond.
    assert!(dl.contains("$2"));
    assert!(!dl.contains("$3"));
}

/// The pure twin of the insert predicate: only a blocking-policy PARTITIONED
/// row dead-letters on a guest-observed terminal failure.
#[test]
fn dead_letters_on_terminal_is_blocking_partitioned_only() {
    let unpartitioned = QueueEntry::ready("t1", "r", 0, 20);
    assert!(!dead_letters_on_terminal(&unpartitioned));
    // The column default IS blocking (D20) — a plain keyed row dead-letters.
    let blocking = QueueEntry::ready_partition("t1", "r", "k", 0, 20);
    assert!(dead_letters_on_terminal(&blocking));
    let leapfrog =
        QueueEntry::ready_partition("t1", "r", "k", 0, 20).with_policy(PartitionPolicy::Leapfrog);
    assert!(!dead_letters_on_terminal(&leapfrog));
    // The policy column is inert without a key: still no ledger row.
    let unpart_blocking =
        QueueEntry::ready("t1", "r", 0, 20).with_policy(PartitionPolicy::Blocking);
    assert!(!dead_letters_on_terminal(&unpart_blocking));
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
    // A key with a woken budget-spent head (lease released by a park) is still
    // acquirable — the same wamn-fqg.7 disjunct as the global claim.
    assert!(acq.contains("q.attempts < q.max_attempts OR q.lease_expires_at IS NULL"));

    // Claim head: owned partitions only, one-in-flight + head-first, SKIP LOCKED.
    let claim = claim_partition_head_sql(8);
    assert!(claim.contains("JOIN partition_owner AS o"));
    assert!(claim.contains("o.lease_owner = $1 AND o.lease_expires_at > now()"));
    assert!(claim.contains("c.partition_key IS NOT NULL"));
    // The NOT EXISTS reduces each partition to a single head candidate, which is
    // what makes FOR UPDATE OF c (no DISTINCT) legal. Its disjuncts are the
    // ordering guards: a live-leased sibling (one-in-flight) plus the per-policy
    // head-first arm (D20). The behavioral live-apply gate proves the in-flight
    // branch is the SOLE blocker of a successor while its head is live-leased
    // (on leapfrog-policy fixtures, where the stream-order arm cannot mask it).
    assert!(claim.contains("NOT EXISTS"));
    assert!(claim.contains("b.lease_expires_at IS NOT NULL AND b.lease_expires_at > now()"));
    // D20 blocking (default): ANY earlier sibling in the STREAM order — stamped
    // (enqueued_at, run_id), which a park/backoff never moves — blocks the head.
    // These pins are the load-bearing drift guard for the policy branch: the
    // runtime effect is timing/plan-dependent, the strings are deterministic.
    assert!(claim.contains("c.partition_policy = 'blocking'"));
    // E4: stream_seq rides AHEAD of run_id in BOTH per-key orders (blocking stream
    // order + leapfrog ready order), so an evt-keyed partition advances numerically.
    assert!(claim.contains(
        "(b.enqueued_at, b.stream_seq, b.run_id) < (c.enqueued_at, c.stream_seq, c.run_id)"
    ));
    // D20 leapfrog (opt-in): only an earlier CURRENTLY-READY sibling blocks, in
    // (available_at, stream_seq, run_id) order — the pre-D20 behavior, now explicit.
    assert!(claim.contains("c.partition_policy = 'leapfrog'"));
    assert!(claim.contains(
        "(b.available_at, b.stream_seq, b.run_id) < (c.available_at, c.stream_seq, c.run_id)"
    ));
    assert!(claim.contains("FOR UPDATE OF c SKIP LOCKED"));
    // wamn-fqg.7: the budget disjunct is on BOTH the head candidate `c` and the
    // earlier-ready-sibling sub-check `b`, so a woken budget-spent head is claimable
    // AND still blocks its later siblings (in-order preserved).
    assert!(claim.contains("c.attempts < c.max_attempts OR c.lease_expires_at IS NULL"));
    assert!(claim.contains("b.attempts < b.max_attempts OR b.lease_expires_at IS NULL"));
    assert!(claim.contains("LIMIT 8"));
    // Same crash-evidence increment as the global claim: a parked head is re-claimed
    // on EVERY wake, so this path would burn the budget fastest unconditionally.
    assert!(claim.contains(
        "attempts = q.attempts + CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END"
    ));
    assert!(claim.contains("RETURNING q.run_id, q.partition_key, q.attempts, q.lease_expires_at"));
    // The head selection is fenced `AS MATERIALIZED` (same over-lock fix as the global
    // claim, wamn-fqg.10): `FOR UPDATE OF c SKIP LOCKED LIMIT n` runs once, then the
    // outer UPDATE joins the materialized heads by PK — no re-scannable `IN (subquery)`
    // that could lease more than `limit` heads under a nested-loop plan.
    assert!(claim.contains("WITH heads AS MATERIALIZED ("));
    assert!(claim.contains("q.tenant_id = heads.tenant_id AND q.run_id = heads.run_id"));
    assert!(!claim.contains(") IN ("));
    assert!(!claim.contains("FROM ("));

    // Acquire / renew / release / gc carry an explicit tenant claim. (The head
    // claim is tenant-scoped purely by RLS on run_queue + partition_owner — it
    // writes no explicit app.tenant literal. Unlike the GLOBAL claim, R8b-b did NOT
    // add the explicit predicate here: this builder is out of that finding's scope.)
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
    // The head claim relies on RLS, not an explicit tenant literal (R8b-b left it).
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

#[test]
fn budget_spent_null_lease_wakes_but_expired_lease_stays_exhausted() {
    // wamn-fqg.7: the wedge. A budget-spent run that PARKED (park released the lease,
    // proving the last owner was alive) must WAKE — a NULL lease is never crash
    // evidence. A budget-spent run still holding an *expired* lease (a crash after the
    // budget was spent) stays Exhausted for the janitor. The one distinguishing signal
    // is `lease_expires_at`: NULL = a live park (Ready), Some = crash evidence.
    let now = 100;

    // Woken: attempts at max, lease released by a park, due -> Ready (the fix).
    let woken = QueueEntry {
        attempts: 20,
        ..QueueEntry::ready("t1", "wedge", 50, 20)
    };
    assert_eq!(claim_state(&woken, now), ClaimState::Ready);
    assert!(is_claimable(&woken, now));

    // Poison: attempts at max, lease EXPIRED (not released) -> Exhausted (janitor's).
    let poison = QueueEntry {
        lease_owner: Some("dead".into()),
        lease_expires_at: Some(80),
        attempts: 20,
        ..QueueEntry::ready("t1", "poison", 50, 20)
    };
    assert_eq!(claim_state(&poison, now), ClaimState::Exhausted);
    assert!(!is_claimable(&poison, now));

    // Regression: an expired lease with budget REMAINING is still a plain reclaim.
    let reclaim = QueueEntry {
        lease_owner: Some("dead".into()),
        lease_expires_at: Some(80),
        attempts: 2,
        ..QueueEntry::ready("t1", "reclaim", 50, 20)
    };
    assert_eq!(claim_state(&reclaim, now), ClaimState::Ready);

    // plan_claim takes the woken row, and the crash-evidence CASE leaves its attempts
    // UNCHANGED (a NULL lease is not bumped) — waking a park costs no redelivery budget.
    let plan = plan_claim(std::slice::from_ref(&woken), now, 10, 60_000);
    assert_eq!(plan.claimed.len(), 1);
    assert_eq!(plan.claimed[0].run_id, "wedge");
    assert_eq!(
        plan.claimed[0].attempts, 20,
        "waking a park does not bump attempts"
    );
    // ...and skips the poison row (left for the janitor).
    assert!(
        plan_claim(std::slice::from_ref(&poison), now, 10, 60_000)
            .claimed
            .is_empty()
    );

    // The partition path (is_claimable-routed) wakes a budget-spent NULL-lease head too,
    // and an earlier such head still blocks its later sibling (in-order preserved).
    let owned: HashSet<&str> = ["site-w"].into_iter().collect();
    let woken_head = QueueEntry {
        attempts: 20,
        ..QueueEntry::ready_partition("t1", "pw-0", "site-w", 50, 20)
    };
    let later_sibling = QueueEntry::ready_partition("t1", "pw-1", "site-w", 60, 20);
    let plan = plan_partition_claim(&[woken_head, later_sibling], &owned, now, 10, 60_000);
    let ids: Vec<&str> = plan.claimed.iter().map(|c| c.run_id.as_str()).collect();
    assert_eq!(
        ids,
        ["pw-0"],
        "the woken budget-spent head is claimed; its later sibling waits"
    );
    assert_eq!(
        plan.claimed[0].attempts, 20,
        "waking a partition head park does not bump attempts"
    );
}

// ---- crash-evidence attempts (park/wake is free) -----------------------------

#[test]
fn park_wake_cycles_never_consume_the_redelivery_budget() {
    // A delay-loop flow with max_attempts = 1 parks N times and stays claimable at
    // EVERY wake: park releases the lease, so the wake re-claim sees no crash
    // evidence and attempts stays 0. Before the fix each claim bumped attempts, so
    // the second wake already classified the run Exhausted — a run that never
    // failed, killed for sleeping.
    let mut entry = QueueEntry::ready("t1", "r", 0, 1); // budget: ONE crash allowed
    let mut now = 100;
    for wake in 0..10 {
        assert_eq!(
            claim_state(&entry, now),
            ClaimState::Ready,
            "wake {wake}: a parked-and-woken run must stay claimable"
        );
        let plan = plan_claim(std::slice::from_ref(&entry), now, 1, 1_000);
        assert_eq!(plan.claimed.len(), 1);
        assert_eq!(plan.claimed[0].attempts, 0, "wake {wake}: re-claim is free");
        // The runner parks (park_sql: lease released, available_at pushed out).
        entry = QueueEntry {
            attempts: plan.claimed[0].attempts,
            ..QueueEntry::ready("t1", "r", now + 500, 1)
        };
        now += 1_000; // the wake: available_at has arrived
    }
}

#[test]
fn crash_loop_exhausts_at_exactly_max_attempts() {
    // Repeated expired-lease reclaims each count one unit of crash evidence; the
    // row classifies Exhausted (left for the janitor) exactly when attempts reaches
    // max_attempts. max_attempts = "how many times may a runner die holding this
    // run": the first dispatch is free, so a budget of N tolerates N deaths
    // (N+1 deliveries) — crash-loops still retire.
    let max = 3;
    let mut entry = QueueEntry::ready("t1", "r", 0, max);
    let mut now = 100;
    let mut deliveries = 0;
    while is_claimable(&entry, now) {
        let plan = plan_claim(std::slice::from_ref(&entry), now, 1, 1_000);
        deliveries += 1;
        // The claimant crashes: its lease expires unreleased (crash evidence).
        entry = QueueEntry {
            lease_owner: Some(format!("dead-{deliveries}")),
            lease_expires_at: Some(plan.claimed[0].lease_expires_at),
            attempts: plan.claimed[0].attempts,
            ..entry
        };
        now = plan.claimed[0].lease_expires_at + 1;
    }
    assert_eq!(entry.attempts, max, "spent at exactly max_attempts");
    assert_eq!(claim_state(&entry, now), ClaimState::Exhausted);
    assert_eq!(
        deliveries,
        max + 1,
        "first dispatch free + max counted reclaims"
    );
}

#[test]
fn park_wake_crash_reclaim_costs_one_unit_not_three() {
    // claim -> park -> wake-claim -> crash -> reclaim: only the crash counts.
    let fresh = QueueEntry::ready("t1", "r", 0, 20);
    let first = plan_claim(std::slice::from_ref(&fresh), 100, 1, 1_000);
    assert_eq!(first.claimed[0].attempts, 0); // first dispatch: free

    // The runner parks; at wake the re-claim is free too.
    let parked = QueueEntry {
        attempts: first.claimed[0].attempts,
        ..QueueEntry::ready("t1", "r", 5_000, 20)
    };
    let woken = plan_claim(std::slice::from_ref(&parked), 6_000, 1, 1_000);
    assert_eq!(woken.claimed[0].attempts, 0); // wake re-claim: free

    // The runner CRASHES holding the run: its lease expires unreleased, and the
    // reclaim counts exactly one unit — not three.
    let crashed = QueueEntry {
        lease_owner: Some("dead".into()),
        lease_expires_at: Some(woken.claimed[0].lease_expires_at),
        attempts: woken.claimed[0].attempts,
        ..QueueEntry::ready("t1", "r", 5_000, 20)
    };
    let reclaimed = plan_claim(std::slice::from_ref(&crashed), 60_000, 1, 1_000);
    assert_eq!(reclaimed.claimed[0].attempts, 1);

    // The partition head claim mirrors the same rule (both claim paths).
    let owned: HashSet<&str> = ["site-1"].into_iter().collect();
    let fresh_head = QueueEntry::ready_partition("t1", "s1-0", "site-1", 0, 20);
    let plan = plan_partition_claim(std::slice::from_ref(&fresh_head), &owned, 100, 10, 1_000);
    assert_eq!(plan.claimed[0].attempts, 0); // first head claim: free
    let crashed_head = QueueEntry {
        lease_owner: Some("dead".into()),
        lease_expires_at: Some(50),
        ..fresh_head.clone()
    };
    let plan = plan_partition_claim(std::slice::from_ref(&crashed_head), &owned, 100, 10, 1_000);
    assert_eq!(plan.claimed[0].attempts, 1); // expired-lease reclaim: counted

    // A NONZERO base pins the ADDITIVE term of the partition mirror: prior crash
    // evidence accumulates (2 + 1), so a "set to 1 on reclaim" implementation —
    // indistinguishable from 0 + 1 in every from-zero assert above — fails here.
    let repeat_crash = QueueEntry {
        attempts: 2,
        ..crashed_head
    };
    let plan = plan_partition_claim(std::slice::from_ref(&repeat_crash), &owned, 100, 10, 1_000);
    assert_eq!(plan.claimed[0].attempts, 3);
    // And a free wake-claim preserves (never resets) accumulated crash evidence.
    let parked_after_crashes = QueueEntry {
        attempts: 2,
        ..fresh_head
    };
    let plan = plan_partition_claim(
        std::slice::from_ref(&parked_after_crashes),
        &owned,
        100,
        10,
        1_000,
    );
    assert_eq!(plan.claimed[0].attempts, 2);
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
    assert_eq!(plan.claimed[0].attempts, 0); // never leased -> first claim is FREE (crash evidence only)
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

/// fqg.9: the GUEST loop (`components/flowrunner` `claim_partition_run`) modelled
/// purely — accumulate ownership with `plan_acquire(.., 1)`, claim ONE head with
/// `plan_partition_claim(.., 1)`, "drive + dequeue" it, repeat — over partitions
/// with MIXED `(enqueued_at, stream_seq)` so the per-key stream order differs from
/// run-id order. The drive order, filtered per key, must be each key's blocking
/// stream order `(enqueued_at, stream_seq, run_id)` — never lexical run-id order.
/// This is the pure-plan mutant surface for the guest's partitioned dispatch:
/// dropping the `stream_seq` or `enqueued_at` field from the head decision
/// re-orders a key and fails here.
#[test]
fn guest_partition_loop_drives_each_key_in_stream_order() {
    // A partitioned row available now (available_at 0), with an explicit
    // (enqueued_at, stream_seq) — the blocking stream-order coordinates.
    let row = |run_id: &str, key: &str, enq: i64, seq: i64| QueueEntry {
        enqueued_at: enq,
        stream_seq: seq,
        ..QueueEntry::ready_partition("t1", run_id, key, 0, 20)
    };
    // kA: run-id order a0,a1,a2,a3 -> stream order a2,a1,a0,a3 (differs on every
    // field). kB: b0,b1,b2 -> b2,b1,b0.
    let rows = vec![
        row("a0", "kA", 100, 2),
        row("a1", "kA", 100, 1),
        row("a2", "kA", 50, 9),
        row("a3", "kA", 200, 0),
        row("b0", "kB", 300, 5),
        row("b1", "kB", 100, 5),
        row("b2", "kB", 100, 4),
    ];
    let now: i64 = 1_000;
    let ttl: i64 = 60_000;
    let total = rows.len();

    let mut remaining = rows;
    let mut owned: HashSet<String> = HashSet::new();
    let mut drive_order: Vec<String> = Vec::new();
    // Bound the loop so a stuck model fails loudly instead of hanging.
    for _ in 0..(total + 4) {
        // acquire up to one NOT-yet-owned partition (live leases exclude the rest).
        let owners: Vec<PartitionOwner> = owned
            .iter()
            .map(|k| PartitionOwner::new("t1", k.as_str(), "me", now + ttl))
            .collect();
        for k in plan_acquire(&remaining, &owners, now, 1) {
            owned.insert(k);
        }
        // claim ONE head across the owned partitions, then "drive + dequeue" it.
        let owned_set: HashSet<&str> = owned.iter().map(String::as_str).collect();
        let plan = plan_partition_claim(&remaining, &owned_set, now, 1, ttl);
        let Some(head) = plan.claimed.first() else {
            break;
        };
        let run_id = head.run_id.clone();
        drive_order.push(run_id.clone());
        remaining.retain(|e| e.run_id != run_id);
    }

    // Every run dispatched exactly once.
    assert_eq!(drive_order.len(), total, "drive order: {drive_order:?}");
    // Per-key subsequence (run_ids share the key's leading letter) == the blocking
    // stream order, NOT run-id order.
    let per_key = |prefix: char| -> Vec<String> {
        drive_order
            .iter()
            .filter(|id| id.starts_with(prefix))
            .cloned()
            .collect()
    };
    assert_eq!(per_key('a'), ["a2", "a1", "a0", "a3"]);
    assert_eq!(per_key('b'), ["b2", "b1", "b0"]);
    // Guard the discriminator: the expected order is NOT the lexical run-id order,
    // so a head decision that ignored stream_seq/enqueued_at would fail above.
    assert_ne!(per_key('a'), ["a0", "a1", "a2", "a3"]);
}

#[test]
fn partition_policy_decides_whether_a_later_run_overtakes_an_unavailable_head() {
    // The R6 decision (D20): what a key does while its earliest (head) run is
    // unavailable. The stream head `p-0` (earliest by enqueued_at) is backed off
    // into the future (available_at 5_000 at now=1_000); the later `p-1`
    // (enqueued after it) is ready now.
    let owned: HashSet<&str> = ["site"].into_iter().collect();
    let backed_off_head = QueueEntry {
        available_at: 5_000, // parked/backed-off — not yet due
        enqueued_at: 100,    // but FIRST in the key's stream order
        ..QueueEntry::ready_partition("t1", "p-0", "site", 5_000, 20)
    };
    let ready_later = QueueEntry {
        enqueued_at: 200, // enqueued AFTER p-0
        ..QueueEntry::ready_partition("t1", "p-1", "site", 100, 20)
    };

    // Blocking (default): the backed-off head still blocks its key — `p-1` does
    // NOT overtake, so the key dispatches nothing until `p-0` becomes due (the
    // Kafka model; the corruption R6 exists to forbid). `blocks` ranks by the
    // stable stream order, which a park never moves.
    let blk_head = backed_off_head
        .clone()
        .with_policy(PartitionPolicy::Blocking);
    let blk_later = ready_later.clone().with_policy(PartitionPolicy::Blocking);
    let plan = plan_partition_claim(&[blk_head, blk_later], &owned, 1_000, 10, 60_000);
    assert!(
        plan.claimed.is_empty(),
        "blocking: a backed-off head holds the key, p-1 must not overtake"
    );

    // Leapfrog (opt-in): the backed-off head yields; the later ready run
    // overtakes (pre-D20 behavior). Only currently-ready siblings block.
    let lf_head = backed_off_head.with_policy(PartitionPolicy::Leapfrog);
    let lf_later = ready_later.with_policy(PartitionPolicy::Leapfrog);
    let plan = plan_partition_claim(&[lf_head, lf_later], &owned, 1_000, 10, 60_000);
    let ids: Vec<&str> = plan.claimed.iter().map(|c| c.run_id.as_str()).collect();
    assert_eq!(
        ids,
        ["p-1"],
        "leapfrog: a later ready run overtakes an unavailable head"
    );
}

#[test]
fn blocking_wedges_a_key_behind_an_exhausted_head_leapfrog_releases_it() {
    // The terminal fold-in (D20): a budget-exhausted head (expired lease, budget
    // spent → Exhausted, awaiting the janitor) that is FIRST in the stream order.
    let owned: HashSet<&str> = ["site"].into_iter().collect();
    let exhausted_head = QueueEntry {
        lease_expires_at: Some(500), // expired at now=1_000
        attempts: 20,
        max_attempts: 20,
        enqueued_at: 100, // first in stream order
        ..QueueEntry::ready_partition("t1", "e-0", "site", 0, 20)
    };
    let ready_later = QueueEntry {
        enqueued_at: 200,
        ..QueueEntry::ready_partition("t1", "e-1", "site", 0, 20)
    };

    // Blocking: the exhausted head still blocks — the key WEDGES (nothing
    // dispatches) until an operator clears the head. The janitor leaves the row
    // (see `blocking_partition_orphan_wedges_instead_of_being_reaped`), so the
    // wedge persists rather than silently releasing.
    let blk = plan_partition_claim(
        &[
            exhausted_head
                .clone()
                .with_policy(PartitionPolicy::Blocking),
            ready_later.clone().with_policy(PartitionPolicy::Blocking),
        ],
        &owned,
        1_000,
        10,
        60_000,
    );
    assert!(
        blk.claimed.is_empty(),
        "blocking: an exhausted head wedges its key"
    );

    // Leapfrog: the exhausted head (not currently ready) does not block; `e-1`
    // dispatches and the janitor's verdict on `e-0` releases the key.
    let lf = plan_partition_claim(
        &[
            exhausted_head.with_policy(PartitionPolicy::Leapfrog),
            ready_later.with_policy(PartitionPolicy::Leapfrog),
        ],
        &owned,
        1_000,
        10,
        60_000,
    );
    let ids: Vec<&str> = lf.claimed.iter().map(|c| c.run_id.as_str()).collect();
    assert_eq!(ids, ["e-1"], "leapfrog: an exhausted head releases its key");
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
    // D20 terminal fold-in: a blocking-policy partitioned row is EXEMPT from the
    // sweep — it stays and wedges its key. Only unpartitioned rows and leapfrog
    // partitions are reaped. (The pin is the load-bearing drift guard: dropping
    // the DELETE arm silently un-wedges every blocking key.)
    assert!(
        sweep.contains("q.partition_key IS NULL OR q.partition_policy = 'leapfrog'"),
        "janitor sweep must exempt a blocking-policy partition (D20 wedge): {sweep}"
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

    // The policy-materializing enqueue (D20) writes the partition_policy column
    // from a $5 literal, so the claim SQL branches on the row, not a flow join.
    let ewp = enqueue_with_policy_sql();
    assert!(ewp.contains("current_setting('app.tenant', true)"));
    assert!(ewp.contains("partition_policy"));
    assert!(ewp.contains("$5"));
    assert!(ewp.contains("ON CONFLICT (tenant_id, run_id) DO NOTHING"));
    // The two policy literals match the model's as_sql (drift guard over the CHECK).
    assert_eq!(PartitionPolicy::Blocking.as_sql(), "blocking");
    assert_eq!(PartitionPolicy::Leapfrog.as_sql(), "leapfrog");
    assert_eq!(
        PartitionPolicy::from_sql("leapfrog"),
        Some(PartitionPolicy::Leapfrog)
    );
    assert_eq!(PartitionPolicy::from_sql("nope"), None);
    assert_eq!(PartitionPolicy::default(), PartitionPolicy::Blocking);
}

#[test]
fn evt_enqueue_builders_carry_stream_seq_and_stay_kq0z_coherent() {
    // E4/l5i9.17: the materializer's enqueue writes the REAL stream position;
    // the pins are the load-bearing drift guard (runtime gates can be
    // insensitive to a dropped column while every row is still 0).
    let unkeyed = enqueue_evt_sql();
    assert!(unkeyed.contains("current_setting('app.tenant', true)"));
    assert!(unkeyed.contains("stream_seq"));
    assert!(unkeyed.contains("$5"));
    assert!(unkeyed.contains("ON CONFLICT (tenant_id, run_id) DO NOTHING"));
    // kq0z coherence: the UNKEYED evt row takes the column-default policy —
    // writing the policy column without a key would decohere the D20 stamp.
    assert!(
        !unkeyed.contains("partition_policy"),
        "an unkeyed evt enqueue must leave partition_policy to the column default"
    );

    let keyed = enqueue_evt_with_policy_sql();
    assert!(keyed.contains("current_setting('app.tenant', true)"));
    assert!(keyed.contains("stream_seq"));
    assert!(keyed.contains("partition_policy"));
    assert!(keyed.contains("$6"));
    assert!(keyed.contains("ON CONFLICT (tenant_id, run_id) DO NOTHING"));

    // The mint's zero-pad (the belt) keeps lexical order equal to numeric order.
    assert_eq!(mint_evt_run_id("f1", 9), "f1:evt:00000000000000000009");
    assert!(mint_evt_run_id("f1", 9) < mint_evt_run_id("f1", 10));
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
fn cron_error_variants_pin_the_failure_mode() {
    // SR5: CronError is a structured enum, one variant per failure mode, folded
    // mechanically from the exact construction site — not a stringly-typed error.
    // An unparseable schedule is INVALID-EXPRESSION (the parse() site).
    assert!(matches!(
        next_fire("not a cron", 0),
        Err(CronError::InvalidExpression { .. })
    ));
    // A parseable-but-unsatisfiable calendar is NO-OCCURRENCE (croner's search
    // fails), on BOTH the next_fire and due_tick paths.
    assert!(matches!(
        next_fire("0 0 30 2 *", JAN1_2026),
        Err(CronError::NoOccurrence { .. })
    ));
    assert!(matches!(
        due_tick("0 0 30 2 *", JAN1_2026, JAN1_2026 + DAY),
        Err(CronError::NoOccurrence { .. })
    ));
    // An instant past the representable DateTime<Utc> horizon is OUT-OF-RANGE
    // (the private to_dt() site, reached here via next_fire(after)).
    assert!(matches!(
        next_fire("* * * * *", i64::MAX),
        Err(CronError::OutOfRangeInstant { ms }) if ms == i64::MAX
    ));
    // Display preserves the `cron: …` log shape the dispatcher quarantine records.
    for e in [
        CronError::InvalidExpression {
            schedule: "x".into(),
            detail: "bad".into(),
        },
        CronError::OutOfRangeInstant { ms: i64::MAX },
        CronError::NoOccurrence {
            schedule: "0 0 30 2 *".into(),
            detail: "none".into(),
        },
    ] {
        assert!(e.to_string().starts_with("cron: "), "{e}");
    }
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

// ---- trigger dispatcher: adaptive cadence ---------------------------------------

#[test]
fn adaptive_interval_tightens_on_work_and_decays_to_max() {
    let cadence = Cadence::new(DEFAULT_MIN_INTERVAL_MS, DEFAULT_MAX_INTERVAL_MS).unwrap();
    let (min, max) = (cadence.min(), cadence.max());
    // Work snaps the cadence to the tight bound, from anywhere.
    assert_eq!(cadence.next_interval(max, true), min);
    assert_eq!(cadence.next_interval(min, true), min);
    // Idleness decays exponentially and caps at max (the reconciliation band).
    assert_eq!(cadence.next_interval(min, false), 2 * min);
    assert_eq!(cadence.next_interval(2 * min, false), 4 * min);
    assert_eq!(cadence.next_interval(20_000, false), max); // 40k clamps to 30k
    assert_eq!(cadence.next_interval(max, false), max);
    // A degenerate current clamps up into the band.
    assert_eq!(cadence.next_interval(0, false), min);
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

    // wamn-fqg.6: the DURABLE anchor recovery (the primary path, demoting
    // cron_last_run_sql to a bootstrap fallback) reads last_tick from the
    // cron_anchor table, tenant-scoped and flow-keyed ($1).
    let anchor = cron_anchor_sql();
    assert!(anchor.contains("SELECT last_tick FROM cron_anchor"));
    assert!(anchor.contains("flow_id = $1"));
    assert!(anchor.contains("current_setting('app.tenant', true)"));

    // The anchor upsert co-transacted with the fire: monotonic (GREATEST) so a
    // losing replica / redelivery / misfire-collapse never REWINDS the anchor
    // (a rewind would let an already-fired tick re-fire), keyed on the PK.
    let up = upsert_cron_anchor_sql();
    assert!(up.contains("INSERT INTO cron_anchor (tenant_id, flow_id, last_tick)"));
    assert!(up.contains("current_setting('app.tenant', true), $1, $2"));
    assert!(up.contains("ON CONFLICT (tenant_id, flow_id) DO UPDATE"));
    assert!(up.contains("GREATEST(cron_anchor.last_tick, EXCLUDED.last_tick)"));
    assert!(!up.contains("LEAST("));

    // The registry scan: active flows only; the trigger lives in graph_json.
    // R8b-b: the tenant predicate now precedes the `active` filter (explicit
    // defense-in-depth; inert under RLS).
    let flows = active_flows_sql();
    assert!(flows.contains("tenant_id = current_setting('app.tenant', true) AND active"));
    assert!(flows.contains("graph_json::text"));

    // The wake/reconciliation scan mirrors the claim predicate (due,
    // unleased-or-expired, budget remaining) but is strictly read-only.
    let wake = parked_due_sql(100);
    assert!(wake.contains("available_at <= now()"));
    assert!(wake.contains("lease_expires_at IS NULL OR lease_expires_at <= now()"));
    // wamn-2jkm.29: the wake scan carries the SAME partition guard the global
    // claim CTE carries, so it surfaces only rows the global claim would take
    // (ACTIONABLE work). Without it a partitioned follower wedged behind a D20
    // blocking head is surfaced every sweep and pins found_work()/cadence at min.
    assert!(wake.contains("partition_key IS NULL"));
    // R8b-b: the explicit tenant predicate (inert under RLS, defense-in-depth).
    assert!(wake.contains("tenant_id = current_setting('app.tenant', true)"));
    // Mirrors the claim predicate incl. the wamn-fqg.7 disjunct: a woken budget-spent
    // (NULL-lease) run is surfaced for a doorbell hint, not left invisible.
    assert!(wake.contains("attempts < max_attempts OR lease_expires_at IS NULL"));
    // E4: the wake scan mirrors the claim's numeric stream-position ordering.
    assert!(wake.contains("ORDER BY available_at, stream_seq, run_id"));
    assert!(wake.contains("LIMIT 100"));
    assert!(!wake.contains("FOR UPDATE"));
    assert!(!wake.contains("UPDATE "));
}

// [EVT-TEARDOWN l5i9.19]: the outbox table + its builders/DDL pins are gone —
// row events are the event plane's (CDC reader → JetStream → materializer).

// ---- record JSON round-trip ------------------------------------------------

#[test]
fn queue_entry_round_trips_as_kebab_json() {
    let e = QueueEntry {
        partition_key: Some("site-7".into()),
        priority: 5,
        lease_owner: Some("replica-2".into()),
        lease_expires_at: Some(1_700_000_000_000),
        attempts: 2,
        enqueued_at: 1_699_999_998_000,
        ..QueueEntry::ready("t1", "run-9", 1_699_999_999_000, 20)
            .with_policy(PartitionPolicy::Leapfrog)
            .with_stream_seq(42)
    };
    let json = serde_json::to_string(&e).unwrap();
    assert!(json.contains("\"partition-key\":\"site-7\""));
    assert!(json.contains("\"partition-policy\":\"leapfrog\""));
    assert!(json.contains("\"stream-seq\":42"));
    assert!(json.contains("\"enqueued-at\":1699999998000"));
    assert!(json.contains("\"lease-expires-at\":1700000000000"));
    assert!(json.contains("\"max-attempts\":20"));
    assert_eq!(serde_json::from_str::<QueueEntry>(&json).unwrap(), e);

    // A ready row omits the optional lease fields.
    let ready = QueueEntry::ready("t1", "r", 0, 20);
    let rj = serde_json::to_string(&ready).unwrap();
    assert!(!rj.contains("lease-owner"));
    assert!(!rj.contains("partition-key"));
}

// ---- deploy/sql/run-queue.sql drift guard --------------------------------------

#[test]
fn run_queue_sql_matches_the_model() {
    let sql = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/sql/run-queue.sql"
    ))
    .expect("read deploy/sql/run-queue.sql");

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
        "partition_policy",
        "priority",
        "available_at",
        "stream_seq",
        "lease_owner",
        "lease_expires_at",
        "attempts",
        "max_attempts",
    ] {
        assert!(sql.contains(col), "run-queue.sql missing column {col}");
    }
    // E4: stream_seq is a BIGINT (numeric semantics, no width ceiling) defaulting
    // to 0, and sits in the claimable index's ordering prefix so the numeric claim
    // key is index-supported.
    assert!(sql.contains("stream_seq       bigint NOT NULL DEFAULT 0"));
    assert!(
        sql.contains("run_queue_claimable ON wamn_run.run_queue (tenant_id, available_at, stream_seq, lease_expires_at)"),
        "the claimable index must carry stream_seq in its ordering prefix (E4)"
    );
    // D20: partition_policy defaults to 'blocking' and is CHECK-constrained to the
    // model's two literals (drift guard over PartitionPolicy::as_sql).
    assert!(sql.contains("partition_policy text NOT NULL DEFAULT 'blocking'"));
    for p in PartitionPolicy::ALL {
        assert!(
            sql.contains(&format!("'{}'", p.as_sql())),
            "run-queue.sql CHECK missing policy literal {}",
            p.as_sql()
        );
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

    // wamn-v8cv: the terminal dead-letter ledger — tenant floor, FK into the run
    // history, and APPEND-ONLY from the app role (SELECT + INSERT, no
    // UPDATE/DELETE — redrive/purge is a control-plane follow-up).
    assert!(sql.contains("CREATE TABLE wamn_run.run_dead_letters"));
    assert!(sql.contains("CREATE POLICY run_dead_letters_tenant ON wamn_run.run_dead_letters"));
    assert!(sql.contains("GRANT SELECT, INSERT ON wamn_run.run_dead_letters TO wamn_app"));
    assert!(
        !sql.contains("DELETE ON wamn_run.run_dead_letters"),
        "run_dead_letters must stay append-only for wamn_app"
    );
    // The marker columns the composed insert writes (partition_key is NOT NULL
    // here, unlike run_queue's — a ledger row exists only for a keyed head).
    assert!(sql.contains("partition_key text NOT NULL"));
    assert!(sql.contains("reason        text NOT NULL"));
    assert!(sql.contains("failed_at     timestamptz NOT NULL DEFAULT now()"));
}

// ---- live-apply gate (optional) --------------------------------------------

/// Apply `deploy/sql/run-state.sql` + `deploy/sql/run-queue.sql` to a throwaway Postgres
/// and assert the queue's real behaviour: the `SKIP LOCKED` claim predicate
/// (Ready claimed, Parked/Leased skipped), lease-expiry reclaim, the janitor sweep
/// (orphan → `infrastructure-failure` + dequeued), tenant RLS isolation, the
/// FK cascade from `runs`, and the trigger dispatcher's cron path (triggered
/// write-ahead, cron last-tick recovery, the wake scan). Gated on
/// `WAMN_RUN_QUEUE_PG_URL` (a superuser URL — the harness provisions `wamn_app`);
/// skips cleanly when unset. Mirrors the wamn-run-store / wamn-ddl / wamn-rls
/// gates. (True concurrent contention is the queuebench/dispatchbench gates; this
/// asserts the schema + predicates on one session.)
///
/// SR12b (wamn-2jkm.23): this is the `WAMN_*_PG_URL` live coverage of the
/// claim/queue SQL — where plan-sensitivity actually bites (the `AS MATERIALIZED`
/// fence, RLS, `ON CONFLICT`, index selection) that no pure test can observe. It
/// PREPARE/EXECUTEs the REAL builders (never hand-copied SQL) — the global +
/// combined + per-partition claims, the composed
/// checkpoint/complete statements, the janitor, and the registry scan — including
/// every builder the wave-2 queue cluster touched: the E4 stream_seq numeric
/// ordering, the SR11 composed statements, and
/// the R8b-b tenant predicates (below).
#[test]
fn run_queue_schema_applies_and_claims_on_postgres() {
    let Ok(url) = std::env::var("WAMN_RUN_QUEUE_PG_URL") else {
        eprintln!(
            "skipping run_queue_schema_applies_and_claims_on_postgres (set WAMN_RUN_QUEUE_PG_URL to run)"
        );
        return;
    };

    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .expect("read deploy/sql/run-state.sql");
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .expect("read deploy/sql/run-queue.sql");
    // The flow registry: active_flows_sql (the dispatcher's registry scan) reads
    // it, and cd-0's discriminating fixture (ACTIVE=4 vs PERSISTED=3) seeds it.
    let flows_ddl = std::fs::read_to_string(format!("{root}/deploy/sql/flows.sql"))
        .expect("read deploy/sql/flows.sql");

    // Exercise the REAL builders (not hand-copied SQL) via PREPARE/EXECUTE, so a
    // bug in claim_batch_sql / janitor_sweep_sql / the partition builders is caught here.
    let claim_sql = claim_batch_sql(10);
    let janitor_sql = janitor_sweep_sql();
    let park_stmt_sql = park_sql();
    let acquire_sql = acquire_partitions_sql(10);
    let claim_head_sql = claim_partition_head_sql(10);
    // The trigger dispatcher's builders (cron/wake).
    let triggered_sql = write_ahead_triggered_run_sql();
    let last_run_sql = cron_last_run_sql();
    // The durable cron anchor builders (wamn-fqg.6).
    let anchor_sel_sql = cron_anchor_sql();
    let upsert_anchor_sql = upsert_cron_anchor_sql();
    let parked_sql = parked_due_sql(50);
    // R8b-b / SR12b: the registry scan — the one bead-4 builder not otherwise
    // PREPARE/EXECUTE'd on the live path (the dispatcher reads it every sweep).
    let active_flows = active_flows_sql();
    // The fqg.18 combined claim/checkpoint/complete statements.
    let claim_dispatch = claim_dispatch_sql();
    let complete_dequeue = complete_dequeue_sql();
    let record_success_renew = record_success_and_renew_sql();
    let record_error_renew = record_error_and_renew_sql();
    // The materializer's evt enqueue pair (E4 / l5i9.17).
    let enq_evt = enqueue_evt_sql();
    let enq_evt_pol = enqueue_evt_with_policy_sql();
    // The terminal dead-letter dequeue (wamn-v8cv).
    let dl_dequeue = dead_letter_dequeue_sql();

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
    script.push_str(&flows_ddl);
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
    // attempts counts CRASH EVIDENCE only: the never-leased rq-ready/rq-healthy are
    // claimed for free, while the expired-lease rq-expired/rq-reclaim (their owner
    // died) are bumped — and a park->wake re-claim through the REAL park_sql is
    // free too (park releases the lease, so the claim sees no crash evidence).
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE claim_stmt (text, bigint) AS {claim_sql};\n\
         PREPARE park_stmt (text, bigint) AS {park_stmt_sql};\n\
         EXECUTE claim_stmt('c1', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM run_queue WHERE lease_owner='c1') = 4, 'claimed the 4 Ready rows'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='rq-leased') = 'X', 'live lease not stolen'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='rq-parked') IS NULL, 'parked row not claimed'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='rq-spent') = 'dead', 'budget-spent row not claimed'; \
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='rq-ready') = 0, 'first claim of a never-leased row is FREE'; \
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='rq-healthy') = 0, 'first claim of a never-leased row is FREE'; \
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='rq-expired') = 1, 'expired-lease reclaim counts crash evidence'; \
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='rq-reclaim') = 2, 'expired-lease reclaim counts crash evidence'; \
         END $$;\n\
         EXECUTE park_stmt('rq-healthy', 0);\n\
         EXECUTE claim_stmt('c2', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='rq-healthy') = 'c2', 'a parked-and-due row is re-claimed at wake'; \
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='rq-healthy') = 0, 'a park->wake re-claim burns NO redelivery budget'; \
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
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='pa-0') = 0, 'first head claim is FREE (crash evidence only)'; \
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
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='pa-1') = 0, 'advancing to the next never-leased head is FREE'; \
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

    // The fqg.18 combined statements via the REAL builders. Seed a dedicated run
    // (cd-0) whose PERSISTED flow_version is 3, and DISCRIMINATE against the
    // active version: register v3 INACTIVE and v4 ACTIVE, so ACTIVE=4 while the
    // run's own version is 3 (wamn-cox). The combined claim must return the run's
    // PERSISTED flow_version (3), not the active one (4) — a resume pins the
    // version the run started under. All earlier unpartitioned rows are
    // leased/parked/spent by now, so the LIMIT-1 combined claim deterministically
    // takes cd-0.
    script.push_str(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status, input_json) \
           VALUES ('t1','cd-0','f',3,'dispatched','\"rec\"'::jsonb);\n\
         INSERT INTO wamn_run.run_queue (tenant_id, run_id, available_at, attempts, max_attempts) \
           VALUES ('t1','cd-0', now(), 0, 20);\n\
         INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) VALUES \
           ('t1','f',3,false,'{}'::jsonb), \
           ('t1','f',4,true,'{}'::jsonb);\n",
    );
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE cd_stmt (text, bigint) AS {claim_dispatch};\n\
         PREPARE csr_stmt (text, text, int, int, text, jsonb, jsonb, bigint, text) AS {record_success_renew};\n\
         PREPARE cer_stmt (text, text, int, int, jsonb, jsonb, text, jsonb, bigint, text) AS {record_error_renew};\n\
         PREPARE cdq_stmt (text, jsonb) AS {complete_dequeue};\n\
         -- ONE statement: claim + mark running + dispatch read + persisted version.\n\
         CREATE TEMP TABLE cd_probe AS EXECUTE cd_stmt('cd-owner', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM cd_probe) = 1, 'combined claim takes exactly one run'; \
           ASSERT (SELECT run_id FROM cd_probe) = 'cd-0', 'combined claim takes the ready run'; \
           ASSERT (SELECT flow_id FROM cd_probe) = 'f', 'combined claim returns the dispatch flow'; \
           ASSERT (SELECT input_json FROM cd_probe) = '\"rec\"', 'combined claim returns the trigger input'; \
           ASSERT (SELECT flow_version FROM cd_probe) = 3, 'claim returns the run''s persisted flow_version (3), not the active one (4)'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='cd-0') = 'cd-owner', 'combined claim leased the row'; \
           ASSERT (SELECT status FROM runs WHERE run_id='cd-0') = 'running', 'combined claim marked the run running in-statement'; \
         END $$;\n\
         -- Per-node checkpoint + heartbeat: record advances the lease (owner-guarded).\n\
         CREATE TEMP TABLE lease_t0 AS SELECT lease_expires_at FROM run_queue WHERE run_id='cd-0';\n\
         EXECUTE csr_stmt('cd-0','n1',0,0,'main','\"out\"','\"in\"',120000,'cd-owner');\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM node_runs WHERE run_id='cd-0' AND node_id='n1' AND status='success') = 1, 'combined record wrote the checkpoint'; \
           ASSERT (SELECT lease_expires_at FROM run_queue WHERE run_id='cd-0') > (SELECT lease_expires_at FROM lease_t0), 'combined record renewed the lease'; \
         END $$;\n\
         -- Per-visit occurrence (wamn-03m/cjv.10): a REPLAY of visit 0 is an\n\
         -- ON CONFLICT no-op (first writer wins), while visit 1 of the SAME\n\
         -- node is a distinct row — N visits persist N rows.\n\
         EXECUTE csr_stmt('cd-0','n1',0,90,'main','\"out-replay\"','\"in\"',120000,'cd-owner');\n\
         EXECUTE csr_stmt('cd-0','n1',1,91,'main','\"out-v2\"','\"in2\"',120000,'cd-owner');\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM node_runs WHERE run_id='cd-0' AND node_id='n1') = 2, 'distinct visits persist distinct rows'; \
           ASSERT (SELECT output_json FROM node_runs WHERE run_id='cd-0' AND node_id='n1' AND occurrence=0) = '\"out\"', 'a replayed visit does not overwrite its row'; \
           ASSERT (SELECT output_json FROM node_runs WHERE run_id='cd-0' AND node_id='n1' AND occurrence=1) = '\"out-v2\"', 'the second visit carries its own emission'; \
         END $$;\n\
         -- A straggler with the WRONG owner still records (idempotent checkpoint,\n\
         -- same as the split path) but cannot renew the lease.\n\
         CREATE TEMP TABLE lease_t1 AS SELECT lease_expires_at FROM run_queue WHERE run_id='cd-0';\n\
         EXECUTE csr_stmt('cd-0','n2',0,1,'main','\"out\"','\"in\"',300000,'not-the-owner');\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM node_runs WHERE run_id='cd-0' AND node_id='n2') = 1, 'wrong-owner record still checkpoints'; \
           ASSERT (SELECT lease_expires_at FROM run_queue WHERE run_id='cd-0') = (SELECT lease_expires_at FROM lease_t1), 'wrong-owner record does NOT renew the lease'; \
         END $$;\n\
         -- The error-routed twin.\n\
         EXECUTE cer_stmt('cd-0','n3',0,2,'{{\"error\":{{}}}}','\"in\"','terminal','{{\"message\":\"x\"}}',240000,'cd-owner');\n\
         DO $$ BEGIN \
           ASSERT (SELECT error_kind FROM node_runs WHERE run_id='cd-0' AND node_id='n3') = 'terminal', 'combined error record carries the taxonomy'; \
           ASSERT (SELECT lease_expires_at FROM run_queue WHERE run_id='cd-0') > (SELECT lease_expires_at FROM lease_t1), 'error record renews the lease too'; \
         END $$;\n\
         -- Completion + dequeue, atomic in one statement.\n\
         EXECUTE cdq_stmt('cd-0','\"done\"');\n\
         DO $$ BEGIN \
           ASSERT (SELECT status FROM runs WHERE run_id='cd-0') = 'completed', 'combined complete marked the run'; \
           ASSERT (SELECT result_json FROM runs WHERE run_id='cd-0') = '\"done\"', 'combined complete recorded the result'; \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='cd-0') = 0, 'combined complete dequeued atomically'; \
         END $$;\n\
         -- Drained: a second combined claim returns no row.\n\
         CREATE TEMP TABLE cd_probe2 AS EXECUTE cd_stmt('cd-owner', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM cd_probe2) = 0, 'combined claim of a drained queue returns nothing'; \
         END $$;\n\
         COMMIT;\n"
    ));
    // Cron last-fired-tick recovery: FLOW-EXCLUSIVE (flow_id + trigger_source
    // predicate, cron_last_run_sql) — foreign flows whose ids sort inside a
    // lexical range under the deployed collation (or literally nest ':cron:' in
    // their flow id) must never leak into another flow's anchor.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE last_stmt (text) AS {last_run_sql};\n\
         PREPARE triggered_stmt (text, text, int, text, text) AS {triggered_sql};\n\
         EXECUTE triggered_stmt('cronflow:cron:0000000000100', 'cronflow', 1, 'cron', '{{\"fire-at-ms\": 100}}');\n\
         EXECUTE triggered_stmt('cronflow:cron:0000000000200', 'cronflow', 1, 'cron', '{{\"fire-at-ms\": 200}}');\n\
         -- Foreign-anchor poison: a flow whose id embeds ':cron:' and a colon-free\n\
         -- neighbor that sorts inside 'cronflow's lexical range under en_US-style\n\
         -- collations, both with LATER ticks; and a non-cron run for the flow itself.\n\
         EXECUTE triggered_stmt('cronflow:cron:5x:cron:0000000000999', 'cronflow:cron:5x', 1, 'cron', '{{\"fire-at-ms\": 999}}');\n\
         EXECUTE triggered_stmt('cronflowx:cron:0000000000999', 'cronflowx', 1, 'cron', '{{\"fire-at-ms\": 999}}');\n\
         EXECUTE triggered_stmt('cronflow:manual:7', 'cronflow', 1, 'manual', '{{\"seq\": 7}}');\n\
         CREATE TEMP TABLE lastrun AS EXECUTE last_stmt('cronflow');\n\
         DO $$ BEGIN \
           ASSERT (SELECT max FROM lastrun) = 'cronflow:cron:0000000000200', 'last-fired recovery is flow-exclusive (no foreign/non-cron leak)'; \
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

    // wamn-2jkm.29: the wake scan carries the global claim's `partition_key IS
    // NULL` guard, so it never surfaces a PARTITIONED row — the cadence reflects
    // ACTIONABLE (global-claimable) work only. The killer scenario is a D20
    // `blocking` WEDGE: `wc-wg-0` is an exhausted head (expired lease past grace,
    // budget spent) the janitor is EXEMPT from reaping, and `wc-wg-1` is a due,
    // unleased, budget-remaining FOLLOWER that `claim_partition_head_sql` holds
    // behind the wedge forever. Pre-fix, the follower matched the wake scan every
    // sweep -> `found_work()` true -> `next_interval` pinned at min PERMANENTLY.
    // The scenario proves: (a) the wedged follower (and the head) are NOT woken;
    // (b) an unpartitioned due park IS still woken; (c) a non-wedged partitioned
    // due park stays claimable via its OWN partition path (never stranded).
    script.push_str(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
           ('t1','wc-unp','f',1,'dispatched'), \
           ('t1','wc-wg-0','f',1,'running'),('t1','wc-wg-1','f',1,'dispatched'), \
           ('t1','wc-ok-0','f',1,'dispatched');\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, partition_key, available_at, enqueued_at, lease_owner, lease_expires_at, attempts, max_attempts) VALUES \
           ('t1','wc-unp', NULL,     now() - interval '1 min', now() - interval '2 min', NULL,  NULL,                      0,  20), \
           ('t1','wc-wg-0','wc-wg', now() - interval '3 hour', now() - interval '2 min','dead', now() - interval '2 hour', 20, 20), \
           ('t1','wc-wg-1','wc-wg', now() - interval '30 sec', now() - interval '1 min', NULL,  NULL,                      0,  20), \
           ('t1','wc-ok-0','wc-ok', now() - interval '30 sec', now() - interval '1 min', NULL,  NULL,                      0,  20);\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         -- (a)+(b): the read-only wake scan surfaces the unpartitioned due park\n\
         -- and NONE of the partitioned rows (the wedged follower can never pin it).\n\
         CREATE TEMP TABLE cad_woken AS EXECUTE parked_stmt;\n\
         DO $$ BEGIN \
           ASSERT EXISTS (SELECT 1 FROM cad_woken WHERE run_id='wc-unp'), 'an unpartitioned due park is still woken (the guard does not over-exclude)'; \
           ASSERT NOT EXISTS (SELECT 1 FROM cad_woken WHERE run_id='wc-wg-1'), 'a due partitioned follower behind a blocking wedge is NOT woken (would pin cadence at min: wamn-2jkm.29)'; \
           ASSERT NOT EXISTS (SELECT 1 FROM cad_woken WHERE run_id='wc-wg-0'), 'the wedged partitioned head is NOT woken'; \
           ASSERT NOT EXISTS (SELECT 1 FROM cad_woken WHERE run_id='wc-ok-0'), 'even a claimable partitioned head is not on the (global) wake path'; \
         END $$;\n\
         -- The wedge is genuine: the follower is NOT claimable via the partition\n\
         -- path either (blocked behind the exhausted head), so excluding it from\n\
         -- the wake scan strands nothing that WAS actionable.\n\
         EXECUTE acquire_stmt('WC', 60000);\n\
         EXECUTE claimhead_stmt('WC', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='wc-wg-1') IS NULL, 'the wedged follower is not claimable via the partition path either (genuinely not actionable)'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='wc-ok-0') = 'WC', '(c) a non-wedged partitioned due park stays claimable via its OWN partition path — never stranded by the wake-scan exclusion'; \
         END $$;\n\
         -- And the janitor leaves the blocking wedge (D20 exemption) — the reason\n\
         -- the follower cannot make progress and must not pin the cadence.\n\
         EXECUTE janitor_stmt(3600000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='wc-wg-0') = 1, 'the exhausted blocking head is NOT reaped (D20 wedge) — the follower stays permanently blocked'; \
         END $$;\n\
         COMMIT;\n",
    );

    // wamn-fqg.7: the wedge. A budget-spent run (attempts == max_attempts) whose lease
    // a park RELEASED (NULL) must WAKE — a NULL lease is proof the last owner was alive
    // (it parked, it did not crash), so the crash budget must not gate it. A budget-spent
    // run still holding an EXPIRED lease (a crash after the budget was spent) stays
    // terminal: not claimed, not woken, reaped by the janitor. Both are exercised through
    // the REAL builders (claim_batch_sql / parked_due_sql / janitor_sweep_sql).
    script.push_str(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
           ('t1','rq-wedge-woken','f',1,'running'), \
           ('t1','rq-wedge-poison','f',1,'running');\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, available_at, lease_owner, lease_expires_at, attempts, max_attempts) VALUES \
           ('t1','rq-wedge-woken', now() - interval '1 min', NULL,  NULL,                     20, 20), \
           ('t1','rq-wedge-poison',now() - interval '1 min','dead', now() - interval '2 hour', 20, 20);\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         -- Read-only wake scan FIRST: the woken (NULL-lease) row is surfaced for a\n\
         -- doorbell hint; the expired-lease poison row is NOT (the fix is narrow).\n\
         CREATE TEMP TABLE wedge_woken AS EXECUTE parked_stmt;\n\
         DO $$ BEGIN \
           ASSERT EXISTS (SELECT 1 FROM wedge_woken WHERE run_id='rq-wedge-woken'), 'a woken budget-spent (NULL-lease) run is surfaced by the wake scan (wamn-fqg.7)'; \
           ASSERT NOT EXISTS (SELECT 1 FROM wedge_woken WHERE run_id='rq-wedge-poison'), 'an expired-lease budget-spent run is NOT woken (poison stays terminal)'; \
         END $$;\n\
         -- The claim takes the woken row (attempts UNCHANGED — a NULL lease is not crash\n\
         -- evidence) and leaves the poison row for the janitor.\n\
         EXECUTE claim_stmt('cw', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='rq-wedge-woken') = 'cw', 'a woken budget-spent run is claimed (was invisible before wamn-fqg.7)'; \
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='rq-wedge-woken') = 20, 'waking a park burns NO redelivery budget'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='rq-wedge-poison') = 'dead', 'an expired-lease budget-spent run is NOT claimed (poison stays terminal)'; \
         END $$;\n\
         -- The janitor reaps the still-unclaimed poison row; the woken row, now\n\
         -- live-leased by the claim, is left in flight.\n\
         EXECUTE janitor_stmt(3600000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='rq-wedge-poison') = 0, 'poison row reaped'; \
           ASSERT (SELECT status FROM runs WHERE run_id='rq-wedge-poison') = 'infrastructure-failure', 'poison run marked infra-failure (max_attempts stays terminal)'; \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='rq-wedge-woken') = 1, 'the woken run progresses (not reaped)'; \
           ASSERT (SELECT status FROM runs WHERE run_id='rq-wedge-woken') = 'running', 'the woken run stays in flight (janitor leaves the live-leased claim)'; \
         END $$;\n\
         COMMIT;\n",
    );

    // wamn-fqg.7 (partition path): a woken budget-spent partition HEAD (NULL lease) is
    // acquirable AND head-claimable, and an earlier such head still blocks its later
    // sibling (in-order preserved). Exercises the `OR ... IS NULL` disjunct on all three
    // partition builders (acquire candidate, head candidate `c`, sibling sub-check `b`).
    script.push_str(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
           ('t1','pw-0','f',1,'running'),('t1','pw-1','f',1,'dispatched');\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, partition_key, available_at, lease_owner, lease_expires_at, attempts, max_attempts) VALUES \
           ('t1','pw-0','site-w', now() - interval '1 min', NULL, NULL, 20, 20), \
           ('t1','pw-1','site-w', now(),                    NULL, NULL, 0,  20);\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         EXECUTE acquire_stmt('RW', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM partition_owner WHERE partition_key='site-w' AND lease_owner='RW') = 1, 'a partition whose only head is a woken budget-spent run is acquirable (wamn-fqg.7)'; \
         END $$;\n\
         EXECUTE claimhead_stmt('RW', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='pw-0') = 'RW', 'the woken budget-spent partition head is claimed'; \
           ASSERT (SELECT attempts FROM run_queue WHERE run_id='pw-0') = 20, 'waking a partition head park burns NO redelivery budget'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='pw-1') IS NULL, 'the later sibling stays blocked behind the woken head (in-order preserved)'; \
         END $$;\n\
         COMMIT;\n",
    );

    // ------------------------------------------------------------------------
    // D20 (R6): the partitioned(key) head-unavailability POLICY, through the REAL
    // claim_partition_head_sql (policy branch) + janitor_sweep_sql (wedge
    // exemption). All prior partitions (site-a/-b/-w) are still live-owned, so the
    // acquire calls below grab only these new keys. enqueued_at is stamped
    // explicitly and INDEPENDENTLY of available_at, so the stream order the
    // blocking policy ranks by is not an artifact of the (backed-off) availability.
    // ------------------------------------------------------------------------
    //
    // The policy-materializing enqueue builder (fqg.9 wires this on the guest
    // claim path): enqueue_with_policy_sql writes partition_policy from $5, and a
    // plain enqueue_sql takes the column DEFAULT ('blocking').
    let enqueue_policy_sql = enqueue_with_policy_sql();
    let enqueue_plain_sql = enqueue_sql();
    script.push_str(&format!(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
           ('t1','ep-lf','f',1,'dispatched'),('t1','ep-def','f',1,'dispatched');\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE enq_policy (text, text, int, bigint, text) AS {enqueue_policy_sql};\n\
         PREPARE enq_plain (text, text, int, bigint) AS {enqueue_plain_sql};\n\
         EXECUTE enq_policy('ep-lf', NULL, 0, 0, 'leapfrog');\n\
         EXECUTE enq_plain('ep-def', NULL, 0, 0);\n\
         DO $$ BEGIN \
           ASSERT (SELECT partition_policy FROM run_queue WHERE run_id='ep-lf') = 'leapfrog', 'enqueue_with_policy_sql materializes the declared policy onto the row'; \
           ASSERT (SELECT partition_policy FROM run_queue WHERE run_id='ep-def') = 'blocking', 'a plain enqueue takes the blocking column default'; \
         END $$;\n\
         COMMIT;\n"
    ));
    //
    // Phase A — a backed-off head (future available_at) that is FIRST in the
    // stream order. `blk` uses the DEFAULT policy (no column written) to also
    // prove the default is 'blocking'; `lf` opts into 'leapfrog'.
    script.push_str(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
           ('t1','blk-0','f',1,'dispatched'),('t1','blk-1','f',1,'dispatched'), \
           ('t1','lf-0','f',1,'dispatched'),('t1','lf-1','f',1,'dispatched');\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, partition_key, available_at, enqueued_at, attempts, max_attempts) VALUES \
           ('t1','blk-0','blk', now() + interval '1 hour', now() - interval '2 min', 0, 20), \
           ('t1','blk-1','blk', now() - interval '30 sec', now() - interval '1 min', 0, 20);\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, partition_key, available_at, enqueued_at, attempts, max_attempts, partition_policy) VALUES \
           ('t1','lf-0','lf', now() + interval '1 hour', now() - interval '2 min', 0, 20, 'leapfrog'), \
           ('t1','lf-1','lf', now() - interval '30 sec', now() - interval '1 min', 0, 20, 'leapfrog');\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN ASSERT (SELECT partition_policy FROM run_queue WHERE run_id='blk-0') = 'blocking', 'a partitioned row defaults to the blocking policy (D20)'; END $$;\n\
         EXECUTE acquire_stmt('PA', 60000);\n\
         EXECUTE claimhead_stmt('PA', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='blk-1') IS NULL, 'blocking: a backed-off head HOLDS its key — the later ready run does NOT overtake'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='blk-0') IS NULL, 'blocking: the not-yet-due head is not claimed either (the key dispatches nothing)'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='lf-1') = 'PA', 'leapfrog: the later ready run OVERTAKES the backed-off head'; \
         END $$;\n\
         COMMIT;\n",
    );

    // Phase B — an EXHAUSTED head (expired lease past grace, budget spent). `wg`
    // is blocking (default): the janitor must NOT reap it (it wedges the key);
    // `lx` is leapfrog: the janitor reaps it and the key releases.
    script.push_str(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
           ('t1','wg-0','f',1,'running'),('t1','wg-1','f',1,'dispatched'), \
           ('t1','lx-0','f',1,'running'),('t1','lx-1','f',1,'dispatched');\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, partition_key, available_at, enqueued_at, lease_owner, lease_expires_at, attempts, max_attempts) VALUES \
           ('t1','wg-0','wg', now() - interval '3 hour', now() - interval '2 min','dead', now() - interval '2 hour', 20, 20), \
           ('t1','wg-1','wg', now() - interval '30 sec', now() - interval '1 min', NULL,  NULL,                     0,  20);\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, partition_key, available_at, enqueued_at, lease_owner, lease_expires_at, attempts, max_attempts, partition_policy) VALUES \
           ('t1','lx-0','lx', now() - interval '3 hour', now() - interval '2 min','dead', now() - interval '2 hour', 20, 20, 'leapfrog'), \
           ('t1','lx-1','lx', now() - interval '30 sec', now() - interval '1 min', NULL,  NULL,                     0,  20, 'leapfrog');\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         EXECUTE janitor_stmt(3600000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='wg-0') = 1, 'blocking wedge: the janitor does NOT reap an exhausted blocking head'; \
           ASSERT (SELECT status FROM runs WHERE run_id='wg-0') = 'running', 'blocking wedge: the exhausted head''s run status is left untouched (operator releases)'; \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='lx-0') = 0, 'leapfrog: the janitor DOES reap an exhausted leapfrog head'; \
           ASSERT (SELECT status FROM runs WHERE run_id='lx-0') = 'infrastructure-failure', 'leapfrog: the reaped head''s run is marked infra-failure'; \
         END $$;\n\
         EXECUTE acquire_stmt('PB', 60000);\n\
         EXECUTE claimhead_stmt('PB', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='wg-1') IS NULL, 'blocking wedge: the later run stays BLOCKED behind the exhausted head — the key is wedged'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='lx-1') = 'PB', 'leapfrog: with the exhausted head reaped, the key RELEASES and the next run dispatches'; \
         END $$;\n\
         COMMIT;\n",
    );

    // ------------------------------------------------------------------------
    // E4: the queue orders on stream_seq (numeric) AHEAD of run_id (text). CDC
    // event runs are keyed <flow>:evt:<stream_seq>, whose text order INVERTS the
    // numeric one (f1:evt:10 < f1:evt:9 lexically). Seed one partition key whose
    // four heads share available_at + enqueued_at and differ ONLY by stream_seq,
    // with run_ids in the WRONG lexical order, then drive the REAL partition
    // claim: the head must advance 8 -> 9 by numeric stream position, never
    // 10 -> 11 by lexical run-id. This is the assertion that FAILS before the fix.
    // ------------------------------------------------------------------------
    script.push_str(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
           ('t1','f1:evt:8','f',1,'dispatched'),('t1','f1:evt:9','f',1,'dispatched'), \
           ('t1','f1:evt:10','f',1,'dispatched'),('t1','f1:evt:11','f',1,'dispatched');\n\
         -- One statement: available_at and enqueued_at are the SAME now() for all\n\
         -- four rows, so stream_seq is the sole differentiator of the claim order.\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, partition_key, available_at, enqueued_at, stream_seq, attempts, max_attempts) VALUES \
           ('t1','f1:evt:8', 'evt-key', now(), now(), 8,  0, 20), \
           ('t1','f1:evt:9', 'evt-key', now(), now(), 9,  0, 20), \
           ('t1','f1:evt:10','evt-key', now(), now(), 10, 0, 20), \
           ('t1','f1:evt:11','evt-key', now(), now(), 11, 0, 20);\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         DO $$ BEGIN ASSERT (SELECT partition_policy FROM run_queue WHERE run_id='f1:evt:8') = 'blocking', 'evt rows default to the blocking policy'; END $$;\n\
         EXECUTE acquire_stmt('EV', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM partition_owner WHERE partition_key='evt-key' AND lease_owner='EV') = 1, 'EV leases the evt-key partition'; \
         END $$;\n\
         EXECUTE claimhead_stmt('EV', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='f1:evt:8') = 'EV', 'head is stream_seq 8 (numeric min), NOT lexical-min f1:evt:10'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='f1:evt:10') IS NULL, 'the lexically-smallest run_id is NOT the head'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='f1:evt:9') IS NULL, 'later stream positions stay blocked behind the head'; \
         END $$;\n\
         -- Advance the key: with stream_seq 8 dequeued, the next head must be\n\
         -- stream_seq 9 (numeric), not lexical-min f1:evt:10.\n\
         DELETE FROM run_queue WHERE run_id='f1:evt:8';\n\
         EXECUTE claimhead_stmt('EV', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='f1:evt:9') = 'EV', 'the key advances 8 -> 9 by numeric stream position'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='f1:evt:10') IS NULL, 'f1:evt:10 does NOT overtake f1:evt:9 (lexical order would)'; \
         END $$;\n\
         COMMIT;\n",
    );

    // ------------------------------------------------------------------------
    // l5i9.17: the REAL materializer enqueue pair (write-ahead + enqueue_evt in
    // one transaction, the fire() shape) as wamn_app under t1. Asserts: the row
    // carries the REAL stream_seq; the run_id is the zero-padded mint; a
    // duplicate re-fire (redelivery past the JetStream dedupe window) collapses
    // on ON CONFLICT — the exactly-once guarantee; the KEYED variant stamps
    // policy + key coherently (kq0z) while the unkeyed row keeps the column
    // defaults.
    // ------------------------------------------------------------------------
    let evt_run_unkeyed = mint_evt_run_id("fe", 907);
    let evt_run_keyed = mint_evt_run_id("fk", 908);
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE wat_stmt (text, text, int, text, text) AS {triggered_sql};\n\
         PREPARE enq_evt (text, text, int, bigint, bigint) AS {enq_evt};\n\
         PREPARE enq_evt_pol (text, text, int, bigint, bigint, text) AS {enq_evt_pol};\n\
         EXECUTE wat_stmt('{evt_run_unkeyed}', 'fe', 1, 'evt:907', '{{\"trigger\":\"event\"}}');\n\
         EXECUTE enq_evt('{evt_run_unkeyed}', NULL, 0, 0, 907);\n\
         EXECUTE wat_stmt('{evt_run_keyed}', 'fk', 1, 'evt:908', '{{\"trigger\":\"event\"}}');\n\
         EXECUTE enq_evt_pol('{evt_run_keyed}', 'site-9', 0, 0, 908, 'leapfrog');\n\
         DO $$ BEGIN \
           ASSERT (SELECT stream_seq FROM run_queue WHERE run_id='{evt_run_unkeyed}') = 907, 'the evt enqueue writes the REAL stream_seq'; \
           ASSERT (SELECT partition_key FROM run_queue WHERE run_id='{evt_run_unkeyed}') IS NULL, 'unkeyed evt row has no partition key'; \
           ASSERT (SELECT partition_policy FROM run_queue WHERE run_id='{evt_run_unkeyed}') = 'blocking', 'unkeyed evt row takes the column-default policy (kq0z coherence)'; \
           ASSERT (SELECT stream_seq FROM run_queue WHERE run_id='{evt_run_keyed}') = 908, 'the keyed evt enqueue writes the REAL stream_seq'; \
           ASSERT (SELECT partition_key FROM run_queue WHERE run_id='{evt_run_keyed}') = 'site-9', 'keyed evt row carries its key'; \
           ASSERT (SELECT partition_policy FROM run_queue WHERE run_id='{evt_run_keyed}') = 'leapfrog', 'keyed evt row carries the declared policy'; \
         END $$;\n\
         -- Exactly-once: a redelivered firing re-mints the same id; both halves\n\
         -- collapse on ON CONFLICT (0 rows inserted), never a second run/queue row.\n\
         EXECUTE wat_stmt('{evt_run_unkeyed}', 'fe', 1, 'evt:907', '{{\"trigger\":\"event\"}}');\n\
         EXECUTE enq_evt('{evt_run_unkeyed}', NULL, 0, 0, 907);\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM runs WHERE run_id='{evt_run_unkeyed}') = 1, 'redelivery mints no second run row'; \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='{evt_run_unkeyed}') = 1, 'redelivery mints no second queue row'; \
         END $$;\n\
         COMMIT;\n"
    ));

    // ------------------------------------------------------------------------
    // SR12b / R8b-b: the dispatcher's registry scan (active_flows_sql) — the one
    // bead-touched claim/queue builder not otherwise PREPARE/EXECUTE'd here. Seed a
    // t2 active flow alongside t1's, then run the REAL builder under each tenant
    // claim: each sees ONLY its own active flow (the explicit tenant predicate +
    // the `active` filter + RLS), and an inactive version is excluded.
    // ------------------------------------------------------------------------
    script.push_str(
        "INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) VALUES \
           ('t2','g',1,true,'{}'::jsonb);\n",
    );
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE active_flows AS {active_flows};\n\
         CREATE TEMP TABLE af_t1 AS EXECUTE active_flows;\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM af_t1) = 1, 'the registry scan returns t1''s single ACTIVE flow (the inactive v3 is excluded)'; \
           ASSERT (SELECT flow_id FROM af_t1) = 'f' AND (SELECT version FROM af_t1) = 4, 't1 sees active flow f v4, not the inactive v3 nor t2''s g'; \
         END $$;\n\
         COMMIT;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't2';\n\
         CREATE TEMP TABLE af_t2 AS EXECUTE active_flows;\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM af_t2) = 1, 'the registry scan is tenant-scoped: t2 sees only its own active flow'; \
           ASSERT (SELECT flow_id FROM af_t2) = 'g', 't2 sees flow g, never t1''s f (R8b-b predicate + RLS)'; \
         END $$;\n\
         COMMIT;\n"
    ));

    // ------------------------------------------------------------------------
    // wamn-v8cv (D20 dead-letter + continue): the REAL dead_letter_dequeue_sql
    // via PREPARE/EXECUTE. A guest-observed terminal failure of a BLOCKING
    // partition head dequeues + lands the run_dead_letters marker in ONE
    // statement (one txn), and the key CONTINUES — the next same-key run is
    // head-claimable. A leapfrog head and an unpartitioned run dequeue with NO
    // marker (no strict-ordering promise), redelivery collapses on the PK, and
    // the ledger is tenant-isolated under RLS.
    // ------------------------------------------------------------------------
    script.push_str(&format!(
        "INSERT INTO wamn_run.runs (tenant_id, run_id, flow_id, flow_version, status, fail_kind) VALUES \
           ('t1','dl-blk-0','f',1,'failed','terminal'), \
           ('t1','dl-blk-1','f',1,'dispatched',NULL), \
           ('t1','dl-lf-0','f',1,'failed','terminal'), \
           ('t1','dl-un-0','f',1,'failed','terminal');\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, partition_key, available_at, enqueued_at, attempts, max_attempts) VALUES \
           ('t1','dl-blk-0','dl-key', now(), now() - interval '2 min', 0, 20), \
           ('t1','dl-blk-1','dl-key', now(), now() - interval '1 min', 0, 20), \
           ('t1','dl-un-0', NULL,     now(), now(),                    0, 20);\n\
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, partition_key, available_at, enqueued_at, attempts, max_attempts, partition_policy) VALUES \
           ('t1','dl-lf-0','dl-lf-key', now(), now(), 0, 20, 'leapfrog');\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE dl_stmt (text, text) AS {dl_dequeue};\n\
         -- The blocking head: dequeue + marker, atomically.\n\
         EXECUTE dl_stmt('dl-blk-0', 'terminal: n1: capability-denied');\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id='dl-blk-0') = 0, 'terminal blocking head dequeued (the key continues)'; \
           ASSERT (SELECT count(*) FROM run_dead_letters WHERE run_id='dl-blk-0') = 1, 'the dequeue landed the dead-letter marker in the same statement'; \
           ASSERT (SELECT partition_key FROM run_dead_letters WHERE run_id='dl-blk-0') = 'dl-key', 'the marker names the breached key'; \
           ASSERT (SELECT flow_id FROM run_dead_letters WHERE run_id='dl-blk-0') = 'f', 'the marker carries the flow from the run''s own row'; \
           ASSERT (SELECT reason FROM run_dead_letters WHERE run_id='dl-blk-0') = 'terminal: n1: capability-denied', 'the marker carries the failure verdict'; \
           ASSERT (SELECT failed_at FROM run_dead_letters WHERE run_id='dl-blk-0') IS NOT NULL, 'the marker is stamped server-side'; \
         END $$;\n\
         -- Redelivery: a second terminal settle of the SAME (now-dequeued) run\n\
         -- inserts nothing and deletes nothing — converged, no error.\n\
         EXECUTE dl_stmt('dl-blk-0', 'terminal: n1: capability-denied');\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM run_dead_letters WHERE run_id='dl-blk-0') = 1, 'redelivered terminal settle mints no second marker'; \
         END $$;\n\
         -- Leapfrog head + unpartitioned run: dequeue, NO marker.\n\
         EXECUTE dl_stmt('dl-lf-0', 'terminal: n1: capability-denied');\n\
         EXECUTE dl_stmt('dl-un-0', 'terminal: n1: capability-denied');\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM run_queue WHERE run_id IN ('dl-lf-0','dl-un-0')) = 0, 'leapfrog/unpartitioned terminal runs still dequeue'; \
           ASSERT (SELECT count(*) FROM run_dead_letters WHERE run_id IN ('dl-lf-0','dl-un-0')) = 0, 'no ordering promise -> no dead-letter marker (leapfrog + unpartitioned)'; \
         END $$;\n\
         -- The key CONTINUES: with the failed head gone, the next same-key run\n\
         -- is acquirable + head-claimable through the REAL partition builders.\n\
         EXECUTE acquire_stmt('DL', 60000);\n\
         EXECUTE claimhead_stmt('DL', 60000);\n\
         DO $$ BEGIN \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='dl-blk-1') = 'DL', 'the key advances to the next run in order past the dead-lettered head'; \
         END $$;\n\
         COMMIT;\n\
         -- RLS: the marker is tenant-scoped — t2 sees nothing.\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't2';\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM run_dead_letters) = 0, 'the dead-letter ledger is tenant-isolated (RLS)'; \
         END $$;\n\
         COMMIT;\n"
    ));

    // wamn-fqg.6: the DURABLE cron anchor, through the REAL builders. The upsert
    // is monotonic (a lower tick never rewinds — a rewind would let an
    // already-fired tick re-fire), and the anchor SURVIVES a prune of the flow's
    // cron runs (the exact retention bug: a re-fire the write-ahead ON CONFLICT
    // could not absorb once the run was gone), while the runs-based FALLBACK
    // loses it — the demoted bootstrap path.
    script.push_str(&format!(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1';\n\
         PREPARE anchor_sel (text) AS {anchor_sel_sql};\n\
         PREPARE anchor_up (text, bigint) AS {upsert_anchor_sql};\n\
         PREPARE last_sel (text) AS {last_run_sql};\n\
         -- First upsert creates the row; the SELECT reads it back.\n\
         EXECUTE anchor_up('anchorflow', 200);\n\
         CREATE TEMP TABLE a0 AS EXECUTE anchor_sel('anchorflow');\n\
         DO $$ BEGIN ASSERT (SELECT last_tick FROM a0) = 200, 'the anchor upsert persists last_tick'; END $$;\n\
         -- Monotonic: a HIGHER tick advances; a LOWER tick does NOT rewind.\n\
         EXECUTE anchor_up('anchorflow', 500);\n\
         EXECUTE anchor_up('anchorflow', 300);\n\
         CREATE TEMP TABLE a1 AS EXECUTE anchor_sel('anchorflow');\n\
         DO $$ BEGIN ASSERT (SELECT last_tick FROM a1) = 500, 'GREATEST: a lower upsert never rewinds the anchor'; END $$;\n\
         -- Seed a cron run then PRUNE it (9.6 retention): the anchor + its SELECT\n\
         -- are unchanged, while the runs-based fallback now finds nothing.\n\
         INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, status, trigger_source) \
           VALUES ('t1','anchorflow:cron:0000000000500','anchorflow',1,'completed','cron');\n\
         DELETE FROM runs WHERE flow_id='anchorflow' AND trigger_source='cron';\n\
         CREATE TEMP TABLE a2 AS EXECUTE anchor_sel('anchorflow');\n\
         CREATE TEMP TABLE lf AS EXECUTE last_sel('anchorflow');\n\
         DO $$ BEGIN \
           ASSERT (SELECT last_tick FROM a2) = 500, 'the durable anchor survives a prune of the flow''s cron runs (wamn-fqg.6)'; \
           ASSERT (SELECT max FROM lf) IS NULL, 'the runs-based fallback loses the anchor once the runs are pruned (the demoted bootstrap path)'; \
         END $$;\n\
         COMMIT;\n\
         -- RLS: the anchor is tenant-scoped — t2 sees no row.\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't2';\n\
         PREPARE anchor_sel_t2 (text) AS {anchor_sel_sql};\n\
         CREATE TEMP TABLE a_t2 AS EXECUTE anchor_sel_t2('anchorflow');\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM a_t2) = 0, 'the cron anchor is tenant-isolated (RLS)'; END $$;\n\
         COMMIT;\n"
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
