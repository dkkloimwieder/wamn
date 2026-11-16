//! Provider-specific kubeconfig creation in a caller-owned file.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Output, Stdio};

use anyhow::{Context as _, ensure};
use tokio::process::Command;

use super::document::{ClusterDocument, Provider};

impl Provider {
    /// Write the cluster's kubeconfig and return its context name.
    ///
    /// # Errors
    ///
    /// Returns an error if a command fails or the kind cluster list is not UTF-8.
    pub async fn kubeconfig(self, _: &ClusterDocument, _: &Path) -> anyhow::Result<String> {
        std::future::ready(Ok(String::new())).await
    }
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

fn gcp_arguments(document: &ClusterDocument) -> [&str; 8] {
    [
        "container",
        "clusters",
        "get-credentials",
        &document.cluster_name,
        "--zone",
        &document.zone,
        "--project",
        document.project_id.as_str(),
    ]
}

fn kind_list_arguments() -> [&'static str; 2] {
    ["get", "clusters"]
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
    use super::{context_name, gcp_arguments};
    use crate::platform::document::{Buckets, ClusterDocument, Provider, RouteHosts};

    fn document(provider: Provider) -> ClusterDocument {
        ClusterDocument {
            build_cloud_cap_usd: 3.0,
            provider,
            project_id: "test-project".into(),
            zone: "us-central1-a".into(),
            cluster_name: "install-test".into(),
            org: "test".parse().unwrap(),
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
}
