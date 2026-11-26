//! Host-attested operation session identity.

use crate::database::SqlError;

/// The complete host-injected identity set for an operation session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionIdentity {
    /// The target project environment, required before database selection.
    pub project_env_key: Option<wamn_registry::ProjectEnvKey>,
    /// Which project database this session resolves against. `None` ⇒ the
    /// default project.
    pub project: Option<wamn_registry::ProjectId>,
    /// `SET LOCAL search_path`. `None` ⇒ the server's own search_path.
    pub schema: Option<String>,
    pub runner: Option<String>,
    pub role: Option<String>,
    pub user_id: Option<wamn_identity_protocol::PrincipalId>,
    pub operation: Option<wamn_package::canonical_name::CanonicalName>,
    pub app: Option<wamn_registry::AppName>,
}

/// A durable-queue lease owner. An identity-format contract: bounded
/// `[A-Za-z0-9_-]`, no quotes/backslashes. Since R2 this is NO LONGER the
/// injection boundary on the plugin path: the begin batch escapes the runner
/// as a PostgreSQL literal, so a quote/backslash is inert data, but a malformed owner still
/// fails closed.
pub fn valid_runner(runner: &str) -> bool {
    !runner.is_empty()
        && runner.len() <= 128
        && runner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A `search_path` schema name. Stricter than a project: no hyphens. Since R2 the
/// value uses PostgreSQL literal escaping in the begin batch, but the no-hyphen
/// rule still matters where a schema
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

/// Reject a malformed session identity before it is bound (R16). Since R2 these
/// validators are not the injection boundary: identity values use PostgreSQL
/// literal escaping in the begin batch, so a `'`/`;`/`--` value is inert data, but a
/// malformed identity still fails closed: they define what a *legal* id is (and
/// the no-hyphen `valid_schema` rule still matters where a schema name is quoted
/// into DDL elsewhere).
pub fn validate_sql_identity(
    schema: Option<&str>,
    runner: Option<&str>,
    role: Option<&str>,
) -> Result<(), SqlError> {
    if let Some(schema) = schema
        && !valid_schema(schema)
    {
        return Err(SqlError::QueryError((
            "WAMN0".to_string(),
            "invalid search_path schema".to_string(),
        )));
    }
    if let Some(runner) = runner
        && !valid_runner(runner)
    {
        return Err(SqlError::QueryError((
            "WAMN0".to_string(),
            "invalid runner owner".to_string(),
        )));
    }
    if let Some(role) = role
        && !role
            .parse::<wamn_role::RoleName>()
            .is_ok_and(|parsed| parsed.as_str() == role)
    {
        return Err(SqlError::QueryError((
            "WAMN0".to_string(),
            "invalid caller role".to_string(),
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_sql_identity;

    #[test]
    fn validate_identity_rejects_malformed_identities() {
        assert!(validate_sql_identity(Some("public"), Some("owner-1"), Some("inspector")).is_ok());
        assert!(validate_sql_identity(None, None, None).is_ok());
        assert!(validate_sql_identity(Some("has-hyphen"), None, None).is_err());
        assert!(validate_sql_identity(None, Some("bad;runner"), None).is_err());
        // `''` is the base denial policy, never an identity.
        assert!(validate_sql_identity(None, None, Some("")).is_err());
    }
}
