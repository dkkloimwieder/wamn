//! Provision an org's system database logins, registry row, and local OAuth key.

use anyhow::Context as _;
use ring::rand::{SecureRandom as _, SystemRandom};
use std::fmt;
use wamn_kubernetes::client::KubernetesApi;
use wamn_principal::PlatformComponent;
use wamn_registry::{EnvName, OrgId};
use wamn_role::LoginName;

use crate::{
    org_install::{PLATFORM, PROVISION_JOB},
    platform_event::{Command, EventType, Outcome, record},
    provision_system::connect_system_database,
};

pub const CONTROL_SECRET: &str = "wamn-control-db";
pub const APPLY_SECRET: &str = "wamn-apply-db";
pub(crate) const OAUTH_SECRET: &str = "wamn-oauth";
pub const ORG_INPUTS_DIR: &str = "/etc/wamn/org-inputs";
pub const OAUTH_ENCRYPTION_INPUT: &str = "oauthEncryption";
pub const SQL_ACCOUNT: &str = "wamn-org-sql";
pub const INVITATION_ACCOUNT: &str = "wamn-org-invitation";
pub const CONTROL_AUDIENCE: &str = "wamn-control";
pub const CONTROL_TOKEN_PATH: &str = "/var/run/secrets/wamn-control/token";
pub const FIRST_OWNER_PATH: &str = "/internal/first-owner";

pub fn control_login(org: &OrgId, database: &str) -> LoginName {
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

/// Provision the org's credentials and record the Job's start and result.
///
/// # Errors
///
/// Returns an error if database access, journal writes or Secret provisioning fail.
/// A Job setup error is recorded after the system database can be reached.
pub async fn provision_org(
    database: &str,
    org: &OrgId,
    setup: anyhow::Result<(KubernetesApi, serde_json::Value)>,
) -> anyhow::Result<()> {
    let (_, database_name) = wamn_postgres::identity(database)?;
    let mut connection = connect_system_database(database)
        .await
        .context("connect to the system database for org provisioning")?;
    let command = Command {
        id: sqlx::query_scalar("SELECT gen_random_uuid()")
            .fetch_one(&mut connection)
            .await
            .context("allocate the org provisioning command ID")?,
        event_type: EventType::OrgCreate,
        principal_id: PlatformComponent::Provisioning.principal_id(),
        env: None,
        project: None,
        app: None,
        package_digest: None,
        helm_revision: None,
    };
    let job = Some((PLATFORM, PROVISION_JOB));
    record(&mut connection, org, &command, Outcome::Started, job).await?;
    let result = match setup {
        Ok((secrets, oauth_encryption)) => {
            provision(
                &mut connection,
                &database_name,
                org,
                &oauth_encryption,
                &secrets,
            )
            .await
        }
        Err(error) => Err(error),
    };
    let outcome = if result.is_ok() {
        Outcome::Succeeded
    } else {
        Outcome::Failed("The org provisioning Job failed.")
    };
    let recorded = record(&mut connection, org, &command, outcome, job).await;
    match (result, recorded) {
        (Ok(()), recorded) => recorded,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(journal)) => Err(error.context(format!(
            "also failed to record the org provisioning outcome: {journal:#}"
        ))),
    }
}

/// The platform operator's requested org activity.
#[derive(Debug, Clone, Copy)]
pub enum OrgActivity {
    Active,
    Inactive,
}

/// Update the org's admission flag using the platform SQL credential.
///
/// # Errors
/// Returns an error if the org does not exist or the database write fails.
pub async fn set_activity(
    database: &str,
    org: &OrgId,
    activity: OrgActivity,
) -> anyhow::Result<()> {
    let mut connection = connect_system_database(database).await?;
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut connection).await?;
    let result = sqlx::query("UPDATE registry.org SET active=$2 WHERE id=$1")
        .bind(org.as_str())
        .bind(matches!(activity, OrgActivity::Active))
        .execute(&mut *tx)
        .await
        .with_context(|| format!("set activity for org {org}"))?;
    anyhow::ensure!(result.rows_affected() != 0, "org {org} is not registered");
    tx.commit().await.context("commit org activity")
}

async fn provision(
    connection: &mut sqlx::PgConnection,
    database: &str,
    org: &OrgId,
    oauth_encryption: &serde_json::Value,
    secrets: &KubernetesApi,
) -> anyhow::Result<()> {
    let control = control_login(org, database);
    crate::platform_database::lock_org_login(connection, &control).await?;
    if oauth_encryption["provider"] == "local"
        && !secrets
            .secret_contains_key(org.as_str(), OAUTH_SECRET, "key")
            .await?
    {
        // The host's envelope reader requires 32 raw bytes.
        let mut key = [0_u8; 32];
        SystemRandom::new()
            .fill(&mut key)
            .map_err(|_| anyhow::anyhow!("generate the OAuth encryption key for org {org}"))?;
        secrets
            .patch_namespaced_secret_data(org.as_str(), OAUTH_SECRET, "key", &key)
            .await?;
    }

    for login in create_org_logins(connection, org, database).await? {
        secrets
            .patch_namespaced_secret_data(
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
    let logins = [
        (
            control_login(org, database),
            wamn_role::CONTROL_ROLE,
            CONTROL_SECRET,
        ),
        (
            apply_login(org, database),
            wamn_role::APPLY_ROLE,
            APPLY_SECRET,
        ),
    ];
    let mut passwords = Vec::with_capacity(logins.len());
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(connection).await?;
    for (login, group, secret) in logins {
        let password = login_password()?;
        // -1: the Poolers cap total server connections, not each login (E5.6).
        write_login(&mut tx, &login, &password, group, -1).await?;
        passwords.push(OrgLogin {
            login,
            secret,
            password,
        });
    }
    sqlx::query("INSERT INTO registry.org (id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(org.as_str())
        .execute(&mut *tx)
        .await
        .context("record the org in registry.org")?;
    tx.commit().await.context("commit org provisioning")?;
    Ok(passwords)
}

/// Credentials for an environment's system database worker.
pub struct EnvWorkerLogin {
    /// The database username of the worker.
    pub login: LoginName,
    /// The password to publish in the worker's Secret.
    pub password: String,
    pub(crate) namespace: String,
    pub(crate) quota: crate::env_install::Quota,
}

impl fmt::Debug for EnvWorkerLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvWorkerLogin")
            .field("login", &self.login)
            .finish_non_exhaustive()
    }
}

/// Record the env mapping and quota and create or rotate its worker login atomically.
///
/// # Errors
///
/// Returns an error if the org is unavailable, the name does not fit, or a database write fails.
/// Publish the credentials after this function commits. Serialize provisioning and
/// publication per environment so a concurrent retry cannot publish a stale password.
pub async fn create_env_worker_login(
    connection: &mut sqlx::PgConnection,
    org: &OrgId,
    env: &EnvName,
) -> anyhow::Result<EnvWorkerLogin> {
    let password = login_password()?;
    let mut transaction =
        wamn_postgres::provisioning_transaction::provisioning_transaction(connection).await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(
             registry.env_worker_login($1, $2, current_database()), 0))",
    )
    .bind(org.as_str())
    .bind(env.as_str())
    .execute(&mut *transaction)
    .await
    .context("lock the env worker login")?;
    let login = sqlx::query_scalar::<_, String>("SELECT registry.provision_env($1, $2)")
        .bind(org.as_str())
        .bind(env.as_str())
        .fetch_one(&mut *transaction)
        .await
        .with_context(|| format!("record env {org}/{env}"))?
        .parse::<LoginName>()?;
    write_login(
        &mut transaction,
        &login,
        &password,
        wamn_role::APPLY_ROLE,
        -1,
    )
    .await?;
    sqlx::query(
        "INSERT INTO registry.env_quota (org, env) VALUES ($1, $2)
         ON CONFLICT (org, env) DO NOTHING",
    )
    .bind(org.as_str())
    .bind(env.as_str())
    .execute(&mut *transaction)
    .await
    .context("initialize the env quota")?;
    let (namespace, quota): (String, serde_json::Value) = sqlx::query_as(
        "SELECT e.namespace, to_jsonb(q)
           FROM registry.env e JOIN registry.env_quota q USING (org, env)
          WHERE e.org=$1 AND e.env=$2",
    )
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_one(&mut *transaction)
    .await
    .context("read the env provisioning data")?;
    let quota = serde_json::from_value(quota).context("decode the env quota")?;
    transaction
        .commit()
        .await
        .context("commit env provisioning")?;
    Ok(EnvWorkerLogin {
        login,
        password,
        namespace,
        quota,
    })
}

fn login_password() -> anyhow::Result<String> {
    let mut entropy = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut entropy)
        .map_err(|_| anyhow::anyhow!("generate the system database login password"))?;
    Ok(hex::encode(entropy))
}

async fn write_login(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    login: &LoginName,
    password: &str,
    group: &str,
    limit: i32,
) -> Result<(), SystemRoleError> {
    let role_error = |error: sqlx::Error| SystemRoleError {
        login: login.as_str().to_owned(),
        code: error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .map(std::borrow::Cow::into_owned),
    };
    // Bind the password so the client statement text never contains it.
    sqlx::query(
        "SELECT set_config('wamn.provision_login', $1, true), set_config('wamn.provision_password', $2, true),
                set_config('wamn.provision_group', $3, true), set_config('wamn.provision_connection_limit', $4::int::text, true)",
    )
    .bind(login.as_str())
    .bind(password)
    .bind(group)
    .bind(limit)
    .execute(&mut **transaction)
    .await
    .map_err(role_error)?;
    sqlx::raw_sql(
        "DO $provision_login$
         DECLARE
             login_name text := current_setting('wamn.provision_login');
             login_password text := current_setting('wamn.provision_password');
             login_group text := current_setting('wamn.provision_group');
             connection_limit integer := current_setting('wamn.provision_connection_limit')::integer;
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
         $provision_login$",
    )
    .execute(&mut **transaction)
    .await
    .map(drop)
    .map_err(role_error)
}

#[derive(Debug)]
struct SystemRoleError {
    login: String,
    // PostgreSQL error details can contain the dynamic SQL and password.
    code: Option<String>,
}

impl std::fmt::Display for SystemRoleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "provision system database login {}", self.login)?;
        if let Some(code) = &self.code {
            write!(formatter, " (SQLSTATE {code})")?;
        }
        Ok(())
    }
}

impl std::error::Error for SystemRoleError {}
