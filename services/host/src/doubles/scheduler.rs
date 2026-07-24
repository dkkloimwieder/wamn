//! The test scheduler (production delta 2): drive a [`VirtualClock`] to the next
//! parked-wake deadline and re-drive, until nothing is parked.
//!
//! A real flow with a 24h `delay` node parks: it records a wake deadline and
//! returns. Rather than wait 24h of wall time (prod) or advance the clock by a
//! hand-known amount (the pre-extraction bench), the scheduler reads the ACTUAL
//! parked deadlines from the run store, advances the shared virtual clock to the
//! EARLIEST one, and re-drives — collapsing arbitrary delays to milliseconds
//! with no test-side knowledge of how long each delay was.
//!
//! Two backends plug into the same [`TestScheduler`] via [`SchedulerBackend`]:
//!
//! - **run-s6** (the guest's single-run `run-s6` export): the wake deadline
//!   lives in `runs.state_json->'wake'->'<node>'` as epoch seconds, read from
//!   the guest's (virtualized) wall clock — so advancing the virtual clock
//!   alone collapses it. Query it with [`RUN_S6_WAKE_DEADLINES_SQL`].
//! - **run-next** (the production `RunWorker` claim loop): the wake lives in
//!   `run_queue.available_at`, anchored to Postgres `now()` at park time — so a
//!   virtual GUEST clock cannot make it claimable. A run-next backend's
//!   `redrive` must ALSO nudge the DB (`UPDATE run_queue SET available_at =
//!   now()` for the due rows, [`RUN_QUEUE_DUE_NUDGE_SQL`]) before draining.
//!   [`RUN_QUEUE_NEXT_WAKE_SQL`] reads its next deadline.
//!
//! The clock/deadlines are epoch nanoseconds throughout (the [`VirtualClock`]
//! unit); the SQL helpers return epoch seconds, which the backend scales.

use super::clock::VirtualClock;

/// The earliest-first pick, and the loop that applies it, live here so the
/// "advance to the EARLIEST deadline" rule is one testable line.
pub struct TestScheduler {
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

impl TestScheduler {
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
                "test scheduler exceeded {} steps — a parked run never made progress",
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

/// The next FUTURE parked-wake deadline (epoch SECONDS) on the `run_queue`
/// (run-next path): the minimum `available_at` still ahead of Postgres `now()`.
/// Global (unpartitioned) rows only, matching the global claim.
pub const RUN_QUEUE_NEXT_WAKE_SQL: &str = "SELECT extract(epoch FROM min(available_at))::bigint \
     FROM run_queue \
     WHERE tenant_id = current_setting('app.tenant', true) \
       AND partition_key IS NULL \
       AND available_at > now()";

/// Nudge parked `run_queue` rows claimable NOW (run-next path): a run-next
/// backend calls this in `redrive` after advancing the virtual clock, because
/// `available_at` is Postgres-time anchored and a virtual clock cannot move it.
/// Pulls every future `available_at` back to `now()` so the next drain claims
/// it. Tenant-scoped, global rows only.
pub const RUN_QUEUE_DUE_NUDGE_SQL: &str = "UPDATE run_queue SET available_at = now() \
     WHERE tenant_id = current_setting('app.tenant', true) \
       AND partition_key IS NULL \
       AND available_at > now()";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

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

        let sched = TestScheduler::new(clock.clone());
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
        let steps = TestScheduler::new(clock)
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
        let steps = TestScheduler::new(clock.clone())
            .drive_to_quiescence(&mut backend)
            .await
            .unwrap();
        assert_eq!(steps, 1);
        assert_eq!(clock.now_nanos(), far, "advanced exactly to the deadline");
    }
}
