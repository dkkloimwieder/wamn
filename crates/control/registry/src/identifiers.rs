//! Identity-format validators — the ONE owner for the charsets the tenant /
//! project / runner / schema claims must match (R16b, wamn-2jkm.20).
//!
//! Both the `wamn:postgres` plugin (claim injection, `wamn-host`) and the flow
//! dispatcher (its pinned per-project session, `wamn-dispatcher`) import these,
//! so a config value is held to the SAME shape on both sides. They live in this
//! pure crate — not either consumer — so the dispatcher artifact never links
//! the runtime (SR9, wamn-2jkm.22). The pre-R16b divergence was a dispatch-local
//! `valid_tenant` with NO length bound while the plugin's bounded at 64 — a
//! 65-char tenant that the plugin rejected the dispatcher would have accepted.
//!
//! Since R2 these are no longer the injection boundary on the PLUGIN path (claim
//! values bind as parameters there); they define what a *legal* id is and fail
//! closed on a malformed one. The dispatcher still interpolates its pinned
//! session `SET`s, so on that path they remain the boundary — one more reason the
//! two sides must share exactly one rule. The `valid_schema` no-hyphen rule also
//! still matters where a schema name is quoted into DDL elsewhere.

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;

/// A validated opaque execution-placement token.
///
/// The token is exactly one NATS subject segment: 1–64 ASCII characters from
/// `[A-Za-z0-9_-]`. Tenant identity and execution placement are deliberately
/// different types even though the MVP placement adapter initially gives them
/// equal values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExecutionTargetId(String);

/// A value cannot be used as an [`ExecutionTargetId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidExecutionTargetId;

impl fmt::Display for InvalidExecutionTargetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("execution target id must be 1-64 ASCII characters from [A-Za-z0-9_-]")
    }
}

impl std::error::Error for InvalidExecutionTargetId {}

impl ExecutionTargetId {
    /// Validate an execution-placement token.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidExecutionTargetId> {
        let value = value.into();
        if !valid_tenant(&value) {
            return Err(InvalidExecutionTargetId);
        }
        Ok(Self(value))
    }

    /// Return the validated subject-token value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExecutionTargetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for ExecutionTargetId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for ExecutionTargetId {
    type Err = InvalidExecutionTargetId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ExecutionTargetId {
    type Error = InvalidExecutionTargetId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for ExecutionTargetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// The sole MVP placement adapter: initially map a tenant key to its equal,
/// independently validated execution target.
pub fn mvp_execution_target_id(
    tenant: &str,
) -> Result<ExecutionTargetId, InvalidExecutionTargetId> {
    ExecutionTargetId::new(tenant)
}

/// Format the one shared doorbell subject for an execution target.
pub fn doorbell_subject(execution_target_id: &ExecutionTargetId) -> String {
    format!("wamn.doorbell.{execution_target_id}")
}

/// A tenant claim: 1–64 chars of `[A-Za-z0-9_-]`.
pub fn valid_tenant(tenant: &str) -> bool {
    !tenant.is_empty()
        && tenant.len() <= 64
        && tenant
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A project id. Used only as a map key and provider lookup (never embedded in
/// SQL), so the charset just needs to be a sane, bounded identifier.
pub fn valid_project(project: &str) -> bool {
    !project.is_empty()
        && project.len() <= 64
        && project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A durable-queue lease owner. An identity-format contract: bounded
/// `[A-Za-z0-9_-]`, no quotes/backslashes. Since R2 this is NO LONGER the
/// injection boundary on the plugin path — the runner binds as a parameter into
/// `CLAIM_SQL`, so a quote/backslash is inert data — but a malformed owner still
/// fails closed.
pub fn valid_runner(runner: &str) -> bool {
    !runner.is_empty()
        && runner.len() <= 128
        && runner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A `search_path` schema name. Stricter than a tenant: no hyphens. Since R2 the
/// value binds as a parameter into `CLAIM_SQL` on the plugin path rather than
/// being spliced into SQL — but the no-hyphen rule still matters where a schema
/// name is quoted into DDL elsewhere (e.g. the migrate / copy paths), and a
/// malformed schema still fails closed.
pub fn valid_schema(schema: &str) -> bool {
    !schema.is_empty()
        && schema.len() <= 63
        && schema
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && schema
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_target_id_guards_the_subject_token() {
        let max_length = "x".repeat(64);
        for valid in ["a", "tenant-a", "T_1", max_length.as_str()] {
            assert!(
                ExecutionTargetId::new(valid).is_ok(),
                "valid target {valid:?}"
            );
        }
        let over_length = "x".repeat(65);
        for invalid in [
            "",
            "tenant.a",
            "tenant*",
            "tenant>",
            "tenant a",
            " tenant",
            "tenant\n",
            over_length.as_str(),
        ] {
            assert!(
                ExecutionTargetId::new(invalid).is_err(),
                "invalid target {invalid:?}"
            );
        }
    }

    #[test]
    fn mvp_adapter_and_subject_formatter_are_explicit() {
        let target = mvp_execution_target_id("tenant-a").expect("tenant-safe target");
        assert_eq!(target.as_str(), "tenant-a");
        assert_eq!(doorbell_subject(&target), "wamn.doorbell.tenant-a");
    }

    #[test]
    fn execution_target_deserialization_enforces_the_type_invariant() {
        assert_eq!(
            serde_json::from_str::<ExecutionTargetId>(r#""target-a""#)
                .expect("valid JSON target")
                .as_str(),
            "target-a"
        );
        assert!(serde_json::from_str::<ExecutionTargetId>(r#""bad.target""#).is_err());
    }

    #[test]
    fn tenant_validation() {
        assert!(valid_tenant("tenant-a"));
        assert!(valid_tenant("T_1"));
        assert!(!valid_tenant(""));
        assert!(!valid_tenant("bad'tenant"));
        assert!(!valid_tenant("x".repeat(65).as_str()));
        assert!(!valid_tenant("a;b"));
    }
}
