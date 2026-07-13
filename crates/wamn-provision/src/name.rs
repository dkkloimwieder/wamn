//! Identifier naming + the connection-URL composer.
//!
//! A project id is a K8s-friendly lowercase slug (`[a-z0-9-]`, start/end
//! alphanumeric) — the same shape the platform uses for flow ids (wi4). It maps
//! to a database and Secret named `wamn-db-<project>`. Hyphenated names are
//! quoted in DDL (see [`crate::sql`]) and are unreserved in a connection URL
//! path, so one slug serves both the K8s (hyphen) and Postgres (quoted) domains
//! without translation.

use crate::error::ProvisionError;

/// The single shared, cluster-global application role. Every generated tenant
/// floor and hand-written schema grants to it; provisioning grants it `CONNECT`
/// on each project database (isolation is per-database + RLS, not per-role — see
/// the crate docs).
pub const APP_ROLE: &str = "wamn_app";

/// Prefix for the per-project database **and** Secret name: `wamn-db-<project>`.
/// It is under the platform-reserved `wamn` prefix (wamn-66x) on purpose — the
/// platform mints it, and project ids in that space are rejected.
pub const DB_PREFIX: &str = "wamn-db-";

/// Max project-id length. Keeps `wamn-db-<project>` within Postgres's 63-byte
/// identifier limit (`63 - len("wamn-db-") = 55`) with comfortable margin.
pub const MAX_PROJECT_ID_LEN: usize = 40;

/// Validate a project id: a non-empty lowercase slug `[a-z0-9-]`, starting and
/// ending alphanumeric, at most [`MAX_PROJECT_ID_LEN`] bytes, and not under the
/// reserved `wamn` prefix.
///
/// Lowercase + hyphen (not underscore) is deliberate: the id is both a K8s
/// Secret-name suffix (hyphens, no underscores) and — quoted — a database name.
pub fn validate_project_id(id: &str) -> Result<(), ProvisionError> {
    let invalid = |reason| {
        Err(ProvisionError::InvalidProjectId {
            id: id.to_string(),
            reason,
        })
    };
    if id.is_empty() {
        return invalid("empty");
    }
    if id.len() > MAX_PROJECT_ID_LEN {
        return invalid("too long (max 40 bytes)");
    }
    if !id.bytes().all(is_slug_byte) {
        return invalid("only lowercase letters, digits, and hyphens are allowed");
    }
    let bytes = id.as_bytes();
    if !is_alnum(bytes[0]) || !is_alnum(bytes[bytes.len() - 1]) {
        return invalid("must start and end with a lowercase letter or digit");
    }
    // wamn-66x: the `wamn` prefix is platform-reserved. The id is already
    // lowercase; reject the bare word and any `wamn-…` id (the boundary is a
    // hyphen, so `wamning` is fine — mirrors the catalog reserved-prefix rule).
    if id == "wamn" || id.starts_with("wamn-") {
        return Err(ProvisionError::ReservedProjectId { id: id.to_string() });
    }
    Ok(())
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

fn is_slug_byte(b: u8) -> bool {
    is_alnum(b) || b == b'-'
}

/// The project's database name: `wamn-db-<project>`. Quote it in DDL (it
/// contains hyphens) via [`crate::sql`]; it is URL-path-safe as-is.
pub fn database_name(project: &str) -> String {
    format!("{DB_PREFIX}{project}")
}

/// The project's credential Secret name: `wamn-db-<project>` — the same string
/// the future `K8sSecretProvider` (5x0.1) will look up.
pub fn secret_name(project: &str) -> String {
    format!("{DB_PREFIX}{project}")
}

/// Compose a libpq connection URL. Userinfo and the database path segment are
/// percent-encoded, so a password with URL-reserved characters is carried
/// safely (tokio_postgres percent-decodes them).
pub fn compose_url(user: &str, password: &str, host: &str, port: u16, database: &str) -> String {
    format!(
        "postgres://{}:{}@{}:{}/{}",
        pct(user),
        pct(password),
        host,
        port,
        pct(database),
    )
}

/// Percent-encode everything outside the URL unreserved set `[A-Za-z0-9-._~]`.
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_project_ids_pass() {
        let max = "x".repeat(MAX_PROJECT_ID_LEN);
        for id in [
            "a",
            "acme",
            "acme-corp",
            "proj-1",
            "p9",
            "a--b",
            max.as_str(),
        ] {
            assert!(validate_project_id(id).is_ok(), "{id:?} should be valid");
        }
    }

    #[test]
    fn invalid_project_ids_are_rejected_with_a_reason() {
        // empty / too long / charset / boundary
        assert!(matches!(
            validate_project_id(""),
            Err(ProvisionError::InvalidProjectId {
                reason: "empty",
                ..
            })
        ));
        assert!(matches!(
            validate_project_id(&"x".repeat(41)),
            Err(ProvisionError::InvalidProjectId { .. })
        ));
        for bad in [
            "Acme",
            "under_score",
            "has.dot",
            "space bar",
            "sql;--",
            "tab\tx",
        ] {
            assert!(
                matches!(
                    validate_project_id(bad),
                    Err(ProvisionError::InvalidProjectId {
                        reason: "only lowercase letters, digits, and hyphens are allowed",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
        for bad in ["-lead", "trail-", "-", "9-"] {
            // "9-" ends on a hyphen; "-lead"/"-" start on one.
            assert!(
                matches!(
                    validate_project_id(bad),
                    Err(ProvisionError::InvalidProjectId {
                        reason: "must start and end with a lowercase letter or digit",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn reserved_wamn_prefix_is_rejected() {
        for bad in ["wamn", "wamn-db", "wamn-proj", "wamn-anything"] {
            assert!(
                matches!(
                    validate_project_id(bad),
                    Err(ProvisionError::ReservedProjectId { .. })
                ),
                "{bad:?} should be reserved"
            );
        }
        // The boundary is a hyphen: `wamn` + non-hyphen is a normal project.
        assert!(validate_project_id("wamning").is_ok());
        assert!(validate_project_id("wamnable").is_ok());
    }

    #[test]
    fn names_derive_from_the_project_id() {
        assert_eq!(database_name("acme-corp"), "wamn-db-acme-corp");
        assert_eq!(secret_name("acme-corp"), "wamn-db-acme-corp");
        // Database and Secret names are identical (one lookup key for 5x0.1).
        assert_eq!(database_name("p1"), secret_name("p1"));
    }

    #[test]
    fn compose_url_percent_encodes_userinfo() {
        let u = compose_url("wamn_app", "wamn_app", "wamn-pg-rw", 5432, "wamn-db-acme");
        assert_eq!(
            u,
            "postgres://wamn_app:wamn_app@wamn-pg-rw:5432/wamn-db-acme"
        );
        // A password with URL-reserved characters is encoded, not injected.
        let u = compose_url("wamn_app", "p@ss:w/rd", "h", 5432, "wamn-db-x");
        assert_eq!(u, "postgres://wamn_app:p%40ss%3Aw%2Frd@h:5432/wamn-db-x");
        // The hyphenated database name is URL-unreserved (no encoding).
        assert!(u.ends_with("/wamn-db-x"));
    }
}
