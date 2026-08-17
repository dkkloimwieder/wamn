//! Scoped control-database author credential contract (wamn-0h0g.8.18).
//!
//! The management service stopped treating a project database as the durable
//! owner of drafts, authoring commands, reservations, case runs, and finalized
//! reports. Its one authoring/report connection now names the **control**
//! database, and the identity on that connection is a scoped A/B LOGIN
//! generation of the stable NOLOGIN [`CONTROL_AUTHOR_ROLE`] — the same
//! generation model the effect writer already runs under (wamn-0h0g.13.31 /
//! .13.34), with its own domain separator so the two planes can never share a
//! name.
//!
//! `wamn_scenario_author` is deliberately absent from this module: it is the
//! project plane's author role, it is never granted control-database `CONNECT`,
//! and it is never reused here.
//!
//! Everything in this module is pure. [`parse_control_authoring_url`] is what
//! lets a management process refuse an absent or out-of-scope connection input
//! **before it opens a socket**: one management instance serves exactly one
//! `(org, project, environment)`, so the role name it must present is fully
//! determined by that scope plus the database the URL names.

use std::fmt;

use sha2::{Digest as _, Sha256};
use url::Url;

use wamn_run_state::CredentialGeneration;

/// Stable NOLOGIN ACL role carrying control-database authoring authority.
///
/// ctl creates and hardens it (`crate::sql::ensure_control_author_acl_role_sql`);
/// `deploy/sql/control-portable-store.sql` grants it the exact authority class.
/// It never logs in — the A/B generations below do.
pub const CONTROL_AUTHOR_ROLE: &str = "wamn_control_author";

/// Domain separator for the control-author scope digest.
///
/// Distinct from the effect writer's `wamn.effect-writer.scope.v0.1`, so the
/// same `(org, project, environment, database)` can never derive one plane's
/// role name from the other's.
const CONTROL_AUTHOR_SCOPE_DOMAIN: &[u8] = b"wamn.control-author.scope.v0.1";

/// 160 bits of the scope digest, hex-encoded — the same width the effect-writer
/// generations use, which keeps `wamn_control_author_<40>_<a|b>` at 62 bytes and
/// therefore inside PostgreSQL's 63-byte identifier limit.
const CONTROL_AUTHOR_SCOPE_HASH_HEX_LEN: usize = 40;

/// Length-prefix one preimage field so no two distinct scopes can frame to the
/// same bytes (`org = "a-b"`, `project = "c"` must not collide with
/// `org = "a"`, `project = "b-c"`).
fn frame(preimage: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("a framed scope field fits u64");
    preimage.extend_from_slice(&length.to_be_bytes());
    preimage.extend_from_slice(value);
}

/// Deterministic 160-bit suffix for one management scope.
pub fn control_author_scope_hash(
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
) -> String {
    let mut preimage = Vec::new();
    frame(&mut preimage, CONTROL_AUTHOR_SCOPE_DOMAIN);
    for (tag, value) in [
        ("org", org),
        ("project", project),
        ("environment", environment),
        ("database", database),
    ] {
        frame(&mut preimage, tag.as_bytes());
        frame(&mut preimage, value.as_bytes());
    }
    let digest = hex::encode(Sha256::digest(preimage));
    digest[..CONTROL_AUTHOR_SCOPE_HASH_HEX_LEN].to_string()
}

/// Scoped PostgreSQL LOGIN role for one control-author generation slot.
pub fn control_author_generation_role(
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
    generation: CredentialGeneration,
) -> String {
    format!(
        "{CONTROL_AUTHOR_ROLE}_{}_{}",
        control_author_scope_hash(org, project, environment, database),
        generation.as_str()
    )
}

/// Which predicate refused a control authoring connection input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAuthoringUrlErrorKind {
    /// No connection input was supplied at all.
    Absent,
    /// The input is not a parseable URL, or names no host.
    Malformed,
    /// The scheme is not `postgres`/`postgresql`.
    Scheme,
    /// The path does not name exactly one database.
    Database,
    /// The user is not this scope's A or B control-author generation.
    Role,
    /// The input carries a query or fragment, which a connection input must not.
    Extra,
}

impl ControlAuthoringUrlErrorKind {
    /// Stable label for logs and tests.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Malformed => "malformed",
            Self::Scheme => "scheme",
            Self::Database => "database",
            Self::Role => "role",
            Self::Extra => "extra",
        }
    }
}

/// A control authoring connection input that failed closed before any I/O.
///
/// It carries the refused predicate and a fixed reason, never the input: the
/// input holds a password, so neither `Debug` nor `Display` may echo it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlAuthoringUrlError {
    kind: ControlAuthoringUrlErrorKind,
    reason: &'static str,
}

impl ControlAuthoringUrlError {
    /// Which predicate refused the input.
    pub const fn kind(&self) -> ControlAuthoringUrlErrorKind {
        self.kind
    }

    /// The fixed reason this predicate refuses for.
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for ControlAuthoringUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "WAMN_CONTROL_AUTHORING_PG_URL is out of scope ({}): {}",
            self.kind.as_str(),
            self.reason
        )
    }
}

impl std::error::Error for ControlAuthoringUrlError {}

fn refuse(kind: ControlAuthoringUrlErrorKind, reason: &'static str) -> ControlAuthoringUrlError {
    ControlAuthoringUrlError { kind, reason }
}

/// The scoped control-database author connection one management process holds.
///
/// Deliberately does not retain the URL, so its derived `Debug` cannot leak the
/// password the URL carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlAuthoringConnection {
    database: String,
    role: String,
    generation: CredentialGeneration,
}

impl ControlAuthoringConnection {
    /// The one control database this connection addresses.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// The exact scoped generation role the input authenticates as.
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Which of the two reusable slots is in use.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }
}

/// Refuse an absent or out-of-scope control authoring connection input, purely.
///
/// This is the fail-closed gate the management surface runs **before any
/// network, database, or filesystem I/O**. It proves everything a pure function
/// can prove about scope:
///
/// * the input exists, parses, names a host, and carries no query or fragment;
/// * its path names exactly one database;
/// * its user is one of the two control-author generation roles derived from the
///   fixed `(org, project, environment)` this process serves *and* the database
///   the input itself names.
///
/// The last predicate is what makes a project-database URL fail here rather
/// than at connect time: the database name is inside the scope digest, so a URL
/// pointing at `wamn-db-acme--receiving--dev` cannot present a role named for
/// the control database. What a pure check cannot prove is that the named
/// database *is* the control database — no offline input distinguishes two
/// reachable databases — and that half is enforced by the ACL: only the control
/// database grants this role `CONNECT`, and only the control database carries
/// the login-to-tenant mapping every author-accessed relation's restrictive
/// policy consults.
pub fn parse_control_authoring_url(
    url: &str,
    org: &str,
    project: &str,
    environment: &str,
) -> Result<ControlAuthoringConnection, ControlAuthoringUrlError> {
    if url.is_empty() {
        return Err(refuse(
            ControlAuthoringUrlErrorKind::Absent,
            "the control authoring connection input is required and has no fallback",
        ));
    }
    let parsed = Url::parse(url).map_err(|_| {
        refuse(
            ControlAuthoringUrlErrorKind::Malformed,
            "the control authoring connection input is not a URL",
        )
    })?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") {
        return Err(refuse(
            ControlAuthoringUrlErrorKind::Scheme,
            "the control authoring connection input must be a postgres URL",
        ));
    }
    // `postgres` is not a "special" scheme, so `url` accepts an empty authority
    // (`postgres:///db`) and reports `Some("")` rather than `None`.
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(refuse(
            ControlAuthoringUrlErrorKind::Malformed,
            "the control authoring connection input names no host",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(refuse(
            ControlAuthoringUrlErrorKind::Extra,
            "the control authoring connection input must carry no query or fragment",
        ));
    }
    let database = parsed.path().strip_prefix('/').unwrap_or(parsed.path());
    if database.is_empty() || database.contains('/') {
        return Err(refuse(
            ControlAuthoringUrlErrorKind::Database,
            "the control authoring connection input must name exactly one database",
        ));
    }
    let presented = parsed.username();
    let scoped = [CredentialGeneration::A, CredentialGeneration::B].map(|generation| {
        (
            generation,
            control_author_generation_role(org, project, environment, database, generation),
        )
    });
    let Some((generation, role)) = scoped
        .into_iter()
        .find(|(_, role)| role.as_str() == presented)
    else {
        return Err(refuse(
            ControlAuthoringUrlErrorKind::Role,
            "the control authoring connection input does not authenticate as this \
             org/project/environment's control-author generation",
        ));
    };
    Ok(ControlAuthoringConnection {
        database: database.to_owned(),
        role,
        generation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORG: &str = "acme";
    const PROJECT: &str = "receiving";
    const ENVIRONMENT: &str = "dev";
    const DATABASE: &str = "wamn-system";

    fn role(generation: CredentialGeneration) -> String {
        control_author_generation_role(ORG, PROJECT, ENVIRONMENT, DATABASE, generation)
    }

    fn url(user: &str, database: &str) -> String {
        format!("postgres://{user}:secret@control.invalid:5432/{database}")
    }

    #[test]
    fn generation_roles_fit_postgres_and_never_collide_with_the_effect_writer() {
        let a = role(CredentialGeneration::A);
        let b = role(CredentialGeneration::B);
        assert_ne!(a, b);
        assert!(a.starts_with("wamn_control_author_"));
        assert!(a.ends_with("_a") && b.ends_with("_b"));
        // 19 + 1 + 40 + 1 + 1: inside PostgreSQL's 63-byte identifier limit.
        assert_eq!(a.len(), 62);
        assert_eq!(b.len(), 62);
        let digest = &a["wamn_control_author_".len()..a.len() - 2];
        assert_eq!(digest.len(), CONTROL_AUTHOR_SCOPE_HASH_HEX_LEN);
        assert!(digest.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()));

        // The two planes must never derive one name from the other's scope. Only
        // the domain separator distinguishes the preimages, so this is the
        // assertion that fails if the separator is dropped or copied.
        assert_ne!(
            control_author_scope_hash(ORG, PROJECT, ENVIRONMENT, DATABASE),
            wamn_run_state::effect_writer_scope_hash(ORG, PROJECT, ENVIRONMENT, DATABASE)
        );
        assert!(!a.contains("effect_writer"));
        assert!(!a.contains("scenario_author"));
    }

    #[test]
    fn scope_framing_separates_ambiguous_component_boundaries() {
        // Without length-prefix framing these two scopes share a preimage.
        assert_ne!(
            control_author_scope_hash("a-b", "c", ENVIRONMENT, DATABASE),
            control_author_scope_hash("a", "b-c", ENVIRONMENT, DATABASE)
        );
        // Every component participates.
        let base = control_author_scope_hash(ORG, PROJECT, ENVIRONMENT, DATABASE);
        assert_ne!(base, control_author_scope_hash("other", PROJECT, ENVIRONMENT, DATABASE));
        assert_ne!(base, control_author_scope_hash(ORG, "other", ENVIRONMENT, DATABASE));
        assert_ne!(base, control_author_scope_hash(ORG, PROJECT, "prod", DATABASE));
        assert_ne!(base, control_author_scope_hash(ORG, PROJECT, ENVIRONMENT, "other-db"));
    }

    #[test]
    fn an_in_scope_generation_url_resolves_its_slot_and_database() {
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            let connection = parse_control_authoring_url(
                &url(&role(generation), DATABASE),
                ORG,
                PROJECT,
                ENVIRONMENT,
            )
            .expect("an in-scope generation URL is admitted");
            assert_eq!(connection.generation(), generation);
            assert_eq!(connection.database(), DATABASE);
            assert_eq!(connection.role(), role(generation));
            // The password never reaches the accepted value, so no log or panic
            // formatting it can leak the credential.
            assert!(!format!("{connection:?}").contains("secret"));
        }
    }

    #[test]
    fn every_out_of_scope_connection_input_fails_closed_by_predicate() {
        let admitted = role(CredentialGeneration::A);
        for (input, expected) in [
            ("", ControlAuthoringUrlErrorKind::Absent),
            ("not a url", ControlAuthoringUrlErrorKind::Malformed),
            ("postgres:///only-a-path", ControlAuthoringUrlErrorKind::Malformed),
            (
                "mysql://wamn_control_author_x_a:secret@control.invalid/wamn-system",
                ControlAuthoringUrlErrorKind::Scheme,
            ),
        ] {
            let error = parse_control_authoring_url(input, ORG, PROJECT, ENVIRONMENT)
                .expect_err("out-of-scope input must refuse");
            assert_eq!(error.kind(), expected, "{input:?}");
        }

        // A path that names no database, or more than one.
        for path in ["", "one/two"] {
            let error =
                parse_control_authoring_url(&url(&admitted, path), ORG, PROJECT, ENVIRONMENT)
                    .expect_err("a non-database path must refuse");
            assert_eq!(error.kind(), ControlAuthoringUrlErrorKind::Database, "{path:?}");
        }

        // A query or fragment on a connection input.
        for suffix in ["?sslmode=disable", "#fragment"] {
            let error = parse_control_authoring_url(
                &format!("{}{suffix}", url(&admitted, DATABASE)),
                ORG,
                PROJECT,
                ENVIRONMENT,
            )
            .expect_err("a decorated connection input must refuse");
            assert_eq!(error.kind(), ControlAuthoringUrlErrorKind::Extra, "{suffix:?}");
        }

        // The identity half: a plain role, the project plane's author role, the
        // stable NOLOGIN role itself, and a generation minted for another scope.
        let foreign = control_author_generation_role(
            ORG,
            PROJECT,
            "prod",
            DATABASE,
            CredentialGeneration::A,
        );
        for user in [
            "wamn_app",
            "wamn_scenario_author",
            CONTROL_AUTHOR_ROLE,
            foreign.as_str(),
        ] {
            let error = parse_control_authoring_url(
                &url(user, DATABASE),
                ORG,
                PROJECT,
                ENVIRONMENT,
            )
            .expect_err("an out-of-scope identity must refuse");
            assert_eq!(error.kind(), ControlAuthoringUrlErrorKind::Role, "{user}");
        }

        // The project database with this scope's control role: the database name
        // is inside the digest, so the role no longer matches.
        let error = parse_control_authoring_url(
            &url(&admitted, "wamn-db-acme--receiving--dev"),
            ORG,
            PROJECT,
            ENVIRONMENT,
        )
        .expect_err("a project-database URL must refuse");
        assert_eq!(error.kind(), ControlAuthoringUrlErrorKind::Role);
    }

    #[test]
    fn a_refusal_never_echoes_the_connection_input() {
        let error = parse_control_authoring_url(
            &url("wamn_app", DATABASE),
            ORG,
            PROJECT,
            ENVIRONMENT,
        )
        .expect_err("an out-of-scope identity must refuse");
        for rendered in [format!("{error}"), format!("{error:?}")] {
            assert!(!rendered.contains("secret"), "{rendered}");
            assert!(!rendered.contains("control.invalid"), "{rendered}");
        }
        assert!(format!("{error}").contains("WAMN_CONTROL_AUTHORING_PG_URL"));
    }
}
