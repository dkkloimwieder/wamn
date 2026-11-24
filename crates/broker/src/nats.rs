//! Verified TLS and user credentials for broker connections.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

pub use wamn_event::deploy_broker::{CA_FILE, CREDENTIALS_KEY};

/// Credentials for a broker operation.
#[derive(Debug, Clone)]
pub struct PlatformNats {
    pub nats_url: String,
    pub nats_tls_ca: PathBuf,
    pub creds: PathBuf,
}

impl PlatformNats {
    pub async fn connect(&self) -> anyhow::Result<async_nats::Client> {
        connect_platform(&self.nats_url, &self.nats_tls_ca, &self.creds).await
    }
}

pub(crate) async fn connect_platform(
    url: &str,
    ca: &Path,
    credentials: &Path,
) -> anyhow::Result<async_nats::Client> {
    let options = credentials_options(async_nats::ConnectOptions::new(), credentials).await?;
    connect(tls_options(options, Some(ca)), url, InitialConnect::Once).await
}

/// Parse the public subject to derive the exact inbox this user JWT authorizes.
pub fn credentials_inbox(creds: &str) -> anyhow::Result<String> {
    #[derive(serde::Deserialize)]
    struct Claim {
        sub: String,
    }
    let token = creds
        .lines()
        .skip_while(|line| line.trim() != "-----BEGIN NATS USER JWT-----")
        .nth(1)
        .context("NATS credentials have no user JWT")?;
    let payload = token
        .trim()
        .split('.')
        .nth(1)
        .context("invalid NATS user JWT")?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .context("invalid NATS user JWT payload")?;
    let claim: Claim = serde_json::from_slice(&payload).context("decode NATS user subject")?;
    anyhow::ensure!(
        claim.sub.len() == 56
            && claim.sub.starts_with('U')
            && claim
                .sub
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || (b'2'..=b'7').contains(&byte)),
        "invalid NATS user public key"
    );
    Ok(wamn_registry::name::nats_user_inbox(&claim.sub))
}

/// Apply one issued user JWT, its local NKey, and the matching inbox prefix.
pub fn user_options(
    options: async_nats::ConnectOptions,
    creds: &str,
) -> anyhow::Result<async_nats::ConnectOptions> {
    Ok(options
        .custom_inbox_prefix(credentials_inbox(creds)?)
        .credentials(creds)?)
}

pub async fn credentials_options(
    options: async_nats::ConnectOptions,
    path: &Path,
) -> anyhow::Result<async_nats::ConnectOptions> {
    let creds = zeroize::Zeroizing::new(
        tokio::fs::read_to_string(path)
            .await
            .context("read NATS credentials")?,
    );
    user_options(options, &creds)
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

/// Set server TLS trust roots when a CA is supplied.
pub fn tls_options(
    options: async_nats::ConnectOptions,
    ca: Option<&Path>,
) -> async_nats::ConnectOptions {
    match ca {
        Some(ca) => options
            .require_tls(true)
            .add_root_certificates(ca.to_path_buf()),
        None => options,
    }
}
