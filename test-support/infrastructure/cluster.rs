//! Disposable cluster ownership, candidate image transport, and bounded teardown.

use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use async_trait::async_trait;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

enum Input {
    Bytes(Vec<u8>),
    File(PathBuf),
}

#[async_trait]
trait Runner: Send + Sync {
    async fn output(
        &self,
        command: &mut Command,
        input: Option<Input>,
        bound: Duration,
    ) -> anyhow::Result<Output>;
    async fn pause(&self, duration: Duration);
}

struct ProcessRunner;

#[async_trait]
impl Runner for ProcessRunner {
    async fn output(
        &self,
        command: &mut Command,
        input: Option<Input>,
        bound: Duration,
    ) -> anyhow::Result<Output> {
        command
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let bytes = match input {
            Some(Input::Bytes(bytes)) => {
                command.stdin(Stdio::piped());
                Some(bytes)
            }
            Some(Input::File(path)) => {
                command.stdin(fs::File::open(path)?);
                None
            }
            None => {
                command.stdin(Stdio::null());
                None
            }
        };
        let mut child = command.spawn().context("start fixture command")?;
        let output = async {
            if let Some(input) = bytes {
                let mut stdin = child.stdin.take().context("open fixture command input")?;
                stdin.write_all(&input).await?;
            }
            child
                .wait_with_output()
                .await
                .context("read fixture command output")
        };
        tokio::time::timeout(bound, output)
            .await
            .context("fixture command exceeded its deadline")?
    }
    async fn pause(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

async fn checked<R: Runner + ?Sized>(
    runner: &R,
    command: &mut Command,
    input: Option<Input>,
    bound: Duration,
) -> anyhow::Result<Vec<u8>> {
    let output = runner.output(command, input, bound).await?;
    // Arguments carry no private bytes: those enter on standard input.
    // Standard output can carry them and is not printed.
    ensure!(
        output.status.success(),
        "fixture command {} {:?} failed with {}: {}",
        command.as_std().get_program().display(),
        command.as_std().get_args().collect::<Vec<_>>(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

/// Run a fixture command without exposing private output in errors.
pub async fn run(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    checked(&ProcessRunner, command, None, Duration::from_secs(300)).await
}

/// Supply private bytes on standard input, never in command arguments.
pub async fn input(command: &mut Command, bytes: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    checked(
        &ProcessRunner,
        command,
        Some(Input::Bytes(bytes)),
        Duration::from_secs(300),
    )
    .await
}

/// Resources observed after bounded deletion. Leftovers alone do not fail a case.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Teardown {
    pub leftovers: Vec<String>,
    pub failures: Vec<String>,
}

impl Teardown {
    pub fn result(&self) -> anyhow::Result<()> {
        ensure!(
            self.failures.is_empty(),
            "cluster cleanup failed: {:?}",
            self.failures
        );
        Ok(())
    }
}

/// One cluster owns its kubeconfig and disposes only resources with its name.
pub struct Cluster {
    pub name: String,
    pub work: PathBuf,
    owned: bool,
    runner: Arc<dyn Runner>,
}

fn refuse(name: &str) -> anyhow::Result<()> {
    ensure!(
        !name.is_empty() && name != "wamn",
        "refusing kind cluster name: {name}"
    );
    ensure!(
        name.len() <= 63
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid disposable cluster name"
    );
    Ok(())
}

impl std::fmt::Debug for Cluster {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Cluster")
            .field("name", &self.name)
            .field("work", &self.work)
            .field("owned", &self.owned)
            .finish_non_exhaustive()
    }
}

impl Cluster {
    async fn run(&self, command: &mut Command) -> anyhow::Result<Vec<u8>> {
        checked(
            self.runner.as_ref(),
            command,
            None,
            Duration::from_secs(300),
        )
        .await
    }

    /// Describe a cluster before creation. An attached description owns no cleanup.
    pub fn new(name: &str, work: &Path) -> anyhow::Result<Self> {
        refuse(name)?;
        Ok(Self {
            name: name.to_owned(),
            work: work.to_owned(),
            owned: false,
            runner: Arc::new(ProcessRunner),
        })
    }

    pub fn owned(&self) -> bool {
        self.owned
    }

    /// Refuse existing resources before claiming this name.
    pub async fn preflight(&self) -> anyhow::Result<()> {
        refuse(&self.name)?;
        checked(
            self.runner.as_ref(),
            &mut docker_version(),
            None,
            Duration::from_secs(5),
        )
        .await?;
        for mut command in [clusters(), containers(), images()] {
            let bytes = checked(
                self.runner.as_ref(),
                &mut command,
                None,
                Duration::from_secs(5),
            )
            .await?;
            ensure!(
                !String::from_utf8_lossy(&bytes).contains(&self.name),
                "the disposable resource name already exists: {}",
                self.name
            );
        }
        Ok(())
    }

    /// Render the shared kind configuration and create a fresh cluster.
    pub async fn create(&mut self, repository: &Path) -> anyhow::Result<()> {
        self.preflight().await?;
        let template = fs::read_to_string(repository.join("deploy/infra/kind-config.yaml"))?;
        fs::write(
            self.work.join("kind.yaml"),
            crate::rendering::render_kind_cluster(&template)?,
        )?;
        self.owned = true;
        self.run(
            Command::new("kind")
                .args(["create", "cluster", "--name", &self.name, "--config"])
                .arg(self.work.join("kind.yaml"))
                .arg("--kubeconfig")
                .arg(self.work.join("kubeconfig")),
        )
        .await?;
        Ok(())
    }

    /// Import a local tag whose bytes the delivery owner selects by digest.
    pub async fn load_local_image(&self, image: &str) -> anyhow::Result<()> {
        refuse(&self.name)?;
        self.run(Command::new("kind").args(["load", "docker-image", image, "--name", &self.name]))
            .await
            .map(drop)
    }

    /// Export an existing kept cluster's kubeconfig without claiming its lifecycle.
    pub async fn attach(&self) -> anyhow::Result<()> {
        self.run(
            Command::new("kind")
                .args(["export", "kubeconfig", "--name", &self.name, "--kubeconfig"])
                .arg(self.work.join("kubeconfig")),
        )
        .await
        .map(drop)
    }

    /// Transfer explicit ownership to the caller that retains a delivery cluster.
    pub fn keep(&mut self) {
        self.owned = false;
    }

    /// Import digest-pinned candidate bytes through archives or the loopback registry.
    pub async fn import_images(&self, images: &[&str]) -> anyhow::Result<()> {
        refuse(&self.name)?;
        import_images(self.runner.as_ref(), &self.name, &self.work, images).await
    }

    /// Install the platform objects rendered by the production owner.
    pub async fn install(
        &self,
        repository: &Path,
        namespace: &str,
        prerequisites: &[serde_json::Value],
        nats: &[serde_json::Value],
        operator: &serde_json::Value,
    ) -> anyhow::Result<()> {
        refuse(&self.name)?;
        crate::platform::install(
            repository,
            &self.name,
            &self.work,
            namespace,
            prerequisites,
            nats,
            operator,
        )
        .await
    }

    /// Capture resources, events and container logs without reading Secret objects.
    pub async fn inspect(&self, namespace: &str, evidence: &Path) {
        if let Ok(files) = fs::read_dir(&self.work) {
            for entry in files.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("failure-") || name == "node-image-waits.log" {
                    let _ = fs::copy(entry.path(), evidence.join(name.as_ref()));
                }
            }
        }
        if !self.owned || !self.work.join("kubeconfig").is_file() {
            return;
        }
        for (file, args) in [
            (
                "final-resources.json",
                vec!["get", "jobs,pods,deployments,services", "-o", "json"],
            ),
            ("final-events.json", vec!["get", "events", "-o", "json"]),
        ] {
            if let Ok(bytes) = run(self.kubectl().args(["-n", namespace]).args(args)).await {
                let _ = fs::write(evidence.join(file), &bytes);
                if file == "final-resources.json" {
                    let value: serde_json::Value =
                        serde_json::from_slice(&bytes).unwrap_or_default();
                    for item in value["items"].as_array().into_iter().flatten() {
                        if item["kind"] != "Pod" {
                            continue;
                        }
                        if let Some(name) = item["metadata"]["name"].as_str()
                            && let Ok(logs) = run(self.kubectl().args([
                                "-n",
                                namespace,
                                "logs",
                                name,
                                "--all-containers=true",
                            ]))
                            .await
                        {
                            let _ = fs::write(evidence.join(format!("final-{name}.log")), logs);
                        }
                    }
                }
            }
        }
    }

    pub fn kubectl(&self) -> Command {
        kubectl(&self.name, &self.work)
    }

    /// Remove the cluster and every owned sidecar, even after partial creation.
    pub async fn cleanup(&mut self) -> Teardown {
        if !self.owned {
            return Teardown::default();
        }
        let result = cleanup(self.runner.as_ref(), &self.name, &self.work).await;
        if result.failures.is_empty() {
            self.owned = false;
        }
        result
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let result = self.cleanup_blocking();
        if !result.failures.is_empty() {
            eprintln!(
                "cluster {} cleanup failed: {:?}",
                self.name, result.failures
            );
        }
    }
}

async fn import_images<R: Runner + ?Sized>(
    runner: &R,
    name: &str,
    work: &Path,
    images: &[&str],
) -> anyhow::Result<()> {
    let bound = Duration::from_secs(180);
    let nodes = checked(runner, &mut nodes(name), None, bound).await?;
    let nodes = std::str::from_utf8(&nodes)?.lines().collect::<Vec<_>>();
    ensure!(!nodes.is_empty(), "kind reported no nodes");
    let authority = work.join("native-registry-authority");
    let authority = if authority.exists() {
        Some(fs::read_to_string(authority)?)
    } else {
        None
    };
    for image in images {
        let (_, digest) = image
            .rsplit_once("@sha256:")
            .context("candidate image must be pinned by sha256 digest")?;
        let archive = work.join(format!("node-image-{digest}.tar"));
        if authority.is_none() {
            for node in &nodes {
                checked(
                    runner,
                    Command::new("docker").args([
                        "exec",
                        "-i",
                        node,
                        "ctr",
                        "--namespace",
                        "k8s.io",
                        "images",
                        "import",
                        "--platform",
                        "linux/amd64",
                        "-",
                    ]),
                    Some(Input::File(archive.clone())),
                    bound,
                )
                .await?;
            }
            fs::remove_file(archive)?;
        }
    }
    for node in nodes {
        if let Some(authority) = &authority {
            let directory = format!("/etc/containerd/certs.d/{}", authority.trim());
            checked(
                runner,
                Command::new("docker").args(["exec", node, "mkdir", "-p", &directory]),
                None,
                bound,
            )
            .await?;
            checked(
                runner,
                Command::new("docker")
                    .arg("cp")
                    .arg(work.join("native-registry-hosts.toml"))
                    .arg(format!("{node}:{directory}/hosts.toml")),
                None,
                bound,
            )
            .await?;
        }
        for image in images {
            if authority.is_some() {
                let pull = digest_reference(image)?;
                checked(
                    runner,
                    Command::new("docker").args([
                        "exec",
                        node,
                        "ctr",
                        "--namespace",
                        "k8s.io",
                        "images",
                        "pull",
                        "--hosts-dir",
                        "/etc/containerd/certs.d",
                        "--platform",
                        "linux/amd64",
                        &pull,
                    ]),
                    None,
                    bound,
                )
                .await?;
            }
            for attempt in 1..=10 {
                let inspected = runner
                    .output(&mut node_image(node, image), None, Duration::from_secs(5))
                    .await;
                if inspected.as_ref().is_ok_and(|o| o.status.success()) {
                    break;
                }
                if attempt == 10 {
                    for (suffix, mut command) in [
                        ("cri-images.json", cri_images(node)),
                        ("containerd-images.txt", containerd_images(node)),
                    ] {
                        if let Ok(output) = runner
                            .output(&mut command, None, Duration::from_secs(5))
                            .await
                        {
                            fs::write(
                                work.join(format!("failure-{node}-{suffix}")),
                                [output.stdout, output.stderr].concat(),
                            )?;
                        }
                    }
                    anyhow::bail!("CRI shows no candidate image {image} on {node}");
                }
                writeln!(
                    fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(work.join("node-image-waits.log"))?,
                    "CRI shows no {image} on {node} after {attempt} s"
                )?;
                runner.pause(Duration::from_secs(1)).await;
            }
        }
    }
    Ok(())
}

/// CRI resolves a pinned reference without its optional tag.
pub fn digest_reference(image: &str) -> anyhow::Result<String> {
    let (repository, digest) = image
        .rsplit_once('@')
        .context("image is pinned by digest")?;
    let repository = if repository
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains(':'))
    {
        repository.rsplit_once(':').unwrap().0
    } else {
        repository
    };
    Ok(format!("{repository}@{digest}"))
}

async fn delete<R: Runner + ?Sized>(runner: &R, name: &str, work: &Path) -> Teardown {
    let mut result = Teardown::default();
    if let Err(error) = refuse(name) {
        result.failures.push(error.to_string());
        return result;
    }
    for attempt in 1..=3 {
        let output = runner
            .output(
                Command::new("kind")
                    .args(["delete", "cluster", "--name", name, "--kubeconfig"])
                    .arg(work.join("kubeconfig")),
                None,
                Duration::from_secs(120),
            )
            .await;
        if output.is_ok_and(|o| o.status.success()) {
            break;
        }
        if attempt < 3 {
            runner.pause(Duration::from_secs(attempt * 2)).await;
        }
    }
    for (label, mut command) in [
        ("clusters", clusters()),
        ("containers", kind_containers(name)),
    ] {
        match runner
            .output(&mut command, None, Duration::from_secs(5))
            .await
        {
            Ok(output) if output.status.success() => {
                for line in String::from_utf8_lossy(&output.stdout).lines() {
                    if !line.is_empty() && (label == "containers" || line == name) {
                        result.leftovers.push(line.to_owned());
                    }
                }
            }
            _ => result.failures.push(format!("cannot inspect kind {label}")),
        }
    }
    result
}

async fn cleanup<R: Runner + ?Sized>(runner: &R, name: &str, work: &Path) -> Teardown {
    let mut result = delete(runner, name, work).await;
    if refuse(name).is_err() {
        return result;
    }
    let bound = Duration::from_secs(5);
    match checked(runner, &mut containers(), None, bound).await {
        Ok(bytes) => {
            for line in String::from_utf8_lossy(&bytes).lines() {
                let container = line.trim_matches('"');
                if container.starts_with(&format!("{name}-"))
                    && !result
                        .leftovers
                        .iter()
                        .any(|leftover| leftover == container)
                    && checked(
                        runner,
                        Command::new("docker").args(["rm", "--force", "--volumes", container]),
                        None,
                        Duration::from_secs(120),
                    )
                    .await
                    .is_err()
                {
                    result
                        .failures
                        .push(format!("remove container {container}"));
                }
            }
        }
        Err(_) => result.failures.push("cannot inspect owned sidecars".into()),
    }
    let volume = format!("{name}-registry-data");
    if runner
        .output(
            Command::new("docker").args(["volume", "inspect", &volume]),
            None,
            bound,
        )
        .await
        .is_ok_and(|o| o.status.success())
        && checked(
            runner,
            Command::new("docker").args(["volume", "rm", &volume]),
            None,
            bound,
        )
        .await
        .is_err()
    {
        result.failures.push(format!("remove volume {volume}"));
    }
    match checked(runner, &mut images(), None, bound).await {
        Ok(bytes) => {
            for line in String::from_utf8_lossy(&bytes).lines() {
                let Some((_, tag)) = line.rsplit_once(':') else {
                    continue;
                };
                if tag.starts_with(&format!("{name}-"))
                    && checked(
                        runner,
                        Command::new("docker").args(["image", "rm", line]),
                        None,
                        bound,
                    )
                    .await
                    .is_err()
                {
                    result.failures.push(format!("remove image {line}"));
                }
            }
        }
        Err(_) => result.failures.push("cannot inspect owned images".into()),
    }
    if let Err(error) = fs::write(
        work.join("kind-teardown-leftovers"),
        result.leftovers.join("\n"),
    ) {
        result.failures.push(error.to_string());
    }
    result
}

/// Remove a kept cluster after all of its cases finish.
pub async fn remove_kept(name: &str) -> anyhow::Result<Teardown> {
    let suffix = name
        .strip_prefix("wamn-delivery-")
        .context("a kept delivery cluster name is required")?;
    ensure!(
        (1..=24).contains(&suffix.len())
            && suffix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "invalid kept cluster name"
    );
    let work = std::env::temp_dir().join(name);
    fs::create_dir_all(&work)?;
    let result = cleanup(&ProcessRunner, name, &work).await;
    fs::remove_dir_all(&work).context("remove private delivery files")?;
    result.result()?;
    Ok(result)
}

pub fn kubectl(name: &str, work: &Path) -> Command {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(work.join("kubeconfig"))
        .args(["--context", &format!("kind-{name}")]);
    command
}
fn docker(args: &[&str]) -> Command {
    let mut command = Command::new("docker");
    command.args(args);
    command
}
pub fn docker_version() -> Command {
    docker(&["info", "--format", "{{json .ServerVersion}}"])
}
pub fn clusters() -> Command {
    let mut command = Command::new("kind");
    command.args(["get", "clusters"]);
    command
}
pub fn containers() -> Command {
    docker(&["container", "ls", "--all", "--format", "{{.Names}}"])
}
pub fn images() -> Command {
    docker(&["image", "ls", "--format", "{{.Repository}}:{{.Tag}}"])
}
fn kind_containers(name: &str) -> Command {
    docker(&[
        "ps",
        "-a",
        "--filter",
        &format!("label=io.x-k8s.kind.cluster={name}"),
        "--format",
        "{{.Names}}",
    ])
}
pub fn nodes(name: &str) -> Command {
    let mut command = Command::new("kind");
    command.args(["get", "nodes", "--name", name]);
    command
}
pub fn inspect(container: &str) -> Command {
    docker(&[
        "container",
        "inspect",
        "--format",
        "{{json .NetworkSettings}}",
        container,
    ])
}
/// Read the configuration of the candidate bytes that a node actually imported.
pub async fn image_config(name: &str, image: &str) -> anyhow::Result<serde_json::Value> {
    let names = run(&mut nodes(name)).await?;
    let node = std::str::from_utf8(&names)?
        .lines()
        .next()
        .context("kind reported no node")?;
    let observed: serde_json::Value =
        serde_json::from_slice(&run(&mut node_image(node, image)).await?)?;
    observed
        .pointer("/info/imageSpec")
        .cloned()
        .context("CRI returned no image configuration")
}

pub fn node_image(node: &str, image: &str) -> Command {
    docker(&["exec", node, "crictl", "inspecti", "-o", "json", image])
}
pub fn cri_images(node: &str) -> Command {
    docker(&["exec", node, "crictl", "images", "-o", "json"])
}
pub fn containerd_images(node: &str) -> Command {
    docker(&["exec", node, "ctr", "--namespace", "k8s.io", "images", "ls"])
}
pub fn container_logs(name: &str) -> Command {
    docker(&["logs", name])
}

/// Hash a registry password through private standard input.
pub async fn registry_password(username: &str, password: &str) -> anyhow::Result<Vec<u8>> {
    input(
        Command::new("docker").args([
            "run",
            "--rm",
            "-i",
            "--entrypoint",
            "htpasswd",
            "httpd:2-alpine",
            "-Bni",
            username,
        ]),
        format!("{password}\n").into_bytes(),
    )
    .await
}

/// Start an owned sidecar with application-selected arguments.
pub async fn sidecar(
    name: &str,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> anyhow::Result<()> {
    run(Command::new("docker")
        .args(["run", "--detach", "--name", name, "--network", "kind"])
        .args(args))
    .await
    .map(drop)
}

impl Cluster {
    /// Dispose during an application's synchronous Drop handler.
    pub fn cleanup_blocking(&mut self) -> Teardown {
        if !self.owned {
            return Teardown::default();
        }
        self.owned = false;
        let name = self.name.clone();
        let work = self.work.clone();
        let runner = Arc::clone(&self.runner);
        let result = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("cleanup runtime");
            runtime.block_on(cleanup(runner.as_ref(), &name, &work))
        })
        .join()
        .expect("cleanup thread");
        if !result.leftovers.is_empty() {
            eprintln!(
                "Leftover kind resources for {}: {:?}",
                self.name, result.leftovers
            );
        }
        result
    }

    /// Start the temporary PostgreSQL server used by application assertions.
    pub async fn start_postgres(&self, repository: &Path) -> anyhow::Result<()> {
        fs::copy(
            repository.join("deploy/sql/postgres-init.sql"),
            self.work.join("postgres-init.sql"),
        )?;
        let mount = format!(
            "{}:/docker-entrypoint-initdb.d/postgres-init.sql:ro",
            self.work.join("postgres-init.sql").display()
        );
        sidecar(
            &format!("{}-postgres", self.name),
            [
                "--env",
                "POSTGRES_PASSWORD=probe",
                "--publish",
                "127.0.0.1::5432",
                "--volume",
                &mount,
                "postgres:18",
                "-c",
                "wal_level=logical",
                "-c",
                "max_replication_slots=10",
                "-c",
                "max_wal_senders=10",
            ],
        )
        .await
    }

    /// Start the private registry using the caller's protected password file.
    pub async fn start_registry(&self) -> anyhow::Result<()> {
        let mount = format!(
            "{}:/auth/htpasswd:ro",
            self.work.join("registry/htpasswd").display()
        );
        sidecar(
            &format!("{}-registry", self.name),
            [
                "--env",
                "REGISTRY_AUTH=htpasswd",
                "--env",
                "REGISTRY_AUTH_HTPASSWD_REALM=WAMN cluster fixture",
                "--env",
                "REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd",
                "--volume",
                &mount,
                "registry:2",
            ],
        )
        .await
    }
}

/// Read a bounded command result, including unsuccessful status for diagnostics.
pub async fn output(command: &mut Command) -> anyhow::Result<Output> {
    ProcessRunner
        .output(command, None, Duration::from_secs(30))
        .await
}

/// Remove resources after a killed test without requiring its private directory.
pub async fn remove_owned(name: &str) -> anyhow::Result<Teardown> {
    refuse(name)?;
    if name.starts_with("wamn-delivery-") {
        return remove_kept(name).await;
    }
    ensure!(
        [
            "wamn-receiving-",
            "wamn-wms-",
            "wamn-identity-jwks-",
            "wamn-rc"
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix)),
        "an owned test cluster name is required"
    );
    let work = std::env::temp_dir().join(name);
    fs::create_dir_all(&work)?;
    let result = cleanup(&ProcessRunner, name, &work).await;
    fs::remove_dir_all(work)?;
    Ok(result)
}

impl Cluster {
    /// Refuse surviving owned resources except those already reported by teardown.
    pub async fn absent(&self, leftovers: &[String]) -> anyhow::Result<()> {
        for mut command in [clusters(), containers()] {
            let bytes = run(&mut command).await?;
            for line in String::from_utf8_lossy(&bytes).lines() {
                ensure!(
                    !(line == self.name || line.starts_with(&format!("{}-", self.name)))
                        || leftovers.iter().any(|leftover| leftover == line),
                    "owned cluster resource remains after cleanup: {line}"
                );
            }
        }
        Ok(())
    }
}

/// Read a command result within the caller's declared deadline.
pub async fn output_with_timeout(command: &mut Command, bound: Duration) -> anyhow::Result<Output> {
    ProcessRunner.output(command, None, bound).await
}

/// Start a long-lived fixture child under its caller's shutdown guard.
pub fn spawn(command: &mut Command) -> anyhow::Result<tokio::process::Child> {
    command
        .kill_on_drop(true)
        .spawn()
        .context("start fixture process")
}

#[cfg(test)]
mod tests;
