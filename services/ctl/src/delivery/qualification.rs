//! Execute repository checks against one selected revision and exact candidate.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use clap::Args;
use serde::Serialize;
use tokio::process::Command;

use super::{ApplicationResult, Candidate, CheckResult, Qualification, sqlx};
use crate::dev::{DevSourceState, watch::GitSource};

const RECEIVING_CASES: &[&str] = &[
    "route_authentication_live::cluster::route_cases::command_histories",
    "route_authentication_live::cluster::postcommit_case::baseline_overlay_and_materializer_progress",
];
const WMS_CASES: &[&str] = &["cluster::released_wms_routes"];
// Existing deployed cases own bounded setup and cleanup inside this outer limit.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(45 * 60);

/// Qualify a clean selected revision through the existing application cases.
#[derive(Debug, Args)]
pub struct QualifyReleaseArgs {
    #[arg(long, default_value = ".")]
    pub repository: PathBuf,
    /// Integrated main, a release tag, or an explicitly selected revision.
    #[arg(long, default_value = "main")]
    pub revision: String,
    #[arg(long)]
    pub candidate: PathBuf,
    /// Fresh temporary result file outside tracked source.
    #[arg(long)]
    pub result: PathBuf,
}

/// Run explicitly selected existing behavior checks before integration.
#[derive(Debug, Args)]
pub struct CheckChangesArgs {
    #[arg(long, default_value = ".")]
    pub repository: PathBuf,
    #[arg(long)]
    pub package: String,
    /// Omit for the library test target.
    #[arg(long)]
    pub test: Option<String>,
    #[arg(long = "case", required = true)]
    pub cases: Vec<String>,
    #[arg(long)]
    pub include_ignored: bool,
    /// Test URL variables supplied by a fresh owned PostgreSQL server.
    #[arg(long = "database-url-env")]
    pub database_url_env: Vec<String>,
    #[arg(long)]
    pub result: PathBuf,
}

#[derive(Debug, Serialize)]
struct ChangeResult {
    source_commit: String,
    source_dirty: bool,
    checks: Vec<CheckResult>,
    result: &'static str,
}

fn nonce() -> anyhow::Result<String> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("generate an owned result name"))?;
    Ok(hex::encode(bytes))
}

struct TemporaryResults(PathBuf);

impl TemporaryResults {
    fn create(base: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(base).context("create the allowed evidence directory")?;
        let path = base.join(format!("wamn-qualification-{}", nonce()?));
        fs::create_dir(&path).context("create temporary application results")?;
        Ok(Self(path))
    }
}

impl Drop for TemporaryResults {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Application {
    package: &'static str,
    cases: &'static [&'static str],
    schemas: &'static [(&'static str, &'static str)],
    evidence_env: &'static str,
}

fn application(candidate: &Candidate) -> anyhow::Result<Application> {
    let (manifest, _) = candidate.manifest()?;
    let packages: Vec<_> = manifest
        .release
        .packages
        .iter()
        .map(|package| package.package_id())
        .collect();
    if packages.contains(&"wamn_wms") && !packages.contains(&"wamn_receiving") {
        Ok(Application {
            package: "wamn-wms-tests",
            cases: WMS_CASES,
            schemas: &[("wamn_wms", "wms")],
            evidence_env: "WAMN_WMS_EVIDENCE_DIR",
        })
    } else if packages.contains(&"wamn_receiving")
        && packages.contains(&"client_acme_receiving")
        && !packages.contains(&"wamn_wms")
    {
        Ok(Application {
            package: "wamn-receiving-tests",
            cases: RECEIVING_CASES,
            schemas: &[
                ("wamn_receiving", "receiving"),
                ("client_acme_receiving", "receiving"),
            ],
            evidence_env: "WAMN_RECEIVING_EVIDENCE_DIR",
        })
    } else {
        anyhow::bail!("the candidate has no supported existing application qualification case")
    }
}

/// Refuse partial results before publication can use them.
pub(super) fn require_complete_checks(result: &Qualification) -> anyhow::Result<()> {
    let app = application(&result.candidate)?;
    for case in app.cases {
        ensure!(
            result
                .checks
                .iter()
                .any(|check| check.command.iter().any(|arg| arg == case)
                    && check.command.iter().any(|arg| arg == "--exact")),
            "qualification lacks required case {case}"
        );
    }
    for (package, _) in app.schemas {
        ensure!(
            result.checks.iter().any(|check| check
                .command
                .iter()
                .any(|arg| arg == "materialize_package")
                && check.command.iter().any(|arg| arg == "check")
                && check
                    .command
                    .iter()
                    .any(|arg| arg.ends_with(&format!("apps/{package}")))),
            "qualification lacks generated output comparison for {package}"
        );
        if let Some(verifier) = sqlx::verifier_for(package) {
            ensure!(
                result
                    .checks
                    .iter()
                    .any(|check| check.command.iter().any(|arg| arg == verifier)
                        && check.command.iter().any(|arg| arg == "--check")
                        && check.command.iter().any(|arg| arg == "prepare")),
                "qualification lacks SQLx metadata comparison for {verifier}"
            );
        }
    }
    Ok(())
}

/// Execute fresh required checks and write one machine result, including failures.
pub async fn qualify(args: QualifyReleaseArgs) -> anyhow::Result<()> {
    let output_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&args.result)
        .context("create a fresh qualification result file")?;
    let candidate = Candidate::read(&fs::canonicalize(&args.candidate)?)?;
    let source = GitSource::discover(&args.repository).await?;
    let snapshot = source.snapshot().await?;
    let (manifest, digest) = candidate.manifest()?;
    let mut result = Qualification {
        source_commit: snapshot.source_commit().to_owned(),
        release: manifest.release,
        manifest_digest: digest.as_str().to_owned(),
        artifact_hashes: candidate.artifact_hashes()?,
        candidate,
        checks: Vec::new(),
        result: "fail".to_owned(),
    };
    let run = qualify_candidate(&args, &source, &mut result).await;
    if run.is_ok() {
        result.result = "pass".to_owned();
    }
    if let Err(error) = &run {
        result.checks.push(CheckResult {
            command: vec!["qualify-release".to_owned()],
            result: "fail".to_owned(),
            cause: Some(format!("{error:#}")),
        });
    }
    serde_json::to_writer_pretty(output_file, &result).context("write the qualification result")?;
    run
}

async fn qualify_candidate(
    args: &QualifyReleaseArgs,
    source: &GitSource,
    result: &mut Qualification,
) -> anyhow::Result<()> {
    let snapshot = source.snapshot().await?;
    ensure!(
        snapshot.state() == DevSourceState::Clean,
        "qualification requires a clean selected source revision"
    );
    let root = snapshot.repository_root();
    let selected = Command::new("git")
        .current_dir(root)
        .args([
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", args.revision),
        ])
        .output()
        .await?;
    ensure!(
        selected.status.success()
            && String::from_utf8_lossy(&selected.stdout).trim() == snapshot.source_commit(),
        "the current checkout does not match the selected integrated revision"
    );
    let app = application(&result.candidate)?;
    let target = &result.candidate.target_directory.clone();
    let rust = run(
        root,
        &strings(&["rustc", "--version"]),
        &[],
        &mut result.checks,
    )
    .await?;
    let pin = fs::read_to_string(root.join("rust-toolchain.toml"))?;
    let version = String::from_utf8(rust.stdout)?;
    let actual = version
        .split_whitespace()
        .nth(1)
        .context("read the Rust version")?;
    ensure!(
        pin.lines()
            .any(|line| line.trim() == format!("channel = \"{actual}\"")),
        "Rust differs from rust-toolchain.toml"
    );
    let target_env = vec![("CARGO_TARGET_DIR".to_owned(), target.display().to_string())];
    run(
        root,
        &strings(&[
            "cargo",
            "build",
            "--locked",
            "--offline",
            "-p",
            "wamn-test-infrastructure",
            "--bin",
            "wamn-test-postgres",
        ]),
        &target_env,
        &mut result.checks,
    )
    .await?;
    if app
        .schemas
        .iter()
        .any(|(package, _)| sqlx::verifier_for(package).is_some())
    {
        let sqlx = run(
            root,
            &strings(&["cargo", "sqlx", "--version"]),
            &target_env,
            &mut result.checks,
        )
        .await?;
        sqlx::require_cli_version(&sqlx.stdout)?;
    }
    for &(package, schema) in app.schemas {
        let app_root = root.join("apps").join(package);
        let mut prefix = vec![
            target
                .join("debug/wamn-test-postgres")
                .display()
                .to_string(),
            "--database".to_owned(),
            "delivery_schema".to_owned(),
            "--schema".to_owned(),
            schema.to_owned(),
        ];
        if package == "client_acme_receiving" {
            prefix.extend([
                "--migration-dir".to_owned(),
                root.join("apps/wamn_receiving/migrations")
                    .display()
                    .to_string(),
            ]);
        }
        prefix.extend([
            "--migration-dir".to_owned(),
            app_root.join("migrations").display().to_string(),
            "--history-manifest".to_owned(),
            app_root.join("wamn.json").display().to_string(),
            "--url-env".to_owned(),
            "WAMN_SCHEMA_INTROSPECTION_PG_URL".to_owned(),
            "--url-env".to_owned(),
            "DATABASE_URL".to_owned(),
            "--".to_owned(),
        ]);
        let mut generate = prefix.clone();
        generate.extend(strings(&[
            "cargo",
            "run",
            "--locked",
            "--offline",
            "-p",
            "wamn-schema-generator",
            "--example",
            "materialize_package",
            "--",
            "check",
        ]));
        generate.push(app_root.display().to_string());
        run(root, &generate, &target_env, &mut result.checks).await?;
        if let Some(verifier) = sqlx::verifier_for(package) {
            prefix.extend(sqlx::prepare_arguments(verifier, true));
            let mut prepare_env = target_env.clone();
            prepare_env.push(("SQLX_OFFLINE".to_owned(), "false".to_owned()));
            run(
                &app_root.join("tests"),
                &prefix,
                &prepare_env,
                &mut result.checks,
            )
            .await?;
        }
    }
    // Existing build owners establish which source produced the candidate.
    run(
        root,
        &strings(&["bash", "tools/build-components", "all"]),
        &target_env,
        &mut result.checks,
    )
    .await?;
    run(
        root,
        &strings(&[
            "cargo",
            "build",
            "--locked",
            "--offline",
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
        ]),
        &target_env,
        &mut result.checks,
    )
    .await?;
    result.assert_artifacts()?;
    let images = [
        ("host", Some(result.candidate.host_image.clone())),
        ("gates", result.candidate.gates_image.clone()),
        ("identity", result.candidate.identity_image.clone()),
        ("executor", result.candidate.executor_image.clone()),
    ];
    for (target, image) in images {
        if let Some(image) = image {
            compare_built_image(
                root,
                target,
                &image,
                &result.source_commit,
                &mut result.checks,
            )
            .await?;
        }
    }
    let executable =
        compile_tests(root, app.package, None, &target_env, &mut result.checks).await?;
    // Compiling check harnesses cannot replace deployment artifacts.
    result.assert_artifacts()?;
    let common = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .await?;
    ensure!(
        common.status.success(),
        "resolve the repository evidence owner"
    );
    let common = PathBuf::from(String::from_utf8(common.stdout)?.trim());
    let evidence_root = common
        .parent()
        .context("the common Git directory has a parent")?
        .join("evidence");
    let temporary = TemporaryResults::create(&evidence_root)?;
    for (index, case) in app.cases.iter().enumerate() {
        ensure!(
            Candidate::read(&args.candidate)? == result.candidate,
            "candidate inputs changed during qualification"
        );
        let case_result = temporary.0.join(format!("case-{index}.json"));
        let evidence = temporary.0.join(format!("evidence-{index}"));
        let mut env = target_env.clone();
        env.extend([
            (
                "WAMN_DELIVERY_CANDIDATE".to_owned(),
                fs::canonicalize(&args.candidate)?.display().to_string(),
            ),
            (
                "WAMN_DELIVERY_RESULT".to_owned(),
                case_result.display().to_string(),
            ),
            (app.evidence_env.to_owned(), evidence.display().to_string()),
        ]);
        let command = vec![
            executable.display().to_string(),
            (*case).to_owned(),
            "--exact".to_owned(),
            "--ignored".to_owned(),
            "--nocapture".to_owned(),
            "--test-threads=1".to_owned(),
        ];
        let executed = run(root, &command, &env, &mut result.checks).await?;
        require_one_case(&executed)?;
        let observed: ApplicationResult = serde_json::from_slice(
            &fs::read(&case_result)
                .context("the required application case did not report executed success")?,
        )?;
        let (manifest, digest) = result.candidate.manifest()?;
        ensure!(
            observed.candidate == result.candidate
                && observed.result == "pass"
                && observed.release == manifest.release
                && observed.manifest_digest == digest.as_str(),
            "the application observed another release or did not pass"
        );
        result.assert_artifacts()?;
    }
    let finished = source.snapshot().await?;
    ensure!(
        finished.state() == DevSourceState::Clean
            && finished.source_commit() == result.source_commit,
        "source changed during qualification"
    );
    require_complete_checks(result)
}

/// Execute exact selected tests and distinguish executed success from a skip.
pub async fn check_changes(args: CheckChangesArgs) -> anyhow::Result<()> {
    let output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&args.result)
        .context("create a fresh change result file")?;
    let source = GitSource::discover(&args.repository).await?;
    let snapshot = source.snapshot().await?;
    let mut result = ChangeResult {
        source_commit: snapshot.source_commit().to_owned(),
        source_dirty: snapshot.state() == DevSourceState::Dirty,
        checks: Vec::new(),
        result: "fail",
    };
    let checked = async {
        let root = snapshot.repository_root();
        let binary = compile_tests(
            root,
            &args.package,
            args.test.as_deref(),
            &[],
            &mut result.checks,
        )
        .await?;
        let runner = if args.database_url_env.is_empty() {
            None
        } else {
            let output = run(
                root,
                &strings(&[
                    "cargo",
                    "build",
                    "--locked",
                    "--offline",
                    "-p",
                    "wamn-test-infrastructure",
                    "--bin",
                    "wamn-test-postgres",
                    "--message-format=json",
                ]),
                &[],
                &mut result.checks,
            )
            .await?;
            Some(artifact_executable(&output, "wamn-test-postgres")?)
        };
        for case in &args.cases {
            ensure!(
                !case.trim().is_empty(),
                "a required case name cannot be empty"
            );
            let mut command = Vec::new();
            if let Some(runner) = &runner {
                command.extend([
                    runner.display().to_string(),
                    "--database".to_owned(),
                    "delivery_changes".to_owned(),
                ]);
                for name in &args.database_url_env {
                    command.extend(["--url-env".to_owned(), name.clone()]);
                }
                command.push("--".to_owned());
            }
            command.extend([
                binary.display().to_string(),
                case.clone(),
                "--exact".to_owned(),
                "--nocapture".to_owned(),
                "--test-threads=1".to_owned(),
            ]);
            if args.include_ignored {
                command.push("--include-ignored".to_owned());
            }
            require_one_case(&run(root, &command, &[], &mut result.checks).await?)?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if checked.is_ok() {
        result.result = "pass";
    }
    if let Err(error) = &checked {
        result.checks.push(CheckResult {
            command: vec!["check-changes".to_owned()],
            result: "fail".to_owned(),
            cause: Some(format!("{error:#}")),
        });
    }
    serde_json::to_writer_pretty(output, &result)?;
    checked
}

async fn compare_built_image(
    root: &Path,
    target: &str,
    image: &str,
    source_commit: &str,
    checks: &mut Vec<CheckResult>,
) -> anyhow::Result<()> {
    run(
        root,
        &strings(&["docker", "pull", "--quiet", image]),
        &[],
        checks,
    )
    .await?;
    let expected = run(
        root,
        &strings(&["docker", "image", "inspect", "--format", "{{.Id}}", image]),
        &[],
        checks,
    )
    .await?;
    let tag = format!("wamn-qualification-{target}:{}", nonce()?);
    let built = async {
        run(
            root,
            &strings(&[
                "docker",
                "build",
                "--platform=linux/amd64",
                "--provenance=false",
                "--file",
                "Dockerfile",
                "--target",
                target,
                "--tag",
                &tag,
                "--label",
                &format!("wamn.dev/source-head={source_commit}"),
                "--label",
                "wamn.dev/build-profile=release",
                ".",
            ]),
            &[],
            checks,
        )
        .await?;
        let actual = run(
            root,
            &strings(&["docker", "image", "inspect", "--format", "{{.Id}}", &tag]),
            &[],
            checks,
        )
        .await?;
        ensure!(
            actual.stdout == expected.stdout,
            "the pinned {target} image differs from the selected source build"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    // Remove only this invocation's tag. Never remove a candidate reference or prune caches.
    let cleanup = Command::new("docker")
        .args(["image", "rm", &tag])
        .output()
        .await;
    built?;
    ensure!(
        cleanup
            .context("remove the owned image comparison tag")?
            .status
            .success(),
        "the owned image comparison tag could not be removed"
    );
    Ok(())
}

async fn compile_tests(
    root: &Path,
    package: &str,
    test: Option<&str>,
    env: &[(String, String)],
    checks: &mut Vec<CheckResult>,
) -> anyhow::Result<PathBuf> {
    let mut args = strings(&[
        "cargo",
        "test",
        "--locked",
        "--offline",
        "-p",
        package,
        "--no-run",
        "--message-format=json",
    ]);
    match test {
        Some(test) => args.extend(["--test".to_owned(), test.to_owned()]),
        None => args.push("--lib".to_owned()),
    }
    artifact_executable(
        &run(root, &args, env, checks).await?,
        test.unwrap_or(&package.replace('-', "_")),
    )
}

fn artifact_executable(output: &Output, name: &str) -> anyhow::Result<PathBuf> {
    let executables: Vec<PathBuf> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .filter(|value| value["reason"] == "compiler-artifact" && value["target"]["name"] == name)
        .filter_map(|value| value["executable"].as_str().map(PathBuf::from))
        .collect();
    ensure!(
        executables.len() == 1,
        "Cargo did not produce exactly one selected executable for {name}"
    );
    Ok(executables[0].clone())
}

fn require_one_case(output: &Output) -> anyhow::Result<()> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        output.status.success()
            && stdout
                .lines()
                .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")),
        "the required test did not execute exactly one passing case"
    );
    ensure!(
        !stdout.lines().chain(stderr.lines()).any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("[skip]") || lower.contains("skipping ") || lower.starts_with("skip:")
        }),
        "the required test reported an explicit skip"
    );
    Ok(())
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

async fn run(
    root: &Path,
    argv: &[String],
    env: &[(String, String)],
    checks: &mut Vec<CheckResult>,
) -> anyhow::Result<Output> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(root)
        .kill_on_drop(true)
        .env("CARGO_NET_OFFLINE", "true")
        .env("WAMN_TEST_REQUIRED", "1");
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            name.starts_with("PG") || name.ends_with("_PG_URL") || name.ends_with("DATABASE_URL")
        }) {
            command.env_remove(name);
        }
    }
    // Publication credentials never flow into application checks through these inputs.
    for name in [
        "WAMN_REGISTRY_AUTH_FILE",
        "DOCKER_AUTH_CONFIG",
        "REGISTRY_PASSWORD",
        "WASH_OCI_PASSWORD",
    ] {
        command.env_remove(name);
    }
    command.envs(env.iter().cloned());
    let result = execute_owned(&mut command, COMMAND_TIMEOUT).await;
    let cause = match &result {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(format!(
            "command exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(error) => Some(error.to_string()),
    };
    checks.push(CheckResult {
        command: argv.to_vec(),
        result: if cause.is_none() { "pass" } else { "fail" }.to_owned(),
        cause,
    });
    let output = result?;
    ensure!(
        output.status.success(),
        "required command failed: {}",
        argv.join(" ")
    );
    Ok(output)
}

/// Give each selected case time to remove its own resources on cancellation.
pub async fn execute_owned(command: &mut Command, timeout: Duration) -> anyhow::Result<Output> {
    crate::owned_command::execute(command, timeout, Duration::from_secs(60)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt as _;

    #[test]
    fn required_case_refuses_empty_ignored_skipped_and_failed_execution() {
        let output = |stdout: &str, stderr: &str, code| Output {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            status: std::process::ExitStatus::from_raw(code),
        };
        let pass =
            "test result: ok. 1 passed; 0 failed; 0 ignored; 7 filtered out; finished in 0.1s";
        assert!(require_one_case(&output(pass, "", 0)).is_ok());
        for failed in [
            output("test result: ok. 0 passed; 0 failed; 0 ignored;", "", 0),
            output("test result: ok. 0 passed; 0 failed; 1 ignored;", "", 0),
            output(pass, "[skip] missing database", 0),
            output(pass, "", 256),
        ] {
            assert!(require_one_case(&failed).is_err());
        }
    }
}
