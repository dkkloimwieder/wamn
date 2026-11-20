//! wamn-host: the production host binary.
//!
//! MVP outcome: crash minimum · M0 execution · flow composition.
//!
//! `host` runs workloads and serves HTTP.
//! The runtime-operator controls ClusterHost over NATS. Identity and CDC
//! remain separate services.
//!
//! The one-shot control-plane verbs (provision*, apply-package, publish-release,
//! enable-cdc-project-env) live in `wamn`
//! (SR9); this artifact ships none of them.
//!

use std::str::FromStr as _;

use clap::{Parser, Subcommand};

mod host;
mod observability;

#[derive(Parser)]
#[command(name = "wamn-host", version, about)]
struct Cli {
    /// Log level (the chart passes this before the subcommand)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the operator-managed wasmCloud host.
    Host(Box<host::HostArgs>),
}

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let cli = Cli::parse();
    // wash-runtime builds its log filter from RUST_LOG, else from the level,
    // and then quiets `async_nats` to error. The more specific directive keeps
    // the cause of each failed NATS attempt, which async-nats logs at debug
    // (wamn-gdex). A RUST_LOG the deployment sets wins.
    let log_filter = format!("{},async_nats::connector=debug", cli.log_level);
    // wash-runtime reads residency settings from the process environment.
    // Keep the defaults used by deployed hosts when launching outside Helm, too.
    for (key, default) in [
        ("RUST_LOG", log_filter.as_str()),
        ("WASMTIME_POOLING_LINEAR_MEMORY_KEEP_RESIDENT", "65536"),
        ("WASMTIME_POOLING_TABLE_KEEP_RESIDENT", "4096"),
        ("WASMTIME_POOLING_ASYNC_STACK_KEEP_RESIDENT", "8192"),
    ] {
        if std::env::var_os(key).is_none() {
            // SAFETY: no other thread exists yet; the runtime starts below.
            #[expect(
                unsafe_code,
                reason = "No other thread exists yet because the runtime starts below."
            )]
            unsafe {
                std::env::set_var(key, default);
            }
        }
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .event_interval(1)
        .enable_all()
        .build()?;
    let result = runtime.block_on(async_main(cli));
    runtime.shutdown_timeout(wamn_engine::lifecycle::RUNTIME_SHUTDOWN_BUDGET);
    result
}

async fn async_main(cli: Cli) -> anyhow::Result<()> {
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // OTel exporters activate when OTEL_* env vars are present.
    let _shutdown_observability = observability::initialize(level)?;

    let result = match cli.command {
        Command::Host(args) => Box::pin(host::run(*args)).await,
    };

    wamn_engine::lifecycle::finish(result).await
}
