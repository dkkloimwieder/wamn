//! Durable effect-attempt recovery decisions.

use serde::{Deserialize, Serialize};

/// The replay contract persisted before a node dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryClass {
    Replay,
    IdempotentWithKey,
    NeverReplay,
}

/// How the exact environment generations justifying an admitted recovery class
/// are represented in the attempt ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GenerationFactKind {
    /// This occurrence has no portable connection requirement, so no
    /// connection or credential generation may be recorded.
    NotRequired,
    /// An environment attestation identified exact immutable generations.
    Attested,
}

impl GenerationFactKind {
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::NotRequired => "not-required",
            Self::Attested => "attested",
        }
    }

    pub fn from_sql(value: &str) -> Option<Self> {
        match value {
            "not-required" => Some(Self::NotRequired),
            "attested" => Some(Self::Attested),
            _ => None,
        }
    }
}

impl RecoveryClass {
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::IdempotentWithKey => "idempotent-with-key",
            Self::NeverReplay => "never-replay",
        }
    }

    pub fn from_sql(value: &str) -> Option<Self> {
        match value {
            "replay" => Some(Self::Replay),
            "idempotent-with-key" => Some(Self::IdempotentWithKey),
            "never-replay" => Some(Self::NeverReplay),
            _ => None,
        }
    }
}

/// The only actions a worker may take after the intent transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptStartResult {
    Started,
    Redispatch,
    AlreadyCompleted,
    EffectUncertain,
    MissingAttemptKey,
    ConnectionRefused,
    AttemptNotStarted,
    RunTerminal,
    FenceLost,
    CrossRunAuthority,
    NotFound,
}

/// Result of the fenced transition immediately before external dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptDispatchResult {
    Marked,
    AlreadyDispatched,
    AttemptNotFound,
    AttemptNotStarted,
    AttemptDeadlineExpired,
    RunDeadlineExpired,
    RunTerminal,
    FenceLost,
    CrossRunAuthority,
    NotFound,
}

impl AttemptDispatchResult {
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "marked" => Some(Self::Marked),
            "already-dispatched" => Some(Self::AlreadyDispatched),
            "attempt-not-found" => Some(Self::AttemptNotFound),
            "attempt-not-started" => Some(Self::AttemptNotStarted),
            "attempt-deadline-expired" => Some(Self::AttemptDeadlineExpired),
            "run-deadline-expired" => Some(Self::RunDeadlineExpired),
            "run-terminal" => Some(Self::RunTerminal),
            "fence-lost" => Some(Self::FenceLost),
            "cross-run-authority" => Some(Self::CrossRunAuthority),
            "not-found" => Some(Self::NotFound),
            _ => None,
        }
    }

    pub const fn permits_dispatch(self) -> bool {
        matches!(self, Self::Marked)
    }
}

impl AttemptStartResult {
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "started" => Some(Self::Started),
            "redispatch" => Some(Self::Redispatch),
            "already-completed" => Some(Self::AlreadyCompleted),
            "effect-uncertain" => Some(Self::EffectUncertain),
            "missing-attempt-key" => Some(Self::MissingAttemptKey),
            "connection-refused" => Some(Self::ConnectionRefused),
            "attempt-not-started" => Some(Self::AttemptNotStarted),
            "run-terminal" => Some(Self::RunTerminal),
            "fence-lost" => Some(Self::FenceLost),
            "cross-run-authority" => Some(Self::CrossRunAuthority),
            "not-found" => Some(Self::NotFound),
            _ => None,
        }
    }

    /// `FenceLost` is absolute: the caller may not touch the store again.
    pub const fn permits_access(self) -> bool {
        !matches!(self, Self::FenceLost)
    }

    pub const fn permits_dispatch(self) -> bool {
        matches!(self, Self::Started | Self::Redispatch)
    }
}

#[cfg(test)]
mod tests {
    use super::{AttemptDispatchResult, AttemptStartResult, GenerationFactKind, RecoveryClass};

    #[test]
    fn recovery_class_sql_round_trips() {
        for class in [
            RecoveryClass::Replay,
            RecoveryClass::IdempotentWithKey,
            RecoveryClass::NeverReplay,
        ] {
            assert_eq!(RecoveryClass::from_sql(class.as_sql()), Some(class));
        }
    }

    #[test]
    fn generation_fact_kind_sql_round_trips() {
        for kind in [
            GenerationFactKind::NotRequired,
            GenerationFactKind::Attested,
        ] {
            assert_eq!(GenerationFactKind::from_sql(kind.as_sql()), Some(kind));
        }
    }

    #[test]
    fn only_new_or_authorized_replay_dispatches() {
        assert!(AttemptStartResult::Started.permits_dispatch());
        assert!(AttemptStartResult::Redispatch.permits_dispatch());
        assert!(!AttemptStartResult::EffectUncertain.permits_dispatch());
        assert!(!AttemptStartResult::MissingAttemptKey.permits_dispatch());
        assert!(!AttemptStartResult::AlreadyCompleted.permits_dispatch());
        assert!(!AttemptStartResult::FenceLost.permits_access());
        assert!(AttemptDispatchResult::Marked.permits_dispatch());
        assert!(!AttemptDispatchResult::AlreadyDispatched.permits_dispatch());
    }
}
