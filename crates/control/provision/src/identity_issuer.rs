//! Scoped database credentials for the separate identity authority.
//!
//! The resource scope is the exact HTTPS issuer and the T1 system database.
//! It is not a project environment or a runtime authority class. This module
//! emits role SQL only. It does not create or read session signing keys.

use std::fmt;

use sha2::{Digest as _, Sha256};
use url::Url;
use wamn_pg_core::{quote_ident, quote_literal};

use crate::CredentialGeneration;

/// Stable role that owns no objects and cannot log in.
pub const IDENTITY_ISSUER_ROLE: &str = "wamn_identity_issuer";
/// Exact database served by the identity authority.
pub const IDENTITY_ISSUER_DATABASE: &str = "wamn_system";
/// The only tables the signing authority may mutate.
pub const IDENTITY_ISSUER_TABLES: [&str; 2] = ["session_keys", "session_signing_state"];
/// Fresh PAT, membership and current environment-incarnation inputs to exchange.
pub const IDENTITY_ISSUER_READ_COLUMNS: [(&str, &str, &[&str]); 4] = [
    (
        "identity",
        "principals",
        &["id", "kind", "subject", "display_name", "status"],
    ),
    (
        "identity",
        "pats",
        &[
            "principal_id",
            "token_prefix",
            "token_hash",
            "revoked_at",
            "expires_at",
        ],
    ),
    (
        "identity",
        "project_env_memberships",
        &["principal_id", "org", "project", "env"],
    ),
    (
        "registry",
        "project_envs",
        &["org", "project", "env", "instance_suffix"],
    ),
];

/// Which input predicate refused an identity credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityIssuerUrlErrorKind {
    Issuer,
    Absent,
    Malformed,
    Scheme,
    Database,
    Role,
    Extra,
}

/// A refused input with a fixed reason and no credential material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityIssuerUrlError {
    kind: IdentityIssuerUrlErrorKind,
    reason: &'static str,
}

impl IdentityIssuerUrlError {
    /// The predicate that refused the input.
    pub const fn kind(&self) -> IdentityIssuerUrlErrorKind {
        self.kind
    }
}

impl fmt::Display for IdentityIssuerUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "identity issuer configuration refused: {}",
            self.reason
        )
    }
}

impl std::error::Error for IdentityIssuerUrlError {}

fn refuse(kind: IdentityIssuerUrlErrorKind, reason: &'static str) -> IdentityIssuerUrlError {
    IdentityIssuerUrlError { kind, reason }
}

/// Require an HTTPS issuer without user information, a query, or a fragment.
pub fn validate_identity_issuer(issuer: &str) -> Result<(), IdentityIssuerUrlError> {
    let parsed = Url::parse(issuer).map_err(|_| {
        refuse(
            IdentityIssuerUrlErrorKind::Issuer,
            "issuer must be an HTTPS URL",
        )
    })?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none_or(str::is_empty)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || issuer.chars().any(char::is_whitespace)
    {
        return Err(refuse(
            IdentityIssuerUrlErrorKind::Issuer,
            "issuer must use HTTPS with no credentials, whitespace, query, or fragment",
        ));
    }
    Ok(())
}

/// Derive a 160-bit digest from the exact issuer and fixed system database.
pub fn identity_issuer_scope_hash(issuer: &str) -> Result<String, IdentityIssuerUrlError> {
    validate_identity_issuer(issuer)?;
    let mut digest = Sha256::new();
    for field in [
        "wamn.identity-issuer.scope.v0.1",
        "issuer",
        issuer,
        "database",
        IDENTITY_ISSUER_DATABASE,
    ] {
        let length = u64::try_from(field.len()).expect("an issuer scope field fits u64");
        digest.update(length.to_be_bytes());
        digest.update(field.as_bytes());
    }
    Ok(hex::encode(digest.finalize())[..40].to_owned())
}

/// Derive one A/B login name within PostgreSQL's 63-byte identifier limit.
pub fn identity_issuer_generation_role(
    issuer: &str,
    generation: CredentialGeneration,
) -> Result<String, IdentityIssuerUrlError> {
    Ok(format!(
        "{IDENTITY_ISSUER_ROLE}_{}_{}",
        identity_issuer_scope_hash(issuer)?,
        generation.as_str()
    ))
}

/// A parsed scoped credential whose formatted forms never expose its URL.
#[derive(Clone)]
pub struct IdentityIssuerConnection {
    url: String,
    role: String,
    generation: CredentialGeneration,
}

impl IdentityIssuerConnection {
    /// The validated credential, for the database driver only.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The fixed T1 database name.
    pub const fn database(&self) -> &'static str {
        IDENTITY_ISSUER_DATABASE
    }

    /// The exact scoped login role.
    pub fn role(&self) -> &str {
        &self.role
    }

    /// The A/B credential slot.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }
}

impl fmt::Debug for IdentityIssuerConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityIssuerConnection")
            .field("url", &"[REDACTED]")
            .field("role", &self.role)
            .field("generation", &self.generation)
            .finish()
    }
}

impl fmt::Display for IdentityIssuerConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} on {IDENTITY_ISSUER_DATABASE}", self.role)
    }
}

/// Refuse broad or mis-scoped credentials before any I/O.
pub fn parse_identity_issuer_url(
    raw: &str,
    issuer: &str,
) -> Result<IdentityIssuerConnection, IdentityIssuerUrlError> {
    validate_identity_issuer(issuer)?;
    if raw.is_empty() {
        return Err(refuse(
            IdentityIssuerUrlErrorKind::Absent,
            "WAMN_IDENTITY_DATABASE_URL is required",
        ));
    }
    let parsed = Url::parse(raw).map_err(|_| {
        refuse(
            IdentityIssuerUrlErrorKind::Malformed,
            "database credential must be a URL",
        )
    })?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") {
        return Err(refuse(
            IdentityIssuerUrlErrorKind::Scheme,
            "database credential must use postgres or postgresql",
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty) || raw.chars().any(char::is_whitespace) {
        return Err(refuse(
            IdentityIssuerUrlErrorKind::Malformed,
            "database credential must name a host and contain no whitespace",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(refuse(
            IdentityIssuerUrlErrorKind::Extra,
            "database credential must carry no query or fragment",
        ));
    }
    if parsed.path() != "/wamn_system" {
        return Err(refuse(
            IdentityIssuerUrlErrorKind::Database,
            "database credential must name wamn_system exactly",
        ));
    }
    for generation in [CredentialGeneration::A, CredentialGeneration::B] {
        let role = identity_issuer_generation_role(issuer, generation)?;
        if parsed.username() == role {
            return Ok(IdentityIssuerConnection {
                url: raw.to_owned(),
                role,
                generation,
            });
        }
    }
    Err(refuse(
        IdentityIssuerUrlErrorKind::Role,
        "database user must be this issuer's A or B login generation",
    ))
}

/// Grant key-table mutations and the fresh system inputs required by exchange.
///
/// The driver must first refuse unexpected grants, ownership, or memberships.
/// Live tests exercise the resulting permissions, not the SQL text.
pub fn grant_identity_issuer_surface_sql() -> String {
    let role = quote_ident(IDENTITY_ISSUER_ROLE);
    let mut sql = format!(
        "{} GRANT USAGE ON SCHEMA identity, registry TO {role}; \
         GRANT SELECT, INSERT, UPDATE, DELETE ON \
           identity.session_keys, identity.session_signing_state TO {role};",
        crate::sql::ensure_acl_role_sql(IDENTITY_ISSUER_ROLE),
    );
    for (schema, table, columns) in IDENTITY_ISSUER_READ_COLUMNS {
        let columns = columns
            .iter()
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(
            " GRANT SELECT ({columns}) ON TABLE {schema}.{table} TO {role};",
            schema = quote_ident(schema),
            table = quote_ident(table),
        ));
    }
    sql
}

/// Prepare an inactive login after the driver checks its exact role state.
pub fn prepare_identity_issuer_generation_sql(
    issuer: &str,
    generation: CredentialGeneration,
    password: &str,
    expires_at: &str,
) -> Result<String, IdentityIssuerUrlError> {
    let role = identity_issuer_generation_role(issuer, generation)?;
    let role_ident = quote_ident(&role);
    Ok(format!(
        "{surface} DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role_ident} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               INHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; END $$; \
         GRANT {acl_role} TO {role_ident} WITH ADMIN FALSE, INHERIT TRUE, SET FALSE; \
         ALTER ROLE {role_ident} LOGIN PASSWORD {password} VALID UNTIL {expires_at}; \
         GRANT CONNECT ON DATABASE {database} TO {role_ident};",
        surface = grant_identity_issuer_surface_sql(),
        role_lit = quote_literal(&role),
        acl_role = quote_ident(IDENTITY_ISSUER_ROLE),
        password = quote_literal(password),
        expires_at = quote_literal(expires_at),
        database = quote_ident(IDENTITY_ISSUER_DATABASE),
    ))
}

/// Remove authority and authentication before the driver drains old sessions.
pub fn retire_identity_issuer_generation_sql(
    issuer: &str,
    generation: CredentialGeneration,
) -> Result<String, IdentityIssuerUrlError> {
    Ok(crate::sql::retire_acl_generation_sql(
        IDENTITY_ISSUER_ROLE,
        IDENTITY_ISSUER_DATABASE,
        &identity_issuer_generation_role(issuer, generation)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        IdentityIssuerUrlErrorKind, identity_issuer_generation_role, parse_identity_issuer_url,
        validate_identity_issuer,
    };
    use crate::CredentialGeneration;

    const ISSUER: &str = "https://wamn-identity.wamn-system.svc";

    #[test]
    fn both_generation_urls_round_trip_without_disclosing_passwords() {
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            let role = identity_issuer_generation_role(ISSUER, generation).unwrap();
            assert!(role.len() <= 63);
            let raw = format!("postgres://{role}:hidden-value@sysdb/wamn_system");
            let parsed = parse_identity_issuer_url(&raw, ISSUER).unwrap();
            assert_eq!(parsed.url(), raw);
            assert_eq!(parsed.role(), role);
            assert_eq!(parsed.generation(), generation);
            assert_eq!(parsed.database(), "wamn_system");
            assert!(!format!("{parsed:?} {parsed}").contains("hidden-value"));
        }
        assert_ne!(
            identity_issuer_generation_role(ISSUER, CredentialGeneration::A).unwrap(),
            identity_issuer_generation_role(ISSUER, CredentialGeneration::B).unwrap()
        );
    }

    #[test]
    fn owner_wrong_issuer_generation_database_and_url_overrides_are_refused() {
        let a = identity_issuer_generation_role(ISSUER, CredentialGeneration::A).unwrap();
        let other =
            identity_issuer_generation_role("https://other.example", CredentialGeneration::A)
                .unwrap();
        for (raw, kind) in [
            (String::new(), IdentityIssuerUrlErrorKind::Absent),
            (
                "not-a-url-hidden-value".into(),
                IdentityIssuerUrlErrorKind::Malformed,
            ),
            (
                "postgres://wamn_system:hidden-value@sysdb/wamn_system".into(),
                IdentityIssuerUrlErrorKind::Role,
            ),
            (
                format!("postgres://{other}:hidden-value@sysdb/wamn_system"),
                IdentityIssuerUrlErrorKind::Role,
            ),
            (
                format!("postgres://{a}c:hidden-value@sysdb/wamn_system"),
                IdentityIssuerUrlErrorKind::Role,
            ),
            (
                format!("postgres://{a}:hidden-value@sysdb/other"),
                IdentityIssuerUrlErrorKind::Database,
            ),
            (
                format!("postgres://{a}:hidden-value@sysdb/%77amn_system"),
                IdentityIssuerUrlErrorKind::Database,
            ),
            (
                format!("postgres://{a}:hidden-value@sysdb/wamn_system?user=wamn_system"),
                IdentityIssuerUrlErrorKind::Extra,
            ),
            (
                format!("postgres://{a}:hidden-value@sysdb/wamn_system#hidden-value"),
                IdentityIssuerUrlErrorKind::Extra,
            ),
            (
                format!("https://{a}:hidden-value@sysdb/wamn_system"),
                IdentityIssuerUrlErrorKind::Scheme,
            ),
        ] {
            let error = parse_identity_issuer_url(&raw, ISSUER).unwrap_err();
            assert_eq!(error.kind(), kind);
            assert!(!format!("{error:?} {error}").contains("hidden-value"));
        }
    }

    #[test]
    fn issuer_refuses_insecure_or_ambiguous_authorities() {
        for issuer in [
            "http://identity",
            "https://u:p@identity",
            "https://identity?q=x",
            "https://identity#x",
            " https://identity",
        ] {
            assert!(validate_identity_issuer(issuer).is_err());
        }
    }
}
