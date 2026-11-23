//! Private Postgres and NATS servers using the platform images.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
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
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
/// The name and namespace of the platform release.
const PLATFORM_RELEASE: (&str, &str) = ("wamn-platform", "platform");
/// The Secret and key to which identity-service writes the bootstrap operator include.
const OPERATOR_SECRET: (&str, &str) = ("wamn-nats-operator", "operator.conf");

/// Make each directory up to `--`, then start nats-server with the remaining arguments.
const START_NATS: &str = r#"
while [ "$1" != -- ]; do mkdir -p "$1"; shift; done
shift
exec nats-server "$@"
"#;

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
#[serde(rename_all = "camelCase")]
struct NatsValues<'a> {
    fullname_override: &'a str,
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

/// Render the platform chart and its subcharts with the default values.
///
/// Rendering runs `helm`, which is a required input.
pub fn render_platform() -> Result<Vec<Value>> {
    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/platform");
    let metadata: Value = serde_yaml::from_slice(
        &std::fs::read(chart.join("Chart.yaml")).context("read the platform chart metadata")?,
    )?;
    let (name, namespace) = PLATFORM_RELEASE;
    wamn_kubernetes::helm::template(&json!({
        "chart": chart,
        "version": metadata["version"],
        "name": name,
        "namespace": namespace,
        "values": {},
    }))
}

/// A private NATS server with the rendered platform configuration, removed on drop.
pub struct Nats {
    container: ContainerAsync<GenericImage>,
    _cert_directory: TempDir,
    ca_file: PathBuf,
    url: Url,
}

impl fmt::Debug for Nats {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Nats")
            .field("container_id", &self.container.id())
            .finish_non_exhaustive()
    }
}

impl Nats {
    /// The server's connection URL.
    pub fn url(&self) -> &str {
        self.url.as_str()
    }

    /// Client options that require TLS and trust only the server's certificate.
    pub fn connect_options(&self) -> async_nats::ConnectOptions {
        // A test binary links more than one rustls provider, so it names one.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        async_nats::ConnectOptions::new()
            .require_tls(true)
            .add_root_certificates(self.ca_file.clone())
    }
}

/// Start the NATS server of the rendered platform chart.
///
/// The container gets the server's rendered mounts, arguments and environment.
/// The operator Secret holds `operator_include`, the file that identity bootstrap
/// returns. Each Certificate Secret holds a new self-signed certificate for the
/// names of the rendered Certificate.
pub async fn start_nats(platform: &[Value], operator_include: &str) -> Result<Nats> {
    let values: PlatformValues<'_> = serde_yaml::from_str(PLATFORM_VALUES)?;
    let image_reference = values.nats.container.image.full_image_name;
    let statefulset = values.nats.fullname_override;
    let pod = &rendered(platform, "StatefulSet", statefulset)?["spec"]["template"]["spec"];
    let container = items(pod, "containers")
        .find(|container| container["image"] == image_reference)
        .context("the NATS pod has no container of the platform NATS image")?;

    let mut secrets = BTreeMap::new();
    let mut server_ca = None;
    let mut files = Vec::new();
    let mut directories = Vec::new();
    for mount in items(container, "volumeMounts") {
        let path = text(mount, "mountPath")?;
        let volume = items(pod, "volumes").find(|volume| volume["name"] == mount["name"]);
        if let Some(name) = volume.and_then(|volume| volume["configMap"]["name"].as_str()) {
            let config = rendered(platform, "ConfigMap", name)?;
            for (key, data) in config["data"].as_object().into_iter().flatten() {
                let data = data.as_str().context("ConfigMap data must be text")?;
                files.push((format!("{path}/{key}"), data.as_bytes().to_vec()));
            }
        } else if let Some(name) = volume.and_then(|volume| volume["secret"]["secretName"].as_str())
        {
            if !secrets.contains_key(name) {
                let data = secret_data(platform, name, operator_include)?;
                if let Some((_, ca)) = data.iter().find(|(key, _)| *key == "ca.crt") {
                    ensure!(
                        server_ca.replace(ca.clone()).is_none(),
                        "the NATS pod mounts more than one certificate Secret"
                    );
                }
                secrets.insert(name, data);
            }
            for (key, data) in &secrets[name] {
                files.push((format!("{path}/{key}"), data.clone()));
            }
        } else {
            // emptyDir and volume claim mounts start empty.
            directories.push(path);
        }
    }

    let mut environment: Vec<(&str, String)> = Vec::new();
    for variable in items(container, "env") {
        let name = text(variable, "name")?;
        let value = if variable["valueFrom"]["fieldRef"]["fieldPath"] == "metadata.name" {
            format!("{statefulset}-0")
        } else {
            let value = text(variable, "value")?;
            environment
                .iter()
                .fold(value.to_owned(), |value, (name, known)| {
                    value.replace(&format!("$({name})"), known)
                })
        };
        environment.push((name, value));
    }
    let port = items(container, "ports")
        .find(|port| port["name"] == "nats")
        .and_then(|port| port["containerPort"].as_u64())
        .context("the NATS container has no client port")?;
    let port = u16::try_from(port)?;
    let arguments = items(container, "args")
        .map(|argument| {
            argument
                .as_str()
                .context("container arguments must be text")
        })
        .collect::<Result<Vec<_>>>()?;

    let cert_directory = tempfile::tempdir().context("create NATS certificate directory")?;
    let ca_file = cert_directory.path().join("ca.crt");
    std::fs::write(
        &ca_file,
        server_ca.context("the NATS pod mounts no certificate Secret")?,
    )
    .context("write NATS trust certificate")?;

    let image = pinned_image(image_reference)?
        .with_entrypoint("/bin/sh")
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"));
    let mut request = publish_on_loopback(image, port).with_cmd(
        ["-ec", START_NATS, "sh"]
            .into_iter()
            .chain(directories)
            .chain(["--"])
            .chain(arguments),
    );
    for (path, data) in files {
        request = request.with_copy_to(path, data);
    }
    for (name, value) in environment {
        request = request.with_env_var(name, value);
    }
    let container = request
        .with_startup_timeout(Duration::from_secs(120))
        .start()
        .await
        .context("start the NATS test container")?;
    let host_port = container.get_host_port_ipv4(port).await?;
    Ok(Nats {
        container,
        _cert_directory: cert_directory,
        ca_file,
        url: Url::parse(&format!("nats://127.0.0.1:{host_port}"))?,
    })
}

/// The data that the platform's producer writes to a Secret the NATS pod mounts.
fn secret_data(
    platform: &[Value],
    name: &str,
    operator_include: &str,
) -> Result<Vec<(&'static str, Vec<u8>)>> {
    if name == OPERATOR_SECRET.0 {
        return Ok(vec![(
            OPERATOR_SECRET.1,
            operator_include.as_bytes().to_vec(),
        )]);
    }
    let certificate = platform
        .iter()
        .find(|object| object["kind"] == "Certificate" && object["spec"]["secretName"] == name)
        .with_context(|| format!("no rendered producer of the Secret {name}"))?;
    let names = ["dnsNames", "ipAddresses"]
        .into_iter()
        .flat_map(|key| items(&certificate["spec"], key))
        .map(|name| name.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .context("Certificate names must be text")?;
    let certificate = rcgen::generate_simple_self_signed(names)?;
    let certificate_pem = certificate.cert.pem().into_bytes();
    // The self-signed certificate is its own CA, as for the Postgres helper.
    Ok(vec![
        ("ca.crt", certificate_pem.clone()),
        ("tls.crt", certificate_pem),
        (
            "tls.key",
            certificate.signing_key.serialize_pem().into_bytes(),
        ),
    ])
}

fn rendered<'a>(platform: &'a [Value], type_: &str, name: &str) -> Result<&'a Value> {
    platform
        .iter()
        .find(|object| object["kind"] == type_ && object["metadata"]["name"] == name)
        .with_context(|| format!("the platform chart renders no {type_} {name}"))
}

fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("rendered field {key} must be text"))
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
