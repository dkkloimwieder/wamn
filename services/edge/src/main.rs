//! `wamn-edge [--config <path>]`: serve one release bundle until the process
//! is interrupted.
//!
//! `wamn-edge [--config <path>] intents ...` lists and resolves uncertain
//! intents instead, and `samples ...` lists and resolves refused samples,
//! while the edge is stopped. Without `--config`,
//! `WAMN_EDGE_CONFIG` names the file, and without either, every key comes from
//! its `WAMN_EDGE_*` variable.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use wamn_edge::config::{CONFIG_VARIABLE, EdgeConfig};
use wamn_edge::serve::serve;
use wamn_edge::{intents, refusals};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let path = if args.first().map(String::as_str) == Some("--config") {
        let path = args.get(1).context("--config names a file")?;
        let path = PathBuf::from(path);
        args.drain(..2);
        Some(path)
    } else {
        std::env::var_os(CONFIG_VARIABLE).map(PathBuf::from)
    };
    let config = EdgeConfig::load(path.as_deref())?;
    match args.split_first() {
        None => {
            let host = serve(config).await?;
            tokio::signal::ctrl_c().await?;
            host.stop().await
        }
        Some((command, rest)) if command == "intents" => {
            print!("{}", intents::run(rest, &config.store.db).await?);
            Ok(())
        }
        Some((command, rest)) if command == "samples" => {
            print!("{}", refusals::run(rest, &config.store.db).await?);
            Ok(())
        }
        Some(_) => bail!(intents::USAGE),
    }
}
