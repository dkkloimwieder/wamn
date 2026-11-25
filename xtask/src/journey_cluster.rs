//! Install the local kind platform for journey tests and record their inputs.
//!
//! The outputs file at `target/journey/cluster.json` matches
//! `tests/journeys/tests/common/mod.rs::Outputs` field for field.

use crate::{kind, platform, process};
use anyhow::{Context as _, ensure};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Pod, Secret};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use kube::Api;
use kube::api::{AttachParams, DeleteParams, ListParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use serde::Serialize;
use sqlx::ConnectOptions as _;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use std::io::{Read as _, Write as _};
use std::net::Ipv4Addr;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};

const OUTPUTS: &str = "target/journey";
const REGISTRY: &str = "localhost:5001";
const CONTEXT: &str = "kind-wamn-local";
const PLATFORM: &str = "platform";
/// The namespaces that the infrastructure and platform charts fill.
const PLATFORM_NAMESPACES: [&str; 3] = ["platform", "hosts", "cert-manager"];
/// `deploy/kind/kind-config.yaml` maps host port 80 to the ingress.
const WEB_ADDRESS: &str = "127.0.0.1:80";
const SYSTEM_CLUSTER: &str = "wamn-pg";
const SYSTEM_DATABASE: &str = "wamn_system";
const OBSERVER: &str = "wamn_journey_observer";
const READY_DEADLINE: Duration = Duration::from_secs(600);
/// The namespace that holds the NetworkPolicy check pods while it runs.
const NETWORK_CHECK: &str = "wamn-journey-network-check";
const NETWORK_DEADLINE: Duration = Duration::from_secs(120);

/// Arguments of `cargo xtask journey-cluster`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// A bundle directory built by `cargo xtask build-platform`. Without it, a new bundle is built.
    #[arg(long)]
    chart: Option<PathBuf>,
}

/// Everything the journey tests read. Field names match the journey crate.
#[derive(Serialize)]
struct Outputs<'a> {
    kubeconfig: &'a Path,
    context: &'a str,
    bundle: &'a Path,
    wamn: &'a Path,
    registry_auth_file: &'a Path,
    registry: &'a str,
    web_address: &'a str,
    base_domain: &'a str,
    observer_role: &'a str,
    observer_password: &'a str,
}

/// Create or reuse the cluster and platform, then write the outputs file.
pub async fn run(root: &Path, args: Args) -> anyhow::Result<()> {
    let results = std::env::var_os("WAMN_RESULTS")
        .filter(|path| !path.is_empty())
        .map(|path| telemetry_directory(Path::new(&path)))
        .transpose()?;
    let endpoint = kind::create_cluster_with_results(root, results.as_deref()).await?;
    let directory = root.join(OUTPUTS);
    std::fs::create_dir_all(&directory)?;
    let kubeconfig = directory.join("kubeconfig");
    process::run(
        root,
        "kind",
        &[
            "export",
            "kubeconfig",
            "--name",
            kind::CLUSTER,
            "--kubeconfig",
            text(&kubeconfig)?,
        ],
    )
    .await?;
    let config = kube::Config::from_custom_kubeconfig(
        Kubeconfig::read_from(&kubeconfig)?,
        &KubeConfigOptions {
            context: Some(CONTEXT.to_owned()),
            ..KubeConfigOptions::default()
        },
    )
    .await?;
    let client = kube::Client::try_from(config)?;
    check_network_policy(&client).await?;

    let bundle = if let Some(chart) = args.chart {
        chart
            .canonicalize()
            .with_context(|| format!("resolve bundle {}", chart.display()))?
    } else {
        let out = directory.join("bundle");
        if out.exists() {
            std::fs::remove_dir_all(&out)?;
        }
        platform::run(platform::Args {
            registry: REGISTRY.to_owned(),
            out: out.clone(),
            repository: root.to_owned(),
            stamp_only: false,
            cluster: None,
        })
        .await?;
        out
    };
    let (values, base_domain) =
        platform_values(root, &bundle, &endpoint, &directory, results.is_some())?;
    let registry_auth_file = directory.join("registry-auth.json");
    std::fs::write(
        &registry_auth_file,
        serde_json::to_vec(&serde_json::json!({"auths": {REGISTRY: {}}}))?,
    )?;
    ensure_platform_inputs(&client).await?;

    process::run(root, "cargo", &["build", "--locked", "-p", "wamn-cli"]).await?;
    let wamn = cargo_metadata::MetadataCommand::new()
        .current_dir(root)
        .no_deps()
        .exec()?
        .target_directory
        .into_std_path_buf()
        .join("debug/wamn");
    ensure!(wamn.is_file(), "wamn is absent at {}", wamn.display());

    let reference = std::fs::read_to_string(bundle.join("chart.ref"))
        .with_context(|| format!("read chart.ref in bundle {}", bundle.display()))?;
    let installed = Api::<ConfigMap>::namespaced(client.clone(), PLATFORM)
        .get_opt("wamn-org-inputs")
        .await?
        .and_then(|map| map.data)
        .and_then(|mut data| data.remove("applicationChart"));
    let collector_mount = Api::<Deployment>::namespaced(client.clone(), PLATFORM)
        .get_opt("otel-collector")
        .await?
        .and_then(|deployment| deployment.spec?.template.spec?.volumes)
        .is_some_and(|volumes| {
            volumes.into_iter().any(|volume| {
                volume
                    .host_path
                    .is_some_and(|path| path.path == kind::RESULTS_MOUNT)
            })
        });
    if installed.as_deref() == Some(reference.trim()) && collector_mount == results.is_some() {
        println!("The platform is installed at {}.", reference.trim());
    } else {
        process::execute(
            tokio::process::Command::new(&wamn)
                .current_dir(root)
                .args(["platform", "deploy"])
                .arg(&values)
                .arg("--chart")
                .arg(&bundle)
                .env("KUBECONFIG", &kubeconfig)
                .env("WAMN_REGISTRY_AUTH_FILE", &registry_auth_file),
        )
        .await?;
    }
    check_startup(&client).await?;
    let observer_password = create_observer(&client).await?;

    let outputs = Outputs {
        kubeconfig: &kubeconfig,
        context: CONTEXT,
        bundle: &bundle,
        wamn: &wamn,
        registry_auth_file: &registry_auth_file,
        registry: REGISTRY,
        web_address: WEB_ADDRESS,
        base_domain: &base_domain,
        observer_role: OBSERVER,
        observer_password: &observer_password,
    };
    let path = directory.join("cluster.json");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&serde_json::to_vec_pretty(&outputs)?)?;
    println!("{}", path.display());
    Ok(())
}

fn telemetry_directory(results: &Path) -> anyhow::Result<PathBuf> {
    let directory = results.canonicalize()?.join("telemetry");
    match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && directory.is_dir() => {}
        Err(error) => return Err(error).with_context(|| format!("create {}", directory.display())),
    }
    Ok(directory)
}

/// Write the bundle's kind values with the registry endpoint and optional telemetry.
fn platform_values(
    root: &Path,
    bundle: &Path,
    endpoint: &kind::RegistryEndpoint,
    directory: &Path,
    capture: bool,
) -> anyhow::Result<(PathBuf, String)> {
    let charts = bundle.join("platform/platform");
    let mut values: serde_yaml::Mapping =
        serde_yaml::from_slice(&std::fs::read(charts.join("values-kind.yaml"))?)?;
    values.insert(
        "registryEndpoint".into(),
        serde_yaml::to_value(serde_json::json!({
            "addressType": "IPv4",
            "address": endpoint.address,
            "port": endpoint.port,
        }))?,
    );
    if capture {
        // Read the capture configuration from this checkout so a prebuilt bundle can use it.
        let mut kind_values: serde_yaml::Mapping = serde_yaml::from_reader(std::fs::File::open(
            root.join("deploy/charts/platform/values-kind.yaml"),
        )?)?;
        let key = serde_yaml::Value::from("opentelemetry-collector");
        let mut collector = kind_values
            .remove(&key)
            .context("kind collector values are missing")?;
        let mount: serde_yaml::Mapping = serde_yaml::from_value(serde_yaml::to_value(
            serde_json::json!({
                "extraVolumes": [{
                    "name": "journey-results",
                    "hostPath": {"path": kind::RESULTS_MOUNT, "type": "Directory"}
                }],
                "extraVolumeMounts": [{"name": "journey-results", "mountPath": kind::RESULTS_MOUNT}],
                "securityContext": {
                    "runAsUser": rustix::process::getuid().as_raw(),
                    "runAsGroup": rustix::process::getgid().as_raw(),
                },
            }),
        )?)?;
        collector
            .as_mapping_mut()
            .context("kind collector values must be a map")?
            .extend(mount);
        for signal in ["traces", "logs"] {
            collector["config"]["service"]["pipelines"][signal] =
                serde_yaml::to_value(serde_json::json!({
                    "processors": ["memory_limiter", "transform/journey", "batch"],
                    "exporters": ["debug", "file/journey"],
                }))?;
        }
        values.insert(key, collector);
    }
    let defaults: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(charts.join("values.yaml"))?)?;
    let base_domain = values
        .get("platformDomain")
        .unwrap_or(&defaults["platformDomain"])
        .as_str()
        .context("the platform values have no platformDomain")?
        .to_owned();
    let path = directory.join("platform-kind.yaml");
    std::fs::write(&path, serde_yaml::to_string(&values)?)?;
    Ok((path, base_domain))
}

/// Create the platform namespace and the identity encryption key if they are absent.
async fn ensure_platform_inputs(client: &kube::Client) -> anyhow::Result<()> {
    let namespaces = Api::<Namespace>::all(client.clone());
    if namespaces.get_opt(PLATFORM).await?.is_none() {
        let namespace =
            serde_json::from_value(serde_json::json!({"metadata": {"name": PLATFORM}}))?;
        namespaces
            .create(&PostParams::default(), &namespace)
            .await?;
    }
    let secrets = Api::<Secret>::namespaced(client.clone(), PLATFORM);
    if secrets.get_opt("wamn-oauth").await?.is_none() {
        let mut key = [0_u8; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut key)?;
        let secret = Secret {
            metadata: kube::api::ObjectMeta {
                name: Some("wamn-oauth".to_owned()),
                ..kube::api::ObjectMeta::default()
            },
            data: Some(
                [("key".to_owned(), k8s_openapi::ByteString(key.to_vec()))]
                    .into_iter()
                    .collect(),
            ),
            ..Secret::default()
        };
        secrets.create(&PostParams::default(), &secret).await?;
    }
    Ok(())
}

/// Wait until each platform pod is ready, and print each restart with its last reason.
async fn check_startup(client: &kube::Client) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + READY_DEADLINE;
    loop {
        let mut pods = Vec::new();
        for namespace in PLATFORM_NAMESPACES {
            pods.extend(
                Api::<Pod>::namespaced(client.clone(), namespace)
                    .list(&ListParams::default())
                    .await?
                    .items,
            );
        }
        // Job pods finish; they are not services that become ready.
        pods.retain(|pod| {
            !pod.metadata
                .owner_references
                .iter()
                .flatten()
                .any(|owner| owner.kind == "Job")
        });
        let waiting: Vec<_> = pods
            .iter()
            .filter(|pod| !is_ready(pod))
            .map(pod_name)
            .collect();
        if waiting.is_empty() {
            for pod in &pods {
                let statuses = pod
                    .status
                    .as_ref()
                    .and_then(|status| status.container_statuses.as_ref());
                for container in statuses.into_iter().flatten() {
                    if container.restart_count > 0 {
                        let reason = container
                            .last_state
                            .as_ref()
                            .and_then(|state| state.terminated.as_ref())
                            .and_then(|terminated| terminated.reason.as_deref())
                            .unwrap_or("unknown");
                        println!(
                            "restart: {} container {} restarted {} times, last reason {reason}",
                            pod_name(pod),
                            container.name,
                            container.restart_count
                        );
                    }
                }
            }
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "platform pods never became ready within {} s: {}",
            READY_DEADLINE.as_secs(),
            waiting.join(", ")
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn pod_name(pod: &Pod) -> String {
    format!(
        "{}/{}",
        pod.metadata.namespace.as_deref().unwrap_or_default(),
        pod.metadata.name.as_deref().unwrap_or_default()
    )
}

/// Fail unless the cluster network enforces a NetworkPolicy that denies ingress.
async fn check_network_policy(client: &kube::Client) -> anyhow::Result<()> {
    let namespaces = Api::<Namespace>::all(client.clone());
    let deadline = tokio::time::Instant::now() + NETWORK_DEADLINE;
    // A namespace that an interrupted run left behind is removed first.
    while namespaces.get_opt(NETWORK_CHECK).await?.is_some() {
        namespaces
            .delete(NETWORK_CHECK, &DeleteParams::default())
            .await?;
        ensure!(
            tokio::time::Instant::now() < deadline,
            "namespace {NETWORK_CHECK} was not removed within {} s",
            NETWORK_DEADLINE.as_secs()
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let namespace =
        serde_json::from_value(serde_json::json!({"metadata": {"name": NETWORK_CHECK}}))?;
    namespaces
        .create(&PostParams::default(), &namespace)
        .await?;
    let checked = probe_network_policy(client).await;
    namespaces
        .delete(NETWORK_CHECK, &DeleteParams::default())
        .await?;
    checked
}

/// Connect one etcd pod to another, deny the server's ingress, and see the connection refused.
async fn probe_network_policy(client: &kube::Client) -> anyhow::Result<()> {
    // The etcd image is on the node already and holds both a server and a client.
    let image = Api::<Pod>::namespaced(client.clone(), "kube-system")
        .list(&ListParams::default().labels("component=etcd"))
        .await?
        .items
        .into_iter()
        .find_map(|pod| pod.spec?.containers.into_iter().next()?.image)
        .context("the cluster has no etcd pod")?;
    // A new namespace refuses pods until its default ServiceAccount exists.
    // A watch from resource version 0 first reports the objects that exist.
    let accounts = Api::<k8s_openapi::api::core::v1::ServiceAccount>::namespaced(
        client.clone(),
        NETWORK_CHECK,
    );
    let params = kube::api::WatchParams::default().fields("metadata.name=default");
    tokio::time::timeout(NETWORK_DEADLINE, async {
        let events = accounts.watch(&params, "0").await?;
        let mut events = std::pin::pin!(events);
        while let Some(event) = futures_util::TryStreamExt::try_next(&mut events).await? {
            if matches!(event, kube::api::WatchEvent::Added(_)) {
                return anyhow::Ok(());
            }
        }
        anyhow::bail!("the ServiceAccount watch ended before {NETWORK_CHECK}/default existed")
    })
    .await
    .with_context(|| {
        format!(
            "{NETWORK_CHECK}/default did not exist within {} s",
            NETWORK_DEADLINE.as_secs()
        )
    })??;
    let pods = Api::<Pod>::namespaced(client.clone(), NETWORK_CHECK);
    for name in ["server", "client"] {
        let pod = serde_json::from_value(serde_json::json!({
            "metadata": {"name": name, "labels": {"app": name}},
            "spec": {"containers": [{
                "name": "etcd",
                "image": image,
                "imagePullPolicy": "IfNotPresent",
                "command": [
                    "etcd",
                    "--data-dir=/tmp/etcd",
                    "--listen-client-urls=http://0.0.0.0:2379",
                    "--advertise-client-urls=http://127.0.0.1:2379",
                ],
            }]},
        }))?;
        pods.create(&PostParams::default(), &pod).await?;
    }
    let deadline = tokio::time::Instant::now() + NETWORK_DEADLINE;
    let server = loop {
        let server = pods.get("server").await?;
        let client_ready = is_ready(&pods.get("client").await?);
        if let Some(address) = server
            .status
            .as_ref()
            .and_then(|status| status.pod_ip.clone())
            .filter(|_| is_ready(&server) && client_ready)
        {
            break address;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "the NetworkPolicy check pods never became ready"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    let endpoint = format!("--endpoints=http://{server}:2379");
    let reach = async || {
        let mut process = pods
            .exec(
                "client",
                [
                    "etcdctl",
                    endpoint.as_str(),
                    "--dial-timeout=2s",
                    "--command-timeout=2s",
                    "endpoint",
                    "health",
                ],
                &AttachParams::default(),
            )
            .await?;
        let status = process.take_status().context("etcdctl exit status")?;
        process.join().await?;
        anyhow::Ok(status.await.and_then(|status| status.status).as_deref() == Some("Success"))
    };
    while !reach().await? {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "the NetworkPolicy check client never reached its server"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let policy: NetworkPolicy = serde_json::from_value(serde_json::json!({
        "metadata": {"name": "deny-server"},
        "spec": {"podSelector": {"matchLabels": {"app": "server"}}, "policyTypes": ["Ingress"]},
    }))?;
    Api::<NetworkPolicy>::namespaced(client.clone(), NETWORK_CHECK)
        .create(&PostParams::default(), &policy)
        .await?;
    let deadline = tokio::time::Instant::now() + NETWORK_DEADLINE;
    while reach().await? {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "the cluster network does not enforce NetworkPolicy: \
             the client still reaches a server whose ingress is denied"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}

fn is_ready(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        })
}

/// Create or reset the read-only observer login on the system Cluster, and return its password.
async fn create_observer(client: &kube::Client) -> anyhow::Result<String> {
    let pods = Api::<Pod>::namespaced(client.clone(), PLATFORM);
    let primary = pods
        .list(&ListParams::default().labels(&format!(
            "cnpg.io/cluster={SYSTEM_CLUSTER},cnpg.io/instanceRole=primary"
        )))
        .await?
        .items
        .into_iter()
        .find_map(|pod| pod.metadata.name)
        .context("the system Cluster has no primary pod")?;
    let forward = PortForward::start(pods, &primary, 5432).await?;
    let secrets = Api::<Secret>::namespaced(client.clone(), PLATFORM);
    let entry = async |name: &str, key: &str| {
        secrets
            .get(name)
            .await?
            .data
            .and_then(|mut data| data.remove(key))
            .map(|bytes| bytes.0)
            .with_context(|| format!("Secret {PLATFORM}/{name} has no {key}"))
    };
    let ca = entry(&format!("{SYSTEM_CLUSTER}-ca"), "ca.crt").await?;
    let admin_tls = format!("{SYSTEM_CLUSTER}-postgres-tls");
    let certificate = entry(&admin_tls, "tls.crt").await?;
    let key = entry(&admin_tls, "tls.key").await?;
    let server = || {
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(forward.port)
            .ssl_mode(PgSslMode::VerifyFull)
            .ssl_root_cert_from_pem(ca.clone())
    };
    // The system pg_hba admits postgres from 127.0.0.1 to the postgres database only.
    let mut admin = server()
        .username("postgres")
        .database("postgres")
        .ssl_client_cert_from_pem(&certificate)
        .ssl_client_key_from_pem(&key)
        .connect()
        .await
        .context("connect to the system Cluster as its admin")?;
    let mut bytes = [0_u8; 24];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let password = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes);
    // The names are constants and the password is URL-safe base64.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{OBSERVER}') THEN
             CREATE ROLE {OBSERVER};
           END IF;
         END $$;
         ALTER ROLE {OBSERVER} LOGIN BYPASSRLS PASSWORD '{password}';
         ALTER ROLE {OBSERVER} SET default_transaction_read_only = on;
         GRANT pg_read_all_data, pg_read_all_stats TO {OBSERVER};
         GRANT CONNECT ON DATABASE {SYSTEM_DATABASE} TO {OBSERVER};"
    )))
    .execute(&mut admin)
    .await
    .context("create the observer role")?;
    server()
        .username(OBSERVER)
        .password(&password)
        .database(SYSTEM_DATABASE)
        .connect()
        .await
        .with_context(|| format!("the system Cluster refuses {OBSERVER} to {SYSTEM_DATABASE}"))?;
    Ok(password)
}

/// A local TCP listener that forwards each connection to one pod port. Drop closes it.
struct PortForward {
    port: u16,
    task: JoinHandle<()>,
}

impl PortForward {
    async fn start(pods: Api<Pod>, pod: &str, port: u16) -> anyhow::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let local = listener.local_addr()?.port();
        let pod: Arc<str> = pod.into();
        let task = tokio::spawn(async move {
            // Aborting this task drops the set, which aborts every open connection.
            let mut connections = JoinSet::new();
            while let Ok((mut tcp, _)) = listener.accept().await {
                let pods = pods.clone();
                let pod = Arc::clone(&pod);
                connections.spawn(async move {
                    let forwarded = async {
                        let mut forwarder = pods.portforward(&pod, &[port]).await?;
                        let mut upstream = forwarder
                            .take_stream(port)
                            .context("the port-forward has no stream")?;
                        tokio::io::copy_bidirectional(&mut tcp, &mut upstream).await?;
                        drop(upstream);
                        forwarder.join().await?;
                        anyhow::Ok(())
                    };
                    if let Err(error) = forwarded.await {
                        eprintln!("port-forward to {pod}:{port}: {error:#}");
                    }
                });
            }
        });
        Ok(Self { port: local, task })
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn text(path: &Path) -> anyhow::Result<&str> {
    path.to_str().context("path is not UTF-8")
}
