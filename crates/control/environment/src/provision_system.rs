//! The `provision-system` subcommand: install the control store into an empty
//! system database and record the platform domain of the deployment.
//!
//! The database and its owner role `wamn_system` exist before this runs. On
//! CloudNativePG, the `initdb` bootstrap of the cluster creates both. The
//! verb connects as the superuser, installs [`CONTROL_BOOTSTRAP_SQL`] as
//! `wamn_system`, closes the PUBLIC minimums, creates `wamn_apply`, and writes
//! `registry.meta.platform_domain`. A complete installation needs no replay.
//! Partial initialization requires explicit repair. Interrupted migrations resume
//! through `wamn platform schema apply --target system --chart <prepared-chart>`.
//!
use std::time::Duration;
use std::{fmt, io};

use anyhow::Context as _;
use sqlx::PgConnection as Client;
use sqlx::Row as _;

use sqlx::migrate::Migration;
use wamn_control_provision::migrations::MigrationDatabase;
use wamn_control_provision::{CONTROL_BOOTSTRAP_SQL, sql, validate_platform_domain};

/// Inputs of one `provision-system` run.
#[derive(Debug)]
pub struct ProvisionSystemRequest {
    /// Superuser URL of the system database.
    pub system_database_url: String,
    /// The platform migration login's actual credential for explicit PostgreSQL access.
    pub migration_database_url: Option<String>,
    /// The email domain of the platform principal rows.
    pub platform_domain: String,
}

/// Install the control store and record the platform domain.
pub async fn provision_system(request: &ProvisionSystemRequest) -> anyhow::Result<()> {
    validate_platform_domain(&request.platform_domain)
        .context("make sure of the platform domain")?;
    let mut client = connect_system_database(&request.system_database_url)
        .await
        .context("connect to the system database")?;

    let result = async {
        let migration_input = match request.migration_database_url.as_deref() {
            Some(input) => input.to_owned(),
            None => {
                crate::provision_project_env::workload::provision_platform_migration(
                    &request.system_database_url,
                    MigrationDatabase::System,
                )
                .await?
            }
        };
        initialize_system(
            &mut client,
            &migration_input,
            &crate::platform_migrations::embedded_sources(MigrationDatabase::System),
        )
        .await?;
        set_platform_domain(&mut client, &request.platform_domain).await?;
        Ok(())
    }
    .await;
    drop(client);
    result
}

// Allow 30 seconds for the CNPG service to accept TCP after Cluster Ready,
// well inside the platform Job's 15-minute deadline. Never replay initialization.
const CONNECT_RETRIES: u32 = 30;
const CONNECT_RETRY_DELAY: Duration = Duration::from_secs(1);

async fn connect_system_database(url: &str) -> anyhow::Result<Client> {
    let mut retries = 0;
    loop {
        match wamn_postgres::connect(url).await {
            Ok(connection) => return Ok(connection),
            Err(error) => {
                let refused = error.chain().any(|cause| {
                    cause
                        .downcast_ref::<io::Error>()
                        .is_some_and(|error| error.kind() == io::ErrorKind::ConnectionRefused)
                });
                if !refused || retries == CONNECT_RETRIES {
                    return Err(error);
                }
                retries += 1;
                tokio::time::sleep(CONNECT_RETRY_DELAY).await;
            }
        }
    }
}

/// Record the deployment domain with the provisioning actor.
pub async fn set_platform_domain(client: &mut Client, domain: &str) -> anyhow::Result<()> {
    let mut transaction = crate::provision_project_env::provisioning_transaction(client).await?;
    sqlx::query(wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_PROVISION_SYSTEM_SET_PLATFORM_DOMAIN)
        .bind(domain)
        .execute(&mut *transaction)
        .await
        .map(|result| result.rows_affected())
        .context("record the platform domain")?;
    transaction.commit().await?;
    Ok(())
}

/// Reconcile the apply login after the SQL schema installer completes.
pub async fn provision_apply(input: &str) -> anyhow::Result<()> {
    let mut connection = wamn_postgres::connect(input).await?;
    let api = wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?;
    let options = wamn_postgres::connect_options(input).await?;
    let cluster = options
        .get_host()
        .split('.')
        .next()
        .context("database host is absent")?
        .trim_end_matches("-rw");
    let kubeconfig = std::env::var_os("KUBECONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let role = wamn_control_provision::connection::managed_role(
        "wamn_apply".into(),
        "wamn_db_owner",
        "wamn-apply".into(),
    );
    crate::provision_project_env::workload::reconcile(
        &kubeconfig,
        "",
        &api.namespace,
        cluster,
        &role,
    )
    .await?;
    crate::provision_project_env::workload::wait_role(&mut connection, &role.name).await?;
    sqlx::raw_sql(wamn_control_provision::apply::ensure_apply_role_sql())
        .execute(&mut connection)
        .await?;
    sqlx::raw_sql(wamn_control_provision::apply::APPLY_GRANTS_SQL)
        .execute(&mut connection)
        .await?;
    let config = crate::provision_project_env::workload::configuration(
        &api.namespace,
        cluster,
        options.get_host().into(),
        options.get_port(),
        "wamn_system".into(),
        role.name,
        role.password_secret.name,
    );
    let value = serde_json::json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"wamn-apply","namespace":api.namespace},"data":{"apply.json":config.to_json()?}});
    wamn_kubernetes::kubectl::apply(&kubeconfig, "", &api.namespace, &value).await?;
    Ok(())
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
    let complete: bool =
        sqlx::query_scalar("SELECT to_regclass('registry._sqlx_migrations') IS NOT NULL")
            .fetch_one(client)
            .await?;
    Ok(if complete {
        SystemState::Complete
    } else {
        SystemState::Incomplete
    })
}

/// Initialize an empty system database, then run the selected image's migrations.
/// Existing initialization never runs pending files on platform reapply.
pub async fn initialize_system(
    admin: &mut Client,
    migration_input: &str,
    sources: &[Migration],
) -> anyhow::Result<()> {
    if install_control_store(admin).await? {
        let mut migration = wamn_postgres::connect(migration_input).await?;
        crate::platform_migrations::apply_reported(
            &mut migration,
            crate::platform_migrations::MigrationTarget::System,
            sources,
        )
        .await?;
    }
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
        match system_state(admin).await? {
            SystemState::Absent => {}
            SystemState::Complete => {
                sqlx::raw_sql("COMMIT")
                    .execute(&mut *admin)
                    .await
                    .map(drop)
                    .context("finish system initialization read")?;
                return Ok(false);
            }
            state @ SystemState::Incomplete => anyhow::bail!("{state}"),
        }

        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (sql::ensure_db_owner_role_sql()).to_string(),
        ))
        .execute(&mut *admin)
        .await
        .map(drop)
        .context("ensure the database-owner role that the record history grants name")?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            ("DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
                                            current_database()); END $$;")
                .to_string(),
        ))
        .execute(&mut *admin)
        .await
        .map(drop)
        .context("give wamn_system the system database")?;
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
        wamn_control_provision::migration::prepare_history(
            admin,
            "registry._sqlx_migrations",
            "wamn_system",
        )
        .await?;
        sqlx::raw_sql("RESET ROLE")
            .execute(&mut *admin)
            .await
            .map(drop)
            .context("release the control owner before cluster ACL convergence")?;
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
        sqlx::raw_sql("COMMIT")
            .execute(&mut *admin)
            .await
            .map(drop)
            .context("commit system initialization")?;
        Ok(true)
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
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinSet;

    #[tokio::test]
    async fn initial_connect_retries_only_connection_refusal() -> anyhow::Result<()> {
        let database = wamn_test_postgres::database();
        let mut url = url::Url::parse(database.url())?;
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
        assert_eq!(row.get::<String, _>(0), database.name());
        drop(client);

        // PostgreSQL accepts the TCP connection but refuses an absent database.
        url.set_path(&format!("/{}_absent", database.name()));
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
