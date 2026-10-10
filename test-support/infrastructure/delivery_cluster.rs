//! The kept delivery cluster (docs/plan/platform-deploy.md §17.2: the cluster
//! stage of Epic 6).
//!
//! The Receiving and WMS delivery cases run on one kind cluster that
//! `WAMN_DELIVERY_CLUSTER` names. The first case creates it and a later case
//! attaches, so the second case sees two applications in one namespace (A12).
//! Nothing here removes it: `tools/delivery-owned remove-kept <name>` does.
//!
//! The platform is the one of `crates/control/lib/tests/environment_apply_kind.rs`:
//! cert-manager, CloudNativePG, the operator release `wamn` in `wamn-system`,
//! and a registry with TLS from a private CA and a password. `env apply` and
//! the release chart's hosts read a registry over TLS only, because the chart
//! takes no `--allow-insecure-registries`. The application fixtures push over
//! plain HTTP, so a second registry container serves the same storage over
//! HTTP with the same password.
//!
//! Each application has its own CloudNativePG `Cluster` in its own namespace,
//! because each fixture creates its own `wamn_system`. A proxy container on the
//! kind network forwards port 5432 to it, so this machine and the pods reach
//! the database at one address, as the fixtures expect.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

/// The operator namespace, which every environment's release chart shares (R6).
pub const NAMESPACE: &str = "wamn-system";
/// The Secret every host mounts for the registry: `.dockerconfigjson` and `ca.crt`.
pub const REGISTRY_SECRET: &str = "delivery-registry";
const OPERATOR_CHART: &str = "oci://ghcr.io/wasmcloud/charts/runtime-operator";
/// The pin of tests/conformance/tests/chart_seam_governance.rs.
const OPERATOR_VERSION: &str = "2.10.3";
const REGISTRY_USER: &str = "delivery";
/// socat on Alpine, the database proxy. Unpinned, as the journeys' service
/// images are.
const PROXY_IMAGE: &str = "alpine/socat";

/// The kept cluster and its registry.
#[derive(Debug, Clone)]
pub struct KeptCluster {
    pub name: String,
    /// The private directory of the kept cluster: `kubeconfig`, the registry
    /// CA, the registry password and each application's own directory.
    pub work: PathBuf,
    /// The TLS registry authority `<address>:5000`, which `env apply` and the
    /// hosts read.
    pub registry: String,
    /// The plain HTTP authority of the same storage, which the fixtures push to.
    pub http_registry: String,
    /// Credentials for both authorities.
    pub registry_auth: PathBuf,
    /// The registry's CA certificate.
    pub ca: PathBuf,
}

impl KeptCluster {
    pub fn kubeconfig(&self) -> PathBuf {
        self.work.join("kubeconfig")
    }

    pub fn context(&self) -> String {
        format!("kind-{}", self.name)
    }

    pub fn kubectl(&self) -> Command {
        let mut command = Command::new("kubectl");
        command
            .arg("--kubeconfig")
            .arg(self.kubeconfig())
            .arg("--context")
            .arg(self.context());
        command
    }

    /// Apply one object, given on standard input, so a Secret is not written
    /// to a file.
    pub async fn apply(&self, object: &Value) -> anyhow::Result<()> {
        let mut child = self
            .kubectl()
            .args(["apply", "-f", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("start kubectl apply")?;
        let mut input = child.stdin.take().context("open the kubectl input")?;
        let written = input.write_all(&serde_json::to_vec(object)?).await;
        drop(input);
        let output = child.wait_with_output().await?;
        written.context("write the kubectl input")?;
        ensure!(
            output.status.success(),
            "kubectl apply of {} {} failed: {}",
            object["kind"],
            object["metadata"]["name"],
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    /// Load a local image into every node of the kept cluster.
    pub async fn load_image(&self, image: &str) -> anyhow::Result<()> {
        checked(Command::new("kind").args(["load", "docker-image", image, "--name", &self.name]))
            .await
            .map(drop)
    }

    /// The kind network address of the cluster's control-plane node.
    pub async fn node_address(&self) -> anyhow::Result<String> {
        container_address(&format!("{}-control-plane", self.name)).await
    }

    /// Copy a ConfigMap from `namespace` into the operator namespace as `name`.
    pub async fn copy_config_map(
        &self,
        namespace: &str,
        source: &str,
        name: &str,
    ) -> anyhow::Result<()> {
        let map: Value = serde_json::from_slice(
            &checked(self.kubectl().args([
                "-n",
                namespace,
                "get",
                "configmap",
                source,
                "-o",
                "json",
            ]))
            .await?,
        )?;
        self.apply(&json!({"apiVersion":"v1","kind":"ConfigMap",
            "metadata":{"name":name,"namespace":NAMESPACE},"data":map["data"]}))
            .await
    }
}

/// The name in `WAMN_DELIVERY_CLUSTER`: `wamn-delivery-<suffix>`, where the
/// suffix is one to 24 lowercase letters and digits.
pub fn name_from_env() -> anyhow::Result<String> {
    let name = std::env::var("WAMN_DELIVERY_CLUSTER")
        .context("WAMN_DELIVERY_CLUSTER names the kept delivery cluster, wamn-delivery-<suffix>")?;
    let suffix = name
        .strip_prefix("wamn-delivery-")
        .context("the kept delivery cluster is named wamn-delivery-<suffix>")?;
    ensure!(
        (1..=24).contains(&suffix.len())
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
        "the kept delivery cluster suffix is one to 24 lowercase letters and digits"
    );
    Ok(name)
}

/// Create the kept cluster and its platform, or attach to them. Every step is
/// an ensure, so a second case changes nothing the first made.
pub async fn ensure(repository: &Path, name: &str) -> anyhow::Result<KeptCluster> {
    ensure!(name != "wamn", "the kind-wamn cluster is frozen");
    let work = std::env::temp_dir().join(name);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&work)
        .with_context(|| format!("create {}", work.display()))?;
    let clusters =
        String::from_utf8(checked(Command::new("kind").args(["get", "clusters"])).await?)?;
    let kubeconfig = work.join("kubeconfig");
    if clusters.lines().any(|line| line == name) {
        checked(
            Command::new("kind")
                .args(["export", "kubeconfig", "--name", name, "--kubeconfig"])
                .arg(&kubeconfig),
        )
        .await?;
    } else {
        checked(
            Command::new("kind")
                .args(["create", "cluster", "--name", name, "--kubeconfig"])
                .arg(&kubeconfig),
        )
        .await?;
    }
    let mut kept = KeptCluster {
        name: name.to_owned(),
        work: work.clone(),
        registry: String::new(),
        http_registry: String::new(),
        registry_auth: work.join("registry-auth.json"),
        ca: work.join("registry/ca.crt"),
    };

    // cert-manager and CloudNativePG, then the operator release.
    for manifest in [
        "deploy/infra/cert-manager.yaml",
        "deploy/infra/cnpg-operator.yaml",
    ] {
        checked(
            kept.kubectl()
                .args(["apply", "--server-side", "-f"])
                .arg(repository.join(manifest)),
        )
        .await?;
    }
    for namespace in ["cert-manager", "cnpg-system"] {
        checked(kept.kubectl().args([
            "-n",
            namespace,
            "wait",
            "--for=condition=Available",
            "deployment",
            "--all",
            "--timeout=300s",
        ]))
        .await?;
    }
    let operator = Command::new("helm")
        .args(["status", "wamn", "-n", NAMESPACE, "--kubeconfig"])
        .arg(&kubeconfig)
        .args(["--kube-context", &kept.context()])
        .kill_on_drop(true)
        .output()
        .await
        .context("start helm status")?;
    if !operator.status.success() {
        checked(
            Command::new("helm")
                .args([
                    "upgrade",
                    "--install",
                    "wamn",
                    OPERATOR_CHART,
                    "--version",
                    OPERATOR_VERSION,
                    "-n",
                    NAMESPACE,
                    "--create-namespace",
                    "--kubeconfig",
                ])
                .arg(&kubeconfig)
                .args(["--kube-context", &kept.context(), "-f"])
                .arg(repository.join("deploy/infra/values-wamn.yaml"))
                .args(["--wait", "--timeout", "5m"]),
        )
        .await?;
    }

    // The registry, its HTTP twin, and the Secret every host mounts.
    let tls = format!("{name}-registry");
    let http = format!("{name}-registry-http");
    if !container_exists(&tls).await? {
        create_registry(&kept, &tls, &http).await?;
    }
    kept.registry = format!("{}:5000", container_address(&tls).await?);
    kept.http_registry = format!("{}:5000", container_address(&http).await?);
    let password = fs::read_to_string(work.join("registry/password"))
        .context("read the kept registry password")?;
    let credentials = json!({"username": REGISTRY_USER, "password": password.trim()});
    let auth = json!({"auths": {
        kept.registry.as_str(): credentials,
        kept.http_registry.as_str(): credentials,
    }});
    write_private(&kept.registry_auth, &serde_json::to_vec(&auth)?)?;
    kept.apply(&json!({"apiVersion":"v1","kind":"Secret",
    "metadata":{"name":REGISTRY_SECRET,"namespace":NAMESPACE},"type":"Opaque",
    "stringData":{
        ".dockerconfigjson": json!({"auths": {kept.registry.as_str(): credentials}}).to_string(),
        "ca.crt": fs::read_to_string(&kept.ca)?,
    }}))
    .await?;
    Ok(kept)
}

/// Start the TLS registry and its HTTP twin on one storage volume, with a
/// private CA whose certificate names the TLS registry's address.
async fn create_registry(kept: &KeptCluster, tls: &str, http: &str) -> anyhow::Result<()> {
    let directory = kept.work.join("registry");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let path = |name: &str| directory.join(name).to_string_lossy().into_owned();
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("generate the registry password"))?;
    let password = hex::encode(bytes);
    write_private(&directory.join("password"), password.as_bytes())?;
    let htpasswd = checked(Command::new("docker").args([
        "run",
        "--rm",
        "--entrypoint",
        "htpasswd",
        "httpd:2-alpine",
        "-Bbn",
        REGISTRY_USER,
        &password,
    ]))
    .await?;
    write_private(&directory.join("htpasswd"), &htpasswd)?;
    checked(Command::new("openssl").args([
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-days",
        "7",
        "-subj",
        "/CN=wamn-delivery-ca",
        "-keyout",
        &path("ca.key"),
        "-out",
        &path("ca.crt"),
    ]))
    .await?;
    checked(Command::new("openssl").args([
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-subj",
        &format!("/CN={tls}"),
        "-keyout",
        &path("tls.key"),
        "-out",
        &path("tls.csr"),
    ]))
    .await?;
    let volume = format!("{}-registry-data", kept.name);
    checked(Command::new("docker").args(["volume", "create", &volume])).await?;
    for (name, secure) in [(tls, true), (http, false)] {
        let mut run = Command::new("docker");
        run.args(["run", "--detach", "--name", name, "--network", "kind"])
            .arg("--volume")
            .arg(format!("{}:/certs:ro", directory.display()))
            .args(["--volume", &format!("{volume}:/var/lib/registry")])
            .args([
                "--env",
                "REGISTRY_AUTH=htpasswd",
                "--env",
                "REGISTRY_AUTH_HTPASSWD_REALM=delivery",
                "--env",
                "REGISTRY_AUTH_HTPASSWD_PATH=/certs/htpasswd",
            ]);
        if secure {
            run.args([
                "--env",
                "REGISTRY_HTTP_TLS_CERTIFICATE=/certs/tls.crt",
                "--env",
                "REGISTRY_HTTP_TLS_KEY=/certs/tls.key",
            ]);
            // The certificate names the address, which is known only once the
            // container runs; the registry starts after the certificate exists.
            sign(&directory, tls, "127.0.0.1")?;
        }
        checked(run.arg("registry:2")).await?;
    }
    let address = container_address(tls).await?;
    sign(&directory, tls, &address)?;
    checked(Command::new("docker").args(["restart", tls])).await?;
    Ok(())
}

/// Sign the registry certificate for its name and `address`.
fn sign(directory: &Path, name: &str, address: &str) -> anyhow::Result<()> {
    let path = |file: &str| directory.join(file);
    fs::write(
        path("san.ext"),
        format!("subjectAltName=DNS:{name},IP:127.0.0.1,IP:{address}\n"),
    )?;
    let status = std::process::Command::new("openssl")
        .args(["x509", "-req", "-in"])
        .arg(path("tls.csr"))
        .arg("-CA")
        .arg(path("ca.crt"))
        .arg("-CAkey")
        .arg(path("ca.key"))
        .args(["-CAcreateserial", "-days", "7", "-extfile"])
        .arg(path("san.ext"))
        .arg("-out")
        .arg(path("tls.crt"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("start openssl")?;
    ensure!(
        status.success(),
        "openssl could not sign the registry certificate"
    );
    // The registry reads the key as another user.
    fs::set_permissions(path("tls.key"), fs::Permissions::from_mode(0o644))?;
    Ok(())
}

/// One application's database: a CloudNativePG `Cluster` named `cluster` in
/// `namespace`, reached through the proxy container `proxy` at port 5432.
#[derive(Debug, Clone)]
pub struct Database {
    /// The proxy's kind network address.
    pub host: String,
    pub password: String,
}

impl Database {
    /// The superuser URL of `database`.
    pub fn url(&self, database: &str) -> String {
        format!(
            "postgresql://postgres:{}@{}:5432/{database}",
            self.password, self.host
        )
    }
}

/// Ensure the application's CloudNativePG `Cluster`, a NodePort Service on its
/// primary, and the proxy container. The `Cluster` is
/// `deploy/infra/cnpg-cluster.yaml` under another name and namespace. The
/// journeys' `deploy/sql/postgres-init.sql` runs on it once it answers.
pub async fn ensure_database(
    kept: &KeptCluster,
    repository: &Path,
    namespace: &str,
    cluster: &str,
    proxy: &str,
) -> anyhow::Result<Database> {
    kept.apply(&json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":namespace}}))
        .await?;
    let mut definition: Value = serde_yaml::from_str(
        &fs::read_to_string(repository.join("deploy/infra/cnpg-cluster.yaml"))
            .context("read the CNPG Cluster")?,
    )?;
    definition["metadata"]["name"] = json!(cluster);
    definition["metadata"]["namespace"] = json!(namespace);
    kept.apply(&definition).await?;
    checked(kept.kubectl().args([
        "-n",
        namespace,
        "wait",
        &format!("cluster.postgresql.cnpg.io/{cluster}"),
        "--for=condition=Ready",
        "--timeout=600s",
    ]))
    .await?;
    let service = format!("{cluster}-node");
    kept.apply(&json!({"apiVersion":"v1","kind":"Service",
        "metadata":{"name":service,"namespace":namespace},
        "spec":{"type":"NodePort",
            "selector":{"cnpg.io/cluster":cluster,"cnpg.io/instanceRole":"primary"},
            "ports":[{"port":5432,"targetPort":5432}]}}))
        .await?;
    let observed: Value = serde_json::from_slice(
        &checked(
            kept.kubectl()
                .args(["-n", namespace, "get", "service", &service, "-o", "json"]),
        )
        .await?,
    )?;
    let node_port = observed["spec"]["ports"][0]["nodePort"]
        .as_u64()
        .context("the database Service has a NodePort")?;
    let encoded = checked(kept.kubectl().args([
        "-n",
        namespace,
        "get",
        "secret",
        &format!("{cluster}-superuser"),
        "-o",
        "jsonpath={.data.password}",
    ]))
    .await?;
    let password = String::from_utf8(base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        String::from_utf8(encoded)?.trim(),
    )?)?;
    if !container_exists(proxy).await? {
        let target = format!("tcp-connect:{}:{node_port}", kept.node_address().await?);
        checked(Command::new("docker").args([
            "run",
            "--detach",
            "--name",
            proxy,
            "--network",
            "kind",
            PROXY_IMAGE,
            "tcp-listen:5432,fork,reuseaddr",
            &target,
        ]))
        .await?;
    }
    let database = Database {
        host: container_address(proxy).await?,
        password,
    };
    let init = fs::read_to_string(repository.join("deploy/sql/postgres-init.sql"))?;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match tokio_postgres::connect(&database.url("postgres"), tokio_postgres::NoTls).await {
            Ok((client, connection)) => {
                let task = tokio::spawn(connection);
                let result = client.batch_execute(&init).await;
                task.abort();
                result.context("run postgres-init.sql on the application database")?;
                return Ok(database);
            }
            Err(error) if Instant::now() >= deadline => {
                bail!(
                    "the application database does not answer at {}: {error}",
                    database.host
                )
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
}

/// Start `name` on the kind network with `arguments` after `docker run
/// --detach --name <name> --network kind`, replacing a container of that
/// name, and return its address.
pub async fn start_container(name: &str, arguments: &[&str]) -> anyhow::Result<String> {
    if container_exists(name).await? {
        checked(Command::new("docker").args(["rm", "--force", "--volumes", name])).await?;
    }
    checked(
        Command::new("docker")
            .args(["run", "--detach", "--name", name, "--network", "kind"])
            .args(arguments),
    )
    .await?;
    container_address(name).await
}

async fn container_exists(name: &str) -> anyhow::Result<bool> {
    Ok(Command::new("docker")
        .args(["container", "inspect", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .context("start docker")?
        .success())
}

/// The kind network address of one container.
pub async fn container_address(name: &str) -> anyhow::Result<String> {
    let address = String::from_utf8(
        checked(Command::new("docker").args([
            "inspect",
            "--format",
            "{{(index .NetworkSettings.Networks \"kind\").IPAddress}}",
            name,
        ]))
        .await?,
    )?
    .trim()
    .to_owned();
    ensure!(
        !address.is_empty(),
        "container {name} has no kind network address"
    );
    Ok(address)
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create private file {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write private file {}", path.display()))
}

async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let output = command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("start {program}"))?;
    ensure!(
        output.status.success(),
        "{program} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}
