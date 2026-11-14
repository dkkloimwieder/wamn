use super::{Duration, Error, fmt};

/// Exact stage order of one local development run.
pub const DEV_STAGE_ORDER: [DevStage; 8] = [
    DevStage::Migrate,
    DevStage::Introspect,
    DevStage::Build,
    DevStage::Acl,
    DevStage::Admit,
    DevStage::Condition,
    DevStage::Release,
    DevStage::Activate,
];

/// One stable stage identity in the development loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevStage {
    Migrate,
    Introspect,
    Build,
    Admit,
    Condition,
    Acl,
    Release,
    Activate,
}

/// One client-owned invalidation delivered to the watch engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevInvalidation {
    /// The event has no effect on generated or deployed development state.
    Ignore,
    /// Re-run the exact stage suffix beginning at `from`.
    Rerun { from: DevStage },
}

/// Stable category of a failed development run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevRunErrorType {
    StageFailed,
}

/// Cloneable client-facing context for one failed development stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevStageFailure {
    pub(super) code: Box<str>,
    pub(super) detail: Box<str>,
    pub(super) remedy: Option<Box<str>>,
}

/// Failure of one ordered development run.
#[derive(Debug)]
pub struct DevRunError {
    pub(super) type_: DevRunErrorType,
    pub(super) stage: DevStage,
    pub(super) source: Option<Box<dyn Error + Send + Sync>>,
}

/// Successful result of one exact ordered run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevRunResult {
    pub(super) completed: Box<[DevStage]>,
    pub(super) skipped: Box<[DevStage]>,
    pub(super) timings: Box<[(DevStage, Duration)]>,
    pub(super) prepared: Duration,
    pub(super) notices: Box<[DevRunNotice]>,
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
    pub(super) code: Box<str>,
    pub(super) detail: Box<str>,
}

/// Result of one serialized watch run.
#[derive(Debug)]
pub struct DevWatchOutcome {
    pub(super) from: DevStage,
    pub(super) result: Result<DevRunResult, DevRunError>,
}

impl DevStage {
    pub(super) fn position(self) -> usize {
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
            Self::Build => "build",
            Self::Admit => "admit",
            Self::Condition => "condition",
            Self::Acl => "acl",
            Self::Release => "release",
            Self::Activate => "activate",
        }
    }
}

impl DevRunErrorType {
    /// Stable diagnostic code for this error category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StageFailed => "dev-stage-failed",
        }
    }
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

impl DevRunError {
    pub(super) fn stage_failed(
        stage: DevStage,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            type_: DevRunErrorType::StageFailed,
            stage,
            source: Some(Box::new(source)),
        }
    }

    /// Stable error category.
    pub const fn error_type(&self) -> DevRunErrorType {
        self.type_
    }

    /// Stage that failed or was refused before invocation.
    pub const fn stage(&self) -> DevStage {
        self.stage
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
    /// Includes testing the retained lease and recreating an invalidated target.
    pub const fn prepared(&self) -> Duration {
        self.prepared
    }
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

impl fmt::Display for DevStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Display for DevRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at {}", self.type_.as_str(), self.stage)?;
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
