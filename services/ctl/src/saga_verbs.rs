//! Arguments and output of the `saga-resume` and `saga-abandon` verbs
//! (docs/plan/platform-ui.md §5.5).

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;
use wamn_control::environment_saga;

/// One create-environment saga.
#[derive(Args)]
pub struct SagaArgs {
    /// Postgres URL to the system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// The saga id.
    #[arg(long)]
    pub saga: String,
}

impl std::fmt::Debug for SagaArgs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SagaArgs")
            .field("system_database_url", &"[REDACTED]")
            .field("saga", &self.saga)
            .finish()
    }
}

async fn connect(url: &str) -> anyhow::Result<tokio_postgres::Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to the system database")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

/// Return a failed saga to pending.
pub async fn resume(args: SagaArgs) -> anyhow::Result<()> {
    let mut client = connect(&args.system_database_url).await?;
    environment_saga::saga_resume(&mut client, &args.saga).await?;
    println!("resumed {}", args.saga);
    Ok(())
}

/// End a failed or pending saga as abandoned.
pub async fn abandon(args: SagaArgs) -> anyhow::Result<()> {
    let mut client = connect(&args.system_database_url).await?;
    environment_saga::saga_abandon(&mut client, &args.saga).await?;
    println!("abandoned {}", args.saga);
    Ok(())
}
