//! Bounded service shutdown and supervision of native runtime liveness.

use std::time::Duration;

use anyhow::Context as _;
use wash_runtime::host::probes::{Liveness, ProbeState};

/// Bound the runtime's final wait for blocking exporters and guest compilation.
/// A timed-out `flush_within` leaves its blocking task running until this wait.
pub const RUNTIME_SHUTDOWN_BUDGET: Duration = Duration::from_secs(1);

/// Finish telemetry without hiding a service failure or an incomplete flush.
pub async fn finish(result: anyhow::Result<()>) -> anyhow::Result<()> {
    let flushed =
        wash_runtime::observability::flush_within(wash_runtime::observability::FLUSH_BUDGET).await;
    if !flushed {
        tracing::error!("telemetry flush failed or exceeded its shutdown budget");
        return result.and(Err(anyhow::anyhow!("telemetry flush did not complete")));
    }
    result
}

/// Await cleanup for at most `budget`, retaining its failure context.
pub async fn bounded_cleanup(
    budget: Duration,
    cleanup: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    tokio::time::timeout(budget, cleanup)
        .await
        .context("service cleanup exceeded its shutdown budget")?
}

/// Supervise real loop progress and optionally announce startup after the first beat.
///
/// Native liveness intentionally permits an unstarted host indefinitely. Once
/// native startup has returned, a command task that never subscribes must still
/// fail within its normal silence budget. Pass no probe state for secondary
/// loops whose progress must not announce process startup. This observer never
/// writes a beat.
pub async fn watch_liveness(
    liveness: &Liveness,
    probes: Option<&ProbeState>,
    silence_budget: Duration,
) -> anyhow::Error {
    let started = tokio::time::Instant::now();
    let mut poll = tokio::time::interval(Duration::from_millis(100));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        poll.tick().await;
        match liveness_progress(liveness.silence(), started.elapsed(), silence_budget) {
            Ok(true) => {
                if let Some(probes) = probes {
                    probes.started();
                }
            }
            Ok(false) => {}
            Err(error) => return error,
        }
    }
}

fn liveness_progress(
    silence: Option<Duration>,
    startup_elapsed: Duration,
    silence_budget: Duration,
) -> anyhow::Result<bool> {
    match silence {
        None if startup_elapsed >= silence_budget => {
            anyhow::bail!("service loop did not produce its first liveness beat")
        }
        None => Ok(false),
        Some(silence) if silence > silence_budget => {
            anyhow::bail!("service loop exceeded its native liveness silence budget")
        }
        Some(_) => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn startup_requires_a_beat_and_stale_detection_preserves_the_native_boundary() {
        let budget = Duration::from_secs(45);
        assert!(
            !liveness_progress(
                None,
                budget.checked_sub(Duration::from_nanos(1)).unwrap(),
                budget
            )
            .unwrap()
        );
        assert!(liveness_progress(None, budget, budget).is_err());
        assert!(liveness_progress(Some(budget), Duration::MAX, budget).unwrap());
        assert!(
            liveness_progress(
                Some(budget + Duration::from_nanos(1)),
                Duration::ZERO,
                budget
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn missing_first_beat_fails_without_creating_a_beat() {
        let liveness = Liveness::new(Duration::ZERO);
        let probes = ProbeState::default();
        let _error = watch_liveness(&liveness, Some(&probes), Duration::ZERO).await;
        assert_eq!(liveness.silence(), None);
    }

    /// The native command loop beats on every turn, also while NATS is down.
    /// Time past the budget after the first beat does not stop the host.
    #[tokio::test(start_paused = true)]
    async fn a_beating_loop_stays_alive_long_past_the_budget() {
        let budget = Duration::from_secs(45);
        let liveness = Liveness::new(budget);
        liveness.beat();
        let beating = Arc::clone(&liveness);
        let beater = tokio::spawn(async move {
            let mut turn = tokio::time::interval(Duration::from_secs(15));
            loop {
                turn.tick().await;
                beating.beat();
            }
        });
        let observed = Arc::clone(&liveness);
        let watcher = tokio::spawn(async move { watch_liveness(&observed, None, budget).await });
        tokio::time::sleep(budget * 20).await;
        assert!(!watcher.is_finished());
        watcher.abort();
        beater.abort();
    }

    #[tokio::test]
    async fn cleanup_timeout_and_original_failure_remain_failures() {
        let _error = bounded_cleanup(Duration::ZERO, std::future::pending())
            .await
            .unwrap_err();
        let _error = bounded_cleanup(Duration::from_secs(1), async {
            anyhow::bail!("native command task failed")
        })
        .await
        .unwrap_err();
    }
}
