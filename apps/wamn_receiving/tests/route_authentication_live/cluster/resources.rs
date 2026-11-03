//! Ownership of one Receiving cluster and its private working files.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};

use wamn_gate_harness::case_release::NAMESPACE;
use wamn_test_infrastructure::cluster::{self, Cluster};
pub(super) use wamn_test_infrastructure::workload::{kind_address, postgres_host_port};

const REGISTRY_USERNAME: &str = "wamn-receiving-journey";

pub(super) struct Resources {
    pub repository: PathBuf,
    pub work: PathBuf,
    pub evidence: PathBuf,
    pub teardown_leftovers: Vec<String>,
    pub name: String,
    pub lifecycle: Cluster,
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
    /// A kept cluster outlives this process: the delivery case's.
    kept: bool,
}

/// Reserve a new name before any lifecycle action can create a resource.
/// A Receiving cluster case runs the candidate that `qualify-release` names.
pub(super) fn prepare(
    repository: &Path,
    evidence: &Path,
    gates_image: bool,
) -> anyhow::Result<Resources> {
    let (candidate, manifest) = super::super::delivery::candidate()?;
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "the amd64 Receiving images require an x86_64 machine"
    );
    let name = format!("wamn-receiving-{}", uuid::Uuid::new_v4().simple());
    ensure!(
        evidence.is_absolute() && evidence.is_dir(),
        "the evidence directory must be an existing absolute path"
    );
    let work = std::env::temp_dir().join(&name);
    DirBuilder::new().mode(0o700).create(&work)?;
    let cluster = Resources {
        repository: repository.to_owned(),
        lifecycle: Cluster::new(&name, &work)?,
        work,
        evidence: evidence.to_owned(),
        teardown_leftovers: Vec::new(),
        host_image: super::super::delivery::image_reference(&candidate.images.host)?,
        gates_image: gates_image
            .then(|| super::super::delivery::image_reference(&candidate.qualification.gates))
            .transpose()?,
        identity_image: Some(super::super::delivery::image_reference(
            &candidate.qualification.identity,
        )?),
        candidate: Some((candidate, manifest)),
        name,
        reader: None,
        kept: false,
    };
    write_private(
        &cluster.evidence.join("source.json"),
        &serde_json::to_vec_pretty(&json!({
            "cluster":cluster.name,
            "host_image":cluster.host_image,"gates_image":cluster.gates_image,
            "identity_image":cluster.identity_image,
        }))?,
    )?;
    Ok(cluster)
}

/// The resources of the Receiving delivery case on the kept delivery cluster
/// (`delivery_case`): `name` is the application's owned name there. The case
/// creates and removes nothing through them.
pub(super) fn delivery(repository: &Path, work: &Path, evidence: &Path, name: &str) -> Resources {
    Resources {
        repository: repository.to_owned(),
        work: work.to_owned(),
        evidence: evidence.to_owned(),
        teardown_leftovers: Vec::new(),
        name: name.to_owned(),
        lifecycle: Cluster::new(name, work).expect("owned application cluster name"),
        host_image: format!("wamn-host:{name}"),
        gates_image: Some(format!("wamn-gates:{name}")),
        identity_image: Some(format!("wamn-identity:{name}")),
        candidate: None,
        reader: None,
        kept: true,
    }
}

impl Resources {
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
    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow::anyhow!("generate the private registry password"))?;
    let password = hex::encode(random);
    let hash = cluster::registry_password(REGISTRY_USERNAME, &password).await?;
    write_private(&cluster.work.join("registry/htpasswd"), &hash)?;
    Ok(password)
}

pub(super) async fn create(cluster: &mut Resources) -> anyhow::Result<()> {
    if let Some((candidate, _)) = &cluster.candidate {
        let images = std::iter::once(&cluster.host_image)
            .chain(&cluster.gates_image)
            .chain(&cluster.identity_image)
            .map(String::as_str)
            .collect::<Vec<_>>();
        super::super::delivery::registry_files(candidate, &cluster.work, &images).await?;
    }
    cluster.lifecycle.create(&cluster.repository).await?;
    let images = std::iter::once(&cluster.host_image)
        .chain(&cluster.gates_image)
        .chain(&cluster.identity_image)
        .map(String::as_str)
        .collect::<Vec<_>>();
    cluster.lifecycle.import_images(&images).await?;
    cluster
        .lifecycle
        .start_postgres(&cluster.repository)
        .await?;
    cluster.lifecycle.start_registry().await?;
    Ok(())
}

pub(super) async fn inspect(cluster: &Resources, suffix: &str) -> anyhow::Result<Value> {
    let bytes = checked(&mut cluster::inspect(&format!("{}-{suffix}", cluster.name))).await?;
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

/// Keep the environment release's objects in the operator namespace.
pub(super) async fn capture_failure(cluster: &Resources) {
    cluster
        .lifecycle
        .inspect(NAMESPACE, &cluster.evidence)
        .await;
    let host_deployment = super::release_name()
        .map(|name| format!("hostgroup-{name}"))
        .unwrap_or_default();
    for (name, args) in [
        (
            "failure-workloads.json",
            vec!["get", "workloads", "-o", "json"],
        ),
        (
            "failure-host-deployment.json",
            vec!["get", "deployment", &host_deployment, "-o", "json"],
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
            .args(["-n", NAMESPACE])
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
                    NAMESPACE,
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
    if cluster.lifecycle.owned() {
        let teardown = cluster.lifecycle.cleanup().await;
        cluster.teardown_leftovers.clone_from(&teardown.leftovers);
        teardown.result()?;
        if !cluster.teardown_leftovers.is_empty() {
            eprintln!(
                "Receiving leftover kind resources: {:?}",
                cluster.teardown_leftovers
            );
        }
    }
    cluster
        .lifecycle
        .absent(&cluster.teardown_leftovers)
        .await?;
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
        if self.lifecycle.owned() {
            let teardown = self.lifecycle.cleanup_blocking();
            let _ = fs::copy(
                self.work.join("kind-teardown-leftovers"),
                self.evidence.join("kind-teardown-leftovers"),
            );
            if teardown.result().is_err() {
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

pub(super) use super::super::case_execution::checked;
