//! The platform journey uses the product CLI and observes its deployed owners.

use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::{DirBuilderExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use base64::Engine as _;
use rustix::process::{Pid, Signal, kill_process_group};
use serde::Deserialize as _;
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_dev_control::owned_command;
use wamn_environment_control::platform::{
    artifact_plan,
    document::{ClusterDocument, Provider},
    provider, resources,
};
use wamn_release_control::release_chart::Target;

#[path = "../../environment/tests/support/platform_images.rs"]
pub mod platform_images;

#[path = "support/application_deploy.rs"]
mod application_deploy;

const COMMAND_LIMIT: Duration = Duration::from_mins(15);
const POLL_LIMIT: Duration = Duration::from_mins(5);

struct Run {
    repository: PathBuf,
    directory: PathBuf,
    checkout: PathBuf,
    document: ClusterDocument,
    target: Target,
    binary: PathBuf,
    chart: PathBuf,
    kcl: PathBuf,
    owns_cluster: bool,
    cleaned: bool,
}

impl Drop for Run {
    fn drop(&mut self) {
        if !self.cleaned {
            eprintln!(
                "Retained private inputs and logs: {}",
                self.directory.display()
            );
            if self.owns_cluster {
                eprintln!("kind delete cluster --name {}", self.document.cluster_name);
            }
            if self.checkout.exists() {
                eprintln!(
                    "git -c core.hooksPath=/dev/null -C '{}' worktree remove --force '{}'",
                    self.repository.display(),
                    self.checkout.display()
                );
            }
            eprintln!("rm -rf -- '{}'", self.directory.display());
        }
    }
}

// The cluster document is the one target input authorized by wamn-alrvy.18.
// The namespace annotation records consent to use the platform as disposable.
#[tokio::test]
#[ignore = "requires: bin:kind, bin:docker, bin:kubectl, bin:helm, bin:openssl, bin:script"]
async fn application_deploy_kind() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:kind, bin:docker, bin:kubectl, bin:helm, bin:openssl, bin:script",
    );
    focused(application_deploy::Case::Application).await
}

#[tokio::test]
#[ignore = "requires: env:WAMN_TEST_CLUSTER, env:WAMN_RELEASE_CHART, env:WAMN_TEST_OWNER_PASSWORD"]
async fn platform_upgrade_kind() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_TEST_CLUSTER, env:WAMN_RELEASE_CHART, env:WAMN_TEST_OWNER_PASSWORD",
    );
    focused(application_deploy::Case::PlatformUpgrade).await
}

#[tokio::test]
#[ignore = "requires: bin:kind, bin:docker, bin:kubectl, bin:helm, bin:openssl, bin:script"]
async fn sql_job_kind() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:kind, bin:docker, bin:kubectl, bin:helm, bin:openssl, bin:script",
    );
    focused(application_deploy::Case::SqlJob).await
}

#[tokio::test]
#[ignore = "requires: bin:kind, bin:docker, bin:kubectl, bin:helm, bin:openssl, bin:script"]
async fn same_name_reset_kind() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:kind, bin:docker, bin:kubectl, bin:helm, bin:openssl, bin:script",
    );
    focused(application_deploy::Case::Reset).await
}

#[tokio::test]
#[ignore = "requires: bin:kind, bin:docker, bin:kubectl, bin:helm, bin:openssl"]
async fn platform_install_recovery_kind() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:kind, bin:docker, bin:kubectl, bin:helm, bin:openssl",
    );
    fresh(false).await
}

/// Fresh-platform acceptance includes installation, recovery, application, SQL, and reset.
#[tokio::test]
#[ignore = "requires: bin:kind, bin:docker, bin:kubectl, bin:helm, bin:openssl, bin:script"]
async fn platform_apply_kind() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:kind, bin:docker, bin:kubectl, bin:helm, bin:openssl, bin:script",
    );
    fresh(true).await
}

fn inputs() -> anyhow::Result<(PathBuf, PathBuf, PathBuf)> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let binary = std::env::current_exe()?
        .parent()
        .context("test directory")?
        .parent()
        .context("debug directory")?
        .join("wamn");
    ensure!(
        binary.is_file() && binary.with_file_name("wamn-ctl").is_file(),
        "build wamn-ctl --bin wamn --bin wamn-ctl first"
    );
    let kcl = PathBuf::from(
        std::env::var_os("WAMN_KCL").context("set WAMN_KCL to the prepared compiler")?,
    )
    .canonicalize()?;
    eprintln!("Selected CLI: {}", binary.display());
    Ok((repository, binary, kcl))
}

async fn focused(case: application_deploy::Case) -> anyhow::Result<()> {
    let (repository, binary, kcl) = inputs()?;
    let file = PathBuf::from(
        std::env::var_os("WAMN_TEST_CLUSTER")
            .context("set WAMN_TEST_CLUSTER to the disposable platform cluster.k")?,
    )
    .canonicalize()?;
    let document = ClusterDocument::compile(&file)?;
    ensure!(
        document.provider == Provider::Kind,
        "focused tests require kind"
    );
    let mut run = prepare(&repository, Path::new(""), &binary, &kcl, Some(document))?;
    run.target.context =
        provider::existing_kubeconfig(&run.document, &run.target.kubeconfig).await?;
    let namespace = get(&run, "", "namespace", "platform").await?;
    ensure!(
        namespace["metadata"]["annotations"]["wamn.dev/disposable"] == "true",
        "platform is not recorded as disposable: annotate namespace platform wamn.dev/disposable=true"
    );
    // Helm records the exact prepared chart that the product installed.
    let values = success(
        Command::new("helm")
            .arg("--kubeconfig")
            .arg(&run.target.kubeconfig)
            .args([
                "--kube-context",
                &run.target.context,
                "get",
                "values",
                "wamn",
                "-n",
                "platform",
                "-o",
                "json",
            ]),
    )
    .await?;
    let values: Value = serde_json::from_slice(&values.stdout)?;
    run.chart = PathBuf::from(
        values["platform"]["chart"]
            .as_str()
            .context("installed platform chart")?,
    )
    .canonicalize()?;
    let selected = artifact_plan::select(&run.chart)?;
    report_images(&selected.values()["platform"]);
    let password = std::env::var("WAMN_TEST_OWNER_PASSWORD")
        .context("set WAMN_TEST_OWNER_PASSWORD to the disposable platform owner's password")?;
    ensure!(!password.is_empty(), "WAMN_TEST_OWNER_PASSWORD is empty");
    application_deploy::exercise(&run, &[case], &password).await?;
    cleanup(&mut run).await
}

async fn fresh(acceptance: bool) -> anyhow::Result<()> {
    let (repository, binary, kcl) = inputs()?;
    let chart = platform_images::source_chart(&repository, &repository)?;
    let selected = artifact_plan::select(&chart)?;
    eprintln!("Selected chart: {}", chart.display());
    eprintln!("Selected host: {}", selected.image("host")?);
    report_images(&selected.values()["platform"]);
    // The focused recovery case always interrupts a fresh platform installation.
    let installations: &[bool] = if acceptance { &[false, true] } else { &[true] };
    for &interrupted in installations {
        let mut run = prepare(&repository, &chart, &binary, &kcl, None)?;
        let password = exercise(&run, interrupted).await?;
        if acceptance && !interrupted {
            application_deploy::exercise(
                &run,
                &[
                    application_deploy::Case::Application,
                    application_deploy::Case::SqlJob,
                    application_deploy::Case::Reset,
                ],
                &password,
            )
            .await?;
        }
        cleanup(&mut run).await?;
    }
    Ok(())
}

async fn cleanup(run: &mut Run) -> anyhow::Result<()> {
    if run.owns_cluster {
        success(Command::new("kind").args([
            "delete",
            "cluster",
            "--name",
            &run.document.cluster_name,
        ]))
        .await?;
    }
    if run.checkout.exists() {
        success(
            Command::new("git")
                .args(["-c", "core.hooksPath=/dev/null"])
                .arg("-C")
                .arg(&run.repository)
                .args(["worktree", "remove", "--force"])
                .arg(&run.checkout),
        )
        .await?;
    }
    std::fs::remove_dir_all(&run.directory)?;
    run.cleaned = true;
    Ok(())
}

fn report_images(value: &Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "image" {
                    if let Some(image) = value.as_str().filter(|image| !image.is_empty()) {
                        eprintln!("Selected image: {image}");
                    }
                } else {
                    report_images(value);
                }
            }
        }
        Value::Array(entries) => entries.iter().for_each(report_images),
        _ => {}
    }
}

fn prepare(
    repository: &Path,
    chart: &Path,
    binary: &Path,
    kcl: &Path,
    document: Option<ClusterDocument>,
) -> anyhow::Result<Run> {
    let name = format!(
        "wamn-test-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..16]
    );
    let directory = std::env::temp_dir().join(&name);
    std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
    std::fs::create_dir(directory.join("environments"))?;
    std::fs::create_dir(directory.join("bin"))?;
    let owns_cluster = document.is_none();
    let document = if let Some(document) = document {
        document
    } else {
        let original = std::fs::read_to_string(repository.join("deploy/kind/cluster.k"))?;
        std::fs::write(
            directory.join("cluster.k"),
            original.replace(
                "cluster_name = \"wamn-local\"",
                &format!("cluster_name = \"{name}\""),
            ),
        )?;
        let document = ClusterDocument::compile(&directory.join("cluster.k"))?;
        ensure!(
            document.cluster_name == name,
            "the copied document must use the owned cluster name"
        );
        document
    };
    std::fs::write(directory.join("cluster.k"), document.to_kcl())?;
    // Only deployment tools are visible. Cloud, package and build executables are absent.
    let path = std::env::var_os("PATH").context("PATH is required")?;
    for tool in ["kind", "docker", "kubectl", "helm", "openssl"] {
        let executable = std::env::split_paths(&path)
            .map(|directory| directory.join(tool))
            .find(|file| file.is_file())
            .with_context(|| format!("missing prerequisite: {tool}"))?;
        symlink(executable.canonicalize()?, directory.join("bin").join(tool))?;
    }
    Ok(Run {
        repository: repository.into(),
        checkout: directory.join("checkout"),
        target: Target {
            kubeconfig: directory.join("kubeconfig"),
            context: format!("kind-{}", document.cluster_name),
            namespace: "platform".into(),
        },
        directory,
        document,
        binary: binary.into(),
        chart: chart.into(),
        kcl: kcl.into(),
        owns_cluster,
        cleaned: false,
    })
}

fn product(run: &Run, verb: &str) -> Command {
    let mut command = Command::new(&run.binary);
    command
        .current_dir(&run.directory)
        .env("PATH", run.directory.join("bin"))
        .env("WAMN_KCL", &run.kcl)
        .env("KUBECONFIG", &run.target.kubeconfig)
        .args(["platform", verb])
        .arg(run.directory.join("cluster.k"));
    if verb == "deploy" {
        command.arg("--chart").arg(&run.chart);
    }
    command
}

async fn captured(run: &Run, verb: &str, label: &str) -> anyhow::Result<Output> {
    let output = execute(&mut product(run, verb))
        .await
        .inspect_err(|error| {
            let _ = std::fs::write(
                run.directory.join(format!("{label}.error")),
                format!("{error:#}"),
            );
        })?;
    std::fs::write(
        run.directory.join(format!("{label}.stdout")),
        &output.stdout,
    )?;
    std::fs::write(
        run.directory.join(format!("{label}.stderr")),
        &output.stderr,
    )?;
    Ok(output)
}

async fn deploy(run: &Run, label: &str) -> anyhow::Result<()> {
    let started = Instant::now();
    let output = captured(run, "deploy", label).await?;
    eprintln!("{label}: {:.2}s", started.elapsed().as_secs_f64());
    ensure!(
        output.status.success(),
        "{label} failed; see {}",
        run.directory.display()
    );
    Ok(())
}

async fn ready(run: &Run, label: &str) -> anyhow::Result<()> {
    let output = captured(run, "status", label).await?;
    ensure!(
        output.status.success(),
        "independent status failed; see {}",
        run.directory.display()
    );
    Ok(())
}

fn kubectl(run: &Run) -> Command {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(&run.target.kubeconfig)
        .args(["--context", &run.target.context, "--request-timeout=10s"]);
    command
}

async fn execute(command: &mut Command) -> anyhow::Result<Output> {
    owned_command::execute(
        command.stdin(Stdio::null()),
        COMMAND_LIMIT,
        Duration::from_secs(5),
    )
    .await
}

async fn success(command: &mut Command) -> anyhow::Result<Output> {
    let output = execute(command).await?;
    ensure!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

async fn get(run: &Run, namespace: &str, kind: &str, name: &str) -> anyhow::Result<Value> {
    let mut command = kubectl(run);
    if !namespace.is_empty() {
        command.args(["-n", namespace]);
    }
    command.args(["get", kind]);
    if !name.is_empty() {
        command.arg(name);
    }
    let output = success(command.args(["-o", "json"])).await?;
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn exercise(run: &Run, interrupted: bool) -> anyhow::Result<String> {
    if interrupted {
        interrupt(run).await?;
    }
    deploy(run, if interrupted { "retry" } else { "fresh-deploy" }).await?;
    success(
        Command::new("kind")
            .args([
                "export",
                "kubeconfig",
                "--name",
                &run.document.cluster_name,
                "--kubeconfig",
            ])
            .arg(&run.target.kubeconfig),
    )
    .await?;
    ready(run, "initial-status").await?;
    let password = uuid::Uuid::new_v4().to_string();
    sign_in(run, &password, true).await?;
    let before = database_state(run).await?;
    if !interrupted {
        deploy(run, "immediate-reapply").await?;
        ready(run, "reapply-status").await?;
        ensure!(
            database_state(run).await? == before,
            "reapply changed baseline or stored data"
        );
        sign_in(run, &password, false).await?;
        recover_pod(run).await?;
        repair_release(run).await?;
        ready(run, "repair-status").await?;
        ensure!(
            database_state(run).await? == before,
            "repair changed baseline or stored data"
        );
        sign_in(run, &password, false).await?;
    }
    let maps = get(run, "", "configmaps", "--all-namespaces").await?;
    ensure!(
        !maps["items"]
            .as_array()
            .context("ConfigMaps list")?
            .iter()
            .any(|map| {
                map["metadata"]["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("wamn-install-"))
            }),
        "deployment created a generic completion ConfigMap"
    );
    Ok(password)
}

async fn database_state(run: &Run) -> anyhow::Result<Value> {
    let cluster = get(run, "platform", "cluster.postgresql.cnpg.io", "wamn-pg").await?;
    let primary = cluster["status"]["currentPrimary"]
        .as_str()
        .context("CNPG primary")?;
    // Read existing product data only. Password enrollment supplies the user mutation.
    let sql = "SELECT json_build_object(
        'baseline', (SELECT row_to_json(b) FROM registry.schema_baseline b),
        'orgs', (SELECT json_agg(json_build_array(id, created_at) ORDER BY id) FROM registry.orgs),
        'passwords', (SELECT json_agg(json_build_array(principal_id, created_at) ORDER BY principal_id) FROM identity.password_credentials),
        'environments', (SELECT count(*) FROM registry.project_envs))";
    let output = success(kubectl(run).args([
        "-n",
        "platform",
        "exec",
        primary,
        "-c",
        "postgres",
        "--",
        "psql",
        "-XAt",
        "-v",
        "ON_ERROR_STOP=1",
        "-U",
        "postgres",
        "-d",
        "wamn_system",
        "-c",
        sql,
    ]))
    .await?;
    let state: Value = serde_json::from_slice(&output.stdout)?;
    ensure!(state["baseline"].is_object(), "system baseline is missing");
    ensure!(
        state["passwords"].is_array(),
        "owner password data is missing"
    );
    ensure!(
        state["environments"] == 0,
        "platform created application environments"
    );
    // Read application membership from every installed Helm release.
    let releases = success(Command::new("helm").args([
        "--kubeconfig",
        &run.target.kubeconfig.to_string_lossy(),
        "--kube-context",
        &run.target.context,
        "list",
        "-n",
        "hosts",
        "-q",
    ]))
    .await?;
    for release in String::from_utf8(releases.stdout)?.split_whitespace() {
        let values = success(
            Command::new("helm")
                .arg("--kubeconfig")
                .arg(&run.target.kubeconfig)
                .args(["get", "values", release, "-n", "hosts", "-o", "json"]),
        )
        .await?;
        let values: Value = serde_json::from_slice(&values.stdout)?;
        ensure!(
            values["applications"]
                .as_object()
                .is_none_or(serde_json::Map::is_empty),
            "platform created application release {release}"
        );
    }
    Ok(state)
}

// Enrollment runs only for an owner without a password credential. Later focused
// runs receive the same password from the caller, not from a completion file.
async fn owner_session(
    run: &Run,
    client: &reqwest::Client,
    endpoint: &str,
    password: &str,
    enroll: bool,
) -> anyhow::Result<Value> {
    if enroll {
        let mail = success(kubectl(run).args([
            "-n",
            "identity",
            "exec",
            "deployment/identity",
            "-c",
            "identity",
            "--",
            "cat",
            "/var/run/wamn-identity/mail/outbox",
        ]))
        .await?;
        let records: Vec<Value> = String::from_utf8(mail.stdout)?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let code = records
            .iter()
            .filter(|record| record["to"] == run.document.owner_email)
            .filter_map(|record| record["text"].as_str())
            .flat_map(str::lines)
            .find_map(|line| line.strip_prefix("Invitation code: "))
            .context("owner invitation is absent from local mail")?;
        let (principal, invitation) = code.split_once(':').context("invitation code")?;
        let response = client
            .post(format!("{endpoint}/password/enroll"))
            .json(
                &json!({"principal_id": principal, "invitation": invitation, "password": password}),
            )
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "owner enrollment returned {}",
            response.status()
        );
    }
    let response = client.post(format!("{endpoint}/password/session"))
        .json(&json!({"email": run.document.owner_email, "password": password, "aud": format!("urn:wamn:control:{}", run.document.org)})).send().await?;
    ensure!(
        response.status().is_success(),
        "owner login returned {}",
        response.status()
    );
    Ok(response.json().await?)
}

async fn sign_in(run: &Run, password: &str, enroll: bool) -> anyhow::Result<()> {
    let forward = resources::port_forward(&run.target, "identity", "service/identity", 443).await?;
    let tls = get(run, "identity", "secret", "identity-tls").await?;
    let ca = base64::engine::general_purpose::STANDARD
        .decode(tls["data"]["ca.crt"].as_str().context("identity CA")?)?;
    let host = "identity.identity.svc.cluster.local";
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .add_root_certificate(reqwest::Certificate::from_pem(&ca)?)
        .resolve(host, SocketAddr::from((Ipv4Addr::LOCALHOST, forward.port)))
        .build()?;
    let endpoint = format!("https://{host}:{}", forward.port);
    ensure!(
        client
            .get(format!("{endpoint}/healthz"))
            .send()
            .await?
            .status()
            .is_success(),
        "identity health refused"
    );
    let login = owner_session(run, &client, &endpoint, password, enroll).await?;
    let token = login["access_token"]
        .as_str()
        .context("identity access token")?;
    let control = resources::port_forward(
        &run.target,
        "hosts",
        &format!("service/c-{}-http", run.document.org),
        80,
    )
    .await?;
    let url = format!(
        "http://127.0.0.1:{}/wamn_control/control/mine",
        control.port
    );
    let request = || {
        client
            .get(&url)
            .header(reqwest::header::HOST, &run.document.route_hosts.control)
    };
    ensure!(
        request().send().await?.status() == reqwest::StatusCode::UNAUTHORIZED,
        "control must refuse an unauthenticated query with 401"
    );
    let response = request().bearer_auth(token).send().await?;
    ensure!(
        response.status().is_success(),
        "authorized control query returned {}",
        response.status()
    );
    let mine: Value = response.json().await.context("control query response")?;
    // A host route answers one outcome per request element.
    ensure!(
        mine[0]["value"]["org_admin"] == true,
        "control did not recognize the signed-in owner: {mine}"
    );
    Ok(())
}

async fn recover_pod(run: &Run) -> anyhow::Result<()> {
    let pods = get(run, "cert-manager", "pods", "-l=app=webhook").await?;
    let pod = pods["items"]
        .as_array()
        .and_then(|pods| pods.first())
        .context("cert-manager webhook pod")?;
    let name = pod["metadata"]["name"]
        .as_str()
        .context("webhook pod name")?;
    let uid = &pod["metadata"]["uid"];
    success(kubectl(run).args(["-n", "cert-manager", "delete", "pod", name, "--wait=false"]))
        .await?;
    tokio::time::timeout(POLL_LIMIT, async {
        loop {
            let pods = get(run, "cert-manager", "pods", "-l=app=webhook").await?;
            if pods["items"]
                .as_array()
                .context("webhook pods")?
                .iter()
                .any(|pod| {
                    pod["metadata"]["uid"] != *uid
                        && pod["metadata"]["deletionTimestamp"].is_null()
                        && pod["status"]["conditions"]
                            .as_array()
                            .is_some_and(|conditions| {
                                conditions
                                    .iter()
                                    .any(|c| c["type"] == "Ready" && c["status"] == "True")
                            })
                })
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .context("controller did not replace the webhook pod")??;
    Ok(())
}

async fn repair_release(run: &Run) -> anyhow::Result<()> {
    let manifest = success(
        Command::new("helm")
            .arg("--kubeconfig")
            .arg(&run.target.kubeconfig)
            .args([
                "--kube-context",
                &run.target.context,
                "get",
                "manifest",
                "wamn",
                "-n",
                "platform",
            ]),
    )
    .await?;
    let objects = serde_yaml::Deserializer::from_slice(&manifest.stdout)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    let object = objects
        .iter()
        .find(|object| object["kind"] == "Service")
        .context("operator release has no Service to repair")?;
    let name = object["metadata"]["name"]
        .as_str()
        .context("Helm Service name")?;
    let namespace = object["metadata"]["namespace"]
        .as_str()
        .unwrap_or("platform");
    let before = get(run, namespace, "service", name).await?;
    success(kubectl(run).args(["-n", namespace, "delete", "service", name])).await?;
    let missing = success(kubectl(run).args([
        "-n",
        namespace,
        "get",
        "service",
        name,
        "--ignore-not-found",
        "-o",
        "name",
    ]))
    .await?;
    ensure!(
        missing.stdout.is_empty(),
        "Helm-owned Service still exists after deletion"
    );
    deploy(run, "repair-deploy").await?;
    let after = get(run, namespace, "service", name).await?;
    ensure!(
        before["metadata"]["uid"] != after["metadata"]["uid"],
        "deploy did not restore the deleted Helm resource"
    );
    Ok(())
}

struct ProcessGroup(Pid);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = kill_process_group(self.0, Signal::KILL);
    }
}

async fn interrupt(run: &Run) -> anyhow::Result<()> {
    let mut command = product(run, "deploy");
    command
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(
            run.directory.join("interrupted.stdout"),
        )?)
        .stderr(std::fs::File::create(
            run.directory.join("interrupted.stderr"),
        )?);
    let mut child = command.spawn()?;
    let group = ProcessGroup(
        Pid::from_raw(i32::try_from(child.id().context("deploy PID")?)?)
            .context("positive deploy PID")?,
    );
    tokio::time::timeout(POLL_LIMIT, async {
        loop {
            ensure!(
                child.try_wait()?.is_none(),
                "fresh deploy exited before interruption"
            );
            let config = execute(Command::new("kind").args([
                "get",
                "kubeconfig",
                "--name",
                &run.document.cluster_name,
            ]))
            .await?;
            if config.status.success() {
                std::fs::write(&run.target.kubeconfig, config.stdout)?;
                let namespace =
                    execute(kubectl(run).args(["get", "namespace", "platform", "-o", "name"]))
                        .await?;
                if namespace.status.success() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("no first Kubernetes write within five minutes")??;
    kill_process_group(group.0, Signal::KILL)?;
    ensure!(
        !tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await??
            .success(),
        "interrupted deploy succeeded"
    );
    drop(group);
    let output = captured(run, "status", "partial-status").await?;
    let report = String::from_utf8(output.stdout)?;
    ensure!(
        !output.status.success()
            && report
                .lines()
                .any(|line| matches!(line, "platform: incomplete" | "platform: failed")),
        "interrupted deployment did not report incomplete or failed; see {}",
        run.directory.display()
    );
    Ok(())
}
