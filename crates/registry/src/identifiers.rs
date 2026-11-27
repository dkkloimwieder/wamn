//! Slug syntax shared by registry coordinate constructors.
//!
//! Organization, project, environment, and application identifiers share this
//! module's character, length, and hyphen rules.
//! Organization and project identifiers also reject the reserved `wamn` prefix.
//! Project identifiers also reject reserved root path segments. Derived resource
//! names permit consecutive hyphens and use their own length limit.

/// Max bytes in an organization, project, environment, or application identifier.
///
/// The number comes from the name provisioning composes, not from this crate.
/// A per-project-env database is named
/// `wamn-db-<org>--<env>--<project>--<instance>` and must fit
/// `crate::name::MAX_DB_NAME_LEN` of 63 bytes, which is Postgres's
/// identifier limit and the DNS-1123 label limit together. The `wamn-db-`
/// prefix, the three `--` separators and the 8-byte instance suffix spend 22 of
/// those bytes, so the org, the project and the env share the remaining 41.
///
/// This cap is a per-component ceiling inside that shared budget, not the exact
/// bound. Three components at the cap still overflow 63 bytes, so provisioning
/// also tests the assembled name in `validate_project_env`. The registry holds
/// the ceiling so an id the registry accepts is an id provisioning can shape.
pub const MAX_PROJECT_ID_LEN: usize = 40;

/// Space left in a DNS label after the separator and the longest org slug.
pub const MAX_APP_LABEL_LEN: usize = 63 - "--".len() - MAX_PROJECT_ID_LEN;

/// Explain why an application host label fails the slug or length rule.
pub fn app_label_reason(label: &str) -> Option<&'static str> {
    if label.len() > MAX_APP_LABEL_LEN {
        return Some("too long (max 21 bytes)");
    }
    component_slug_reason(label)
}

/// The platform-reserved id prefix (wamn-66x). The platform mints `wamn-db-…`
/// and `wamn-…` database, namespace and Secret names, so a project id in that
/// space collides with a platform-owned name.
const RESERVED_WAMN_PREFIX: &str = "wamn";

pub(crate) fn is_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// The shared slug shape used by coordinate constructors.
pub(crate) fn project_id_reason(id: &str) -> Option<&'static str> {
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

/// Whether `id` is `wamn` or starts with `wamn-`.
pub(crate) fn has_reserved_wamn_prefix(id: &str) -> bool {
    id == RESERVED_WAMN_PREFIX || id.starts_with("wamn-")
}

/// Whether a name is a valid API service slug outside the reserved `wamn` prefix.
pub fn api_service_name_is_valid(name: &str) -> bool {
    project_id_reason(name).is_none() && !has_reserved_wamn_prefix(name)
}

/// Whether `id` is reserved for a platform name or a whole root path segment.
pub(crate) fn project_id_is_reserved(id: &str) -> bool {
    has_reserved_wamn_prefix(id)
        || matches!(
            id,
            "api"
                | "password"
                | "oauth"
                | "assets"
                | "clients"
                | "platform-ui"
                | "healthz"
                | "invite"
                | "recover"
                | "reset"
        )
}
