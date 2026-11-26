//! The identity issuer role and its database privileges.

use postgres_protocol::escape::escape_identifier;
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use wamn_role::IDENTITY_ISSUER_ROLE;

use crate::provision_system::SYSTEM_DATABASE;

/// Tables that allow full row mutations by the signing authority.
pub const IDENTITY_ISSUER_TABLES: [&str; 2] = ["session_key", "session_signing_state"];
/// Reads for the identity issuer.
pub const IDENTITY_ISSUER_READ_COLUMNS: [(&str, &str, &[&str]); 9] = [
    (
        "identity",
        "principal",
        &["id", "type", "subject", "display_name", "active"],
    ),
    (
        "identity",
        "project_env_membership",
        &["principal_id", "org", "project", "env"],
    ),
    (
        "registry",
        "project_env",
        &["org", "env", "project", "instance_suffix"],
    ),
    // The control audience of an org
    // (docs/architecture/execution.md#control-sessions): the org must be
    // registered and the user must hold a control role there.
    (
        "identity",
        "project_role",
        &["principal_id", "org", "project", "role"],
    ),
    ("registry", "org", &["id"]),
    ("registry", "project", &["org", "id"]),
    ("registry", "env", &["org", "env", "active"]),
    ("identity", "org_role", &["principal_id", "org", "role"]),
    (
        "identity",
        "org_membership",
        &["principal_id", "org", "active"],
    ),
];

/// Narrow column grants for passwords and user management.
pub const IDENTITY_ISSUER_PASSWORD_COLUMNS: &[(&str, &str, &[&str])] = &[
    ("principal", "SELECT", &["email"]),
    (
        "principal",
        "INSERT",
        &["type", "subject", "email", "display_name"],
    ),
    ("principal", "UPDATE", &["active"]),
    (
        "password_credential",
        "SELECT",
        &["principal_id", "password_hash"],
    ),
    (
        "password_credential",
        "INSERT",
        &["principal_id", "password_hash"],
    ),
    (
        "password_token",
        "SELECT",
        &[
            "token_hash",
            "principal_id",
            "purpose",
            "expires_at",
            "consumed_at",
        ],
    ),
    (
        "password_token",
        "INSERT",
        &["token_hash", "principal_id", "purpose", "expires_at"],
    ),
    ("password_token", "UPDATE", &["consumed_at"]),
    ("password_credential", "UPDATE", &["password_hash"]),
    (
        "password_login",
        "SELECT",
        &[
            "id",
            "principal_id",
            "issuer",
            "audience",
            "authenticated_at",
            "expires_at",
            "renewal_expires_at",
            "revoked_at",
        ],
    ),
    (
        "password_login",
        "INSERT",
        &[
            "principal_id",
            "issuer",
            "audience",
            "authenticated_at",
            "expires_at",
            "renewal_expires_at",
        ],
    ),
    (
        "password_login",
        "UPDATE",
        &["renewal_expires_at", "revoked_at"],
    ),
    (
        "renewal_credential",
        "SELECT",
        &["token_hash", "login_id", "consumed_at"],
    ),
    ("renewal_credential", "INSERT", &["token_hash", "login_id"]),
    ("renewal_credential", "UPDATE", &["consumed_at"]),
];

/// Refuse an issuer outside the HTTPS identity contract.
pub fn validate_identity_issuer(issuer: &str) -> anyhow::Result<()> {
    let parsed = url::Url::parse(issuer)?;
    anyhow::ensure!(
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && !issuer.chars().any(char::is_whitespace),
        "invalid HTTPS issuer"
    );
    Ok(())
}

/// Derive the login of one issuer.
pub fn identity_issuer_login(issuer: &str) -> anyhow::Result<wamn_role::LoginName> {
    validate_identity_issuer(issuer)?;
    let mut digest = Sha256::new();
    for field in [
        "wamn.identity-issuer.identity.v0.1",
        "issuer",
        issuer,
        "database",
        SYSTEM_DATABASE,
    ] {
        digest.update(u64::try_from(field.len())?.to_be_bytes());
        digest.update(field.as_bytes());
    }
    Ok(wamn_registry::name::credential_login(
        IDENTITY_ISSUER_ROLE,
        &hex::encode(digest.finalize())[..40],
    ))
}

pub fn grant_identity_issuer_surface_sql() -> String {
    format!("{} {}", crate::sql::ensure_system_roles_sql(), grants_sql())
}

/// Grant the issuer surface after the cluster role and system store exist.
pub fn grants_sql() -> String {
    let role = escape_identifier(IDENTITY_ISSUER_ROLE);
    let mut sql = format!(
        "DO $$ BEGIN EXECUTE format('GRANT CONNECT ON DATABASE %I TO {role}', current_database()); END $$; \
         GRANT USAGE ON SCHEMA identity, registry, platform, wamn_authority TO {role}; \
         GRANT USAGE ON SCHEMA wamn_history TO {role}; \
         GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO {role}; \
         GRANT INSERT ON identity.principal_history, identity.password_credential_history TO {role}; \
         GRANT INSERT ON platform.tx_identity TO {role}; \
         GRANT EXECUTE ON FUNCTION wamn_authority.current_identity(), wamn_authority.current_principal_id(), wamn_authority.current_role(), wamn_authority.current_app() TO {role}; \
         GRANT SELECT, INSERT, UPDATE, DELETE ON \
           identity.session_key, identity.session_signing_state TO {role}; \
         GRANT SELECT, INSERT ON identity.nats_key TO {role}; \
         GRANT UPDATE (project_count, issued_at) ON identity.nats_key TO {role}; \
         GRANT EXECUTE ON FUNCTION identity.is_project_admin(uuid,text,text), identity.has_project_access(uuid,text,text,text) TO {role};",
    );
    for (schema, table, columns) in IDENTITY_ISSUER_READ_COLUMNS {
        let columns = columns
            .iter()
            .map(|column| escape_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            sql,
            " GRANT SELECT ({columns}) ON TABLE {schema}.{table} TO {role};",
            schema = escape_identifier(schema),
            table = escape_identifier(table),
        )
        .expect("writing to a String cannot fail");
    }
    for (table, privilege, columns) in IDENTITY_ISSUER_PASSWORD_COLUMNS {
        let columns = columns
            .iter()
            .map(|c| escape_identifier(c))
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            sql,
            " GRANT {privilege} ({columns}) ON TABLE identity.{} TO {role};",
            escape_identifier(table)
        )
        .expect("string write");
    }
    write!(sql, " GRANT SELECT, INSERT, UPDATE, DELETE ON identity.password_attempt TO {role}; GRANT EXECUTE ON FUNCTION identity.lock_password_principal(uuid) TO {role};").expect("string write");
    write!(sql, " GRANT DELETE ON identity.password_login TO {role};").expect("string write");
    sql.push_str(&crate::platform_sql::grant_control_policy_execute_sql(
        &role,
    ));
    sql
}
