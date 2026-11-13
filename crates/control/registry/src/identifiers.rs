//! Shared validators for registry coordinates.
//! PostgreSQL identity use bound parameters. Schema names also use these
//! validators before interpolation into DDL.
//!
//! Organization, project, and environment identifiers share this module's
//! character, length, and hyphen rules (wamn-0h0g.9.20).
//! Organization and project identifiers also reject the reserved `wamn` prefix.
//! Environment identifiers permit that prefix. Derived resource names permit
//! consecutive hyphens and use their own length limit.
//! The public project-named functions and constant retain their existing names
//! for provisioning and runtime callers. [`MAX_PROJECT_ID_LEN`] explains the
//! shared identifier budget.

/// Whether a key contains exactly three valid registry coordinates.
pub fn valid_project_env_key(key: &str) -> bool {
    let mut coordinates = key.split("--");
    let (Some(org), Some(env), Some(project)) =
        (coordinates.next(), coordinates.next(), coordinates.next())
    else {
        return false;
    };
    coordinates.next().is_none()
        && valid_project(org)
        && project_id_reason(env).is_none()
        && valid_project(project)
        && org.len() + env.len() + project.len() <= 41
}

/// Max bytes in an organization, project, or environment identifier.
///
/// The number comes from the name provisioning composes, not from this crate.
/// A per-project-env database is named
/// `wamn-db-<org>--<env>--<project>--<instance>` and must fit
/// `wamn-control-provision::MAX_DB_NAME_LEN` of 63 bytes, which is Postgres's
/// identifier limit and the DNS-1123 label limit together. The `wamn-db-`
/// prefix, the three `--` separators and the 8-byte instance suffix spend 22 of
/// those bytes, so the org, the project and the env share the remaining 41.
///
/// This cap is a per-component ceiling inside that shared budget, not the exact
/// bound. Three components at the cap still overflow 63 bytes, so provisioning
/// also tests the assembled name in `validate_project_env`. The registry holds
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_env_key_validation() {
        assert!(valid_project_env_key("org-a--dev--project"));
        assert!(!valid_project_env_key("T_1"));
        assert!(!valid_project_env_key(""));
        assert!(!valid_project_env_key("bad'project"));
        assert!(!valid_project_env_key("x".repeat(65).as_str()));
        assert!(!valid_project_env_key("a;b"));
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
