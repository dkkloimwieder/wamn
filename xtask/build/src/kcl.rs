//! Install the KCL artifact pinned by the repository tool configuration.
use anyhow::{Context as _, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;

/// Install KCL once and return the repository-local binary.
pub async fn install(repository: &Path) -> Result<PathBuf> {
    let pins: toml::Value = toml::from_str(&fs::read_to_string(repository.join("mise.toml"))?)?;
    let pin = &pins["tools"]["http:kcl"];
    let version = pin["version"]
        .as_str()
        .context("KCL has no pinned version")?;
    let checksum = pin["checksum"].as_str().context("KCL has no checksum")?;
    let directory = repository.join(".tools/kcl").join(version);
    let binary = directory.join("kcl");
    if binary.is_file() {
        return Ok(binary);
    }
    fs::create_dir_all(&directory)?;
    let staging = tempfile::tempdir_in(&directory)?;
    let archive = staging.path().join("kcl.tar.gz");
    let download = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--retry",
            "2",
        ])
        .arg(pin["url"].as_str().context("KCL has no download URL")?)
        .arg("--output")
        .arg(&archive)
        .output()
        .await?;
    ensure!(
        download.status.success(),
        "download KCL: {}",
        String::from_utf8_lossy(&download.stderr)
    );
    let bytes = fs::read(&archive)?;
    ensure!(
        wamn_digest::bytes_sha256(&bytes) == checksum,
        "KCL archive differs from its pinned checksum"
    );
    let unpack = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(staging.path())
        .arg("kcl")
        .output()
        .await?;
    fs::remove_file(archive)?;
    ensure!(
        unpack.status.success(),
        "unpack KCL: {}",
        String::from_utf8_lossy(&unpack.stderr)
    );
    fs::rename(staging.path().join("kcl"), &binary)?;
    Ok(binary)
}
