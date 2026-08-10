//! The scenario scheduler drives a [`ScenarioClock`] to the next deterministic
//! retry deadline and re-drives until no retry is scheduled.
//!
//! The production `ExecutionHost` claim loop represents the queue wait in
//! `run_queue.available_at`, but scenarios treat that database timestamp only
//! as an opaque stale-selection token. The authoritative deadline comes from
//! the retry cursor's deterministic `delay-ms`. When the
//!   logical clock makes that schedule due, [`DatabaseClockBoundary`](crate::DatabaseClockBoundary)
//!   maps the decision to the captured database origin and
//!   [`RUN_QUEUE_DUE_NUDGE_SQL`] moves only that row there so the unchanged
//!   production claim path can take it.
//!
//! The scheduler clock/deadlines are epoch nanoseconds (the [`ScenarioClock`]
//! unit). Queue backends retain PostgreSQL's exact timestamp only as a
//! stale-selection token.

use std::fmt;

use super::clock::ScenarioClock;

/// The earliest-first pick, and the loop that applies it, live here so the
/// "advance to the EARLIEST deadline" rule is one testable line.
#[derive(Debug)]
pub struct ScenarioScheduler {
    clock: ScenarioClock,
    max_steps: usize,
}

/// A backend the scheduler drives: report scheduled retry deadlines and
/// re-drive all now-due work once.
#[async_trait::async_trait]
pub trait SchedulerBackend {
    /// Every scheduled retry deadline, in epoch NANOSECONDS. Empty means the
    /// backend is quiescent and the loop ends.
    async fn wake_deadlines_nanos(&mut self) -> anyhow::Result<Vec<u64>>;

    /// Re-drive all now-due retry work once. A retry still in the future remains
    /// queued.
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
    pub fn new(clock: ScenarioClock) -> Self {
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

    /// Drive `backend` to quiescence: read the retry deadlines, advance the
    /// clock to the EARLIEST future one, re-drive, and repeat until nothing is
    /// waiting on retry. Returns the number of advance/re-drive steps taken.
    ///
    /// Advancing to the earliest (not just any) deadline is load-bearing: it
    /// wakes exactly the run(s) actually due and leaves later retries queued.
    pub async fn drive_to_quiescence(
        &self,
        backend: &mut impl SchedulerBackend,
    ) -> anyhow::Result<usize> {
        let mut steps = 0usize;
        loop {
            let deadlines = backend.wake_deadlines_nanos().await?;
            // The EARLIEST retry deadline — the next moment any run is due.
            let Some(&next) = deadlines.iter().min() else {
                return Ok(steps);
            };
            self.clock.advance_to_nanos(next);
            backend.redrive().await?;
            steps += 1;
            anyhow::ensure!(
                steps <= self.max_steps,
                "scenario scheduler exceeded {} steps — a retry never made progress",
                self.max_steps
            );
        }
    }
}

/// The selected scenario run's queued retry schedule. Parameter `$1` is the
/// exact case-owned `run_id`. Returning
/// the PostgreSQL timestamp itself gives the shift statement an exact opaque
/// stale-selection token; `state_json` carries the authoritative virtual
/// schedule. Due/not-due is decided only from that virtual schedule, and this
/// query deliberately has no database-wall-clock comparison. Global
/// (unpartitioned) rows only, matching the global claim.
pub const RUN_QUEUE_NEXT_WAKE_SQL: &str = "SELECT q.available_at, r.state_json::text \
     FROM run_queue AS q \
     JOIN runs AS r ON r.tenant_id = q.tenant_id AND r.run_id = q.run_id \
     WHERE q.tenant_id = current_setting('app.tenant', true) \
       AND q.run_id = $1 \
       AND q.partition_key IS NULL";

/// Shift one selected scenario queue row claimable NOW (run-next path).
///
/// Parameters are `$1` = exact case-owned `run_id`, `$2` = the exact
/// `available_at` returned by [`RUN_QUEUE_NEXT_WAKE_SQL`], and `$3` = the
/// database origin captured beside the scenario origin. The materialized
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
     ), cardinality AS ( \
       SELECT count(*)::bigint AS matched FROM candidate \
     ), shifted AS ( \
       UPDATE run_queue AS q SET available_at = $3 \
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

    /// An in-memory backend: a set of queued retries, each with a deadline.
    /// `redrive` completes any run whose deadline is at or before the clock.
    struct FakeBackend {
        clock: ScenarioClock,
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
    // advance to the 1h deadline FIRST (running only retry A), then the 24h
    // (running retry B) — TWO steps. A mutant that picks `.max()`
    // (earliest→latest swap) advances straight to 24h, runs BOTH at once, and
    // finishes in ONE step.
    #[tokio::test]
    async fn scheduler_runs_the_earliest_retry_first() {
        let hour = 3_600u64 * 1_000_000_000;
        let clock = ScenarioClock::at_secs(1_000_000_000);
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
    async fn scheduler_is_quiescent_without_retries() {
        let clock = ScenarioClock::at_secs(100);
        let mut backend = FakeBackend {
            clock: clock.clone(),
            runs: Arc::new(Mutex::new(Vec::new())),
        };
        let steps = ScenarioScheduler::new(clock)
            .drive_to_quiescence(&mut backend)
            .await
            .unwrap();
        assert_eq!(steps, 0, "no retries means no steps");
    }

    #[tokio::test]
    async fn scheduler_collapses_a_single_far_future_backoff() {
        let base = 500u64 * 1_000_000_000;
        let far = base + 86_400 * 1_000_000_000; // +24h
        let clock = ScenarioClock::at_secs(500);
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
        assert!(RUN_QUEUE_NEXT_WAKE_SQL.contains("q.run_id = $1"));
        assert!(RUN_QUEUE_NEXT_WAKE_SQL.contains("r.state_json::text"));
        assert!(RUN_QUEUE_DUE_NUDGE_SQL.contains("q.run_id = $1"));
        assert!(RUN_QUEUE_DUE_NUDGE_SQL.contains("q.available_at = $2"));
        assert!(RUN_QUEUE_DUE_NUDGE_SQL.contains("available_at = $3"));
        assert!(RUN_QUEUE_DUE_NUDGE_SQL.contains("n.matched = 1"));
        assert!(!RUN_QUEUE_NEXT_WAKE_SQL.contains("now()"));
        assert!(!RUN_QUEUE_DUE_NUDGE_SQL.contains("now()"));
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
        database_origin: SystemTime,
    ) -> (u64, u64) {
        let row = client
            .query_one(
                RUN_QUEUE_DUE_NUDGE_SQL,
                &[&run_id, &selected_at, &database_origin],
            )
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
                 CREATE TABLE wamn_scenario_schedule_test.runs ( \
                   tenant_id text NOT NULL, \
                   run_id text NOT NULL, \
                   state_json jsonb NOT NULL, \
                   PRIMARY KEY (tenant_id, run_id) \
                 ); \
                 SELECT set_config('app.tenant', 'tenant-a', false); \
                 SELECT set_config('search_path', 'wamn_scenario_schedule_test', false); \
                 INSERT INTO wamn_scenario_schedule_test.runs \
                   (tenant_id, run_id, state_json) VALUES \
                   ('tenant-a', 'scenario-execution-a', '{\"retry\":{\"delay-ms\":3600000}}'), \
                   ('tenant-a', 'scenario-execution-b', '{\"retry\":{\"delay-ms\":86400000}}'); \
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
        let database_origin: SystemTime =
            client.query_one("SELECT now()", &[]).await.unwrap().get(0);
        let unrelated_before: SystemTime = client
            .query_one(
                "SELECT available_at FROM run_queue WHERE run_id = 'scenario-execution-b'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        let (selected_counts, stale_counts) = tokio::join!(
            execute_queue_shift(
                &client,
                "scenario-execution-a",
                selected_at,
                database_origin
            ),
            execute_queue_shift(&peer, "scenario-execution-b", selected_at, database_origin),
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
                 INSERT INTO runs (tenant_id, run_id, state_json) \
                 VALUES ('tenant-a', 'ambiguous', \
                         '{\"retry\":{\"delay-ms\":172800000}}'); \
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
        let ambiguous_counts =
            execute_queue_shift(&client, "ambiguous", ambiguous_at, database_origin).await;
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
