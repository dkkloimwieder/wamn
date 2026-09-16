//! Retained self-contained builds for the WMS cluster cases.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio::process::Command;

pub(super) async fn build(
    repository: &Path,
    target: &Path,
    evidence: &Path,
    generated_terminal: bool,
    delivery: bool,
) -> anyhow::Result<()> {
    if let Some(candidate) = wamn_control::delivery::Candidate::from_env()? {
        candidate.artifact_hashes()?;
        for name in [
            "wamn-ctl",
            "wamn-identity",
            "wamn-cdc-reader",
            "wamn-scenario-worker",
        ] {
            let path = target.join("debug").join(name);
            let metadata = fs::metadata(&path)
                .with_context(|| format!("read supplied helper {}", path.display()))?;
            ensure!(
                metadata.is_file()
                    && metadata.len() > 0
                    && metadata.permissions().mode() & 0o111 != 0,
                "the supplied helper is not an executable file: {}",
                path.display()
            );
        }
        for relative in [
            "virtualized/std-empty-environment/wms.wasm",
            "virtualized/std-empty-environment/blob_put.wasm",
            "wasm32-wasip2/release/label_render.wasm",
            "wasm32-wasip2/release/http_route.wasm",
            "wasm32-wasip2/release/materializer.wasm",
        ] {
            let path = target.join(relative);
            ensure!(
                !fs::read(&path)
                    .with_context(|| format!("read supplied component {}", path.display()))?
                    .is_empty(),
                "the supplied component is empty: {}",
                path.display()
            );
        }
        return Ok(());
    }
    let mut guests = Command::new(repository.join("tools/build-components"));
    guests.arg("all");
    prepare(&mut guests, repository, target);
    run(&mut guests, evidence, "guests").await?;

    let mut native = Command::new("cargo");
    native.args(["build", "--locked", "--offline"]);
    if !delivery {
        native.args(["-p", "wamn-host"]);
    }
    native.args([
        "-p",
        "wamn-ctl",
        "-p",
        "wamn-identity",
        "-p",
        "wamn-cdc-reader",
        "-p",
        "wamn-scenario-worker",
    ]);
    if delivery {
        native.args(["-p", "wamn-executor"]);
    }
    prepare(&mut native, repository, target);
    run(&mut native, evidence, "native").await?;
    if generated_terminal {
        let mut terminal = Command::new("cargo");
        terminal.args([
            "build",
            "--locked",
            "--offline",
            "-p",
            "wamn-client-terminal",
            "--example",
            "wms_move",
        ]);
        prepare(&mut terminal, repository, target);
        run(&mut terminal, evidence, "terminal").await?;
    }
    let mut components = Vec::new();
    for directory in [
        target.join("wasm32-wasip2/release"),
        target.join("virtualized/std-empty-environment"),
    ] {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "wasm")
                && path.is_file()
            {
                let bytes = fs::read(&path)?;
                ensure!(
                    !bytes.is_empty(),
                    "a built guest artifact is empty: {}",
                    path.display()
                );
                components.push((path, bytes.len(), hex::encode(Sha256::digest(bytes))));
            }
        }
    }
    components.sort_by(|left, right| left.0.cmp(&right.0));
    crate::wms_runtime_live::write_result(evidence, "component-bytes.json", &json!(components))?;
    if delivery {
        return Ok(());
    }
    let host = target.join("debug/wamn-host");
    let bytes = fs::read(&host).context("read the host produced by the native build")?;
    crate::wms_runtime_live::write_result(
        evidence,
        "host-binary.json",
        &json!({
            "path":host,"bytes":bytes.len(),"sha256":hex::encode(Sha256::digest(bytes)),
        }),
    )
}

fn prepare(command: &mut Command, repository: &Path, target: &Path) {
    command
        .current_dir(repository)
        .env("CARGO_TARGET_DIR", target)
        .env("RUSTC_WRAPPER", "")
        .env("CARGO_BUILD_JOBS", "2");
    for (name, _) in std::env::vars_os() {
        let key = name.to_string_lossy();
        if key.starts_with("WAMN")
            || key.starts_with("PG")
            || key.starts_with("OTEL")
            || key == "DATABASE_URL"
        {
            command.env_remove(name);
        }
    }
}

async fn run(command: &mut Command, evidence: &Path, name: &str) -> anyhow::Result<()> {
    let stdout = fs::File::create(evidence.join(format!("build-{name}.stdout")))?;
    let stderr = fs::File::create(evidence.join(format!("build-{name}.stderr")))?;
    let argv = std::iter::once(command.as_std().get_program())
        .chain(command.as_std().get_args())
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let started = std::time::Instant::now();
    let status = command
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true)
        .status()
        .await
        .with_context(|| format!("start the retained {name} build"))?;
    crate::wms_runtime_live::write_result(
        evidence,
        &format!("build-{name}.json"),
        &json!({
            "argv":argv,"exit":status.code(),"success":status.success(),"elapsed_seconds":started.elapsed().as_secs_f64(),
        }),
    )?;
    ensure!(
        status.success(),
        "the retained {name} build failed: {status}"
    );
    Ok(())
}
