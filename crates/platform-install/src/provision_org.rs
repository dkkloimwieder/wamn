//! Provision an org's control login and registry row from inside the cluster.

use anyhow::Context as _;
use ring::rand::{SecureRandom as _, SystemRandom};
use wamn_identity_protocol::LoginName;
use wamn_postgres::ConnectionConfig;
use wamn_registry::OrgId;

pub const CONTROL_SECRET: &str = "wamn-control-db";
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

/// Rotate the org login, recording the org before releasing its credential.
pub async fn provision_org(database: &ConnectionConfig, org: &OrgId) -> anyhow::Result<()> {
    let api = wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?;
    let mut connection = wamn_postgres::connect(&database.to_json()?).await?;
    let login = control_login(org, &database.database);
    // Keep concurrent provisioning Jobs ordered through the Secret patch.
    // Dropping the dedicated connection releases this lock on every exit.
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(&login)
        .execute(&mut connection)
        .await
        .context("lock the org login")?;

    let mut entropy = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut entropy)
        .map_err(|_| anyhow::anyhow!("generate the org login password"))?;
    let password = hex::encode(entropy);

    let mut tx = crate::transaction::provisioning_transaction(&mut connection).await?;
    // Bind the password rather than putting it in the client statement text.
    sqlx::query(
        "SELECT set_config('wamn.org_login', $1, true), set_config('wamn.org_password', $2, true)",
    )
    .bind(&login)
    .bind(&password)
    .execute(&mut *tx)
    .await
    .context("bind the org login settings")?;
    sqlx::raw_sql(
        "DO $org_login$
         DECLARE
             login_name text := current_setting('wamn.org_login');
             login_password text := current_setting('wamn.org_password');
         BEGIN
             IF EXISTS (SELECT FROM pg_roles WHERE rolname = login_name) THEN
                 EXECUTE format('ALTER ROLE %I LOGIN PASSWORD %L CONNECTION LIMIT 4',
                                login_name, login_password);
                 EXECUTE format('GRANT wamn_control TO %I', login_name);
             ELSE
                 EXECUTE format('CREATE ROLE %I LOGIN PASSWORD %L CONNECTION LIMIT 4 IN ROLE wamn_control',
                                login_name, login_password);
             END IF;
         END
         $org_login$",
    )
    .execute(&mut *tx)
    .await
    .map_err(|error| OrgRoleError {
        login,
        code: error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .map(std::borrow::Cow::into_owned),
    })?;
    sqlx::query("INSERT INTO registry.orgs (id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(org)
        .execute(&mut *tx)
        .await
        .context("record the org in registry.orgs")?;
    tx.commit().await.context("commit org provisioning")?;

    api.patch_namespaced_secret_data(
        org.as_str(),
        CONTROL_SECRET,
        "password",
        password.as_bytes(),
    )
    .await?;
    Ok(())
}

#[derive(Debug)]
struct OrgRoleError {
    login: LoginName,
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
