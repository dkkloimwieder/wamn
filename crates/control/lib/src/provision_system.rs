//! The `provision-system` subcommand: install the control store into an empty
//! system database and record the platform domain of the deployment.
//!
//! The database and its owner role `wamn_system` exist before this runs. On
//! CloudNativePG, the `initdb` bootstrap of the cluster creates both. The
//! verb connects as the superuser, installs [`CONTROL_BOOTSTRAP_SQL`] as
//! `wamn_system`, closes the PUBLIC floors, creates the login
//! `wamn_provisioner` of the provisioning worker, and writes
//! `registry.meta.platform_domain`. It refuses a database that already has
//! the schema `registry`, so a second run changes nothing.
//!
//! With the emit flags, the verb installs nothing. It writes the Secret
//! manifest of `wamn_provisioner` and the role statement that sets its
//! password, see [`emit_provisioner_credential`].

use std::fmt;
use std::num::NonZeroU32;
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::rand::{SecureRandom as _, SystemRandom};
use ring::{digest, hmac, pbkdf2};
use serde_json::json;
use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_control_provision::provisioner::{PROVISIONER_ROLE, provisioner_statement_sql};
use wamn_control_provision::schema_migrations::MigrationTarget;

use crate::provision_project_env::{
    ensure_distinct_secret_paths, ensure_secret_path, write_secret_json,
};
use wamn_control_provision::{CONTROL_BOOTSTRAP_SQL, sql, validate_platform_domain};

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
    let (client, connection) = tokio_postgres::connect(&request.system_database_url, NoTls)
        .await
        .context("connect to the system database")?;
    let connection = tokio::spawn(connection);
    let result = async {
        install_control_store(&client).await?;
        client
            .execute(
                "UPDATE registry.meta SET platform_domain = $1",
                &[&request.platform_domain],
            )
            .await
            .context("record the platform domain")?;
        Ok(())
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

/// Install [`CONTROL_BOOTSTRAP_SQL`] as `wamn_system` into the database of
/// `admin`, a superuser connection, and close the PUBLIC floors.
///
/// It refuses when the schema `registry` exists.
pub async fn install_control_store(admin: &Client) -> anyhow::Result<()> {
    let installed: bool = admin
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_catalog.pg_namespace WHERE nspname = 'registry')",
            &[],
        )
        .await
        .context("look for the schema registry")?
        .get(0);
    ensure!(
        !installed,
        "the system database already has the schema registry; provision-system runs once"
    );
    admin
        .batch_execute(&sql::ensure_control_author_acl_role_sql())
        .await
        .context("ensure the portable store's control-author ACL role")?;
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
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("assume the control owner")?;
    for stage in CONTROL_BOOTSTRAP_SQL {
        admin
            .batch_execute(stage)
            .await
            .context("install the control store")?;
    }
    crate::upgrade_schema::record_fresh_install(admin, MigrationTarget::System)
        .await
        .context("record the system migrations that the full schema holds")?;
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

/// The SCRAM-SHA-256 verifier that PostgreSQL stores for `password`
/// (RFC 7677, PostgreSQL's default 4096 iterations). The password is ASCII
/// hex, so SASLprep leaves it unchanged.
fn scram_sha256_verifier(password: &str, salt: &[u8]) -> String {
    const ITERATIONS: u32 = 4096;
    let mut salted = [0_u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(ITERATIONS).expect("4096 is not zero"),
        salt,
        password.as_bytes(),
        &mut salted,
    );
    let key = hmac::Key::new(hmac::HMAC_SHA256, &salted);
    let client_key = hmac::sign(&key, b"Client Key");
    let stored_key = digest::digest(&digest::SHA256, client_key.as_ref());
    let server_key = hmac::sign(&key, b"Server Key");
    format!(
        "SCRAM-SHA-256${ITERATIONS}:{}${}:{}",
        STANDARD.encode(salt),
        STANDARD.encode(stored_key.as_ref()),
        STANDARD.encode(server_key.as_ref())
    )
}
