//! Identifier naming + the connection-URL composer.
//!
//! A project id is a K8s-friendly lowercase slug (`[a-z0-9-]`, start/end
//! alphanumeric) — the same shape the platform uses for flow ids (wi4). It maps
//! to a database and Secret named `wamn-db-<project>`. Hyphenated names are
//! quoted in DDL (see [`crate::sql`]) and are unreserved in a connection URL
//! path, so one slug serves both the K8s (hyphen) and Postgres (quoted) domains
//! without translation.

use wamn_registry::Env;

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

/// Max length (bytes) of a provisioned database / K8s resource name: Postgres's
/// identifier limit (63) — also within the DNS-1123 **label** limit (63) that the
/// CNPG `Database` resource name must satisfy. Per-project-env names encode the
/// full triple, so the assembled length is validated (see [`validate_project_env`]).
pub const MAX_DB_NAME_LEN: usize = 63;

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

/// The per-project-env database name: `wamn-db-<org>--<project>--<env>`
/// (wamn-q3n.7).
///
/// The **org** is encoded — unlike the 2.3 [`database_name`] — because the shared
/// T3 trials pool hosts many orgs (two orgs' identically-named projects would
/// otherwise collide on one cluster), and because every cluster's CNPG `Database`
/// resources share the one K8s namespace, so the resource name must be unique
/// there. `--` separates the identity components (the `Triple::host_label`
/// convention). Validate the assembled length with [`validate_project_env`]
/// before use; quote it in DDL (it contains hyphens) via [`crate::sql`].
pub fn project_env_database_name(org: &str, project: &str, env: Env) -> String {
    format!("{DB_PREFIX}{org}--{project}--{}", env.as_str())
}

/// The per-project-env credential Secret name — identical to the database name
/// (`wamn-db-<org>--<project>--<env>`), the single lookup key the future
/// `K8sSecretProvider` (5x0.1) reads and the registry records as the project-env's
/// `SecretRef`.
pub fn project_env_secret_name(org: &str, project: &str, env: Env) -> String {
    project_env_database_name(org, project, env)
}

/// Validate that a `(org, project, env)` yields a safe provisioned database /
/// Secret name: the project id is a slug (the org id is validated by the registry
/// at org creation) and the assembled name fits [`MAX_DB_NAME_LEN`] — a legal
/// Postgres identifier and a legal DNS-1123 label for the CNPG `Database` resource.
pub fn validate_project_env(org: &str, project: &str, env: Env) -> Result<(), ProvisionError> {
    validate_project_id(project)?;
    let name = project_env_database_name(org, project, env);
    if name.len() > MAX_DB_NAME_LEN {
        return Err(ProvisionError::NameTooLong {
            name,
            max: MAX_DB_NAME_LEN,
        });
    }
    Ok(())
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
    fn project_env_names_encode_the_full_triple() {
        // The org is encoded (unlike the 2.3 project-only name) so identically
        // named projects across orgs never collide on the shared T3 pool.
        assert_eq!(
            project_env_database_name("acme", "billing", Env::Dev),
            "wamn-db-acme--billing--dev"
        );
        assert_eq!(
            project_env_database_name("acme", "billing", Env::Prod),
            "wamn-db-acme--billing--prod"
        );
        // canary and prod are distinct names (they co-reside on <org>-prod, so the
        // env MUST be in the name to keep them apart).
        assert_ne!(
            project_env_database_name("acme", "billing", Env::Canary),
            project_env_database_name("acme", "billing", Env::Prod)
        );
        // Two orgs, same project+env → different db names (the pool collision fix).
        assert_ne!(
            project_env_database_name("org-a", "demo", Env::Dev),
            project_env_database_name("org-b", "demo", Env::Dev)
        );
        // Secret name == database name (one lookup key).
        assert_eq!(
            project_env_secret_name("acme", "billing", Env::Dev),
            project_env_database_name("acme", "billing", Env::Dev)
        );
    }

    #[test]
    fn validate_project_env_slug_checks_and_bounds_the_name() {
        assert!(validate_project_env("acme", "billing", Env::Prod).is_ok());
        // A bad project id is rejected (the reason flows from validate_project_id).
        assert!(matches!(
            validate_project_env("acme", "Bad", Env::Dev),
            Err(ProvisionError::InvalidProjectId { .. })
        ));
        assert!(matches!(
            validate_project_env("acme", "wamn-x", Env::Dev),
            Err(ProvisionError::ReservedProjectId { .. })
        ));
        // The assembled name must fit the Postgres / DNS-1123 63-byte limit.
        let long_org = "o".repeat(40);
        let long_proj = "p".repeat(40);
        let err = validate_project_env(&long_org, &long_proj, Env::Canary).unwrap_err();
        assert!(matches!(err, ProvisionError::NameTooLong { max: 63, .. }));
        // A comfortably-sized triple is fine.
        assert!(validate_project_env(&"o".repeat(20), &"p".repeat(20), Env::Prod).is_ok());
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
