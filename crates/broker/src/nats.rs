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
        let options = tls_options(
            async_nats::ConnectOptions::new(),
            Some(&self.nats_tls_ca),
            Some(&self.nats_tls_cert),
            Some(&self.nats_tls_key),
            self.nats_tls_server_name.as_deref(),
        )?;
        connect(options, &self.nats_url, InitialConnect::Once).await
    }
}

/// Verifies the server certificate against one fixed name instead of the host
/// of the URL. The chain and the signatures get the full WebPKI test.
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

/// Whether a first transport outage starts the client's reconnect loop.
#[derive(Debug, Clone, Copy)]
pub enum InitialConnect {
    Once,
    RetryTransport,
}

/// Connect with one initial attempt. Only transport outages may retry at startup.
pub async fn connect(
    options: async_nats::ConnectOptions,
    url: &str,
    initial: InitialConnect,
) -> anyhow::Result<async_nats::Client> {
    use anyhow::Context as _;
    // Connection setup must not enlarge every caller's suspended state.
    Box::pin(async {
        if matches!(initial, InitialConnect::Once) {
            return options.connect(url).await.context("connect to NATS");
        }
        match options.clone().connect(url).await {
            Ok(client) => Ok(client),
            Err(error)
                if matches!(initial, InitialConnect::RetryTransport)
                    && matches!(
                        error.kind(),
                        async_nats::ConnectErrorKind::Io
                            | async_nats::ConnectErrorKind::TimedOut
                            | async_nats::ConnectErrorKind::Dns
                    ) =>
            {
                tracing::warn!(%error, "NATS is not reachable; the client keeps retrying");
                options
                    .retry_on_initial_connect()
                    .connect(url)
                    .await
                    .context("configure the NATS connection")
            }
            Err(error) => Err(error).context("connect to NATS"),
        }
    })
    .await
}

/// Configure the shared NATS transport, with an optional client certificate.
pub fn tls_options(
    options: async_nats::ConnectOptions,
    ca: Option<&std::path::Path>,
    cert: Option<&std::path::Path>,
    key: Option<&std::path::Path>,
    server_name: Option<&str>,
) -> anyhow::Result<async_nats::ConnectOptions> {
    use anyhow::Context as _;
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
    let Some(ca) = ca else {
        anyhow::ensure!(
            cert.is_none() && key.is_none() && server_name.is_none(),
            "NATS TLS needs a CA"
        );
        return Ok(options);
    };
    anyhow::ensure!(
        cert.is_some() == key.is_some(),
        "NATS client certificate and key must be supplied together"
    );
    // A CA-only connection still verifies the server through the same root input.
    let (Some(cert), Some(key)) = (cert, key) else {
        anyhow::ensure!(
            server_name.is_none(),
            "a pinned NATS server name needs a client certificate"
        );
        return Ok(options
            .require_tls(true)
            .add_root_certificates(ca.to_owned()));
    };
    let mut roots = rustls::RootCertStore::empty();
    for certificate in
        CertificateDer::pem_file_iter(ca).with_context(|| format!("read {}", ca.display()))?
    {
        roots.add(certificate?)?;
    }
    let chain = CertificateDer::pem_file_iter(cert)
        .with_context(|| format!("read {}", cert.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    let key =
        PrivateKeyDer::from_pem_file(key).with_context(|| format!("read {}", key.display()))?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let roots = Arc::new(roots);
    let verifier =
        rustls::client::WebPkiServerVerifier::builder_with_provider(roots, Arc::clone(&provider))
            .build()?;
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?;
    let config = match &server_name {
        Some(name) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedName {
                inner: verifier,
                name: ServerName::try_from((*name).to_owned())?,
            })),
        None => builder.with_webpki_verifier(verifier),
    }
    .with_client_auth_cert(chain, key)?;
    Ok(options.require_tls(true).tls_client_config(config))
}
