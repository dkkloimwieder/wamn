//! A private CA and certificates for a test NATS on the loopback address
//! (docs/plan/alignment.md R4). The server maps each client certificate to a
//! user by its DNS name (`verify_and_map`).

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};

/// The user of the full-permission development certificate.
pub const DEVELOPMENT_USER: &str = "wamn-dev";

/// A CA, the server certificate for 127.0.0.1 and the files of both.
pub struct Authority {
    directory: PathBuf,
    issuer: Issuer<'static, KeyPair>,
    /// The CA certificate, which clients trust.
    pub ca: PathBuf,
    /// The server certificate and key.
    pub server_cert: PathBuf,
    pub server_key: PathBuf,
}

impl std::fmt::Debug for Authority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Authority")
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}

/// A client certificate of one user.
#[derive(Debug, Clone)]
pub struct ClientCertificate {
    /// The user that the server maps the certificate to.
    pub user: String,
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl ClientCertificate {
    /// The connection of a platform process with this certificate.
    pub fn platform_nats(&self, url: &str) -> wamn_platform_identity::nats::PlatformNats {
        wamn_platform_identity::nats::PlatformNats {
            nats_url: url.to_owned(),
            nats_tls_ca: self.ca.clone(),
            nats_tls_cert: self.cert.clone(),
            nats_tls_key: self.key.clone(),
            nats_tls_server_name: None,
        }
    }

    /// Connect with this certificate. Replies use the user's private inbox.
    pub async fn connect(&self, url: &str) -> anyhow::Result<async_nats::Client> {
        self.options()
            .connect(url)
            .await
            .with_context(|| format!("connect to {url} as {}", self.user))
    }

    /// The options of [`Self::connect`], for a caller that adds its own.
    pub fn options(&self) -> async_nats::ConnectOptions {
        async_nats::ConnectOptions::new()
            .require_tls(true)
            .add_root_certificates(self.ca.clone())
            .add_client_certificate(self.cert.clone(), self.key.clone())
            .custom_inbox_prefix(format!("_INBOX_{}", self.user))
    }
}

impl Authority {
    /// Make a CA and a server certificate for 127.0.0.1 in `directory`.
    ///
    /// # Errors
    ///
    /// Returns an error when a key or a file cannot be made.
    pub fn new(directory: &Path) -> anyhow::Result<Self> {
        let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "wamn test NATS CA");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_key = KeyPair::generate().context("generate the test CA key")?;
        let ca = ca_params.self_signed(&ca_key).context("sign the test CA")?;
        let issuer = Issuer::new(ca_params, ca_key);
        let ca_path = directory.join("ca.crt");
        write_private(&ca_path, ca.pem().as_bytes())?;
        let mut authority = Self {
            directory: directory.to_owned(),
            issuer,
            ca: ca_path,
            server_cert: directory.join("server.crt"),
            server_key: directory.join("server.key"),
        };
        let (cert, key) = authority.sign(
            "server",
            vec!["127.0.0.1".into(), "localhost".into()],
            ExtendedKeyUsagePurpose::ServerAuth,
        )?;
        authority.server_cert = cert;
        authority.server_key = key;
        Ok(authority)
    }

    /// A client certificate whose DNS name is `user`.
    ///
    /// # Errors
    ///
    /// Returns an error when a key or a file cannot be made.
    pub fn client(&self, user: &str) -> anyhow::Result<ClientCertificate> {
        // Brokers that share this CA share one certificate per user.
        let cert = self.directory.join(format!("client-{user}.crt"));
        if cert.is_file() {
            return Ok(ClientCertificate {
                user: user.to_owned(),
                ca: self.ca.clone(),
                key: self.directory.join(format!("client-{user}.key")),
                cert,
            });
        }
        let (cert, key) = self.sign(
            &format!("client-{user}"),
            vec![user.to_owned()],
            ExtendedKeyUsagePurpose::ClientAuth,
        )?;
        Ok(ClientCertificate {
            user: user.to_owned(),
            ca: self.ca.clone(),
            cert,
            key,
        })
    }

    /// The `tls` block of a server configuration, in the JSON form that
    /// nats-server reads.
    pub fn server_tls(&self) -> serde_json::Value {
        serde_json::json!({
            "cert_file": self.server_cert,
            "key_file": self.server_key,
            "ca_file": self.ca,
            "verify_and_map": true
        })
    }

    fn sign(
        &self,
        stem: &str,
        names: Vec<String>,
        purpose: ExtendedKeyUsagePurpose,
    ) -> anyhow::Result<(PathBuf, PathBuf)> {
        let key = KeyPair::generate().context("generate a test TLS key")?;
        let common_name = names[0].clone();
        let mut params = CertificateParams::new(names)?;
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        params.extended_key_usages = vec![purpose];
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let certificate = params
            .signed_by(&key, &self.issuer)
            .context("sign a test TLS certificate")?;
        let cert = self.directory.join(format!("{stem}.crt"));
        let key_path = self.directory.join(format!("{stem}.key"));
        write_private(&cert, certificate.pem().as_bytes())?;
        write_private(&key_path, key.serialize_pem().as_bytes())?;
        Ok((cert, key_path))
    }
}

/// A full-permission user for one client certificate.
pub fn full_permission_user(user: &str) -> serde_json::Value {
    serde_json::json!({ "user": user })
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create private file {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))
}
