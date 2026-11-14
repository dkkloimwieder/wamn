//! Operator commands and repository checks.

use crate::process;
use anyhow::{Context as _, ensure};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use tokio::process::Command;

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum Mode {
    #[default]
    Run,
    DryRun,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(value_enum, default_value = "run")]
    pub mode: Mode,
}

#[derive(Debug, Args)]
pub struct UpstreamArgs {
    #[arg(value_enum, default_value = "run")]
    mode: Mode,
    #[arg(default_value = "../wasmcloud")]
    checkout: std::path::PathBuf,
}

pub fn describe(command: &Command) -> String {
    let command = command.as_std();
    let mut parts = Vec::new();
    let quote =
        |value: &std::ffi::OsStr| format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"));
    for (name, value) in command.get_envs() {
        if name == "RUSTFLAGS"
            && let Some(value) = value
        {
            parts.push(format!("RUSTFLAGS={}", quote(value)));
        }
    }
    parts.push(quote(command.get_program()));
    parts.extend(command.get_args().map(quote));
    parts.join(" ")
}

pub async fn step(command: &mut Command, mode: Mode) -> anyhow::Result<()> {
    if matches!(mode, Mode::DryRun) {
        println!("{}", describe(command));
    } else {
        process::execute(command).await?;
    }
    Ok(())
}

pub async fn lint(root: &Path, mode: Mode) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for manifest in ["Cargo.toml", "apps/Cargo.toml"] {
        let mut format = Command::new("cargo");
        format.current_dir(root).args([
            "fmt",
            "--manifest-path",
            manifest,
            "--all",
            "--",
            "--check",
        ]);
        if let Err(error) = step(&mut format, mode).await {
            failures.push(format!("{manifest}: {error:#}"));
        }
        let variants: &[(&[&str], Option<&str>)] = match manifest {
            "Cargo.toml" => &[(&["--workspace", "--all-targets"], None)],
            "apps/Cargo.toml" => &[
                (
                    &[
                        "--workspace",
                        "--exclude",
                        "connection-http-standard",
                        "--all-targets",
                    ],
                    None,
                ),
                (
                    &["--package", "connection-http-standard"],
                    Some("-C panic=abort"),
                ),
                (
                    &[
                        "--package",
                        "connection-http-standard",
                        "--target",
                        "wasm32-wasip2",
                    ],
                    None,
                ),
                (
                    &[
                        "--workspace",
                        "--exclude",
                        "connection-http-standard",
                        "--target",
                        "wasm32-wasip2",
                    ],
                    None,
                ),
            ],
            _ => unreachable!("known Cargo workspace"),
        };
        for (args, flags) in variants {
            let mut command = Command::new("cargo");
            command
                .current_dir(root)
                .args(["clippy", "--manifest-path", manifest, "--locked"])
                .args(*args)
                .args(["--", "-D", "warnings"]);
            if let Some(flags) = flags {
                command.env("RUSTFLAGS", flags);
            }
            if let Err(error) = step(&mut command, mode).await {
                failures.push(format!("{manifest}: {error:#}"));
            }
        }
    }
    if matches!(mode, Mode::Run) {
        if let Err(error) = dependencies(root).await {
            failures.push(format!("{error:#}"));
        }
    } else {
        println!("cargo xtask repo-lint-dependencies");
    }
    let mut policy = Command::new("cargo");
    policy
        .current_dir(root)
        .args([
            "run",
            "--locked",
            "--quiet",
            "-p",
            "wamn-conformance-tests",
            "--bin",
            "repo-policy",
            "--",
        ])
        .arg(root);
    if let Err(error) = step(&mut policy, mode).await {
        failures.push(format!("{error:#}"));
    }
    ensure!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

pub async fn packages(root: &Path, manifest: Option<&Path>) -> anyhow::Result<()> {
    let manifest = manifest.unwrap_or_else(|| Path::new("Cargo.toml"));
    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(root)
        .manifest_path(manifest)
        .no_deps()
        .exec()?;
    let mut failures = Vec::new();
    for package in metadata.workspace_packages() {
        let mut command = Command::new("cargo");
        command
            .current_dir(root)
            .args(["check", "--locked", "--quiet", "--manifest-path"])
            .arg(manifest)
            .args(["-p", package.name.as_ref()]);
        if let Err(error) = process::execute(&mut command).await {
            failures.push(format!("{}: {error:#}", package.name));
        }
    }
    ensure!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

async fn linked(root: &Path, package: &str, extra: &[&str]) -> anyhow::Result<BTreeSet<String>> {
    let mut args = vec![
        "tree",
        "-p",
        package,
        "-e",
        "normal",
        "--prefix",
        "none",
        "--locked",
        "--offline",
    ];
    args.extend(extra);
    Ok(process::output(root, "cargo", &args)
        .await?
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect())
}

pub async fn dependencies(root: &Path) -> anyhow::Result<()> {
    let network = [
        "tokio-postgres",
        "deadpool-postgres",
        "postgres-types",
        "async-nats",
        "object_store",
        "redis",
        "oci-client",
        "oci-wasm",
        "opentelemetry-otlp",
        "tonic",
    ];
    for (package, extra, allowed) in [
        (
            "wamn-edge",
            &[][..],
            &["async-nats", "opentelemetry-otlp", "redis", "tonic"][..],
        ),
        (
            "wamn-edge",
            &["--target", "aarch64-unknown-linux-gnu"][..],
            &["async-nats", "opentelemetry-otlp", "redis", "tonic"][..],
        ),
        (
            "wamn-engine",
            &[][..],
            &[
                "async-nats",
                "hyper-rustls",
                "hyper-util",
                "opentelemetry-otlp",
                "redis",
                "tonic",
            ][..],
        ),
    ] {
        let mut refused = network.to_vec();
        if package == "wamn-edge" {
            refused.extend([
                "wamn-workflow",
                "wamn-execution-host",
                "wamn-platform-identity",
            ]);
        } else {
            refused.extend(["hyper-util", "hyper-rustls", "reqwest", "wamn-workflow"]);
        }
        let mut pruned = extra.to_vec();
        pruned.extend(["--prune", "wash-runtime"]);
        let outside = linked(root, package, &pruned).await?;
        ensure!(
            !refused.iter().any(|name| outside.contains(*name)),
            "{package} links forbidden dependencies outside wash-runtime: {:?}",
            refused
                .iter()
                .filter(|name| outside.contains(**name))
                .collect::<Vec<_>>()
        );
        let all = linked(root, package, extra).await?;
        ensure!(
            !refused
                .iter()
                .any(|name| all.contains(*name) && !allowed.contains(name)),
            "{package} links forbidden dependencies through wash-runtime"
        );
    }
    for package in [
        "wamn-postgres-capability",
        "wamn-http-capability",
        "wamn-blobstore",
        "wamn-authentication",
        "wamn-events-capability",
        "wamn-execution-host",
    ] {
        let all = linked(root, package, &[]).await?;
        ensure!(
            !["wamn-workflow"].iter().any(|name| all.contains(*name)),
            "{package} links the workflow layer"
        );
    }
    Ok(())
}

pub async fn contract(root: &Path, mode: Mode) -> anyhow::Result<()> {
    let mut command = Command::new("cargo");
    command.current_dir(root).args([
        "test",
        "--manifest-path",
        "apps/Cargo.toml",
        "--locked",
        "--offline",
        "-p",
        "http-route",
        "--test",
        "adversarial",
    ]);
    if matches!(mode, Mode::DryRun) {
        return step(&mut command, mode).await;
    }
    process::validate(&process::execute(&mut command).await?, None, false)
}

pub async fn upstream(root: &Path, args: &UpstreamArgs) -> anyhow::Result<()> {
    // Cargo.toml owns the upstream URL and release tag.
    let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml"))?)?;
    let upstream = &manifest["workspace"]["dependencies"]["wash-runtime"];
    let url = upstream["git"]
        .as_str()
        .context("wash-runtime repository")?;
    let tag = upstream["tag"].as_str().context("wash-runtime tag")?;
    let check = async || -> anyhow::Result<()> {
        ensure!(
            process::output(&args.checkout, "git", &["remote", "get-url", "origin"])
                .await?
                .trim()
                .trim_end_matches(".git")
                == url,
            "origin is not upstream"
        );
        let release = process::output(
            &args.checkout,
            "git",
            &["rev-parse", &format!("{tag}^{{commit}}")],
        )
        .await?;
        ensure!(
            process::output(&args.checkout, "git", &["rev-parse", "HEAD"]).await? == release,
            "checkout is not {tag}"
        );
        ensure!(
            process::output(
                &args.checkout,
                "git",
                &["status", "--porcelain", "--untracked-files=all"]
            )
            .await?
            .is_empty(),
            "upstream checkout is dirty"
        );
        Ok(())
    };
    let mut failures = Vec::new();
    for arguments in [
        vec!["fmt", "--all", "--", "--check"],
        vec![
            "run",
            "--locked",
            "--package",
            "xtask",
            "--",
            "build-fixtures",
        ],
        vec![
            "tree",
            "--locked",
            "-p",
            "wash-runtime",
            "--features",
            "default",
            "-e",
            "features",
        ],
        vec![
            "test",
            "--locked",
            "-p",
            "wash-runtime",
            "--features",
            "default",
            "--no-fail-fast",
            "--",
            "--include-ignored",
            "--nocapture",
        ],
        vec![
            "tree",
            "--locked",
            "-p",
            "wash",
            "--features",
            "default",
            "-e",
            "features",
        ],
        vec![
            "test",
            "--locked",
            "-p",
            "wash",
            "--features",
            "default",
            "--lib",
            "clone_template_",
            "--",
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ],
    ] {
        if matches!(args.mode, Mode::Run) {
            check().await?;
        }
        let mut command = Command::new("cargo");
        command
            .current_dir(&args.checkout)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(&arguments);
        let result = if matches!(args.mode, Mode::DryRun) {
            step(&mut command, args.mode).await
        } else {
            match process::execute(&mut command).await {
                Ok(text) if arguments[0] == "test" => process::validate(&text, None, false),
                Ok(_) => Ok(()),
                Err(error) => Err(error),
            }
        };
        if let Err(error) = result {
            failures.push(format!("{error:#}"));
        }
    }
    if matches!(args.mode, Mode::Run) {
        check().await?;
    }
    ensure!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

pub fn collector(root: &Path, output: &Path) -> anyhow::Result<()> {
    let directory = output.join("local-artifacts/otel");
    std::fs::create_dir_all(&directory)?;
    let document = wamn_platform_install::platform::document::ClusterDocument::compile(
        &root.join("deploy/kind/cluster.k"),
    )?;
    let mut values: Value = serde_yaml::from_slice(&std::fs::read(
        root.join("deploy/platform/installation/values-gcp.yaml"),
    )?)?;
    values["org"] = document.org.into();
    values["registry"] = document.registry.into();
    let entry = wamn_platform_install::environment::entry(
        &root.join("deploy/platform/installation"),
        "platform",
        "wamn",
        values,
    )?;
    let documents = wamn_kubernetes::helm::template(&entry)?;
    let config = documents
        .iter()
        .find_map(|doc| doc.pointer("/data/relay").and_then(Value::as_str))
        .context("collector configuration")?;
    let mut config: Value = serde_yaml::from_str(config)?;
    config["exporters"] = json!({"file/traces":{"path":"/var/lib/otelcol/traces.jsonl","format":"json"},"file/logs":{"path":"/var/lib/otelcol/logs.jsonl","format":"json"},"nop":{}});
    for signal in ["traces", "logs"] {
        config["service"]["pipelines"][signal]["exporters"] = json!([format!("file/{signal}")]);
    }
    std::fs::write(
        directory.join("config.yaml"),
        serde_yaml::to_string(&config)?,
    )?;
    let image = documents
        .iter()
        .find_map(|doc| {
            doc.pointer("/spec/template/spec/containers/0/image")
                .and_then(Value::as_str)
        })
        .context("collector image")?;
    let user = process_user()?;
    let compose = json!({"services":{"receiving-dev-collector":{"image":image,"pull_policy":"never","user":user,"command":["--config=/var/lib/otelcol/config.yaml"],"environment":{"CLUSTER":document.cluster_name},"volumes":[{"type":"bind","source":directory.canonicalize()?,"target":"/var/lib/otelcol"}],"ports":["127.0.0.1:${WAMN_RECEIVING_DEV_OTLP_PORT:-4319}:4317"]}}});
    std::fs::write(
        directory.join("compose.json"),
        serde_json::to_vec_pretty(&compose)?,
    )?;
    println!("{image}");
    Ok(())
}

fn process_user() -> anyhow::Result<String> {
    let user = std::process::Command::new("id").arg("-u").output()?;
    let group = std::process::Command::new("id").arg("-g").output()?;
    ensure!(
        user.status.success() && group.status.success(),
        "read process user"
    );
    Ok(format!(
        "{}:{}",
        String::from_utf8(user.stdout)?.trim(),
        String::from_utf8(group.stdout)?.trim()
    ))
}

pub async fn delete_cluster(root: &Path, cluster: &str) -> anyhow::Result<()> {
    ensure!(
        !cluster.is_empty() && cluster != "wamn",
        "refusing cluster name {cluster}"
    );
    for attempt in 1..=3 {
        let result = process::run(
            root,
            "timeout",
            &[
                "--kill-after=5s",
                "120s",
                "kind",
                "delete",
                "cluster",
                "--name",
                cluster,
            ],
        )
        .await;
        if result.is_ok() {
            break;
        }
        if attempt < 3 {
            tokio::time::sleep(std::time::Duration::from_secs(attempt * 2)).await;
        }
    }
    let clusters = process::output(
        root,
        "timeout",
        &["--kill-after=1s", "5s", "kind", "get", "clusters"],
    )
    .await?;
    let containers = process::output(
        root,
        "timeout",
        &[
            "--kill-after=1s",
            "5s",
            "docker",
            "ps",
            "-a",
            "--filter",
            &format!("label=io.x-k8s.kind.cluster={cluster}"),
            "--format",
            "{{.Names}}",
        ],
    )
    .await?;
    ensure!(
        !clusters.lines().any(|name| name == cluster) && containers.trim().is_empty(),
        "leftover cluster resources: {containers}"
    );
    Ok(())
}

pub async fn lane(worktree: &Path, print_only: bool) -> anyhow::Result<()> {
    let worktree = worktree.canonicalize()?;
    ensure!(
        Path::new(
            process::output(&worktree, "git", &["rev-parse", "--show-toplevel"])
                .await?
                .trim()
        )
        .canonicalize()?
            == worktree,
        "use the worktree root"
    );
    if !print_only {
        let status = Command::new("git")
            .current_dir(&worktree)
            .args(["submodule", "update", "--init", "--recursive"])
            .stdout(std::io::stderr())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .status()
            .await
            .context("initialize submodules")?;
        ensure!(status.success(), "initialize submodules: {status}");
    }
    let digest = wamn_digest::bytes_sha256(worktree.as_os_str().as_encoded_bytes());
    let key = &digest[7..23];
    let home = std::env::var("HOME")?;
    println!(
        "export CARGO_TARGET_DIR='{}'\nexport WAMN_CHECKOUT_KEY='{key}'",
        format!("{home}/.cache/wamn-target/{key}").replace('\'', "'\\''")
    );
    Ok(())
}

pub async fn cranelift(root: &Path, arguments: &[String]) -> anyhow::Result<()> {
    for argument in arguments {
        ensure!(
            ![
                "-r",
                "--release",
                "--profile",
                "--target",
                "--manifest-path",
                "--config"
            ]
            .iter()
            .any(|option| argument == option || argument.starts_with(&format!("{option}="))),
            "Cranelift is debug/native only: {argument}"
        );
    }
    process::execute(
        Command::new("cargo")
            .current_dir(root)
            .env("RUSTFLAGS", "-Zcodegen-backend=cranelift")
            .args(["+nightly", "build"])
            .args(arguments),
    )
    .await?;
    Ok(())
}
