//! Provider-specific kubeconfig creation in a caller-owned file.

use std::ffi::OsString;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Output, Stdio};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;

use super::document::{ClusterDocument, Provider};

impl Provider {
    /// Write the cluster's kubeconfig and return its context name.
    ///
    /// # Errors
    ///
    /// Returns an error if a command fails or the kind cluster list is not UTF-8.
    pub async fn kubeconfig(
        self,
        document: &ClusterDocument,
        path: &Path,
    ) -> anyhow::Result<String> {
        match self {
            Self::Gcp => {
                run(Command::new("gcloud")
                    .args(gcp_arguments(document))
                    .env("KUBECONFIG", path))
                .await?;
            }
            Self::Kind => {
                let local_registry = ensure_registry(document).await?;
                let output = run(Command::new("kind").args(kind_list_arguments())).await?;
                let clusters =
                    std::str::from_utf8(&output.stdout).context("decode kind get clusters")?;
                let configuration = path.with_extension("type.yaml");
                if !cluster_exists(clusters, &document.cluster_name) {
                    let registry = path.with_extension("registry");
                    let (host, _) = registry_endpoint(&document.registry)?;
                    std::fs::create_dir_all(registry.join(host))
                        .context("create the kind registry directory")?;
                    std::fs::write(
                        registry.join(host).join("hosts.toml"),
                        registry_hosts(&local_registry),
                    )
                    .context("write the kind registry mapping")?;
                    let registry = std::fs::canonicalize(registry)
                        .context("find the kind registry directory")?;
                    std::fs::write(
                        &configuration,
                        serde_yaml::to_string(&kind_configuration(&registry))?,
                    )
                    .context("write the kind configuration")?;
                }
                run(Command::new("kind").args(kind_arguments(
                    &document.cluster_name,
                    path,
                    clusters,
                    &configuration,
                )))
                .await?;
                repair_registry_mapping(document, &local_registry).await?;
            }
        }
        Ok(context_name(self, document))
    }
}

/// A registry attached to the kind network.
#[derive(Debug)]
pub struct LocalRegistry {
    pub container: String,
    /// The container's address on the kind network, for pod access.
    pub address: IpAddr,
    /// The registry port inside the container, not the host's published port.
    pub port: u16,
}

/// Start or reuse the registry at the type document's loopback endpoint.
///
/// # Errors
///
/// Refuses non-kind documents, unrelated endpoint owners, or failed Docker commands.
pub async fn ensure_registry(document: &ClusterDocument) -> anyhow::Result<LocalRegistry> {
    ensure!(
        document.provider == Provider::Kind,
        "local registries require type"
    );
    let (_, endpoint) = registry_endpoint(&document.registry)?;
    let output = run(Command::new("docker").args(["ps", "--all", "--quiet"])).await?;
    let ids = std::str::from_utf8(&output.stdout).context("decode Docker container IDs")?;
    let containers: Vec<Value> = if ids.trim().is_empty() {
        Vec::new()
    } else {
        let output = run(Command::new("docker")
            .args(["inspect"])
            .args(ids.split_whitespace()))
        .await?;
        serde_json::from_slice(&output.stdout).context("decode Docker containers")?
    };
    let matching = matching_registry(&containers, endpoint)?;
    // Docker remains the final arbiter of the port binding. This also refuses
    // a host process before creating a container or pulling its image.
    if matching.is_none_or(|container| container["State"]["Running"] != true) {
        TcpListener::bind(endpoint)
            .with_context(|| format!("registry endpoint {endpoint} is occupied or unavailable"))?;
    }
    let output =
        run(Command::new("docker").args(["network", "ls", "--format", "{{.Name}}"])).await?;
    if !std::str::from_utf8(&output.stdout)
        .context("decode Docker network names")?
        .lines()
        .any(|name| name == "kind")
    {
        run(Command::new("docker").args(["network", "create", "kind"])).await?;
    }
    let created;
    let id = if let Some(container) = matching {
        let id = container["Id"]
            .as_str()
            .context("registry container has no ID")?;
        if container["NetworkSettings"]["Networks"]["kind"].is_null() {
            run(Command::new("docker").args(["network", "connect", "kind", id])).await?;
        }
        if container["State"]["Running"] != true {
            run(Command::new("docker").args(["start", id])).await?;
        }
        id
    } else {
        let output = run(Command::new("docker").args(registry_arguments(endpoint))).await?;
        created = String::from_utf8(output.stdout).context("decode created registry ID")?;
        created.trim()
    };
    let container = inspect_container(id).await?;
    let network = &container["NetworkSettings"]["Networks"]["kind"];
    let address = ["IPAddress", "GlobalIPv6Address"]
        .into_iter()
        .filter_map(|key| network[key].as_str())
        .find(|address| !address.is_empty())
        .context("registry has no address on the kind network")?
        .parse()
        .context("parse the registry container address")?;
    Ok(LocalRegistry {
        container: container["Name"]
            .as_str()
            .context("registry container has no name")?
            .trim_start_matches('/')
            .to_owned(),
        address,
        port: 5000,
    })
}

/// Export an existing cluster's kubeconfig without creating local resources.
///
/// # Errors
///
/// Returns an error if the named cluster is absent or credential export fails.
pub async fn existing_kubeconfig(
    document: &ClusterDocument,
    path: &Path,
) -> anyhow::Result<String> {
    available_kubeconfig(document, path)
        .await?
        .with_context(|| format!("kind cluster {} does not exist", document.cluster_name))
}

/// Export credentials if the cluster exists, without creating local resources.
///
/// # Errors
/// Returns an error if cluster discovery or credential export fails.
pub async fn available_kubeconfig(
    document: &ClusterDocument,
    path: &Path,
) -> anyhow::Result<Option<String>> {
    match document.provider {
        Provider::Gcp => {
            run(Command::new("gcloud")
                .args(gcp_arguments(document))
                .env("KUBECONFIG", path))
            .await?;
        }
        Provider::Kind => {
            let output = run(Command::new("kind").args(kind_list_arguments())).await?;
            let clusters =
                std::str::from_utf8(&output.stdout).context("decode kind get clusters")?;
            if !cluster_exists(clusters, &document.cluster_name) {
                return Ok(None);
            }
            run(Command::new("kind").args(kind_export_arguments(&document.cluster_name, path)))
                .await?;
        }
    }
    Ok(Some(context_name(document.provider, document)))
}

fn registry_endpoint(registry: &str) -> anyhow::Result<(&str, SocketAddr)> {
    let authority = registry.split('/').next().unwrap_or_default();
    let endpoint: SocketAddr = if let Some(port) = authority.strip_prefix("localhost:") {
        format!("127.0.0.1:{port}").parse()
    } else {
        authority.parse()
    }
    .context("kind registry requires a loopback host and port")?;
    ensure!(
        endpoint.ip().is_loopback() && endpoint.port() != 0,
        "kind registry requires a loopback address and nonzero port"
    );
    Ok((authority, endpoint))
}

fn matching_registry(containers: &[Value], endpoint: SocketAddr) -> anyhow::Result<Option<&Value>> {
    let port = endpoint.port().to_string();
    let mut matching = None;
    for container in containers {
        // Docker assigns an actual host port when a caller publishes port zero.
        // Stopped containers retain their requested bindings in HostConfig.
        let ports = if container["State"]["Running"] == true {
            &container["NetworkSettings"]["Ports"]
        } else {
            &container["HostConfig"]["PortBindings"]
        };
        let Some(bindings) = ports.as_object() else {
            continue;
        };
        for (container_port, bindings) in bindings {
            if !container_port.ends_with("/tcp") {
                continue;
            }
            let Some(bindings) = bindings.as_array() else {
                continue;
            };
            if !bindings.iter().any(|binding| {
                binding["HostPort"] == port
                    && binding["HostIp"].as_str().is_some_and(|host| {
                        host.is_empty()
                            || host.parse::<IpAddr>().is_ok_and(|ip| {
                                ip == endpoint.ip()
                                    || (ip.is_unspecified() && (ip.is_ipv6() || endpoint.is_ipv4()))
                            })
                    })
            }) {
                continue;
            }
            let image = container["Config"]["Image"].as_str().unwrap_or_default();
            let image = image.strip_prefix("docker.io/").unwrap_or(image);
            let image = image.strip_prefix("library/").unwrap_or(image);
            ensure!(
                container_port == "5000/tcp"
                    && (image == "registry"
                        || image.starts_with("registry:")
                        || image.starts_with("registry@")),
                "registry endpoint {endpoint} belongs to unrelated container {}",
                container["Name"]
            );
            ensure!(
                matching.is_none(),
                "multiple registry containers bind {endpoint}"
            );
            matching = Some(container);
        }
    }
    Ok(matching)
}

fn registry_arguments(endpoint: SocketAddr) -> Vec<String> {
    vec![
        "run".into(),
        "--detach".into(),
        "--restart=always".into(),
        "--network".into(),
        "kind".into(),
        "--publish".into(),
        format!("{endpoint}:5000"),
        "registry:2".into(),
    ]
}

async fn inspect_container(name: &str) -> anyhow::Result<Value> {
    let output = run(Command::new("docker").args(["inspect", name])).await?;
    let mut containers: Vec<Value> =
        serde_json::from_slice(&output.stdout).context("decode Docker inspect")?;
    containers
        .pop()
        .context("Docker inspect returned no container")
}

async fn repair_registry_mapping(
    document: &ClusterDocument,
    registry: &LocalRegistry,
) -> anyhow::Result<()> {
    let (authority, _) = registry_endpoint(&document.registry)?;
    let hosts = registry_hosts(registry);
    let output =
        run(Command::new("kind").args(["get", "nodes", "--name", &document.cluster_name])).await?;
    let nodes = std::str::from_utf8(&output.stdout).context("decode kind nodes")?;
    ensure!(
        !nodes.trim().is_empty(),
        "kind cluster {} has no nodes",
        document.cluster_name
    );
    for node in nodes.lines() {
        let container = inspect_container(node).await?;
        // Existing WAMN clusters mount this directory read-only. Repair its
        // source instead of replacing the mount or recreating the node.
        let source = container["Mounts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|mount| mount["Destination"] == "/etc/containerd/certs.d")
            .and_then(|mount| mount["Source"].as_str());
        if let Some(source) = source {
            let directory = Path::new(source).join(authority);
            std::fs::create_dir_all(&directory).context("create registry mapping directory")?;
            let path = directory.join("hosts.toml");
            let existing = match std::fs::read_to_string(&path) {
                Ok(existing) => existing,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(error).context("read mounted registry mapping"),
            };
            if existing != hosts {
                std::fs::write(path, &hosts).context("repair mounted registry mapping")?;
            }
        } else {
            run(Command::new("docker").args([
                "exec",
                node,
                "sh",
                "-c",
                "if [ ! -f \"$1/hosts.toml\" ] || [ \"$(cat \"$1/hosts.toml\")\" != \"$2\" ]; then mkdir -p \"$1\" && printf '%s\\n' \"$2\" > \"$1/hosts.toml\"; fi",
                "sh",
                &format!("/etc/containerd/certs.d/{authority}"),
                hosts.trim_end(),
            ]))
            .await?;
        }
    }
    Ok(())
}

/// The Secret Manager secret that holds the Resend key on `gcp` (wamn-ie1j2).
pub(super) const RESEND_SECRET: &str = "identity-resend";

impl Provider {
    /// Read the Resend key from the provider's own key store, or `None` where
    /// the provider has none and the cluster Secret is the owner input.
    ///
    /// On `gcp` the key lives in Secret Manager, which outlives a teardown.
    /// The key never reaches a log: a failure names only gcloud's standard error.
    ///
    /// # Errors
    ///
    /// Returns an error if gcloud fails or writes no UTF-8 text.
    pub async fn resend_key(self, document: &ClusterDocument) -> anyhow::Result<Option<String>> {
        match self {
            Self::Gcp => {
                let output = run(Command::new("gcloud").args(resend_arguments(document))).await?;
                String::from_utf8(output.stdout)
                    .map(Some)
                    .context("decode the Resend key")
            }
            Self::Kind => Ok(None),
        }
    }
}

fn resend_arguments(document: &ClusterDocument) -> [&str; 8] {
    [
        "secrets",
        "versions",
        "access",
        "latest",
        "--secret",
        RESEND_SECRET,
        "--project",
        &document.project_id,
    ]
}

fn gcp_arguments(document: &ClusterDocument) -> [&str; 8] {
    [
        "container",
        "clusters",
        "get-credentials",
        &document.cluster_name,
        "--zone",
        &document.zone,
        "--project",
        &document.project_id,
    ]
}

fn kind_list_arguments() -> [&'static str; 2] {
    ["get", "clusters"]
}

fn kind_arguments(
    cluster_name: &str,
    path: &Path,
    clusters: &str,
    configuration: &Path,
) -> Vec<OsString> {
    if cluster_exists(clusters, cluster_name) {
        return kind_export_arguments(cluster_name, path);
    }
    vec![
        "create".into(),
        "cluster".into(),
        "--name".into(),
        cluster_name.into(),
        "--kubeconfig".into(),
        path.as_os_str().to_owned(),
        "--config".into(),
        configuration.as_os_str().to_owned(),
        "--retain".into(),
    ]
}

fn cluster_exists(clusters: &str, cluster_name: &str) -> bool {
    clusters.lines().any(|name| name.trim() == cluster_name)
}

fn kind_export_arguments(cluster_name: &str, path: &Path) -> Vec<OsString> {
    vec![
        "export".into(),
        "kubeconfig".into(),
        "--name".into(),
        cluster_name.into(),
        "--kubeconfig".into(),
        path.as_os_str().to_owned(),
    ]
}

fn registry_hosts(registry: &LocalRegistry) -> String {
    let endpoint = format!("{}:{}", registry.container, registry.port);
    format!(
        "server = \"http://{endpoint}\"\n[host.\"http://{endpoint}\"]\n  capabilities = [\"pull\", \"resolve\"]\n"
    )
}

fn kind_configuration(directory: &Path) -> Value {
    // kind's local registry pattern uses containerd's per-authority hosts file.
    // Mount the whole hosts directory at creation so it also survives a node
    // restart: Docker refuses a mount target with the authority's port colon.
    json!({
        "kind": "Cluster",
        "apiVersion": "kind.x-k8s.io/v1alpha4",
        "containerdConfigPatches": [
            "[plugins.\"io.containerd.grpc.v1.cri\".registry]\n  config_path = \"/etc/containerd/certs.d\"\n"
        ],
        "nodes": [{
            "role": "control-plane",
            "extraMounts": [{
                "hostPath": directory,
                "containerPath": "/etc/containerd/certs.d",
                "readOnly": true
            }]
        }]
    })
}

fn context_name(provider: Provider, document: &ClusterDocument) -> String {
    match provider {
        Provider::Gcp => format!(
            "gke_{}_{}_{}",
            document.project_id, document.zone, document.cluster_name
        ),
        Provider::Kind => format!("kind-{}", document.cluster_name),
    }
}

pub(super) async fn run(command: &mut Command) -> anyhow::Result<Output> {
    let name = std::iter::once(command.as_std().get_program())
        .chain(command.as_std().get_args())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let output = command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("run {name}"))?;
    ensure!(
        output.status.success(),
        "{name} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{
        LocalRegistry, context_name, gcp_arguments, kind_arguments, kind_configuration,
        kind_list_arguments, matching_registry, registry_arguments, registry_endpoint,
        registry_hosts, resend_arguments,
    };

    #[test]
    fn gcp_reads_the_resend_key_from_secret_manager_in_the_document_project() {
        assert_eq!(
            resend_arguments(&document(Provider::Gcp)),
            [
                "secrets",
                "versions",
                "access",
                "latest",
                "--secret",
                "identity-resend",
                "--project",
                "test-project"
            ]
        );
    }
    use crate::platform::document::{Buckets, ClusterDocument, Provider, RouteHosts};
    use std::ffi::OsString;
    use std::path::Path;

    fn document(provider: Provider) -> ClusterDocument {
        ClusterDocument {
            build_cloud_cap_usd: 3.0,
            provider,
            project_id: "test-project".into(),
            zone: "us-central1-a".into(),
            cluster_name: "install-test".into(),
            org: "test".into(),
            owner_email: "owner@example.com".into(),
            registry: "localhost:5000".into(),
            buckets: Buckets {
                web: String::new(),
                otel: String::new(),
                state: String::new(),
            },
            route_hosts: RouteHosts {
                control: "control.example.com".into(),
            },
            resend_from: "sender@example.com".into(),
        }
    }

    #[test]
    fn gcp_only_gets_credentials_and_returns_the_gke_context() {
        let document = document(Provider::Gcp);
        assert_eq!(
            gcp_arguments(&document),
            [
                "container",
                "clusters",
                "get-credentials",
                "install-test",
                "--zone",
                "us-central1-a",
                "--project",
                "test-project",
            ]
        );
        assert_eq!(
            context_name(document.provider, &document),
            "gke_test-project_us-central1-a_install-test"
        );
    }

    #[test]
    fn kind_creates_only_when_the_exact_cluster_is_absent() {
        assert_eq!(kind_list_arguments(), ["get", "clusters"]);
        for clusters in ["", "install-test-other\nother-install-test\n"] {
            assert_eq!(
                kind_arguments(
                    "install-test",
                    Path::new("/var/tmp/install kubeconfig"),
                    clusters,
                    Path::new("/var/tmp/install.type.yaml"),
                ),
                [
                    "create",
                    "cluster",
                    "--name",
                    "install-test",
                    "--kubeconfig",
                    "/var/tmp/install kubeconfig",
                    "--config",
                    "/var/tmp/install.type.yaml",
                    "--retain",
                ]
                .map(OsString::from)
            );
        }
    }

    #[test]
    fn kind_exports_on_a_second_run_and_returns_the_kind_context() {
        let document = document(Provider::Kind);
        assert_eq!(
            kind_arguments(
                &document.cluster_name,
                Path::new("/var/tmp/install kubeconfig"),
                "other\ninstall-test\n",
                Path::new("/var/tmp/install.type.yaml"),
            ),
            [
                "export",
                "kubeconfig",
                "--name",
                "install-test",
                "--kubeconfig",
                "/var/tmp/install kubeconfig",
            ]
            .map(OsString::from)
        );
        assert_eq!(
            context_name(document.provider, &document),
            "kind-install-test"
        );
    }

    #[test]
    fn kind_mounts_the_registry_hosts_directory() {
        let value = kind_configuration(Path::new("/var/tmp/kind registry"));
        assert_eq!(value["apiVersion"], "kind.x-k8s.io/v1alpha4");
        assert_eq!(value["kind"], "Cluster");
        assert_eq!(
            value["nodes"],
            serde_json::json!([{
                "role":"control-plane",
                "extraMounts":[{
                    "hostPath":"/var/tmp/kind registry",
                    "containerPath":"/etc/containerd/certs.d",
                    "readOnly":true
                }]
            }])
        );
        assert_eq!(
            value["containerdConfigPatches"],
            serde_json::json!([
                "[plugins.\"io.containerd.grpc.v1.cri\".registry]\n  config_path = \"/etc/containerd/certs.d\"\n"
            ])
        );
    }

    #[test]
    fn registry_authority_is_loopback_with_an_explicit_port() {
        for (input, authority, endpoint) in [
            (
                "localhost:5001/platform",
                "localhost:5001",
                "127.0.0.1:5001",
            ),
            ("127.0.0.2:5002", "127.0.0.2:5002", "127.0.0.2:5002"),
            ("[::1]:5003/platform", "[::1]:5003", "[::1]:5003"),
        ] {
            let (actual_authority, actual_endpoint) = registry_endpoint(input).unwrap();
            assert_eq!(actual_authority, authority);
            assert_eq!(actual_endpoint, endpoint.parse().unwrap());
        }
        for input in [
            "localhost",
            "localhost:0",
            "localhost:65536",
            "localhost:port",
            "localhost:+5001",
            "https://localhost:5001",
            "user@localhost:5001",
            "example.com:5001",
            "192.168.1.1:5001",
            "[::]:5001",
            "::1:5001",
            "",
        ] {
            assert!(registry_endpoint(input).is_err(), "{input}");
        }
    }

    #[test]
    fn registry_creation_publishes_only_the_selected_loopback_endpoint() {
        for endpoint in ["127.0.0.1:5001", "[::1]:5001"] {
            assert_eq!(
                registry_arguments(endpoint.parse().unwrap()),
                [
                    "run",
                    "--detach",
                    "--restart=always",
                    "--network",
                    "kind",
                    "--publish",
                    &format!("{endpoint}:5000"),
                    "registry:2",
                ]
            );
        }
    }

    #[test]
    fn registry_mapping_uses_the_discovered_container() {
        let registry = LocalRegistry {
            container: "existing-artifacts".into(),
            address: "172.18.0.3".parse().unwrap(),
            port: 5000,
        };
        assert_eq!(
            registry_hosts(&registry),
            "server = \"http://existing-artifacts:5000\"\n[host.\"http://existing-artifacts:5000\"]\n  capabilities = [\"pull\", \"resolve\"]\n"
        );
    }

    #[test]
    fn registry_selection_uses_bindings_including_stopped_containers() {
        let mut containers = [serde_json::json!({
            "Name": "/arbitrary-registry", "Config": {"Image": "registry:2"},
            "State": {"Running": false},
            "HostConfig": {"PortBindings": {"5000/tcp": [
                {"HostIp": "127.0.0.1", "HostPort": "5001"}
            ]}}
        })];
        let endpoint = "127.0.0.1:5001".parse().unwrap();
        assert!(matching_registry(&containers, endpoint).unwrap().is_some());
        assert!(
            matching_registry(&containers, "127.0.0.1:5002".parse().unwrap())
                .unwrap()
                .is_none()
        );
        assert!(
            matching_registry(&containers, "127.0.0.2:5001".parse().unwrap())
                .unwrap()
                .is_none()
        );
        containers[0]["State"]["Running"] = true.into();
        containers[0]["HostConfig"]["PortBindings"]["5000/tcp"][0]["HostPort"] = "0".into();
        containers[0]["NetworkSettings"] = serde_json::json!({"Ports": {"5000/tcp": [
            {"HostIp": "127.0.0.1", "HostPort": "5001"}
        ]}});
        assert!(matching_registry(&containers, endpoint).unwrap().is_some());
        containers[0]["Config"]["Image"] = "nginx:latest".into();
        assert!(matching_registry(&containers, endpoint).is_err());
    }
}
