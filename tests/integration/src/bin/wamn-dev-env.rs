//! Compatibility entry point for `wamn dev up`.
//!
//! Standing the environment up is a product command now: `wamn dev up` in
//! `wamn-ctl` owns the flags, the standup module and the spawned Gate
//! (wamn-10yt.10.32). This binary parses the same arguments and hands them
//! straight over, so the `[WAMN-DEV-ENVIRONMENT]` recipe keeps working until
//! that recipe is rewritten onto the product binary. It carries no logic of its
//! own and no second path; delete it with that recipe.

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "wamn-dev-env",
    about = "Stand up the disposable environment the wamn dev loop runs against"
)]
struct Cli {
    #[command(flatten)]
    up: wamn_ctl::dev::up::DevUpArgs,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    wamn_ctl::dev::up::run(Cli::parse().up).await
}
