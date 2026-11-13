//! Loopback port forwarding to selected Kubernetes resources.
use crate::{Target, kubectl};
use anyhow::{Context as _, ensure};
use serde_json::Value;
use std::{process::Stdio, time::Duration};
use tokio::process::Command;
/// A loopback port-forward whose child stops when this handle is dropped.
#[derive(Debug)]
pub struct PortForward {
    pub port: u16,
    child: tokio::process::Child,
    output: tokio::task::JoinHandle<()>,
}

impl Drop for PortForward {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.output.abort();
    }
}

/// Forward a resource port to an automatically allocated loopback port.
///
/// # Errors
/// Returns a process failure or a readiness timeout after thirty seconds.
/// kubectl forwards a Service only through its selector. The wasmCloud
/// operator publishes workload Services without one and owns their
/// EndpointSlices, so such a Service forwards to its ready endpoint Pod.
async fn forward_target(
    target: &Target,
    namespace: &str,
    resource: &str,
    port: u16,
) -> anyhow::Result<(String, u16)> {
    let Some(name) = resource.strip_prefix("service/") else {
        return Ok((resource.to_owned(), port));
    };
    let service: Value = crate::kubectl::get(
        &target.kubeconfig,
        &target.context,
        namespace,
        "service",
        name,
    )
    .await?
    .with_context(|| format!("missing Service {namespace}/{name}"))?;
    if service["spec"]["selector"]
        .as_object()
        .is_some_and(|selector| !selector.is_empty())
    {
        return Ok((resource.to_owned(), port));
    }
    let port_name = service["spec"]["ports"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["port"] == port)
        .and_then(|entry| entry["name"].as_str())
        .unwrap_or_default();
    loop {
        let slices: Value = serde_json::from_str(
            &kubectl::kubectl(
                &target.kubeconfig,
                &target.context,
                namespace,
                &[
                    "get",
                    "endpointslices",
                    &format!("--selector=kubernetes.io/service-name={name}"),
                    "-o",
                    "json",
                ],
                None,
            )
            .await?,
        )?;
        for slice in slices["items"].as_array().into_iter().flatten() {
            let Some(remote) = slice["ports"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| entry["name"].as_str().unwrap_or_default() == port_name)
                .and_then(|entry| entry["port"].as_u64())
            else {
                continue;
            };
            for endpoint in slice["endpoints"].as_array().into_iter().flatten() {
                if endpoint["conditions"]["ready"] == false {
                    continue;
                }
                let Some(address) = endpoint["addresses"][0].as_str() else {
                    continue;
                };
                let pods: Value = serde_json::from_str(
                    &kubectl::kubectl(
                        &target.kubeconfig,
                        &target.context,
                        namespace,
                        &[
                            "get",
                            "pods",
                            &format!("--field-selector=status.podIP={address}"),
                            "-o",
                            "json",
                        ],
                        None,
                    )
                    .await?,
                )?;
                if let Some(pod) = pods["items"][0]["metadata"]["name"].as_str() {
                    return Ok((format!("pod/{pod}"), u16::try_from(remote)?));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

pub async fn port_forward(
    target: &Target,
    namespace: &str,
    resource: &str,
    remote_port: u16,
) -> anyhow::Result<PortForward> {
    use tokio::io::AsyncBufReadExt as _;
    let (resource, remote_port) = tokio::time::timeout(
        Duration::from_secs(600),
        forward_target(target, namespace, resource, remote_port),
    )
    .await
    .with_context(|| format!("{resource} in {namespace} has no ready endpoint Pod after 600s"))??;
    let resource = resource.as_str();
    let mut child = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(&target.kubeconfig)
        .args([
            "--context",
            &target.context,
            "-n",
            namespace,
            "port-forward",
            "--address=127.0.0.1",
            resource,
            &format!(":{remote_port}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("start kubectl port-forward")?;
    let mut output = tokio::io::BufReader::new(child.stdout.take().context("port-forward output")?);
    let port = tokio::time::timeout(Duration::from_secs(30), async {
        let mut line = String::new();
        loop {
            line.clear();
            ensure!(
                output.read_line(&mut line).await? != 0,
                "kubectl port-forward stopped before binding"
            );
            if let Some(binding) = line.strip_prefix("Forwarding from 127.0.0.1:") {
                let (port, _) = binding.split_once(" -> ").context("port-forward binding")?;
                return port.parse::<u16>().context("port-forward local port");
            }
        }
    })
    .await
    .context("port-forward did not bind within 30s")??;
    let output = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut output, &mut tokio::io::sink()).await;
    });
    Ok(PortForward {
        port,
        child,
        output,
    })
}
