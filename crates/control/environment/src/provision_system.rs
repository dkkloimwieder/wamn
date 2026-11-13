//! The `provision-system` subcommand: install the control store into an empty
//! system database and record the platform domain of the deployment.
//!
//! The database and its owner role `wamn_system` exist before this runs. On
//! CloudNativePG, the `initdb` bootstrap of the cluster creates both. The
//! verb connects as the superuser, installs [`CONTROL_BOOTSTRAP_SQL`] as
//! `wamn_system`, closes the PUBLIC floors, creates the login
//! `wamn_provisioner` of the provisioning worker and `wamn_apply`, and writes
//! `registry.meta.platform_domain`. A complete installation needs no replay.
//! Partial initialization requires explicit repair. Interrupted migrations resume
//! through `wamn platform schema apply --target system --chart <prepared-chart>`.
//!
//! With the emit flags, the verb installs nothing. It writes the Secret
//! manifest of `wamn_provisioner` and the role statement that sets its
//! password, see [`emit_provisioner_credential`].

pub(super) mod plan;
use plan::scram_sha256_verifier;

use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use base64::engine::general_purpose::STANDARD;
use ring::rand::{SecureRandom as _, SystemRandom};
use ring::{digest, hmac, pbkdf2};
use serde_json::json;
use tokio_postgres::{Client, Connection, GenericClient, NoTls, Socket, tls::NoTlsStream};
use url::Url;
use wamn_control_provision::provisioner::{PROVISIONER_ROLE, provisioner_statement_sql};

use crate::provision_project_env::{
    ensure_distinct_secret_paths, ensure_secret_path, write_secret_json,
};
use wamn_control_provision::schema_migrations::MigrationDatabase;
use wamn_control_provision::{CONTROL_BOOTSTRAP_SQL, sql, validate_platform_domain};
use wamn_schema_control::MigrationSource;

/// Inputs of one `provision-system` run.
#[derive(Debug)]
pub struct ProvisionSystemRequest {
    /// Superuser URL of the system database.
    pub system_database_url: String,
    /// The email domain of the platform principal rows.
    pub platform_domain: String,
}

/// Install the control store and record the platform domain.
pub async fn provision_system(request: &ProvisionSystemRequest) -> anyhow::Result<()> {
    validate_platform_domain(&request.platform_domain).context("check the platform domain")?;
    let (mut client, connection) = connect_system_database(&request.system_database_url)
        .await
        .context("connect to the system database")?;
    let connection = tokio::spawn(connection);
    let result = async {
        initialize_system(
            &mut client,
            &crate::platform_migrations::embedded_sources(MigrationDatabase::System),
        )
        .await?;
        set_platform_domain(&mut client, &request.platform_domain).await?;
        Ok(())
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

// Allow 30 seconds for the CNPG service to accept TCP after Cluster Ready,
// well inside the platform Job's 15-minute deadline. Never replay initialization.
const CONNECT_RETRIES: u32 = 30;
const CONNECT_RETRY_DELAY: Duration = Duration::from_secs(1);

async fn connect_system_database(
    url: &str,
) -> anyhow::Result<(Client, Connection<Socket, NoTlsStream>)> {
    let mut retries = 0;
    loop {
        match tokio_postgres::connect(url, NoTls).await {
            Ok(connection) => return Ok(connection),
            Err(error) => {
                let error = anyhow::Error::new(error);
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
    let transaction = crate::provision_project_env::provisioning_transaction(client).await?;
    transaction
        .execute(
            "UPDATE registry.meta SET platform_domain = $1 WHERE platform_domain IS DISTINCT FROM $1",
            &[&domain],
        )
        .await
        .context("record the platform domain")?;
    transaction.commit().await?;
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
    /// The recorded schema belongs to a different artifact.
    BaselineMismatch { installed: String, expected: String },
}

impl fmt::Display for SystemState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => formatter.write_str("system initialization absent"),
            Self::Complete => formatter.write_str("system initialization complete"),
            Self::Incomplete => formatter.write_str(
                "incomplete system initialization: no system baseline record. Restore a verified system database backup, then rerun provision-system. Do not replay bootstrap or reset the database",
            ),
            Self::BaselineMismatch { installed, expected } => write!(formatter,
                "system schema baseline mismatch: installed {installed}, expected {expected}. Use the matching system schema artifact. Do not replay or reset the system database"),
        }
    }
}

/// Read the completion record without changing schemas, grants, or data.
pub async fn system_state(client: &(impl GenericClient + Sync)) -> anyhow::Result<SystemState> {
    // Development resets retain the shared history schema. It does not identify
    // a control-store installation on its own.
    let present: bool = client
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_namespace WHERE nspname IN \
         ('registry', 'provisioning', 'identity', 'catalog', 'wamn_run', 'wamn_authority'))",
            &[],
        )
        .await
        .context("look for control-store schemas")?
        .get(0);
    if !present {
        return Ok(SystemState::Absent);
    }
    let baseline: bool = client
        .query_one(
            "SELECT to_regclass('registry.schema_baseline') IS NOT NULL",
            &[],
        )
        .await
        .context("look for the system baseline table")?
        .get(0);
    if !baseline {
        return Ok(SystemState::Incomplete);
    }
    let Some(row) = client
        .query_opt("SELECT sha256 FROM registry.schema_baseline", &[])
        .await
        .context("read the system schema baseline")?
    else {
        return Ok(SystemState::Incomplete);
    };
    let installed: String = row.get(0);
    let expected = wamn_control_provision::schema_baseline::system_sha256();
    if installed == expected {
        Ok(SystemState::Complete)
    } else {
        Ok(SystemState::BaselineMismatch {
            installed,
            expected,
        })
    }
}

/// Initialize an empty system database, then run the selected image's migrations.
/// Existing initialization never runs pending files on platform reapply.
pub async fn initialize_system(
    admin: &mut Client,
    sources: &[MigrationSource],
) -> anyhow::Result<()> {
    if install_control_store(admin).await? {
        admin.batch_execute("SET ROLE wamn_system").await?;
        let result = crate::platform_migrations::apply_reported(
            admin,
            crate::platform_migrations::MigrationTarget::System,
            sources,
            None,
        )
        .await;
        let reset = admin.batch_execute("RESET ROLE").await;
        result?;
        reset?;
    }
    Ok(())
}

/// Install the control store and baseline atomically. Return true only for a new installation.
pub async fn install_control_store(admin: &Client) -> anyhow::Result<bool> {
    admin
        .batch_execute("BEGIN")
        .await
        .context("begin system initialization")?;
    let result = async {
        admin
            .batch_execute("SELECT pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'))")
            .await
            .context("lock system initialization")?;
        match system_state(admin).await? {
            SystemState::Absent => {}
            SystemState::Complete => {
                admin
                    .batch_execute("COMMIT")
                    .await
                    .context("finish system initialization read")?;
                return Ok(false);
            }
            state => anyhow::bail!("{state}"),
        }

        admin
            .batch_execute(sql::ensure_db_owner_role_sql())
            .await
            .context("ensure the database-owner role that the record history grants name")?;
        admin
            .batch_execute(
                "DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
                                            current_database()); END $$;",
            )
            .await
            .context("give wamn_system the system database")?;
        admin
            .batch_execute("SET LOCAL ROLE wamn_system")
            .await
            .context("assume the control owner")?;
        for stage in CONTROL_BOOTSTRAP_SQL {
            admin
                .batch_execute(stage)
                .await
                .context("install the control store")?;
        }
        wamn_package_control::schema_baseline::record_system(admin)
            .await
            .context("record the system schema baseline")?;
        admin
            .batch_execute("RESET ROLE")
            .await
            .context("release the control owner before cluster ACL convergence")?;
        admin
            .batch_execute(sql::revoke_public_connect_floor_sql())
            .await
            .context("converge the cluster PUBLIC CONNECT floor")?;
        admin
            .batch_execute(
                "DO $$ BEGIN EXECUTE format(\
                   'REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()); END $$;",
            )
            .await
            .context("converge the system database PUBLIC TEMPORARY floor")?;
        admin
            .batch_execute(&wamn_control_provision::provisioner::ensure_provisioner_role_sql())
            .await
            .context("create the provisioning worker's login")?;
        admin
            .batch_execute(wamn_control_provision::apply::ensure_apply_role_sql())
            .await?;
        admin
            .batch_execute(wamn_control_provision::apply::APPLY_GRANTS_SQL)
            .await?;
        admin
            .batch_execute("COMMIT")
            .await
            .context("commit system initialization")?;
        Ok(true)
    }
    .await;
    match result {
        Ok(initialized) => Ok(initialized),
        Err(error) => {
            admin
                .batch_execute("ROLLBACK")
                .await
                .with_context(|| format!("roll back system initialization after {error:#}"))?;
            Err(error)
        }
    }
}

/// Write the restricted apply credential after system provisioning.
pub async fn emit_apply_credential(
    system_url: &str,
    host: &str,
    port: u16,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    ensure_secret_path(path, "--emit-apply-secret")?;
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("generate apply credential"))?;
    let password = hex::encode(bytes);
    let (admin, connection) = tokio_postgres::connect(system_url, NoTls).await?;
    let task = tokio::spawn(connection);
    admin
        .batch_execute(&wamn_control_provision::apply::apply_password_sql(
            &password,
        ))
        .await?;
    let mut url = Url::parse(system_url)?;
    url.set_username(wamn_control_provision::apply::APPLY_ROLE)
        .map_err(|()| anyhow::anyhow!("encode apply user"))?;
    url.set_password(Some(&password))
        .map_err(|()| anyhow::anyhow!("encode apply password"))?;
    url.set_host(Some(host))?;
    url.set_port(Some(port))
        .map_err(|()| anyhow::anyhow!("encode apply port"))?;
    let secret =
        wamn_control_provision::render_control_administration_secret_manifest("apply", "platform");
    let mut secret = secret;
    secret["metadata"]["name"] = "wamn-apply".into();
    secret["metadata"]["labels"] = json!({"app.kubernetes.io/name": "wamn-apply"});
    secret["stringData"] = json!({"url": url.as_str()});
    write_secret_json(path, &secret)?;
    drop(admin);
    task.await??;
    Ok(())
}

/// Inputs of one `provision-system` run with the emit flags.
pub struct EmitProvisionerRequest {
    /// Superuser URL of the system database. The run reads only its database
    /// name and opens no connection.
    pub system_database_url: String,
    /// Where the Secret manifest `wamn-provisioner` goes, mode 0600.
    pub emit_secret: PathBuf,
    /// Where the role statement goes.
    pub emit_provisioner_sql: PathBuf,
    /// Host the URL in the Secret names.
    pub db_host: String,
    /// Port the URL in the Secret names.
    pub db_port: u16,
}

impl fmt::Debug for EmitProvisionerRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmitProvisionerRequest")
            .field("system_database_url", &"[REDACTED]")
            .field("emit_secret", &self.emit_secret)
            .field("emit_provisioner_sql", &self.emit_provisioner_sql)
            .finish_non_exhaustive()
    }
}

/// Generate the password of `wamn_provisioner` and write the two outputs of
/// one run: the Secret manifest `wamn-provisioner` in `platform`, which holds
/// the password, and the role statement, which holds only its SCRAM-SHA-256
/// verifier. The operator applies the Secret with `kubectl apply` and the
/// statement as superuser (owner rulings of 2026-10-02 on `wamn-zua8.3`).
pub fn emit_provisioner_credential(request: &EmitProvisionerRequest) -> anyhow::Result<()> {
    ensure_secret_path(&request.emit_secret, "--emit-secret")?;
    ensure_distinct_secret_paths([
        ("--emit-secret", Some(request.emit_secret.as_path())),
        (
            "--emit-provisioner-sql",
            Some(request.emit_provisioner_sql.as_path()),
        ),
    ])?;
    let mut url = Url::parse(&request.system_database_url)
        .map_err(|_| anyhow::anyhow!("the system database URL cannot be parsed"))?;
    ensure!(
        url.path().len() > 1,
        "the system database URL names no database"
    );
    let rng = SystemRandom::new();
    let mut random = [0_u8; 32];
    rng.fill(&mut random)
        .map_err(|_| anyhow::anyhow!("operating system could not supply credential entropy"))?;
    let password = hex::encode(random);
    let mut salt = [0_u8; 16];
    rng.fill(&mut salt)
        .map_err(|_| anyhow::anyhow!("operating system could not supply credential entropy"))?;
    url.set_username(PROVISIONER_ROLE)
        .map_err(|()| anyhow::anyhow!("cannot encode the provisioner user"))?;
    url.set_password(Some(&password))
        .map_err(|()| anyhow::anyhow!("cannot encode the provisioner password"))?;
    url.set_host(Some(&request.db_host))
        .context("cannot encode the provisioner host")?;
    url.set_port(Some(request.db_port))
        .map_err(|()| anyhow::anyhow!("cannot encode the provisioner port"))?;
    url.set_query(None);
    url.set_fragment(None);
    let document = json!({
        "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
        "metadata": {"name": "wamn-provisioner", "namespace": "platform",
            "labels": {"app.kubernetes.io/name": "wamn-provisioner", "app.kubernetes.io/managed-by": "wamn"}},
        "stringData": {"url": url.as_str()}
    });
    write_secret_json(&request.emit_secret, &document)?;
    let statement = provisioner_statement_sql(&scram_sha256_verifier(&password, &salt));
    std::fs::write(&request.emit_provisioner_sql, statement)
        .with_context(|| format!("write {}", request.emit_provisioner_sql.display()))
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
        let mut url = Url::parse(database.url())?;
        let upstream = (url.host_str().unwrap().to_owned(), url.port().unwrap());
        let reserved = TcpListener::bind("127.0.0.1:0").await?;
        let address = reserved.local_addr()?;
        url.set_port(Some(address.port())).unwrap();
        drop(reserved);

        let attempts = Arc::new(AtomicUsize::new(0));
        let mut proxy = JoinSet::new();
        let (client, connection) = {
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
        let connection = tokio::spawn(connection);
        let row = client.query_one("SELECT current_database()", &[]).await?;
        assert_eq!(row.get::<_, String>(0), database.name());
        drop(client);
        connection.await??;

        // PostgreSQL accepts the TCP connection but refuses an absent database.
        url.set_path(&format!("/{}_absent", database.name()));
        let Err(error) = connect_system_database(url.as_str()).await else {
            panic!("an absent database must refuse the connection");
        };
        assert_eq!(
            error
                .downcast_ref::<tokio_postgres::Error>()
                .unwrap()
                .code(),
            Some(&tokio_postgres::error::SqlState::INVALID_CATALOG_NAME)
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
