//! The builds of the Receiving delivery case, which publishes the release of
//! this checkout. The qualification cases build nothing.

use std::fmt::Write as _;

use std::fs::{self, File};
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
) -> anyhow::Result<Artifacts> {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map_or_else(
            || repository.join("target"),
            |path| {
                if path.is_absolute() {
                    path
                } else {
                    repository.join(path)
                }
            },
        );
    {
        // `wamn build` of the packages compiles their components into the
        // target directory, against a PostgreSQL server of its own. The
        // workload guests are no package's.
        let mut packages = Command::new("cargo");
        packages
            .args([
                "run",
                "--locked",
                "--offline",
                "-p",
                "wamn-test-infrastructure",
                "--bin",
                "wamn-test-postgres",
                "--",
                "--database",
                "receiving_cluster_build",
                "--url-env",
                "DATABASE_URL",
                "--",
            ])
            .args(["cargo", "run", "--locked", "--offline", "-p", "wamn-ctl"])
            .args(["--bin", "wamn", "--", "build"])
            .args(["apps/wamn_receiving", "apps/client_acme_receiving"]);
        run_build(
            &mut packages,
            repository,
            &target,
            &evidence.join("wamn-build.log"),
        )
        .await?;
        for package in ["http-route", "materializer"] {
            wamn_schema_generator::build::build_guest(
                repository,
                &repository.join("apps/Cargo.toml"),
                package,
                Some(&target),
            )
            .await?;
        }
        let mut tools = Command::new(repository.join("tools/delivery-owned"));
        tools.arg("build-native").arg(&target);
        run_build(
            &mut tools,
            repository,
            &target,
            &evidence.join("build-native-tools.log"),
        )
        .await?;
    }
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
        writeln!(
            component_hashes,
            "{}  {}",
            hex::encode(Sha256::digest(fs::read(&path)?)),
            path.display()
        )
        .expect("writing to a String cannot fail");
    }
    fs::write(evidence.join("component-bytes.sha256"), component_hashes)?;
    Ok(artifacts)
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
