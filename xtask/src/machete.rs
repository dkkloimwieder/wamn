//! Unused dependency checks for both Cargo workspaces.

use anyhow::{Context as _, Result, ensure};
use cargo_metadata::MetadataCommand;
use std::fs;
use std::path::Path;
use tokio::process::Command;

pub(super) async fn run(root: &Path) -> Result<()> {
    let pins: toml::Value = toml::from_str(&fs::read_to_string(root.join("mise.toml"))?)?;
    let pinned = pins
        .get("tools")
        .and_then(|tools| tools.get("http:cargo-machete"))
        .and_then(|tool| tool.get("version"))
        .and_then(toml::Value::as_str)
        .context("mise.toml is missing the cargo-machete version pin")?;
    let version = command(root)
        .arg("--version")
        .output()
        .await
        .with_context(|| format!("cargo-machete {pinned} is required. Run mise install"))?;
    ensure!(
        version.status.success(),
        "cargo-machete {pinned} is required. Run mise install\n{}",
        String::from_utf8_lossy(&version.stderr)
    );
    let installed = String::from_utf8_lossy(&version.stdout);
    ensure!(
        installed.trim() == pinned,
        "mise.toml pins cargo-machete {pinned}, but the installed version is {}. Run mise install",
        installed.trim()
    );

    let mut failures = Vec::new();
    for manifest in ["Cargo.toml", "apps/Cargo.toml"] {
        let workspace = MetadataCommand::new()
            .current_dir(root)
            .manifest_path(manifest)
            .no_deps()
            .exec()
            .with_context(|| format!("read workspace members from {manifest}"))?;
        let packages = workspace.workspace_packages();
        let mut command = command(workspace.workspace_root.as_std_path());
        command.arg("--with-metadata");
        for package in &packages {
            command.arg(&package.manifest_path);
        }
        let output = command
            .output()
            .await
            .with_context(|| format!("run cargo machete for {manifest}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success()
            && !stdout
                .lines()
                .chain(stderr.lines())
                .any(|line| line.starts_with("error when handling "))
        {
            println!(
                "{manifest}: checked {} workspace members, no unused dependencies",
                packages.len()
            );
        } else {
            failures.push(format!(
                "{manifest}: cargo machete reported findings or scan errors ({})\n{stderr}{stdout}",
                output.status
            ));
        }
    }
    ensure!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

fn command(directory: &Path) -> Command {
    let mut command = Command::new("cargo");
    // Cargo sets CARGO_PKG_NAME when running xtask. Machete uses its absence
    // to recognize that Cargo invoked it as an external subcommand.
    command
        .current_dir(directory)
        .env_remove("CARGO_PKG_NAME")
        .arg("machete");
    command
}
