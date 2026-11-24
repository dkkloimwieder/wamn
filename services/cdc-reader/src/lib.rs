//! Start and reopen sessions for the change capture process.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::bail;
use tokio_util::sync::CancellationToken;
use wamn_broker::publisher::JetStreamPublisher;
use wamn_event::reader::{ReaderConfig, SessionFate, drain};
use wamn_postgres::replication::ReplicationSource;

const CAPTURE_GAP_EVENT: &str = "CDC_CAPTURE_GAP";

/// Shared reopen backoff + cap ladder (R11). BOTH the open-failure arm and the
/// drain-sever arm feed this ONE ladder, so a session that opens cleanly then
/// severs immediately can no longer hot-loop connect→sever as fast as
/// Postgres answers: every re-open is a bounded backoff, and the cap trips two
/// independent ways.
///
/// - `consecutive_failures` is the FAST-flap guard. It resets ONLY when a drain
///   committed a transaction (`note_reopen(_, commits > 0)` — productivity, not
///   open success), so an open-then-sever session that never commits keeps
///   incrementing it until `max_consecutive` trips.
/// - `window_reopens` is the SLOW-flap guard. EVERY re-open (productive or not)
///   is timestamped; more than `rate_cap` inside `rate_window` trips even when
///   each session commits once and thus keeps clearing the streak.
struct ReopenLadder {
    /// Consecutive re-opens with no committed transaction between them.
    consecutive_failures: u32,
    /// Every re-open ever taken (the `reopens` gauge — E2).
    total_reopens: u64,
    /// Re-open instants inside the trailing window, oldest first.
    window_reopens: VecDeque<Instant>,
    max_consecutive: u32,
    rate_window: Duration,
    rate_cap: usize,
    base_backoff: Duration,
    max_backoff: Duration,
}

/// The ladder's verdict for one re-open.
enum LadderStep {
    /// Sleep this long, then re-open.
    Backoff(Duration),
    /// The cap tripped — terminate the reader with this reason.
    Trip(String),
}

impl ReopenLadder {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            total_reopens: 0,
            window_reopens: VecDeque::new(),
            // Bails at 10 in a row (the pre-R11 open-path cap), now measuring
            // productivity rather than open success.
            max_consecutive: 10,
            // >20 re-opens inside a rolling minute is a sustained flap even if
            // each one committed and cleared the streak.
            rate_window: Duration::from_secs(60),
            rate_cap: 20,
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }

    fn reopens(&self) -> u64 {
        self.total_reopens
    }

    fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Record a re-open of a session that produced `commits` committed
    /// transactions, and return the next step: a bounded backoff, or a trip
    /// that must terminate the reader.
    fn note_reopen(&mut self, now: Instant, commits: u64) -> LadderStep {
        self.total_reopens += 1;
        if commits > 0 {
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures += 1;
        }
        self.window_reopens.push_back(now);
        while let Some(&front) = self.window_reopens.front() {
            if now.saturating_duration_since(front) > self.rate_window {
                self.window_reopens.pop_front();
            } else {
                break;
            }
        }
        if self.consecutive_failures >= self.max_consecutive {
            return LadderStep::Trip(format!(
                "{} consecutive re-opens with no committed transaction between them",
                self.consecutive_failures
            ));
        }
        if self.window_reopens.len() > self.rate_cap {
            return LadderStep::Trip(format!(
                "{} re-opens within {:?} — a sustained flap",
                self.window_reopens.len(),
                self.rate_window
            ));
        }
        // Backoff grows with the unproductive streak (a productive re-open,
        // streak 0, backs off the base minimum). Capped at `max_backoff`.
        let shift = self.consecutive_failures.saturating_sub(1).min(20);
        let backoff = self
            .base_backoff
            .saturating_mul(1u32 << shift)
            .min(self.max_backoff);
        LadderStep::Backoff(backoff)
    }
}

/// Run the reader and reopen transiently severed sessions on a bounded backoff.
pub async fn run(
    source: &Arc<ReplicationSource>,
    config: &ReaderConfig,
    publisher: &JetStreamPublisher,
    token: &CancellationToken,
) -> anyhow::Result<()> {
    let monitor = token.child_token();
    source.start_monitor(Duration::from_secs(30), 256 << 20, monitor.clone());
    let _monitor_shutdown = monitor.drop_guard();
    let is_stopping = || token.is_cancelled();
    let wait_to_retry = async |delay| {
        tokio::select! {
            () = token.cancelled() => true,
            () = tokio::time::sleep(delay) => false,
        }
    };

    let mut ladder = ReopenLadder::new();
    loop {
        if token.is_cancelled() {
            return Ok(());
        }
        let mut session = match source
            .open(Duration::from_secs(5), token.child_token())
            .await
        {
            Ok(session) => session,
            Err(error) => match error.fate {
                SessionFate::Cancelled => return Ok(()),
                SessionFate::CaptureGap => {
                    return Err(capture_failure(source.name(), error.source));
                }
                SessionFate::Fatal => return Err(error.source.context("open session")),
                SessionFate::Reopen => {
                    ladder_step_or_bail(&mut ladder, 0, &error.source, token).await?;
                    continue;
                }
            },
        };
        tracing::info!(
            reopens = ladder.reopens(),
            "walsender session open; draining"
        );

        let outcome = drain(
            &mut session,
            config,
            publisher,
            &is_stopping,
            &wait_to_retry,
        )
        .await;
        // Shutting down a session cancels its own token. Check caller intent first.
        let cancellation_requested = token.is_cancelled();
        session.shutdown().await;
        match outcome.result {
            Ok(()) => {
                tracing::info!(
                    reopens = ladder.reopens(),
                    commits = outcome.commits,
                    "shutdown requested; exiting cleanly"
                );
                return Ok(());
            }
            Err(_) if cancellation_requested => return Ok(()),
            Err(error) => match error.fate {
                SessionFate::Cancelled => return Ok(()),
                SessionFate::CaptureGap => {
                    return Err(capture_failure(source.name(), error.source));
                }
                SessionFate::Fatal => return Err(error.source.context("drain")),
                SessionFate::Reopen => {
                    ladder_step_or_bail(&mut ladder, outcome.commits, &error.source, token).await?;
                }
            },
        }
    }
}

fn capture_failure(name: &str, error: anyhow::Error) -> anyhow::Error {
    tracing::error!(target: "wamn::event_reader", event = CAPTURE_GAP_EVENT, "slot" = name,
        %error, "capture stopped; recover the slot and restart the reader");
    error.context("CDC capture gap")
}

/// Feed one re-open into the shared ladder and act on its verdict: bail if the
/// cap tripped (R11 — nonzero exit), else back off the returned interval
/// (interruptible by shutdown) before the loop re-opens.
async fn ladder_step_or_bail(
    ladder: &mut ReopenLadder,
    commits: u64,
    e: &anyhow::Error,
    token: &CancellationToken,
) -> anyhow::Result<()> {
    match ladder.note_reopen(Instant::now(), commits) {
        LadderStep::Trip(reason) => {
            bail!("reader reopen cap tripped ({reason}); last error: {e}")
        }
        LadderStep::Backoff(delay) => {
            tracing::warn!(
                error = %e,
                reopens = ladder.reopens(),
                consecutive_failures = ladder.consecutive_failures(),
                backoff_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                "session severed/failed; backing off before re-open"
            );
            tokio::select! {
                () = token.cancelled() => {}
                () = tokio::time::sleep(delay) => {}
            }
            Ok(())
        }
    }
}
