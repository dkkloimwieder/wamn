//! Local identity has no credential store. Cluster deployment owns issuer credentials.
use anyhow::Context as _;
use std::path::{Path, PathBuf};
use wamn_identity_client::PatIssuerConfig;

#[derive(Debug)]
pub struct Bootstrap {
    pub args: PatIssuerConfig,
}
impl Bootstrap {
    pub fn stop(self) -> anyhow::Result<()> {
        Ok(())
    }
    pub fn retain(self, _root: &Path) -> anyhow::Result<()> {
        anyhow::bail!("no credential store in local mode")
    }
}
pub fn preflight(_system_url: &str) -> anyhow::Result<()> {
    anyhow::bail!("no credential store in local mode")
}
pub fn start(_system_url: &str, _root: &Path) -> anyhow::Result<Bootstrap> {
    anyhow::bail!("no credential store in local mode")
}

pub fn stop_environment(_root: &Path) -> anyhow::Result<()> {
    Ok(())
}

pub fn identity_binary() -> anyhow::Result<PathBuf> {
    let binary = if let Some(path) = std::env::var_os("WAMN_IDENTITY_BINARY") {
        PathBuf::from(path)
    } else {
        let executable = std::env::current_exe().context("locate the current executable")?;
        let directory = executable.parent().context("locate the binary directory")?;
        let directory = if directory.file_name().is_some_and(|name| name == "deps") {
            directory
                .parent()
                .context("locate the Cargo binary directory")?
        } else {
            directory
        };
        directory.join("wamn-identity")
    };
    anyhow::ensure!(
        binary.is_file(),
        "build wamn-identity beside the CLI or set WAMN_IDENTITY_BINARY to its executable"
    );
    Ok(binary)
}
