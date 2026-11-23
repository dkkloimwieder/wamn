//! Admit a durable run and grant its first lease in one transaction.

/// The source of a durable workflow execution.
#[derive(Debug, Clone, Copy)]
pub enum RunTrigger {
    Request,
    Schedule,
    DataChange,
    RunNow,
}

impl RunTrigger {
    /// Persisted trigger vocabulary.
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Schedule => "schedule",
            Self::DataChange => "event",
            Self::RunNow => "manual",
        }
    }
}

/// Frozen workflow version, input, and execution identity.
#[derive(Debug)]
pub struct RunAdmission<'a> {
    pub application: &'a wamn_registry::ApplicationName,
    pub environment: &'a wamn_registry::EnvName,
    pub workflow_id: &'a wamn_package::canonical_name::CanonicalName,
    pub workflow_version: i32,
    pub graph_hash: &'a wamn_digest::Digest,
    pub execution_principal_id: &'a wamn_identity_protocol::PrincipalId,
    pub execution_role: &'a str,
    pub trigger: RunTrigger,
    pub invoked_by: &'a str,
    pub invoked_by_user_id: Option<&'a wamn_identity_protocol::PrincipalId>,
    pub idempotency_key: &'a str,
    pub input: &'a serde_json::Value,
    pub source_run: Option<&'a str>,
    pub root_run: Option<&'a str>,
    pub depth: Option<i32>,
}

/// Only the winning insert returns execution authority.
#[derive(Debug, PartialEq, Eq)]
pub enum RunAdmitted {
    Leased { run_id: String, generation: i64 },
    Existing { run_id: String },
    Conflict,
}
