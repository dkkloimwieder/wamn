//! Ownership of one Receiving cluster and its private working files.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::net::Ipv4Addr;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context as _, ensure};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use wamn_test_infrastructure::rendering::render_kind_cluster;

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
    pub reader: Option<(
        pg_walstream::CancellationToken,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    )>,
    owned: bool,
}

/// Reserve a new name before any lifecycle action can create a resource.
pub(super) async fn prepare(
    repository: &Path,
    evidence: &Path,
    standard_images: bool,
    session_host: bool,
) -> anyhow::Result<Resources> {
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "the Receiving images require an x86_64 build host"
    );
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
    let source = String::from_utf8(
        checked(Command::new("git").current_dir(repository).args([
            "rev-parse",
            "--verify",
            "HEAD",
        ]))
        .await?,
    )?
    .trim()
    .to_owned();
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
        host_image: format!("wamn-host:{name}"),
        gates_image: standard_images.then(|| format!("wamn-gates:{name}")),
        identity_image: session_host.then(|| format!("wamn-identity:{name}")),
        name,
        source,
        lifecycle,
        reader: None,
        owned: false,
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

pub(super) async fn build_images(
    cluster: &mut Resources,
    standard_images: bool,
) -> anyhow::Result<()> {
    cluster.owned = true;
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
            .arg(if standard_images { "standard" } else { "local" }),
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

pub(super) fn kind_address(settings: &Value) -> anyhow::Result<Ipv4Addr> {
    let address: Ipv4Addr = settings["Networks"]["kind"]["IPAddress"]
        .as_str()
        .context("the owned container has a kind-network address")?
        .parse()
        .context("the owned container has an IPv4 kind-network address")?;
    ensure!(
        !address.is_unspecified(),
        "the kind-network address is empty"
    );
    Ok(address)
}

pub(super) fn postgres_host_port(settings: &Value) -> anyhow::Result<u16> {
    let ports = settings["Ports"]["5432/tcp"]
        .as_array()
        .context("PostgreSQL publishes port 5432")?;
    ensure!(
        ports.len() == 1 && ports[0]["HostIp"] == "127.0.0.1",
        "PostgreSQL must publish one loopback port"
    );
    let port = ports[0]["HostPort"]
        .as_str()
        .context("the PostgreSQL host port is present")?
        .parse::<u16>()
        .context("the PostgreSQL host port is valid")?;
    ensure!(port != 0, "the PostgreSQL host port must be allocated");
    Ok(port)
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

// Image-build arguments contain source paths and image names, never credentials.
async fn record_build(evidence: &Path, name: &str, command: &mut Command) -> anyhow::Result<()> {
    let standard = command.as_std();
    let argv = std::iter::once(standard.get_program())
        .chain(standard.get_args())
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let cwd = standard
        .get_current_dir()
        .map(Path::to_path_buf)
        .map_or_else(std::env::current_dir, Ok)?;
    let stdout_path = evidence.join(format!("{name}.stdout.log"));
    let stderr_path = evidence.join(format!("{name}.stderr.log"));
    write_private(
        &evidence.join(format!("{name}-command.json")),
        &serde_json::to_vec_pretty(&json!({
            "argv": argv, "cwd": cwd,
            "stdout": stdout_path, "stderr": stderr_path,
        }))?,
    )?;
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stdout_path)?;
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stderr_path)?;
    let started = Instant::now();
    let status = command
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true)
        .status()
        .await;
    let result = match &status {
        Ok(status) => json!({
            "exit_code": status.code(), "signal": status.signal(),
            "status": status.to_string(), "passed": status.success(),
            "elapsed_seconds": started.elapsed().as_secs_f64(),
        }),
        Err(error) => json!({
            "exit_code": null, "signal": null, "passed": false,
            "failure": error.to_string(),
            "elapsed_seconds": started.elapsed().as_secs_f64(),
        }),
    };
    write_private(
        &evidence.join(format!("{name}-result.json")),
        &serde_json::to_vec_pretty(&result)?,
    )?;
    let status = status.with_context(|| format!("start Receiving {name}"))?;
    ensure!(
        status.success(),
        "Receiving {name} failed with {status}; see {} and {}",
        stdout_path.display(),
        stderr_path.display(),
    );
    Ok(())
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
        if let Ok(output) = output {
            if output.status.success() {
                let _ = fs::write(cluster.evidence.join(name), output.stdout);
            }
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

pub(super) async fn remove(cluster: &mut Resources) -> anyhow::Result<()> {
    let reader_result = if let Some((cancellation, mut task)) = cluster.reader.take() {
        cancellation.cancel();
        match tokio::time::timeout(std::time::Duration::from_secs(10), &mut task).await {
            Ok(result) => result
                .context("join the production CDC reader")
                .and_then(|result| result),
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err(anyhow::anyhow!(
                    "the production CDC reader did not stop within 10 seconds"
                ))
            }
        }
    } else {
        Ok(())
    };
    if cluster.owned {
        checked(
            Command::new(&cluster.lifecycle)
                .arg("remove")
                .arg(&cluster.name)
                .arg(&cluster.work)
                .arg(&cluster.host_image)
                .args(cluster.gates_image.iter())
                .args(cluster.identity_image.iter()),
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
            cancellation.cancel();
            task.abort();
        }
        if self.owned {
            let status = std::process::Command::new(&self.lifecycle)
                .arg("remove")
                .arg(&self.name)
                .arg(&self.work)
                .arg(&self.host_image)
                .args(self.gates_image.iter())
                .args(self.identity_image.iter())
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use super::{kind_address, postgres_host_port, record_build};
    use serde_json::{Value, json};
    use tokio::process::Command;
    use wamn_test_infrastructure::scratch::ScratchRoot;

    #[tokio::test]
    async fn failed_image_build_retains_command_status_and_output_bytes() {
        let root = ScratchRoot(
            std::env::temp_dir().join(format!("receiving-build-output-{}", uuid::Uuid::new_v4())),
        );
        fs::create_dir(root.path()).unwrap();
        let script = "printf 'stdout\\000data\\n'; printf 'stderr\\377data\\n' >&2; exit 7";
        let failure = record_build(
            root.path(),
            "build-images",
            Command::new("/bin/sh")
                .current_dir(root.path())
                .env("WAMN_TEST_PRIVATE_VALUE", "not-for-evidence")
                .args(["-c", script]),
        )
        .await
        .unwrap_err();
        assert!(failure.to_string().contains("build-images.stderr.log"));
        assert_eq!(
            fs::read(root.path().join("build-images.stdout.log")).unwrap(),
            b"stdout\0data\n"
        );
        assert_eq!(
            fs::read(root.path().join("build-images.stderr.log")).unwrap(),
            b"stderr\xffdata\n"
        );
        let command = fs::read_to_string(root.path().join("build-images-command.json")).unwrap();
        assert!(!command.contains("not-for-evidence"));
        let command: Value = serde_json::from_str(&command).unwrap();
        assert_eq!(command["argv"], json!(["/bin/sh", "-c", script]));
        assert_eq!(command["cwd"], json!(root.path()));
        let result: Value = serde_json::from_slice(
            &fs::read(root.path().join("build-images-result.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(result["exit_code"], 7);
        assert_eq!(result["passed"], false);
        assert!(result["signal"].is_null());
        for file in fs::read_dir(root.path()).unwrap() {
            assert_eq!(
                file.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn image_build_spawn_failure_is_retained_before_returning() {
        let root = ScratchRoot(
            std::env::temp_dir().join(format!("receiving-build-spawn-{}", uuid::Uuid::new_v4())),
        );
        fs::create_dir(root.path()).unwrap();
        assert!(
            record_build(
                root.path(),
                "build-identity",
                Command::new(root.path().join("absent-build-command")).current_dir(root.path()),
            )
            .await
            .is_err()
        );
        let result: Value = serde_json::from_slice(
            &fs::read(root.path().join("build-identity-result.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(result["passed"], false);
        assert!(result["exit_code"].is_null());
        assert!(!result["failure"].as_str().unwrap().is_empty());
        assert!(
            fs::read(root.path().join("build-identity.stdout.log"))
                .unwrap()
                .is_empty()
        );
        assert!(
            fs::read(root.path().join("build-identity.stderr.log"))
                .unwrap()
                .is_empty()
        );
        assert!(root.path().join("build-identity-command.json").is_file());
    }

    #[test]
    fn rejects_wildcard_extra_and_unallocated_postgres_ports() {
        let valid = json!({"Ports":{"5432/tcp":[{"HostIp":"127.0.0.1","HostPort":"15432"}]}});
        assert_eq!(postgres_host_port(&valid).unwrap(), 15432);
        for binding in [
            json!([{"HostIp":"0.0.0.0","HostPort":"15432"}]),
            json!([{"HostIp":"127.0.0.1","HostPort":"0"}]),
            json!([{"HostIp":"127.0.0.1","HostPort":"15432"},{"HostIp":"::","HostPort":"15432"}]),
            json!([]),
        ] {
            assert!(postgres_host_port(&json!({"Ports":{"5432/tcp":binding}})).is_err());
        }
    }

    #[test]
    fn requires_the_owned_kind_network_address() {
        assert_eq!(
            kind_address(&json!({"Networks":{"kind":{"IPAddress":"172.18.0.4"}}}))
                .unwrap()
                .to_string(),
            "172.18.0.4"
        );
        for invalid in [
            json!({"Networks":{"bridge":{"IPAddress":"172.18.0.4"}}}),
            json!({"Networks":{"kind":{"IPAddress":"0.0.0.0"}}}),
            json!({"Networks":{"kind":{"IPAddress":"::1"}}}),
        ] {
            assert!(kind_address(&invalid).is_err());
        }
    }
}
