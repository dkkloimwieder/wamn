//! Probe-owned readiness for the synchronous release closure.

use std::fmt;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

use crate::RouterDriver;
use crate::router_driver::PreparedReleaseReadiness;

/// Stable redacted probe refusal for any store, registry, integrity or capacity failure.
pub const RELEASE_READINESS_CHECK_FAILED: &str = "release-readiness-check-failed";

/// Stable probe refusal after an activation invalidates the resident closure.
pub const RELEASE_READINESS_INVALIDATED: &str = "release-readiness-invalidated";

/// Whether this process may receive synchronous release traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterReadinessStatus {
    NotReady,
    Ready,
}

/// One stable observation of the readiness state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterReadinessSnapshot {
    /// Current serving posture for synchronous traffic.
    pub status: RouterReadinessStatus,
    /// Monotonic activation fence for in-flight evaluations.
    pub generation: u64,
    /// Evaluations attempted in this activation generation.
    pub attempts: u32,
    /// Distinct request-reachable wiring versions prepared on success.
    pub synchronous_wirings: usize,
    /// Distinct digest-keyed instances prepared on success.
    pub component_digests: usize,
    /// Stable redacted reason while NotReady.
    pub refusal: Option<&'static str>,
}

impl RouterReadinessSnapshot {
    /// True only after the current generation's complete closure was prepared.
    pub fn is_ready(&self) -> bool {
        self.status == RouterReadinessStatus::Ready
    }
}

#[derive(Debug)]
struct ReadinessState {
    snapshot: RouterReadinessSnapshot,
}

impl ReadinessState {
    fn new() -> Self {
        Self {
            snapshot: RouterReadinessSnapshot {
                status: RouterReadinessStatus::NotReady,
                generation: 0,
                attempts: 0,
                synchronous_wirings: 0,
                component_digests: 0,
                refusal: Some("release-readiness-not-evaluated"),
            },
        }
    }

    fn begin(&mut self) -> Option<u64> {
        if self.snapshot.is_ready() {
            return None;
        }
        self.snapshot.attempts = self.snapshot.attempts.saturating_add(1);
        Some(self.snapshot.generation)
    }

    fn finish(&mut self, generation: u64, result: Result<PreparedReleaseReadiness, &'static str>) {
        if generation != self.snapshot.generation {
            return;
        }
        match result {
            Ok(prepared) => {
                self.snapshot.status = RouterReadinessStatus::Ready;
                self.snapshot.synchronous_wirings = prepared.synchronous_wirings;
                self.snapshot.component_digests = prepared.component_digests;
                self.snapshot.refusal = None;
            }
            Err(refusal) => {
                self.snapshot.status = RouterReadinessStatus::NotReady;
                self.snapshot.synchronous_wirings = 0;
                self.snapshot.component_digests = 0;
                self.snapshot.refusal = Some(refusal);
            }
        }
    }

    fn invalidate(&mut self, refusal: &'static str) {
        self.snapshot.generation = self.snapshot.generation.saturating_add(1);
        self.snapshot.attempts = 0;
        self.snapshot.status = RouterReadinessStatus::NotReady;
        self.snapshot.synchronous_wirings = 0;
        self.snapshot.component_digests = 0;
        self.snapshot.refusal = Some(refusal);
    }
}

/// The readiness owner for one production router driver.
///
/// Probe transport is intentionally outside this type. A transport observes
/// [`snapshot`](Self::snapshot), calls [`refresh`](Self::refresh) while false,
/// and calls [`invalidate`](Self::invalidate) when it receives a new activation
/// generation. Concurrent refreshes collapse onto one evaluation, and a Ready
/// observation is a process-memory hit with no PostgreSQL or registry call.
pub struct RouterReadinessProbe {
    driver: Arc<RouterDriver>,
    evaluation: AsyncMutex<()>,
    state: Mutex<ReadinessState>,
}

impl RouterReadinessProbe {
    pub fn new(driver: Arc<RouterDriver>) -> Self {
        Self {
            driver,
            evaluation: AsyncMutex::new(()),
            state: Mutex::new(ReadinessState::new()),
        }
    }

    pub fn snapshot(&self) -> RouterReadinessSnapshot {
        self.state
            .lock()
            .expect("router readiness lock poisoned")
            .snapshot
            .clone()
    }

    /// Re-evaluate the current release generation once.
    ///
    /// Failure is represented as NotReady rather than escaping to process
    /// startup. The caller owns its bounded probe cadence; all underlying store
    /// and registry calls already carry their configured timeouts.
    pub async fn refresh(&self) -> RouterReadinessSnapshot {
        let _evaluation = self.evaluation.lock().await;
        let generation = {
            let mut state = self.state.lock().expect("router readiness lock poisoned");
            let Some(generation) = state.begin() else {
                return state.snapshot.clone();
            };
            generation
        };

        let prepared = self.driver.prepare_synchronous_release().await;
        let mut state = self.state.lock().expect("router readiness lock poisoned");
        match prepared {
            Ok(prepared) => state.finish(generation, Ok(prepared)),
            Err(error) => {
                // The refusal string stays redacted for the probe TRANSPORT,
                // which must not leak internals to a caller. The host's own log
                // is not a caller: without this line the one process that knows
                // why the release did not load reports `NotReady` and nothing
                // else (wamn-10yt.10.34).
                // `{:#}` renders the whole context chain. Plain Display
                // prints only the outermost message, which names the wiring
                // that failed and not one word about why.
                let rendered = format!("{error:#}");
                tracing::warn!(
                    target: "wamn::host",
                    error = %rendered,
                    "release readiness evaluation failed"
                );
                state.finish(generation, Err(RELEASE_READINESS_CHECK_FAILED))
            }
        }
        state.snapshot.clone()
    }

    /// Fence an in-flight evaluation and return the next generation to NotReady.
    pub fn invalidate(&self) {
        self.state
            .lock()
            .expect("router readiness lock poisoned")
            .invalidate(RELEASE_READINESS_INVALIDATED);
    }
}

impl fmt::Debug for RouterReadinessProbe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouterReadinessProbe")
            .field("snapshot", &self.snapshot())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared() -> PreparedReleaseReadiness {
        PreparedReleaseReadiness {
            synchronous_wirings: 2,
            component_digests: 3,
        }
    }

    #[test]
    fn failure_stays_not_ready_and_success_advances_the_same_generation() {
        let mut state = ReadinessState::new();
        let generation = state.begin().expect("initial evaluation runs");
        state.finish(generation, Err(RELEASE_READINESS_CHECK_FAILED));
        assert_eq!(state.snapshot.status, RouterReadinessStatus::NotReady);
        assert_eq!(state.snapshot.attempts, 1);
        assert_eq!(state.snapshot.refusal, Some(RELEASE_READINESS_CHECK_FAILED));

        let generation = state.begin().expect("a failed generation retries");
        state.finish(generation, Ok(prepared()));
        assert!(state.snapshot.is_ready());
        assert_eq!(state.snapshot.attempts, 2);
        assert_eq!(state.snapshot.synchronous_wirings, 2);
        assert_eq!(state.snapshot.component_digests, 3);
        assert_eq!(state.begin(), None, "Ready is a no-store cache hit");
    }

    #[test]
    fn invalidation_fences_an_older_in_flight_success() {
        let mut state = ReadinessState::new();
        let old_generation = state.begin().expect("initial evaluation runs");
        state.invalidate(RELEASE_READINESS_INVALIDATED);
        state.finish(old_generation, Ok(prepared()));

        assert_eq!(state.snapshot.status, RouterReadinessStatus::NotReady);
        assert_eq!(state.snapshot.generation, 1);
        assert_eq!(state.snapshot.attempts, 0);
        assert_eq!(state.snapshot.refusal, Some(RELEASE_READINESS_INVALIDATED));
    }
}
