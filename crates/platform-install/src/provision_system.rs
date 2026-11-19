//! Install or upgrade the control store using the platform SQL Job.

use std::{fmt, io};

use anyhow::Context as _;
use sqlx::PgConnection as Client;
use sqlx::Row as _;

use sqlx::migrate::Migration;
use wamn_provision::migrations::MigrationDatabase;
use wamn_provision::{CONTROL_BOOTSTRAP_SQL, sql};

/// Inputs of one `provision-system` run.
#[derive(Debug)]
pub struct ProvisionSystemRequest {
    /// Public connection settings for the mounted CNPG postgres login.
    pub database: wamn_postgres::ConnectionConfig,
    /// The email domain of the platform principal rows.
    pub platform_domain: wamn_registry::platform_domain::PlatformDomain,
    /// The platform-wide project stream limit.
    pub max_projects: std::num::NonZeroU32,
}

/// Install the control store and record the platform domain and stream limit.
pub async fn provision_system(request: &ProvisionSystemRequest) -> anyhow::Result<()> {
    anyhow::ensure!(
        request.database.username.as_str() == "postgres"
            && request.database.database == wamn_provision::identity_role::IDENTITY_ISSUER_DATABASE,
        "platform SQL requires the system database's postgres login"
    );
    let database = request.database.to_json()?;
    let mut client = connect_system_database(&database)
        .await
        .context("connect to the system database")?;
    let scope = MigrationDatabase::System;
    crate::credentials::wait_role(&mut client, &scope.login(&request.database.database)).await?;
    crate::credentials::wait_group_role(&mut client, &wamn_role::APPLY_ROLE.parse()?).await?;
    let migration_input = scope.configuration(&database)?;

    initialize_system(
        &mut client,
        &migration_input,
        &crate::platform_migrations::embedded_sources(scope),
    )
    .await?;
    install_pooler_auth(&database).await?;
    let mut transaction = crate::transaction::provisioning_transaction(&mut client).await?;
    sqlx::raw_sql("SET LOCAL ROLE wamn_system")
        .execute(&mut *transaction)
        .await
        .context("assume the control owner to finalize system provisioning")?;
    sqlx::query(wamn_registry::sql::PROJECT_STREAM_CAPACITY_LOCK_SQL)
        .execute(&mut *transaction)
        .await
        .context("lock platform project stream capacity")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_provision::identity_role::grants_sql(),
    ))
    .execute(&mut *transaction)
    .await
    .context("grant the identity issuer surface")?;
    sqlx::raw_sql(wamn_provision::apply::APPLY_GRANTS_SQL)
        .execute(&mut *transaction)
        .await
        .context("grant the apply surface")?;
    let first_limit: Option<bool> = sqlx::query_scalar(wamn_registry::sql::UPDATE_PLATFORM_META_SQL)
        .bind(&request.platform_domain)
        .bind(i64::from(request.max_projects.get()))
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

async fn install_pooler_auth(admin_database: &str) -> anyhow::Result<()> {
    use sqlx::Connection as _;

    let options = wamn_postgres::connect_options(admin_database)
        .await?
        .database("postgres");
    let mut admin = Client::connect_with(&options)
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

// The Job starts alongside the CNPG Cluster. Retry refused connections before
// Kubernetes starts another attempt.
const CONNECT_RETRIES: u32 = 30;

async fn connect_system_database(url: &str) -> anyhow::Result<Client> {
    let mut retries = 0;
    crate::credentials::connect_with_retry(url, |error| {
        let refused = error.chain().any(|cause| {
            cause
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::ConnectionRefused)
        });
        if !refused || retries == CONNECT_RETRIES {
            return Err(error);
        }
        retries += 1;
        Ok(())
    })
    .await
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
    let complete = wamn_registry::migrations::history_exists(client).await?;
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
    let mut migration = wamn_postgres::connect(migration_input).await?;
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
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                wamn_provision::system_reader::grants_sql(),
            ))
            .execute(&mut *admin)
            .await?;
            wamn_provision::migration::prepare_history(
                admin,
                wamn_registry::migrations::HISTORY_TABLE,
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinSet;

    #[sqlx::test(migrations = false)]
    async fn initial_connect_retries_only_connection_refusal(
        pool: sqlx::PgPool,
    ) -> anyhow::Result<()> {
        let database = wamn_test_postgres::url(&pool);
        let mut url = url::Url::parse(database.as_str())?;
        let upstream = (url.host_str().unwrap().to_owned(), url.port().unwrap());
        let reserved = TcpListener::bind("127.0.0.1:0").await?;
        let address = reserved.local_addr()?;
        url.set_port(Some(address.port())).unwrap();
        drop(reserved);

        let attempts = Arc::new(AtomicUsize::new(0));
        let mut proxy = JoinSet::new();
        let mut client = {
            let connecting = connect_system_database(url.as_str());
            tokio::pin!(connecting);
            tokio::select! {
                result = &mut connecting => panic!("connect completed before the listener existed: {:?}", result.err()),
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }

            let listener = TcpListener::bind(address).await?;
            proxy.spawn({
                let attempts = Arc::clone(&attempts);
                async move {
                    let mut connections = JoinSet::new();
                    loop {
                        let (mut incoming, _) = listener.accept().await.unwrap();
                        attempts.fetch_add(1, Ordering::Relaxed);
                        let mut outgoing = TcpStream::connect((upstream.0.as_str(), upstream.1))
                            .await
                            .unwrap();
                        connections.spawn(async move {
                            tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await
                        });
                    }
                }
            });
            connecting.await?
        };

        let row = sqlx::query("SELECT current_database()")
            .fetch_one(&mut client)
            .await?;
        assert_eq!(
            row.get::<String, _>(0),
            database.path().trim_start_matches('/')
        );
        drop(client);

        // PostgreSQL accepts the TCP connection but refuses an absent database.
        url.set_path("/unprovisioned_fixture");
        let Err(error) = connect_system_database(url.as_str()).await else {
            panic!("an absent database must refuse the connection");
        };
        assert_eq!(
            error
                .downcast_ref::<sqlx::Error>()
                .unwrap()
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("3D000")
        );
        assert_eq!(
            attempts.load(Ordering::Relaxed),
            2,
            "one TCP connection per PostgreSQL handshake"
        );
        proxy.shutdown().await;
        Ok(())
    }
}
