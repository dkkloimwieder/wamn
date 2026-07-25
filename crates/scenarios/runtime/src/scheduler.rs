//! The scenario scheduler: drive a [`VirtualClock`] to the next
//! parked-wake deadline and re-drive, until nothing is parked.
//!
//! A real flow with a 24h `delay` node parks: it records a wake deadline and
//! returns. Rather than wait 24h of wall time (prod) or advance the clock by a
//! hand-known amount (the pre-extraction bench), the scheduler reads the ACTUAL
//! parked deadlines from the run store, advances the shared virtual clock to the
//! EARLIEST one, and re-drives — collapsing arbitrary delays to milliseconds
//! with no test-side knowledge of how long each delay was.
//!
//! Two backends plug into the same [`ScenarioScheduler`] via [`SchedulerBackend`]:
//!
//! - **run-s6** (the guest's single-run `run-s6` export): the wake deadline
//!   lives in `runs.state_json->'wake'->'<node>'` as epoch seconds, read from
//!   the guest's (virtualized) wall clock — so advancing the virtual clock
//!   alone collapses it. Query it with [`RUN_S6_WAKE_DEADLINES_SQL`].
//! - **run-next** (the production `ExecutionHost` claim loop): the wake lives in
//!   `run_queue.available_at`, anchored to Postgres `now()` at park time — so a
//!   virtual GUEST clock cannot make it claimable. A run-next backend's
//!   `redrive` must ALSO nudge the DB before draining. The queue statements take
//!   the exact scenario `run_id`; [`RUN_QUEUE_NEXT_WAKE_SQL`] selects that run's
//!   deadline and [`RUN_QUEUE_DUE_NUDGE_SQL`] shifts only that same selected row.
//!
//! The scheduler clock/deadlines are epoch nanoseconds (the [`VirtualClock`]
//! unit). Queue backends retain PostgreSQL's exact timestamp as a stale-selection
//! token while converting it to nanoseconds for the scheduler.

use std::fmt;

use super::clock::VirtualClock;

/// The earliest-first pick, and the loop that applies it, live here so the
/// "advance to the EARLIEST deadline" rule is one testable line.
#[derive(Debug)]
pub struct ScenarioScheduler {
    clock: VirtualClock,
    max_steps: usize,
}

/// A backend the scheduler drives: report the currently-parked wake deadlines,
/// and re-drive all now-due work once. Implemented per run store (run-s6 over
/// `runs.state_json`, run-next over `run_queue`).
#[async_trait::async_trait]
pub trait SchedulerBackend {
    /// Every currently-parked wake deadline, in epoch NANOSECONDS, across all
    /// parked runs. Empty ⇒ nothing is parked (quiescent — the loop ends).
    async fn wake_deadlines_nanos(&mut self) -> anyhow::Result<Vec<u64>>;

    /// Re-drive all now-due parked work once (re-invoke the parked run / claim +
    /// drain the queue). A run whose deadline has passed should complete; one
    /// still in the future should re-park.
    async fn redrive(&mut self) -> anyhow::Result<()>;
}

/// Why a selected scenario queue wake could not be shifted.
///
/// The shift statement returns both counts from one PostgreSQL statement. It
/// performs the update only after candidate cardinality is exactly one, so
/// these failures never accompany a partial schedule change.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueueScheduleShiftError {
    /// The selected queue row was removed or its `available_at` changed after
    /// selection.
    Stale {
        /// Exact scenario run whose selected schedule was stale.
        run_id: String,
    },
    /// More than one row matched an identity that must be unique.
    Ambiguous {
        /// Exact scenario run whose identity unexpectedly matched multiple rows.
        run_id: String,
        /// Number of matching queue rows.
        matched: u64,
    },
    /// PostgreSQL found one candidate but did not shift exactly that one row.
    Incomplete {
        /// Exact scenario run whose shift did not complete.
        run_id: String,
        /// Number of rows actually shifted.
        shifted: u64,
    },
}

impl fmt::Display for QueueScheduleShiftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stale { run_id } => {
                write!(formatter, "scenario run {run_id:?} queue schedule is stale")
            }
            Self::Ambiguous { run_id, matched } => write!(
                formatter,
                "scenario run {run_id:?} matched {matched} queue schedules"
            ),
            Self::Incomplete { run_id, shifted } => write!(
                formatter,
                "scenario run {run_id:?} matched one queue schedule but shifted {shifted}"
            ),
        }
    }
}

impl std::error::Error for QueueScheduleShiftError {}

/// Validate the cardinality report returned by [`RUN_QUEUE_DUE_NUDGE_SQL`].
pub fn validate_queue_due_nudge(
    run_id: &str,
    matched: u64,
    shifted: u64,
) -> Result<(), QueueScheduleShiftError> {
    match (matched, shifted) {
        (1, 1) => Ok(()),
        (0, _) => Err(QueueScheduleShiftError::Stale {
            run_id: run_id.to_string(),
        }),
        (matched, _) if matched > 1 => Err(QueueScheduleShiftError::Ambiguous {
            run_id: run_id.to_string(),
            matched,
        }),
        (_, shifted) => Err(QueueScheduleShiftError::Incomplete {
            run_id: run_id.to_string(),
            shifted,
        }),
    }
}

impl ScenarioScheduler {
    /// A scheduler driving `clock`, capped at a generous default step count so a
    /// run that never makes progress fails loudly instead of looping forever.
    pub fn new(clock: VirtualClock) -> Self {
        Self {
            clock,
            max_steps: 1024,
        }
    }

    /// Override the step cap.
    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Drive `backend` to quiescence: read the parked deadlines, advance the
    /// clock to the EARLIEST future one, re-drive, and repeat until nothing is
    /// parked. Returns the number of advance/re-drive steps taken.
    ///
    /// Advancing to the earliest (not just any) deadline is load-bearing: it
    /// wakes exactly the run(s) actually due and leaves later ones parked, so
    /// independent delays fire in order — a run parked for 1h must not ride a
    /// sibling's 24h wake.
    pub async fn drive_to_quiescence(
        &self,
        backend: &mut impl SchedulerBackend,
    ) -> anyhow::Result<usize> {
        let mut steps = 0usize;
        loop {
            let deadlines = backend.wake_deadlines_nanos().await?;
            // The EARLIEST parked deadline — the next moment any run wakes.
            let Some(&next) = deadlines.iter().min() else {
                return Ok(steps);
            };
            self.clock.advance_to_nanos(next);
            backend.redrive().await?;
            steps += 1;
            anyhow::ensure!(
                steps <= self.max_steps,
                "scenario scheduler exceeded {} steps — a parked run never made progress",
                self.max_steps
            );
        }
    }
}

/// Every parked-wake deadline (epoch SECONDS) across run-s6 runs: one row per
/// still-armed `delay` node. The deadline is a JSON number under
/// `runs.state_json->'wake'->'<node>'`. Scoped by the caller's session
/// (`app.tenant` RLS claim + `search_path`); a completed run has cleared its
/// wake, so it does not appear.
pub const RUN_S6_WAKE_DEADLINES_SQL: &str = "SELECT (w.value#>>'{}')::bigint \
     FROM runs r, jsonb_each(r.state_json->'wake') AS w \
     WHERE r.tenant_id = current_setting('app.tenant', true) \
       AND r.state_json ? 'wake'";

/// The selected scenario run's FUTURE parked-wake deadline on `run_queue`
/// (run-next path). Parameter `$1` is the exact case-owned `run_id`. Returning
/// the PostgreSQL timestamp itself gives the shift statement an exact stale
/// selection token. Global (unpartitioned) rows only, matching the global
/// claim.
pub const RUN_QUEUE_NEXT_WAKE_SQL: &str = "SELECT available_at \
     FROM run_queue \
     WHERE tenant_id = current_setting('app.tenant', true) \
       AND run_id = $1 \
       AND partition_key IS NULL \
       AND available_at > now()";

/// Shift one selected scenario queue row claimable NOW (run-next path).
///
/// Parameters are `$1` = exact case-owned `run_id` and `$2` = the exact
/// `available_at` returned by [`RUN_QUEUE_NEXT_WAKE_SQL`]. The materialized
/// candidate is counted before the update CTE. Only a cardinality of one opens
/// the update gate; zero (deleted or concurrently rescheduled) and impossible
/// ambiguity both update nothing. The returned `(matched, shifted)` counts must
/// be checked with [`validate_queue_due_nudge`] before draining.
pub const RUN_QUEUE_DUE_NUDGE_SQL: &str = "WITH candidate AS MATERIALIZED ( \
       SELECT q.ctid \
       FROM run_queue AS q \
       WHERE q.tenant_id = current_setting('app.tenant', true) \
         AND q.run_id = $1 \
         AND q.partition_key IS NULL \
         AND q.available_at = $2 \
         AND q.available_at > now() \
     ), cardinality AS ( \
       SELECT count(*)::bigint AS matched FROM candidate \
     ), shifted AS ( \
       UPDATE run_queue AS q SET available_at = now() \
       FROM candidate AS c, cardinality AS n \
       WHERE n.matched = 1 AND q.ctid = c.ctid \
       RETURNING 1 \
     ) \
     SELECT cardinality.matched, \
            (SELECT count(*)::bigint FROM shifted) AS shifted \
     FROM cardinality";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::SystemTime;

    /// An in-memory backend: a set of parked runs, each with a wake deadline
    /// (nanos). `redrive` completes any run whose deadline is at/under the
    /// clock, mirroring the guest's `now < wake` park check — so the scheduler's
    /// earliest-first pick is observable without a database.
    struct FakeBackend {
        clock: VirtualClock,
        /// (deadline_nanos, completed)
        runs: Arc<Mutex<Vec<(u64, bool)>>>,
    }

    #[async_trait::async_trait]
    impl SchedulerBackend for FakeBackend {
        async fn wake_deadlines_nanos(&mut self) -> anyhow::Result<Vec<u64>> {
            Ok(self
                .runs
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, done)| !*done)
                .map(|(d, _)| *d)
                .collect())
        }
        async fn redrive(&mut self) -> anyhow::Result<()> {
            let now = self.clock.now_nanos();
            for (deadline, done) in self.runs.lock().unwrap().iter_mut() {
                if !*done && *deadline <= now {
                    *done = true;
                }
            }
            Ok(())
        }
    }

    // Mutation target (delta 2, mutant i): the earliest-deadline pick
    // (`deadlines.iter().min()`). Two runs at 1h and 24h: the scheduler must
    // advance to the 1h deadline FIRST (waking only run A), then the 24h
    // (waking run B) — TWO steps. A mutant that picks `.max()` (earliest→latest
    // swap) advances straight to 24h, wakes BOTH at once, and finishes in ONE
    // step — failing the `steps == 2` assertion here. It also lets run A "wake"
    // at a time later than its own deadline (still correct completion), but the
    // step count and the intermediate parked-state pin the ordering.
    #[tokio::test]
    async fn scheduler_wakes_the_earliest_deadline_first() {
        let hour = 3_600u64 * 1_000_000_000;
        let clock = VirtualClock::at_secs(1_000_000_000);
        let base = clock.now_nanos(); // the clock's start, in nanos
        let runs = Arc::new(Mutex::new(vec![
            (base + hour, false),      // run A: +1h
            (base + 24 * hour, false), // run B: +24h
        ]));
        let mut backend = FakeBackend {
            clock: clock.clone(),
            runs: runs.clone(),
        };

        let sched = ScenarioScheduler::new(clock.clone());
        let steps = sched.drive_to_quiescence(&mut backend).await.unwrap();
        assert_eq!(
            steps, 2,
            "earliest-first must take two distinct-deadline steps"
        );
        assert!(
            runs.lock().unwrap().iter().all(|(_, done)| *done),
            "both runs complete"
        );
        // The clock landed exactly on the latest deadline, never past it.
        assert_eq!(clock.now_nanos(), base + 24 * hour);
    }

    #[tokio::test]
    async fn scheduler_is_quiescent_when_nothing_is_parked() {
        let clock = VirtualClock::at_secs(100);
        let mut backend = FakeBackend {
            clock: clock.clone(),
            runs: Arc::new(Mutex::new(Vec::new())),
        };
        let steps = ScenarioScheduler::new(clock)
            .drive_to_quiescence(&mut backend)
            .await
            .unwrap();
        assert_eq!(steps, 0, "no parked runs ⇒ no steps");
    }

    #[tokio::test]
    async fn scheduler_collapses_a_single_far_future_delay() {
        let base = 500u64 * 1_000_000_000;
        let far = base + 86_400 * 1_000_000_000; // +24h
        let clock = VirtualClock::at_secs(500);
        let mut backend = FakeBackend {
            clock: clock.clone(),
            runs: Arc::new(Mutex::new(vec![(far, false)])),
        };
        let steps = ScenarioScheduler::new(clock.clone())
            .drive_to_quiescence(&mut backend)
            .await
            .unwrap();
        assert_eq!(steps, 1);
        assert_eq!(clock.now_nanos(), far, "advanced exactly to the deadline");
    }

    #[test]
    fn queue_shift_cardinality_is_a_typed_failure_contract() {
        assert_eq!(validate_queue_due_nudge("selected", 1, 1), Ok(()));
        assert_eq!(
            validate_queue_due_nudge("selected", 0, 0),
            Err(QueueScheduleShiftError::Stale {
                run_id: "selected".to_string()
            })
        );
        assert_eq!(
            validate_queue_due_nudge("selected", 2, 0),
            Err(QueueScheduleShiftError::Ambiguous {
                run_id: "selected".to_string(),
                matched: 2
            })
        );
        assert_eq!(
            validate_queue_due_nudge("selected", 1, 0),
            Err(QueueScheduleShiftError::Incomplete {
                run_id: "selected".to_string(),
                shifted: 0
            })
        );
    }

    #[test]
    fn queue_sql_requires_run_and_selected_schedule_before_update() {
        assert!(RUN_QUEUE_NEXT_WAKE_SQL.contains("run_id = $1"));
        assert!(RUN_QUEUE_DUE_NUDGE_SQL.contains("q.run_id = $1"));
        assert!(RUN_QUEUE_DUE_NUDGE_SQL.contains("q.available_at = $2"));
        assert!(RUN_QUEUE_DUE_NUDGE_SQL.contains("n.matched = 1"));
        let cardinality = RUN_QUEUE_DUE_NUDGE_SQL
            .find("SELECT count(*)::bigint AS matched")
            .unwrap();
        let update = RUN_QUEUE_DUE_NUDGE_SQL.find("UPDATE run_queue").unwrap();
        assert!(
            cardinality < update,
            "candidate cardinality must be established before the update CTE"
        );
    }

    async fn execute_queue_shift(
        client: &tokio_postgres::Client,
        run_id: &str,
        selected_at: SystemTime,
    ) -> (u64, u64) {
        let row = client
            .query_one(RUN_QUEUE_DUE_NUDGE_SQL, &[&run_id, &selected_at])
            .await
            .unwrap();
        let matched = u64::try_from(row.get::<_, i64>(0)).unwrap();
        let shifted = u64::try_from(row.get::<_, i64>(1)).unwrap();
        (matched, shifted)
    }

    /// Opt-in proof over PostgreSQL's real CTE/update and timestamp semantics:
    ///
    /// `WAMN_PG_TEST_URL=... cargo test -p wamn-scenario-runtime \
    /// queue_shift_is_execution_scoped_in_real_postgresql -- --ignored`
    #[tokio::test]
    #[ignore = "requires WAMN_PG_TEST_URL pointing at a disposable PostgreSQL database"]
    async fn queue_shift_is_execution_scoped_in_real_postgresql() {
        let database_url = std::env::var("WAMN_PG_TEST_URL").expect("WAMN_PG_TEST_URL must be set");
        let (client, connection) = tokio_postgres::connect(&database_url, tokio_postgres::NoTls)
            .await
            .unwrap();
        let connection_task = tokio::spawn(async move { connection.await.unwrap() });
        client
            .batch_execute(
                "DROP SCHEMA IF EXISTS wamn_scenario_schedule_test CASCADE; \
                 CREATE SCHEMA wamn_scenario_schedule_test; \
                 CREATE TABLE wamn_scenario_schedule_test.run_queue ( \
                   tenant_id text NOT NULL, \
                   run_id text NOT NULL, \
                   partition_key text, \
                   available_at timestamptz NOT NULL, \
                   PRIMARY KEY (tenant_id, run_id) \
                 ); \
                 SELECT set_config('app.tenant', 'tenant-a', false); \
                 SELECT set_config('search_path', 'wamn_scenario_schedule_test', false); \
                 INSERT INTO wamn_scenario_schedule_test.run_queue \
                   (tenant_id, run_id, available_at) VALUES \
                   ('tenant-a', 'scenario-execution-a', now() + interval '1 hour'), \
                   ('tenant-a', 'scenario-execution-b', now() + interval '24 hours');",
            )
            .await
            .unwrap();
        let (peer, peer_connection) = tokio_postgres::connect(&database_url, tokio_postgres::NoTls)
            .await
            .unwrap();
        let peer_connection_task = tokio::spawn(async move { peer_connection.await.unwrap() });
        peer.query_one(
            "SELECT set_config('app.tenant', 'tenant-a', false), \
                    set_config('search_path', 'wamn_scenario_schedule_test', false)",
            &[],
        )
        .await
        .unwrap();

        let selected_at: SystemTime = client
            .query_one(RUN_QUEUE_NEXT_WAKE_SQL, &[&"scenario-execution-a"])
            .await
            .unwrap()
            .get(0);
        let unrelated_before: SystemTime = client
            .query_one(
                "SELECT available_at FROM run_queue WHERE run_id = 'scenario-execution-b'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        let (selected_counts, stale_counts) = tokio::join!(
            execute_queue_shift(&client, "scenario-execution-a", selected_at),
            execute_queue_shift(&peer, "scenario-execution-b", selected_at),
        );
        validate_queue_due_nudge("scenario-execution-a", selected_counts.0, selected_counts.1)
            .unwrap();
        assert_eq!(
            validate_queue_due_nudge("scenario-execution-b", stale_counts.0, stale_counts.1),
            Err(QueueScheduleShiftError::Stale {
                run_id: "scenario-execution-b".to_string()
            })
        );
        let row = client
            .query_one(
                "SELECT available_at <= now(), \
                        (SELECT available_at FROM run_queue \
                         WHERE run_id = 'scenario-execution-b') = $1 \
                 FROM run_queue WHERE run_id = 'scenario-execution-a'",
                &[&unrelated_before],
            )
            .await
            .unwrap();
        assert!(row.get::<_, bool>(0), "selected execution became claimable");
        assert!(
            row.get::<_, bool>(1),
            "unrelated future execution kept its exact schedule"
        );

        let unrelated_after_stale: SystemTime = client
            .query_one(
                "SELECT available_at FROM run_queue WHERE run_id = 'scenario-execution-b'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(unrelated_after_stale, unrelated_before);

        client
            .batch_execute(
                "ALTER TABLE run_queue DROP CONSTRAINT run_queue_pkey; \
                 INSERT INTO run_queue (tenant_id, run_id, available_at) \
                 SELECT 'tenant-a', 'ambiguous', now() + interval '48 hours' \
                 FROM (VALUES (1), (2)) AS duplicate(n);",
            )
            .await
            .unwrap();
        let ambiguous_at: SystemTime = client
            .query_one(
                "SELECT available_at FROM run_queue WHERE run_id = 'ambiguous' LIMIT 1",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        let ambiguous_counts = execute_queue_shift(&client, "ambiguous", ambiguous_at).await;
        assert_eq!(
            validate_queue_due_nudge("ambiguous", ambiguous_counts.0, ambiguous_counts.1),
            Err(QueueScheduleShiftError::Ambiguous {
                run_id: "ambiguous".to_string(),
                matched: 2
            })
        );
        let still_future: i64 = client
            .query_one(
                "SELECT count(*) FROM run_queue \
                 WHERE run_id = 'ambiguous' AND available_at > now()",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(still_future, 2, "ambiguity caused no partial update");

        client
            .batch_execute("DROP SCHEMA wamn_scenario_schedule_test CASCADE")
            .await
            .unwrap();
        peer_connection_task.abort();
        connection_task.abort();
    }
}
