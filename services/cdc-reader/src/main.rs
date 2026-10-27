//! wamn-cdc-reader: the CDC event-reader service binary (SR9).
//!
//! Pre-split this ran as `wamn-host event-reader`; the flags are unchanged,
//! the `event-reader` subcommand literal is gone (single-purpose binary).

use std::str::FromStr as _;

use clap::Parser;

#[derive(Parser)]
#[command(name = "wamn-cdc-reader", version, about)]
struct Cli {
    /// Log level (the chart passes this before the service flags)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(flatten)]
    args: wamn_cdc_reader::EventReaderArgs,
}

fn init_tracing(level: tracing::Level) {
    // Same shape as the pre-split no-OTEL path of
    // wash_runtime::observability::initialize_observability: stderr fmt layer,
    // RUST_LOG overriding --log-level.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.as_str()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .log_internal_errors(false)
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    init_tracing(level);

    wamn_cdc_reader::run(cli.args).await
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::fs::OpenOptions;
    use std::process::Command;

    #[test]
    fn logging_survives_output_failure() {
        const CHILD: &str = "WAMN_CDC_READER_LOG_FAILURE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            super::init_tracing(tracing::Level::INFO);
            tracing::info!("the failed log write must not panic");
            return;
        }

        // /dev/full fails both the event write and any internal diagnostic.
        // A subprocess keeps the failing stderr away from the parent harness.
        let stderr = OpenOptions::new().write(true).open("/dev/full").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::logging_survives_output_failure",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("RUST_LOG", "info")
            .stderr(stderr)
            .output()
            .expect("run the logging test with failing stderr");
        assert!(
            output.status.success(),
            "logging subprocess failed: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
