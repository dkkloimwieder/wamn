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
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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
    let mut completed = Vec::with_capacity(DEV_STAGE_ORDER.len());
    for stage in DEV_STAGE_ORDER {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
