//! Install the platform on a private kind cluster and require an unchanged rerun.
//!
//! Requires Docker with buildx manifest reads, kind, Helm, kubectl, pnpm,
//! package build tools, and debug wamn and wamn-ctl binaries.
//! The runner exports CLOUDSDK_CONFIG=$HOME/.config/gcloud-wamn-agent and
//! DOCKER_CONFIG pointing to a config.json with the GCP gcloud credential helper.
//! The fixture reads deploy/gcp/cluster.k and requires its registry to hold all
//! six images of Chart.yaml's version. It copies them to its registry:2 container
//! and stamps them. It never builds images or needs image build tools.
//! Network access to the GCP registry and public registries is required.
//! No external database, bucket or DNS is required. The test keeps its input,
//! kubeconfig and progress under /var/tmp and removes its cluster and registry.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context as _, ensure};
use chrono::Utc;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::Value;
use wamn_control::environment::document::{DeclaredRelease, EnvironmentDocument};
use wamn_control::kubectl;
use wamn_control::platform::document::ClusterDocument;
use wamn_control::platform::{install, steps};
use wash_runtime::oci::{OciConfig, OciPullPolicy, pull_component, push_component};

struct ClusterGuard {
    name: String,
}

impl Drop for ClusterGuard {
    fn drop(&mut self) {
        match Command::new("kind")
            .args(["delete", "cluster", "--name", &self.name])
            .stdin(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => eprintln!("delete kind cluster {} exited {status}", self.name),
            Err(error) => eprintln!("delete kind cluster {}: {error}", self.name),
        }
        match Command::new("docker")
            .args(["rm", "-f", &format!("{}-registry", self.name)])
            .stdin(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => eprintln!("delete registry {}-registry exited {status}", self.name),
            Err(error) => eprintln!("delete registry {}-registry: {error}", self.name),
        }
    }
}

const STEP_TITLES: [&str; 8] = [
    "4.3 cert-manager and the edge certificate",
    "4.4 Namespaces, runtime operator and internal CA",
    "4.4.1 Collector",
    "4.5 PostgreSQL",
    "4.6 Packages and platform build",
    "4.7 System database, org and identity issuer",
    "4.8 Event NATS",
    "4.9 Platform install inputs",
];

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: docker, kind, helm, kubectl, pnpm, debug CLI binaries, package build tools, agent identity, GCP and public registry access; no image builds"]
async fn platform_apply_kind() -> anyhow::Result<()> {
    let mut suffix = [0u8; 6];
    SystemRandom::new()
        .fill(&mut suffix)
        .map_err(|_| anyhow::anyhow!("generate the cluster suffix"))?;
    let name = format!("platform-apply-{}", hex::encode(suffix));
    let directory = Path::new("/var/tmp").join(&name);
    std::fs::create_dir(&directory).context("create the fixture directory")?;
    let work = directory.join("work");
    std::fs::create_dir(&work).context("create the install work directory")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let version =
        steps::read_platform_version(&repository.join("deploy/platform/release/Chart.yaml"))?;
    let source_registry =
        ClusterDocument::compile(&repository.join("deploy/gcp/cluster.k"))?.registry;
    let agent = Path::new(&std::env::var_os("HOME").context("HOME is required")?)
        .join(".config/gcloud-wamn-agent");
    ensure!(
        std::env::var_os("CLOUDSDK_CONFIG").as_deref() == Some(agent.as_os_str()),
        "export CLOUDSDK_CONFIG={} for the agent identity",
        agent.display()
    );
    let docker_config = std::env::var_os("DOCKER_CONFIG")
        .context("export DOCKER_CONFIG with the GCP gcloud credential helper")?;
    let credentials: Value = serde_json::from_slice(&std::fs::read(
        Path::new(&docker_config).join("config.json"),
    )?)?;
    let host = source_registry.split('/').next().context("registry host")?;
    ensure!(
        credentials["credHelpers"][host] == "gcloud",
        "DOCKER_CONFIG must select the gcloud credential helper for {host}"
    );
    println!("work directory {}", directory.display());
    stamp_registry(
        &repository,
        &source_registry,
        &version,
        &directory.join("source-chart"),
    )?;
    let _cluster = ClusterGuard { name: name.clone() };
    let port = registry(&name)?;
    let file = directory.join("cluster.k");
    std::fs::write(
        &file,
        format!(
            r#"import cluster

cluster.Cluster {{
    provider = "kind"
    project_id = ""
    zone = ""
    cluster_name = "{name}"
    org = "test"
    owner_email = "owner@example.com"
    registry = "localhost:{port}"
    buckets = {{web = "", otel = ""}}
    route_hosts = {{control = "control.example.com"}}
    resend_from = "sender@example.com"
}}
"#
        ),
    )?;
    let document = ClusterDocument::compile(&file).context("compile the kind cluster document")?;
    copy_images(&source_registry, &document.registry, &version).await?;
    let fixture_chart = directory.join("fixture-chart");
    stamp_registry(&repository, &document.registry, &version, &fixture_chart)?;
    let chart: Value = serde_yaml::from_slice(&std::fs::read(fixture_chart.join("values.yaml"))?)?;
    let environments = directory.join("environments");
    std::fs::create_dir(&environments)?;
    let source = repository.join("deploy/gcp/environments/dkk--receiving--dev.k");
    let mut environment = EnvironmentDocument::compile(&source)?;
    environment.org.clone_from(&document.org);
    environment.release = DeclaredRelease::None;
    let domain = document
        .route_hosts
        .control
        .split_once('.')
        .context("control host has a domain")?
        .1;
    environment.route_host = format!("receiving.{domain}");
    let input = environments.join("test--receiving--dev.k");
    std::fs::write(&input, environment.to_kcl())?;
    let mut values: Value = serde_json::from_slice(&std::fs::read(source.with_extension("json"))?)?;
    values["cluster"] = name.clone().into();
    values["context"] = format!("kind-{name}").into();
    values["registry"] = document.registry.clone().into();
    values["route_host"] = environment.route_host.into();
    values["web_client"]["bucket"] = "".into();
    values["gates_image"] = chart["platform"]["gates"]["image"].clone();
    std::fs::write(
        input.with_extension("json"),
        serde_json::to_vec_pretty(&values)?,
    )?;

    let mut progress = std::fs::File::create(directory.join("progress.log"))?;
    let mut first = Vec::new();
    let start = Instant::now();
    println!("platform install start {}", Utc::now().to_rfc3339());
    let installed = install::apply(
        &document,
        &version,
        &repository,
        &environments,
        &work,
        |line| {
            println!("{line}");
            writeln!(progress, "{line}")?;
            first.push(line.to_owned());
            Ok(())
        },
    )
    .await;
    println!(
        "platform install end {} elapsed {:?} result {}",
        Utc::now().to_rfc3339(),
        start.elapsed(),
        if installed.is_ok() { "pass" } else { "fail" }
    );
    installed?;
    require_steps(&first, "done")?;

    let kubeconfig = work.join(format!("{name}.kubeconfig"));
    let context = format!("kind-{name}");
    kubectl::wait(
        &kubeconfig,
        &context,
        "platform",
        "cluster.postgresql.cnpg.io",
        "wamn-pg",
        "condition=Ready",
        "60s",
    )
    .await?;
    kubectl::kubectl(
        &kubeconfig,
        &context,
        "platform",
        &[
            "rollout",
            "status",
            "statefulset/evt-nats",
            "--timeout=300s",
        ],
        None,
    )
    .await?;
    for (namespace, kind, name) in [
        ("hosts", "configmap", "identity-ca"),
        ("edge", "configmap", "identity-ca"),
        ("identity", "secret", "wamn-identity-db"),
        ("platform", "configmap", "wamn-install-platform-system"),
        ("platform", "configmap", "wamn-install-platform-org"),
        (
            "platform",
            "configmap",
            "wamn-install-platform-identity-issuer",
        ),
    ] {
        let object: Option<Value> =
            kubectl::get(&kubeconfig, &context, namespace, kind, name).await?;
        ensure!(object.is_some(), "missing {kind} {namespace}/{name}");
    }

    let mut second = Vec::new();
    install::apply(
        &document,
        &version,
        &repository,
        &environments,
        &work,
        |line| {
            println!("{line}");
            writeln!(progress, "{line}")?;
            second.push(line.to_owned());
            Ok(())
        },
    )
    .await?;
    require_steps(&second, "skipped")?;
    Ok(())
}

fn stamp_registry(
    repository: &Path,
    registry: &str,
    version: &str,
    out: &Path,
) -> anyhow::Result<()> {
    let target = std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into());
    let output = Command::new(repository.join(target).join("debug/wamn-ctl"))
        .current_dir(repository)
        .args([
            "build-platform",
            "--stamp-only",
            "--registry",
            registry,
            "--out",
        ])
        .arg(out)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("stamp registry images of platform {version}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprint!("{stderr}");
    if !output.status.success() {
        ensure!(
            stderr.contains(version),
            "stamp-only error does not name version {version}: {stderr}"
        );
        anyhow::bail!("stamp registry images of platform {version}: {stderr}");
    }
    Ok(())
}

async fn copy_images(source: &str, destination: &str, version: &str) -> anyhow::Result<()> {
    for name in ["wamn-host", "wamn-runner", "wamn-gates", "wamn-identity"] {
        let from = format!("{source}/{name}:{version}");
        let to = format!("{destination}/{name}:{version}");
        docker(&["pull", &from])?;
        docker(&["tag", &from, &to])?;
        docker(&["push", &to])?;
    }
    for role in ["http", "materializer"] {
        let from = format!("{source}/wamn/{role}:{version}");
        let to = format!("{destination}/wamn/{role}:{version}");
        let (bytes, _) = pull_component(&from, OciConfig::default(), OciPullPolicy::Always)
            .await
            .with_context(|| format!("pull {from}"))?;
        push_component(&to, &bytes, OciConfig::new_insecure(), None)
            .await
            .with_context(|| format!("push {to}"))?;
    }
    Ok(())
}

fn registry(cluster: &str) -> anyhow::Result<u16> {
    let network = Command::new("docker")
        .args(["network", "inspect", "kind"])
        .stdin(Stdio::null())
        .output()
        .context("look for the kind network")?;
    if !network.status.success() {
        docker(&["network", "create", "kind"])?;
    }
    let name = format!("{cluster}-registry");
    docker(&[
        "run",
        "-d",
        "--name",
        &name,
        "--network",
        "kind",
        "-p",
        "127.0.0.1:0:5000",
        "registry:2",
    ])?;
    let binding = docker(&["port", &name, "5000/tcp"])?;
    binding
        .trim()
        .strip_prefix("127.0.0.1:")
        .context("the registry has no loopback port")?
        .parse()
        .context("read the registry port")
}

fn docker(arguments: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("docker")
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .context("run the fixture Docker command")?;
    ensure!(
        output.status.success(),
        "docker {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("read the fixture Docker output")
}

fn require_steps(lines: &[String], result: &str) -> anyhow::Result<()> {
    let completed: Vec<_> = lines
        .iter()
        .filter(|line| line.starts_with("4.") && !line.ends_with(": starting"))
        .map(String::as_str)
        .collect();
    let expected = STEP_TITLES.map(|title| format!("{title}: {result}"));
    ensure!(
        completed == expected,
        "expected {expected:?}, got {completed:?}"
    );
    Ok(())
}
