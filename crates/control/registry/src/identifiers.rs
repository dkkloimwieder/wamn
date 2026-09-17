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
//!
//! Organization, project, and environment identifiers share this module's
//! character, length, and hyphen rules (wamn-0h0g.9.20).
//! Organization and project identifiers also reject the reserved `wamn` prefix.
//! Environment identifiers permit that prefix. Derived resource names permit
//! consecutive hyphens and use their own length limit.
//! The public project-named functions and constant retain their existing names
//! for provisioning and runtime callers. [`MAX_PROJECT_ID_LEN`] explains the
//! shared identifier budget.

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

/// Max bytes in an organization, project, or environment identifier.
///
/// The number comes from the name provisioning composes, not from this crate.
/// A per-project-env database is named
/// `wamn-db-<org>--<project>--<env>--<instance>` and must fit
/// `wamn-control-provision::MAX_DB_NAME_LEN` of 63 bytes, which is Postgres's
/// identifier limit and the DNS-1123 label limit together. The `wamn-db-`
/// prefix, the three `--` separators and the 8-byte instance suffix spend 22 of
/// those bytes, so the org, the project and the env share the remaining 41.
///
/// This cap is a per-component ceiling inside that shared budget, not the exact
/// bound. Three components at the cap still overflow 63 bytes, so provisioning
/// also checks the assembled name in `validate_project_env`. The registry holds
/// the ceiling so an id the registry accepts is an id provisioning can shape.
pub const MAX_PROJECT_ID_LEN: usize = 40;

/// The platform-reserved id prefix (wamn-66x). The platform mints `wamn-db-…`
/// and `wamn-…` database, namespace and Secret names, so a project id in that
/// space collides with a platform-owned name.
const RESERVED_PROJECT_PREFIX: &str = "wamn";

pub(crate) fn is_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// Why an organization, project, or environment identifier has an invalid shape.
///
/// This owns the shared identifier shape (wamn-0h0g.9.20). It lives here
/// rather than in `wamn-control-provision` because the runtime reads it through
/// [`valid_project`] and provisioning reads it through
/// `wamn-control-provision::validate_project_id`. A shared rule means that an
/// id the registry accepts is always an id provisioning can provision.
///
/// The rule: a non-empty lowercase slug over `[a-z0-9-]`, at most
/// [`MAX_PROJECT_ID_LEN`] bytes, starting and ending on a letter or a digit,
/// with no run of consecutive hyphens.
///
/// Lowercase and hyphen (never underscore) is deliberate. The id is a Kubernetes
/// name component, which admits no underscore. Quoted, the same id is a Postgres
/// database name. One slug therefore serves both domains with no translation.
///
/// The consecutive-hyphen ban (wamn-R27) closes an identity collision. `--`
/// separates the components of the derived database name and, mapped to `__`,
/// of the derived CDC object name. A `--` run inside a component lets two
/// distinct triples, such as `(a, x--p, dev)` and `(a--x, p, dev)`, derive the
/// same names. The charset admits no underscore, so a `-` to `_` mapping only
/// ever yields an isolated `_` and the `__` separator stays unambiguous.
///
/// The reserved `wamn` prefix is a separate question: see
/// [`project_id_is_reserved`]. Callers that must tell the two apart, such as
/// provisioning, ask both. Callers that only need a yes or a no ask
/// [`valid_project`]. The reason is a `&'static str` so a caller can put it
/// straight into its own error type without a second error taxonomy.
pub fn project_id_reason(id: &str) -> Option<&'static str> {
    if id.is_empty() {
        return Some("empty");
    }
    if id.len() > MAX_PROJECT_ID_LEN {
        return Some("too long (max 40 bytes)");
    }
    component_slug_reason(id)
}

/// Identifier shape without the length cap, for registry reference diagnostics.
pub(crate) fn component_slug_reason(id: &str) -> Option<&'static str> {
    if let Some(reason) = slug_reason(id) {
        return Some(reason);
    }
    if id.contains("--") {
        return Some("must not contain consecutive hyphens");
    }
    None
}

/// A derived resource name permits consecutive hyphens and has its own length cap.
pub(crate) fn is_slug(id: &str) -> bool {
    slug_reason(id).is_none()
}

fn slug_reason(id: &str) -> Option<&'static str> {
    if id.is_empty() {
        return Some("empty");
    }
    if !id.bytes().all(|byte| is_alnum(byte) || byte == b'-') {
        return Some("only lowercase letters, digits, and hyphens are allowed");
    }
    let bytes = id.as_bytes();
    if !is_alnum(bytes[0]) || !is_alnum(bytes[bytes.len() - 1]) {
        return Some("must start and end with a lowercase letter or digit");
    }
    None
}

/// Whether `id` sits under the platform-reserved `wamn` prefix (wamn-66x): the
/// bare word `wamn`, or any id starting `wamn-`. The boundary is a hyphen, so
/// `wamning` is an ordinary project id. This mirrors the catalog rule.
pub fn project_id_is_reserved(id: &str) -> bool {
    id == RESERVED_PROJECT_PREFIX || id.starts_with("wamn-")
}

/// A project id: well-formed by [`project_id_reason`] and not reserved by
/// [`project_id_is_reserved`].
///
/// The runtime calls this to fail closed on a malformed project before it looks
/// up a credential. It is the same rule provisioning enforces, so the runtime
/// never admits a project that provisioning refuses to build.
pub fn valid_project(project: &str) -> bool {
    project_id_reason(project).is_none() && !project_id_is_reserved(project)
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

    #[test]
    fn project_validation_is_the_strict_provisioned_name_rule() {
        let at_cap = "x".repeat(MAX_PROJECT_ID_LEN);
        for good in ["a", "p9", "acme", "acme-corp", "a-b-c", at_cap.as_str()] {
            assert!(valid_project(good), "valid project {good:?}");
            assert_eq!(project_id_reason(good), None, "reason for {good:?}");
        }
        let over_cap = "x".repeat(MAX_PROJECT_ID_LEN + 1);
        for bad in [
            "",
            "Acme",
            "under_score",
            "has.dot",
            "space bar",
            "-lead",
            "trail-",
            "a--b",
            over_cap.as_str(),
            "wamn",
            "wamn-db",
        ] {
            assert!(!valid_project(bad), "invalid project {bad:?}");
        }
    }

    #[test]
    fn project_rejection_reasons_are_stable() {
        // Provisioning copies these strings into ProvisionError::InvalidProjectId,
        // so they are a contract, not a message.
        assert_eq!(project_id_reason(""), Some("empty"));
        assert_eq!(
            project_id_reason(&"x".repeat(MAX_PROJECT_ID_LEN + 1)),
            Some("too long (max 40 bytes)")
        );
        assert_eq!(
            project_id_reason("Acme"),
            Some("only lowercase letters, digits, and hyphens are allowed")
        );
        assert_eq!(
            project_id_reason("under_score"),
            Some("only lowercase letters, digits, and hyphens are allowed")
        );
        assert_eq!(
            project_id_reason("-lead"),
            Some("must start and end with a lowercase letter or digit")
        );
        assert_eq!(
            project_id_reason("a--b"),
            Some("must not contain consecutive hyphens")
        );
    }

    #[test]
    fn the_reserved_prefix_stops_at_a_hyphen() {
        for reserved in ["wamn", "wamn-db", "wamn-anything"] {
            assert!(project_id_is_reserved(reserved), "reserved {reserved:?}");
            // The shape is fine. Only the prefix rule refuses these.
            assert_eq!(project_id_reason(reserved), None, "shape of {reserved:?}");
        }
        for ordinary in ["wamning", "wamnable", "wam", "awamn"] {
            assert!(!project_id_is_reserved(ordinary), "ordinary {ordinary:?}");
            assert!(valid_project(ordinary), "valid project {ordinary:?}");
        }
    }
}
