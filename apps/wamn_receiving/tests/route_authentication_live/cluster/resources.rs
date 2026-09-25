//! Ownership of one Receiving cluster and its private working files.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context as _, ensure};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use wamn_test_infrastructure::rendering::render_kind_cluster;
pub(super) use wamn_test_infrastructure::workload::{
    kind_address, postgres_host_port, record_build,
};

const REGISTRY_USERNAME: &str = "wamn-receiving-journey";

pub(super) struct Resources {
    pub repository: PathBuf,
    pub work: PathBuf,
    pub evidence: PathBuf,
    pub name: String,
    pub source: String,
    pub lifecycle: PathBuf,
    pub host_image: String,
    pub gates_image: Option<String>,
    pub identity_image: Option<String>,
    pub candidate: Option<(
        wamn_control::delivery::Candidate,
        wamn_catalog::ServingManifest,
    )>,
    pub reader: Option<(
        wamn_cdc_reader::ReaderShutdown,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    )>,
    owned: bool,
    /// A kept cluster outlives this process: a later stage attaches to it,
    /// and only the teardown stage removes it.
    kept: bool,
}

/// Reserve a new name before any lifecycle action can create a resource.
pub(super) async fn prepare(
    repository: &Path,
    evidence: &Path,
    gates_image: bool,
) -> anyhow::Result<Resources> {
    let candidate = super::super::delivery::candidate()?;
    if let Some((candidate, _)) = &candidate {
        ensure!(
            !gates_image || candidate.gates_image.is_some(),
            "this Receiving case requires a supplied gates image"
        );
        ensure!(
            candidate.identity_image.is_some(),
            "this Receiving case requires a supplied identity image"
        );
    }
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "the Receiving images require an x86_64 build host"
    );
    let source = committed_head(repository).await?;
    let name = format!("wamn-receiving-{}", uuid::Uuid::new_v4().simple());
    let lifecycle = repository.join("tools/receiving-cluster-journey-run");
    for action in ["docker-version", "clusters", "containers", "images"] {
        let output = checked(Command::new(&lifecycle).args([action, &name])).await?;
        if action != "docker-version" {
            ensure!(
                !String::from_utf8_lossy(&output).contains(&name),
                "the Receiving resource name already exists"
            );
        }
    }
    ensure!(
        evidence.is_absolute() && !evidence.exists(),
        "the evidence directory must be a new absolute path"
    );
    DirBuilder::new().mode(0o700).create(evidence)?;
    let work = std::env::temp_dir().join(&name);
    DirBuilder::new().mode(0o700).create(&work)?;
    let cluster = Resources {
        repository: repository.to_owned(),
        work,
        evidence: evidence.to_owned(),
        host_image: candidate
            .as_ref()
            .map(|(candidate, _)| super::super::delivery::image_reference(&candidate.host_image))
            .transpose()?
            .unwrap_or_else(|| format!("wamn-host:{name}")),
        gates_image: if let Some((candidate, _)) = &candidate {
            candidate
                .gates_image
                .as_deref()
                .map(super::super::delivery::image_reference)
                .transpose()?
        } else {
            gates_image.then(|| format!("wamn-gates:{name}"))
        },
        identity_image: if let Some((candidate, _)) = &candidate {
            candidate
                .identity_image
                .as_deref()
                .map(super::super::delivery::image_reference)
                .transpose()?
        } else {
            Some(format!("wamn-identity:{name}"))
        },
        candidate,
        name,
        source,
        lifecycle,
        reader: None,
        owned: false,
        kept: false,
    };
    write_private(
        &cluster.evidence.join("source.json"),
        &serde_json::to_vec_pretty(&json!({
            "source_commit":cluster.source,"cluster":cluster.name,
            "host_image":cluster.host_image,"gates_image":cluster.gates_image,
            "identity_image":cluster.identity_image,
        }))?,
    )?;
    Ok(cluster)
}

/// The HEAD commit of a clean tree. A cluster test runs committed source only.
pub(super) async fn committed_head(repository: &Path) -> anyhow::Result<String> {
    let status = checked(Command::new("git").current_dir(repository).args([
        "status",
        "--porcelain",
        "--untracked-files=normal",
        "--",
        ".",
        ":(exclude).beads/issues.jsonl",
        ":(exclude).beads/interactions.jsonl",
    ]))
    .await?;
    ensure!(
        status.is_empty(),
        "commit the final source before running the cluster test"
    );
    Ok(String::from_utf8(
        checked(Command::new("git").current_dir(repository).args([
            "rev-parse",
            "--verify",
            "HEAD",
        ]))
        .await?,
    )?
    .trim()
    .to_owned())
}

/// The host image identity of HEAD: the source the host and identity images
/// are built from, without the test-only files (tools/journey-image-cache).
pub(super) async fn host_identity(repository: &Path) -> anyhow::Result<String> {
    let identity = checked(
        Command::new(repository.join("tools/journey-image-cache"))
            .arg("identity")
            .arg(repository)
            .arg("host"),
    )
    .await?;
    Ok(String::from_utf8(identity)?.trim().to_owned())
}

/// Attach to a cluster that a setup stage kept. `test_source` names the test
/// code that runs against it.
pub(super) async fn attach(
    repository: &Path,
    evidence: &Path,
    kept: &super::stages::Kept,
    test_source: &str,
) -> anyhow::Result<Resources> {
    let lifecycle = repository.join("tools/receiving-cluster-journey-run");
    let clusters = checked(Command::new(&lifecycle).args(["clusters", &kept.cluster])).await?;
    ensure!(
        String::from_utf8_lossy(&clusters)
            .lines()
            .any(|line| line == kept.cluster),
        "the kept Receiving cluster {} does not exist",
        kept.cluster
    );
    ensure!(
        evidence.is_absolute() && !evidence.exists(),
        "the evidence directory must be a new absolute path"
    );
    DirBuilder::new().mode(0o700).create(evidence)?;
    let cluster = Resources {
        repository: repository.to_owned(),
        work: std::env::temp_dir().join(&kept.cluster),
        evidence: evidence.to_owned(),
        name: kept.cluster.clone(),
        source: kept.source.clone(),
        lifecycle,
        host_image: kept.host_image.clone(),
        gates_image: None,
        identity_image: kept.identity_image.clone(),
        candidate: None,
        reader: None,
        owned: true,
        kept: true,
    };
    write_private(
        &cluster.evidence.join("source.json"),
        &serde_json::to_vec_pretty(&json!({
            "source_commit":cluster.source,"test_commit":test_source,"cluster":cluster.name,
            "host_image":cluster.host_image,"identity_image":cluster.identity_image,
        }))?,
    )?;
    Ok(cluster)
}

impl Resources {
    /// Leave the cluster in place when this process ends.
    pub(super) fn keep(&mut self) {
        self.kept = true;
    }

    /// Remove a kept cluster at the end of this process.
    pub(super) fn release(&mut self) {
        self.kept = false;
    }

    pub(super) fn kept(&self) -> bool {
        self.kept
    }
}

pub(super) async fn prepare_files(cluster: &Resources) -> anyhow::Result<String> {
    for directory in ["registry", "docker", "host-secrets", "wasmtime-cache"] {
        DirBuilder::new()
            .mode(0o700)
            .create(cluster.work.join(directory))?;
    }
    let kind = render_kind_cluster(&fs::read_to_string(
        cluster.repository.join("deploy/infra/kind-config.yaml"),
    )?)?;
    write_private(&cluster.work.join("kind.yaml"), kind.as_bytes())?;

    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow::anyhow!("generate the private registry password"))?;
    let password = hex::encode(random);
    let mut child = Command::new(&cluster.lifecycle)
        .args(["prepare-registry", &cluster.name, REGISTRY_USERNAME])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start the registry password helper")?;
    let mut input = child
        .stdin
        .take()
        .context("open the registry password input")?;
    let written = input.write_all(format!("{password}\n").as_bytes()).await;
    drop(input);
    let output = child.wait_with_output().await?;
    written.context("write the registry password input")?;
    ensure!(
        output.status.success(),
        "the registry password helper failed with {}",
        output.status
    );
    write_private(&cluster.work.join("registry/htpasswd"), &output.stdout)?;
    Ok(password)
}

pub(super) async fn build_images(cluster: &mut Resources) -> anyhow::Result<()> {
    if cluster.candidate.is_some() {
        return Ok(());
    }
    cluster.owned = true;
    // Every case builds the host image from the repository Dockerfile, and
    // only a case that uses the gates image builds it (wamn-szr0).
    record_build(
        &cluster.evidence,
        "build-images",
        Command::new(&cluster.lifecycle)
            .current_dir(&cluster.repository)
            .arg("build-images")
            .arg(&cluster.name)
            .arg(&cluster.work)
            .arg(&cluster.repository)
            .arg(&cluster.source)
            .arg(&cluster.name)
            .arg("host")
            .args(cluster.gates_image.is_some().then_some("gates")),
    )
    .await?;
    if cluster.identity_image.is_some() {
        record_build(
            &cluster.evidence,
            "build-identity",
            Command::new(&cluster.lifecycle)
                .current_dir(&cluster.repository)
                .arg("build-identity")
                .arg(&cluster.name)
                .arg(&cluster.repository)
                .arg(&cluster.source)
                .arg(&cluster.name),
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn create(cluster: &mut Resources) -> anyhow::Result<()> {
    if let Some((candidate, _)) = &cluster.candidate {
        super::super::delivery::registry_files(candidate, &cluster.work)?;
    }
    cluster.owned = true;
    checked(
        Command::new(&cluster.lifecycle)
            .arg("create")
            .arg(&cluster.name)
            .arg(&cluster.work)
            .arg(&cluster.repository)
            .arg(&cluster.host_image)
            .args(cluster.gates_image.iter())
            .args(cluster.identity_image.iter()),
    )
    .await?;
    Ok(())
}

pub(super) async fn inspect(cluster: &Resources, suffix: &str) -> anyhow::Result<Value> {
    let bytes = checked(
        Command::new(&cluster.lifecycle)
            .arg("inspect")
            .arg(format!("{}-{suffix}", cluster.name)),
    )
    .await?;
    let document =
        serde_json::from_slice(&bytes).context("decode the owned container's network settings")?;
    fs::write(
        cluster.evidence.join(format!("{suffix}-network.json")),
        bytes,
    )?;
    Ok(document)
}

pub(super) async fn registry_auth(
    cluster: &Resources,
    authority: &str,
    password: &str,
) -> anyhow::Result<PathBuf> {
    let path = cluster.work.join("docker/config.json");
    write_private(
        &path,
        &serde_json::to_vec(&json!({"auths":{authority:{
            "username":REGISTRY_USERNAME,"password":password,
        }}}))?,
    )?;
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let url = format!("http://{authority}/v2/");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Ok(response) = http
            .get(&url)
            .basic_auth(REGISTRY_USERNAME, Some(password))
            .send()
            .await
            && response.status() == reqwest::StatusCode::OK
        {
            break;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "the owned registry did not become ready"
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    ensure!(
        http.get(&url).send().await?.status() == reqwest::StatusCode::UNAUTHORIZED,
        "the private registry must refuse anonymous access"
    );
    ensure!(
        http.get(&url)
            .basic_auth(REGISTRY_USERNAME, Some(password))
            .send()
            .await?
            .status()
            == reqwest::StatusCode::OK,
        "the private registry must accept its declared credential"
    );
    Ok(path)
}

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
        .with_context(|| format!("write private file {}", path.display()))
}

pub(super) async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .context("run the Receiving test command")?;
    ensure!(
        output.status.success(),
        "Receiving test command failed with {}",
        output.status
    );
    Ok(output.stdout)
}

pub(super) async fn capture_failure(cluster: &Resources) {
    for (name, args) in [
        (
            "failure-workloads.json",
            vec!["get", "workloads", "-o", "json"],
        ),
        (
            "failure-host-deployment.json",
            vec!["get", "deployment", "hostgroup-default", "-o", "json"],
        ),
        ("failure-pods.json", vec!["get", "pods", "-o", "json"]),
        ("failure-jobs.json", vec!["get", "jobs", "-o", "json"]),
        (
            "failure-events.json",
            vec!["get", "events.events.k8s.io", "-o", "json"],
        ),
    ] {
        let output = super::kubectl(cluster)
            .arg("--request-timeout=10s")
            .args(["-n", &cluster.name])
            .args(args)
            .kill_on_drop(true)
            .output()
            .await;
        if let Ok(output) = output
            && output.status.success()
        {
            let _ = fs::write(cluster.evidence.join(name), output.stdout);
        }
    }
    let pods = fs::read(cluster.evidence.join("failure-pods.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    if let Some(pods) = pods.as_ref().and_then(|pods| pods["items"].as_array()) {
        for pod in pods {
            let Some(name) = pod["metadata"]["name"].as_str() else {
                continue;
            };
            let output = super::kubectl(cluster)
                .arg("--request-timeout=10s")
                .args([
                    "-n",
                    &cluster.name,
                    "logs",
                    name,
                    "--all-containers",
                    "--tail=-1",
                ])
                .kill_on_drop(true)
                .output()
                .await;
            if let Ok(output) = output {
                let _ = fs::write(
                    cluster.evidence.join(format!("failure-pod-{name}.log")),
                    output.stdout,
                );
                let _ = fs::write(
                    cluster.evidence.join(format!("failure-pod-{name}.stderr")),
                    output.stderr,
                );
                let _ = fs::write(
                    cluster.evidence.join(format!("failure-pod-{name}-result.json")),
                    json!({"pod":name,"exit_code":output.status.code(),"passed":output.status.success()}).to_string(),
                );
            }
        }
    }
}

/// Stop the in-process CDC reader. A kept cluster keeps its replication slot,
/// and the next stage that needs the reader starts it again.
pub(super) async fn stop_reader(cluster: &mut Resources) -> anyhow::Result<()> {
    if let Some((cancellation, mut task)) = cluster.reader.take() {
        cancellation.shutdown();
        if let Ok(result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), &mut task).await
        {
            result
                .context("join the production CDC reader")
                .and_then(|result| result)
        } else {
            task.abort();
            let _ = task.await;
            Err(anyhow::anyhow!(
                "the production CDC reader did not stop within 10 seconds"
            ))
        }
    } else {
        Ok(())
    }
}

pub(super) async fn remove(cluster: &mut Resources) -> anyhow::Result<()> {
    let reader_result = stop_reader(cluster).await;
    if cluster.owned {
        checked(
            Command::new(&cluster.lifecycle)
                .arg("remove")
                .arg(&cluster.name)
                .arg(&cluster.work)
                .args(cluster.candidate.is_none().then_some(&cluster.host_image))
                .args(
                    cluster
                        .gates_image
                        .iter()
                        .filter(|_| cluster.candidate.is_none()),
                )
                .args(
                    cluster
                        .identity_image
                        .iter()
                        .filter(|_| cluster.candidate.is_none()),
                ),
        )
        .await?;
        cluster.owned = false;
    }
    for action in ["docker-version", "clusters", "containers", "images"] {
        let output =
            checked(Command::new(&cluster.lifecycle).args([action, &cluster.name])).await?;
        if action != "docker-version" {
            ensure!(
                !String::from_utf8_lossy(&output).contains(&cluster.name),
                "an owned Receiving resource remains after cleanup"
            );
        }
    }
    fs::remove_dir_all(&cluster.work).context("remove the private Receiving files")?;
    reader_result
}

impl Drop for Resources {
    fn drop(&mut self) {
        if let Some((cancellation, task)) = &self.reader {
            cancellation.shutdown();
            task.abort();
        }
        if self.kept {
            return;
        }
        if self.owned {
            let status = std::process::Command::new(&self.lifecycle)
                .arg("remove")
                .arg(&self.name)
                .arg(&self.work)
                .args(self.candidate.is_none().then_some(&self.host_image))
                .args(self.gates_image.iter().filter(|_| self.candidate.is_none()))
                .args(
                    self.identity_image
                        .iter()
                        .filter(|_| self.candidate.is_none()),
                )
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if !status.is_ok_and(|status| status.success()) {
                eprintln!(
                    "Receiving cleanup failed for {}; private files remain at {}",
                    self.name,
                    self.work.display()
                );
                return;
            }
        }
        let _ = fs::remove_dir_all(&self.work);
    }
}
