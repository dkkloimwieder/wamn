//! Install or upgrade the control store using the platform SQL Job.

use std::{fmt, io};
use wamn_registry::migrations::MigrationHistory as _;

use anyhow::Context as _;
use sqlx::PgConnection as Client;
use sqlx::Row as _;

use crate::migrations::MigrationDatabase;
use crate::{CONTROL_BOOTSTRAP_SQL, sql};
use sqlx::migrate::Migration;
use wamn_platform_install::provision_system::{self, SystemBootstrap};
use wamn_postgres::platform_database::{connect_with_retry, wait_group_role, wait_role};
use wamn_registry::platform_domain::PlatformDomain;

/// PostgreSQL operations for the system and org SQL Jobs.
#[derive(Debug)]
pub struct PostgresBootstrap;

impl SystemBootstrap for PostgresBootstrap {
    type Connection = Client;

    async fn connect(&self, input: &str) -> anyhow::Result<Client> {
        connect_system_database(input).await
    }

    async fn prepare_inputs(
        &self,
        client: &mut Client,
        database: &str,
        input: &str,
    ) -> anyhow::Result<(String, String)> {
        let scope = MigrationDatabase::System;
        wait_role(client, &scope.login(database)).await?;
        wait_group_role(client, &wamn_role::APPLY_ROLE.parse()?).await?;
        let migration = scope.configuration(input)?;
        let pooler = wamn_postgres::database_configuration(input, "postgres")
            .context("select postgres for Pooler authentication setup")?;
        Ok((migration, pooler))
    }

    async fn initialize(&self, client: &mut Client, migration_input: &str) -> anyhow::Result<()> {
        initialize_system(
            client,
            migration_input,
            &crate::platform_migrations::embedded_sources(MigrationDatabase::System),
        )
        .await
    }

    async fn install_pooler_auth(&self, pooler_input: &str) -> anyhow::Result<()> {
        install_pooler_auth(pooler_input).await
    }

    async fn finalize(
        &self,
        client: &mut Client,
        platform_domain: &PlatformDomain,
        max_projects: std::num::NonZeroU32,
    ) -> anyhow::Result<()> {
        let mut transaction = crate::transaction::provisioning_transaction(client).await?;
        sqlx::raw_sql("SET LOCAL ROLE wamn_system")
            .execute(&mut *transaction)
            .await
            .context("assume the control owner to finalize system provisioning")?;
        sqlx::query(wamn_postgres::platform_sql::PROJECT_STREAM_CAPACITY_LOCK_SQL)
            .execute(&mut *transaction)
            .await
            .context("lock platform project stream capacity")?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(crate::identity_role::grants_sql()))
            .execute(&mut *transaction)
            .await
            .context("grant the identity issuer surface")?;
        sqlx::raw_sql(crate::apply::APPLY_GRANTS_SQL)
            .execute(&mut *transaction)
            .await
            .context("grant the apply surface")?;
        let first_limit: Option<bool> =
            sqlx::query_scalar(wamn_postgres::platform_sql::UPDATE_PLATFORM_META_SQL)
                .bind(platform_domain.as_str())
                .bind(i64::from(max_projects.get()))
                .fetch_optional(&mut *transaction)
                .await
                .context("record the platform domain and project stream limit")?;
        if first_limit == Some(true) {
            sqlx::raw_sql("ALTER TABLE registry.meta ALTER COLUMN max_projects SET NOT NULL")
                .execute(&mut *transaction)
                .await
                .context("require the installed project stream limit")?;
        }
        transaction.commit().await?;
        Ok(())
    }
}

/// Install the system store using the supplied administrator session and managed logins.
pub async fn install_system(
    client: &mut Client,
    migration_input: &str,
    pooler_input: &str,
    platform_domain: &PlatformDomain,
    max_projects: std::num::NonZeroU32,
) -> anyhow::Result<()> {
    provision_system::install_system(
        &PostgresBootstrap,
        client,
        migration_input,
        pooler_input,
        platform_domain,
        max_projects,
    )
    .await
}

async fn install_pooler_auth(database: &str) -> anyhow::Result<()> {
    use sqlx::Connection as _;

    let mut admin = connect_system_database(database)
        .await
        .context("connect to postgres for Pooler authentication setup")?;
    let mut transaction = admin
        .begin()
        .await
        .context("begin Pooler authentication setup")?;
    sqlx::raw_sql(include_str!("../../../deploy/sql/system-pooler-auth.sql"))
        .execute(&mut *transaction)
        .await
        .context("install Pooler authentication in postgres")?;
    transaction
        .commit()
        .await
        .context("commit Pooler authentication setup")
}

// A Job can start before the database listens or CNPG reloads its pod IP in pg_hba.
const CONNECT_RETRIES: u32 = 30;

pub(crate) async fn connect_system_database(url: &str) -> anyhow::Result<Client> {
    let mut retries = 0;
    connect_with_retry(url, |error| {
        if !retries_connect(url, &error) || retries == CONNECT_RETRIES {
            return Err(error);
        }
        retries += 1;
        Ok(())
    })
    .await
}

/// Retry a refused connection, and the pg_hba refusal of a configuration input.
fn retries_connect(url: &str, error: &anyhow::Error) -> bool {
    let refused = error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::ConnectionRefused)
    });
    let awaiting_hba = url.starts_with('{')
        && error
            .downcast_ref::<sqlx::Error>()
            .and_then(sqlx::Error::as_database_error)
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref()
            == Some("28000");
    refused || awaiting_hba
}

/// Database state recorded by the atomic system bootstrap.
#[derive(Debug, PartialEq, Eq)]
pub enum SystemState {
    /// No control-store schema exists.
    Absent,
    /// The current baseline is recorded.
    Complete,
    /// A control-store schema exists without a baseline record.
    Incomplete,
}

impl fmt::Display for SystemState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => formatter.write_str("system initialization absent"),
            Self::Complete => formatter.write_str("system initialization complete"),
            Self::Incomplete => formatter.write_str(
                "incomplete system initialization: no SQLx history. Restore a verified system database backup, then rerun provision-system. Do not replay bootstrap or reset the database",
            ),

        }
    }
}

/// Read the completion record without changing schemas, grants, or data.
pub async fn system_state(client: &mut sqlx::PgConnection) -> anyhow::Result<SystemState> {
    // Development resets retain the shared history schema. It does not identify
    // a control-store installation on its own.
    let present: bool = sqlx::query(
        "SELECT EXISTS (SELECT FROM pg_namespace WHERE nspname IN \
         ('registry', 'provisioning', 'identity', 'catalog', 'wamn_run', 'wamn_authority'))",
    )
    .fetch_one(&mut *client)
    .await
    .context("look for control-store schemas")?
    .get(0);
    if !present {
        return Ok(SystemState::Absent);
    }
    let complete = wamn_postgres::Registry::new(client)
        .history_exists()
        .await?;
    Ok(if complete {
        SystemState::Complete
    } else {
        SystemState::Incomplete
    })
}

/// Ensure the system baseline exists, then apply pending migrations from the selected image.
pub async fn initialize_system(
    admin: &mut Client,
    migration_input: &str,
    sources: &[Migration],
) -> anyhow::Result<()> {
    install_control_store(admin).await?;
    let mut migration = connect_system_database(migration_input)
        .await
        .context("connect as the system migration login")?;
    crate::platform_migrations::apply_target(&mut migration, MigrationDatabase::System, sources)
        .await?;
    Ok(())
}

/// Install the control store and baseline atomically. Return true only for a new installation.
pub async fn install_control_store(admin: &mut Client) -> anyhow::Result<bool> {
    sqlx::raw_sql("BEGIN")
        .execute(&mut *admin)
        .await
        .map(drop)
        .context("begin system initialization")?;
    let result = async {
        sqlx::raw_sql("SELECT pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'))")
            .execute(&mut *admin)
            .await
            .map(drop)
            .context("lock system initialization")?;
        let absent = match system_state(admin).await? {
            SystemState::Absent => true,
            SystemState::Complete => false,
            state @ SystemState::Incomplete => anyhow::bail!("{state}"),
        };

        sqlx::raw_sql(sqlx::AssertSqlSafe(sql::ensure_system_roles_sql()))
            .execute(&mut *admin)
            .await?;
        if absent {
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                ("DO $$ BEGIN EXECUTE format('ALTER DATABASE %I OWNER TO wamn_system', \
                                                current_database()); END $$;")
                    .to_string(),
            ))
            .execute(&mut *admin)
            .await
            .map(drop)
            .context("give wamn_system the system database")?;
        }
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (sql::revoke_public_connect_base_grants_sql()).to_string(),
        ))
        .execute(&mut *admin)
        .await
        .map(drop)
        .context("converge the cluster PUBLIC CONNECT base grants")?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            ("DO $$ BEGIN EXECUTE format(\
                   'REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()); END $$;")
                .to_string(),
        ))
        .execute(&mut *admin)
        .await
        .map(drop)
        .context("converge the system database PUBLIC TEMPORARY base grants")?;
        if absent {
            sqlx::raw_sql("SET LOCAL ROLE wamn_system")
                .execute(&mut *admin)
                .await
                .map(drop)
                .context("assume the control owner")?;
            for stage in CONTROL_BOOTSTRAP_SQL {
                sqlx::raw_sql(sqlx::AssertSqlSafe((stage).to_string()))
                    .execute(&mut *admin)
                    .await
                    .map(drop)
                    .context("install the control store")?;
            }
            sqlx::raw_sql(sqlx::AssertSqlSafe(crate::system_reader::grants_sql()))
                .execute(&mut *admin)
                .await?;
            crate::migration::prepare_history(
                admin,
                wamn_postgres::REGISTRY_HISTORY_TABLE,
                &wamn_role::SYSTEM_ROLE.parse()?,
            )
            .await?;
        }
        sqlx::raw_sql("COMMIT")
            .execute(&mut *admin)
            .await
            .map(drop)
            .context("commit system initialization")?;
        Ok(absent)
    }
    .await;
    match result {
        Ok(initialized) => Ok(initialized),
        Err(error) => {
            sqlx::raw_sql("ROLLBACK")
                .execute(&mut *admin)
                .await
                .map(drop)
                .with_context(|| format!("roll back system initialization after {error:#}"))?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "postgresql://wamn@127.0.0.1:5432/system";

    fn io_error(type_: io::ErrorKind) -> anyhow::Error {
        sqlx::Error::Io(io::Error::from(type_)).into()
    }

    #[test]
    fn initial_connect_retries_only_connection_refusal() {
        assert!(retries_connect(
            URL,
            &io_error(io::ErrorKind::ConnectionRefused)
        ));
        for type_ in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::PermissionDenied,
        ] {
            assert!(!retries_connect(URL, &io_error(type_)), "{type_:?}");
        }
        assert!(!retries_connect(
            URL,
            &sqlx::Error::Protocol("unexpected message".into()).into()
        ));
    }
}
