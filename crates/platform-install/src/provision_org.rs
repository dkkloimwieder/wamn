//! Provision an org's system database logins, registry row, and local OAuth key.

use anyhow::Context as _;
use k8s_openapi::api::core::v1::Secret;
use kube::Api;
use ring::rand::{SecureRandom as _, SystemRandom};
use std::fmt;
use std::path::Path;
use wamn_identity_protocol::LoginName;
use wamn_kubernetes::client::KubernetesApi;
use wamn_postgres::ConnectionConfig;
use wamn_registry::OrgId;

pub const CONTROL_SECRET: &str = "wamn-control-db";
pub(crate) const APPLY_SECRET: &str = "wamn-apply-db";
const OAUTH_SECRET: &str = "wamn-oauth";
pub(crate) const ORG_INPUTS_DIR: &str = "/etc/wamn/org-inputs";
pub(crate) const OAUTH_ENCRYPTION_INPUT: &str = "oauthEncryption";
pub const SQL_ACCOUNT: &str = "wamn-org-sql";
pub const INVITATION_ACCOUNT: &str = "wamn-org-invitation";
pub const CONTROL_AUDIENCE: &str = "wamn-control";
pub const CONTROL_TOKEN_PATH: &str = "/var/run/secrets/wamn-control/token";
pub const FIRST_OWNER_PATH: &str = "/internal/first-owner";

pub(crate) fn control_login(org: &OrgId, database: &str) -> LoginName {
    wamn_registry::name::credential_login(
        wamn_role::CONTROL_ROLE,
        &wamn_registry::control_identity_hash(org, database),
    )
}

/// The system database login used for this org's operations.
pub fn apply_login(org: &OrgId, database: &str) -> LoginName {
    wamn_registry::name::credential_login(
        wamn_role::APPLY_ROLE,
        &wamn_registry::apply_identity_hash(org, database),
    )
}

/// Initialize the local OAuth key if needed, record the org, and rotate both logins.
pub async fn provision_org(database: &ConnectionConfig, org: &OrgId) -> anyhow::Result<()> {
    let api = KubernetesApi::in_cluster()?;
    let oauth_encryption = tokio::fs::read(Path::new(ORG_INPUTS_DIR).join(OAUTH_ENCRYPTION_INPUT))
        .await
        .context("read the platform OAuth encryption configuration")?;
    let oauth_encryption: serde_json::Value = serde_json::from_slice(&oauth_encryption)
        .context("decode the platform OAuth encryption configuration")?;
    let mut connection = crate::provision_system::connect_system_database(&database.to_json()?)
        .await
        .context("connect to the system database for org provisioning")?;
    let control = control_login(org, &database.database);
    // Keep concurrent provisioning Jobs ordered through the Secret writes.
    // Dropping the dedicated connection releases this lock on every exit.
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(control.as_str())
        .execute(&mut connection)
        .await
        .context("lock the org login")?;

    if oauth_encryption["provider"] == "local" {
        let secrets = Api::<Secret>::namespaced(api.client(), org.as_str());
        let secret = tokio::time::timeout(
            wamn_kubernetes::client::REQUEST_TIMEOUT,
            secrets.get(OAUTH_SECRET),
        )
        .await??;
        if !secret.data.is_some_and(|data| data.contains_key("key")) {
            // The host's envelope reader requires 32 raw bytes.
            let mut key = [0_u8; 32];
            SystemRandom::new()
                .fill(&mut key)
                .map_err(|_| anyhow::anyhow!("generate the OAuth encryption key for org {org}"))?;
            api.patch_namespaced_secret_data(org.as_str(), OAUTH_SECRET, "key", &key)
                .await?;
        }
    }

    for login in create_org_logins(&mut connection, org, &database.database).await? {
        api.patch_namespaced_secret_data(
            org.as_str(),
            login.secret,
            "password",
            login.password.as_bytes(),
        )
        .await?;
    }
    Ok(())
}

/// One org login and the password that provisioning gave it.
pub struct OrgLogin {
    pub login: LoginName,
    /// The org Secret that carries the password.
    pub secret: &'static str,
    pub password: String,
}

/// Names the login only: the password must not reach a log.
impl fmt::Debug for OrgLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrgLogin")
            .field("login", &self.login)
            .field("secret", &self.secret)
            .finish_non_exhaustive()
    }
}

/// Create or rotate the org's control and apply logins and record the org, in one transaction.
pub async fn create_org_logins(
    connection: &mut sqlx::PgConnection,
    org: &OrgId,
    database: &str,
) -> anyhow::Result<Vec<OrgLogin>> {
    let random = SystemRandom::new();
    // These limits match the role limits in the platform Poolers.
    let logins = [
        (control_login(org, database), wamn_role::CONTROL_ROLE, 8_i32, CONTROL_SECRET),
        (apply_login(org, database), wamn_role::APPLY_ROLE, 16_i32, APPLY_SECRET),
    ];
    let mut passwords = Vec::with_capacity(logins.len());
    let mut tx = crate::transaction::provisioning_transaction(connection).await?;
    for (login, group, limit, secret) in logins {
        let mut entropy = [0_u8; 32];
        random
            .fill(&mut entropy)
            .map_err(|_| anyhow::anyhow!("generate the org login password"))?;
        let password = hex::encode(entropy);
        let role_error = |error: sqlx::Error| OrgRoleError {
            login: login.as_str().to_owned(),
            code: error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .map(std::borrow::Cow::into_owned),
        };
        // Bind the password so the client statement text never contains it.
        sqlx::query(
            "SELECT set_config('wamn.org_login', $1, true), set_config('wamn.org_password', $2, true),
                    set_config('wamn.org_group', $3, true), set_config('wamn.org_connection_limit', $4::int::text, true)",
        )
        .bind(login.as_str())
        .bind(&password)
        .bind(group)
        .bind(limit)
        .execute(&mut *tx)
        .await
        .map_err(role_error)?;
        sqlx::raw_sql(
            "DO $org_login$
             DECLARE
                 login_name text := current_setting('wamn.org_login');
                 login_password text := current_setting('wamn.org_password');
                 login_group text := current_setting('wamn.org_group');
                 connection_limit integer := current_setting('wamn.org_connection_limit')::integer;
             BEGIN
                 IF EXISTS (SELECT FROM pg_roles WHERE rolname = login_name) THEN
                     EXECUTE format('ALTER ROLE %I LOGIN PASSWORD %L CONNECTION LIMIT %s',
                                    login_name, login_password, connection_limit);
                     EXECUTE format('GRANT %I TO %I', login_group, login_name);
                 ELSE
                     EXECUTE format('CREATE ROLE %I LOGIN PASSWORD %L CONNECTION LIMIT %s IN ROLE %I',
                                    login_name, login_password, connection_limit, login_group);
                 END IF;
             END
             $org_login$",
        )
        .execute(&mut *tx)
        .await
        .map_err(role_error)?;
        passwords.push(OrgLogin {
            login,
            secret,
            password,
        });
    }
    sqlx::query("INSERT INTO registry.orgs (id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(org.as_str())
        .execute(&mut *tx)
        .await
        .context("record the org in registry.orgs")?;
    tx.commit().await.context("commit org provisioning")?;
    Ok(passwords)
}

#[derive(Debug)]
struct OrgRoleError {
    login: String,
    // PostgreSQL error details can contain the dynamic SQL and password.
    code: Option<String>,
}

impl std::fmt::Display for OrgRoleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "provision org login {}", self.login)?;
        if let Some(code) = &self.code {
            write!(formatter, " (SQLSTATE {code})")?;
        }
        Ok(())
    }
}

impl std::error::Error for OrgRoleError {}
