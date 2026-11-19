//! Local identity service and signing-key lifecycle commands.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde_json::json;
use tokio::net::TcpListener;
use wamn_identity::session_keys::{
    activate_session_key, publish_session_key, remove_compromised_session_key, retire_session_keys,
};

use crate::{IdentityConfig, IdentityService, IdentityServiceError, connect, serve, tls_config};

/// Identity authority CLI; database credentials are absent from Debug and help values.
#[derive(Parser)]
#[command(name = "wamn-identity", version, about)]
pub struct Cli {
    /// Exact trusted HTTPS issuer.
    #[arg(long, global = true, env = "WAMN_IDENTITY_ISSUER")]
    issuer: Option<String>,
    #[arg(
        long,
        global = true,
        env = "WAMN_IDENTITY_DATABASE_URL",
        hide = true,
        hide_env_values = true
    )]
    database_url: Option<String>,
    #[command(subcommand)]
    command: Command,
}

impl fmt::Debug for Cli {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Cli")
            .field("issuer", &self.issuer)
            .field("database_url", &"[REDACTED]")
            .field("command", &self.command)
            .finish()
    }
}

/// Email environment inputs with credential-safe diagnostics.
#[derive(clap::Args)]
struct MailArgs {
    /// Private mail file selected by the local development environment.
    #[arg(long)]
    local_mail: Option<PathBuf>,
    #[arg(
        long,
        env = "RESEND_API_KEY",
        hide_env_values = true,
        requires = "resend_from"
    )]
    resend_api_key: Option<String>,
    #[arg(long, env = "RESEND_FROM", requires = "resend_api_key")]
    resend_from: Option<String>,
    /// Base URL of the web client. The invitation mail links to `<base>/invite#<code>`, and the reset mail to `<base>/reset#<secret>`.
    #[arg(long, env = "WAMN_IDENTITY_INVITE_URL", requires = "resend_api_key")]
    invite_url: Option<String>,
}
impl fmt::Debug for MailArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MailArgs")
            .field("resend_api_key", &"[REDACTED]")
            .field("local_mail", &self.local_mail)
            .field("resend_from", &self.resend_from)
            .field("invite_url", &self.invite_url)
            .finish()
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve public keys and configured user authentication endpoints over HTTPS.
    Serve {
        #[command(flatten)]
        mail: Box<MailArgs>,
        #[arg(long, env = "WAMN_IDENTITY_BIND", default_value = "0.0.0.0:8443")]
        bind: SocketAddr,
        #[arg(long, env = "WAMN_IDENTITY_TLS_CERT")]
        tls_cert: PathBuf,
        #[arg(long, env = "WAMN_IDENTITY_TLS_KEY")]
        tls_key: PathBuf,
        /// Rotate signing keys at this interval. The default is 30 days.
        #[arg(long, default_value_t = crate::DEFAULT_KEY_ROTATION.as_secs())]
        key_rotation_seconds: u64,
        /// Number of trusted proxy hops. Zero uses the TLS peer address.
        #[arg(long, default_value_t = 0)]
        trusted_proxy_hops: usize,
    },
    /// Commit a new public generation without activating it.
    Publish,
    /// Activate an already committed public generation.
    Activate {
        #[arg(long)]
        kid: String,
    },
    /// Remove compromised public and private material immediately.
    Remove {
        #[arg(long)]
        kid: String,
    },
    /// Delete generations whose public retention window has expired.
    Retire,
}

/// Run a local command; lifecycle output contains public metadata only.
pub async fn run(cli: Cli) -> Result<(), IdentityServiceError> {
    let issuer = cli
        .issuer
        .ok_or_else(|| IdentityServiceError::new("identity issuer is required"))?;
    let raw = cli
        .database_url
        .ok_or_else(|| IdentityServiceError::new("identity database credential is required"))?;
    let config = IdentityConfig::new(&issuer, &raw)?;
    drop(raw);
    if let Command::Serve {
        mail,
        bind,
        tls_cert,
        tls_key,
        key_rotation_seconds,
        trusted_proxy_hops,
    } = cli.command
    {
        let config = if let Some(path) = mail.local_mail {
            config.with_local_mail(path)
        } else {
            match (mail.resend_api_key, mail.resend_from) {
                (Some(key), Some(from)) => {
                    let resend = crate::mail::ResendConfig::new(key, from)?;
                    config.with_resend(match mail.invite_url {
                        Some(base) => resend.with_invite_url(&base)?,
                        None => resend,
                    })
                }
                (None, None) => config,
                _ => {
                    return Err(IdentityServiceError::new(
                        "Resend key and sender are required together",
                    ));
                }
            }
        };
        let config = config
            .with_trusted_proxy_hops(trusted_proxy_hops)
            .with_key_rotation(std::time::Duration::from_secs(key_rotation_seconds))?;
        let certificate = tokio::fs::read(tls_cert)
            .await
            .map_err(|_| IdentityServiceError::new("read identity TLS certificate failed"))?;
        let private_key = tokio::fs::read(tls_key)
            .await
            .map_err(|_| IdentityServiceError::new("read identity TLS private key failed"))?;
        let tls = tls_config(&certificate, &private_key)?;
        drop(private_key);
        let service = IdentityService::connect(config).await?;
        let rotation_state = Arc::clone(&service.inner);
        rotation_state.rotate_keys().await?;
        let rotation = async {
            let mut poll = tokio::time::interval(
                std::time::Duration::from_secs(60).min(rotation_state.key_rotation),
            );
            poll.tick().await;
            loop {
                poll.tick().await;
                if let Err(error) = rotation_state.rotate_keys().await {
                    tracing::error!(%error, "scheduled signing key rotation failed");
                }
            }
        };
        let listener = TcpListener::bind(bind)
            .await
            .map_err(|_| IdentityServiceError::new("bind identity HTTPS listener failed"))?;
        let address = listener
            .local_addr()
            .map_err(|_| IdentityServiceError::new("read identity HTTPS address failed"))?;
        tracing::info!("identity listening on {address}");
        return tokio::select! {
            result = serve(listener, service, tls) => result,
            () = rotation => unreachable!("rotation runs until shutdown"),
            result = tokio::signal::ctrl_c() => result.map_err(|_| IdentityServiceError::new("identity shutdown signal failed")),
        };
    }
    let mut database = connect(&config).await?;
    let output = match cli.command {
        Command::Publish => serde_json::to_value(
            publish_session_key(&mut database.client, &issuer, &crate::signing_envelope()?)
                .await
                .map_err(|_| IdentityServiceError::new("publish identity key failed"))?,
        )
        .map_err(|_| IdentityServiceError::new("encode public identity key failed"))?,
        Command::Activate { kid } => {
            activate_session_key(&mut database.client, &issuer, &kid)
                .await
                .map_err(|_| IdentityServiceError::new("activate identity key failed"))?;
            json!({"issuer": issuer, "kid": kid, "activated": true})
        }
        Command::Remove { kid } => {
            let removed = remove_compromised_session_key(&mut database.client, &issuer, &kid)
                .await
                .map_err(|_| IdentityServiceError::new("remove identity key failed"))?;
            json!({"issuer": issuer, "kid": kid, "removed": removed})
        }
        Command::Retire => {
            let retired = retire_session_keys(&mut database.client, &issuer)
                .await
                .map_err(|_| IdentityServiceError::new("retire identity keys failed"))?;
            json!({"issuer": issuer, "retired": retired})
        }
        Command::Serve { .. } => unreachable!("serve returned before lifecycle dispatch"),
    };
    println!("{output}");
    Ok(())
}
