//! Local kind cluster and registry setup.

use crate::process;
use anyhow::{Context as _, ensure};
use serde_json::Value;
use std::net::Ipv4Addr;
use std::path::Path;

pub(crate) const CLUSTER: &str = "wamn-local";
pub(super) const RESULTS_MOUNT: &str = "/var/local/wamn-journey-results";
const REGISTRY_PORT: &str = "5001";
const REGISTRY_NAME: &str = "wamn-registry";

/// The registry address that the kind nodes and pods reach.
#[derive(Debug)]
pub(crate) struct RegistryEndpoint {
    pub(crate) address: String,
    pub(crate) port: u16,
}

pub async fn create_cluster(root: &Path) -> anyhow::Result<RegistryEndpoint> {
    create_cluster_with_results(root, None).await
}

pub(super) async fn create_cluster_with_results(
    root: &Path,
    results: Option<&Path>,
) -> anyhow::Result<RegistryEndpoint> {
    let clusters = process::output(root, "kind", &["get", "clusters"]).await?;
    if !clusters.lines().any(|cluster| cluster == CLUSTER) {
        let config = results
            .map(|results| results_config(root, results))
            .transpose()?;
        let path = config.as_ref().map_or(
            Path::new("deploy/kind/kind-config.yaml"),
            tempfile::NamedTempFile::path,
        );
        process::run(
            root,
            "kind",
            &[
                "create",
                "cluster",
                "--name",
                CLUSTER,
                "--config",
                path.to_str()
                    .context("kind configuration path must be UTF-8")?,
            ],
        )
        .await?;
    }

    let registry = ensure_registry(root).await?;
    let nodes = process::output(root, "kind", &["get", "nodes", "--name", CLUSTER]).await?;
    ensure!(
        !nodes.trim().is_empty(),
        "kind cluster {CLUSTER} has no nodes"
    );
    let directory = format!("/etc/containerd/certs.d/localhost:{REGISTRY_PORT}");
    let endpoint = format!("http://{registry}:5000");
    let hosts = format!(
        "server = \"{endpoint}\"\n[host.\"{endpoint}\"]\n  capabilities = [\"pull\", \"resolve\"]"
    );
    for node in nodes.lines() {
        if let Some(results) = results {
            let mounts: Vec<Value> = serde_json::from_str(
                &process::output(
                    root,
                    "docker",
                    &["inspect", "--format", "{{json .Mounts}}", node],
                )
                .await?,
            )?;
            ensure!(
                mounts.iter().any(|mount| {
                    mount["Destination"] == RESULTS_MOUNT
                        && mount["Source"]
                            .as_str()
                            .is_some_and(|source| Path::new(source) == results)
                        && mount["RW"] == true
                }),
                "kind cluster {CLUSTER} needs to be recreated to mount {} on {node}",
                results.display(),
            );
        }
        process::run(
            root,
            "docker",
            &[
                "exec",
                node,
                "sh",
                "-ec",
                r#"if [ ! -f "$1/hosts.toml" ] || [ "$(cat "$1/hosts.toml")" != "$2" ]; then
                       mkdir -p "$1"
                       printf '%s\n' "$2" > "$1/hosts.toml"
                   fi"#,
                "sh",
                &directory,
                &hosts,
            ],
        )
        .await?;
    }
    let address = process::output(
        root,
        "docker",
        &[
            "inspect",
            "--format",
            "{{.NetworkSettings.Networks.kind.IPAddress}}",
            &registry,
        ],
    )
    .await?;
    let address: Ipv4Addr = address
        .trim()
        .parse()
        .context("read the registry address on the kind network")?;
    let endpoint = RegistryEndpoint {
        address: address.to_string(),
        port: 5000,
    };
    println!(
        "registryEndpoint:\n  addressType: IPv4\n  address: {}\n  port: {}",
        endpoint.address, endpoint.port
    );
    Ok(endpoint)
}

fn results_config(root: &Path, results: &Path) -> anyhow::Result<tempfile::NamedTempFile> {
    let mut config: serde_yaml::Value = serde_yaml::from_reader(std::fs::File::open(
        root.join("deploy/kind/kind-config.yaml"),
    )?)?;
    let nodes = config["nodes"]
        .as_sequence_mut()
        .context("kind configuration has no nodes")?;
    for node in nodes {
        if node["extraMounts"].is_null() {
            node["extraMounts"] = serde_yaml::Value::Sequence(Vec::new());
        }
        node["extraMounts"]
            .as_sequence_mut()
            .context("kind extraMounts must be a list")?
            .push(serde_yaml::to_value(serde_json::json!({
                "hostPath": results.to_str().context("run directory path must be UTF-8")?,
                "containerPath": RESULTS_MOUNT,
            }))?);
    }
    let file = tempfile::NamedTempFile::new()?;
    serde_yaml::to_writer(file.as_file(), &config)?;
    Ok(file)
}

async fn ensure_registry(root: &Path) -> anyhow::Result<String> {
    let ids = process::output(root, "docker", &["container", "ls", "--all", "--quiet"]).await?;
    if !ids.trim().is_empty() {
        let mut arguments = vec!["container", "inspect"];
        arguments.extend(ids.split_whitespace());
        let containers: Vec<Value> =
            serde_json::from_str(&process::output(root, "docker", &arguments).await?)?;
        for container in &containers {
            let binds_registry_port = container["HostConfig"]["PortBindings"]["5000/tcp"]
                .as_array()
                .is_some_and(|bindings| {
                    bindings.iter().any(|binding| {
                        binding["HostPort"] == REGISTRY_PORT
                            && matches!(
                                binding["HostIp"].as_str(),
                                Some("" | "127.0.0.1" | "0.0.0.0")
                            )
                    })
                });
            if !binds_registry_port {
                continue;
            }
            let name = container["Name"]
                .as_str()
                .context("read Docker container name")?
                .trim_start_matches('/');
            let image = container["Config"]["Image"]
                .as_str()
                .context("read Docker container image")?;
            let image = image.strip_prefix("docker.io/").unwrap_or(image);
            let image = image.strip_prefix("library/").unwrap_or(image);
            ensure!(
                image.split([':', '@']).next() == Some("registry"),
                "localhost:{REGISTRY_PORT} belongs to unrelated container {name}"
            );
            if container["State"]["Running"] != true {
                process::run(root, "docker", &["start", name]).await?;
            }
            if container["NetworkSettings"]["Networks"]["kind"].is_null() {
                process::run(root, "docker", &["network", "connect", "kind", name]).await?;
            }
            return Ok(name.to_owned());
        }
    }

    process::run(
        root,
        "docker",
        &[
            "run",
            "--detach",
            "--name",
            REGISTRY_NAME,
            "--restart=always",
            "--network",
            "kind",
            "--publish",
            &format!("127.0.0.1:{REGISTRY_PORT}:5000"),
            "--env",
            "REGISTRY_STORAGE_DELETE_ENABLED=true",
            "registry:2",
        ],
    )
    .await?;
    Ok(REGISTRY_NAME.into())
}
