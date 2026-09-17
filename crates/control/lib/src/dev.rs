//! Ordered orchestration boundary for the local development loop.
//!
//! This module owns stage order only. Stage implementations remain with their
//! existing migration, build, gate, and publication owners and enter through
//! [`DevStageRunner`].

pub mod activation;
#[cfg(target_os = "linux")]
pub mod command;
pub mod config;
#[cfg(target_os = "linux")]
pub mod coordinator;
pub mod environment;
#[cfg(target_os = "linux")]
mod native_tui;
#[cfg(target_os = "linux")]
pub mod observations;
#[cfg(target_os = "linux")]
mod operator;
pub mod pat_issuer;
pub mod read;
pub mod target_database;
#[cfg(target_os = "linux")]
pub mod tui;
pub mod up;
#[cfg(target_os = "linux")]
pub mod watch;

use std::error::Error;
use std::fmt;
use std::time::{Duration, Instant};

use read::DevRuntimeEndpoint;

// Cold local builds may take longer than edits. Metadata probes have their own
// smaller bound; neither kind can prevent cooperative shutdown indefinitely.
const PREPARATION_TIMEOUT: Duration = Duration::from_mins(45);
const INPUT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

async fn execute_preparation(
    command: &mut tokio::process::Command,
    timeout: Duration,
) -> anyhow::Result<std::process::Output> {
    wamn_control::owned_command::execute(command, timeout, Duration::from_secs(5)).await
}

/// Exact stage order of one local development run.
pub const DEV_STAGE_ORDER: [DevStage; 10] = [
    DevStage::Migrate,
    DevStage::Introspect,
    DevStage::Generate,
    DevStage::Build,
    DevStage::Virtualize,
    DevStage::Acl,
    DevStage::Admit,
    DevStage::Gate,
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
            Self::Acl => "acl",
            Self::Release => "release",
            Self::Activate => "activate",
        }
    }
}

impl fmt::Display for DevStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Whole-worktree source state observed through Git.
pub use wamn_control::git_source::GitSourceState as DevSourceState;

/// One client-owned invalidation delivered to the watch engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevInvalidation {
    /// The event has no effect on generated or deployed development state.
    Ignore,
    /// Re-run the exact stage suffix beginning at `from`.
    Rerun { from: DevStage },
}

/// Typed source of watch invalidations.
///
/// Filesystem classification belongs to the client adapter. The engine only
/// consumes stage identities. `try_next` drains changes that
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

    /// Reset the exact suffix about to run while retaining its completed prefix.
    fn reset(&mut self, _from: DevStage) {}

    /// Report that one stage has begun.
    fn stage_started(&mut self, _stage: DevStage) {}

    /// Report that one stage completed successfully.
    fn stage_completed(&mut self, _stage: DevStage) {}

    /// Report the typed client-facing form of one stage failure.
    fn stage_failed(&mut self, _stage: DevStage, _failure: DevStageFailure) {}

    /// Translate an owner error once at the development-engine boundary.
    fn classify_error(&self, error: &Self::Error) -> DevStageFailure {
        DevStageFailure::new(
            DevRunErrorKind::StageFailed.as_str(),
            error.to_string(),
            None,
        )
    }

    /// Execute exactly one stage.
    fn run(&mut self, stage: DevStage) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Report that one stage was skipped because its input is unchanged.
    fn stage_skipped(&mut self, _stage: DevStage) {}

    /// Facts observed during the run that the result must carry.
    ///
    /// Read once, after the last stage. A run that refuses reports nothing
    /// here, because the error already carries the refusal.
    fn run_notices(&self) -> Vec<DevRunNotice> {
        Vec::new()
    }

    /// Whether this stage's input is unchanged since the previous run.
    ///
    /// Only a stage whose output SURVIVES the run boundary may answer true. A
    /// stage whose work an earlier stage discards, or whose output lives in a
    /// database the run recreates, has nothing to skip to.
    fn stage_is_unchanged(
        &mut self,
        _stage: DevStage,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send {
        async { Ok(false) }
    }

    /// Prepare the target before the first stage of a run.
    ///
    /// A disposable runner checks its lease and recreates invalidated schema here.
    fn prepare_run(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        async { Ok(()) }
    }
}

/// Stable category of a failed development run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevRunErrorKind {
    StageFailed,
}

impl DevRunErrorKind {
    /// Stable diagnostic code for this error category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StageFailed => "dev-stage-failed",
        }
    }
}

/// Cloneable client-facing context for one failed development stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevStageFailure {
    code: Box<str>,
    detail: Box<str>,
    remedy: Option<Box<str>>,
}

impl DevStageFailure {
    /// Build one closed diagnostic without retaining a non-cloneable error source.
    pub fn new(
        code: impl Into<Box<str>>,
        detail: impl Into<Box<str>>,
        remedy: Option<&str>,
    ) -> Self {
        Self {
            code: code.into(),
            detail: detail.into(),
            remedy: remedy.map(Into::into),
        }
    }

    /// Stable refusal or failure code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Actionable, credential-free failure context.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Fixed remedy when the owning failure defines one.
    pub fn remedy(&self) -> Option<&str> {
        self.remedy.as_deref()
    }
}

/// Failure of one ordered development run.
#[derive(Debug)]
pub struct DevRunError {
    kind: DevRunErrorKind,
    stage: DevStage,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl DevRunError {
    fn stage_failed(stage: DevStage, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            kind: DevRunErrorKind::StageFailed,
            stage,
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
}

impl fmt::Display for DevRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at {}", self.kind.as_str(), self.stage)?;
        if let Some(source) = &self.source {
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
pub struct DevRunResult {
    completed: Box<[DevStage]>,
    skipped: Box<[DevStage]>,
    timings: Box<[(DevStage, Duration)]>,
    prepared: Duration,
    notices: Box<[DevRunNotice]>,
}

/// One fact a run reports even though it did not refuse.
///
/// A notice is not a warning about something that might happen. It is already
/// true of this run, and it is what a DURABLE target would have refused. The
/// first one is the base component pin a development run resolved past: the
/// authored manifest names bytes the run did not build, so a promotion from
/// this source will refuse until the pin is reminted. Saying it here is what
/// stops that from being discovered at promotion time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevRunNotice {
    code: Box<str>,
    detail: Box<str>,
}

impl DevRunNotice {
    /// Build one notice under a stable code.
    pub fn new(code: impl Into<Box<str>>, detail: impl Into<Box<str>>) -> Self {
        Self {
            code: code.into(),
            detail: detail.into(),
        }
    }

    /// Stable category, safe for a scripted caller to match on.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// What is true of this run, in one line.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl DevRunResult {
    /// Stages completed by the runner, in execution order.
    ///
    /// A skipped stage is completed: its work is already present. Read
    /// [`Self::skipped`] to tell the two apart.
    pub fn completed(&self) -> &[DevStage] {
        &self.completed
    }

    /// Stages the runner reported as unchanged, so their work was not redone.
    pub fn skipped(&self) -> &[DevStage] {
        &self.skipped
    }

    /// Facts this run reported without refusing.
    pub fn notices(&self) -> &[DevRunNotice] {
        &self.notices
    }

    /// Wall time each stage took, in execution order.
    ///
    /// A skipped stage is here too, and its time is the cost of deciding it was
    /// unchanged. That is the number worth reading: a skip that costs as much
    /// as the stage bought nothing.
    pub fn timings(&self) -> &[(DevStage, Duration)] {
        &self.timings
    }

    /// Wall time spent preparing the target before the first stage.
    ///
    /// Includes checking the retained lease and recreating an invalidated target.
    pub const fn prepared(&self) -> Duration {
        self.prepared
    }
}

/// Result of one serialized watch run.
#[derive(Debug)]
pub struct DevWatchOutcome {
    from: DevStage,
    result: Result<DevRunResult, DevRunError>,
}

impl DevWatchOutcome {
    /// First stage requested by the coalesced invalidations.
    pub const fn from(&self) -> DevStage {
        self.from
    }

    /// Borrow the run result reported to the client.
    pub const fn result(&self) -> &Result<DevRunResult, DevRunError> {
        &self.result
    }

    /// Consume the outcome and return its run result.
    pub fn into_result(self) -> Result<DevRunResult, DevRunError> {
        self.result
    }
}

/// Receives each completed watch run without owning orchestration.
pub trait DevWatchObserver {
    /// Report one success or failure before the engine accepts another run.
    fn completed(&mut self, outcome: DevWatchOutcome);

    /// Report the activated release before a one-shot session holds it open.
    ///
    /// The default is a no-op because only the plain renderer prints: the
    /// interactive client reads the same endpoint live off the read handle,
    /// and a silent session has nowhere to print it. The endpoint is optional
    /// for the same reason [`crate::dev::command`] treats it as optional when
    /// the run tore down, and the result travels with it so the caller can
    /// emit the completion line first without reaching back into the session.
    fn served(&mut self, _result: &DevRunResult, _endpoint: Option<&DevRuntimeEndpoint>) {}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingRun {
    from: DevStage,
}

impl PendingRun {
    fn include(pending: &mut Option<Self>, invalidation: DevInvalidation) {
        let DevInvalidation::Rerun { from } = invalidation else {
            return;
        };

        match pending {
            Some(current) => {
                if from.position() < current.from.position() {
                    current.from = from;
                }
            }
            None => {
                *pending = Some(Self { from });
            }
        }
    }
}

/// Run the whole fixed stage sequence once.
pub async fn run_once<R>(runner: &mut R) -> Result<DevRunResult, DevRunError>
where
    R: DevStageRunner + Send,
{
    run_suffix(DevStage::Migrate, runner).await
}

async fn run_suffix<R>(from: DevStage, runner: &mut R) -> Result<DevRunResult, DevRunError>
where
    R: DevStageRunner,
{
    let first = from.position();
    runner.reset(from);
    let preparing = Instant::now();
    if let Err(error) = runner.prepare_run().await {
        let failure = runner.classify_error(&error);
        runner.stage_failed(from, failure);
        return Err(DevRunError::stage_failed(from, error));
    }
    let prepared = preparing.elapsed();
    let mut completed = Vec::with_capacity(DEV_STAGE_ORDER.len() - first);
    let mut skipped = Vec::new();
    let mut timings = Vec::with_capacity(DEV_STAGE_ORDER.len() - first);
    for stage in DEV_STAGE_ORDER.into_iter().skip(first) {
        let began = Instant::now();
        runner.stage_started(stage);
        // The decision is made HERE and not inside the stage, because entering
        // a stage tears down a live activation and discards the downstream work
        // the skip exists to keep.
        match runner.stage_is_unchanged(stage).await {
            Ok(true) => {
                runner.stage_skipped(stage);
                completed.push(stage);
                skipped.push(stage);
                timings.push((stage, began.elapsed()));
                continue;
            }
            Ok(false) => {}
            Err(error) => {
                let failure = runner.classify_error(&error);
                runner.stage_failed(stage, failure);
                return Err(DevRunError::stage_failed(stage, error));
            }
        }
        if let Err(error) = runner.run(stage).await {
            let failure = runner.classify_error(&error);
            runner.stage_failed(stage, failure);
            return Err(DevRunError::stage_failed(stage, error));
        }
        runner.stage_completed(stage);
        completed.push(stage);
        timings.push((stage, began.elapsed()));
    }
    Ok(DevRunResult {
        completed: completed.into_boxed_slice(),
        skipped: skipped.into_boxed_slice(),
        timings: timings.into_boxed_slice(),
        prepared,
        notices: runner.run_notices().into_boxed_slice(),
    })
}

/// Watch for invalidations and run each coalesced suffix.
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
    R: DevStageRunner + Send,
    S: DevInvalidationSource + Send,
    O: DevWatchObserver + Send,
{
    run_watch_loop(runner, source, observer).await
}

async fn run_watch_loop<R, S, O>(
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
            let result = run_suffix(pending.from, runner).await;
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

    /// A runner that finishes every stage and has one thing to say about it.
    #[derive(Debug, Default)]
    struct NoticingRunner {
        invoked: Vec<DevStage>,
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "`DevStageRunner` declares its stage seam as `async fn` because a production \
                  stage does I/O; this test runner answers from memory and still has to \
                  match the trait"
    )]
    impl DevStageRunner for NoticingRunner {
        type Error = Infallible;

        async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
            self.invoked.push(stage);
            Ok(())
        }

        fn run_notices(&self) -> Vec<DevRunNotice> {
            vec![DevRunNotice::new(
                "pin stale",
                "client_acme_receiving@3.0.0 wamn.json names sha256:aa, built sha256:bb",
            )]
        }
    }

    /// The result carries what the run reported without refusing. A durable
    /// publish from this same source refuses on that pin, so a run that stayed
    /// silent would push the discovery to promotion time.
    #[tokio::test]
    async fn a_run_that_proceeds_past_a_stale_pin_reports_it_in_the_result() {
        let mut runner = NoticingRunner::default();

        let result = run_once(&mut runner)
            .await
            .expect("a disposable target runs every stage");

        assert_eq!(result.completed().len(), DEV_STAGE_ORDER.len());
        assert_eq!(result.notices().len(), 1);
        assert_eq!(result.notices()[0].code(), "pin stale");
        assert!(
            result.notices()[0].detail().contains("sha256:bb"),
            "the notice names the digest the run actually built"
        );
    }

    /// The default is silence. A runner with nothing to report adds no line,
    /// so the completed and served lines a scripted caller reads never move.
    #[tokio::test]
    async fn a_run_with_nothing_to_report_carries_no_notices() {
        let mut runner = RecordingRunner::default();

        let result = run_once(&mut runner).await.expect("a clean run completes");

        assert!(result.notices().is_empty());
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "`DevStageRunner` declares its stage seam as `async fn` because a production \
                  stage does I/O; this test runner answers from memory and still has to \
                  match the trait"
    )]
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

    #[derive(Debug, PartialEq, Eq)]
    enum LifecycleEvent {
        Reset(DevStage),
        Started(DevStage),
        Completed(DevStage),
        Failed(DevStage, DevStageFailure),
    }

    #[derive(Debug)]
    struct LifecycleRunner {
        events: Vec<LifecycleEvent>,
        fail_at: DevStage,
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "`DevStageRunner` declares its stage seam as `async fn` because a production \
                  stage does I/O; this test runner answers from memory and still has to \
                  match the trait"
    )]
    impl DevStageRunner for LifecycleRunner {
        type Error = SyntheticStageError;

        fn reset(&mut self, from: DevStage) {
            self.events.push(LifecycleEvent::Reset(from));
        }

        fn stage_started(&mut self, stage: DevStage) {
            self.events.push(LifecycleEvent::Started(stage));
        }

        fn stage_completed(&mut self, stage: DevStage) {
            self.events.push(LifecycleEvent::Completed(stage));
        }

        fn stage_failed(&mut self, stage: DevStage, failure: DevStageFailure) {
            self.events.push(LifecycleEvent::Failed(stage, failure));
        }

        async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
            if stage == self.fail_at {
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

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "`DevInvalidationSource` declares `async fn` because the production source \
                  waits on a watcher; this test source answers from a queue and still \
                  has to match the trait"
    )]
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

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "`DevStageRunner` declares its stage seam as `async fn` because a production \
                  stage does I/O; this test runner answers from memory and still has to \
                  match the trait"
    )]
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

        let result = run_once(&mut runner)
            .await
            .expect("clean semantic runner completes");

        assert_eq!(runner.invoked, DEV_STAGE_ORDER);
        assert_eq!(result.completed(), DEV_STAGE_ORDER.as_slice());
    }

    #[tokio::test]
    async fn stage_failure_stops_before_every_later_side_effect() {
        let mut runner = RecordingRunner::failing_at(DevStage::Virtualize);

        let error = run_once(&mut runner)
            .await
            .expect_err("synthetic virtualization failure must stop the run");

        assert_eq!(error.kind(), DevRunErrorKind::StageFailed);
        assert_eq!(error.stage(), DevStage::Virtualize);
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
    async fn runner_lifecycle_reports_the_exact_suffix_and_failure() {
        let mut runner = LifecycleRunner {
            events: Vec::new(),
            fail_at: DevStage::Gate,
        };

        let error = run_suffix(DevStage::Admit, &mut runner)
            .await
            .expect_err("the synthetic Gate failure must stop the suffix");

        assert_eq!(error.stage(), DevStage::Gate);
        assert_eq!(
            runner.events,
            [
                LifecycleEvent::Reset(DevStage::Admit),
                LifecycleEvent::Started(DevStage::Admit),
                LifecycleEvent::Completed(DevStage::Admit),
                LifecycleEvent::Started(DevStage::Gate),
                LifecycleEvent::Failed(
                    DevStage::Gate,
                    DevStageFailure::new("dev-stage-failed", "synthetic gate failure", None)
                ),
            ]
        );
    }

    /// A runner whose target is disposable, and which can refuse its own
    /// preparation. Both are the seams a production loop uses to recreate the
    /// target database before the first stage.
    #[derive(Default)]
    struct DisposableTargetRunner {
        invoked: Vec<DevStage>,
        prepared: u8,
        refuse_preparation: bool,
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "`DevStageRunner` declares its stage seam as `async fn` because a production \
                  stage does I/O; this test runner answers from memory and still has to \
                  match the trait"
    )]
    impl DevStageRunner for DisposableTargetRunner {
        type Error = SyntheticStageError;

        async fn prepare_run(&mut self) -> Result<(), Self::Error> {
            self.prepared += 1;
            if self.refuse_preparation {
                return Err(SyntheticStageError(DevStage::Migrate));
            }
            Ok(())
        }

        async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
            self.invoked.push(stage);
            Ok(())
        }
    }

    /// A runner that reports one stage unchanged, and records what actually ran.
    #[derive(Default)]
    struct SkippingRunner {
        invoked: Vec<DevStage>,
        reported_skips: Vec<DevStage>,
        unchanged: Option<DevStage>,
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "`DevStageRunner` declares its stage seam as `async fn` because a production \
                  stage does I/O; this test runner answers from memory and still has to \
                  match the trait"
    )]
    impl DevStageRunner for SkippingRunner {
        type Error = SyntheticStageError;

        fn stage_skipped(&mut self, stage: DevStage) {
            self.reported_skips.push(stage);
        }

        async fn stage_is_unchanged(&mut self, stage: DevStage) -> Result<bool, Self::Error> {
            Ok(self.unchanged == Some(stage))
        }

        async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
            self.invoked.push(stage);
            Ok(())
        }
    }

    #[tokio::test]
    async fn an_unchanged_stage_is_not_run_and_is_reported_as_skipped() {
        let mut runner = SkippingRunner {
            unchanged: Some(DevStage::Generate),
            ..SkippingRunner::default()
        };

        let result = run_once(&mut runner)
            .await
            .expect("an unchanged stage does not fail a run");

        assert_eq!(result.skipped(), [DevStage::Generate]);
        assert_eq!(
            result.completed(),
            DEV_STAGE_ORDER,
            "a skipped stage is still completed: its work is present"
        );
        assert!(
            !runner.invoked.contains(&DevStage::Generate),
            "a skipped stage must not be entered, because entering it discards downstream work"
        );
        assert_eq!(runner.reported_skips, [DevStage::Generate]);
        assert_eq!(runner.invoked.len(), DEV_STAGE_ORDER.len() - 1);
    }

    #[tokio::test]
    async fn a_run_with_nothing_unchanged_reports_no_skips() {
        let mut runner = SkippingRunner::default();

        let result = run_once(&mut runner).await.expect("a full run succeeds");

        assert!(result.skipped().is_empty());
        assert_eq!(runner.invoked, DEV_STAGE_ORDER);
    }

    #[tokio::test]
    async fn the_target_is_prepared_once_before_every_stage_runs() {
        let mut runner = DisposableTargetRunner::default();

        let result = run_once(&mut runner)
            .await
            .expect("a prepared target runs every stage");

        assert_eq!(result.completed(), DEV_STAGE_ORDER);
        assert_eq!(runner.invoked, DEV_STAGE_ORDER);
        assert_eq!(
            runner.prepared, 1,
            "the target is prepared once per run, not once per stage"
        );
    }

    #[tokio::test]
    async fn a_refused_preparation_stops_the_run_at_its_first_stage() {
        let mut runner = DisposableTargetRunner {
            refuse_preparation: true,
            ..DisposableTargetRunner::default()
        };

        let error = run_once(&mut runner)
            .await
            .expect_err("a target that cannot be prepared must not run a stage against it");

        assert_eq!(error.kind(), DevRunErrorKind::StageFailed);
        assert_eq!(error.stage(), DevStage::Migrate);
        assert!(
            runner.invoked.is_empty(),
            "no stage runs against an unprepared target"
        );
    }

    #[tokio::test]
    async fn watch_coalesces_to_earliest_stage() {
        let events = FakeEvents::with([
            DevInvalidation::Ignore,
            DevInvalidation::Rerun {
                from: DevStage::Release,
            },
            DevInvalidation::Rerun {
                from: DevStage::Generate,
            },
            DevInvalidation::Rerun {
                from: DevStage::Gate,
            },
        ]);
        let mut source = FakeSource::new(events);
        let mut runner = WatchRunner::default();
        let mut observer = RecordingObserver::default();

        run_watch_loop(&mut runner, &mut source, &mut observer)
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
        }]);
        let mut source = FakeSource::new(events.clone());
        let mut runner = WatchRunner::inject_during(
            DevStage::Gate,
            vec![
                DevInvalidation::Rerun {
                    from: DevStage::Acl,
                },
                DevInvalidation::Ignore,
                DevInvalidation::Rerun {
                    from: DevStage::Introspect,
                },
            ],
            events,
        );
        let mut observer = RecordingObserver::default();

        run_watch_loop(&mut runner, &mut source, &mut observer)
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
        }]);
        let mut source = FakeSource::new(events.clone());
        let mut runner = WatchRunner::inject_during(
            DevStage::Gate,
            vec![DevInvalidation::Rerun {
                from: DevStage::Virtualize,
            }],
            events,
        );
        runner.fail_once_at = Some(DevStage::Gate);
        let mut observer = RecordingObserver::default();

        run_watch_loop(&mut runner, &mut source, &mut observer)
            .await
            .expect("fake source is infallible");

        let expected = [
            DevStage::Build,
            DevStage::Virtualize,
            DevStage::Acl,
            DevStage::Admit,
            DevStage::Gate,
        ]
        .into_iter()
        .chain(
            DEV_STAGE_ORDER[DevStage::Virtualize.position()..]
                .iter()
                .copied(),
        )
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
}
