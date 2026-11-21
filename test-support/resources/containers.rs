//! Private Postgres and NATS servers using the platform images.

use std::fmt;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, ensure};
use serde::Deserialize;
use sqlx::{Connection as _, PgConnection};
use tempfile::TempDir;
use testcontainers::{
    ContainerAsync, ContainerRequest, GenericImage, ImageExt as _,
    bollard::models::PortBinding,
    core::{IntoContainerPort as _, WaitFor},
    runners::AsyncRunner as _,
};
use url::Url;

const PLATFORM_VALUES: &str = include_str!("../../deploy/charts/platform/values.yaml");
const POSTGRES_PORT: u16 = 5432;
const NATS_PORT: u16 = 4222;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

const START_POSTGRES: &str = r#"
umask 077
data=$(mktemp -d)
initdb --no-sync -D "$data" --username=postgres --auth-host=scram-sha-256 \
    --auth-local=reject --no-locale --encoding=UTF8 --pwfile=/tmp/wamn-password
cat >> "$data/pg_hba.conf" <<'PG_HBA'
hostssl all all all scram-sha-256
hostssl replication all all scram-sha-256
PG_HBA
cat /tmp/wamn-server.crt > "$data/server.crt"
cat /tmp/wamn-server.key > "$data/server.key"
# Keep synchronous commits so logical decoding can read WAL as soon as a commit returns.
exec postgres -D "$data" -h '*' -k '' \
    -c ssl=on -c wal_level=logical -c fsync=off -c full_page_writes=off
"#;

#[derive(Deserialize)]
struct PlatformValues<'a> {
    #[serde(borrow)]
    postgres: PostgresValues<'a>,
    #[serde(borrow)]
    nats: NatsValues<'a>,
}

#[derive(Deserialize)]
struct PostgresValues<'a> {
    image: &'a str,
}

#[derive(Deserialize)]
struct NatsValues<'a> {
    #[serde(borrow)]
    container: NatsContainerValues<'a>,
}

#[derive(Deserialize)]
struct NatsContainerValues<'a> {
    #[serde(borrow)]
    image: NatsImageValues<'a>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NatsImageValues<'a> {
    full_image_name: &'a str,
}

fn pinned_image(reference: &str) -> Result<GenericImage> {
    let (tagged, _) = reference
        .split_once("@sha256:")
        .with_context(|| format!("missing digest in platform image {reference}"))?;
    let separator = tagged
        .rfind(':')
        .with_context(|| format!("missing tag in platform image {reference}"))?;
    let (name, tag_and_digest) = reference.split_at(separator);
    Ok(GenericImage::new(name, &tag_and_digest[1..]))
}

fn publish_on_loopback(image: GenericImage, port: u16) -> ContainerRequest<GenericImage> {
    image
        .with_exposed_port(port.tcp())
        .with_host_config_modifier(move |config| {
            config.publish_all_ports = Some(false);
            config.port_bindings = Some(
                [(
                    port.tcp().to_string(),
                    Some(vec![PortBinding {
                        host_ip: Some("127.0.0.1".into()),
                        host_port: Some("0".into()),
                    }]),
                )]
                .into(),
            );
        })
}

/// A private Postgres server and its TLS trust file, removed on drop.
pub struct Postgres {
    container: ContainerAsync<GenericImage>,
    _cert_directory: TempDir,
    admin_url: Url,
}

impl fmt::Debug for Postgres {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Postgres")
            .field("container_id", &self.container.id())
            .finish_non_exhaustive()
    }
}

/// Start a fresh Postgres server and authenticate over verified TLS.
pub async fn start_postgres() -> Result<Postgres> {
    let values: PlatformValues<'_> = serde_yaml::from_str(PLATFORM_VALUES)?;
    let password = random_password()?;
    let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])?;
    let certificate_pem = certificate.cert.pem();
    let cert_directory = tempfile::tempdir().context("create Postgres certificate directory")?;
    let cert_path = cert_directory.path().join("server.crt");
    std::fs::write(&cert_path, &certificate_pem).context("write Postgres trust certificate")?;

    let image = pinned_image(values.postgres.image)?
        .with_entrypoint("/bin/sh")
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));
    let container = publish_on_loopback(image, POSTGRES_PORT)
        .with_copy_to("/tmp/wamn-password", password.as_bytes().to_vec())
        .with_copy_to("/tmp/wamn-server.crt", certificate_pem.into_bytes())
        .with_copy_to(
            "/tmp/wamn-server.key",
            certificate.signing_key.serialize_pem().into_bytes(),
        )
        .with_cmd(["-ec", START_POSTGRES])
        .with_startup_timeout(Duration::from_secs(120))
        .start()
        .await
        .context("start the Postgres test container")?;
    let port = container.get_host_port_ipv4(POSTGRES_PORT).await?;
    let mut admin_url = Url::parse(&format!(
        "postgresql://postgres:{password}@127.0.0.1:{port}/postgres"
    ))?;
    admin_url
        .query_pairs_mut()
        .append_pair("sslmode", "verify-full")
        .append_pair(
            "sslrootcert",
            cert_path
                .to_str()
                .context("Postgres certificate path must be UTF-8")?,
        );
    tokio::time::timeout(
        CONNECTION_TIMEOUT,
        PgConnection::connect(admin_url.as_str()),
    )
    .await
    .context("wait for the Postgres test server connection")?
    .context("connect to the Postgres test server")?;

    Ok(Postgres {
        container,
        _cert_directory: cert_directory,
        admin_url,
    })
}

impl Postgres {
    /// Connect as the administrator to the `postgres` database.
    pub fn admin_url(&self) -> &str {
        self.admin_url.as_str()
    }

    /// Build a URL for an existing database and login with the server's TLS settings.
    pub fn database_url(&self, database: &str, login: &str, password: &str) -> Result<Url> {
        let mut url = self.admin_url.clone();
        url.set_username(login)
            .map_err(|()| anyhow!("set Postgres login"))?;
        url.set_password(Some(password))
            .map_err(|()| anyhow!("set Postgres password"))?;
        set_database(&mut url, database);
        Ok(url)
    }

    /// Create an empty database and return its administrator URL.
    pub async fn create_database(&self, name: &str) -> Result<Url> {
        let name_sql = database_identifier(name)?;
        let mut admin = PgConnection::connect(self.admin_url()).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name_sql}")))
            .execute(&mut admin)
            .await
            .context("create the test database")?;
        let mut url = self.admin_url.clone();
        set_database(&mut url, name);
        Ok(url)
    }

    /// Create the restricted role that owns the control store after installation.
    pub async fn create_system_role(&self) -> Result<()> {
        let mut admin = PgConnection::connect(self.admin_url()).await?;
        sqlx::raw_sql("CREATE ROLE wamn_system NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS")
            .execute(&mut admin)
            .await
            .context("create the control store owner role")?;
        Ok(())
    }

    /// Create the project roles and database, then return the direct migration login URL.
    pub async fn create_project_database(&self, name: &str) -> Result<Url> {
        let name_sql = database_identifier(name)?;
        let password = random_password()?;
        let mut admin = PgConnection::connect(self.admin_url()).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE ROLE wamn_app NOLOGIN NOINHERIT; \
             CREATE ROLE wamn_query NOLOGIN NOINHERIT; \
             CREATE ROLE wamn_platform NOLOGIN NOINHERIT; \
             CREATE ROLE wamn_migration LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '{password}'; \
             GRANT wamn_platform TO wamn_migration;"
        )))
        .execute(&mut admin)
        .await
        .context("create the project database roles")?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE {name_sql} OWNER wamn_migration"
        )))
        .execute(&mut admin)
        .await
        .context("create the migration-owned test database")?;
        self.database_url(name, "wamn_migration", &password)
    }
}

fn set_database(url: &mut Url, name: &str) {
    url.path_segments_mut()
        .expect("Postgres URL permits a database path")
        .clear()
        .push(name);
}

fn database_identifier(name: &str) -> Result<String> {
    ensure!(
        name.len() <= 63
            && name.starts_with(|letter: char| letter.is_ascii_lowercase())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
        "the test database name must be a lowercase SQL identifier"
    );
    Ok(format!("\"{name}\""))
}

fn random_password() -> Result<String> {
    let random = ring::rand::generate::<[u8; 24]>(&ring::rand::SystemRandom::new())
        .map_err(|_| anyhow!("generate test database password"))?;
    Ok(hex::encode(random.expose()))
}

/// A private NATS server, removed on drop.
#[derive(Debug)]
pub struct Nats {
    _container: ContainerAsync<GenericImage>,
    url: Url,
}

impl Nats {
    /// The server's connection URL.
    pub fn url(&self) -> &str {
        self.url.as_str()
    }
}

/// Start a NATS server with JetStream and wait for it to answer a client.
pub async fn start_nats() -> Result<Nats> {
    let values: PlatformValues<'_> = serde_yaml::from_str(PLATFORM_VALUES)?;
    let image = pinned_image(values.nats.container.image.full_image_name)?
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"));
    let container = publish_on_loopback(image, NATS_PORT)
        .with_cmd(["nats-server", "--jetstream"])
        .start()
        .await
        .context("start the NATS test container")?;
    let port = container.get_host_port_ipv4(NATS_PORT).await?;
    let url = Url::parse(&format!("nats://127.0.0.1:{port}"))?;
    tokio::time::timeout(CONNECTION_TIMEOUT, async {
        let client = async_nats::connect(url.as_str()).await?;
        client.flush().await?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("wait for the NATS test server connection")?
    .context("connect to the NATS test server")?;
    Ok(Nats {
        _container: container,
        url,
    })
}
