//! Scoped T1 system-database READER credential contract
//! (`wamn-0h0g.12.116`, `wamn-0h0g.12.67`).
//!
//! Two consumers read the control database and only read it: the CDC reader
//! wants one registration row, and the management surface wants a token digest
//! and a principal's project roles. Both mounted `WAMN_SYSTEM_URL`, and that
//! Secret authenticated as `wamn_system` — the AUTHORIZATION owner of the
//! `registry`, `provisioning` and `identity` schemas. `deploy/sql/system-schema.sql`
//! forces no row-level security, so that credential is not merely wide, it is
//! unconfined.
//!
//! Each consumer's identity is now a scoped A/B LOGIN generation of its own
//! stable NOLOGIN ACL role, on the [`crate::sql::stable_surface_sql`] grant set
//! for its family. ONE module for both because the derivation and the refusal
//! are identical; TWO families because the grant sets are disjoint and must
//! stay so. [`SystemReader`] is closed over exactly the two, so no third caller
//! can reach this machinery with a family that has no read surface.
//!
//! Everything here is pure. [`parse_system_reader_url`] is what lets a consumer
//! refuse a mis-scoped connection input **before it opens a socket** — including
//! the wide owner credential, which cannot present a role name derived from any
//! scope.

use std::fmt;

use url::Url;

use wamn_run_state::CredentialGeneration;

use crate::workload_role::{
    WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role, workload_role_scope_hash,
};

/// The closed pair of T1 control-database read consumers.
///
/// Not `WorkloadRoleFamily`: only these two families have a read surface on the
/// control database, and taking the wider enum here would let a caller derive a
/// plausible-looking system-reader login for a family that has no grant set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemReader {
    /// `services/cdc-reader` — `registry.event_readers`, one `SELECT`.
    Registry,
    /// `services/scenario-worker`'s management surface — `identity.*`, two
    /// `SELECT`s.
    Identity,
}

impl SystemReader {
    /// Both readers, in family declaration order.
    pub const ALL: [Self; 2] = [Self::Registry, Self::Identity];

    /// The provisioning family this reader's credential belongs to.
    pub const fn family(self) -> WorkloadRoleFamily {
        match self {
            Self::Registry => WorkloadRoleFamily::RegistryReader,
            Self::Identity => WorkloadRoleFamily::IdentityReader,
        }
    }
}

impl fmt::Display for SystemReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.family().label())
    }
}

/// Deterministic 160-bit suffix for one system-reader scope.
pub fn system_reader_scope_hash(
    reader: SystemReader,
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
) -> String {
    workload_role_scope_hash(
        reader.family(),
        WorkloadRoleScope::Control {
            org,
            project,
            environment,
            database,
        },
    )
    .expect("a system reader always uses control scope")
}

/// Scoped PostgreSQL LOGIN role for one system-reader generation slot.
pub fn system_reader_generation_role(
    reader: SystemReader,
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
    generation: CredentialGeneration,
) -> String {
    workload_generation_role(
        reader.family(),
        WorkloadRoleScope::Control {
            org,
            project,
            environment,
            database,
        },
        generation,
    )
    .expect("a system reader always uses control scope")
}

/// Which predicate refused a system-reader connection input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemReaderUrlErrorKind {
    /// No connection input was supplied at all.
    Absent,
    /// The input is not a parseable URL, or names no host.
    Malformed,
    /// The scheme is not `postgres`/`postgresql`.
    Scheme,
    /// The path does not name exactly one database.
    Database,
    /// The user is not this scope's A or B reader generation.
    Role,
    /// The input carries a query or fragment, which a connection input must not.
    Extra,
}

impl SystemReaderUrlErrorKind {
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

/// A system-reader connection input that failed closed before any I/O.
///
/// It carries the refused predicate and a fixed reason, never the input: the
/// input holds a password, so neither `Debug` nor `Display` may echo it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemReaderUrlError {
    reader: SystemReader,
    kind: SystemReaderUrlErrorKind,
    reason: &'static str,
}

impl SystemReaderUrlError {
    /// Which reader refused the input.
    pub const fn reader(&self) -> SystemReader {
        self.reader
    }

    /// Which predicate refused the input.
    pub const fn kind(&self) -> SystemReaderUrlErrorKind {
        self.kind
    }

    /// The fixed reason this predicate refuses for.
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for SystemReaderUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "WAMN_SYSTEM_URL is out of scope for the {} ({}): {}",
            self.reader,
            self.kind.as_str(),
            self.reason
        )
    }
}

impl std::error::Error for SystemReaderUrlError {}

fn refuse(
    reader: SystemReader,
    kind: SystemReaderUrlErrorKind,
    reason: &'static str,
) -> SystemReaderUrlError {
    SystemReaderUrlError {
        reader,
        kind,
        reason,
    }
}

/// The scoped control-database read connection one consumer process holds.
///
/// Deliberately does not retain the URL, so its derived `Debug` cannot leak the
/// password the URL carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemReaderConnection {
    reader: SystemReader,
    database: String,
    role: String,
    generation: CredentialGeneration,
}

impl SystemReaderConnection {
    /// Which reader this connection belongs to.
    pub const fn reader(&self) -> SystemReader {
        self.reader
    }

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

/// Refuse an absent or out-of-scope `WAMN_SYSTEM_URL`, purely.
///
/// The fail-closed gate a reading consumer runs **before any network, database,
/// or filesystem I/O**, on the [`crate::parse_control_authoring_url`] terms:
///
/// * the input exists, parses, names a host, and carries no query or fragment;
/// * its path names exactly one database;
/// * its user is one of the two generation roles derived from the fixed
///   `(org, project, environment)` this process serves, *this reader's family*,
///   and the database the input itself names.
///
/// The last predicate is what refuses the wide owner credential offline: no
/// scope derives the name `wamn_system`, so a `WAMN_SYSTEM_URL` carrying the
/// unconfined owner crash-loops the consumer instead of serving from it. It is
/// also what keeps the two readers' Secrets from being swapped — the family's
/// domain separator is inside the digest, so the identity reader's login cannot
/// satisfy the registry reader's check or the reverse.
///
/// What a pure check cannot prove is that the named database *is* the control
/// database, and that half is enforced by the ACL: only the control database
/// grants these roles `CONNECT`, and each role's grant set is confined to its
/// own schema there.
pub fn parse_system_reader_url(
    reader: SystemReader,
    url: &str,
    org: &str,
    project: &str,
    environment: &str,
) -> Result<SystemReaderConnection, SystemReaderUrlError> {
    if url.is_empty() {
        return Err(refuse(
            reader,
            SystemReaderUrlErrorKind::Absent,
            "the system read connection input is required and has no fallback",
        ));
    }
    let parsed = Url::parse(url).map_err(|_| {
        refuse(
            reader,
            SystemReaderUrlErrorKind::Malformed,
            "the system read connection input is not a URL",
        )
    })?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") {
        return Err(refuse(
            reader,
            SystemReaderUrlErrorKind::Scheme,
            "the system read connection input must be a postgres URL",
        ));
    }
    // `postgres` is not a "special" scheme, so `url` accepts an empty authority
    // (`postgres:///db`) and reports `Some("")` rather than `None`.
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(refuse(
            reader,
            SystemReaderUrlErrorKind::Malformed,
            "the system read connection input names no host",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(refuse(
            reader,
            SystemReaderUrlErrorKind::Extra,
            "the system read connection input must carry no query or fragment",
        ));
    }
    let database = parsed.path().strip_prefix('/').unwrap_or(parsed.path());
    if database.is_empty() || database.contains('/') {
        return Err(refuse(
            reader,
            SystemReaderUrlErrorKind::Database,
            "the system read connection input must name exactly one database",
        ));
    }
    let presented = parsed.username();
    let scoped = [CredentialGeneration::A, CredentialGeneration::B].map(|generation| {
        (
            generation,
            system_reader_generation_role(reader, org, project, environment, database, generation),
        )
    });
    let Some((generation, role)) = scoped
        .into_iter()
        .find(|(_, role)| role.as_str() == presented)
    else {
        return Err(refuse(
            reader,
            SystemReaderUrlErrorKind::Role,
            "the system read connection input does not authenticate as this \
             org/project/environment's system-reader generation",
        ));
    };
    Ok(SystemReaderConnection {
        reader,
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
    const DATABASE: &str = "wamn_system";

    fn role(reader: SystemReader, generation: CredentialGeneration) -> String {
        system_reader_generation_role(reader, ORG, PROJECT, ENVIRONMENT, DATABASE, generation)
    }

    fn url(user: &str, database: &str) -> String {
        format!("postgres://{user}:secret@sysdb.invalid:5432/{database}")
    }

    fn parse(
        reader: SystemReader,
        url: &str,
    ) -> Result<SystemReaderConnection, SystemReaderUrlError> {
        parse_system_reader_url(reader, url, ORG, PROJECT, ENVIRONMENT)
    }

    /// THE FORGERY PRIMITIVE, REFUSED OFFLINE (`wamn-0h0g.12.67`).
    ///
    /// `wamn-system-db` authenticates as `wamn_system`, which owns
    /// `identity.pats` and `identity.project_roles` under no row-level security
    /// — so whoever reads that Secret can mint a PAT for any principal and
    /// self-grant any project role. No scope derives that name, so the consumer
    /// refuses it before it opens a socket rather than serving from it.
    #[test]
    fn the_unconfined_system_owner_credential_is_refused_by_both_readers() {
        for reader in SystemReader::ALL {
            let error = parse(reader, &url("wamn_system", DATABASE))
                .expect_err("the unconfined owner credential was accepted");
            assert_eq!(error.kind(), SystemReaderUrlErrorKind::Role);
            assert_eq!(error.reader(), reader);
            // The refusal must not echo the input: it carries a password.
            assert!(!format!("{error}").contains("secret"));
            assert!(!format!("{error:?}").contains("secret"));
        }
    }

    /// Each reader accepts its OWN generations and nothing else.
    #[test]
    fn each_reader_accepts_only_its_own_two_generations() {
        for reader in SystemReader::ALL {
            for generation in [CredentialGeneration::A, CredentialGeneration::B] {
                let accepted = parse(reader, &url(&role(reader, generation), DATABASE))
                    .expect("a reader must accept its own generation");
                assert_eq!(accepted.generation(), generation);
                assert_eq!(accepted.reader(), reader);
                assert_eq!(accepted.database(), DATABASE);
                assert_eq!(accepted.role(), role(reader, generation));
            }
        }
    }

    /// THE SWAP GUARD. The two grant sets are disjoint, so handing one reader
    /// the other's Secret would hand it the other's authority. The family's
    /// domain separator is inside the scope digest, so the swap is refused on
    /// the role predicate — offline, before either credential is used.
    #[test]
    fn neither_reader_accepts_the_other_readers_credential() {
        for reader in SystemReader::ALL {
            let other = match reader {
                SystemReader::Registry => SystemReader::Identity,
                SystemReader::Identity => SystemReader::Registry,
            };
            let error = parse(reader, &url(&role(other, CredentialGeneration::A), DATABASE))
                .expect_err("a reader accepted the other reader's credential");
            assert_eq!(error.kind(), SystemReaderUrlErrorKind::Role);
        }
    }

    /// A credential for another environment presents a role derived from that
    /// environment's digest, so it cannot satisfy this process's check.
    #[test]
    fn a_credential_scoped_to_another_environment_is_refused() {
        let foreign = system_reader_generation_role(
            SystemReader::Identity,
            ORG,
            PROJECT,
            "prod",
            DATABASE,
            CredentialGeneration::A,
        );
        let error = parse(SystemReader::Identity, &url(&foreign, DATABASE))
            .expect_err("another environment's credential was accepted");
        assert_eq!(error.kind(), SystemReaderUrlErrorKind::Role);
    }

    /// The database name is inside the digest, so a URL that names a different
    /// database cannot present a role derived for this one.
    #[test]
    fn a_credential_pointed_at_another_database_is_refused() {
        let error = parse(
            SystemReader::Registry,
            &url(
                &role(SystemReader::Registry, CredentialGeneration::A),
                "wamn-db-acme--receiving--dev",
            ),
        )
        .expect_err("a project-database URL was accepted");
        assert_eq!(error.kind(), SystemReaderUrlErrorKind::Role);
    }

    #[test]
    fn the_shape_predicates_refuse_before_the_role_predicate_is_reached() {
        let good = role(SystemReader::Registry, CredentialGeneration::A);
        for (input, kind) in [
            (String::new(), SystemReaderUrlErrorKind::Absent),
            ("not a url".to_owned(), SystemReaderUrlErrorKind::Malformed),
            (
                format!("postgres://{good}:secret@/{DATABASE}"),
                SystemReaderUrlErrorKind::Malformed,
            ),
            (
                format!("mysql://{good}:secret@sysdb.invalid:5432/{DATABASE}"),
                SystemReaderUrlErrorKind::Scheme,
            ),
            (
                format!("postgres://{good}:secret@sysdb.invalid:5432/{DATABASE}?sslmode=require"),
                SystemReaderUrlErrorKind::Extra,
            ),
            (
                format!("postgres://{good}:secret@sysdb.invalid:5432/"),
                SystemReaderUrlErrorKind::Database,
            ),
        ] {
            let error = parse(SystemReader::Registry, &input)
                .expect_err("an out-of-shape connection input was accepted");
            assert_eq!(error.kind(), kind, "{input:?}");
        }
    }

    /// Both readers' derived logins are exactly 63 bytes — the largest
    /// PostgreSQL stores untruncated. A longer role name or digest would be
    /// truncated with a NOTICE rather than refused, and truncation drops the
    /// `_a`/`_b` suffix that keeps the two generations apart.
    #[test]
    fn every_derived_system_reader_login_fits_postgres_exactly() {
        for reader in SystemReader::ALL {
            for generation in [CredentialGeneration::A, CredentialGeneration::B] {
                let derived = role(reader, generation);
                assert_eq!(derived.len(), 63, "{reader} {generation:?}");
                assert!(derived.starts_with(reader.family().acl_role()));
            }
            assert_ne!(
                role(reader, CredentialGeneration::A),
                role(reader, CredentialGeneration::B),
            );
            assert_eq!(
                system_reader_scope_hash(reader, ORG, PROJECT, ENVIRONMENT, DATABASE).len(),
                40,
            );
        }
    }
}
