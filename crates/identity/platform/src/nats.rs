//! The NATS client of the platform processes that are not hosts.

use std::path::PathBuf;
use std::sync::Arc;

/// The NATS connection of a platform process that is not a host: identity,
/// wamn-ctl, the CDC reader and the provisioning Jobs. The server takes only a
/// client certificate of the platform CA, and maps it to the process's user
/// (docs/plan/alignment.md R4).
#[derive(Debug, Clone)]
pub struct PlatformNats {
    /// The NATS URL.
    pub nats_url: String,
    /// The CA certificate of the server.
    pub nats_tls_ca: PathBuf,
    /// The client certificate.
    pub nats_tls_cert: PathBuf,
    /// The key of the client certificate.
    pub nats_tls_key: PathBuf,
    /// The name the server certificate must carry, when the URL reaches the
    /// server by another name, for example through a port-forward.
    pub nats_tls_server_name: Option<String>,
}

impl PlatformNats {
    /// Read the connection from `WAMN_NATS_URL`, `WAMN_NATS_TLS_CA`,
    /// `WAMN_NATS_TLS_CERT`, `WAMN_NATS_TLS_KEY` and the optional
    /// `WAMN_NATS_TLS_SERVER_NAME`.
    pub fn from_env() -> anyhow::Result<Self> {
        use anyhow::Context as _;
        let variable = |name: &str| {
            std::env::var(name).with_context(|| format!("set {name} for the platform NATS"))
        };
        Ok(Self {
            nats_url: variable("WAMN_NATS_URL")?,
            nats_tls_ca: variable("WAMN_NATS_TLS_CA")?.into(),
            nats_tls_cert: variable("WAMN_NATS_TLS_CERT")?.into(),
            nats_tls_key: variable("WAMN_NATS_TLS_KEY")?.into(),
            nats_tls_server_name: std::env::var("WAMN_NATS_TLS_SERVER_NAME").ok(),
        })
    }

    /// Connect with the client certificate.
    pub async fn connect(&self) -> anyhow::Result<async_nats::Client> {
        use anyhow::Context as _;
        use rustls::pki_types::pem::PemObject as _;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

        let mut roots = rustls::RootCertStore::empty();
        for certificate in CertificateDer::pem_file_iter(&self.nats_tls_ca)
            .with_context(|| format!("read {}", self.nats_tls_ca.display()))?
        {
            roots.add(certificate?)?;
        }
        let chain = CertificateDer::pem_file_iter(&self.nats_tls_cert)
            .with_context(|| format!("read {}", self.nats_tls_cert.display()))?
            .collect::<Result<Vec<_>, _>>()?;
        let key = PrivateKeyDer::from_pem_file(&self.nats_tls_key)
            .with_context(|| format!("read {}", self.nats_tls_key.display()))?;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let roots = Arc::new(roots);
        let verifier =
            rustls::client::WebPkiServerVerifier::builder_with_provider(roots, provider.clone())
                .build()?;
        let builder = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?;
        let config = match &self.nats_tls_server_name {
            Some(name) => builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(PinnedName {
                    inner: verifier,
                    name: ServerName::try_from(name.clone())?,
                })),
            None => builder.with_webpki_verifier(verifier),
        }
        .with_client_auth_cert(chain, key)?;
        async_nats::ConnectOptions::new()
            .require_tls(true)
            .tls_client_config(config)
            .connect(&self.nats_url)
            .await
            .with_context(|| format!("connect to the NATS at {}", self.nats_url))
    }
}

/// Verifies the server certificate against one fixed name instead of the host
/// of the URL. The chain and the signatures get the full WebPKI check.
#[derive(Debug)]
struct PinnedName {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    name: rustls::pki_types::ServerName<'static>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedName {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        self.inner
            .verify_server_cert(end_entity, intermediates, &self.name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner
            .verify_tls12_signature(message, certificate, signature)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner
            .verify_tls13_signature(message, certificate, signature)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
