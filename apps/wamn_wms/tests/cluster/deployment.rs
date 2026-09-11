//! WMS-owned deployment files and the released HTTP endpoint.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use wamn_gate_harness::journey::JourneyDocument;

use super::bootstrap::{self, BootstrapFiles};

pub(super) async fn preflight(lifecycle: &Path, cluster: &str, image: &str) -> anyhow::Result<()> {
    checked(Command::new(lifecycle).args(["docker-version", cluster])).await?;
    let clusters = checked(Command::new(lifecycle).args(["clusters", cluster])).await?;
    ensure!(
        !String::from_utf8(clusters)?
            .lines()
            .any(|name| name == cluster),
        "the requested kind cluster already exists"
    );
    let containers = checked(Command::new(lifecycle).args(["containers", cluster])).await?;
    for line in String::from_utf8(containers)?.lines() {
        let name: String = serde_json::from_str(line)?;
        ensure!(
            ![
                "postgres",
                "registry",
                "nats",
                "minio",
                "control-plane",
                "worker",
                "worker2",
                "demo-proxy"
            ]
            .iter()
            .any(|suffix| name == format!("{cluster}-{suffix}")),
            "an exact owned container name already exists"
        );
    }
    let images = checked(Command::new(lifecycle).args(["images", cluster])).await?;
    for line in String::from_utf8(images)?.lines() {
        let mut fields = serde_json::Deserializer::from_str(line).into_iter::<String>();
        let repository = fields
            .next()
            .context("Docker names an image repository")??;
        let tag = fields.next().context("Docker names an image tag")??;
        ensure!(
            format!("{repository}:{tag}") != image,
            "the owned image name already exists"
        );
    }
    Ok(())
}

pub(super) fn prepare_image(target: &Path, work: &Path, head: &str) -> anyhow::Result<()> {
    let directory = work.join("host-image");
    fs::create_dir(&directory)?;
    fs::copy(target.join("debug/wamn-host"), directory.join("wamn-host"))
        .context("copy the already built native host")?;
    fs::write(
        directory.join("Dockerfile"),
        format!(
            "FROM debian:trixie-slim\n\
         RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \\\n          && rm -rf /var/lib/apt/lists/*\n\
         COPY wamn-host /usr/local/bin/wamn-host\n\
         ENV HOME=/tmp\n\
         LABEL wamn.dev/source-head=\"{head}\" wamn.dev/build-profile=\"debug\"\n\
         ENTRYPOINT [\"/usr/local/bin/wamn-host\"]\n"
        ),
    )?;
    Ok(())
}

pub(super) async fn inspect(lifecycle: &Path, container: &str) -> anyhow::Result<Value> {
    let bytes = checked(Command::new(lifecycle).args(["inspect", container])).await?;
    serde_json::from_slice(&bytes).context("read the owned container network settings")
}

pub(super) async fn wait_postgres(admin_url: &str) -> anyhow::Result<()> {
    for _ in 0..30 {
        if let Ok(Ok((client, connection))) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_postgres::connect(admin_url, tokio_postgres::NoTls),
        )
        .await
        {
            let task = tokio::spawn(connection);
            let ready = client.simple_query("SELECT 1").await;
            drop(client);
            task.abort();
            if ready.is_ok() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!("owned PostgreSQL did not accept an external connection after 30 attempts")
}

pub(super) async fn wait_registry(authority: &str, auth_file: &Path) -> anyhow::Result<()> {
    let auth: Value = serde_json::from_slice(&fs::read(auth_file)?)?;
    let username = auth["auths"][authority]["username"]
        .as_str()
        .context("registry username exists")?;
    let password = auth["auths"][authority]["password"]
        .as_str()
        .context("registry password exists")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let url = format!("http://{authority}/v2/");
    for _ in 0..30 {
        if let Ok(response) = client.get(&url).send().await {
            if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                let authenticated = client
                    .get(&url)
                    .basic_auth(username, Some(password))
                    .send()
                    .await?;
                ensure!(
                    authenticated.status() == reqwest::StatusCode::OK,
                    "registry refused its private credential"
                );
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!("owned registry did not require authentication after 30 attempts")
}

pub(super) async fn wait_minio(endpoint: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    for _ in 0..30 {
        if let Ok(response) = client
            .get(format!("{endpoint}/minio/health/live"))
            .send()
            .await
        {
            if response.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!("owned labels store did not become ready after 30 attempts")
}

pub(super) async fn install_application_secrets(
    document: &JourneyDocument,
    files: &BootstrapFiles,
    cluster: &str,
    work: &Path,
    database_host: &str,
) -> anyhow::Result<()> {
    for secret in super::application::host_secrets(document, database_host)? {
        let body: Value = serde_json::from_slice(&fs::read(secret.path)?)?;
        apply_secret(cluster, work, &body).await?;
    }
    let credentials = bootstrap::object_store_credentials(files);
    let body = json!({"apiVersion":"v1","kind":"Secret",
        "metadata":{"name":"wamn-object-store-credentials-acme--wms--dev","namespace":document.host_secret_namespace},
        "type":"Opaque","stringData":{"credentials.json":serde_json::to_string(&json!({
            "wms":{"labels-store":serde_json::to_string(&credentials)?}
        }))?}});
    apply_secret(cluster, work, &body).await?;
    let registry = fs::read_to_string(&document.registry_auth_file)?;
    apply_secret(
        cluster,
        work,
        &json!({"apiVersion":"v1","kind":"Secret",
            "metadata":{"name":"wamn-registry-pull","namespace":document.host_secret_namespace},
            "type":"kubernetes.io/dockerconfigjson","stringData":{".dockerconfigjson":registry}
        }),
    )
    .await?;
    let mut caller: Value =
        serde_json::from_slice(&fs::read(&document.route_caller_secret_output)?)?;
    caller["metadata"]["namespace"] = json!(document.host_secret_namespace);
    apply_secret(cluster, work, &caller).await
}

pub(super) async fn apply_secret(cluster: &str, work: &Path, body: &Value) -> anyhow::Result<()> {
    let mut child = kubectl(cluster, work)
        .args(["apply", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start private Secret installation")?;
    let written = match child.stdin.take() {
        Some(mut input) => input
            .write_all(&serde_json::to_vec(body)?)
            .await
            .context("write the private Secret input"),
        None => Err(anyhow::anyhow!("private Secret input is unavailable")),
    };
    let status = child
        .wait()
        .await
        .context("wait for private Secret installation")?;
    written?;
    ensure!(
        status.success(),
        "private Secret installation failed: {status}"
    );
    Ok(())
}

pub(super) async fn publish_runtime(
    repository: &Path,
    work: &Path,
    artifact: &Path,
    reference: &str,
    result: &Path,
) -> anyhow::Result<String> {
    let binary = checked(&mut Command::new(repository.join("tools/install-wash"))).await?;
    let binary = String::from_utf8(binary)?.trim().to_owned();
    ensure!(
        !binary.is_empty(),
        "install-wash returned no executable path"
    );
    let output = Command::new(binary)
        .env("DOCKER_CONFIG", work.join("docker"))
        .args(["-o", "json", "oci", "push", "--insecure", reference])
        .arg(artifact)
        .kill_on_drop(true)
        .output()
        .await
        .context("publish the existing runtime component")?;
    ensure!(
        output.status.success(),
        "runtime component publication failed: {}",
        output.status
    );
    let body: Value =
        serde_json::from_slice(&output.stdout).context("read the component publication result")?;
    ensure!(
        body["success"] == true && body["data"]["success"] == true,
        "runtime publication did not succeed"
    );
    let digest = body["data"]["digest"]
        .as_str()
        .context("runtime publication returned its digest")?;
    ensure!(
        digest
            .strip_prefix("sha256:")
            .is_some_and(|tail| tail.len() == 64
                && tail
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))),
        "runtime publication digest is invalid"
    );
    fs::write(result, serde_json::to_vec_pretty(&body)?)?;
    let repository = reference
        .rsplit_once(':')
        .context("the component publication used a tag")?
        .0;
    Ok(format!("{repository}@{digest}"))
}

pub(super) async fn expose_route(
    cluster: &str,
    work: &Path,
    namespace: &str,
    lifecycle: &Path,
    route_host: &str,
    demo: bool,
) -> anyhow::Result<String> {
    let slices = checked(kubectl(cluster, work).args([
        "-n",
        namespace,
        "get",
        "endpointslices",
        "-l",
        "kubernetes.io/service-name=flow-http",
        "-o",
        "json",
    ]))
    .await?;
    let slices: Value = serde_json::from_slice(&slices)?;
    let mut endpoints = Vec::new();
    for slice in slices["items"]
        .as_array()
        .context("route EndpointSlices have items")?
    {
        for endpoint in slice["endpoints"]
            .as_array()
            .context("route EndpointSlice has endpoints")?
        {
            endpoints.push(json!({"addresses":endpoint["addresses"],"conditions":{"ready":true}}));
        }
    }
    ensure!(
        !endpoints.is_empty(),
        "flow-http has no placed endpoint to mirror"
    );
    let mut body = json!({"apiVersion":"v1","kind":"List","items":[
        {"apiVersion":"v1","kind":"Service","metadata":{"name":"flow-http-nodeport","namespace":namespace},
         "spec":{"type":"NodePort","ports":[{"name":"http","port":80,"targetPort":80,"protocol":"TCP"}]}},
        {"apiVersion":"discovery.k8s.io/v1","kind":"EndpointSlice",
         "metadata":{"name":"flow-http-nodeport","namespace":namespace,"labels":{"kubernetes.io/service-name":"flow-http-nodeport"}},
         "addressType":"IPv4","ports":[{"name":"http","port":80,"protocol":"TCP"}],"endpoints":endpoints}
    ]});
    if demo {
        body["items"][0]["spec"]["ports"][0]["nodePort"] = json!(30950);
    }
    let path = work.join("flow-http-nodeport.json");
    fs::write(&path, serde_json::to_vec_pretty(&body)?)?;
    checked(kubectl(cluster, work).args(["apply", "-f"]).arg(&path)).await?;
    let service = checked(kubectl(cluster, work).args([
        "-n",
        namespace,
        "get",
        "service",
        "flow-http-nodeport",
        "-o",
        "json",
    ]))
    .await?;
    let service: Value = serde_json::from_slice(&service)?;
    let port = service["spec"]["ports"][0]["nodePort"]
        .as_u64()
        .context("route NodePort is allocated")?;
    ensure!(
        (1000..=65535).contains(&port),
        "route NodePort is outside the retained port range"
    );
    let node = inspect(lifecycle, &format!("{cluster}-control-plane")).await?;
    let address = bootstrap::kind_address(&node)?;
    let endpoint = format!("http://{address}:{port}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    for _ in 0..60 {
        if client
            .get(format!("{endpoint}/"))
            .header("Host", route_host)
            .send()
            .await
            .is_ok()
        {
            return Ok(endpoint);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!("the released route answered no HTTP status after 60 attempts")
}

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create private file {}", path.display()))?;
    file.write_all(bytes)
        .context("write private application input")
}

pub(super) fn kubectl(cluster: &str, work: &Path) -> Command {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(work.join("kubeconfig"))
        .arg("--context")
        .arg(format!("kind-{cluster}"));
    command
}

pub(super) async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .context("run the WMS setup command")?;
    ensure!(
        output.status.success(),
        "WMS setup command failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

/// Retain the old failure diagnostics before deleting the owned namespace.
pub(super) async fn capture_failure(
    cluster: &str,
    work: &Path,
    evidence: &Path,
) -> anyhow::Result<()> {
    if !work.join("kubeconfig").is_file() {
        return Ok(());
    }
    async fn capture(
        command: &mut Command,
        evidence: &Path,
        name: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let output = command.kill_on_drop(true).output().await?;
        fs::write(evidence.join(name), &output.stdout)?;
        fs::write(evidence.join(format!("{name}.stderr")), &output.stderr)?;
        Ok(output.stdout)
    }
    let mut failures = Vec::new();
    for (name, arguments) in [
        (
            "failure-host-deployment.json",
            vec!["get", "deployment", "hostgroup-default", "-o", "json"],
        ),
        ("failure-pods.json", vec!["get", "pods", "-o", "json"]),
        (
            "failure-nodeport.json",
            vec![
                "get",
                "service,endpointslice",
                "flow-http-nodeport",
                "-o",
                "json",
            ],
        ),
        ("failure-events.json", vec!["get", "events", "-o", "json"]),
        (
            "failure-secret-names.txt",
            vec!["get", "secrets", "-o", "name"],
        ),
        ("failure-jobs.json", vec!["get", "jobs", "-o", "json"]),
    ] {
        if let Err(error) = capture(
            kubectl(cluster, work)
                .args(["--request-timeout=10s", "-n", cluster])
                .args(arguments),
            evidence,
            name,
        )
        .await
        {
            failures.push(format!("{name}: {error:#}"));
        }
    }
    if let Ok(bytes) = fs::read(evidence.join("failure-pods.json")) {
        if let Ok(pods) = serde_json::from_slice::<Value>(&bytes) {
            for pod in pods["items"].as_array().into_iter().flatten() {
                if pod["metadata"]["labels"]["wasmcloud.com/name"] != "hostgroup" {
                    continue;
                }
                let Some(name) = pod["metadata"]["name"].as_str() else {
                    continue;
                };
                for previous in [false, true] {
                    let mut command = kubectl(cluster, work);
                    command.args([
                        "--request-timeout=10s",
                        "-n",
                        cluster,
                        "logs",
                        "--timestamps",
                        "-c",
                        "host",
                        name,
                    ]);
                    if previous {
                        command.arg("--previous");
                    }
                    if let Err(error) = capture(
                        &mut command,
                        evidence,
                        &format!(
                            "failure-{name}{}.log",
                            if previous { ".previous" } else { "" }
                        ),
                    )
                    .await
                    {
                        failures.push(format!("host {name}: {error:#}"));
                    }
                }
            }
        }
    }
    if let Ok(bytes) = fs::read(evidence.join("failure-jobs.json")) {
        if let Ok(jobs) = serde_json::from_slice::<Value>(&bytes) {
            for job in jobs["items"].as_array().into_iter().flatten() {
                if job["status"]["conditions"]
                    .as_array()
                    .is_some_and(|conditions| {
                        conditions.iter().any(|condition| {
                            condition["type"] == "Complete" && condition["status"] == "True"
                        })
                    })
                {
                    continue;
                }
                let Some(name) = job["metadata"]["name"].as_str() else {
                    continue;
                };
                if let Err(error) = capture(
                    kubectl(cluster, work).args([
                        "--request-timeout=10s",
                        "-n",
                        cluster,
                        "logs",
                        &format!("job/{name}"),
                        "--all-containers",
                        "--tail=-1",
                    ]),
                    evidence,
                    &format!("failure-job-{name}.log"),
                )
                .await
                {
                    failures.push(format!("Job {name}: {error:#}"));
                }
            }
        }
    }
    crate::wms_runtime_live::write_result(
        evidence,
        "failure-capture.json",
        &json!({"errors":failures}),
    )
}
