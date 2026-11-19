//! Provision an org's system database logins and registry row inside the cluster.

use anyhow::Context as _;
use ring::rand::{SecureRandom as _, SystemRandom};
use wamn_identity_protocol::LoginName;
use wamn_postgres::ConnectionConfig;
use wamn_registry::OrgId;

pub const CONTROL_SECRET: &str = "wamn-control-db";
pub(crate) const APPLY_SECRET: &str = "wamn-apply-db";
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

/// Rotate both logins, recording the org before releasing their credentials.
pub async fn provision_org(database: &ConnectionConfig, org: &OrgId) -> anyhow::Result<()> {
    let api = wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?;
    let mut connection = wamn_postgres::connect(&database.to_json()?).await?;
    let control = control_login(org, &database.database);
    let apply = apply_login(org, &database.database);
    // Keep concurrent provisioning Jobs ordered through the Secret patch.
    // Dropping the dedicated connection releases this lock on every exit.
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(&control)
        .execute(&mut connection)
        .await
        .context("lock the org login")?;

    // These limits match the role limits in the platform Poolers.
    let logins = [
        (control, wamn_role::CONTROL_ROLE, 8_i32, CONTROL_SECRET),
        (apply, wamn_role::APPLY_ROLE, 16_i32, APPLY_SECRET),
    ];
    let mut passwords = Vec::with_capacity(logins.len());
    let mut tx = crate::transaction::provisioning_transaction(&mut connection).await?;
    let random = SystemRandom::new();
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
        .bind(&login)
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
        passwords.push((secret, password));
    }
    sqlx::query("INSERT INTO registry.orgs (id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(org)
        .execute(&mut *tx)
        .await
        .context("record the org in registry.orgs")?;
    tx.commit().await.context("commit org provisioning")?;

    for (secret, password) in passwords {
        api.patch_namespaced_secret_data(org.as_str(), secret, "password", password.as_bytes())
            .await?;
    }
    Ok(())
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
