//! Provision an org's system database logins, registry row, and local OAuth key.

use anyhow::Context as _;
use ring::rand::{SecureRandom as _, SystemRandom};
use std::fmt;
use wamn_kubernetes::client::KubernetesApi;
use wamn_principal::PlatformComponent;
use wamn_registry::OrgId;
use wamn_role::LoginName;

use crate::{
    org_install::{PLATFORM, PROVISION_JOB},
    platform_event::{Command, EventType, Outcome, record},
    provision_system::connect_system_database,
};

pub const CONTROL_SECRET: &str = "wamn-control-db";
pub const APPLY_SECRET: &str = "wamn-apply-db";
const OAUTH_SECRET: &str = "wamn-oauth";
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
    let random = SystemRandom::new();
    // These limits match the role limits in the platform Poolers.
    let logins = [
        (
            control_login(org, database),
            wamn_role::CONTROL_ROLE,
            8_i32,
            CONTROL_SECRET,
        ),
        (
            apply_login(org, database),
            wamn_role::APPLY_ROLE,
            16_i32,
            APPLY_SECRET,
        ),
    ];
    let mut passwords = Vec::with_capacity(logins.len());
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(connection).await?;
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
    sqlx::query("INSERT INTO registry.org (id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(org.as_str())
        .execute(&mut *tx)
        .await
        .context("record the org in registry.org")?;
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
