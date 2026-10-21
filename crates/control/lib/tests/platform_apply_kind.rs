//! Install the platform on a private kind cluster and require an unchanged rerun.
//!
//! Requires Docker with buildx manifest reads, kind, Helm, kubectl, pnpm, OpenSSL,
//! package build tools, and debug wamn and wamn-ctl binaries.
//! The runner exports CLOUDSDK_CONFIG=$HOME/.config/gcloud-wamn-agent and
//! DOCKER_CONFIG pointing to a config.json with the GCP gcloud credential helper.
//! The fixture reads deploy/gcp/cluster.k and requires its registry to hold all
//! six images of Chart.yaml's version. It copies them to its registry:2 container
//! and stamps them. It never builds images or needs image build tools.
//! Network access to the GCP registry and public registries is required.
//! No external database, bucket or DNS is required. The test keeps its input,
//! kubeconfig and progress under /var/tmp and removes its cluster and registries.
//! A TLS artifact registry serves Receiving and its unqualified release.
//! The test requires the qualification refusal and skips through step 4.14.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
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
            .args([
                "rm",
                "-f",
                &format!("{}-registry", self.name),
                &format!("{}-artifacts", self.name),
            ])
            .stdin(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => eprintln!("delete registry {}-registry exited {status}", self.name),
            Err(error) => eprintln!("delete registry {}-registry: {error}", self.name),
        }
    }
}

const STEP_TITLES: [&str; 12] = [
    "4.3 cert-manager and the edge certificate",
    "4.4 Namespaces, runtime operator and internal CA",
    "4.4.1 Collector",
    "4.5 PostgreSQL",
    "4.6 Packages and platform build",
    "4.7 System database, org and identity issuer",
    "4.8 Event NATS",
    "4.9 Platform install inputs",
    "4.11 First apply",
    "4.12 Identity and session key",
    "4.13 Packages",
    "4.14 Publish releases",
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
    let artifacts = artifact_registry(&name, &directory).await?;
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
    let host_group = format!("host-group-{}.yaml", environment.project);
    std::fs::copy(
        repository.join("deploy/gcp").join(&host_group),
        directory.join(&host_group),
    )?;
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
    values["route_host"] = environment.route_host.clone().into();
    values["web_client"]["bucket"] = "".into();
    values["gates_image"] = chart["platform"]["gates"]["image"].clone();
    std::fs::write(
        input.with_extension("json"),
        serde_json::to_vec_pretty(&values)?,
    )?;

    // The fixture supplies the owner input before the verb starts.
    let kubeconfig = work.join(format!("{name}.kubeconfig"));
    let context = document.provider.kubeconfig(&document, &kubeconfig).await?;
    kubectl::apply(
        &kubeconfig,
        &context,
        "identity",
        &json!({
            "apiVersion":"v1","kind":"Namespace","metadata":{"name":"identity"}
        }),
    )
    .await?;
    kubectl::apply(
        &kubeconfig,
        &context,
        "identity",
        &json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"identity-resend","namespace":"identity"},
            "stringData":{"api-key":"re_platform_fixture"}
        }),
    )
    .await?;

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
        &artifacts,
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
        (
            "identity",
            "secret",
            "wamn-session-role-reader-test--receiving--dev",
        ),
        (
            "platform",
            "configmap",
            "wamn-install-platform-first-apply-test--receiving--dev",
        ),
        (
            "platform",
            "configmap",
            "wamn-install-platform-session-key-activate",
        ),
        (
            "platform",
            "configmap",
            "wamn-install-platform-identity-check",
        ),
    ] {
        let object: Option<Value> =
            kubectl::get(&kubeconfig, &context, namespace, kind, name).await?;
        ensure!(object.is_some(), "missing {kind} {namespace}/{name}");
    }
    // nats-server opens to anonymous clients when its users list is empty.
    let anonymous = kubectl::kubectl(
        &kubeconfig,
        &context,
        "platform",
        &[
            "exec",
            "-i",
            "evt-nats-0",
            "--",
            "nc",
            "-w",
            "2",
            "127.0.0.1",
            "4222",
        ],
        Some(b"CONNECT {}\r\nPING\r\n"),
    )
    .await?;
    ensure!(
        anonymous.contains("Authorization Violation") && !anonymous.contains("PONG"),
        "event NATS accepted an anonymous connection: {anonymous}"
    );
    let prefix = format!("{}: release = \"", input.display());
    let digest = first
        .iter()
        .find_map(|line| {
            line.strip_prefix(&prefix)
                .and_then(|line| line.strip_suffix('"'))
        })
        .context("publication printed no release line")?
        .to_owned();
    ensure!(
        EnvironmentDocument::compile(&input)?.release == DeclaredRelease::None,
        "platform apply changed the environment file"
    );
    environment.release = DeclaredRelease::Digest(digest.clone());
    std::fs::write(&input, environment.to_kcl())?;
    let refused = install::apply(
        &document,
        &version,
        &repository,
        &environments,
        &work,
        &artifacts,
        |line| {
            println!("{line}");
            writeln!(progress, "{line}")?;
            Ok(())
        },
    )
    .await
    .expect_err("an unqualified release must refuse");
    let refusal = format!("{refused:#}");
    ensure!(
        refusal.contains(&format!(
            "release {digest} has no qualification on the chart's image set:"
        )),
        "{refusal}"
    );
    ensure!(
        refusal.contains(&format!("qualify-release --release-digest {digest}")),
        "{refusal}"
    );
    writeln!(progress, "{refusal}")?;
    let secrets = secret_versions(&kubeconfig, &context).await?;
    let mut logs = format!("{}\n{refusal}", first.join("\n"));
    for entry in std::fs::read_dir(work.join("jobs"))? {
        logs.push_str(&std::fs::read_to_string(entry?.path())?);
    }
    for (_, secret) in &secrets {
        for (key, encoded) in secret["data"].as_object().context("Secret has no data")? {
            let encoded = encoded.as_str().context("Secret data is not text")?;
            let decoded = STANDARD.decode(encoded).context("decode fixture Secret")?;
            let value = std::str::from_utf8(&decoded).context("fixture Secret is not UTF-8")?;
            let mut private = Vec::new();
            if matches!(key.as_str(), "password" | "api-key" | "url") {
                private.push(value.to_owned());
                if key == "url" {
                    private_url_password(value, &mut private);
                }
            } else if matches!(
                key.as_str(),
                "binding.json" | "target.json" | "authorization.conf"
            ) {
                let value: Value =
                    serde_json::from_str(value.strip_prefix("authorization: ").unwrap_or(value))?;
                private_values(&value, &mut private);
            }
            for value in private {
                ensure!(
                    !logs.contains(&value) && !logs.contains(&STANDARD.encode(&value)),
                    "a credential reached the install progress or Job logs"
                );
            }
        }
    }

    let mut second = Vec::new();
    let repeated = install::apply(
        &document,
        &version,
        &repository,
        &environments,
        &work,
        &artifacts,
        |line| {
            println!("{line}");
            writeln!(progress, "{line}")?;
            second.push(line.to_owned());
            Ok(())
        },
    )
    .await
    .expect_err("the unchanged unqualified release still refuses");
    ensure!(
        format!("{repeated:#}").contains(&format!("release {digest} has no qualification")),
        "{repeated:#}"
    );
    require_steps(&second, "skipped")?;
    for ((name, first), (_, second)) in secrets
        .iter()
        .zip(secret_versions(&kubeconfig, &context).await?)
    {
        ensure!(
            first["metadata"]["resourceVersion"] == second["metadata"]["resourceVersion"],
            "unchanged apply rewrote Secret {name}"
        );
    }
    Ok(())
}

fn private_values(value: &Value, values: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(key.as_str(), "password" | "database_url") {
                    if let Some(value) = value.as_str() {
                        values.push(value.to_owned());
                        if key == "database_url" {
                            private_url_password(value, values);
                        }
                    }
                } else {
                    private_values(value, values);
                }
            }
        }
        Value::Array(array) => array.iter().for_each(|value| private_values(value, values)),
        _ => {}
    }
}

fn private_url_password(value: &str, values: &mut Vec<String>) {
    if let Ok(url) = url::Url::parse(value)
        && let Some(password) = url.password()
    {
        values.push(password.to_owned());
    }
}

async fn secret_versions(kubeconfig: &Path, context: &str) -> anyhow::Result<Vec<(String, Value)>> {
    let names = [
        ("platform", "wamn-pg-superuser"),
        ("platform", "evt-nats-authorization"),
        ("platform", "evt-nats-test--receiving--dev-provisioning"),
        ("platform", "evt-nats-test--receiving--dev-publisher"),
        ("platform", "evt-nats-test--receiving--dev-observer"),
        ("hosts", "evt-nats-test--receiving--dev-publisher"),
        ("hosts", "evt-nats-test--receiving--dev-runtime"),
        ("hosts", "evt-nats-test--receiving--dev-materializer"),
        ("identity", "wamn-session-role-reader-test--receiving--dev"),
        ("identity", "identity-resend"),
        ("identity", "wamn-identity-db"),
    ];
    let mut secrets = Vec::new();
    for (namespace, name) in names {
        let secret = kubectl::get(kubeconfig, context, namespace, "secret", name)
            .await?
            .with_context(|| format!("missing Secret {namespace}/{name}"))?;
        secrets.push((format!("{namespace}/{name}"), secret));
    }
    Ok(secrets)
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

async fn artifact_registry(
    cluster: &str,
    directory: &Path,
) -> anyhow::Result<wamn_control::package_artifact::PackageRegistry> {
    use std::os::unix::fs::DirBuilderExt as _;
    let certs = directory.join("artifact-registry");
    std::fs::DirBuilder::new().mode(0o700).create(&certs)?;
    let name = format!("{cluster}-artifacts");
    certificate(&certs, "127.0.0.1")?;
    docker(&[
        "run",
        "-d",
        "--name",
        &name,
        "--network",
        "kind",
        "-v",
        &format!("{}:/certs:ro", certs.display()),
        "-e",
        "REGISTRY_HTTP_TLS_CERTIFICATE=/certs/tls.crt",
        "-e",
        "REGISTRY_HTTP_TLS_KEY=/certs/tls.key",
        "registry:2",
    ])?;
    let address = docker(&[
        "inspect",
        "--format",
        "{{(index .NetworkSettings.Networks \"kind\").IPAddress}}",
        &name,
    ])?
    .trim()
    .to_owned();
    certificate(&certs, &address)?;
    docker(&["restart", &name])?;
    let authority = format!("{address}:5000");
    let mut password = [0u8; 32];
    SystemRandom::new()
        .fill(&mut password)
        .map_err(|_| anyhow::anyhow!("generate registry fixture credential"))?;
    let auth = certs.join("config.json");
    std::fs::write(
        &auth,
        serde_json::to_vec(&json!({"auths": {&authority: {
            "username": "fixture", "password": hex::encode(password)
        }}}))?,
    )?;
    let certificate = reqwest::Certificate::from_pem(&std::fs::read(certs.join("ca.crt"))?)?;
    let client = reqwest::Client::builder()
        .add_root_certificate(certificate)
        .build()?;
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if client
            .get(format!("https://{authority}/v2/"))
            .send()
            .await
            .is_ok_and(|reply| reply.status().is_success())
        {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "artifact registry did not become ready"
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Ok(wamn_control::package_artifact::PackageRegistry {
        artifact_base: format!("{authority}/packages"),
        registry_auth_file: auth,
        insecure_registry: false,
        oci_ca_paths: vec![certs.join("ca.crt")],
    })
}

// rustls refuses a CA certificate as the server certificate, so the fixture
// signs a CA:FALSE server certificate with its own CA.
fn certificate(directory: &Path, address: &str) -> anyhow::Result<()> {
    let ca = directory.join("ca.crt");
    if !ca.exists() {
        openssl(
            Command::new("openssl")
                .args([
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-days",
                    "1",
                    "-subj",
                    "/CN=platform-artifacts-ca",
                    "-keyout",
                ])
                .arg(directory.join("ca.key"))
                .arg("-out")
                .arg(&ca),
        )?;
    }
    openssl(
        Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=platform-artifacts",
                "-addext",
                &format!("subjectAltName=IP:{address}"),
                "-addext",
                "basicConstraints=critical,CA:FALSE",
                "-CA",
            ])
            .arg(&ca)
            .arg("-CAkey")
            .arg(directory.join("ca.key"))
            .arg("-keyout")
            .arg(directory.join("tls.key"))
            .arg("-out")
            .arg(directory.join("tls.crt")),
    )
}

fn openssl(command: &mut Command) -> anyhow::Result<()> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .context("create the registry TLS certificate")?;
    ensure!(
        output.status.success(),
        "openssl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    let completed = completed
        .iter()
        .map(|line| {
            let (step, elapsed) = line.rsplit_once(' ').context("step has no elapsed time")?;
            let seconds: f64 = elapsed
                .strip_suffix('s')
                .context("elapsed time has no seconds suffix")?
                .parse()?;
            ensure!(
                seconds.is_finite() && seconds >= 0.0,
                "invalid elapsed time"
            );
            Ok(step)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let expected = STEP_TITLES.map(|title| format!("{title}: {result}"));
    ensure!(
        completed == expected,
        "expected {expected:?}, got {completed:?}"
    );
    Ok(())
}
