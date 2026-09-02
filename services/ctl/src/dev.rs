//! Ordered orchestration boundary for the local development loop.
//!
//! This module owns stage order and the committed-source boundary only. Stage
//! implementations remain with their existing migration, build, gate, and
//! publication owners and enter through [`DevStageRunner`].

use std::error::Error;
use std::fmt;

/// Stable refusal code for a dirty worktree reaching committed-source work.
pub const DIRTY_WORKTREE_ERROR: &str = "dev-worktree-dirty";

/// Stable remedy for [`DIRTY_WORKTREE_ERROR`].
pub const COMMIT_WORKTREE_REMEDY: &str = "commit the worktree";

/// Exact stage order of one local development run.
pub const DEV_STAGE_ORDER: [DevStage; 12] = [
    DevStage::Migrate,
    DevStage::Introspect,
    DevStage::Generate,
    DevStage::Build,
    DevStage::Virtualize,
    DevStage::Admit,
    DevStage::Gate,
    DevStage::Publish,
    DevStage::Apply,
    DevStage::Acl,
    DevStage::Release,
    DevStage::Activate,
];

/// One stable stage identity in the development loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevStage {
    Migrate,
    Introspect,
    Generate,
    Build,
    Virtualize,
    Admit,
    Gate,
    Publish,
    Apply,
    Acl,
    Release,
    Activate,
}

impl DevStage {
    fn position(self) -> usize {
        DEV_STAGE_ORDER
            .iter()
            .position(|stage| *stage == self)
            .expect("every DevStage belongs to DEV_STAGE_ORDER")
    }

    /// Stable command-facing spelling of this stage.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Migrate => "migrate",
            Self::Introspect => "introspect",
            Self::Generate => "generate",
            Self::Build => "build",
            Self::Virtualize => "virtualize",
            Self::Admit => "admit",
            Self::Gate => "gate",
            Self::Publish => "publish",
            Self::Apply => "apply",
            Self::Acl => "acl",
            Self::Release => "release",
            Self::Activate => "activate",
        }
    }

    /// Source-integrity boundary this stage requires.
    pub const fn boundary(self) -> DevStageBoundary {
        match self {
            Self::Migrate
            | Self::Introspect
            | Self::Generate
            | Self::Build
            | Self::Virtualize
            | Self::Admit
            | Self::Gate => DevStageBoundary::SavedBytes,
            Self::Publish | Self::Apply | Self::Acl | Self::Release | Self::Activate => {
                DevStageBoundary::CommittedSource
            }
        }
    }
}

impl fmt::Display for DevStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Source-integrity requirement at a stage boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevStageBoundary {
    /// Saved worktree bytes may execute against disposable development state.
    SavedBytes,
    /// The stage can mint or deploy durable provenance and requires a commit.
    CommittedSource,
}

/// Source state supplied by the client at the start of one run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevSourceState {
    Clean,
    Dirty,
}

/// One client-owned invalidation delivered to the watch engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevInvalidation {
    /// The event has no effect on generated or deployed development state.
    Ignore,
    /// Re-run the exact stage suffix beginning at `from`.
    Rerun {
        from: DevStage,
        source_state: DevSourceState,
    },
}

/// Typed source of watch invalidations.
///
/// Filesystem classification belongs to the client adapter. The engine only
/// consumes stage identities and source state. `try_next` drains changes that
/// accumulated before or during a run without polling the filesystem.
pub trait DevInvalidationSource {
    type Error: Error + Send + Sync + 'static;

    /// Wait for the next invalidation, or return `None` when the source closes.
    fn next(&mut self)
    -> impl Future<Output = Result<Option<DevInvalidation>, Self::Error>> + Send;

    /// Return one already-available invalidation without waiting.
    fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error>;
}

/// The one execution seam implemented by stage owners and semantic tests.
pub trait DevStageRunner {
    type Error: Error + Send + Sync + 'static;

    /// Execute exactly one stage.
    fn run(&mut self, stage: DevStage) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Stable category of a failed development run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevRunErrorKind {
    DirtyWorktree,
    StageFailed,
}

impl DevRunErrorKind {
    /// Stable diagnostic code for this error category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DirtyWorktree => DIRTY_WORKTREE_ERROR,
            Self::StageFailed => "dev-stage-failed",
        }
    }
}

/// Failure of one ordered development run.
#[derive(Debug)]
pub struct DevRunError {
    kind: DevRunErrorKind,
    stage: DevStage,
    remedy: Option<&'static str>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl DevRunError {
    fn dirty_worktree(stage: DevStage) -> Self {
        Self {
            kind: DevRunErrorKind::DirtyWorktree,
            stage,
            remedy: Some(COMMIT_WORKTREE_REMEDY),
            source: None,
        }
    }

    fn stage_failed(stage: DevStage, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            kind: DevRunErrorKind::StageFailed,
            stage,
            remedy: None,
            source: Some(Box::new(source)),
        }
    }

    /// Stable error category.
    pub const fn kind(&self) -> DevRunErrorKind {
        self.kind
    }

    /// Stage that failed or was refused before invocation.
    pub const fn stage(&self) -> DevStage {
        self.stage
    }

    /// Actionable fixed remedy when this error category owns one.
    pub const fn remedy(&self) -> Option<&'static str> {
        self.remedy
    }
}

impl fmt::Display for DevRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at {}", self.kind.as_str(), self.stage)?;
        if let Some(remedy) = self.remedy {
            write!(formatter, ": {remedy}")?;
        } else if let Some(source) = &self.source {
            write!(formatter, ": {source}")?;
        }
        Ok(())
    }
}

impl Error for DevRunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Successful result of one exact ordered run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevRunReceipt {
    completed: Box<[DevStage]>,
}

impl DevRunReceipt {
    /// Stages completed by the runner, in execution order.
    pub fn completed(&self) -> &[DevStage] {
        &self.completed
    }
}

/// Result of one serialized watch run.
#[derive(Debug)]
pub struct DevWatchOutcome {
    from: DevStage,
    result: Result<DevRunReceipt, DevRunError>,
}

impl DevWatchOutcome {
    /// First stage requested by the coalesced invalidations.
    pub const fn from(&self) -> DevStage {
        self.from
    }

    /// Borrow the run result reported to the client.
    pub const fn result(&self) -> &Result<DevRunReceipt, DevRunError> {
        &self.result
    }

    /// Consume the outcome and return its run result.
    pub fn into_result(self) -> Result<DevRunReceipt, DevRunError> {
        self.result
    }
}

/// Receives each completed watch run without owning orchestration.
pub trait DevWatchObserver {
    /// Report one success or failure before the engine accepts another run.
    fn completed(&mut self, outcome: DevWatchOutcome);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingRun {
    from: DevStage,
    source_state: DevSourceState,
}

impl PendingRun {
    fn include(pending: &mut Option<Self>, invalidation: DevInvalidation) {
        let DevInvalidation::Rerun { from, source_state } = invalidation else {
            return;
        };

        match pending {
            Some(current) => {
                if from.position() < current.from.position() {
                    current.from = from;
                }
                // The latest event describes the current worktree state, while
                // every event contributes to the earliest affected stage.
                current.source_state = source_state;
            }
            None => {
                *pending = Some(Self { from, source_state });
            }
        }
    }
}

/// Run the fixed development stage sequence once.
///
/// A dirty source may exercise saved-byte stages through the gate. The engine
/// refuses before invoking the first stage that can mint committed provenance,
/// so no later deployment side effect can run under a false source identity.
pub async fn run_once<R>(
    source_state: DevSourceState,
    runner: &mut R,
) -> Result<DevRunReceipt, DevRunError>
where
    R: DevStageRunner,
{
    run_suffix(DevStage::Migrate, source_state, runner).await
}

/// Run the exact development stage suffix beginning at `from`.
pub async fn run_suffix<R>(
    from: DevStage,
    source_state: DevSourceState,
    runner: &mut R,
) -> Result<DevRunReceipt, DevRunError>
where
    R: DevStageRunner,
{
    let first = from.position();
    let mut completed = Vec::with_capacity(DEV_STAGE_ORDER.len() - first);
    for stage in DEV_STAGE_ORDER.into_iter().skip(first) {
        if source_state == DevSourceState::Dirty
            && stage.boundary() == DevStageBoundary::CommittedSource
        {
            return Err(DevRunError::dirty_worktree(stage));
        }
        runner
            .run(stage)
            .await
            .map_err(|error| DevRunError::stage_failed(stage, error))?;
        completed.push(stage);
    }
    Ok(DevRunReceipt {
        completed: completed.into_boxed_slice(),
    })
}

/// Consume invalidations and execute one affected suffix at a time.
///
/// Events already queued together coalesce to the earliest affected stage. An
/// event arriving during a run remains queued for the next run, so runs never
/// overlap. Stage failures are reported and do not terminate the watch loop;
/// only an invalidation-source failure does.
pub async fn run_watch<R, S, O>(
    runner: &mut R,
    source: &mut S,
    observer: &mut O,
) -> Result<(), S::Error>
where
    R: DevStageRunner,
    S: DevInvalidationSource,
    O: DevWatchObserver,
{
    while let Some(first) = source.next().await? {
        let mut pending = None;
        PendingRun::include(&mut pending, first);
        while let Some(invalidation) = source.try_next()? {
            PendingRun::include(&mut pending, invalidation);
        }

        if let Some(pending) = pending {
            let result = run_suffix(pending.from, pending.source_state, runner).await;
            observer.completed(DevWatchOutcome {
                from: pending.from,
                result,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct SyntheticStageError(DevStage);

    impl fmt::Display for SyntheticStageError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "synthetic {} failure", self.0)
        }
    }

    impl Error for SyntheticStageError {}

    #[derive(Debug, Default)]
    struct RecordingRunner {
        invoked: Vec<DevStage>,
        fail_at: Option<DevStage>,
    }

    impl RecordingRunner {
        fn failing_at(stage: DevStage) -> Self {
            Self {
                invoked: Vec::new(),
                fail_at: Some(stage),
            }
        }
    }

    impl DevStageRunner for RecordingRunner {
        type Error = SyntheticStageError;

        async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
            self.invoked.push(stage);
            if self.fail_at == Some(stage) {
                Err(SyntheticStageError(stage))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeEvents(Arc<Mutex<VecDeque<DevInvalidation>>>);

    impl FakeEvents {
        fn with(events: impl IntoIterator<Item = DevInvalidation>) -> Self {
            Self(Arc::new(Mutex::new(events.into_iter().collect())))
        }

        fn push_all(&self, events: impl IntoIterator<Item = DevInvalidation>) {
            self.0.lock().expect("fake event queue lock").extend(events);
        }

        fn pop(&self) -> Option<DevInvalidation> {
            self.0.lock().expect("fake event queue lock").pop_front()
        }
    }

    #[derive(Debug)]
    struct FakeSource {
        events: FakeEvents,
    }

    impl FakeSource {
        fn new(events: FakeEvents) -> Self {
            Self { events }
        }
    }

    impl DevInvalidationSource for FakeSource {
        type Error = Infallible;

        async fn next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
            Ok(self.events.pop())
        }

        fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
            Ok(self.events.pop())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingObserver {
        outcomes: Vec<DevWatchOutcome>,
    }

    impl DevWatchObserver for RecordingObserver {
        fn completed(&mut self, outcome: DevWatchOutcome) {
            self.outcomes.push(outcome);
        }
    }

    #[derive(Debug, Default)]
    struct WatchRunner {
        invoked: Vec<DevStage>,
        inject_at: Option<DevStage>,
        injected: Vec<DevInvalidation>,
        events: Option<FakeEvents>,
        fail_once_at: Option<DevStage>,
    }

    impl WatchRunner {
        fn inject_during(
            stage: DevStage,
            injected: Vec<DevInvalidation>,
            events: FakeEvents,
        ) -> Self {
            Self {
                inject_at: Some(stage),
                injected,
                events: Some(events),
                ..Self::default()
            }
        }
    }

    impl DevStageRunner for WatchRunner {
        type Error = SyntheticStageError;

        async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
            self.invoked.push(stage);
            if self.inject_at == Some(stage) {
                self.inject_at = None;
                self.events
                    .as_ref()
                    .expect("injection has an event queue")
                    .push_all(std::mem::take(&mut self.injected));
            }
            if self.fail_once_at == Some(stage) {
                self.fail_once_at = None;
                return Err(SyntheticStageError(stage));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn clean_source_completes_the_exact_stage_order() {
        let mut runner = RecordingRunner::default();

        let receipt = run_once(DevSourceState::Clean, &mut runner)
            .await
            .expect("clean semantic runner completes");

        assert_eq!(runner.invoked, DEV_STAGE_ORDER);
        assert_eq!(receipt.completed(), DEV_STAGE_ORDER.as_slice());
    }

    #[tokio::test]
    async fn stage_failure_stops_before_every_later_side_effect() {
        let mut runner = RecordingRunner::failing_at(DevStage::Virtualize);

        let error = run_once(DevSourceState::Clean, &mut runner)
            .await
            .expect_err("synthetic virtualization failure must stop the run");

        assert_eq!(error.kind(), DevRunErrorKind::StageFailed);
        assert_eq!(error.stage(), DevStage::Virtualize);
        assert_eq!(error.remedy(), None);
        assert_eq!(
            runner.invoked,
            [
                DevStage::Migrate,
                DevStage::Introspect,
                DevStage::Generate,
                DevStage::Build,
                DevStage::Virtualize,
            ]
        );
        assert_eq!(
            error.to_string(),
            "dev-stage-failed at virtualize: synthetic virtualize failure"
        );
    }

    #[tokio::test]
    async fn dirty_source_reaches_gate_then_refuses_before_publish() {
        let mut runner = RecordingRunner::default();

        let error = run_once(DevSourceState::Dirty, &mut runner)
            .await
            .expect_err("dirty source must not reach durable provenance stages");

        assert_eq!(error.kind(), DevRunErrorKind::DirtyWorktree);
        assert_eq!(error.stage(), DevStage::Publish);
        assert_eq!(error.remedy(), Some(COMMIT_WORKTREE_REMEDY));
        assert_eq!(
            error.to_string(),
            "dev-worktree-dirty at publish: commit the worktree"
        );
        assert_eq!(
            runner.invoked,
            [
                DevStage::Migrate,
                DevStage::Introspect,
                DevStage::Generate,
                DevStage::Build,
                DevStage::Virtualize,
                DevStage::Admit,
                DevStage::Gate,
            ]
        );
    }

    #[tokio::test]
    async fn watch_coalesces_to_earliest_stage_with_latest_source_state() {
        let events = FakeEvents::with([
            DevInvalidation::Ignore,
            DevInvalidation::Rerun {
                from: DevStage::Publish,
                source_state: DevSourceState::Dirty,
            },
            DevInvalidation::Rerun {
                from: DevStage::Generate,
                source_state: DevSourceState::Dirty,
            },
            DevInvalidation::Rerun {
                from: DevStage::Gate,
                source_state: DevSourceState::Clean,
            },
        ]);
        let mut source = FakeSource::new(events);
        let mut runner = WatchRunner::default();
        let mut observer = RecordingObserver::default();

        run_watch(&mut runner, &mut source, &mut observer)
            .await
            .expect("fake source is infallible");

        assert_eq!(runner.invoked, DEV_STAGE_ORDER[2..]);
        assert_eq!(observer.outcomes.len(), 1);
        assert_eq!(observer.outcomes[0].from(), DevStage::Generate);
        assert!(observer.outcomes[0].result().is_ok());
    }

    #[tokio::test]
    async fn changes_during_a_run_form_one_serialized_next_suffix() {
        let events = FakeEvents::with([DevInvalidation::Rerun {
            from: DevStage::Build,
            source_state: DevSourceState::Clean,
        }]);
        let mut source = FakeSource::new(events.clone());
        let mut runner = WatchRunner::inject_during(
            DevStage::Gate,
            vec![
                DevInvalidation::Rerun {
                    from: DevStage::Apply,
                    source_state: DevSourceState::Clean,
                },
                DevInvalidation::Ignore,
                DevInvalidation::Rerun {
                    from: DevStage::Introspect,
                    source_state: DevSourceState::Clean,
                },
            ],
            events,
        );
        let mut observer = RecordingObserver::default();

        run_watch(&mut runner, &mut source, &mut observer)
            .await
            .expect("fake source is infallible");

        let expected = DEV_STAGE_ORDER[3..]
            .iter()
            .chain(&DEV_STAGE_ORDER[1..])
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(runner.invoked, expected);
        assert_eq!(observer.outcomes.len(), 2);
        assert_eq!(observer.outcomes[0].from(), DevStage::Build);
        assert_eq!(observer.outcomes[1].from(), DevStage::Introspect);
        assert!(
            observer
                .outcomes
                .iter()
                .all(|outcome| outcome.result().is_ok())
        );
    }

    #[tokio::test]
    async fn watch_accepts_a_new_invalidation_after_stage_failure() {
        let events = FakeEvents::with([DevInvalidation::Rerun {
            from: DevStage::Build,
            source_state: DevSourceState::Clean,
        }]);
        let mut source = FakeSource::new(events.clone());
        let mut runner = WatchRunner::inject_during(
            DevStage::Gate,
            vec![DevInvalidation::Rerun {
                from: DevStage::Virtualize,
                source_state: DevSourceState::Clean,
            }],
            events,
        );
        runner.fail_once_at = Some(DevStage::Gate);
        let mut observer = RecordingObserver::default();

        run_watch(&mut runner, &mut source, &mut observer)
            .await
            .expect("fake source is infallible");

        let expected = [
            DevStage::Build,
            DevStage::Virtualize,
            DevStage::Admit,
            DevStage::Gate,
        ]
        .into_iter()
        .chain(DEV_STAGE_ORDER[4..].iter().copied())
        .collect::<Vec<_>>();
        assert_eq!(runner.invoked, expected);
        assert_eq!(observer.outcomes.len(), 2);
        let first = observer.outcomes[0]
            .result()
            .as_ref()
            .expect_err("first run fails at the injected stage");
        assert_eq!(first.kind(), DevRunErrorKind::StageFailed);
        assert_eq!(first.stage(), DevStage::Gate);
        assert!(observer.outcomes[1].result().is_ok());
    }

    #[tokio::test]
    async fn dirty_watch_suffix_refuses_before_its_first_provenance_stage() {
        let events = FakeEvents::with([DevInvalidation::Rerun {
            from: DevStage::Apply,
            source_state: DevSourceState::Dirty,
        }]);
        let mut source = FakeSource::new(events);
        let mut runner = WatchRunner::default();
        let mut observer = RecordingObserver::default();

        run_watch(&mut runner, &mut source, &mut observer)
            .await
            .expect("fake source is infallible");

        assert!(runner.invoked.is_empty());
        assert_eq!(observer.outcomes.len(), 1);
        let error = observer.outcomes[0]
            .result()
            .as_ref()
            .expect_err("dirty source must not invoke apply");
        assert_eq!(error.kind(), DevRunErrorKind::DirtyWorktree);
        assert_eq!(error.stage(), DevStage::Apply);
        assert_eq!(error.remedy(), Some(COMMIT_WORKTREE_REMEDY));
    }
}
