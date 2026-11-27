//! The journey Pooler endpoints and their TLS-verified tenant logins.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use k8s_openapi::api::core::v1::{ConfigMap, Pod, Secret};
use kube::Api;
use kube::api::ListParams;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use tokio::net::{TcpStream, UnixListener};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};

use super::{PLATFORM, PortForward, SYSTEM_DATABASE};

pub(crate) const CONTROL: &str = "control";
pub(crate) const APPLY: &str = "apply";
pub(crate) const APPLY_TRANSACTION: &str = "apply-transaction";
const POSTGRES_PORT: u16 = 5432;
const READY_DEADLINE: Duration = Duration::from_secs(60);

pub(crate) async fn ready_pooler(pods: &Api<Pod>, family: &str) -> anyhow::Result<String> {
    let name = format!("wamn-pg-{family}");
    let selector = ListParams::default().labels(&format!("cnpg.io/poolerName={name}"));
    timeout(READY_DEADLINE, async {
        loop {
            let ready = pods.list(&selector).await?.items.into_iter().find(|pod| {
                pod.metadata.deletion_timestamp.is_none()
                    && pod.status.as_ref().is_some_and(|status| {
                        status.conditions.as_ref().is_some_and(|conditions| {
                            conditions
                                .iter()
                                .any(|item| item.type_ == "Ready" && item.status == "True")
                        })
                    })
            });
            if let Some(pod) = ready {
                return pod.metadata.name.context("ready Pooler pod name");
            }
            sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .with_context(|| format!("find a ready pod for {PLATFORM}/{name} within {READY_DEADLINE:?}"))?
}

/// A forward shared by all clients of one Pooler. Keep its socket directory alive with it.
pub(crate) struct Pooler {
    port: u16,
    socket_dir: PathBuf,
    family: &'static str,
    _relay: JoinSet<()>,
    _forward: PortForward,
}

impl Pooler {
    pub(crate) async fn start(
        pods: Api<Pod>,
        pod: &str,
        family: &'static str,
        socket_dir: &Path,
    ) -> anyhow::Result<Self> {
        let forward = PortForward::start(pods, pod, POSTGRES_PORT).await?;
        // SQLx uses the host as the TLS name and the Unix socket as the transport.
        let address = forward.address;
        let socket = socket_dir.join(format!(".s.PGSQL.{}", address.port()));
        let listener = UnixListener::bind(&socket)
            .with_context(|| format!("bind the SQLx relay at {}", socket.display()))?;
        let mut relay = JoinSet::new();
        relay.spawn(async move {
            let mut connections = JoinSet::new();
            while let Ok((mut local, _)) = listener.accept().await {
                while connections.try_join_next().is_some() {}
                connections.spawn(async move {
                    let result = async {
                        let mut tcp = TcpStream::connect(address).await?;
                        tokio::io::copy_bidirectional(&mut local, &mut tcp).await?;
                        std::io::Result::Ok(())
                    }
                    .await;
                    if let Err(error) = result {
                        eprintln!("relay SQLx to {address}: {error}");
                    }
                });
            }
        });
        Ok(Self {
            port: address.port(),
            socket_dir: socket_dir.to_owned(),
            family,
            _relay: relay,
            _forward: forward,
        })
    }

    pub(crate) async fn login(
        &self,
        client: &kube::Client,
        namespace: &str,
    ) -> anyhow::Result<PgConnectOptions> {
        let family = self.family;
        let role = if family == CONTROL { CONTROL } else { APPLY };
        let secret = format!("wamn-{role}-db");
        let secrets = Api::<Secret>::namespaced(client.clone(), namespace);
        let roots = Api::<ConfigMap>::namespaced(client.clone(), namespace);
        let (login, ca) = tokio::try_join!(secrets.get(&secret), roots.get("wamn-pg-ca"))
            .with_context(|| format!("read {namespace}/{secret} and the system database CA"))?;
        let mut data = login.data.context("Pooler login Secret has no data")?;
        let mut entry = |key| {
            let bytes = data
                .remove(key)
                .with_context(|| format!("{secret} has no {key}"))?;
            String::from_utf8(bytes.0).with_context(|| format!("{secret} {key} must be UTF-8"))
        };
        let username = entry("username")?;
        let password = entry("password")?;
        let ca = ca
            .data
            .and_then(|mut data| data.remove("ca.crt"))
            .context("system database CA ConfigMap has no ca.crt")?;
        ensure!(
            !username.is_empty() && !password.is_empty() && !ca.is_empty(),
            "empty Pooler login or CA in {namespace}/{secret}"
        );
        Ok(PgConnectOptions::new_without_pgpass()
            .host(&format!("wamn-pg-{family}.{PLATFORM}.svc.cluster.local"))
            .port(self.port)
            .socket(&self.socket_dir)
            .database(SYSTEM_DATABASE)
            .username(&username)
            .password(&password)
            .ssl_root_cert_from_pem(ca.into_bytes())
            .ssl_mode(PgSslMode::VerifyFull))
    }
}
