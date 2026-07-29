//! Deploy-facing composition for the callable-flow Wave-2 gate.

use clap::Args;
use wamn_proof_system::callable_wave2;

use crate::callable_wave1;

#[derive(Debug, Args)]
pub struct CallableWave2Args {
    #[command(flatten)]
    pub identity: callable_wave2::CallableWave2IdentityArgs,

    /// Application-role PostgreSQL URL used by the production invocation provider.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// Administrative PostgreSQL URL used for from-zero proof databases.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,
}

pub async fn run(args: CallableWave2Args) -> anyhow::Result<()> {
    callable_wave1::prove_runtime(
        args.identity.flowrunner.clone(),
        args.database_url,
        args.admin_database_url,
        "wamn_callable_flow_wave2",
    )
    .await?;
    callable_wave2::run(args.identity)?;
    Ok(())
}
