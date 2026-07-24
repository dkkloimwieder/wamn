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
    fn tenant_validation() {
        assert!(valid_tenant("tenant-a"));
        assert!(valid_tenant("T_1"));
        assert!(!valid_tenant(""));
        assert!(!valid_tenant("bad'tenant"));
        assert!(!valid_tenant("x".repeat(65).as_str()));
        assert!(!valid_tenant("a;b"));
    }
}
