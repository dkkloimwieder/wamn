//! Scoped project-environment credentials for fresh session role reads.
//!
//! The provisioner supplies the scope and physical database. This pure parser
//! rejects broad and swapped credentials before I/O; database ACLs enforce the
//! approved columns. Neither success formatting nor errors reveal the URL.

use std::fmt;

use url::Url;

use crate::CredentialGeneration;
use crate::workload_role::{WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role};

/// Which predicate refused a session role-reader credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRoleReaderUrlErrorKind {
    Absent,
    Malformed,
    Scheme,
    Database,
    Role,
    Extra,
}

/// A fixed credential refusal that contains no input or secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRoleReaderUrlError {
    kind: SessionRoleReaderUrlErrorKind,
    reason: &'static str,
}

impl SessionRoleReaderUrlError {
    /// The predicate that refused the credential.
    pub const fn kind(&self) -> SessionRoleReaderUrlErrorKind {
        self.kind
    }
}

impl fmt::Display for SessionRoleReaderUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "session role-reader credential refused: {}",
            self.reason
        )
    }
}

impl std::error::Error for SessionRoleReaderUrlError {}

fn refuse(kind: SessionRoleReaderUrlErrorKind, reason: &'static str) -> SessionRoleReaderUrlError {
    SessionRoleReaderUrlError { kind, reason }
}

/// A credential checked against the provisioner-controlled environment target.
#[derive(Clone)]
pub struct SessionRoleReaderConnection {
    url: String,
    database: String,
    role: String,
    generation: CredentialGeneration,
}

impl SessionRoleReaderConnection {
    /// The validated credential, for the database driver only.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The trusted physical target database.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// The scoped A/B login name.
    pub fn role(&self) -> &str {
        &self.role
    }

    /// The credential's reusable generation slot.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }
}

impl fmt::Debug for SessionRoleReaderConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionRoleReaderConnection")
            .field("url", &"[REDACTED]")
            .field("database", &self.database)
            .field("role", &self.role)
            .field("generation", &self.generation)
            .finish()
    }
}

impl fmt::Display for SessionRoleReaderConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} on {}", self.role, self.database)
    }
}

/// Refuse credentials outside the trusted org, project, environment and database.
///
/// The database argument must come from provisioning, never from the request
/// or the presented URL. This check does not authenticate the remote server;
/// deployment supplies the endpoint and database connection security.
pub fn parse_session_role_reader_url(
    raw: &str,
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
) -> Result<SessionRoleReaderConnection, SessionRoleReaderUrlError> {
    use SessionRoleReaderUrlErrorKind as Kind;

    if raw.is_empty() {
        return Err(refuse(
            Kind::Absent,
            "a dedicated role-reader credential is required",
        ));
    }
    let parsed =
        Url::parse(raw).map_err(|_| refuse(Kind::Malformed, "credential must be a URL"))?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") {
        return Err(refuse(
            Kind::Scheme,
            "credential must use postgres or postgresql",
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty) || raw.chars().any(char::is_whitespace) {
        return Err(refuse(
            Kind::Malformed,
            "credential must name a host without whitespace",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(refuse(
            Kind::Extra,
            "credential must carry no query or fragment",
        ));
    }
    if database.is_empty()
        || database.contains('/')
        || parsed.path().strip_prefix('/') != Some(database)
    {
        return Err(refuse(
            Kind::Database,
            "credential must name the trusted physical database exactly",
        ));
    }
    for generation in [CredentialGeneration::A, CredentialGeneration::B] {
        let role = workload_generation_role(
            WorkloadRoleFamily::SessionRoleReader,
            WorkloadRoleScope::ProjectEnvironment {
                org,
                project,
                environment,
                database,
            },
            generation,
        )
        .expect("the session role reader always has project-environment scope");
        if parsed.username() == role {
            return Ok(SessionRoleReaderConnection {
                url: raw.to_owned(),
                database: database.to_owned(),
                role,
                generation,
            });
        }
    }
    Err(refuse(
        Kind::Role,
        "credential must use this environment's dedicated A/B role-reader login",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATABASE: &str = "wamn-db-acme--receiving--dev--k3m9x2p7";
    const SCOPE: WorkloadRoleScope<'_> = WorkloadRoleScope::ProjectEnvironment {
        org: "acme",
        project: "receiving",
        environment: "dev",
        database: DATABASE,
    };

    fn url(
        family: WorkloadRoleFamily,
        scope: WorkloadRoleScope<'_>,
        generation: CredentialGeneration,
    ) -> String {
        let role = workload_generation_role(family, scope, generation).unwrap();
        format!("postgres://{role}:fixture-password@database.invalid/{DATABASE}")
    }

    fn parse(raw: &str) -> Result<SessionRoleReaderConnection, SessionRoleReaderUrlError> {
        parse_session_role_reader_url(raw, "acme", "receiving", "dev", DATABASE)
    }

    #[test]
    fn both_generations_are_scoped_and_formatted_without_credentials() {
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            let raw = url(WorkloadRoleFamily::SessionRoleReader, SCOPE, generation);
            let connection = parse(&raw).unwrap();
            assert_eq!(connection.url(), raw);
            assert_eq!(connection.database(), DATABASE);
            assert_eq!(connection.generation(), generation);
            assert_eq!(connection.role().len(), 61);
            assert!(connection.role().starts_with("wamn_session_roles_"));
            assert!(!format!("{connection:?} {connection}").contains("fixture-password"));
        }
    }

    #[test]
    fn broad_and_other_family_credentials_are_refused() {
        for family in [
            WorkloadRoleFamily::HttpAdmitter,
            WorkloadRoleFamily::ServiceReader,
            WorkloadRoleFamily::ExecutorPlatform,
        ] {
            assert_eq!(
                parse(&url(family, SCOPE, CredentialGeneration::A))
                    .unwrap_err()
                    .kind(),
                SessionRoleReaderUrlErrorKind::Role
            );
        }
        for role in [
            "postgres",
            "wamn_system",
            "wamn_app",
            "wamn_session_role_reader",
        ] {
            let raw = format!("postgres://{role}:fixture-password@database.invalid/{DATABASE}");
            let error = parse(&raw).unwrap_err();
            assert_eq!(error.kind(), SessionRoleReaderUrlErrorKind::Role);
            assert!(!format!("{error:?} {error}").contains("fixture-password"));
        }
    }

    #[test]
    fn each_target_coordinate_and_database_incarnation_is_bound() {
        let raw = url(
            WorkloadRoleFamily::SessionRoleReader,
            SCOPE,
            CredentialGeneration::A,
        );
        for (org, project, environment, database) in [
            ("other", "receiving", "dev", DATABASE),
            ("acme", "other", "dev", DATABASE),
            ("acme", "receiving", "prod", DATABASE),
            (
                "acme",
                "receiving",
                "dev",
                "wamn-db-acme--receiving--dev--z9z9z9z9",
            ),
        ] {
            assert!(
                parse_session_role_reader_url(&raw, org, project, environment, database).is_err()
            );
        }
        let mut changed = Url::parse(&raw).unwrap();
        changed.set_path("/wamn-db-acme--receiving--dev--z9z9z9z9");
        assert_eq!(
            parse(changed.as_str()).unwrap_err().kind(),
            SessionRoleReaderUrlErrorKind::Database
        );
    }

    #[test]
    fn malformed_or_overridable_connections_are_refused_without_echo() {
        let raw = url(
            WorkloadRoleFamily::SessionRoleReader,
            SCOPE,
            CredentialGeneration::A,
        );
        for (input, kind) in [
            (String::new(), SessionRoleReaderUrlErrorKind::Absent),
            (
                "fixture-password".to_owned(),
                SessionRoleReaderUrlErrorKind::Malformed,
            ),
            (
                raw.replacen("postgres:", "https:", 1),
                SessionRoleReaderUrlErrorKind::Scheme,
            ),
            (
                raw.replace("database.invalid", ""),
                SessionRoleReaderUrlErrorKind::Malformed,
            ),
            (
                format!("{raw}?options=fixture-password"),
                SessionRoleReaderUrlErrorKind::Extra,
            ),
            (
                format!("{raw}#fixture-password"),
                SessionRoleReaderUrlErrorKind::Extra,
            ),
            (format!(" {raw}"), SessionRoleReaderUrlErrorKind::Malformed),
            (
                format!("{raw}/extra"),
                SessionRoleReaderUrlErrorKind::Database,
            ),
        ] {
            let error = parse(&input).unwrap_err();
            assert_eq!(error.kind(), kind);
            assert!(!format!("{error:?} {error}").contains("fixture-password"));
        }
    }
}
