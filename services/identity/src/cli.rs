//! Local identity service and signing-key lifecycle commands.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpListener;
use wamn_control_provision::session_target::SessionTarget;
use wamn_platform_identity::session_keys::{
    activate_session_key, publish_session_key, remove_compromised_session_key, retire_session_keys,
};

use crate::{
    IdentityConfig, IdentityService, IdentityServiceError, connect, serve, tls_config,
    tls_config_with_operator_ca,
};

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

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve public keys and explicitly configured human PAT exchanges over HTTPS.
    Serve {
        #[arg(long, env = "WAMN_IDENTITY_BIND", default_value = "0.0.0.0:8443")]
        bind: SocketAddr,
        #[arg(long, env = "WAMN_IDENTITY_TLS_CERT")]
        tls_cert: PathBuf,
        #[arg(long, env = "WAMN_IDENTITY_TLS_KEY")]
        tls_key: PathBuf,
        /// Dedicated client-certificate CA for provisioning operators that mint PATs.
        #[arg(long, env = "WAMN_IDENTITY_OPERATOR_CA")]
        operator_ca: Option<PathBuf>,
        /// Mounted provisioner-generated target file; repeat for each admitted audience.
        #[arg(long)]
        session_target: Vec<PathBuf>,
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
        bind,
        tls_cert,
        tls_key,
        operator_ca,
        session_target,
    } = cli.command
    {
        let mut targets = Vec::with_capacity(session_target.len());
        for path in session_target {
            // Bound startup parsing even if a mounted file is malformed or grows.
            const MAX_TARGET_BYTES: u64 = 65_536;
            let file = tokio::fs::File::open(path)
                .await
                .map_err(|_| IdentityServiceError::new("read identity session target failed"))?;
            let mut bytes = Vec::new();
            file.take(MAX_TARGET_BYTES + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| IdentityServiceError::new("read identity session target failed"))?;
            if bytes.len() as u64 > MAX_TARGET_BYTES {
                return Err(IdentityServiceError::new(
                    "identity session target is too large",
                ));
            }
            targets.push(
                SessionTarget::from_json(&bytes)
                    .map_err(|_| IdentityServiceError::new("identity session target refused"))?,
            );
        }
        let config = config.with_session_targets(targets)?;
        let certificate = tokio::fs::read(tls_cert)
            .await
            .map_err(|_| IdentityServiceError::new("read identity TLS certificate failed"))?;
        let private_key = tokio::fs::read(tls_key)
            .await
            .map_err(|_| IdentityServiceError::new("read identity TLS private key failed"))?;
        let tls = match operator_ca {
            Some(path) => {
                let ca = tokio::fs::read(path)
                    .await
                    .map_err(|_| IdentityServiceError::new("read identity operator CA failed"))?;
                tls_config_with_operator_ca(&certificate, &private_key, &ca)?
            }
            None => tls_config(&certificate, &private_key)?,
        };
        drop(private_key);
        let service = IdentityService::connect(config).await?;
        let listener = TcpListener::bind(bind)
            .await
            .map_err(|_| IdentityServiceError::new("bind identity HTTPS listener failed"))?;
        let address = listener
            .local_addr()
            .map_err(|_| IdentityServiceError::new("read identity HTTPS address failed"))?;
        println!("identity listening on {address}");
        return tokio::select! {
            result = serve(listener, service, tls) => result,
            result = tokio::signal::ctrl_c() => result.map_err(|_| IdentityServiceError::new("identity shutdown signal failed")),
        };
    }
    let mut database = connect(&config).await?;
    let output = match cli.command {
        Command::Publish => serde_json::to_value(
            publish_session_key(&mut database.client, &issuer)
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
