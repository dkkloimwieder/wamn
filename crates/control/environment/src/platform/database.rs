//! Verified SQL connections through an operator's Kubernetes port forward.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;

use anyhow::Context as _;
use wamn_release_control::release_chart::Target;

use super::bootstrap::{SecretData, secret_text};

/// Keep the forward and local socket alive while SQLx verifies the service name.
pub(super) struct Forward {
    pub input: String,
    directory: PathBuf,
    relay: tokio::task::JoinHandle<()>,
    _transport: wamn_kubernetes::forward::PortForward,
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.relay.abort();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Preserve the CNPG service's TLS identity over a loopback transport.
pub(super) async fn forward(
    target: &Target,
    namespace: &str,
    cluster: &str,
    database: &str,
) -> anyhow::Result<Forward> {
    let credential: SecretData = wamn_kubernetes::kubectl::get(
        &target.kubeconfig,
        &target.context,
        namespace,
        "secret",
        &format!("{cluster}-superuser"),
    )
    .await?
    .context("CNPG administrator Secret is absent")?;
    let ca: SecretData = wamn_kubernetes::kubectl::get(
        &target.kubeconfig,
        &target.context,
        namespace,
        "secret",
        &format!("{cluster}-ca"),
    )
    .await?
    .context("CNPG CA is absent")?;
    let forward = wamn_kubernetes::forward::port_forward(
        target,
        namespace,
        &format!("service/{cluster}-rw"),
        5432,
    )
    .await?;
    let directory = std::env::temp_dir().join(format!("wamn-pg-{}", sqlx::types::Uuid::new_v4()));
    std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let result = async {
        let ca_path = directory.join("ca.crt");
        std::fs::write(&ca_path, secret_text(&ca, "ca.crt")?)?;
        let listener = tokio::net::UnixListener::bind(directory.join(".s.PGSQL.5432"))?;
        let port = forward.port;
        let relay = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    if let Ok(mut upstream) =
                        tokio::net::TcpStream::connect(("127.0.0.1", port)).await
                    {
                        let _ = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await;
                    }
                });
            }
        });
        let mut input = url::Url::parse(&format!(
            "postgres://{cluster}-rw.{namespace}.svc.cluster.local/{database}"
        ))?;
        input
            .set_username(&secret_text(&credential, "username")?)
            .map_err(|()| anyhow::anyhow!("administrator username"))?;
        input
            .set_password(Some(&secret_text(&credential, "password")?))
            .map_err(|()| anyhow::anyhow!("administrator password"))?;
        input
            .query_pairs_mut()
            .append_pair("host", &directory.to_string_lossy())
            .append_pair("sslrootcert", &ca_path.to_string_lossy());
        Ok::<_, anyhow::Error>((input.into(), relay))
    }
    .await;
    match result {
        Ok((input, relay)) => Ok(Forward {
            input,
            directory,
            relay,
            _transport: forward,
        }),
        Err(error) => {
            let _ = std::fs::remove_dir_all(directory);
            Err(error)
        }
    }
}
