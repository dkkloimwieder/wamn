//! `wamn-edge`: serve one release bundle until the process is interrupted.
//!
//! `wamn-edge intents ...` lists and resolves uncertain intents instead, while
//! the edge is stopped.

use anyhow::{Context as _, bail};
use wamn_edge::intents;
use wamn_edge::serve::{EdgeConfig, serve};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.split_first() {
        None => {
            let host = serve(EdgeConfig::from_env()?).await?;
            tokio::signal::ctrl_c().await?;
            host.stop().await
        }
        Some((command, rest)) if command == "intents" => {
            let db = std::env::var("WAMN_EDGE_DB").context("read WAMN_EDGE_DB")?;
            print!("{}", intents::run(rest, db.as_ref()).await?);
            Ok(())
        }
        Some(_) => bail!(intents::USAGE),
    }
}
