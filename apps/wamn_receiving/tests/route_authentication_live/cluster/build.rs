//! Builds used by the Receiving cluster tests.

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context as _, ensure};
use sha2::{Digest as _, Sha256};
use tokio::process::Command;

#[derive(Debug)]
pub(super) struct Artifacts {
    pub target: PathBuf,
    pub components: PathBuf,
    pub http: PathBuf,
    pub materializer: PathBuf,
}

/// Keep the complete guest build and the existing native build profiles.
pub(super) async fn components_and_tools(
    repository: &Path,
    evidence: &Path,
    standard_images: bool,
) -> anyhow::Result<Artifacts> {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                repository.join(path)
            }
        })
        .unwrap_or_else(|| repository.join("target"));
    let mut guests = Command::new(repository.join("tools/build-components"));
    guests.arg("proof");
    run_build(
        &mut guests,
        repository,
        &target,
        &evidence.join("build-components.log"),
    )
    .await?;
    if !standard_images {
        let mut host = Command::new("cargo");
        host.args(["build", "--locked", "--release", "-p", "wamn-host"]);
        run_build(
            &mut host,
            repository,
            &target,
            &evidence.join("build-host.log"),
        )
        .await?;
    }
    let mut tools = Command::new("cargo");
    tools.args([
        "build",
        "--locked",
        "-p",
        "wamn-ctl",
        "-p",
        "wamn-identity",
        "-p",
        "wamn-cdc-reader",
        "-p",
        "wamn-scenario-worker",
        "-p",
        "wamn-executor",
    ]);
    run_build(
        &mut tools,
        repository,
        &target,
        &evidence.join("build-native-tools.log"),
    )
    .await?;
    let artifacts = Artifacts {
        components: target.join("virtualized/std-empty-environment"),
        http: target.join("wasm32-wasip2/release/http_route.wasm"),
        materializer: target.join("wasm32-wasip2/release/materializer.wasm"),
        target,
    };
    let component_paths = [
        artifacts.components.join("receiving.wasm"),
        artifacts.components.join("client_acme_receiving.wasm"),
        artifacts.http.clone(),
        artifacts.materializer.clone(),
    ];
    let mut component_hashes = String::new();
    for path in component_paths {
        ensure!(
            fs::metadata(&path)
                .with_context(|| format!("read built component {}", path.display()))?
                .len()
                > 0,
            "the built component is empty: {}",
            path.display()
        );
        component_hashes.push_str(&format!(
            "{}  {}\n",
            hex::encode(Sha256::digest(fs::read(&path)?)),
            path.display()
        ));
    }
    fs::write(evidence.join("component-bytes.sha256"), component_hashes)?;
    Ok(artifacts)
}

pub(super) fn prepare_host_image(work: &Path, target: &Path, source: &str) -> anyhow::Result<()> {
    let directory = work.join("host-image");
    fs::create_dir(&directory)?;
    fs::copy(
        target.join("release/wamn-host"),
        directory.join("wamn-host"),
    )?;
    fs::set_permissions(
        directory.join("wamn-host"),
        fs::Permissions::from_mode(0o755),
    )?;
    fs::write(
        directory.join("Dockerfile"),
        format!(
            "FROM debian:trixie-slim\n\
         RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \\\n\
          && rm -rf /var/lib/apt/lists/*\n\
         COPY wamn-host /usr/local/bin/wamn-host\n\
         ENV HOME=/tmp\n\
         LABEL wamn.dev/source-head=\"{source}\" wamn.dev/build-profile=\"release\"\n\
         ENTRYPOINT [\"/usr/local/bin/wamn-host\"]\n"
        ),
    )?;
    Ok(())
}

async fn run_build(
    command: &mut Command,
    repository: &Path,
    target: &Path,
    log: &Path,
) -> anyhow::Result<()> {
    let output = File::create(log)?;
    let status = command
        .current_dir(repository)
        .env("CARGO_TARGET_DIR", target)
        .env("RUSTC_WRAPPER", "")
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .kill_on_drop(true)
        .status()
        .await
        .context("run the existing Receiving build")?;
    ensure!(
        status.success(),
        "Receiving build failed with {status}; see {}",
        log.display()
    );
    Ok(())
}
