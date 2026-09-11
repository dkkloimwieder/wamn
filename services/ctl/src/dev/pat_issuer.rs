//! Start the separate PAT authority for disposable environment provisioning.

use std::fmt;
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use ring::rand::{SecureRandom as _, SystemRandom};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, BufReader};
use tokio::process::{Child, Command};
use url::Url;
use wamn_control_provision::CredentialGeneration;

use crate::identity_issuer::{self, IdentityIssuerArgs};
use crate::pat_client::PatIssuerArgs;

// These bounds match the identity service's existing I/O timeout. The local
// certificates cover startup only and are deleted when provisioning ends.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const CERTIFICATE_LIFETIME: Duration = Duration::from_secs(3600);

/// One temporary identity process and its scoped database authority.
pub struct Bootstrap {
    pub args: PatIssuerArgs,
    child: Option<Child>,
    issuer: String,
    system_url: String,
    secret_name: String,
    files: PrivateDirectory,
}

impl fmt::Debug for Bootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Bootstrap").finish_non_exhaustive()
    }
}

impl Bootstrap {
    /// Stop the owned process before removing its database login authority.
    pub async fn stop(mut self) -> anyhow::Result<()> {
        if let Some(mut child) = self.child.take() {
            tokio::time::timeout(IO_TIMEOUT, async {
                if child.try_wait()?.is_none() {
                    child.kill().await?;
                }
                child.wait().await
            })
            .await
            .context("disposable identity shutdown timed out")?
            .context("stop the disposable identity process")?;
        }
        identity_issuer::run(self.generation_args(false)).await?;
        std::fs::remove_dir_all(&self.files.0)
            .context("remove the private identity bootstrap files")
    }

    fn generation_args(&self, prepare: bool) -> IdentityIssuerArgs {
        IdentityIssuerArgs {
            issuer: self.issuer.clone(),
            system_database_url: self.system_url.clone(),
            prepare_generation: prepare.then_some(CredentialGeneration::A),
            retire_generation: None,
            abort_generation: (!prepare).then_some(CredentialGeneration::A),
            emit_secret: prepare.then(|| self.files.0.join("database.json")),
            namespace: "wamn-system".into(),
            secret_name: self.secret_name.clone(),
        }
    }

    async fn spawn(&mut self, binary: &Path, bind: std::net::SocketAddr) -> anyhow::Result<()> {
        let database =
            super::environment::secret_value(&self.files.0.join("database.json"), "url")?;
        let mut command = Command::new(binary);
        command
            .env("WAMN_IDENTITY_ISSUER", &self.issuer)
            .env("WAMN_IDENTITY_DATABASE_URL", database)
            .args(["serve", "--bind", &bind.to_string()])
            .arg("--tls-cert")
            .arg(self.files.0.join("server.crt"))
            .arg("--tls-key")
            .arg(self.files.0.join("server.key"))
            .arg("--operator-ca")
            .arg(self.files.0.join("operator-ca.pem"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = command
            .spawn()
            .context("start the separate identity process")?;
        self.child = Some(child);
        let stdout = self
            .child
            .as_mut()
            .and_then(|child| child.stdout.take())
            .context("read the identity readiness pipe")?;
        let mut readiness = String::new();
        tokio::time::timeout(
            IO_TIMEOUT,
            BufReader::new(stdout.take(128)).read_line(&mut readiness),
        )
        .await
        .context("disposable identity startup timed out")?
        .context("read disposable identity readiness")?;
        anyhow::ensure!(
            readiness == format!("identity listening on {bind}\n"),
            "disposable identity process did not report its expected listener"
        );
        Ok(())
    }
}

/// Prepare temporary operator credentials and start the existing service binary.
pub(super) async fn start(system_url: &str, root: &Path) -> anyhow::Result<Bootstrap> {
    start_with(
        system_url,
        root,
        None,
        CERTIFICATE_LIFETIME,
        None,
        "wamn-dev-identity-db",
    )
    .await
}

/// Start the existing bootstrap with the caller's issuer and certificate limits.
pub async fn start_for_issuer(
    system_url: &str,
    root: &Path,
    issuer: &str,
    certificate_lifetime: Duration,
    ca_path_length: u8,
    secret_name: &str,
) -> anyhow::Result<Bootstrap> {
    start_with(
        system_url,
        root,
        Some(issuer),
        certificate_lifetime,
        Some(ca_path_length),
        secret_name,
    )
    .await
}

async fn start_with(
    system_url: &str,
    root: &Path,
    issuer: Option<&str>,
    certificate_lifetime: Duration,
    ca_path_length: Option<u8>,
    secret_name: &str,
) -> anyhow::Result<Bootstrap> {
    let binary = preflight(system_url)?;
    let socket = std::net::TcpListener::bind("127.0.0.1:0")
        .context("select the disposable identity listener")?;
    let bind = socket
        .local_addr()
        .context("read the disposable identity address")?;
    let endpoint = format!("https://{bind}");
    let issuer = issuer.unwrap_or(&endpoint).to_owned();
    let files = PrivateDirectory::new(root)?;
    certificates(
        &files.0,
        "server",
        ExtendedKeyUsagePurpose::ServerAuth,
        certificate_lifetime,
        ca_path_length,
    )?;
    certificates(
        &files.0,
        "operator",
        ExtendedKeyUsagePurpose::ClientAuth,
        certificate_lifetime,
        ca_path_length,
    )?;
    let mut bootstrap = Bootstrap {
        args: PatIssuerArgs {
            endpoint: Some(endpoint),
            client_cert: Some(files.0.join("operator.crt")),
            client_key: Some(files.0.join("operator.key")),
            server_ca: Some(files.0.join("server-ca.pem")),
        },
        child: None,
        issuer,
        system_url: system_url.to_owned(),
        secret_name: secret_name.to_owned(),
        files,
    };
    identity_issuer::run(bootstrap.generation_args(true)).await?;
    // A competing bind fails startup. Never retry with another authority or
    // fall back to direct database minting.
    drop(socket);
    if let Err(error) = bootstrap.spawn(&binary, bind).await {
        bootstrap.stop().await?;
        return Err(error);
    }
    Ok(bootstrap)
}

pub(super) fn preflight(system_url: &str) -> anyhow::Result<PathBuf> {
    let parsed = Url::parse(system_url)
        .map_err(|_| anyhow::anyhow!("disposable system database URL is invalid"))?;
    anyhow::ensure!(
        parsed.path() == "/wamn_system",
        "PAT provisioning requires the disposable system database to be named wamn_system"
    );
    identity_binary()
}

fn identity_binary() -> anyhow::Result<PathBuf> {
    let binary = if let Some(path) = std::env::var_os("WAMN_IDENTITY_BINARY") {
        PathBuf::from(path)
    } else {
        let executable = std::env::current_exe().context("locate the current executable")?;
        let directory = executable.parent().context("locate the binary directory")?;
        let directory = if directory.file_name().is_some_and(|name| name == "deps") {
            directory
                .parent()
                .context("locate the Cargo binary directory")?
        } else {
            directory
        };
        directory.join("wamn-identity")
    };
    anyhow::ensure!(
        binary.is_file(),
        "build wamn-identity beside the CLI or set WAMN_IDENTITY_BINARY to its executable"
    );
    Ok(binary)
}

struct PrivateDirectory(PathBuf);

impl PrivateDirectory {
    fn new(root: &Path) -> anyhow::Result<Self> {
        let mut nonce = [0u8; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| anyhow::anyhow!("generate the private identity directory name"))?;
        let path = root.join(format!(".pat-issuer-{}", hex::encode(nonce)));
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .context("create the private identity directory")?;
        Ok(Self(path))
    }
}

impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        // This path names only the fresh private directory created above.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn certificate_params(names: Vec<String>, lifetime: Duration) -> anyhow::Result<CertificateParams> {
    let mut params =
        CertificateParams::new(names).context("create disposable certificate parameters")?;
    let now = SystemTime::now();
    params.not_before = now.into();
    params.not_after = (now + lifetime).into();
    Ok(params)
}

fn certificates(
    root: &Path,
    stem: &str,
    purpose: ExtendedKeyUsagePurpose,
    lifetime: Duration,
    ca_path_length: Option<u8>,
) -> anyhow::Result<()> {
    let mut ca_params = certificate_params(Vec::new(), lifetime)?;
    ca_params.is_ca = IsCa::Ca(ca_path_length.map_or(
        BasicConstraints::Unconstrained,
        BasicConstraints::Constrained,
    ));
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca_key = KeyPair::generate().context("generate a disposable CA key")?;
    let ca = ca_params
        .self_signed(&ca_key)
        .context("sign a disposable CA certificate")?;
    let issuer = Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate().context("generate a disposable TLS key")?;
    let mut params = certificate_params(vec!["127.0.0.1".into()], lifetime)?;
    params.extended_key_usages = vec![purpose];
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    let certificate = params
        .signed_by(&key, &issuer)
        .context("sign a disposable TLS certificate")?;
    for (name, pem) in [
        (format!("{stem}-ca.pem"), ca.pem()),
        (format!("{stem}.crt"), certificate.pem()),
        (format!("{stem}.key"), key.serialize_pem()),
    ] {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(name))
            .context("create a private TLS file")?;
        file.write_all(pem.as_bytes())
            .context("write a private TLS file")?;
    }
    Ok(())
}
