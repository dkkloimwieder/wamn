//! Scoped project-database management-admission credential contract
//! (`wamn-0h0g.8.5.3`).
//!
//! The management surface holds two connections that must never be confused.
//! [`crate::control_author`] names the **control** database: drafts, authoring
//! commands, reservations, case runs, and finalized reports. This module names
//! the **project-environment** database, and only for admitting a management run
//! into that project's own run state.
//!
//! The two are separate credentials on separate databases by construction: a
//! transaction cannot span them, and neither is ever a fallback for the other.
//! The identity on this connection is a scoped A/B `LOGIN` generation of the
//! stable NOLOGIN [`crate::MANAGEMENT_ADMITTER_ROLE`], derived through the
//! generic [`workload_generation_role`] under
//! [`WorkloadRoleFamily::ManagementAdmitter`] — the seventh frozen family
//! (`wamn-0h0g.13.61`), whose generation prefix is deliberately shorter than its
//! ACL role name so the derived login still fits PostgreSQL's 63-byte
//! identifier cap (`wamn-0h0g.13.62`).
//!
//! Everything in this module is pure. [`parse_management_admission_url`] is what
//! lets a management process refuse an absent or out-of-scope admission
//! connection input **before it opens a socket**: one management instance serves
//! exactly one `(org, project, environment)`, so the role name it must present is
//! fully determined by that scope plus the database the URL names.
//!
//! `wamn_app` is deliberately absent from this module. It is the shared,
//! cluster-global query role every project database grants; presenting it here
//! would carry authority far past admission. Moving the production management
//! path off `wamn_app` and onto this family is `wamn-0h0g.22.10`'s traffic
//! change, which this contract exists to make possible.

use std::fmt;

use url::Url;

use wamn_run_state::CredentialGeneration;

use crate::workload_role::{
    WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role, workload_role_scope_hash,
};

/// Deterministic 160-bit suffix for one management-admission scope.
pub fn management_admitter_scope_hash(
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
) -> String {
    workload_role_scope_hash(
        WorkloadRoleFamily::ManagementAdmitter,
        WorkloadRoleScope::ProjectEnvironment {
            org,
            project,
            environment,
            database,
        },
    )
    .expect("management-admitter always uses project-environment scope")
}

/// Scoped PostgreSQL LOGIN role for one management-admitter generation slot.
pub fn management_admitter_generation_role(
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
    generation: CredentialGeneration,
) -> String {
    workload_generation_role(
        WorkloadRoleFamily::ManagementAdmitter,
        WorkloadRoleScope::ProjectEnvironment {
            org,
            project,
            environment,
            database,
        },
        generation,
    )
    .expect("management-admitter always uses project-environment scope")
}

/// Which predicate refused a management admission connection input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementAdmissionUrlErrorKind {
    /// No connection input was supplied at all.
    Absent,
    /// The input is not a parseable URL, or names no host.
    Malformed,
    /// The scheme is not `postgres`/`postgresql`.
    Scheme,
    /// The path does not name exactly one database.
    Database,
    /// The user is not this scope's A or B management-admitter generation.
    Role,
    /// The input carries a query or fragment, which a connection input must not.
    Extra,
}

impl ManagementAdmissionUrlErrorKind {
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

/// A management admission connection input that failed closed before any I/O.
///
/// It carries the refused predicate and a fixed reason, never the input: the
/// input holds a password, so neither `Debug` nor `Display` may echo it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagementAdmissionUrlError {
    kind: ManagementAdmissionUrlErrorKind,
    reason: &'static str,
}

impl ManagementAdmissionUrlError {
    /// Which predicate refused the input.
    pub const fn kind(&self) -> ManagementAdmissionUrlErrorKind {
        self.kind
    }

    /// The fixed reason this predicate refuses for.
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for ManagementAdmissionUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "WAMN_MANAGEMENT_ADMISSION_PG_URL is out of scope ({}): {}",
            self.kind.as_str(),
            self.reason
        )
    }
}

impl std::error::Error for ManagementAdmissionUrlError {}

fn refuse(
    kind: ManagementAdmissionUrlErrorKind,
    reason: &'static str,
) -> ManagementAdmissionUrlError {
    ManagementAdmissionUrlError { kind, reason }
}

/// The scoped project-database admission connection one management process holds.
///
/// Deliberately does not retain the URL, so its derived `Debug` cannot leak the
/// password the URL carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagementAdmissionConnection {
    database: String,
    role: String,
    generation: CredentialGeneration,
}

impl ManagementAdmissionConnection {
    /// The one project-environment database this connection addresses.
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

/// Refuse an absent or out-of-scope management admission connection input, purely.
///
/// This is the fail-closed gate the management surface runs **before any
/// network, database, or filesystem I/O**. It proves everything a pure function
/// can prove about scope:
///
/// * the input exists, parses, names a host, and carries no query or fragment;
/// * its path names exactly one database;
/// * its user is one of the two management-admitter generation roles derived
///   from the fixed `(org, project, environment)` this process serves *and* the
///   database the input itself names.
///
/// The last predicate is what makes the control-database URL fail here rather
/// than at admission time: the database name is inside the scope digest, so a URL
/// pointing at `wamn-system` cannot present a role named for the project-env
/// database. What a pure check cannot prove is that the named database *is* that
/// project's environment database — no offline input distinguishes two reachable
/// databases — and that half is enforced by the ACL: only the project-env
/// database grants this role `CONNECT`.
pub fn parse_management_admission_url(
    url: &str,
    org: &str,
    project: &str,
    environment: &str,
) -> Result<ManagementAdmissionConnection, ManagementAdmissionUrlError> {
    if url.is_empty() {
        return Err(refuse(
            ManagementAdmissionUrlErrorKind::Absent,
            "the management admission connection input is required and has no fallback",
        ));
    }
    let parsed = Url::parse(url).map_err(|_| {
        refuse(
            ManagementAdmissionUrlErrorKind::Malformed,
            "the management admission connection input is not a URL",
        )
    })?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") {
        return Err(refuse(
            ManagementAdmissionUrlErrorKind::Scheme,
            "the management admission connection input must be a postgres URL",
        ));
    }
    // `postgres` is not a "special" scheme, so `url` accepts an empty authority
    // (`postgres:///db`) and reports `Some("")` rather than `None`.
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(refuse(
            ManagementAdmissionUrlErrorKind::Malformed,
            "the management admission connection input names no host",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(refuse(
            ManagementAdmissionUrlErrorKind::Extra,
            "the management admission connection input must carry no query or fragment",
        ));
    }
    let database = parsed.path().strip_prefix('/').unwrap_or(parsed.path());
    if database.is_empty() || database.contains('/') {
        return Err(refuse(
            ManagementAdmissionUrlErrorKind::Database,
            "the management admission connection input must name exactly one database",
        ));
    }
    let presented = parsed.username();
    let scoped = [CredentialGeneration::A, CredentialGeneration::B].map(|generation| {
        (
            generation,
            management_admitter_generation_role(org, project, environment, database, generation),
        )
    });
    let Some((generation, role)) = scoped
        .into_iter()
        .find(|(_, role)| role.as_str() == presented)
    else {
        return Err(refuse(
            ManagementAdmissionUrlErrorKind::Role,
            "the management admission connection input does not authenticate as this \
             org/project/environment's management-admitter generation",
        ));
    };
    Ok(ManagementAdmissionConnection {
        database: database.to_owned(),
        role,
        generation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_author::control_author_generation_role;
    use crate::{APP_ROLE, MANAGEMENT_ADMITTER_ROLE};

    const ORG: &str = "acme";
    const PROJECT: &str = "receiving";
    const ENVIRONMENT: &str = "dev";
    /// The project-environment database, suffixed exactly as provisioning mints it.
    const DATABASE: &str = "wamn-db-acme--receiving--dev--k3m9x2p7";

    fn role(generation: CredentialGeneration) -> String {
        management_admitter_generation_role(ORG, PROJECT, ENVIRONMENT, DATABASE, generation)
    }

    fn url(user: &str, database: &str) -> String {
        format!("postgres://{user}:secret@project.invalid:5432/{database}")
    }

    #[test]
    fn generation_roles_fit_postgres_and_never_collide_with_the_control_author() {
        let a = role(CredentialGeneration::A);
        let b = role(CredentialGeneration::B);
        assert_ne!(a, b);
        assert!(a.starts_with("wamn_mgmt_admitter_"));
        assert!(a.ends_with("_a") && b.ends_with("_b"));
        // 18 + 1 + 40 + 1 + 1, inside PostgreSQL's 63-byte identifier limit. The
        // 24-byte stable ACL role name would have minted 67 (wamn-0h0g.13.62),
        // which is why the generation prefix is its own frozen, shorter string.
        // This bound is THIS family's, not a universal one: `wamn_dispatch_reader`
        // is 20 bytes and derives a role of exactly 63, which fits.
        assert_eq!(a.len(), 61);
        assert_eq!(b.len(), 61);
        assert!(MANAGEMENT_ADMITTER_ROLE.len() > "wamn_mgmt_admitter".len());
        let digest = &a["wamn_mgmt_admitter_".len()..a.len() - 2];
        assert_eq!(digest.len(), 40);
        assert!(
            digest
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        );

        // The two management-held planes must never derive one name from the
        // other's scope. Only the domain separator distinguishes the preimages
        // once the same four components are framed, so this is the assertion
        // that fails if the separator is dropped or copied.
        assert_ne!(
            management_admitter_scope_hash(ORG, PROJECT, ENVIRONMENT, DATABASE),
            crate::control_author::control_author_scope_hash(ORG, PROJECT, ENVIRONMENT, DATABASE)
        );
        assert!(!a.contains("control_author"));
        assert!(!a.contains(APP_ROLE));
    }

    #[test]
    fn scope_framing_separates_ambiguous_component_boundaries() {
        // Without length-prefix framing these two scopes share a preimage.
        assert_ne!(
            management_admitter_scope_hash("a-b", "c", ENVIRONMENT, DATABASE),
            management_admitter_scope_hash("a", "b-c", ENVIRONMENT, DATABASE)
        );
        // Every component participates.
        let base = management_admitter_scope_hash(ORG, PROJECT, ENVIRONMENT, DATABASE);
        assert_ne!(
            base,
            management_admitter_scope_hash("other", PROJECT, ENVIRONMENT, DATABASE)
        );
        assert_ne!(
            base,
            management_admitter_scope_hash(ORG, "other", ENVIRONMENT, DATABASE)
        );
        assert_ne!(
            base,
            management_admitter_scope_hash(ORG, PROJECT, "prod", DATABASE)
        );
        assert_ne!(
            base,
            management_admitter_scope_hash(ORG, PROJECT, ENVIRONMENT, "other-db")
        );
    }

    #[test]
    fn an_in_scope_generation_url_resolves_its_slot_and_database() {
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            let connection = parse_management_admission_url(
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
            ("", ManagementAdmissionUrlErrorKind::Absent),
            ("not a url", ManagementAdmissionUrlErrorKind::Malformed),
            (
                "postgres:///only-a-path",
                ManagementAdmissionUrlErrorKind::Malformed,
            ),
            (
                "mysql://wamn_mgmt_admitter_x_a:secret@project.invalid/wamn-db-x",
                ManagementAdmissionUrlErrorKind::Scheme,
            ),
        ] {
            let error = parse_management_admission_url(input, ORG, PROJECT, ENVIRONMENT)
                .expect_err("out-of-scope input must refuse");
            assert_eq!(error.kind(), expected, "{input:?}");
        }

        // A path that names no database, or more than one.
        for path in ["", "one/two"] {
            let error =
                parse_management_admission_url(&url(&admitted, path), ORG, PROJECT, ENVIRONMENT)
                    .expect_err("a non-database path must refuse");
            assert_eq!(
                error.kind(),
                ManagementAdmissionUrlErrorKind::Database,
                "{path:?}"
            );
        }

        // A query or fragment on a connection input.
        for suffix in ["?sslmode=disable", "#fragment"] {
            let error = parse_management_admission_url(
                &format!("{}{suffix}", url(&admitted, DATABASE)),
                ORG,
                PROJECT,
                ENVIRONMENT,
            )
            .expect_err("a decorated connection input must refuse");
            assert_eq!(
                error.kind(),
                ManagementAdmissionUrlErrorKind::Extra,
                "{suffix:?}"
            );
        }

        // The identity half: the shared query role, the control plane's author
        // role, the stable NOLOGIN role itself, and a generation minted for
        // another scope.
        let foreign = management_admitter_generation_role(
            ORG,
            PROJECT,
            "prod",
            DATABASE,
            CredentialGeneration::A,
        );
        let control = control_author_generation_role(
            ORG,
            PROJECT,
            ENVIRONMENT,
            DATABASE,
            CredentialGeneration::A,
        );
        for user in [
            APP_ROLE,
            "wamn_scenario_author",
            MANAGEMENT_ADMITTER_ROLE,
            foreign.as_str(),
            control.as_str(),
        ] {
            let error =
                parse_management_admission_url(&url(user, DATABASE), ORG, PROJECT, ENVIRONMENT)
                    .expect_err("an out-of-scope identity must refuse");
            assert_eq!(
                error.kind(),
                ManagementAdmissionUrlErrorKind::Role,
                "{user}"
            );
        }

        // The control database with this scope's admitter role: the database name
        // is inside the digest, so the role no longer matches.
        let error = parse_management_admission_url(
            &url(&admitted, "wamn-system"),
            ORG,
            PROJECT,
            ENVIRONMENT,
        )
        .expect_err("a control-database URL must refuse");
        assert_eq!(error.kind(), ManagementAdmissionUrlErrorKind::Role);
    }

    #[test]
    fn a_refusal_never_echoes_the_connection_input() {
        let error =
            parse_management_admission_url(&url(APP_ROLE, DATABASE), ORG, PROJECT, ENVIRONMENT)
                .expect_err("an out-of-scope identity must refuse");
        for rendered in [format!("{error}"), format!("{error:?}")] {
            assert!(!rendered.contains("secret"), "{rendered}");
            assert!(!rendered.contains("project.invalid"), "{rendered}");
        }
        assert!(format!("{error}").contains("WAMN_MANAGEMENT_ADMISSION_PG_URL"));
    }
}
