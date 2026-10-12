//! Execute repository checks against one exact candidate.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::Serialize;
use tokio::process::Command;

use super::qualification_artifact::{
    self, QUALIFICATION_PAYLOAD_FORMAT, QualificationPayload, QualificationPushDisposition,
};
use super::{ApplicationResult, Candidate, CheckResult, Qualification};
use crate::git_source::{GitSourceState, discover_repository_root, read_status};
use crate::package_artifact::PackageRegistry;

const RECEIVING_CASES: &[&str] = &[
    "route_authentication_live::cluster::route_cases::command_histories",
    "route_authentication_live::cluster::postcommit_case::baseline_overlay_and_materializer_progress",
];
// The Receiving cases that touch no overlay, for a release without Acme.
// queue_recovery is in neither set: it starts its item from the wiring
// wamn_receiving/receiving_record_receipt, which no release carries
// (wamn-ld93.33.11).
const RECEIVING_BASE_CASES: &[&str] =
    &["route_authentication_live::cluster::route_cases::command_histories"];
const WMS_CASES: &[&str] = &[
    "cluster::released_wms_routes",
    "cluster::released_wms_routes_retain_committed_work_after_label_failure",
    "cluster::restarted_wms_host_retains_compiled_code_and_serves_requests",
];
// Existing deployed cases own bounded setup and cleanup inside this outer limit.
const COMMAND_TIMEOUT: Duration = Duration::from_mins(45);
/// Workspace directories, from the repository root, whose members change checks can select.
const TEST_WORKSPACES: &[&str] = &["", "apps"];

/// Exact inputs for qualifying one published release, by its digest, on the
/// platform image set of a release chart through the existing application
/// cases (docs/plan/platform-deploy.md §8.2).
#[derive(Clone, Debug)]
pub struct QualifyReleaseRequest {
    /// The release, `sha256:<hex>`.
    pub release_digest: String,
    /// The release repository: `publish-release` pushed the manifest there,
    /// and the qualification goes beside it.
    pub releases: PackageRegistry,
    /// The release chart whose default values name the platform image set
    /// the cases run on, and the gates, identity and runner images (R15).
    pub release_chart: PathBuf,
}

/// Exact selected existing behavior checks to run before integration.
#[derive(Clone, Debug)]
pub struct CheckChangesRequest {
    pub repository: PathBuf,
    /// A member of the root or apps workspace.
    pub package: String,
    /// Omit for the library test target.
    pub test: Option<String>,
    pub cases: Vec<String>,
    pub include_ignored: bool,
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

struct TemporaryResults {
    path: PathBuf,
    kept: bool,
}

impl TemporaryResults {
    fn create() -> anyhow::Result<Self> {
        let path = std::env::temp_dir().join(format!("wamn-qualification-{}", nonce()?));
        fs::create_dir(&path).context("create temporary application results")?;
        Ok(Self { path, kept: false })
    }

    /// Keeps the results of a failed case and names them in the cause of the
    /// failed check and in the error (wamn-e6iw).
    fn keep(mut self, checks: &mut [CheckResult], error: anyhow::Error) -> anyhow::Error {
        self.kept = true;
        let kept = format!("the case results are kept in {}", self.path.display());
        if let Some(cause) = checks.last_mut().and_then(|check| check.cause.as_mut()) {
            cause.push_str("; ");
            cause.push_str(&kept);
        }
        error.context(kept)
    }
}

impl Drop for TemporaryResults {
    fn drop(&mut self) {
        if !self.kept {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

struct Application {
    package: &'static str,
    cases: &'static [&'static str],
    evidence_env: &'static str,
}

fn application(candidate: &Candidate) -> anyhow::Result<Application> {
    let packages = candidate_packages(candidate)?;
    application_for(&packages.iter().map(String::as_str).collect::<Vec<_>>())
}

/// The package ids of the candidate release, sorted and without repeats.
fn candidate_packages(candidate: &Candidate) -> anyhow::Result<Vec<String>> {
    let (manifest, _) = candidate.manifest()?;
    Ok(manifest
        .release
        .packages
        .iter()
        .map(|package| package.package_id().to_owned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect())
}

/// The cases that qualify a release, chosen by its exact package set in
/// sorted order. No case runs that needs a package the release does not carry.
fn application_for(packages: &[&str]) -> anyhow::Result<Application> {
    match packages {
        ["wamn_wms"] => Ok(Application {
            package: "wamn-wms-tests",
            cases: WMS_CASES,
            evidence_env: "WAMN_WMS_EVIDENCE_DIR",
        }),
        ["wamn_receiving"] => Ok(Application {
            package: "wamn-receiving-tests",
            cases: RECEIVING_BASE_CASES,
            evidence_env: "WAMN_RECEIVING_EVIDENCE_DIR",
        }),
        ["client_acme_receiving", "wamn_receiving"] => Ok(Application {
            package: "wamn-receiving-tests",
            cases: RECEIVING_CASES,
            evidence_env: "WAMN_RECEIVING_EVIDENCE_DIR",
        }),
        _ => {
            anyhow::bail!("the candidate has no supported existing application qualification case")
        }
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
    Ok(())
}

/// Execute fresh required checks and, after every case passed, push the
/// qualification beside the release (§8.2). Returns the qualification's
/// manifest digest. A failed qualification pushes nothing.
pub async fn qualify(
    request: QualifyReleaseRequest,
) -> anyhow::Result<(String, QualificationPushDisposition)> {
    let temporary = TemporaryResults::create()?;
    let candidate_path = temporary.path.join("candidate.json");
    let (candidate, release_size) =
        write_candidate(&request, &temporary.path, &candidate_path).await?;
    let (manifest, digest) = candidate.manifest()?;
    let mut result = Qualification {
        release: manifest.release,
        manifest_digest: digest.as_str().to_owned(),
        candidate,
        checks: Vec::new(),
        result: "fail".to_owned(),
    };
    qualify_candidate(
        &request.release_chart,
        &candidate_path,
        temporary,
        &mut result,
    )
    .await?;
    let payload = QualificationPayload {
        format: QUALIFICATION_PAYLOAD_FORMAT,
        release_digest: result.manifest_digest.clone(),
        image_set: qualification_artifact::image_set_digests(&result.candidate.images)?,
        cases: application(&result.candidate)?
            .cases
            .iter()
            .map(|case| (*case).to_owned())
            .collect(),
    };
    qualification_artifact::push(&request.releases, &payload, release_size).await
}

/// Pull the release by its digest into `directory` and write the candidate
/// the cases read: the release, its registry, and the chart's image set and
/// qualification images. Returns it with the length of the release bytes.
async fn write_candidate(
    request: &QualifyReleaseRequest,
    directory: &Path,
    candidate_path: &Path,
) -> anyhow::Result<(Candidate, usize)> {
    let releases = &request.releases;
    let bytes = wamn_runtime::release_manifest_source::ReleaseManifestSource::new(
        &releases.artifact_base,
        releases.insecure_registry,
        &releases.registry_auth_file,
    )
    .context("open the release manifest source")?
    .with_ca_paths(&releases.oci_ca_paths)
    .context("read the registry CA")?
    .pull_verified(&request.release_digest)
    .await
    .with_context(|| format!("pull release {}", request.release_digest))?;
    let manifest_path = directory.join("manifest.json");
    fs::write(&manifest_path, &bytes)?;
    let candidate = Candidate {
        manifest_path,
        releases: request.releases.clone(),
        images: crate::release_chart::image_set(&request.release_chart).await?,
        qualification: crate::release_chart::qualification_images(&request.release_chart).await?,
    };
    let output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(candidate_path)?;
    serde_json::to_writer_pretty(output, &candidate).context("write the candidate")?;
    Ok((Candidate::read(candidate_path)?, bytes.len()))
}

/// The directory where the runner image keeps the case executables, each
/// named after its package (the `runner` stage of the root `Dockerfile`).
const RUNNER_CASES: &str = "/cases";

/// The Docker daemon's socket, which the runner reaches the daemon through.
const DOCKER_SOCKET: &str = "/var/run/docker.sock";

/// The user the runner runs as: the caller's user and group, so that the
/// files it leaves in the results directory are the caller's, and the group
/// of the Docker socket, so that it reaches the daemon (§8.2).
#[derive(Clone, Copy, Debug)]
struct RunnerUser {
    uid: u32,
    gid: u32,
    socket_gid: u32,
}

fn runner_user() -> anyhow::Result<RunnerUser> {
    use std::os::unix::fs::MetadataExt as _;
    let socket = fs::metadata(DOCKER_SOCKET)
        .with_context(|| format!("read the group of {DOCKER_SOCKET}"))?;
    Ok(RunnerUser {
        uid: rustix::process::getuid().as_raw(),
        gid: rustix::process::getgid().as_raw(),
        socket_gid: socket.gid(),
    })
}

/// The file name of the identity executable in the results directory.
const IDENTITY_BINARY: &str = "wamn-identity";

/// The `docker run` that copies the identity executable out of the identity
/// image the chart pins into the results directory, as `user`. A case that
/// provisions an environment mints its PATs through a local identity process,
/// and that process is this executable, never one the runner builds or
/// carries (§8.2, R15).
fn identity_command(candidate: &Candidate, user: RunnerUser, results: &Path) -> Vec<String> {
    let mut command = strings(&["docker", "run", "--rm"]);
    command.push(format!("--user={}:{}", user.uid, user.gid));
    command.push(format!("--volume={0}:{0}", results.display()));
    command.push("--entrypoint=cp".to_owned());
    command.push(candidate.qualification.identity.clone());
    command.push("/usr/local/bin/wamn-identity".to_owned());
    command.push(results.join(IDENTITY_BINARY).display().to_string());
    command
}

/// The `docker run` of the case executable of `package` in the candidate's
/// runner image as `user`, with `args` and `env`. The runner reaches the
/// Docker daemon through its socket and the case clusters through the host
/// network. The
/// results directory is mounted at its own path, and is the runner's
/// temporary directory, because the daemon resolves the cases' bind mounts
/// on its own host. The release chart and the release registry's access are
/// mounted read-only at their own paths.
fn runner_command(
    candidate: &Candidate,
    user: RunnerUser,
    release_chart: &Path,
    results: &Path,
    package: &str,
    args: &[&str],
    env: &[(&str, String)],
) -> Vec<String> {
    let mut command = strings(&[
        "docker",
        "run",
        "--rm",
        "--network=host",
    ]);
    command.push(format!("--user={}:{}", user.uid, user.gid));
    command.push(format!("--group-add={}", user.socket_gid));
    command.push(format!("--volume={DOCKER_SOCKET}:{DOCKER_SOCKET}"));
    command.push(format!("--volume={0}:{0}", results.display()));
    let read_only = std::iter::once(release_chart)
        .chain(std::iter::once(
            candidate.releases.registry_auth_file.as_path(),
        ))
        .chain(candidate.releases.oci_ca_paths.iter().map(PathBuf::as_path));
    for path in read_only {
        command.push(format!("--volume={0}:{0}:ro", path.display()));
    }
    command.push(format!("--env=TMPDIR={}", results.display()));
    command.push(format!(
        "--env=WAMN_IDENTITY_BINARY={}",
        results.join(IDENTITY_BINARY).display()
    ));
    command.push(format!(
        "--env=WAMN_RELEASE_CHART={}",
        release_chart.display()
    ));
    for (name, value) in env {
        command.push(format!("--env={name}={value}"));
    }
    command.push(format!("--entrypoint={RUNNER_CASES}/{package}"));
    command.push(candidate.qualification.runner.clone());
    command.extend(args.iter().map(|arg| (*arg).to_owned()));
    command
}

async fn qualify_candidate(
    release_chart: &Path,
    candidate_path: &Path,
    temporary: TemporaryResults,
    result: &mut Qualification,
) -> anyhow::Result<()> {
    let app = application(&result.candidate)?;
    let user = runner_user()?;
    let root = temporary.path.clone();
    for command in image_commands(&result.candidate) {
        run(&root, &command, &[], &mut result.checks).await?;
    }
    run(
        &root,
        &identity_command(&result.candidate, user, &temporary.path),
        &[],
        &mut result.checks,
    )
    .await?;
    // A case the executable lacks refuses here, before any case starts a cluster (wamn-pgub).
    let listed = run(
        &root,
        &runner_command(
            &result.candidate,
            user,
            release_chart,
            &temporary.path,
            app.package,
            &["--list", "--format=terse", "--ignored"],
            &[],
        ),
        &[],
        &mut result.checks,
    )
    .await?;
    require_listed_cases(&String::from_utf8_lossy(&listed.stdout), app.cases)?;
    for (index, case) in app.cases.iter().enumerate() {
        let checked: anyhow::Result<()> = async {
            ensure!(
                Candidate::read(candidate_path)? == result.candidate,
                "candidate inputs changed during qualification"
            );
            let case_result = temporary.path.join(format!("case-{index}.json"));
            let env = [
                (
                    "WAMN_DELIVERY_CANDIDATE",
                    candidate_path.display().to_string(),
                ),
                ("WAMN_DELIVERY_RESULT", case_result.display().to_string()),
                // The case creates its own new result directory inside this parent.
                (app.evidence_env, temporary.path.display().to_string()),
            ];
            let command = runner_command(
                &result.candidate,
                user,
                release_chart,
                &temporary.path,
                app.package,
                &[
                    case,
                    "--exact",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ],
                &env,
            );
            let executed = run(&root, &command, &[], &mut result.checks).await?;
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
            Ok(())
        }
        .await;
        if let Err(error) = checked {
            return Err(temporary.keep(&mut result.checks, error));
        }
    }
    require_complete_checks(result)
}

/// Execute exact selected tests and distinguish executed success from a skip.
pub async fn check_changes(request: CheckChangesRequest) -> anyhow::Result<()> {
    let output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&request.result)
        .context("create a fresh change result file")?;
    let snapshot = read_status(&discover_repository_root(&request.repository).await?).await?;
    let mut result = ChangeResult {
        source_commit: snapshot.source_commit().to_owned(),
        source_dirty: snapshot.state() == GitSourceState::Dirty,
        checks: Vec::new(),
        result: "fail",
    };
    let checked = async {
        let root = snapshot.repository_root();
        let workspace = package_workspace(root, &request.package, &mut result.checks).await?;
        let binary = compile_tests(
            &workspace,
            &request.package,
            request.test.as_deref(),
            &[],
            &mut result.checks,
        )
        .await?;
        for case in &request.cases {
            ensure!(
                !case.trim().is_empty(),
                "a required case name cannot be empty"
            );
            let mut command = vec![
                binary.display().to_string(),
                case.clone(),
                "--exact".to_owned(),
                "--nocapture".to_owned(),
                "--test-threads=1".to_owned(),
            ];
            if request.include_ignored {
                command.push("--include-ignored".to_owned());
            }
            require_one_case(&run(&workspace, &command, &[], &mut result.checks).await?)?;
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

/// Select the one test workspace whose members include the package.
async fn package_workspace(
    root: &Path,
    package: &str,
    checks: &mut Vec<CheckResult>,
) -> anyhow::Result<PathBuf> {
    let mut metadata = Vec::new();
    for workspace in TEST_WORKSPACES {
        let workspace = root.join(workspace);
        let mut command = strings(&[
            "cargo",
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version=1",
            "--manifest-path",
        ]);
        command.push(workspace.join("Cargo.toml").display().to_string());
        metadata.push((workspace, run(root, &command, &[], checks).await?.stdout));
    }
    member_workspace(package, &metadata)
}

fn member_workspace(package: &str, metadata: &[(PathBuf, Vec<u8>)]) -> anyhow::Result<PathBuf> {
    let mut workspaces = Vec::new();
    for (workspace, stdout) in metadata {
        let value: serde_json::Value =
            serde_json::from_slice(stdout).context("read the workspace Cargo metadata")?;
        let members = value["packages"]
            .as_array()
            .context("the workspace Cargo metadata lists its members")?;
        if members.iter().any(|member| member["name"] == package) {
            workspaces.push(workspace);
        }
    }
    ensure!(
        workspaces.len() == 1,
        "{package} is not a member of exactly one test workspace"
    );
    Ok(workspaces[0].clone())
}

/// The commands that record each image of the candidate. The candidate names
/// every image by its pushed digest, so qualification pulls each one and
/// records its image id, and builds none (owner ruling of 2026-10-03 on
/// `wamn-t3ox`).
fn image_commands(candidate: &Candidate) -> Vec<Vec<String>> {
    [
        &candidate.images.host,
        &candidate.qualification.identity,
        &candidate.qualification.gates,
        &candidate.qualification.runner,
    ]
    .into_iter()
    .flat_map(|image| {
        [
            strings(&["docker", "pull", "--quiet", image]),
            strings(&["docker", "image", "inspect", "--format", "{{.Id}}", image]),
        ]
    })
    .collect()
}

async fn compile_tests(
    root: &Path,
    package: &str,
    test: Option<&str>,
    env: &[(String, String)],
    checks: &mut Vec<CheckResult>,
) -> anyhow::Result<PathBuf> {
    artifact_executable(
        &run(root, &test_build_command(package, test), env, checks).await?,
        test.unwrap_or(&package.replace('-', "_")),
    )
}

/// The `cargo test --no-run` command that builds one test executable.
fn test_build_command(package: &str, test: Option<&str>) -> Vec<String> {
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
    args
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

/// Refuse a case name that the test executable does not list as an ignored test.
fn require_listed_cases(listing: &str, cases: &[&str]) -> anyhow::Result<()> {
    let missing: Vec<&str> = cases
        .iter()
        .copied()
        .filter(|case| !listing.lines().any(|line| line == format!("{case}: test")))
        .collect();
    ensure!(
        missing.is_empty(),
        "the test executable does not contain the qualification cases {}",
        missing.join(", ")
    );
    Ok(())
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
        .env("CARGO_NET_OFFLINE", "true");
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
        "required command failed: {}\n{}",
        argv.join(" "),
        output_tail(&output)
    );
    Ok(output)
}

/// The last lines of a failed command's stdout and stderr, so the error names
/// the failure line (wamn-5c5z1).
fn output_tail(output: &Output) -> String {
    const LINES: usize = 40;
    [("stdout", &output.stdout), ("stderr", &output.stderr)]
        .into_iter()
        .map(|(name, bytes)| {
            let text = String::from_utf8_lossy(bytes);
            let lines: Vec<&str> = text.lines().collect();
            let tail = lines[lines.len().saturating_sub(LINES)..].join("\n");
            format!("--- {name} (last {LINES} lines) ---\n{tail}")
        })
        .collect::<Vec<_>>()
        .join("\n")
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

    #[test]
    fn a_candidate_with_pinned_digests_qualifies_by_pulling_them() {
        let pinned = |name: &str| format!("registry.example/{name}@sha256:{}", "a".repeat(64));
        let candidate = Candidate {
            manifest_path: "unused".into(),
            releases: PackageRegistry {
                artifact_base: "registry.example/wamn/releases".to_owned(),
                registry_auth_file: "unused".into(),
                insecure_registry: false,
                oci_ca_paths: Vec::new(),
            },
            images: crate::release_chart::ImageSet {
                host: pinned("host"),
                http: pinned("flow-http"),
                materializer: pinned("materializer"),
            },
            qualification: crate::release_chart::QualificationImages {
                runner: pinned("runner"),
                gates: pinned("gates"),
                identity: pinned("identity"),
            },
        };
        let expected: Vec<Vec<String>> = ["host", "identity", "gates", "runner"]
            .into_iter()
            .flat_map(|name| {
                let image = pinned(name);
                [
                    strings(&["docker", "pull", "--quiet", &image]),
                    strings(&["docker", "image", "inspect", "--format", "{{.Id}}", &image]),
                ]
            })
            .collect();
        assert_eq!(image_commands(&candidate), expected);

        let user = RunnerUser {
            uid: 1000,
            gid: 1001,
            socket_gid: 984,
        };
        // The identity executable comes from the image the chart pins.
        assert_eq!(
            identity_command(&candidate, user, Path::new("/tmp/results")),
            strings(&[
                "docker",
                "run",
                "--rm",
                "--user=1000:1001",
                "--volume=/tmp/results:/tmp/results",
                "--entrypoint=cp",
                &pinned("identity"),
                "/usr/local/bin/wamn-identity",
                "/tmp/results/wamn-identity",
            ])
        );

        // A case runs in the runner image, never in a checkout.
        let command = runner_command(
            &candidate,
            user,
            Path::new("/charts/release"),
            Path::new("/tmp/results"),
            "wamn-wms-tests",
            &["cluster::released_wms_routes", "--exact"],
            &[(
                "WAMN_DELIVERY_RESULT",
                "/tmp/results/case-0.json".to_owned(),
            )],
        );
        assert_eq!(&command[..2], ["docker", "run"]);
        for expected in [
            "--network=host",
            "--user=1000:1001",
            "--group-add=984",
            "--volume=/var/run/docker.sock:/var/run/docker.sock",
            "--volume=/tmp/results:/tmp/results",
            "--volume=/charts/release:/charts/release:ro",
            "--volume=unused:unused:ro",
            "--env=TMPDIR=/tmp/results",
            "--env=WAMN_IDENTITY_BINARY=/tmp/results/wamn-identity",
            "--env=WAMN_RELEASE_CHART=/charts/release",
            "--env=WAMN_DELIVERY_RESULT=/tmp/results/case-0.json",
            "--entrypoint=/cases/wamn-wms-tests",
        ] {
            assert!(command.iter().any(|arg| arg == expected), "{expected}");
        }
        assert_eq!(
            &command[command.len() - 3..],
            [
                pinned("runner").as_str(),
                "cluster::released_wms_routes",
                "--exact"
            ]
        );
    }

    #[test]
    fn qualification_refuses_a_case_that_the_executable_does_not_list() {
        let listing = "cluster::released_wms_routes: test\nfixture::helper: test\n";
        require_listed_cases(listing, &["cluster::released_wms_routes"]).unwrap();
        let error =
            require_listed_cases(listing, &["cluster::released_wms_routes", "cluster::gone"])
                .unwrap_err()
                .to_string();
        assert_eq!(
            error,
            "the test executable does not contain the qualification cases cluster::gone"
        );
        assert!(require_listed_cases("", WMS_CASES).is_err());
    }

    #[test]
    fn the_package_set_selects_its_cases_exactly() {
        let cases = |packages: &[&str]| application_for(packages).map(|app| app.cases);
        assert_eq!(cases(&["wamn_wms"]).unwrap(), WMS_CASES);
        assert_eq!(cases(&["wamn_receiving"]).unwrap(), RECEIVING_BASE_CASES);
        assert_eq!(
            cases(&["client_acme_receiving", "wamn_receiving"]).unwrap(),
            RECEIVING_CASES
        );
        for refused in [
            &["client_acme_receiving"][..],
            &["wamn_receiving", "wamn_wms"],
            &["client_acme_receiving", "wamn_receiving", "wamn_wms"],
            &[],
        ] {
            assert!(cases(refused).is_err(), "accepted {refused:?}");
        }
    }

    #[test]
    fn change_checks_select_the_one_workspace_that_has_the_package() {
        let workspaces = |root: &str, apps: &str| {
            [("/r", root), ("/r/apps", apps)].map(|(workspace, member)| {
                let metadata = serde_json::json!({"packages": [{"name": member}]});
                (
                    PathBuf::from(workspace),
                    serde_json::to_vec(&metadata).unwrap(),
                )
            })
        };
        let split = workspaces("wamn-ctl", "wamn-platform-fixture-data-access");
        assert_eq!(
            member_workspace("wamn-ctl", &split).unwrap(),
            Path::new("/r")
        );
        assert_eq!(
            member_workspace("wamn-platform-fixture-data-access", &split).unwrap(),
            Path::new("/r/apps")
        );
        assert!(member_workspace("wamn-absent-data-access", &split).is_err());
        assert!(member_workspace("same", &workspaces("same", "same")).is_err());
    }

    #[test]
    fn a_failed_command_names_its_output_tail() {
        let stdout = (0..50)
            .map(|line| format!("out {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = output_tail(&Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: stdout.into_bytes(),
            stderr: b"missing prerequisites: program jq on PATH\n".to_vec(),
        });
        assert!(!tail.contains("out 9\n") && tail.contains("out 10\n") && tail.contains("out 49"));
        assert!(tail.ends_with(
            "--- stderr (last 40 lines) ---\nmissing prerequisites: program jq on PATH"
        ));
    }

    #[test]
    fn a_failed_case_keeps_its_results_and_names_them_in_the_cause() {
        let passed = TemporaryResults::create().expect("create results");
        let removed = passed.path.clone();
        drop(passed);
        assert!(!removed.exists(), "a passing run removes its results");

        let failed = TemporaryResults::create().expect("create results");
        let kept = failed.path.clone();
        fs::write(kept.join("case-0.json"), "{}").expect("write a case result");
        let mut checks = vec![CheckResult {
            command: strings(&["case"]),
            result: "fail".to_owned(),
            cause: Some("command exited 101".to_owned()),
        }];
        let error = failed.keep(&mut checks, anyhow::anyhow!("the case failed"));
        let named = format!("the case results are kept in {}", kept.display());
        assert!(
            kept.join("case-0.json").is_file(),
            "a failed run keeps its results"
        );
        assert_eq!(
            checks[0].cause.as_deref(),
            Some(format!("command exited 101; {named}").as_str())
        );
        assert_eq!(format!("{error:#}"), format!("{named}: the case failed"));
        fs::remove_dir_all(&kept).expect("remove the kept results");
    }
}
