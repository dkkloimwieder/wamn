//! Start the org control host and serve local built web clients.

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Refuse missing web bytes before provisioning an environment.
pub(super) fn preflight(args: &super::up::DevUpRequest) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.platform_ui.join("index.html").is_file(),
        "build the platform UI before dev up: {} has no index.html",
        args.platform_ui.display()
    );
    for package in &args.packages {
        if package.join("web").is_dir() {
            wamn_compiler::build_record::require_current(package)?;
            let dist = wamn_package_control::web_artifact::artifact_output(package).join("dist");
            anyhow::ensure!(
                dist.join("index.html").is_file(),
                "run wamn web build --package {} before dev up",
                package.display()
            );
        }
    }
    Ok(())
}

/// Start the deployed control binary with local HTTP component bytes.
pub(super) fn start(
    args: &super::up::DevUpRequest,
    environment: &super::environment::DevEnvironment,
) -> anyhow::Result<String> {
    let _ = (args, environment);
    anyhow::bail!("no credential store in local mode")
}

/// Stop the supervisor through its private socket and wait for child cleanup.
pub async fn stop(root: &Path) -> anyhow::Result<()> {
    let socket = root.join("web.sock");
    if !socket.exists() {
        return Ok(());
    }
    let mut stream = tokio::net::UnixStream::connect(&socket)
        .await
        .context("connect the owned web supervisor")?;
    stream.write_all(b"stop\n").await?;
    let mut result = String::new();
    tokio::time::timeout(Duration::from_secs(80), stream.read_to_string(&mut result))
        .await
        .context("control and web shutdown timed out")??;
    anyhow::ensure!(
        result == "stopped\n",
        "control and web cleanup failed: {result}"
    );
    Ok(())
}
