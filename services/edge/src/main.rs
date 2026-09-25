//! `wamn-edge`: serve one release bundle until the process is interrupted.

use wamn_edge::serve::{EdgeConfig, serve};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let host = serve(EdgeConfig::from_env()?).await?;
    tokio::signal::ctrl_c().await?;
    host.stop().await
}
