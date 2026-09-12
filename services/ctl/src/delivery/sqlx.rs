//! Shared SQLx verifier commands for local generation and release qualification.

use std::path::Path;

use anyhow::ensure;
use tokio::process::Command;

/// Return the existing verifier target for an application package.
pub fn verifier_for(package_id: &str) -> Option<&'static str> {
    match package_id {
        "wamn_receiving" => Some("receiving_sqlx_verifier"),
        "client_acme_receiving" => Some("client_acme_sqlx_verifier"),
        _ => None,
    }
}

/// Build the complete command arguments for the selected SQLx verifier.
pub fn prepare_arguments(verifier: &str, check: bool) -> Vec<String> {
    let mut arguments = vec!["cargo".to_owned(), "sqlx".to_owned(), "prepare".to_owned()];
    if check {
        arguments.push("--check".to_owned());
    }
    arguments.extend(
        ["--", "--test", verifier, "--locked", "--offline"]
            .into_iter()
            .map(str::to_owned),
    );
    arguments
}

/// Require the pinned CLI version before preparing metadata.
pub fn require_cli_version(output: &[u8]) -> anyhow::Result<()> {
    ensure!(
        matches!(
            String::from_utf8_lossy(output).trim(),
            "sqlx-cli 0.9.0" | "sqlx-cli-sqlx 0.9.0"
        ),
        "SQLx CLI 0.9.0 is required"
    );
    Ok(())
}

/// Configure preparation while leaving execution and cleanup to the caller.
pub fn prepare_command(
    test_dir: &Path,
    database_url: &str,
    verifier: &str,
    check: bool,
) -> Command {
    let arguments = prepare_arguments(verifier, check);
    let mut command = Command::new(&arguments[0]);
    command
        .args(&arguments[1..])
        .current_dir(test_dir)
        .env("DATABASE_URL", database_url)
        .env("SQLX_OFFLINE", "false")
        .env("CARGO_NET_OFFLINE", "true");
    command
}
