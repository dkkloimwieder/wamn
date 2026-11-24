//! Identity HTTPS server and local key maintenance commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;
use wamn_envelope::encryption::{Configuration, Envelope};
use wamn_identity::identity::session_keys::{remove_compromised_session_key, retire_session_keys};

use crate::{
    IdentityConfig, IdentityService, IdentityServiceError, connect, nats, serve, tls_config,
};

const TLS_CERT: &str = "/var/run/wamn-identity/tls/tls.crt";
const TLS_KEY: &str = "/var/run/wamn-identity/tls/tls.key";
const LOCAL_KEY: &str = "/etc/wamn/oauth/key";
const RESEND_TOKEN: &str = "/var/run/wamn-identity/resend/api-key";

#[derive(Debug, Parser)]
#[command(name = "wamn-identity", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    issuer: String,
    database: wamn_postgres::database::ConnectionConfig,
    envelope: Configuration,
    mail: Option<MailSettings>,
    nats: Option<nats::Settings>,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum MailSettings {
    Local {
        spool: PathBuf,
    },
    Resend {
        sender: String,
        invite_url: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve public keys and configured user authentication endpoints over HTTPS.
    Serve {
        /// Path to the identity JSON settings file.
        #[arg(long)]
        settings: PathBuf,
    },
    /// Remove compromised public and private material immediately.
    Remove {
        /// Path to the identity JSON settings file.
        #[arg(long)]
        settings: PathBuf,
        #[arg(long)]
        kid: String,
    },
    /// Delete generations whose public retention window has expired.
    Retire {
        /// Path to the identity JSON settings file.
        #[arg(long)]
        settings: PathBuf,
    },
}

impl Settings {
    async fn read(path: &Path) -> Result<Self, IdentityServiceError> {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| IdentityServiceError::new("read identity settings failed"))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| IdentityServiceError::new("decode identity settings failed"))
    }
}

/// Run a local command; lifecycle output contains public metadata only.
pub async fn run(cli: Cli) -> Result<(), IdentityServiceError> {
    let settings = match &cli.command {
        Command::Serve { settings }
        | Command::Remove { settings, .. }
        | Command::Retire { settings } => Settings::read(settings).await?,
    };
    let issuer = settings.issuer;
    let database = settings
        .database
        .to_json()
        .map_err(|_| IdentityServiceError::new("encode identity database configuration failed"))?;
    let mut config = IdentityConfig::new(&issuer, &database)?;
    if matches!(cli.command, Command::Serve { .. }) {
        config.nats = settings.nats;
        let config = match settings.mail {
            Some(MailSettings::Local { spool }) => config.with_local_mail(spool),
            Some(MailSettings::Resend { sender, invite_url }) => {
                let mut key = tokio::fs::read_to_string(RESEND_TOKEN)
                    .await
                    .map_err(|_| IdentityServiceError::new("read identity Resend token failed"))?;
                key.truncate(key.trim_end().len());
                let mut resend = crate::mail::ResendConfig::new(key, sender)?;
                if let Some(base) = invite_url {
                    resend = resend.with_invite_url(&base)?;
                }
                config.with_resend(resend)
            }
            None => config,
        };
        let envelope = match settings.envelope {
            Configuration::Local => wamn_key::from_file(Path::new(LOCAL_KEY)),
            configuration @ Configuration::Gcp { .. } => wamn_key::open(&configuration),
        }
        .map_err(|_| IdentityServiceError::new("open signing key envelope failed"))?;
        return run_server(config, envelope).await;
    }
    let mut database = connect(&config).await?;
    let output = match cli.command {
        Command::Remove { kid, .. } => {
            let removed = remove_compromised_session_key(&mut database.client, &issuer, &kid)
                .await
                .map_err(|_| IdentityServiceError::new("remove identity key failed"))?;
            json!({"issuer": issuer, "kid": kid, "removed": removed})
        }
        Command::Retire { .. } => {
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

async fn run_server(
    config: IdentityConfig,
    envelope: Envelope,
) -> Result<(), IdentityServiceError> {
    let certificate = tokio::fs::read(TLS_CERT)
        .await
        .map_err(|_| IdentityServiceError::new("read identity TLS certificate failed"))?;
    let private_key = tokio::fs::read(TLS_KEY)
        .await
        .map_err(|_| IdentityServiceError::new("read identity TLS private key failed"))?;
    let tls = tls_config(&certificate, &private_key)?;
    drop(private_key);
    let service = IdentityService::connect(config, envelope).await?;
    let maintenance_state = Arc::clone(&service.inner);
    maintenance_state.rotate_keys().await?;
    let maintenance = async {
        let mut poll = tokio::time::interval(Duration::from_secs(60));
        poll.tick().await;
        loop {
            poll.tick().await;
            if let Err(error) = maintenance_state.rotate_keys().await {
                tracing::error!(%error, "scheduled signing key rotation failed");
            }
            if let Err(error) = maintenance_state.prune_tx_identity().await {
                tracing::warn!(%error, "system transaction identity retention failed");
            }
        }
    };
    let listener = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, 8443))
        .await
        .map_err(|_| IdentityServiceError::new("bind identity HTTPS listener failed"))?;
    let address = listener
        .local_addr()
        .map_err(|_| IdentityServiceError::new("read identity HTTPS address failed"))?;
    tracing::info!(%address, "identity listening");
    tokio::select! {
        result = serve(listener, service, tls) => result,
        () = maintenance => unreachable!("maintenance runs until shutdown"),
        result = tokio::signal::ctrl_c() => result.map_err(|_| IdentityServiceError::new("identity shutdown signal failed")),
    }
}
