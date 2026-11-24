//! Rotate the org database logins using the provisioning authority.

use anyhow::Context as _;
use ring::rand::{SecureRandom as _, SystemRandom};
use wamn_platform_install::provision_org::{
    APPLY_SECRET, CONTROL_SECRET, LockedOrg, OrgDatabase, OrgLogin, apply_login, control_login,
};
use wamn_postgres::database::ConnectionConfig;
use wamn_registry::OrgId;

use crate::provision_system::{PostgresBootstrap, connect_system_database};

/// Dedicated Postgres session holding the org advisory lock.
#[derive(Debug)]
pub struct LockedOrgDatabase<'a> {
    connection: sqlx::PgConnection,
    org: &'a OrgId,
    database: &'a str,
}

impl OrgDatabase for PostgresBootstrap {
    type Locked<'a> = LockedOrgDatabase<'a>;

    async fn lock_org<'a>(
        &'a self,
        database: &'a ConnectionConfig,
        org: &'a OrgId,
    ) -> anyhow::Result<Self::Locked<'a>> {
        let mut connection = connect_system_database(&database.to_json()?)
            .await
            .context("connect to the system database for org provisioning")?;
        let control = control_login(org, &database.database);
        wamn_postgres::platform_database::lock_org_login(&mut connection, &control).await?;
        Ok(LockedOrgDatabase {
            connection,
            org,
            database: &database.database,
        })
    }
}

impl LockedOrg for LockedOrgDatabase<'_> {
    async fn rotate_logins(&mut self) -> anyhow::Result<Vec<OrgLogin>> {
        create_org_logins(&mut self.connection, self.org, self.database).await
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
