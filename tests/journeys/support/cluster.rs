//! Shared journey-cluster access for the journeys and the kind capacity command.
#![allow(dead_code, reason = "each target uses a part of the shared harness")]

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, ensure};
use k8s_openapi::api::core::v1::{Pod, Secret};
use kube::Api;
use kube::api::{AttachParams, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use serde::Deserialize;
use sqlx::ConnectOptions as _;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};
use wamn_registry::{EnvName, OrgId};

#[path = "pooler.rs"]
pub(crate) mod pooler;

pub(crate) const OUTPUTS: &str = "target/journey";
pub(crate) const PLATFORM: &str = "platform";
const SYSTEM_CLUSTER: &str = "wamn-pg";
pub(crate) const SYSTEM_DATABASE: &str = "wamn_system";

/// The journey-cluster outputs. Field names match `xtask/src/journey_cluster.rs::Outputs`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Outputs {
    pub kubeconfig: PathBuf,
    pub context: String,
    pub bundle: PathBuf,
    pub wamn: PathBuf,
    pub registry_auth_file: PathBuf,
    pub registry: String,
    pub web_address: SocketAddr,
    pub base_domain: String,
    pub observer_role: String,
    pub observer_password: String,
}

/// The installed kind platform and a Kubernetes client on its recorded context.
pub(crate) struct Cluster {
    pub outputs: Outputs,
    pub client: kube::Client,
}

impl Cluster {
    /// Read the outputs file. A missing file or entry fails.
    pub async fn load(root: &Path) -> anyhow::Result<Self> {
        let path = root.join(OUTPUTS).join("cluster.json");
        let outputs: Outputs =
            serde_json::from_slice(&std::fs::read(&path).with_context(|| {
                format!("read {}; run cargo xtask journey-cluster", path.display())
            })?)
            .with_context(|| format!("decode {}", path.display()))?;
        for file in [
            &outputs.kubeconfig,
            &outputs.bundle,
            &outputs.wamn,
            &outputs.registry_auth_file,
        ] {
            ensure!(
                file.exists(),
                "journey-cluster output {} is absent",
                file.display()
            );
        }
        let kubeconfig = Kubeconfig::read_from(&outputs.kubeconfig)?;
        ensure!(
            kubeconfig.current_context.as_deref() == Some(outputs.context.as_str()),
            "the journey kubeconfig must select context {}",
            outputs.context
        );
        let config = kube::Config::from_custom_kubeconfig(
            kubeconfig,
            &KubeConfigOptions {
                context: Some(outputs.context.clone()),
                ..KubeConfigOptions::default()
            },
        )
        .await?;
        let client = kube::Client::try_from(config)?;
        Ok(Self { outputs, client })
    }

    /// A `wamn` child with the cluster inputs and no control session.
    pub fn command(&self) -> std::process::Command {
        let outputs = &self.outputs;
        let mut command = std::process::Command::new(&outputs.wamn);
        command
            .env("KUBECONFIG", &outputs.kubeconfig)
            .env("WAMN_REGISTRY_AUTH_FILE", &outputs.registry_auth_file)
            .env("WAMN_REGISTRY", &outputs.registry)
            .env_remove("WAMN_CONTROL_URL")
            .env_remove("WAMN_TOKEN")
            .env_remove("WAMN_RELEASE_CHART");
        command
    }

    /// Prepare the org creation command and return the unique owner's email.
    pub fn org_create_command(&self, org: &OrgId) -> (std::process::Command, String) {
        let email = format!("owner-{org}@example.com");
        let mut command = self.command();
        command.args(["org", "create", org.as_str(), "--owner", &email]);
        (command, email)
    }

    /// Prepare the env provisioning command.
    pub fn env_create_command(&self, org: &OrgId, env: &EnvName) -> std::process::Command {
        let mut command = self.command();
        command.args(["env", "create"]).arg(format!("{org}/{env}"));
        command
    }

    /// Connect to the system database with the read-only observer login.
    pub async fn observe_system(&self) -> anyhow::Result<Observed> {
        let pods = Api::<Pod>::namespaced(self.client.clone(), PLATFORM);
        let primary = pods
            .list(&ListParams::default().labels(&format!(
                "cnpg.io/cluster={SYSTEM_CLUSTER},cnpg.io/instanceRole=primary"
            )))
            .await?
            .items
            .into_iter()
            .find_map(|pod| pod.metadata.name)
            .context("the system Cluster has no primary pod")?;
        let ca = secret_key(
            &self.client,
            PLATFORM,
            &format!("{SYSTEM_CLUSTER}-server-tls"),
            "ca.crt",
        )
        .await?;
        let outputs = &self.outputs;
        self.observe(
            PLATFORM,
            &primary,
            SYSTEM_DATABASE,
            &ca,
            &outputs.observer_role,
            &outputs.observer_password,
        )
        .await
    }

    /// Connect to `database` on PostgreSQL port 5432 of `pod`, through a port-forward, as `role`.
    pub async fn observe(
        &self,
        namespace: &str,
        pod: &str,
        database: &str,
        server_ca: &[u8],
        role: &str,
        password: &str,
    ) -> anyhow::Result<Observed> {
        let forward =
            PortForward::start(Api::namespaced(self.client.clone(), namespace), pod, 5432).await?;
        let connection = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(forward.address.port())
            .ssl_mode(PgSslMode::VerifyCa)
            .ssl_root_cert_from_pem(server_ca.to_vec())
            .username(role)
            .password(password)
            .database(database)
            .connect()
            .await
            .with_context(|| format!("connect to {database} on {namespace}/{pod} as {role}"))?;
        Ok(Observed {
            connection,
            _forward: forward,
        })
    }
}

/// A read-only database session. The port-forward closes with it.
pub(crate) struct Observed {
    pub connection: sqlx::PgConnection,
    _forward: PortForward,
}

/// Read one Secret key as bytes for Pooler credentials or a certificate.
pub async fn secret_key(
    client: &kube::Client,
    namespace: &str,
    secret: &str,
    key: &str,
) -> anyhow::Result<Vec<u8>> {
    Api::<Secret>::namespaced(client.clone(), namespace)
        .get(secret)
        .await
        .with_context(|| format!("read key {key} from Secret {namespace}/{secret}"))?
        .data
        .and_then(|mut data| data.remove(key))
        .map(|bytes| bytes.0)
        .with_context(|| format!("Secret {namespace}/{secret} has no key {key}"))
}

pub(crate) async fn exec<const N: usize>(
    pods: &Api<Pod>,
    pod: &str,
    params: &AttachParams,
    command: [&str; N],
) -> anyhow::Result<String> {
    let mut process = pods.exec(pod, command, params).await?;
    let mut stdout = process.stdout().context("command output")?;
    let mut stderr = process.stderr().context("command diagnostics")?;
    let status = process.take_status().context("command status")?;
    let (mut output, mut diagnostics) = (String::new(), String::new());
    tokio::try_join!(
        stdout.read_to_string(&mut output),
        stderr.read_to_string(&mut diagnostics),
    )?;
    process.join().await?;
    let status = status.await.context("the command has no exit status")?;
    ensure!(
        status.status.as_deref() == Some("Success"),
        "{command:?} in {pod} failed: {} {diagnostics}",
        status.message.unwrap_or_default()
    );
    Ok(output)
}

pub(crate) async fn run(command: std::process::Command) -> anyhow::Result<String> {
    let mut command = tokio::process::Command::from(command);
    let output = command.kill_on_drop(true).output().await?;
    ensure!(
        output.status.success(),
        "{:?} failed: {}{}",
        command.as_std().get_args().collect::<Vec<_>>(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

/// A local listener that forwards each connection to one pod port. Drop closes it.
pub(crate) struct PortForward {
    pub(crate) address: SocketAddr,
    task: JoinHandle<()>,
}

impl PortForward {
    /// Forward connections to a named pod and port for metrics or a TLS handshake (C7, C11).
    pub(crate) async fn start(pods: Api<Pod>, pod: &str, port: u16) -> anyhow::Result<Self> {
        let namespace = pods
            .namespace()
            .context("a port-forward needs a namespace")?;
        ensure!(port != 0, "a port-forward needs a port");
        pods.get_metadata(pod)
            .await
            .with_context(|| format!("read pod {namespace}/{pod} for port-forward"))?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let pod: Arc<str> = pod.into();
        let task = tokio::spawn(async move {
            // Aborting this task drops the set, which aborts every open connection.
            let mut connections = JoinSet::new();
            while let Ok((mut tcp, _)) = listener.accept().await {
                while connections.try_join_next().is_some() {}
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
        Ok(Self { address, task })
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        self.task.abort();
    }
}
