//! Deploy-facing composition for the callable-flow Wave-1 gate.

use clap::Args;
use wamn_proof_integration::invocationproof;
use wamn_proof_system::{callable_wave1, pocsuiteproof};

#[derive(Debug, Args)]
pub struct CallableWave1Args {
    #[command(flatten)]
    pub identity: callable_wave1::CallableWave1IdentityArgs,

    /// Exact flowrunner component baked into the gates image.
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: std::path::PathBuf,

    /// Application-role PostgreSQL URL used by the production invocation provider.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// Administrative PostgreSQL URL used for from-zero proof databases.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,
}

pub async fn run(args: CallableWave1Args) -> anyhow::Result<()> {
    pocsuiteproof::run(pocsuiteproof::CallableFlowSchemaArgs {
        admin_database_url: args.admin_database_url.clone(),
        schema: "wamn_callable_flow_wave1".to_string(),
        keep: false,
    })
    .await?;
    invocationproof::run(invocationproof::InvocationProofArgs {
        flowrunner: args.flowrunner,
        database_url: args.database_url,
        admin_database_url: args.admin_database_url,
    })
    .await?;
    callable_wave1::run(args.identity)?;
    Ok(())
}
