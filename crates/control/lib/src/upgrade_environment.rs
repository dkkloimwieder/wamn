//! `wamn-ctl upgrade-environment`: one installed environment to the bytes of
//! one commit (docs/plan/upgrade-environment.md).
//!
//! Each stage of §4.2 is a function over the run record. A stage reads the
//! state first and records `already_done` when the state is the target. The
//! verb keeps its work under `<work root>/<commit>`: the checkout, the target
//! directories, the command logs and one run record per environment. Two
//! environments at one commit share the checkout, the builds and the pushed
//! images.
//!
//! Stages 1 to 5 are built (`wamn-m511.4`). The verb stops before the first
//! stage that is not built yet, and records nothing for it.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use tokio::io::AsyncBufReadExt as _;
use tokio::process::{Child, Command};

use crate::delivery::environment::EnvironmentFile;
use crate::delivery::run_record::{RunArguments, RunRecord, Stage, StepResult};

/// The images that the verb builds by source identity. The gates image is
/// pinned in the environment file and never built (`wamn-1s38`).
const BUILT_IMAGES: [&str; 3] = ["host", "identity", "ctl"];
/// The tag that `tools/journey-image-cache` relabels to, and its lease name.
const RELABEL_TAG: &str = "gcp";
const LEASE: &str = "gcp-wamn";
/// The workload guests of `docs/operations/gcp.md` §3.20: file, repository.
const WORKLOAD_GUESTS: [(&str, &str); 2] = [
    ("http_route.wasm", "flow-http"),
    ("materializer.wasm", "materializer"),
];
const BROKER_POD: &str = "evt-nats-0";
const TAP_JOB: &str = "evt-nats-tap-stream";
const TAP_CHECK_POD: &str = "evt-nats-check";
const TAP_PRESENT: &str = r#""config":{"name":"WAMN_TAP""#;
const COMMAND_TIMEOUT: Duration = Duration::from_hours(3);

#[derive(Clone, Debug)]
pub struct UpgradeEnvironmentRequest {
    /// The repository that holds `--commit`. The verb never builds in its
    /// working tree.
    pub repository: PathBuf,
    /// The parent of the private work directory of each commit.
    pub work_root: PathBuf,
    pub arguments: RunArguments,
}

/// Runs every built stage that the run record does not show as finished.
pub async fn upgrade_environment(request: &UpgradeEnvironmentRequest) -> anyhow::Result<RunRecord> {
    let work = request.work_root.join(&request.arguments.commit);
    private_directory(&work)?;
    private_directory(&work.join("runs"))?;
    private_directory(&work.join("logs"))?;
    let arguments = &request.arguments;
    let path = work.join("runs").join(format!(
        "{}--{}--{}.json",
        arguments.org, arguments.project, arguments.environment
    ));
    let mut record = RunRecord::open(&path, arguments.clone())?;
    let run = Run {
        repository: request.repository.clone(),
        checkout: work.join("checkout"),
        work,
        arguments: arguments.clone(),
    };
    while let Some(stage) = record.next_stage() {
        if !built(stage) {
            bail!(
                "the stage {stage:?} is not built yet; stages 1 to 5 finished, and the run record is {}",
                path.display()
            );
        }
        record.start(&path, stage, run.inputs(stage))?;
        let outcome = match stage {
            Stage::Source => run.source().await,
            Stage::Build => run.build().await,
            Stage::Images => run.images().await,
            Stage::Guests => run.guests(&record).await,
            Stage::Preflight => run.preflight().await,
            _ => unreachable!("only built stages start"),
        };
        match outcome {
            Ok((result, outputs)) => record.finish(&path, result, outputs)?,
            Err(error) => {
                let cause = format!("{error:#}");
                record.finish(&path, StepResult::Failed { cause }, BTreeMap::new())?;
                return Err(error.context(format!(
                    "the stage {stage:?} failed, and the run record is {}",
                    path.display()
                )));
            }
        }
    }
    Ok(record)
}

fn built(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::Source | Stage::Build | Stage::Images | Stage::Guests | Stage::Preflight
    )
}

type Outcome = anyhow::Result<(StepResult, BTreeMap<String, String>)>;

struct Run {
    repository: PathBuf,
    work: PathBuf,
    checkout: PathBuf,
    arguments: RunArguments,
}

impl Run {
    fn inputs(&self, stage: Stage) -> BTreeMap<String, String> {
        let mut inputs = BTreeMap::from([("commit".to_owned(), self.arguments.commit.clone())]);
        if stage == Stage::Source {
            inputs.insert(
                "repository".to_owned(),
                self.repository.display().to_string(),
            );
        } else {
            inputs.insert(
                "environment_file".to_owned(),
                EnvironmentFile::path(
                    &self.checkout,
                    &self.arguments.org,
                    &self.arguments.project,
                    &self.arguments.environment,
                )
                .display()
                .to_string(),
            );
        }
        inputs
    }

    fn environment(&self) -> anyhow::Result<EnvironmentFile> {
        EnvironmentFile::read(
            &self.checkout,
            &self.arguments.org,
            &self.arguments.project,
            &self.arguments.environment,
        )
    }

    fn programs(&self) -> PathBuf {
        self.work.join("programs")
    }

    fn delivery(&self) -> PathBuf {
        self.work.join("delivery")
    }

    fn guest_file(&self, name: &str) -> PathBuf {
        self.delivery().join("wasm32-wasip2/release").join(name)
    }

    /// Runs one command in the checkout. Its output goes to a log file of the
    /// work directory, and a failure names that file.
    async fn command(&self, label: &str, command: &mut Command) -> anyhow::Result<String> {
        let log = self.work.join("logs").join(format!("{label}.log"));
        let output = command
            .current_dir(&self.checkout)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(COMMAND_TIMEOUT, output)
            .await
            .with_context(|| format!("{label} ran longer than {COMMAND_TIMEOUT:?}"))?
            .with_context(|| format!("start {label}"))?;
        fs::write(
            &log,
            [output.stdout.as_slice(), output.stderr.as_slice()].concat(),
        )
        .with_context(|| format!("write {}", log.display()))?;
        ensure!(
            output.status.success(),
            "{label} exited {}; its output is in {}",
            output.status,
            log.display()
        );
        String::from_utf8(output.stdout).with_context(|| format!("{label} printed no UTF-8"))
    }

    /// Stage 1: a clean checkout of the commit in the work directory.
    async fn source(&self) -> Outcome {
        let existed = self.checkout.exists();
        if !existed {
            git(
                &self.repository,
                &[
                    "cat-file",
                    "-e",
                    &format!("{}^{{commit}}", self.arguments.commit),
                ],
            )
            .await
            .with_context(|| format!("the repository has no commit {}", self.arguments.commit))?;
            let checkout = self.checkout.display().to_string();
            git(
                &self.repository,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    &checkout,
                    &self.arguments.commit,
                ],
            )
            .await?;
        }
        let head = git(&self.checkout, &["rev-parse", "HEAD"]).await?;
        ensure!(
            head.trim() == self.arguments.commit,
            "the checkout {} is at {}, not at {}",
            self.checkout.display(),
            head.trim(),
            self.arguments.commit
        );
        let status = git(
            &self.checkout,
            &["status", "--porcelain", "--untracked-files=all"],
        )
        .await?;
        ensure!(
            status.is_empty(),
            "the checkout {} is not clean:\n{status}",
            self.checkout.display()
        );
        self.environment()?;
        let outputs =
            BTreeMap::from([("checkout".to_owned(), self.checkout.display().to_string())]);
        Ok((done(existed), outputs))
    }

    /// Stage 2: the programs, the native delivery binaries and the guests,
    /// before any candidate exists. Cargo decides what is fresh.
    async fn build(&self) -> Outcome {
        let programs = self.programs();
        let delivery = self.delivery();
        self.command(
            "build-programs",
            Command::new("cargo")
                .args([
                    "build",
                    "--locked",
                    "-p",
                    "wamn-ctl",
                    "--features",
                    "ops",
                    "--bins",
                ])
                .args(["-p", "wamn-scenario-worker"])
                .env("CARGO_TARGET_DIR", &programs),
        )
        .await?;
        self.command(
            "build-native",
            Command::new(self.checkout.join("tools/delivery-owned"))
                .arg("build-native")
                .arg(&delivery),
        )
        .await?;
        self.command(
            "build-components",
            Command::new(self.checkout.join("tools/build-components"))
                .arg("all")
                .env("CARGO_TARGET_DIR", &delivery),
        )
        .await?;
        let mut outputs = BTreeMap::new();
        let guests = delivery.join("wasm32-wasip2/release");
        let mut files: Vec<PathBuf> = fs::read_dir(&guests)
            .with_context(|| format!("list {}", guests.display()))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<_, _>>()?;
        files.sort();
        for file in files
            .iter()
            .filter(|file| file.extension().is_some_and(|ext| ext == "wasm"))
        {
            let name = file
                .file_name()
                .and_then(|name| name.to_str())
                .context("guest name")?;
            outputs.insert(format!("guest {name}"), sha256_file(file)?);
        }
        for (file, _) in WORKLOAD_GUESTS {
            ensure!(
                outputs.contains_key(&format!("guest {file}")),
                "the build made no workload guest {file}"
            );
        }
        Ok((StepResult::Done, outputs))
    }

    /// Stage 3: host, identity and ctl by source identity, pushed once under
    /// the `src-` tag. The gates image comes from the environment file.
    async fn images(&self) -> Outcome {
        let environment = self.environment()?;
        let registry = Registry::login(&environment.registry, &self.work).await?;
        let mut pushed = false;
        let mut outputs = BTreeMap::new();
        for name in BUILT_IMAGES {
            let built = self
                .command(
                    &format!("image-{name}"),
                    Command::new(self.checkout.join("tools/journey-image-cache"))
                        .arg("ensure")
                        .arg(&self.checkout)
                        .args([name, name, &self.arguments.commit, RELABEL_TAG, LEASE]),
                )
                .await?;
            let identity = built
                .trim()
                .strip_prefix(&format!("wamn-{name}:src-"))
                .with_context(|| format!("the image cache printed {built}"))?
                .to_owned();
            let repository = format!("wamn-{name}");
            let tag = format!("src-{identity}");
            let reference = format!("{}/{repository}:{tag}", environment.registry);
            let digest = if let Some(digest) = registry.digest(&repository, &tag).await? {
                digest
            } else {
                let relabeled = format!("wamn-{name}:{RELABEL_TAG}");
                self.command(
                    &format!("tag-{name}"),
                    Command::new("docker").args(["tag", &relabeled, &reference]),
                )
                .await?;
                self.command(
                    &format!("push-{name}"),
                    registry.docker().args(["push", &reference]),
                )
                .await?;
                pushed = true;
                registry
                    .digest(&repository, &tag)
                    .await?
                    .with_context(|| format!("the registry shows no {reference} after its push"))?
            };
            outputs.insert(name.to_owned(), format!("{reference}@{digest}"));
        }
        outputs.insert("gates".to_owned(), environment.gates_image);
        Ok((done(!pushed), outputs))
    }

    /// Stage 4: each workload guest once under the sha256 hex of its bytes.
    async fn guests(&self, record: &RunRecord) -> Outcome {
        let environment = self.environment()?;
        let registry = Registry::login(&environment.registry, &self.work).await?;
        let wash = self
            .command(
                "install-wash",
                &mut Command::new(self.checkout.join("tools/install-wash")),
            )
            .await?;
        let mut pushed = false;
        let mut outputs = BTreeMap::new();
        for (file, name) in WORKLOAD_GUESTS {
            let path = self.guest_file(file);
            let hex = sha256_file(&path)?;
            ensure!(
                hex == record.output(Stage::Build, &format!("guest {file}"))?,
                "the guest {} changed after the build stage",
                path.display()
            );
            let repository = format!("components/{name}");
            let reference = format!("{}/{repository}", environment.registry);
            let digest = if let Some(digest) = registry.digest(&repository, &hex).await? {
                digest
            } else {
                let mut push = registry.command(wash.trim());
                push.args(["-o", "json", "oci", "push", &format!("{reference}:{hex}")])
                    .arg(&path);
                self.command(&format!("push-{name}"), &mut push).await?;
                pushed = true;
                registry.digest(&repository, &hex).await?.with_context(|| {
                    format!("the registry shows no {reference}:{hex} after its push")
                })?
            };
            outputs.insert(name.to_owned(), format!("{reference}@{digest}"));
        }
        Ok((done(!pushed), outputs))
    }

    /// Stage 5: capacity, broker, `WAMN_TAP` and the next free release id,
    /// before any change to the environment.
    async fn preflight(&self) -> Outcome {
        let environment = self.environment()?;
        let kubectl = Kubectl(environment.context.clone());
        let mut outputs = BTreeMap::new();

        let values = host_values(&self.checkout)?;
        let groups = host_group_requests(&values)?;
        let nodes = kubectl.json(&["get", "nodes"]).await?;
        let pods = kubectl.json(&["get", "pods", "--all-namespaces"]).await?;
        let free = free_capacity(&nodes, &pods)?;
        place(&groups, free)?;
        outputs.insert(
            "host groups".to_owned(),
            groups
                .iter()
                .map(|(name, request)| format!("{name} {}m {}B", request.cpu, request.memory))
                .collect::<Vec<_>>()
                .join(", "),
        );

        let broker = kubectl
            .json(&["-n", "platform", "get", "pod", BROKER_POD])
            .await?;
        ensure!(ready(&broker), "the broker pod {BROKER_POD} is not Ready");
        let tap = if self.tap_exists(&kubectl).await? {
            "present"
        } else {
            self.run_tap_job(&kubectl).await?;
            ensure!(
                self.tap_exists(&kubectl).await?,
                "WAMN_TAP is still missing after the tap Job"
            );
            "made by the tap Job"
        };
        outputs.insert("WAMN_TAP".to_owned(), tap.to_owned());

        let release = next_release(&environment, &kubectl, &self.arguments).await?;
        outputs.insert("tenant".to_owned(), release.tenant);
        outputs.insert("database".to_owned(), release.database);
        outputs.insert(
            "attested releases".to_owned(),
            release
                .attested
                .iter()
                .map(|(id, commit)| format!("{id} {commit}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        outputs.insert("release id".to_owned(), release.next.to_string());
        Ok((StepResult::Done, outputs))
    }

    /// Runs the check pod of `deploy/gcp/evt-nats-check.yaml` and reads its log.
    async fn tap_exists(&self, kubectl: &Kubectl) -> anyhow::Result<bool> {
        let manifest = self.checkout.join("deploy/gcp/evt-nats-check.yaml");
        let manifest = manifest.display().to_string();
        kubectl
            .run(&[
                "-n",
                "platform",
                "delete",
                "pod",
                TAP_CHECK_POD,
                "--ignore-not-found",
            ])
            .await?;
        kubectl.run(&["apply", "-f", &manifest]).await?;
        let waited = kubectl
            .run(&[
                "-n",
                "platform",
                "wait",
                &format!("pod/{TAP_CHECK_POD}"),
                "--for=jsonpath={.status.phase}=Succeeded",
                "--timeout=120s",
            ])
            .await;
        let log = kubectl
            .run(&["-n", "platform", "logs", TAP_CHECK_POD])
            .await;
        let saved = self.work.join("logs").join("evt-nats-check.log");
        fs::write(&saved, log.as_deref().unwrap_or_default())?;
        kubectl
            .run(&[
                "-n",
                "platform",
                "delete",
                "pod",
                TAP_CHECK_POD,
                "--ignore-not-found",
            ])
            .await?;
        waited?;
        Ok(log?.contains(TAP_PRESENT))
    }

    /// Runs the tap-stream Job of `deploy/gcp/nats-jetstream.yaml` alone, so
    /// that no other document of that file changes.
    async fn run_tap_job(&self, kubectl: &Kubectl) -> anyhow::Result<()> {
        let file = self.checkout.join("deploy/gcp/nats-jetstream.yaml");
        let job = tap_job(
            &fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?,
        )?;
        let path = self.work.join("tap-job.yaml");
        fs::write(&path, job)?;
        let saved = self.work.join("logs").join("tap-job-previous.log");
        let previous = kubectl
            .run(&["-n", "platform", "logs", &format!("job/{TAP_JOB}")])
            .await;
        fs::write(&saved, previous.unwrap_or_default())?;
        kubectl
            .run(&[
                "-n",
                "platform",
                "delete",
                "job",
                TAP_JOB,
                "--ignore-not-found",
            ])
            .await?;
        kubectl
            .run(&["apply", "-f", &path.display().to_string()])
            .await?;
        let waited = kubectl
            .run(&[
                "-n",
                "platform",
                "wait",
                "--for=condition=complete",
                &format!("job/{TAP_JOB}"),
                "--timeout=180s",
            ])
            .await;
        let log = kubectl
            .run(&["-n", "platform", "logs", &format!("job/{TAP_JOB}")])
            .await;
        fs::write(
            self.work.join("logs").join("tap-job.log"),
            log.unwrap_or_default(),
        )?;
        waited.map(|_| ())
    }
}

fn done(already: bool) -> StepResult {
    if already {
        StepResult::AlreadyDone
    } else {
        StepResult::Done
    }
}

fn private_directory(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("set the mode of {}", path.display()))
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(hex::encode(ring::digest::digest(
        &ring::digest::SHA256,
        &bytes,
    )))
}

async fn git(directory: &Path, arguments: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .await
        .context("start git")?;
    ensure!(
        output.status.success(),
        "git {} exited {}: {}",
        arguments.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

/// kubectl with the explicit context of the environment file.
struct Kubectl(String);

impl Kubectl {
    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new("kubectl");
        command
            .args(["--context", &self.0])
            .args(arguments)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    async fn run(&self, arguments: &[&str]) -> anyhow::Result<String> {
        let output = self
            .command(arguments)
            .output()
            .await
            .context("start kubectl")?;
        ensure!(
            output.status.success(),
            "kubectl {} exited {}: {}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?)
    }

    async fn json(&self, arguments: &[&str]) -> anyhow::Result<Value> {
        let mut arguments = arguments.to_vec();
        arguments.extend(["-o", "json"]);
        Ok(serde_json::from_str(&self.run(&arguments).await?)?)
    }
}

/// A login to the registry of the environment file with a token of
/// `gcloud auth print-access-token`. The Docker configuration lives in a
/// private directory of the work directory and is removed with this value.
struct Registry {
    host: String,
    /// The `<project>/<repository>` path below the host.
    path: String,
    token: String,
    config: PathBuf,
    http: reqwest::Client,
}

impl Registry {
    async fn login(registry: &str, work: &Path) -> anyhow::Result<Self> {
        let (host, path) = registry
            .split_once('/')
            .with_context(|| format!("the registry {registry} has no path"))?;
        let project = path
            .split('/')
            .next()
            .with_context(|| format!("the registry {registry} names no project"))?;
        let output = Command::new("gcloud")
            .args(["auth", "print-access-token", "--project", project])
            .stdin(Stdio::null())
            .output()
            .await
            .context("start gcloud")?;
        ensure!(
            output.status.success(),
            "gcloud auth print-access-token exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let token = String::from_utf8(output.stdout)?.trim().to_owned();
        let config = work.join(format!("docker-{}", std::process::id()));
        private_directory(&config)?;
        let registry = Self {
            host: host.to_owned(),
            path: path.to_owned(),
            token,
            config,
            http: reqwest::Client::new(),
        };
        let file = registry.config.join("config.json");
        fs::write(&file, docker_config(&registry.host, &registry.token))?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        Ok(registry)
    }

    /// A command that reads the login from the private Docker configuration.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.env("DOCKER_CONFIG", &self.config);
        command
    }

    fn docker(&self) -> Command {
        self.command("docker")
    }

    /// The manifest digest of a tag, or none when the tag does not exist.
    async fn digest(&self, repository: &str, tag: &str) -> anyhow::Result<Option<String>> {
        let url = format!(
            "https://{}/v2/{}/{repository}/manifests/{tag}",
            self.host, self.path
        );
        let response = self
            .http
            .head(&url)
            .basic_auth("oauth2accesstoken", Some(&self.token))
            .header(
                reqwest::header::ACCEPT,
                "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, \
                 application/vnd.docker.distribution.manifest.list.v2+json, \
                 application/vnd.docker.distribution.manifest.v2+json",
            )
            .send()
            .await
            .with_context(|| format!("ask the registry for {url}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status().is_success(),
            "the registry answered {} for {url}",
            response.status()
        );
        let digest = response
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok())
            .with_context(|| format!("the registry named no digest for {url}"))?;
        ensure!(
            digest.starts_with("sha256:") && digest.len() == 71,
            "the registry named the digest {digest} for {url}"
        );
        Ok(Some(digest.to_owned()))
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.config);
    }
}

fn docker_config(host: &str, token: &str) -> String {
    use base64::Engine as _;
    let auth =
        base64::engine::general_purpose::STANDARD.encode(format!("oauth2accesstoken:{token}"));
    let mut auths = serde_json::Map::new();
    auths.insert(host.to_owned(), serde_json::json!({ "auth": auth }));
    serde_json::json!({ "auths": auths }).to_string()
}

fn host_values(checkout: &Path) -> anyhow::Result<serde_yaml::Value> {
    let path = checkout.join("deploy/gcp/values-host.yaml");
    serde_yaml::from_str(
        &fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

/// CPU in millicores and memory in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Resources {
    cpu: u64,
    memory: u64,
}

/// One request per replica of each host group of the host values. One Helm
/// upgrade changes every group, and each Deployment starts one new pod
/// before it stops an old one.
fn host_group_requests(values: &serde_yaml::Value) -> anyhow::Result<Vec<(String, Resources)>> {
    let groups = values["runtime"]["hostGroups"]
        .as_sequence()
        .context("the host values have no runtime.hostGroups")?;
    groups
        .iter()
        .map(|group| {
            let name = group["name"].as_str().context("a host group has no name")?;
            let requests = &group["resources"]["requests"];
            Ok((
                name.to_owned(),
                Resources {
                    cpu: cpu(requests["cpu"]
                        .as_str()
                        .with_context(|| format!("{name} requests no cpu"))?)?,
                    memory: memory(
                        requests["memory"]
                            .as_str()
                            .with_context(|| format!("{name} requests no memory"))?,
                    )?,
                },
            ))
        })
        .collect()
}

/// The allocatable capacity of each Ready node less the requests of the
/// pods that run on it.
fn free_capacity(nodes: &Value, pods: &Value) -> anyhow::Result<Vec<(String, Resources)>> {
    let mut free = Vec::new();
    for node in nodes["items"].as_array().context("no node list")? {
        if !ready(node) || node["spec"]["unschedulable"].as_bool() == Some(true) {
            continue;
        }
        let name = node["metadata"]["name"]
            .as_str()
            .context("a node has no name")?;
        let allocatable = &node["status"]["allocatable"];
        let mut left = Resources {
            cpu: cpu(allocatable["cpu"].as_str().context("no allocatable cpu")?)?,
            memory: memory(
                allocatable["memory"]
                    .as_str()
                    .context("no allocatable memory")?,
            )?,
        };
        for pod in pods["items"].as_array().context("no pod list")? {
            let phase = pod["status"]["phase"].as_str().unwrap_or_default();
            if pod["spec"]["nodeName"].as_str() != Some(name)
                || matches!(phase, "Succeeded" | "Failed")
            {
                continue;
            }
            for container in pod["spec"]["containers"].as_array().into_iter().flatten() {
                let requests = &container["resources"]["requests"];
                if let Some(value) = requests["cpu"].as_str() {
                    left.cpu = left.cpu.saturating_sub(cpu(value)?);
                }
                if let Some(value) = requests["memory"].as_str() {
                    left.memory = left.memory.saturating_sub(memory(value)?);
                }
            }
        }
        free.push((name.to_owned(), left));
    }
    ensure!(
        !free.is_empty(),
        "the cluster has no Ready schedulable node"
    );
    Ok(free)
}

/// Places one new pod of each host group, largest CPU first, on the node
/// with the most free CPU that holds it. A pod that fits no node refuses.
fn place(groups: &[(String, Resources)], mut free: Vec<(String, Resources)>) -> anyhow::Result<()> {
    let mut groups = groups.to_vec();
    groups.sort_by(|a, b| b.1.cpu.cmp(&a.1.cpu).then(b.1.memory.cmp(&a.1.memory)));
    for (group, request) in &groups {
        let summary = format!("{free:?}");
        let node = free
            .iter_mut()
            .filter(|(_, left)| left.cpu >= request.cpu && left.memory >= request.memory)
            .max_by_key(|(_, left)| left.cpu)
            .with_context(|| {
                format!(
                    "no node has room for a new pod of host group {group} ({}m CPU, {} bytes); free: {summary}",
                    request.cpu, request.memory
                )
            })?;
        node.1.cpu -= request.cpu;
        node.1.memory -= request.memory;
    }
    Ok(())
}

fn ready(object: &Value) -> bool {
    object["status"]["conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|condition| condition["type"] == "Ready" && condition["status"] == "True")
}

/// A Kubernetes CPU quantity in millicores.
fn cpu(quantity: &str) -> anyhow::Result<u64> {
    if let Some(milli) = quantity.strip_suffix('m') {
        return milli
            .parse()
            .with_context(|| format!("the CPU quantity {quantity}"));
    }
    let cores: f64 = quantity
        .parse()
        .with_context(|| format!("the CPU quantity {quantity}"))?;
    ensure!(
        (0.0..1e9).contains(&cores),
        "the CPU quantity {quantity} is out of range"
    );
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the range check above bounds the value"
    )]
    let millicores = (cores * 1000.0).round() as u64;
    Ok(millicores)
}

/// A Kubernetes memory quantity in bytes.
fn memory(quantity: &str) -> anyhow::Result<u64> {
    const UNITS: [(&str, u64); 8] = [
        ("Ki", 1 << 10),
        ("Mi", 1 << 20),
        ("Gi", 1 << 30),
        ("Ti", 1 << 40),
        ("k", 1_000),
        ("M", 1_000_000),
        ("G", 1_000_000_000),
        ("T", 1_000_000_000_000),
    ];
    let (number, unit) = UNITS
        .iter()
        .find_map(|(suffix, unit)| quantity.strip_suffix(suffix).map(|number| (number, *unit)))
        .unwrap_or((quantity, 1));
    let number: u64 = number
        .parse()
        .with_context(|| format!("the memory quantity {quantity}"))?;
    number
        .checked_mul(unit)
        .with_context(|| format!("the memory quantity {quantity} is too large"))
}

/// The Job document of the tap stream, alone.
fn tap_job(file: &str) -> anyhow::Result<String> {
    let mut found = None;
    for document in serde_yaml::Deserializer::from_str(file) {
        let value = <serde_yaml::Value as serde::Deserialize>::deserialize(document)?;
        if value["kind"] == "Job" && value["metadata"]["name"] == TAP_JOB {
            ensure!(found.is_none(), "the file has two Jobs {TAP_JOB}");
            found = Some(serde_yaml::to_string(&value)?);
        }
    }
    found.with_context(|| format!("the file has no Job {TAP_JOB}"))
}

struct NextRelease {
    tenant: String,
    database: String,
    /// Each attested release id with its source commit.
    attested: Vec<(i32, String)>,
    next: i32,
}

/// Reads the database of the environment from `registry.project_envs`, and
/// the attestations and releases of its tenant. The next free id is one above
/// every release of the tenant in the environment.
async fn next_release(
    environment: &EnvironmentFile,
    kubectl: &Kubectl,
    arguments: &RunArguments,
) -> anyhow::Result<NextRelease> {
    let system = &environment.system_database;
    let forward = PortForward::open(kubectl, &system.namespace, &system.cluster).await?;
    let password = kubectl
        .run(&[
            "-n",
            &system.namespace,
            "get",
            "secret",
            &format!("{}-superuser", system.cluster),
            "-o",
            "go-template={{.data.password | base64decode}}",
        ])
        .await?;
    let client = forward.connect(&system.database, &password).await?;
    let row = client
        .query_opt(
            "SELECT o.placement_type, o.pool_cluster, e.instance_suffix
               FROM registry.project_envs AS e JOIN registry.orgs AS o ON o.id = e.org
              WHERE e.org = $1 AND e.project = $2 AND e.env = $3",
            &[&arguments.org, &arguments.project, &arguments.environment],
        )
        .await?
        .with_context(|| {
            format!(
                "registry.project_envs has no environment {}/{}/{}",
                arguments.org, arguments.project, arguments.environment
            )
        })?;
    let placement: String = row.get(0);
    let pool: Option<String> = row.get(1);
    let suffix: String = row.get(2);
    ensure!(
        placement == "pooled" && pool.as_deref() == Some(system.cluster.as_str()),
        "the org {} is {placement} on {pool:?}; the verb reaches only the cluster {}",
        arguments.org,
        system.cluster
    );
    let database = wamn_control_provision::project_env_database_name(
        &arguments.org,
        &arguments.project,
        &arguments.environment,
        &suffix,
    );
    let client = forward.connect(&database, &password).await?;
    let tenant: String = client
        .query_one(
            "SELECT tenant_id FROM catalog.tenant_environments
              WHERE org = $1 AND project = $2 AND env = $3",
            &[&arguments.org, &arguments.project, &arguments.environment],
        )
        .await
        .with_context(|| format!("read the tenant of {database}"))?
        .get(0);
    let attested = client
        .query(
            "SELECT effective_release_id, coalesce(source_commit, '') FROM catalog.deployment_attestations
              WHERE tenant_id = $1 AND org_id = $2 AND project_id = $3 AND environment = $4
              ORDER BY effective_release_id",
            &[&tenant, &arguments.org, &arguments.project, &arguments.environment],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    let highest: i32 = client
        .query_one(
            "SELECT coalesce(max(effective_release_id), 0) FROM catalog.effective_releases
              WHERE tenant_id = $1",
            &[&tenant],
        )
        .await?
        .get(0);
    Ok(NextRelease {
        tenant,
        database,
        attested,
        next: highest + 1,
    })
}

/// A port-forward to the primary pod of a CloudNativePG cluster, on a free
/// local port. A pod forward, because a Service forward resets connections
/// (`docs/plan/kind-to-type.md` B0). The forward ends with this value.
struct PortForward {
    port: u16,
    _child: Child,
}

impl PortForward {
    async fn open(kubectl: &Kubectl, namespace: &str, cluster: &str) -> anyhow::Result<Self> {
        let primary = kubectl
            .run(&[
                "-n",
                namespace,
                "get",
                "pods",
                "-l",
                &format!("cnpg.io/cluster={cluster},cnpg.io/instanceRole=primary"),
                "-o",
                "jsonpath={.items[*].metadata.name}",
            ])
            .await?;
        let primary: Vec<&str> = primary.split_whitespace().collect();
        let [primary] = primary.as_slice() else {
            bail!("the cluster {cluster} has {} primary pods", primary.len());
        };
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let mut child = kubectl
            .command(&[
                "-n",
                namespace,
                "port-forward",
                &format!("pod/{primary}"),
                &format!("{port}:5432"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start kubectl port-forward")?;
        let stdout = child.stdout.take().context("port-forward output")?;
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let first = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .context("the port-forward did not start within 30 s")??;
        ensure!(
            first.is_some_and(|line| line.starts_with("Forwarding from")),
            "the port-forward to {primary} did not start"
        );
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Ok(Self {
            port,
            _child: child,
        })
    }

    /// A superuser connection without TLS: the forward is the authenticated
    /// kubectl stream.
    async fn connect(
        &self,
        database: &str,
        password: &str,
    ) -> anyhow::Result<tokio_postgres::Client> {
        let (client, connection) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(self.port)
            .user("postgres")
            .password(password)
            .dbname(database)
            .ssl_mode(tokio_postgres::config::SslMode::Disable)
            .connect(tokio_postgres::NoTls)
            .await
            .with_context(|| format!("connect to {database}"))?;
        tokio::spawn(connection);
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quantities_read_in_millicores_and_bytes() {
        assert_eq!(cpu("500m").unwrap(), 500);
        assert_eq!(cpu("2").unwrap(), 2000);
        assert_eq!(cpu("1.5").unwrap(), 1500);
        assert_eq!(memory("256Mi").unwrap(), 256 << 20);
        assert_eq!(memory("5933980Ki").unwrap(), 5_933_980 << 10);
        assert_eq!(memory("1G").unwrap(), 1_000_000_000);
        assert_eq!(memory("1024").unwrap(), 1024);
        assert!(cpu("x").is_err() && memory("1Xi").is_err());
    }

    fn node(name: &str, cpu: &str, memory: &str, ready: &str) -> Value {
        json!({
            "metadata": {"name": name},
            "status": {
                "allocatable": {"cpu": cpu, "memory": memory},
                "conditions": [{"type": "Ready", "status": ready}],
            },
        })
    }

    fn pod(node: &str, cpu: &str, memory: &str, phase: &str) -> Value {
        json!({
            "spec": {"nodeName": node, "containers": [{"resources": {"requests": {"cpu": cpu, "memory": memory}}}]},
            "status": {"phase": phase},
        })
    }

    #[test]
    fn free_capacity_counts_running_pods_of_ready_nodes() {
        let nodes = json!({"items": [
            node("a", "1930m", "4Gi", "True"),
            node("b", "1930m", "4Gi", "False"),
        ]});
        let pods = json!({"items": [
            pod("a", "500m", "1Gi", "Running"),
            pod("a", "900m", "1Gi", "Succeeded"),
            pod("b", "100m", "1Gi", "Running"),
        ]});
        assert_eq!(
            free_capacity(&nodes, &pods).unwrap(),
            [(
                "a".to_owned(),
                Resources {
                    cpu: 1430,
                    memory: 3 << 30
                }
            )]
        );
    }

    #[test]
    fn every_host_group_needs_room_for_one_new_pod() {
        let group = |name: &str| {
            (
                name.to_owned(),
                Resources {
                    cpu: 500,
                    memory: 256 << 20,
                },
            )
        };
        let groups = [group("default"), group("wms"), group("control")];
        let node = |name: &str, cpu| {
            (
                name.to_owned(),
                Resources {
                    cpu,
                    memory: 1 << 30,
                },
            )
        };
        assert!(place(&groups, vec![node("a", 1000), node("b", 500)]).is_ok());
        let error = place(&groups, vec![node("a", 900), node("b", 600)]).unwrap_err();
        assert!(
            format!("{error:#}").contains("no node has room"),
            "{error:#}"
        );
    }

    #[test]
    fn the_host_values_of_wamn_dev_name_three_groups() {
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let groups = host_group_requests(&host_values(checkout).unwrap()).unwrap();
        let names: Vec<&str> = groups.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["default", "wms", "control"]);
    }

    #[test]
    fn the_tap_job_is_taken_alone_from_its_file() {
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let file = fs::read_to_string(checkout.join("deploy/gcp/nats-jetstream.yaml")).unwrap();
        let job: serde_yaml::Value = serde_yaml::from_str(&tap_job(&file).unwrap()).unwrap();
        assert_eq!(job["kind"], "Job");
        assert_eq!(job["metadata"]["name"], TAP_JOB);
        assert!(tap_job("kind: ConfigMap\n").is_err());
    }

    #[test]
    fn the_docker_configuration_holds_only_the_registry_login() {
        let config: Value =
            serde_json::from_str(&docker_config("us-central1-docker.pkg.dev", "token")).unwrap();
        assert_eq!(
            config,
            json!({"auths": {"us-central1-docker.pkg.dev": {"auth": "b2F1dGgyYWNjZXNzdG9rZW46dG9rZW4="}}})
        );
    }

    fn git_sync(directory: &Path, arguments: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git output")
    }

    #[tokio::test]
    async fn the_source_stage_checks_out_the_commit_once_and_refuses_a_dirty_checkout() {
        let root = std::env::temp_dir().join(format!(
            "wamn-upgrade-source-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().expect("time")
        ));
        let repository = root.join("repository");
        let real = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let file = "deploy/gcp/environments/dkk--receiving--dev.json";
        let environment: Value =
            serde_json::from_str(&fs::read_to_string(real.join(file)).unwrap()).unwrap();
        let mut named = vec![
            file.to_owned(),
            environment["edge"]["values"].as_str().unwrap().to_owned(),
            environment["url_map"]["file"].as_str().unwrap().to_owned(),
        ];
        for workload in environment["workloads"].as_array().unwrap() {
            named.push(workload.as_str().unwrap().to_owned());
        }
        for path in &named {
            let target = repository.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(real.join(path), target).unwrap();
        }
        git_sync(&repository, &["init", "--quiet"]);
        git_sync(
            &repository,
            &["config", "user.email", "test@example.invalid"],
        );
        git_sync(&repository, &["config", "user.name", "test"]);
        git_sync(&repository, &["add", "."]);
        git_sync(&repository, &["commit", "--quiet", "-m", "one"]);
        let commit = git_sync(&repository, &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        let work = root.join("work");
        let run = Run {
            repository: repository.clone(),
            checkout: work.join("checkout"),
            work,
            arguments: RunArguments::new("dkk", "receiving", "dev", &commit).unwrap(),
        };

        let (first, outputs) = run.source().await.expect("first source");
        assert_eq!(first, StepResult::Done);
        assert_eq!(outputs["checkout"], run.checkout.display().to_string());
        let (second, _) = run.source().await.expect("second source");
        assert_eq!(second, StepResult::AlreadyDone);
        fs::write(run.checkout.join("stray"), "x").unwrap();
        let error = run.source().await.expect_err("dirty checkout");
        assert!(format!("{error:#}").contains("is not clean"), "{error:#}");

        let absent = Run {
            arguments: RunArguments::new("dkk", "receiving", "dev", &"0".repeat(40)).unwrap(),
            checkout: root.join("absent"),
            ..run
        };
        let error = absent.source().await.expect_err("absent commit");
        assert!(format!("{error:#}").contains("has no commit"), "{error:#}");
        fs::remove_dir_all(&root).unwrap();
    }
}
