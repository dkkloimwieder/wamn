//! A private scheduler NATS for the local host and its authority writers.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair};
use rustls::pki_types::PrivatePkcs8KeyDer;
use wamn_platform_identity::auth_changes::AuthNats;

/// The scheduler connection and its owned local process.
#[derive(Debug)]
pub struct LocalAuthNats {
    pub connection: AuthNats,
    pub(super) client: async_nats::Client,
    server: tokio::process::Child,
}

impl LocalAuthNats {
    pub(super) async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.server
            .kill()
            .await
            .context("stop local scheduler NATS")
    }

    pub(super) async fn start(scratch: &Path) -> anyhow::Result<Self> {
        let binary = std::env::var_os("WAMN_NATIVE_C_NATS_BIN")
            .context("set WAMN_NATIVE_C_NATS_BIN to a local nats-server executable")?;
        let directory = scratch.join("auth-nats");
        std::fs::create_dir(&directory)?;
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_key = KeyPair::generate()?;
        let ca = ca_params.self_signed(&ca_key)?;
        let issuer = Issuer::new(ca_params, ca_key);
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::new(vec!["localhost".into()])?;
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let certificate = params.signed_by(&key, &issuer)?;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der().clone())?;
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_client_auth_cert(
            vec![certificate.der().clone()],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )?;
        for (name, pem) in [
            ("ca.pem", ca.pem()),
            ("tls.pem", certificate.pem()),
            ("tls.key", key.serialize_pem()),
        ] {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(directory.join(name))?
                .write_all(pem.as_bytes())?;
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let connection = AuthNats {
            nats_url: format!("nats://localhost:{port}"),
            nats_tls_ca: directory.join("ca.pem"),
            nats_tls_cert: directory.join("tls.pem"),
            nats_tls_key: directory.join("tls.key"),
            nats_tls_server_name: Some("localhost".into()),
        };
        let server = tokio::process::Command::new(binary)
            .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
            .arg(&directory)
            .arg("--tlsverify")
            .arg("--tlscacert")
            .arg(&connection.nats_tls_ca)
            .arg("--tlscert")
            .arg(&connection.nats_tls_cert)
            .arg("--tlskey")
            .arg(&connection.nats_tls_key)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(directory.join("server.log"))?)
            .kill_on_drop(true)
            .spawn()
            .context("start local scheduler NATS")?;
        let client = async_nats::ConnectOptions::new()
            .require_tls(true)
            .tls_client_config(tls)
            .retry_on_initial_connect()
            .connect(&connection.nats_url)
            .await?;
        tokio::time::timeout(Duration::from_secs(10), client.flush())
            .await
            .context("local scheduler NATS did not become ready")??;
        Ok(Self {
            connection,
            client,
            server,
        })
    }
}
